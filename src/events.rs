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
/// Removals remembered, each CID's latest: enough that the agent's content
/// records are checked against them long after.
const MAX_REMOVALS: usize = 4096;
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
    /// The payload, only when it isn't text: never sent twice.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_base64: Option<String>,
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
    /// When it finished, in milliseconds since the Unix epoch: what its
    /// content is recorded as stored at, however late the recorder looks.
    #[serde(skip)]
    pub done_at_ms: Option<u64>,
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
    /// Where this run's seqs begin: its start time in milliseconds, times
    /// 1000. A later run starts above anything an earlier one handed out, so
    /// a reader's after_seq from before a restart is always below it.
    run_base: u64,
    next_seq: u64,
    topics: HashMap<String, VecDeque<ReceivedMessage>>,
    sends: HashMap<String, SendStatus>,
    send_order: VecDeque<String>,
    transfers: HashMap<String, Transfer>,
    transfer_order: VecDeque<String>,
    /// Sessions a server sharing this store started, kept apart from the
    /// transfers themselves: in Basecamp the store also sees the user's own,
    /// which can push the agent's out of `transfers`.
    /// With each, its final state once it finished, kept apart: a download's
    /// session is its CID, so the user fetching the same CID later updates
    /// the live transfer, never what the agent reads.
    started: HashMap<String, Option<Transfer>>,
    started_order: VecDeque<String>,
    /// When each CID was last removed from local storage, by anyone, in
    /// milliseconds since the Unix epoch.
    removed: HashMap<String, u64>,
    removed_order: VecDeque<String>,
    next_chain_seq: u64,
    lib_blocks: VecDeque<ChainBlock>,
    /// The newest seq dropped from each topic's buffer, to tell a reader it missed some.
    dropped_upto: HashMap<String, u64>,
    chain_dropped_upto: u64,
}

/// Where a reader's `after` stands against what this store holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cursor {
    /// Nothing between `after` and what is held was lost.
    Fine,
    /// `after` is beyond anything this store has handed out: the server
    /// restarted and its seqs began again.
    Restarted,
    /// Items after `after` were dropped from the buffer before being read.
    Dropped,
}

pub struct EventStore {
    inner: Mutex<Inner>,
    /// Told of each finished removal: the CID and when, in milliseconds.
    on_removed: Mutex<Vec<RemovalHook>>,
    /// Woken by every event, for readers waiting for something new.
    changed: tokio::sync::Notify,
}

impl Default for EventStore {
    fn default() -> Self {
        // Under 2^53 for centuries, so JSON clients read seqs exactly.
        let run_base = unix_ms().saturating_mul(1000);
        Self {
            inner: Mutex::new(Inner {
                run_base,
                next_seq: run_base,
                next_chain_seq: run_base,
                ..Inner::default()
            }),
            on_removed: Mutex::default(),
            changed: tokio::sync::Notify::new(),
        }
    }
}

type RemovalHook = Box<dyn Fn(&str, u64) + Send + Sync>;

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
        self.apply_one(event, data);
        self.changed.notify_waiters();
    }

    /// Wait until `ready` gives something, `wait` passes or `ct` is cancelled,
    /// and return what it gave last. Each event wakes it to look again.
    pub async fn wait_for<T>(
        &self,
        wait: std::time::Duration,
        ct: &tokio_util::sync::CancellationToken,
        mut ready: impl FnMut() -> (T, bool),
    ) -> T {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            // Registered before looking, so an event in between still wakes it.
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let (found, enough) = ready();
            if enough || tokio::time::Instant::now() >= deadline {
                return found;
            }
            tokio::select! {
                () = &mut changed => {}
                () = tokio::time::sleep_until(deadline) => {}
                () = ct.cancelled() => return found,
            }
        }
    }

    fn apply_one(&self, event: &str, data: &Value) {
        if event == "storageRemoveDone" {
            self.removed(data);
            return;
        }
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
                    payload_base64: String::from_utf8(payload.clone())
                        .is_err()
                        .then(|| base64::engine::general_purpose::STANDARD.encode(&payload)),
                    source: arg_str(data, 3).unwrap_or_default(),
                    timestamp_ns: arg(data, 4).and_then(Value::as_i64),
                    topic: topic.clone(),
                };
                let q = s.topics.entry(topic.clone()).or_default();
                q.push_back(msg);
                let mut dropped = None;
                while q.len() > MAX_MESSAGES_PER_TOPIC {
                    dropped = q.pop_front().map(|m| m.seq);
                }
                if let Some(seq) = dropped {
                    s.dropped_upto.insert(topic, seq);
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
                    if let Some(gone) = s.lib_blocks.pop_front() {
                        s.chain_dropped_upto = gone.seq;
                    }
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
                    t.done_at_ms = Some(unix_ms());
                    t.success = p.get("success").and_then(Value::as_bool);
                    if let Some(kept @ None) = s.started.get_mut(id) {
                        *kept = Some(t.clone());
                    }
                }
            }
            _ => {}
        }
    }

    /// Start tracking a transfer the moment its session id is known, so a
    /// status query before the first event reports "in progress", not unknown.
    /// `asked_ms` is when the transfer was asked for: a finished one under the
    /// same session id from before then is an earlier run's, as when storage
    /// restarted and hands out its ids again.
    pub fn expect_transfer(&self, session_id: &str, kind: &str, asked_ms: u64) {
        let mut s = self.inner.lock().unwrap();
        let s = &mut *s;
        track(
            &mut s.started,
            &mut s.started_order,
            session_id,
            MAX_TRACKED_TRANSFERS,
        );
        track(
            &mut s.transfers,
            &mut s.transfer_order,
            session_id,
            MAX_TRACKED_TRANSFERS,
        );
        let t = s.transfers.get_mut(session_id).expect("just tracked");
        let earlier = |t: &Transfer| t.done && t.done_at_ms.is_some_and(|at| at < asked_ms);
        if earlier(t) {
            *t = Transfer::default();
        }
        if t.kind.is_empty() {
            t.kind = kind.into();
        }
        let kept = s.started.get_mut(session_id).expect("just tracked");
        if kept.as_ref().is_some_and(earlier) {
            *kept = None;
        }
        // It may have finished, this run, before this was called.
        if kept.is_none() {
            *kept = t.done.then(|| t.clone());
        }
    }

    /// Forget any earlier transfer under `session_id` before starting a new
    /// one. Downloads use the CID as their session id, so fetching the same
    /// CID twice would otherwise report the first run's finished result.
    pub fn reset_transfer(&self, session_id: &str, kind: &str) {
        let mut s = self.inner.lock().unwrap();
        let s = &mut *s;
        track(
            &mut s.started,
            &mut s.started_order,
            session_id,
            MAX_TRACKED_TRANSFERS,
        );
        track(
            &mut s.transfers,
            &mut s.transfer_order,
            session_id,
            MAX_TRACKED_TRANSFERS,
        );
        // A new run: what an earlier one ended with is no longer the answer.
        s.started.insert(session_id.to_string(), None);
        *s.transfers.get_mut(session_id).expect("just tracked") = Transfer {
            kind: kind.into(),
            ..Transfer::default()
        };
    }

    /// Messages on `topic` with `seq > after`, oldest first, at most `limit`,
    /// with where `after` stood: from the start again when it predates a
    /// restart. Checked and read in one step, so no drop slips between them.
    pub fn read(&self, topic: &str, after: u64, limit: usize) -> (Cursor, Vec<ReceivedMessage>) {
        let s = self.inner.lock().unwrap();
        let cursor = if after > s.next_seq || (after != 0 && after < s.run_base) {
            Cursor::Restarted
        } else if s.dropped_upto.get(topic).is_some_and(|d| after < *d) {
            Cursor::Dropped
        } else {
            Cursor::Fine
        };
        let after = if cursor == Cursor::Restarted {
            0
        } else {
            after
        };
        let messages = s
            .topics
            .get(topic)
            .map(|q| {
                q.iter()
                    .filter(|m| m.seq > after)
                    .take(limit)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        (cursor, messages)
    }

    /// Finalized chain tips with `seq > after`, oldest first, at most `limit`,
    /// with where `after` stood, as `read` does for messages.
    pub fn lib_blocks(&self, after: u64, limit: usize) -> (Cursor, Vec<ChainBlock>) {
        let s = self.inner.lock().unwrap();
        let cursor = if after > s.next_chain_seq || (after != 0 && after < s.run_base) {
            Cursor::Restarted
        } else if after < s.chain_dropped_upto {
            Cursor::Dropped
        } else {
            Cursor::Fine
        };
        let after = if cursor == Cursor::Restarted {
            0
        } else {
            after
        };
        let blocks = s
            .lib_blocks
            .iter()
            .filter(|b| b.seq > after)
            .take(limit)
            .cloned()
            .collect();
        (cursor, blocks)
    }

    pub fn send_status(&self, request_id: &str) -> Option<SendStatus> {
        self.inner.lock().unwrap().sends.get(request_id).cloned()
    }

    /// Settle a transfer this server stopped, so it never reads as a success.
    pub fn stop_transfer(&self, session_id: &str, error: String) {
        let mut s = self.inner.lock().unwrap();
        let s = &mut *s;
        let stop = |t: &mut Transfer| {
            t.done = true;
            t.success = Some(false);
            t.error = Some(error.clone());
        };
        // The agent's own record of it, not the live one, which someone
        // else's later transfer of the same CID may have moved on.
        if let Some(kept) = s.started.get_mut(session_id) {
            let mut ended = match kept.take() {
                Some(ended) => ended,
                None => s.transfers.get(session_id).cloned().unwrap_or_default(),
            };
            stop(&mut ended);
            *kept = Some(ended);
        }
        if let Some(t) = s.transfers.get_mut(session_id) {
            stop(t);
        }
    }

    /// Note a finished removal, then tell whoever asked, outside the lock.
    fn removed(&self, data: &Value) {
        let Some(p) = arg_str(data, 0).and_then(|t| serde_json::from_str::<Value>(&t).ok()) else {
            return;
        };
        let (Some(true), Some(cid)) = (
            p.get("success").and_then(Value::as_bool),
            p.get("cid").and_then(Value::as_str),
        ) else {
            return;
        };
        let at = unix_ms();
        {
            let mut s = self.inner.lock().unwrap();
            let s = &mut *s;
            track(&mut s.removed, &mut s.removed_order, cid, MAX_REMOVALS);
            s.removed.insert(cid.to_string(), at);
        }
        for hook in self.on_removed.lock().unwrap().iter() {
            hook(cid, at);
        }
    }

    /// Call `hook` with the CID and time of every removal from now on.
    pub fn on_removed(&self, hook: impl Fn(&str, u64) + Send + Sync + 'static) {
        self.on_removed.lock().unwrap().push(Box::new(hook));
    }

    /// When `cid` was last removed from local storage, in milliseconds since
    /// the Unix epoch, if this store saw it go.
    pub fn removed_at_ms(&self, cid: &str) -> Option<u64> {
        self.inner.lock().unwrap().removed.get(cid).copied()
    }

    /// Whether a server sharing this store started `session_id`.
    pub fn started_here(&self, session_id: &str) -> bool {
        self.inner.lock().unwrap().started.contains_key(session_id)
    }

    /// A transfer a server sharing this store started, as it stands, or as it
    /// ended once it finished. None for anyone else's.
    pub fn agent_transfer(&self, session_id: &str) -> Option<Transfer> {
        let s = self.inner.lock().unwrap();
        match s.started.get(session_id)? {
            Some(ended) => Some(ended.clone()),
            None => s.transfers.get(session_id).cloned(),
        }
    }

    /// Take back `started_here` for a transfer that never started.
    pub fn never_started(&self, session_id: &str) {
        let mut s = self.inner.lock().unwrap();
        s.started.remove(session_id);
        s.started_order.retain(|id| id != session_id);
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

/// Milliseconds since the Unix epoch.
pub fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
