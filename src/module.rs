//! What the Basecamp module needs from the library: the MCP endpoint over local
//! HTTP behind a bearer token, the token itself, and the module's config file.
//! Kept here, not in the Nix-built module crate, so `cargo test` covers it.

use std::{
    io,
    path::{Path, PathBuf},
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

use crate::{guard::FilesRoot, logosctl::LogosError};

pub const DEFAULT_PORT: u16 = 8765;
/// The bearer token, in the module's data folder, readable by this user only.
pub const TOKEN_FILE: &str = "token";
/// Settings the user (and later the Agent app) can edit, in the module's data folder.
pub const CONFIG_FILE: &str = "config.json";

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

    /// The token in `dir`, created on first use. Deleting the file and restarting
    /// the module issues a new one, which disconnects every client.
    pub fn load_or_create(dir: &Path) -> io::Result<Self> {
        let path = dir.join(TOKEN_FILE);
        if let Ok(text) = std::fs::read_to_string(&path)
            && let Ok(token) = Self::from_hex(text.trim().to_string())
        {
            return Ok(token);
        }
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|e| io::Error::other(e.to_string()))?;
        let token = Self(bytes.iter().map(|b| format!("{b:02x}")).collect());
        crate::guard::write_private(&path, token.as_str().as_bytes())?;
        Ok(token)
    }

    /// Compares in constant time, so response timing reveals nothing about the token.
    fn matches(&self, presented: &str) -> bool {
        let (a, b) = (self.0.as_bytes(), presented.as_bytes());
        a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
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
}

impl Default for ModuleConfig {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            files_root: None,
            discovery_bootstrap: Vec::new(),
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
                let text = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
                std::fs::write(&path, text).map_err(|e| e.to_string())?;
                Ok(config)
            }
            Err(e) => Err(format!("cannot read {}: {e}", path.display())),
        }
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
pub fn router<S, F>(factory: F, token: &Token) -> Router
where
    S: ServerHandler + Send + 'static,
    F: Fn() -> Result<S, io::Error> + Send + Sync + 'static,
{
    let config = StreamableHttpServerConfig::default().enforce_origin_validation();
    let service: StreamableHttpService<S, LocalSessionManager> =
        StreamableHttpService::new(factory, Default::default(), config);
    let token = token.clone();
    Router::new()
        .nest_service("/mcp", service)
        .layer(middleware::from_fn(
            move |req: Request<Body>, next: Next| {
                let token = token.clone();
                async move {
                    let presented = req
                        .headers()
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.strip_prefix("Bearer "))
                        .unwrap_or("");
                    if token.matches(presented) {
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
