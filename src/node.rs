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

    /// The runtime's status, shaped like `logosctl status`:
    /// `{"daemon": {"pid": ..}, "modules": [{"name", "status", "uptime_seconds", "version"}],
    /// "modules_summary": {..}}`. Used to report health and to notice restarts.
    fn status(&self) -> NodeFuture<'_, Value>;

    /// Load `module` if it isn't loaded. `Ok(true)` when this call loaded it.
    fn ensure_loaded<'a>(&'a self, module: &'a str) -> NodeFuture<'a, bool>;

    /// Feed `module`'s events into `store` for as long as the process runs,
    /// recovering on its own if the event source drops.
    fn watch(&self, module: &'static str, store: Arc<EventStore>);
}
