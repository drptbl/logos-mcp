//! The Logos node a server drives: the one seam every tool talks through.
//!
//! Two adapters sit behind it: the `logosctl` subprocess client (the standalone
//! binary) and, in the Basecamp module, in-process calls through the Logos Rust
//! SDK. Callers never see envelopes, argument encoding, processes or transports.

use std::{future::Future, pin::Pin, sync::Arc};

use serde_json::Value;

use crate::{
    events::EventStore,
    logosctl::{Arg, LogosError},
};

/// What a node can tell about itself. Adapters fill in only what they can observe:
/// the in-process adapter in Basecamp sees no daemon and leaves `instance` empty.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NodeStatus {
    /// Identifies this runtime instance. It changes when the runtime restarts.
    pub instance: Option<String>,
    pub modules: Vec<ModuleStatus>,
    /// Adapter-specific detail for reporting, such as logosctl's `daemon` and `modules_summary`.
    pub details: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModuleStatus {
    pub name: String,
    pub status: String,
    /// Seconds since the module was (re)loaded, when the runtime reports it.
    pub uptime_seconds: Option<u64>,
}

impl NodeStatus {
    pub fn module(&self, name: &str) -> Option<&ModuleStatus> {
        self.modules.iter().find(|m| m.name == name)
    }

    /// From `logosctl --json status`.
    pub fn from_logosctl(v: &Value) -> Self {
        let modules = v
            .get("modules")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|m| {
                        Some(ModuleStatus {
                            name: m.get("name")?.as_str()?.to_string(),
                            status: m
                                .get("status")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                            uptime_seconds: m.get("uptime_seconds").and_then(Value::as_u64),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            instance: v
                .pointer("/daemon/pid")
                .and_then(Value::as_u64)
                .map(|p| p.to_string()),
            modules,
            details: serde_json::json!({
                "daemon": v.get("daemon"),
                "modules_summary": v.get("modules_summary"),
            }),
        }
    }
}

pub type NodeFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, LogosError>> + Send + 'a>>;

pub trait Node: Send + Sync {
    /// Invoke `method` on `module` and return its value, with any
    /// `{success, value, error}` result envelope already unwrapped: a refusal
    /// comes back as `LogosError::Refused`.
    fn call<'a>(
        &'a self,
        module: &'a str,
        method: &'a str,
        args: &'a [Arg],
    ) -> NodeFuture<'a, Value>;

    /// What the runtime reports about itself, used to show health and to notice restarts.
    fn status(&self) -> NodeFuture<'_, NodeStatus>;

    /// Load `module` if it isn't loaded. `Ok(true)` when this call loaded it.
    fn ensure_loaded<'a>(&'a self, module: &'a str) -> NodeFuture<'a, bool>;

    /// Feed `module`'s events into `store` for as long as the process runs,
    /// recovering on its own if the event source drops.
    fn watch(&self, module: &'static str, store: Arc<EventStore>);
}

#[cfg(test)]
mod tests;
