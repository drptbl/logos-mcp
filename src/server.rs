//! MCP tools over a running Logos node: status, messaging and storage.

use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::{tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ContentBlock, ServerCapabilities, ServerConfig},
    schemars::{self, JsonSchema},
    tool, tool_handler, tool_router,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    events::EventStore,
    guard::{self, FilesRoot},
    logosctl::{Arg, LogosError, Logosctl, embedded_json},
};

const DELIVERY: &str = "delivery_module";
const STORAGE: &str = "storage_module";
const RLN: &str = "liblogos_rln_module";
const CHUNK_SIZE: i64 = 65_536;

/// What the operator chose at launch. Agents cannot change these.
#[derive(Debug, Clone)]
pub struct NodeSettings {
    pub preset: String,
    pub mode: String,
    pub config_dir: Option<String>,
}

#[derive(Clone)]
pub struct LogosServer {
    ctl: Logosctl,
    events: Arc<EventStore>,
    files: Option<FilesRoot>,
    settings: NodeSettings,
    subscriptions: Arc<Mutex<BTreeSet<String>>>,
    #[expect(dead_code, reason = "read by the tool_handler macro")]
    tool_router: ToolRouter<Self>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TopicParams {
    /// Content topic, e.g. /my-app/1/chat/proto
    pub topic: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SendParams {
    /// Content topic, e.g. /my-app/1/chat/proto
    pub topic: String,
    /// UTF-8 text payload. Give exactly one of `text` or `base64`.
    pub text: Option<String>,
    /// Binary payload, standard base64.
    pub base64: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RequestIdParams {
    /// The request id messaging_send returned.
    pub request_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadParams {
    /// Content topic you subscribed to.
    pub topic: String,
    /// Return only messages with seq greater than this (use next_after from the previous read).
    pub after_seq: Option<u64>,
    /// Max messages to return (default 50, max 256).
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UploadParams {
    /// File path relative to the server's files root.
    pub path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DownloadParams {
    /// Content id to download.
    pub cid: String,
    /// New file path relative to the files root (must not exist yet).
    pub path: String,
    /// Route the download over the mixnet and do not re-share the content.
    pub private: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SessionParams {
    /// Session id returned by storage_upload or storage_download.
    pub session_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CidParams {
    /// Content id.
    pub cid: String,
}

fn ok(value: Value) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::success(vec![ContentBlock::json(value)?]))
}

/// Tool-level failure: reported to the agent as an error result it can act on.
fn fail(message: impl Into<String>) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::error(vec![ContentBlock::text(
        message.into(),
    )]))
}

fn logos_fail(e: LogosError) -> Result<CallToolResult, McpError> {
    fail(e.to_string())
}

fn is_uninitialized(e: &LogosError) -> bool {
    matches!(e, LogosError::Refused(m) if m.to_ascii_lowercase().contains("not initialized"))
}

#[tool_router]
impl LogosServer {
    pub fn new(
        ctl: Logosctl,
        events: Arc<EventStore>,
        files: Option<FilesRoot>,
        settings: NodeSettings,
    ) -> Self {
        Self {
            ctl,
            events,
            files,
            settings,
            subscriptions: Arc::default(),
            tool_router: Self::tool_router(),
        }
    }

    fn node_config(&self) -> String {
        json!({ "mode": self.settings.mode, "preset": self.settings.preset }).to_string()
    }

    fn files(&self) -> Result<&FilesRoot, String> {
        self.files.as_ref().ok_or_else(|| {
            "storage file access is disabled: start the server with --files-root".to_string()
        })
    }

    #[tool(
        description = "Show the Logos daemon, loaded modules, messaging connection and this server's settings."
    )]
    async fn node_status(&self) -> Result<CallToolResult, McpError> {
        let daemon = match self.ctl.command(&["status"]).await {
            Ok(v) => v,
            Err(e) => return logos_fail(e),
        };
        let connection = match self.ctl.call(DELIVERY, "getConnectionStatus", &[]).await {
            Ok(v) => v,
            Err(e) if is_uninitialized(&e) => json!("not created (call messaging_connect)"),
            Err(e) => json!(format!("unavailable: {e}")),
        };
        ok(json!({
            "daemon": daemon.get("daemon"),
            "modules": daemon.get("modules_summary"),
            "messaging": {
                "network": self.settings.preset,
                "mode": self.settings.mode,
                "connection": connection,
                "subscriptions": *self.subscriptions.lock().unwrap(),
            },
            "storage_files_root": self.files.as_ref().map(|f| f.path().display().to_string()),
            "session": self.settings.config_dir,
        }))
    }

    #[tool(
        description = "Bring up the messaging node on the configured network. Safe to call repeatedly: it only creates and starts a node when none exists, and leaves an existing node as it is."
    )]
    async fn messaging_connect(&self) -> Result<CallToolResult, McpError> {
        match self.ctl.ensure_loaded(DELIVERY).await {
            // Calls right after a load can fail while the module settles.
            Ok(true) => tokio::time::sleep(std::time::Duration::from_secs(3)).await,
            Ok(false) => {}
            Err(e) => return logos_fail(e),
        }
        let mut created = false;
        let mut started = false;
        let mut connection = Value::Null;
        match self.ctl.call(DELIVERY, "getConnectionStatus", &[]).await {
            // An existing node is never restarted here: "Disconnected" can be a
            // running node between peers, or one an operator created but has
            // deliberately not started (e.g. waiting for an RLN membership).
            Ok(status) => connection = status,
            Err(e) if is_uninitialized(&e) => {
                if let Err(e) = self
                    .ctl
                    .call(DELIVERY, "createNode", &[Arg::str(self.node_config())])
                    .await
                {
                    return logos_fail(e);
                }
                if let Err(e) = self.ctl.call(DELIVERY, "start", &[]).await {
                    return logos_fail(e);
                }
                created = true;
                started = true;
            }
            Err(e) => return logos_fail(e),
        }
        // A fresh node knows no topics; restore the ones this server tracks.
        let mut resubscribed = Vec::new();
        if created {
            let topics: Vec<String> = self.subscriptions.lock().unwrap().iter().cloned().collect();
            for t in topics {
                if self
                    .ctl
                    .call(DELIVERY, "subscribe", &[Arg::str(&t)])
                    .await
                    .is_ok()
                {
                    resubscribed.push(t);
                }
            }
        }
        ok(json!({
            "network": self.settings.preset,
            "created": created,
            "started": started,
            "connection": connection,
            "resubscribed": resubscribed,
            "note": if created {
                "connection takes a few seconds; check node_status"
            } else {
                "an existing node was left as it is; if it stays Disconnected it may not be started"
            },
        }))
    }

    #[tool(
        description = "Subscribe to a content topic so its messages can be read with messaging_read."
    )]
    async fn messaging_subscribe(
        &self,
        Parameters(p): Parameters<TopicParams>,
    ) -> Result<CallToolResult, McpError> {
        if let Err(e) = guard::validate_topic(&p.topic) {
            return fail(e);
        }
        if let Err(e) = self
            .ctl
            .call(DELIVERY, "subscribe", &[Arg::str(&p.topic)])
            .await
        {
            return logos_fail(e);
        }
        self.subscriptions.lock().unwrap().insert(p.topic.clone());
        ok(json!({ "subscribed": p.topic }))
    }

    #[tool(description = "Unsubscribe from a content topic.")]
    async fn messaging_unsubscribe(
        &self,
        Parameters(p): Parameters<TopicParams>,
    ) -> Result<CallToolResult, McpError> {
        if let Err(e) = guard::validate_topic(&p.topic) {
            return fail(e);
        }
        if let Err(e) = self
            .ctl
            .call(DELIVERY, "unsubscribe", &[Arg::str(&p.topic)])
            .await
        {
            return logos_fail(e);
        }
        self.subscriptions.lock().unwrap().remove(&p.topic);
        ok(json!({ "unsubscribed": p.topic }))
    }

    #[tool(
        description = "Publish a message to a content topic. Returns a request_id; check delivery with messaging_send_status. On logos.test, sending needs an active RLN membership."
    )]
    async fn messaging_send(
        &self,
        Parameters(p): Parameters<SendParams>,
    ) -> Result<CallToolResult, McpError> {
        if let Err(e) = guard::validate_topic(&p.topic) {
            return fail(e);
        }
        let payload = match (p.text, p.base64) {
            (Some(t), None) => t.into_bytes(),
            (None, Some(b)) => match STANDARD.decode(b.trim()) {
                Ok(bytes) => bytes,
                Err(e) => return fail(format!("base64 payload does not decode: {e}")),
            },
            _ => return fail("give exactly one of `text` or `base64`"),
        };
        if let Err(e) = guard::validate_payload(&payload) {
            return fail(e);
        }
        match self
            .ctl
            .call(DELIVERY, "send", &[Arg::str(&p.topic), Arg::Bytes(payload)])
            .await
        {
            Ok(id) => ok(json!({ "request_id": id, "topic": p.topic })),
            Err(e) => logos_fail(e),
        }
    }

    #[tool(
        description = "Delivery progress of a sent message: queued, sent, propagated, or an error."
    )]
    async fn messaging_send_status(
        &self,
        Parameters(p): Parameters<RequestIdParams>,
    ) -> Result<CallToolResult, McpError> {
        match self.events.send_status(&p.request_id) {
            Some(st) => ok(serde_json::to_value(st).unwrap_or(Value::Null)),
            None => ok(json!({ "request_id": p.request_id, "status": "no events yet" })),
        }
    }

    #[tool(
        description = "Read messages received on a subscribed topic since this server started. Pass next_after back as after_seq to page forward."
    )]
    async fn messaging_read(
        &self,
        Parameters(p): Parameters<ReadParams>,
    ) -> Result<CallToolResult, McpError> {
        if let Err(e) = guard::validate_topic(&p.topic) {
            return fail(e);
        }
        let limit = p.limit.unwrap_or(50).clamp(1, 256);
        let after = p.after_seq.unwrap_or(0);
        let messages = self.events.read(&p.topic, after, limit);
        let next_after = messages.last().map(|m| m.seq).unwrap_or(after);
        ok(json!({ "messages": messages, "next_after": next_after }))
    }

    #[tool(
        description = "Whether this node can send right now: RLN membership state and the messages left in the current epoch. On logos.dev RLN is off."
    )]
    async fn rln_status(&self) -> Result<CallToolResult, McpError> {
        let rln = match self.ctl.call(DELIVERY, "rlnState", &[]).await {
            Ok(v) => v,
            Err(e) if is_uninitialized(&e) => {
                return fail("messaging node not created yet: call messaging_connect");
            }
            Err(e) => return logos_fail(e),
        };
        if rln.get("state").and_then(Value::as_str) == Some("Disabled") {
            return ok(json!({
                "network": self.settings.preset,
                "rln": "off",
                "can_send": true,
            }));
        }
        let scope = (
            rln.get("registryId").and_then(Value::as_str),
            rln.get("rlnIdentifier").and_then(Value::as_str),
        );
        let (Some(registry), Some(identifier)) = scope else {
            return fail(format!("RLN is not ready yet: {rln}"));
        };
        let scope_args = [Arg::str(registry), Arg::str(identifier)];

        let membership = match self
            .ctl
            .call(RLN, "get_membership_state", &scope_args)
            .await
        {
            Ok(v) => embedded_json(v),
            Err(e) => return logos_fail(e),
        };
        let state = membership
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let usable = matches!(state, "active" | "grace_period");

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        let quota_args = [
            scope_args[0].clone(),
            scope_args[1].clone(),
            Arg::str(now.to_string()),
        ];
        let quota = self.ctl.call(RLN, "get_epoch_quota", &quota_args).await;
        let remaining = quota
            .as_ref()
            .ok()
            .and_then(|q| q.get("remaining"))
            .and_then(Value::as_u64);

        let reason = if !usable {
            let detail = membership
                .pointer("/provisioning/detail")
                .and_then(Value::as_str);
            Some(match detail {
                Some(d) => format!("membership is {state}: {d}"),
                None => format!("membership is {state}"),
            })
        } else {
            match (&quota, remaining) {
                (Err(e), _) => Some(format!("could not read the epoch quota: {e}")),
                (Ok(_), None) => Some("epoch quota reply has no remaining count".to_string()),
                (Ok(_), Some(0)) => Some(
                    "rate limit reached for this epoch; it resets in the next epoch".to_string(),
                ),
                (Ok(_), Some(_)) => None,
            }
        };
        ok(json!({
            "network": self.settings.preset,
            "rln": "on",
            "can_send": reason.is_none(),
            "reason": reason,
            "membership": {
                "state": state,
                "rate_limit": membership.get("rate_limit"),
                "leaf_index": membership.get("leaf_index"),
            },
            "quota": match &quota {
                Ok(q) => q.clone(),
                Err(e) => json!({ "error": e.to_string() }),
            },
            "epoch_seconds": rln.get("epochSizeSec"),
        }))
    }

    #[tool(
        description = "Upload a file from the files root to Logos Storage. Returns a session_id; the CID appears in storage_transfer_status when done."
    )]
    async fn storage_upload(
        &self,
        Parameters(p): Parameters<UploadParams>,
    ) -> Result<CallToolResult, McpError> {
        let path = match self.files().and_then(|f| f.existing_file(&p.path)) {
            Ok(path) => path,
            Err(e) => return fail(e),
        };
        let args = [
            Arg::str(path.display().to_string()),
            Arg::Int(CHUNK_SIZE),
            Arg::Bool(true),
        ];
        match self.ctl.call(STORAGE, "uploadUrl", &args).await {
            Ok(Value::String(session)) => {
                self.events.expect_transfer(&session, "upload");
                ok(json!({ "session_id": session }))
            }
            Ok(other) => fail(format!("unexpected uploadUrl result: {other}")),
            Err(e) => logos_fail(e),
        }
    }

    #[tool(
        description = "Download content by CID into a new file under the files root. Returns a session_id for storage_transfer_status."
    )]
    async fn storage_download(
        &self,
        Parameters(p): Parameters<DownloadParams>,
    ) -> Result<CallToolResult, McpError> {
        if let Err(e) = guard::validate_cid(&p.cid) {
            return fail(e);
        }
        let path = match self.files().and_then(|f| f.new_file(&p.path)) {
            Ok(path) => path,
            Err(e) => return fail(e),
        };
        let private = p.private.unwrap_or(false);
        let args = [
            Arg::str(&p.cid),
            Arg::str(path.display().to_string()),
            Arg::Bool(false),
            Arg::Int(CHUNK_SIZE),
            Arg::Bool(private),
            // Full privacy also means not re-serving what was fetched.
            Arg::Bool(!private),
        ];
        // The download session id is the CID: clear any earlier run first.
        self.events.reset_transfer(&p.cid, "download");
        match self.ctl.call(STORAGE, "downloadToUrl", &args).await {
            Ok(Value::String(session)) => {
                self.events.expect_transfer(&session, "download");
                ok(json!({ "session_id": session, "path": p.path }))
            }
            Ok(other) => fail(format!("unexpected downloadToUrl result: {other}")),
            Err(e) => logos_fail(e),
        }
    }

    #[tool(
        description = "Progress and result (including the CID for uploads) of a storage upload or download."
    )]
    async fn storage_transfer_status(
        &self,
        Parameters(p): Parameters<SessionParams>,
    ) -> Result<CallToolResult, McpError> {
        match self.events.transfer(&p.session_id) {
            Some(t) => ok(serde_json::to_value(t).unwrap_or(Value::Null)),
            None => fail(format!("unknown session {}", p.session_id)),
        }
    }

    #[tool(description = "Check whether content is already in local storage.")]
    async fn storage_exists(
        &self,
        Parameters(p): Parameters<CidParams>,
    ) -> Result<CallToolResult, McpError> {
        if let Err(e) = guard::validate_cid(&p.cid) {
            return fail(e);
        }
        match self.ctl.call(STORAGE, "exists", &[Arg::str(&p.cid)]).await {
            Ok(v) => ok(json!({ "cid": p.cid, "exists": v })),
            Err(e) => logos_fail(e),
        }
    }

    #[tool(description = "Local storage quota and usage.")]
    async fn storage_space(&self) -> Result<CallToolResult, McpError> {
        match self.ctl.call(STORAGE, "space", &[]).await {
            Ok(v) => ok(embedded_json(v)),
            Err(e) => logos_fail(e),
        }
    }
}

#[tool_handler]
impl ServerHandler for LogosServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Tools for a running Logos node. Call messaging_connect before messaging tools, \
             subscribe before reading, and poll messaging_send_status / storage_transfer_status \
             for asynchronous results.",
        )
    }
}

#[cfg(test)]
mod tests;
