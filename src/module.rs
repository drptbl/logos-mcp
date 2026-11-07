//! What the Basecamp module needs from the library: the MCP endpoint over local
//! HTTP behind a bearer token, the token itself, and the module's config file.
//! Kept here, not in the Nix-built module crate, so `cargo test` covers it.

use std::{
    io,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use rmcp::{
    ServerHandler,
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use serde::{Deserialize, Serialize};

use serde_json::{Value, json};

use crate::{
    control::{ActivityLog, ToolGroups},
    guard::FilesRoot,
    logosctl::LogosError,
    server::LogosServer,
};

pub const DEFAULT_PORT: u16 = 8765;
/// The bearer token, in the module's data folder, readable by this user only.
pub const TOKEN_FILE: &str = "token";
/// Settings the user (and later the Agent app) can edit, in the module's data folder.
pub const CONFIG_FILE: &str = "config.json";
/// The record of the agent's tool calls, in the module's data folder.
pub const ACTIVITY_FILE: &str = "activity.jsonl";

/// The secret an MCP client presents as `Authorization: Bearer <token>`.
#[derive(Clone)]
pub struct Token(String);

impl Token {
    /// 32 bytes as 64 lowercase hex characters.
    pub fn from_hex(hex: String) -> Result<Self, String> {
        if hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            Ok(Self(hex.to_ascii_lowercase()))
        } else {
            Err("token must be 64 hex characters".into())
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The token in `dir`, created on first use.
    pub fn load_or_create(dir: &Path) -> io::Result<Self> {
        let path = dir.join(TOKEN_FILE);
        if let Ok(text) = std::fs::read_to_string(&path)
            && let Ok(token) = Self::from_hex(text.trim().to_string())
        {
            return Ok(token);
        }
        let token = Self::generate()?;
        crate::guard::write_private(&path, token.as_str().as_bytes())?;
        Ok(token)
    }

    fn generate() -> io::Result<Self> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|e| io::Error::other(e.to_string()))?;
        Ok(Self(bytes.iter().map(|b| format!("{b:02x}")).collect()))
    }

    /// Compares in constant time, so response timing reveals nothing about the token.
    fn matches(&self, presented: &str) -> bool {
        let (a, b) = (self.0.as_bytes(), presented.as_bytes());
        a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
    }
}

/// The token the endpoint accepts right now. Rotating it takes effect on the
/// next request, so every client holding the old one is cut off at once.
#[derive(Clone)]
pub struct Tokens {
    file: Option<PathBuf>,
    current: Arc<RwLock<Token>>,
}

impl From<Token> for Tokens {
    /// A token kept in memory only.
    fn from(token: Token) -> Self {
        Self {
            file: None,
            current: Arc::new(RwLock::new(token)),
        }
    }
}

impl Tokens {
    /// The token in `dir`, created on first use.
    pub fn load_or_create(dir: &Path) -> io::Result<Self> {
        Ok(Self {
            file: Some(dir.join(TOKEN_FILE)),
            ..Self::from(Token::load_or_create(dir)?)
        })
    }

    pub fn current(&self) -> Token {
        self.current
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Issue a new token and save it. The old one stops working immediately.
    /// The write lock is held while saving, so concurrent rotations can't leave
    /// the file and the accepted token disagreeing.
    pub fn rotate(&self) -> io::Result<Token> {
        let mut current = self.current.write().unwrap_or_else(|e| e.into_inner());
        let token = Token::generate()?;
        if let Some(file) = &self.file {
            crate::guard::write_private(file, token.as_str().as_bytes())?;
        }
        *current = token.clone();
        Ok(token)
    }

    fn matches(&self, presented: &str) -> bool {
        self.current.read().is_ok_and(|t| t.matches(presented))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModuleConfig {
    /// Port on 127.0.0.1 for the MCP endpoint.
    pub port: u16,
    /// The only folder storage tools may read and write. Storage file access is off without it.
    pub files_root: Option<PathBuf>,
    /// Discovery peers as `/.../p2p/<peer id>` multiaddrs. Discovery tools are off without them.
    pub discovery_bootstrap: Vec<String>,
    /// The tool groups the agent may use, switched from the Agent app.
    pub tools: ToolGroups,
}

impl Default for ModuleConfig {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            files_root: None,
            discovery_bootstrap: Vec::new(),
            tools: ToolGroups::default(),
        }
    }
}

impl ModuleConfig {
    /// The storage files root, refused when it overlaps `data_dir`: storage
    /// downloads must never reach the token, the config or the agent accounts.
    pub fn files_root(&self, data_dir: &Path) -> Result<Option<FilesRoot>, String> {
        let Some(root) = &self.files_root else {
            return Ok(None);
        };
        let root =
            FilesRoot::new(root).map_err(|e| format!("files_root {}: {e}", root.display()))?;
        let data = data_dir.canonicalize().map_err(|e| e.to_string())?;
        if data.starts_with(root.path()) || root.path().starts_with(&data) {
            return Err(format!(
                "files_root {} overlaps the module's data folder {}: pick another folder",
                root.path().display(),
                data.display()
            ));
        }
        Ok(Some(root))
    }

    /// The config in `dir`. A missing file is created with the defaults so it can be
    /// edited. A file that doesn't parse is an error rather than silently ignored.
    pub fn load_or_create(dir: &Path) -> Result<Self, String> {
        let path = dir.join(CONFIG_FILE);
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| format!("{} is not valid: {e}", path.display())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let config = Self::default();
                config.save(dir)?;
                Ok(config)
            }
            Err(e) => Err(format!("cannot read {}: {e}", path.display())),
        }
    }
}

impl ModuleConfig {
    fn save(&self, dir: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        crate::guard::write_private(&dir.join(CONFIG_FILE), text.as_bytes())
            .map_err(|e| e.to_string())
    }
}

/// What the Agent app reads and changes: the endpoint and its token, the tool
/// groups the agent may use, and the record of its calls.
pub struct Control {
    dir: PathBuf,
    config: ModuleConfig,
    tokens: Tokens,
    groups: tokio::sync::watch::Sender<ToolGroups>,
    /// Held while a switch is saved, so two saves can't interleave.
    saving: std::sync::Mutex<()>,
    activity: Arc<ActivityLog>,
}

impl Control {
    /// The module's config, token and activity in `dir`, created on first use.
    pub fn open(dir: &Path) -> Result<Self, String> {
        let config = ModuleConfig::load_or_create(dir)?;
        Ok(Self {
            dir: dir.to_path_buf(),
            tokens: Tokens::load_or_create(dir).map_err(|e| e.to_string())?,
            groups: tokio::sync::watch::Sender::new(config.tools.clone()),
            saving: std::sync::Mutex::new(()),
            activity: Arc::new(ActivityLog::open(dir.join(ACTIVITY_FILE))?),
            config,
        })
    }

    /// The config as it was when the module started.
    pub fn config(&self) -> &ModuleConfig {
        &self.config
    }

    pub fn tokens(&self) -> &Tokens {
        &self.tokens
    }

    pub fn tool_groups(&self) -> tokio::sync::watch::Receiver<ToolGroups> {
        self.groups.subscribe()
    }

    pub fn activity_log(&self) -> Arc<ActivityLog> {
        self.activity.clone()
    }

    /// `server`, obeying the tool groups and recording its calls here.
    pub fn attach(&self, server: LogosServer) -> LogosServer {
        server
            .with_tool_groups(self.tool_groups())
            .with_activity(self.activity_log())
    }

    /// Where clients connect and where the files are. `error` is why the
    /// endpoint isn't serving, if it isn't. Never includes the token.
    pub fn status(&self, error: Option<&str>) -> Value {
        json!({
            "url": format!("http://127.0.0.1:{}/mcp", self.config.port),
            "running": error.is_none(),
            "error": error,
            "token_file": self.dir.join(TOKEN_FILE),
            "config_file": self.dir.join(CONFIG_FILE),
        })
    }

    pub fn permissions(&self) -> Value {
        serde_json::to_value(&*self.groups.borrow()).unwrap_or_default()
    }

    /// Switch one tool group and save it. Only that group changes in the file,
    /// so other edits there are kept, and the file's groups then apply live.
    pub fn set_permission(&self, group: &str, on: bool) -> Result<Value, String> {
        let _saving = self.saving.lock().unwrap_or_else(|e| e.into_inner());
        let mut config = ModuleConfig::load_or_create(&self.dir)?;
        config.tools.set(group, on)?;
        config.save(&self.dir)?;
        // Only a real change tells connected agents to re-list.
        self.groups.send_if_modified(|groups| {
            let changed = *groups != config.tools;
            *groups = config.tools;
            changed
        });
        Ok(self.permissions())
    }

    /// Calls after sequence number `after`, and the cursor to pass next time.
    /// A cursor past the newest entry (the log was reset) starts over.
    pub fn activity(&self, after: u64) -> Value {
        let after = if after > self.activity.latest() {
            0
        } else {
            after
        };
        let entries = self.activity.since(after);
        let next = entries.last().map_or(after, |e| e.seq);
        json!({ "entries": entries, "next": next })
    }
}

/// An in-process SDK call failure. A module Basecamp hasn't loaded is said plainly,
/// since the SDK reports it as an unavailable remote object.
pub fn sdk_error(module: &str, message: String) -> LogosError {
    if message.contains("object_unavailable") {
        LogosError::Module(format!(
            "{module} isn't running in Basecamp: open or load it there first"
        ))
    } else {
        LogosError::Method(message)
    }
}

/// The MCP endpoint at `/mcp`, refusing any request without the right bearer token.
/// Only localhost `Host` headers are accepted, and any request carrying an `Origin`
/// (a web page) is refused: desktop MCP clients send none.
pub fn router<S, F>(factory: F, tokens: &Tokens) -> Router
where
    S: ServerHandler + Send + 'static,
    F: Fn() -> Result<S, io::Error> + Send + Sync + 'static,
{
    let config = StreamableHttpServerConfig::default().enforce_origin_validation();
    let service: StreamableHttpService<S, LocalSessionManager> =
        StreamableHttpService::new(factory, Default::default(), config);
    let tokens = tokens.clone();
    Router::new()
        .nest_service("/mcp", service)
        .layer(middleware::from_fn(
            move |req: Request<Body>, next: Next| {
                let tokens = tokens.clone();
                async move {
                    let presented = req
                        .headers()
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.strip_prefix("Bearer "))
                        .unwrap_or("");
                    if tokens.matches(presented) {
                        next.run(req).await
                    } else {
                        unauthorized()
                    }
                }
            },
        ))
}

fn unauthorized() -> Response {
    (StatusCode::UNAUTHORIZED, "missing or wrong bearer token").into_response()
}

#[cfg(test)]
mod tests;
