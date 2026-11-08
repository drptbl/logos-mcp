//! Thin client over the `logosctl` command surface.
//!
//! Every operation is one `logosctl --json ...` process. The session is chosen
//! through `LOGOSCTL_CONFIG_DIR` (never `--config-dir` after a subcommand, where
//! logosctl would pass it to the method as an argument).

use std::{future::Future, io, path::PathBuf, pin::Pin, process::Stdio, sync::Arc, time::Duration};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawOutput {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub type RunFuture<'a> = Pin<Box<dyn Future<Output = io::Result<RawOutput>> + Send + 'a>>;

/// Runs one `logosctl` invocation. A trait so tests can replay recorded output.
pub trait Runner: Send + Sync {
    fn run(&self, args: Vec<String>) -> RunFuture<'_>;
}

pub struct ProcessRunner {
    pub bin: PathBuf,
    pub config_dir: Option<PathBuf>,
    pub timeout: Duration,
}

impl Runner for ProcessRunner {
    fn run(&self, args: Vec<String>) -> RunFuture<'_> {
        Box::pin(async move {
            let mut cmd = tokio::process::Command::new(&self.bin);
            cmd.args(&args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            if let Some(dir) = &self.config_dir {
                cmd.env("LOGOSCTL_CONFIG_DIR", dir);
            }
            let out = tokio::time::timeout(self.timeout, cmd.output())
                .await
                .map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("logosctl timed out after {:?}", self.timeout),
                    )
                })??;
            Ok(RawOutput {
                code: out.status.code().unwrap_or(-1),
                stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            })
        })
    }
}

/// A positional method argument, encoded the way logosctl's argument typing
/// expects. Strings always carry `str:` so a JSON- or number-looking string is
/// never coerced into another type.
#[derive(Debug, Clone, PartialEq)]
pub enum Arg {
    Str(String),
    Int(i64),
    Bool(bool),
    Bytes(Vec<u8>),
    /// A record argument, passed as a JSON value rather than a string.
    Json(Value),
}

impl Arg {
    pub fn str(s: impl Into<String>) -> Self {
        Arg::Str(s.into())
    }

    pub fn encode(&self) -> String {
        match self {
            Arg::Str(s) => format!("str:{s}"),
            Arg::Int(i) => i.to_string(),
            Arg::Bool(b) => b.to_string(),
            Arg::Bytes(_) | Arg::Json(_) => format!("json:{}", self.to_json()),
        }
    }

    /// The argument as a JSON value, the form in-process SDK calls take. Bytes
    /// use the protocol's tagged form, `{"_bytes": <unpadded base64url>}`.
    pub fn to_json(&self) -> Value {
        match self {
            Arg::Str(s) => json!(s),
            Arg::Int(i) => json!(i),
            Arg::Bool(b) => json!(b),
            Arg::Bytes(b) => json!({ "_bytes": URL_SAFE_NO_PAD.encode(b) }),
            Arg::Json(v) => v.clone(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LogosError {
    #[error("could not run logosctl: {0}")]
    Spawn(#[from] io::Error),
    #[error("no logosctl daemon reachable: {0}")]
    DaemonUnavailable(String),
    #[error("module error: {0}")]
    Module(String),
    #[error("method call failed: {0}")]
    Method(String),
    #[error("method refused: {0}")]
    Refused(String),
    #[error("logosctl failed (exit {code}): {message}")]
    Other { code: i32, message: String },
    #[error("unexpected logosctl output: {0}")]
    BadOutput(String),
}

/// Describe a failed invocation from its JSON envelope, falling back to stderr.
fn failure_message(raw: &RawOutput) -> String {
    if let Ok(Value::Object(env)) = serde_json::from_str::<Value>(raw.stdout.trim()) {
        let code = env.get("code").and_then(Value::as_str).unwrap_or("");
        let message = env.get("message").and_then(Value::as_str).unwrap_or("");
        let detail = env
            .get("error")
            .and_then(|e| e.get("code"))
            .and_then(Value::as_str);
        let mut out = match (code.is_empty(), message.is_empty()) {
            (false, false) => format!("{code}: {message}"),
            (false, true) => code.to_string(),
            _ => message.to_string(),
        };
        if let Some(d) = detail {
            out.push_str(&format!(" ({d})"));
        }
        if !out.is_empty() {
            return out;
        }
    }
    let tail: Vec<&str> = raw.stderr.lines().rev().take(3).collect();
    let tail: Vec<&str> = tail.into_iter().rev().collect();
    if tail.is_empty() {
        "no output".to_string()
    } else {
        tail.join(" | ")
    }
}

/// Map a raw invocation to its JSON value: the exit-code contract first, then
/// the CLI envelope (`status` + `result`).
pub fn parse_output(raw: &RawOutput) -> Result<Value, LogosError> {
    if raw.code != 0 {
        let msg = failure_message(raw);
        return Err(match raw.code {
            2 => LogosError::DaemonUnavailable(msg),
            3 => LogosError::Module(msg),
            4 => LogosError::Method(msg),
            code => LogosError::Other { code, message: msg },
        });
    }
    let stdout = raw.stdout.trim();
    if stdout.is_empty() {
        return Ok(Value::Null);
    }
    let value: Value = serde_json::from_str(stdout)
        .map_err(|e| LogosError::BadOutput(format!("{e}: {}", truncate(stdout, 200))))?;
    if let Value::Object(env) = &value {
        if env.get("status").and_then(Value::as_str) == Some("error") {
            return Err(LogosError::Method(failure_message(raw)));
        }
        if env.contains_key("result") && env.contains_key("method") {
            return Ok(env["result"].clone());
        }
    }
    Ok(value)
}

/// Unwrap the `{success, value, error}` shape many module methods return. Only
/// applied when the value has that shape: plain methods (`version`,
/// `isRunning`) return their value directly.
pub fn unwrap_std_result(value: Value) -> Result<Value, LogosError> {
    let Value::Object(map) = &value else {
        return Ok(value);
    };
    let Some(success) = map.get("success").and_then(Value::as_bool) else {
        return Ok(value);
    };
    if !(map.contains_key("value") || map.contains_key("error")) {
        return Ok(value);
    }
    if success {
        Ok(map.get("value").cloned().unwrap_or(Value::Null))
    } else {
        let reason = match map.get("error") {
            Some(Value::String(s)) if !s.is_empty() => s.clone(),
            Some(Value::Null) | None => "no detail given".to_string(),
            Some(other) => other.to_string(),
        };
        Err(LogosError::Refused(reason))
    }
}

/// Some methods return a JSON document as a string; decode it when it is one.
pub fn embedded_json(value: Value) -> Value {
    if let Value::String(s) = &value {
        let t = s.trim_start();
        if (t.starts_with('{') || t.starts_with('['))
            && let Ok(parsed) = serde_json::from_str(s)
        {
            return parsed;
        }
    }
    value
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        let mut end = n;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}

#[derive(Clone)]
pub struct Logosctl {
    runner: Arc<dyn Runner>,
    /// The logosctl binary and session for `watch`. None in tests, where nothing watches.
    watch: Option<(PathBuf, Option<PathBuf>)>,
}

impl Logosctl {
    pub fn new(runner: Arc<dyn Runner>) -> Self {
        Self {
            runner,
            watch: None,
        }
    }

    /// Let `watch` start `logosctl watch` processes against this binary and session.
    pub fn with_watch(mut self, bin: PathBuf, config_dir: Option<PathBuf>) -> Self {
        self.watch = Some((bin, config_dir));
        self
    }

    /// Run a non-`call` subcommand (`status`, `module ls`, ...).
    pub async fn command(&self, args: &[&str]) -> Result<Value, LogosError> {
        let mut argv = vec!["--json".to_string()];
        argv.extend(args.iter().map(|s| s.to_string()));
        let raw = self.runner.run(argv).await?;
        parse_output(&raw)
    }

    /// Invoke a module method and return its unwrapped value.
    pub async fn call(
        &self,
        module: &str,
        method: &str,
        args: &[Arg],
    ) -> Result<Value, LogosError> {
        let mut argv = vec!["--json".into(), "call".into(), module.into(), method.into()];
        argv.extend(args.iter().map(Arg::encode));
        let raw = self.runner.run(argv).await?;
        unwrap_std_result(parse_output(&raw)?)
    }

    pub async fn loaded_modules(&self) -> Result<Vec<String>, LogosError> {
        let v = self.command(&["module", "ls", "--loaded"]).await?;
        let list = v
            .as_array()
            .ok_or_else(|| LogosError::BadOutput(v.to_string()))?;
        Ok(list
            .iter()
            .filter_map(|m| m.get("name").and_then(Value::as_str).map(str::to_string))
            .collect())
    }

    pub async fn ensure_loaded(&self, module: &str) -> Result<bool, LogosError> {
        if self.loaded_modules().await?.iter().any(|m| m == module) {
            return Ok(false);
        }
        self.command(&["module", "load", module]).await?;
        Ok(true)
    }
}

impl crate::node::Node for Logosctl {
    fn call<'a>(
        &'a self,
        module: &'a str,
        method: &'a str,
        args: &'a [Arg],
    ) -> crate::node::NodeFuture<'a, Value> {
        Box::pin(Logosctl::call(self, module, method, args))
    }

    fn status(&self) -> crate::node::NodeFuture<'_, crate::node::NodeStatus> {
        Box::pin(async move {
            let v = self.command(&["status"]).await?;
            Ok(crate::node::NodeStatus::from_logosctl(&v))
        })
    }

    fn ensure_loaded<'a>(&'a self, module: &'a str) -> crate::node::NodeFuture<'a, bool> {
        Box::pin(Logosctl::ensure_loaded(self, module))
    }

    fn watch(&self, module: &'static str, store: Arc<crate::events::EventStore>) {
        match &self.watch {
            Some((bin, config_dir)) => {
                crate::events::spawn_watcher(bin.clone(), config_dir.clone(), module, store);
            }
            None => tracing::warn!(
                module,
                "watch requested but this Logosctl has no binary set (with_watch): no events will arrive"
            ),
        }
    }
}

#[cfg(test)]
pub mod tests;
