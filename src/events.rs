//! Event intake: one long-lived `logosctl watch <module>` per module, parsed
//! into bounded in-memory buffers the tools read from.
//!
//! Events are NDJSON lines shaped `{"data":{"arg0":..},"event","module","timestamp"}`
//! with positional `argN` fields per event. Messages that arrive while a
//! watcher is restarting are not replayed (a known v1 gap).

use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};

pub const MAX_MESSAGES_PER_TOPIC: usize = 256;
pub const MAX_TRACKED_SENDS: usize = 1024;
pub const MAX_TRACKED_TRANSFERS: usize = 256;
/// Finalized chain blocks kept for chain_events, newest last.
pub const MAX_CHAIN_EVENTS: usize = 256;

/// A new finalized tip, as blockchain_module's libBlock reports it: once per
/// change of the last irreversible block, not once per block.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ChainBlock {
    pub seq: u64,
    pub height: u64,
    pub header_id: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReceivedMessage {
    pub seq: u64,
    pub hash: String,
    pub topic: String,
    /// The payload as text when it is valid UTF-8.
    pub text: Option<String>,
    pub payload_base64: String,
    pub source: String,
    pub timestamp_ns: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct SendStatus {
    pub message_hash: Option<String>,
    pub queued: bool,
    pub sent: bool,
    pub propagated: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Transfer {
    pub kind: String,
    pub done: bool,
    pub success: Option<bool>,
    pub cid: Option<String>,
    pub bytes: Option<u64>,
    pub total: Option<u64>,
    pub error: Option<String>,
}

#[derive(Default)]
struct Inner {
    next_seq: u64,
    topics: HashMap<String, VecDeque<ReceivedMessage>>,
    sends: HashMap<String, SendStatus>,
    send_order: VecDeque<String>,
    transfers: HashMap<String, Transfer>,
    transfer_order: VecDeque<String>,
    next_chain_seq: u64,
    lib_blocks: VecDeque<ChainBlock>,
}

#[derive(Default)]
pub struct EventStore {
    inner: Mutex<Inner>,
}

fn arg(data: &Value, i: usize) -> Option<&Value> {
    data.get(format!("arg{i}"))
}

fn arg_str(data: &Value, i: usize) -> Option<String> {
    arg(data, i).and_then(Value::as_str).map(str::to_string)
}

fn decode_bytes(v: &Value) -> Option<Vec<u8>> {
    let b = v.get("_bytes")?.as_str()?;
    URL_SAFE_NO_PAD.decode(b.trim_end_matches('=')).ok()
}

/// Evict the oldest key once a tracked map grows past its cap.
fn track<T: Default>(
    map: &mut HashMap<String, T>,
    order: &mut VecDeque<String>,
    key: &str,
    cap: usize,
) {
    if !map.contains_key(key) {
        map.insert(key.to_string(), T::default());
        order.push_back(key.to_string());
        while order.len() > cap {
            if let Some(old) = order.pop_front() {
                map.remove(&old);
            }
        }
    }
}

impl EventStore {
    /// Parse one NDJSON line and apply it. Unknown or malformed lines are ignored.
    pub fn ingest_line(&self, line: &str) {
        let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
            return;
        };
        let (Some(event), Some(data)) = (v.get("event").and_then(Value::as_str), v.get("data"))
        else {
            return;
        };
        self.apply(event, data);
    }

    /// Apply one event from any source. `data` holds the event's arguments, either
    /// named `arg0`, `arg1`… (logosctl watch) or as a positional array.
    pub fn apply_event(&self, event: &str, data: &Value) {
        match data {
            Value::Array(args) => {
                let named: serde_json::Map<String, Value> = args
                    .iter()
                    .enumerate()
                    .map(|(i, v)| (format!("arg{i}"), v.clone()))
                    .collect();
                self.apply(event, &Value::Object(named));
            }
            other => self.apply(event, other),
        }
    }

    fn apply(&self, event: &str, data: &Value) {
        let mut s = self.inner.lock().unwrap();
        let s = &mut *s;
        match event {
            "messageReceived" => {
                let (Some(hash), Some(topic)) = (arg_str(data, 0), arg_str(data, 1)) else {
                    return;
                };
                // The same message can arrive more than once (several delivery
                // paths, repeated subscriptions); keep the first copy only.
                if s.topics
                    .get(&topic)
                    .is_some_and(|q| q.iter().any(|m| m.hash == hash))
                {
                    return;
                }
                let payload = arg(data, 2).and_then(decode_bytes).unwrap_or_default();
                s.next_seq += 1;
                let msg = ReceivedMessage {
                    seq: s.next_seq,
                    hash,
                    text: String::from_utf8(payload.clone()).ok(),
                    payload_base64: base64::engine::general_purpose::STANDARD.encode(&payload),
                    source: arg_str(data, 3).unwrap_or_default(),
                    timestamp_ns: arg(data, 4).and_then(Value::as_i64),
                    topic: topic.clone(),
                };
                let q = s.topics.entry(topic).or_default();
                q.push_back(msg);
                while q.len() > MAX_MESSAGES_PER_TOPIC {
                    q.pop_front();
                }
            }
            "messageQueued" | "messageSent" | "messagePropagated" | "messageError" => {
                let Some(id) = arg_str(data, 0) else { return };
                track(&mut s.sends, &mut s.send_order, &id, MAX_TRACKED_SENDS);
                let st = s.sends.get_mut(&id).expect("just tracked");
                if let Some(h) = arg_str(data, 1) {
                    st.message_hash = Some(h);
                }
                match event {
                    "messageQueued" => st.queued = true,
                    "messageSent" => st.sent = true,
                    "messagePropagated" => st.propagated = true,
                    _ => {
                        st.error = Some(arg_str(data, 2).unwrap_or_else(|| "unknown error".into()))
                    }
                }
            }
            "libBlock" => {
                // One JSON document in arg0; "null" when its stream ended.
                let Some(v) = arg_str(data, 0).and_then(|t| serde_json::from_str::<Value>(&t).ok())
                else {
                    return;
                };
                let (Some(height), Some(header_id)) =
                    (v["height"].as_u64(), v["header_id"].as_str())
                else {
                    return;
                };
                s.next_chain_seq += 1;
                let block = ChainBlock {
                    seq: s.next_chain_seq,
                    height,
                    header_id: header_id.to_string(),
                };
                s.lib_blocks.push_back(block);
                while s.lib_blocks.len() > MAX_CHAIN_EVENTS {
                    s.lib_blocks.pop_front();
                }
            }
            "storageUploadProgress"
            | "storageUploadDone"
            | "storageDownloadProgress"
            | "storageDownloadDone" => {
                // Storage events carry a single JSON-encoded string payload.
                let Some(p) = arg_str(data, 0).and_then(|t| serde_json::from_str::<Value>(&t).ok())
                else {
                    return;
                };
                let Some(id) = p.get("sessionId").and_then(Value::as_str) else {
                    return;
                };
                track(
                    &mut s.transfers,
                    &mut s.transfer_order,
                    id,
                    MAX_TRACKED_TRANSFERS,
                );
                let t = s.transfers.get_mut(id).expect("just tracked");
                t.kind = if event.contains("Upload") {
                    "upload"
                } else {
                    "download"
                }
                .into();
                if let Some(b) = p.get("bytes").and_then(Value::as_u64) {
                    t.bytes = Some(b);
                }
                if let Some(total) = p.get("total").and_then(Value::as_u64) {
                    t.total = Some(total);
                }
                if let Some(cid) = p.get("cid").and_then(Value::as_str) {
                    t.cid = Some(cid.into());
                }
                if let Some(e) = p.get("error").and_then(Value::as_str) {
                    t.error = Some(e.into());
                }
                if event.ends_with("Done") {
                    t.done = true;
                    t.success = p.get("success").and_then(Value::as_bool);
                }
            }
            _ => {}
        }
    }

    /// Start tracking a transfer the moment its session id is known, so a
    /// status query before the first event reports "in progress", not unknown.
    pub fn expect_transfer(&self, session_id: &str, kind: &str) {
        let mut s = self.inner.lock().unwrap();
        let s = &mut *s;
        track(
            &mut s.transfers,
            &mut s.transfer_order,
            session_id,
            MAX_TRACKED_TRANSFERS,
        );
        let t = s.transfers.get_mut(session_id).expect("just tracked");
        if t.kind.is_empty() {
            t.kind = kind.into();
        }
    }

    /// Forget any earlier transfer under `session_id` before starting a new
    /// one. Downloads use the CID as their session id, so fetching the same
    /// CID twice would otherwise report the first run's finished result.
    pub fn reset_transfer(&self, session_id: &str, kind: &str) {
        let mut s = self.inner.lock().unwrap();
        let s = &mut *s;
        track(
            &mut s.transfers,
            &mut s.transfer_order,
            session_id,
            MAX_TRACKED_TRANSFERS,
        );
        *s.transfers.get_mut(session_id).expect("just tracked") = Transfer {
            kind: kind.into(),
            ..Transfer::default()
        };
    }

    /// Messages on `topic` with `seq > after`, oldest first, at most `limit`.
    pub fn read(&self, topic: &str, after: u64, limit: usize) -> Vec<ReceivedMessage> {
        let s = self.inner.lock().unwrap();
        s.topics
            .get(topic)
            .map(|q| {
                q.iter()
                    .filter(|m| m.seq > after)
                    .take(limit)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Finalized chain tips with `seq > after`, oldest first, at most `limit`.
    pub fn lib_blocks(&self, after: u64, limit: usize) -> Vec<ChainBlock> {
        let s = self.inner.lock().unwrap();
        s.lib_blocks
            .iter()
            .filter(|b| b.seq > after)
            .take(limit)
            .cloned()
            .collect()
    }

    pub fn send_status(&self, request_id: &str) -> Option<SendStatus> {
        self.inner.lock().unwrap().sends.get(request_id).cloned()
    }

    pub fn transfer(&self, session_id: &str) -> Option<Transfer> {
        self.inner
            .lock()
            .unwrap()
            .transfers
            .get(session_id)
            .cloned()
    }
}

/// Keep `logosctl watch <module>` running, feeding every line to `store`.
/// Restarts with backoff when the watcher exits (it exits when the daemon stops).
pub fn spawn_watcher(
    bin: PathBuf,
    config_dir: Option<PathBuf>,
    module: &'static str,
    store: Arc<EventStore>,
) {
    tokio::spawn(async move {
        let mut backoff = Duration::from_secs(1);
        loop {
            let started = std::time::Instant::now();
            let mut cmd = tokio::process::Command::new(&bin);
            cmd.args(["--json", "watch", module])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            if let Some(dir) = &config_dir {
                cmd.env("LOGOSCTL_CONFIG_DIR", dir);
            }
            match cmd.spawn() {
                Ok(mut child) => {
                    // Drain stderr concurrently: a full stderr pipe stalls stdout.
                    if let Some(err) = child.stderr.take() {
                        tokio::spawn(async move {
                            let mut lines = BufReader::new(err).lines();
                            while let Ok(Some(l)) = lines.next_line().await {
                                tracing::debug!(target: "watch", module, "{l}");
                            }
                        });
                    }
                    if let Some(out) = child.stdout.take() {
                        let mut lines = BufReader::new(out).lines();
                        while let Ok(Some(l)) = lines.next_line().await {
                            store.ingest_line(&l);
                        }
                    }
                    let status = child.wait().await;
                    tracing::warn!(module, ?status, "watcher exited; restarting in {backoff:?}");
                }
                Err(e) => tracing::error!(module, "cannot start watcher: {e}"),
            }
            // A watcher that ran a while was healthy: restart promptly. Keep the
            // cap short so a module loaded later (messaging_connect) is picked
            // up within seconds, since events missed meanwhile are gone.
            if started.elapsed() >= Duration::from_secs(10) {
                backoff = Duration::from_secs(1);
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(4));
        }
    });
}

#[cfg(test)]
mod tests;
