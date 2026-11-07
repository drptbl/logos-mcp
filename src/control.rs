//! What the user controls about the agent from outside MCP: which tool groups it
//! may use, and the record of what it did.

use std::{
    collections::{BTreeMap, VecDeque},
    io::Write as _,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Which groups of tools the agent may use, all on unless the user turns one
/// off, and whether each transfer waits for the user's approval (off unless
/// turned on). `node_status` belongs to no group and is always available.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolGroups {
    pub messaging: bool,
    pub storage: bool,
    pub wallet: bool,
    pub transfers: bool,
    pub discovery: bool,
    pub ask_before_transfer: bool,
}

impl Default for ToolGroups {
    fn default() -> Self {
        Self {
            messaging: true,
            storage: true,
            wallet: true,
            transfers: true,
            discovery: true,
            ask_before_transfer: false,
        }
    }
}

impl ToolGroups {
    /// The group a tool belongs to, by its name.
    pub fn group_of(tool: &str) -> Option<&'static str> {
        match tool {
            "wallet_transfer" | "wallet_transfer_status" => Some("transfers"),
            "rln_status" => Some("messaging"),
            t if t.starts_with("messaging_") => Some("messaging"),
            t if t.starts_with("storage_") => Some("storage"),
            t if t.starts_with("wallet_") => Some("wallet"),
            t if t.starts_with("discovery_") => Some("discovery"),
            _ => None,
        }
    }

    fn flag(&mut self, group: &str) -> Option<&mut bool> {
        match group {
            "messaging" => Some(&mut self.messaging),
            "storage" => Some(&mut self.storage),
            "wallet" => Some(&mut self.wallet),
            "transfers" => Some(&mut self.transfers),
            "discovery" => Some(&mut self.discovery),
            "ask_before_transfer" => Some(&mut self.ask_before_transfer),
            _ => None,
        }
    }

    pub fn allows(&self, tool: &str) -> bool {
        match Self::group_of(tool) {
            Some("messaging") => self.messaging,
            Some("storage") => self.storage,
            Some("wallet") => self.wallet,
            Some("transfers") => self.transfers,
            Some("discovery") => self.discovery,
            _ => true,
        }
    }

    pub fn set(&mut self, group: &str, on: bool) -> Result<(), String> {
        let flag = self
            .flag(group)
            .ok_or_else(|| format!("unknown tool group `{group}`"))?;
        *flag = on;
        Ok(())
    }
}

/// How long a transfer request waits for the user. After that it expires and is
/// never sent.
pub const APPROVAL_EXPIRES: Duration = Duration::from_secs(10 * 60);
/// Requests that may wait for the user at once; more are refused.
pub const MAX_WAITING: usize = 20;
/// Answered requests kept for wallet_transfer_status, beyond those still open.
const KEEP_DECIDED: usize = 100;

/// Where a transfer request stands. `Waiting` and `Approved` (sending) are open;
/// every other state is final.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Decision {
    Waiting,
    Approved,
    Sent {
        tx_hash: String,
    },
    Failed {
        reason: String,
    },
    /// Approved and handed to the wallet, which gave no clear answer: it may have
    /// been sent. Never safe to retry without checking the balances.
    Unknown {
        reason: String,
    },
    Denied,
    Expired,
    Cancelled,
}

impl Decision {
    pub fn is_final(&self) -> bool {
        !matches!(self, Self::Waiting | Self::Approved)
    }
}

/// A transfer the agent asked for, waiting for the user's answer.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TransferRequest {
    pub id: String,
    pub from: String,
    pub to: String,
    pub amount_lepta: String,
    /// Unix seconds.
    pub requested_at: u64,
}

struct Pending {
    request: TransferRequest,
    expires: tokio::time::Instant,
    decision: tokio::sync::watch::Sender<Decision>,
}

/// Transfer requests waiting for the user, kept in memory only: a restart drops
/// them, and nothing dropped is ever sent.
#[derive(Default)]
pub struct Approvals {
    pending: Mutex<BTreeMap<String, Pending>>,
    next: AtomicU64,
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl Approvals {
    /// File a request. The receiver sees every change to its decision.
    pub fn request(
        &self,
        from: &str,
        to: &str,
        amount_lepta: &str,
    ) -> Result<(String, tokio::sync::watch::Receiver<Decision>), String> {
        // Unique across restarts too, so an old id never names a new request.
        let id = format!(
            "{:x}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
            self.next.fetch_add(1, Ordering::Relaxed)
        );
        let (decision, watch) = tokio::sync::watch::channel(Decision::Waiting);
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        Self::tidy(&mut pending);
        let open = pending
            .values()
            .filter(|p| !p.decision.borrow().is_final())
            .count();
        if open >= MAX_WAITING {
            return Err(format!(
                "{MAX_WAITING} transfers are already waiting for the user: wait for answers first"
            ));
        }
        pending.insert(
            id.clone(),
            Pending {
                request: TransferRequest {
                    id: id.clone(),
                    from: from.to_string(),
                    to: to.to_string(),
                    amount_lepta: amount_lepta.to_string(),
                    requested_at: unix_now(),
                },
                expires: tokio::time::Instant::now() + APPROVAL_EXPIRES,
                decision,
            },
        );
        Ok((id, watch))
    }

    /// Expire what waited too long, and keep only the latest answered requests.
    fn tidy(pending: &mut BTreeMap<String, Pending>) {
        let now = tokio::time::Instant::now();
        for p in pending.values() {
            if *p.decision.borrow() == Decision::Waiting && now >= p.expires {
                p.decision.send_replace(Decision::Expired);
            }
        }
        let decided: Vec<String> = pending
            .iter()
            .filter(|(_, p)| p.decision.borrow().is_final())
            .map(|(id, _)| id.clone())
            .collect();
        for id in decided
            .iter()
            .take(decided.len().saturating_sub(KEEP_DECIDED))
        {
            pending.remove(id);
        }
    }

    /// Move a waiting request on, or say why it can't be.
    fn answer(&self, id: &str, to: Decision) -> Result<TransferRequest, String> {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        Self::tidy(&mut pending);
        let p = pending.get(id).ok_or("no such transfer request")?;
        match &*p.decision.borrow() {
            Decision::Waiting => {}
            Decision::Expired => return Err("this request expired and will not be sent".into()),
            Decision::Cancelled => return Err("the agent cancelled this request".into()),
            _ => return Err("this request was already answered".into()),
        }
        p.decision.send_replace(to);
        Ok(p.request.clone())
    }

    /// The user approved: the caller sends it, then reports with `finish`.
    pub fn approve(&self, id: &str) -> Result<TransferRequest, String> {
        self.answer(id, Decision::Approved)
    }

    pub fn deny(&self, id: &str) -> Result<TransferRequest, String> {
        self.answer(id, Decision::Denied)
    }

    /// The agent gave up waiting. A request still waiting is cancelled and never
    /// sent; one the user already approved goes ahead. Returns where it stands.
    pub fn cancel(&self, id: &str) -> Decision {
        let _ = self.answer(id, Decision::Cancelled);
        let pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        pending
            .get(id)
            .map_or(Decision::Cancelled, |p| p.decision.borrow().clone())
    }

    /// How an approved request's send ended.
    pub fn finish(&self, id: &str, outcome: Decision) {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = pending.get(id) {
            p.decision.send_replace(outcome);
        }
        Self::tidy(&mut pending);
    }

    /// A request and where it stands, for wallet_transfer_status.
    pub fn status(&self, id: &str) -> Option<Value> {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        Self::tidy(&mut pending);
        let p = pending.get(id)?;
        let mut status = serde_json::to_value(&p.request).ok()?;
        if let (Some(status), Ok(Value::Object(decision))) = (
            status.as_object_mut(),
            serde_json::to_value(&*p.decision.borrow()),
        ) {
            status.extend(decision);
        }
        Some(status)
    }

    /// Requests waiting for the user, oldest first.
    pub fn waiting(&self) -> Vec<TransferRequest> {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        Self::tidy(&mut pending);
        let mut waiting: Vec<TransferRequest> = pending
            .values()
            .filter(|p| *p.decision.borrow() == Decision::Waiting)
            .map(|p| p.request.clone())
            .collect();
        waiting.sort_by_key(|r| r.requested_at);
        waiting
    }
}

/// How much of a call's arguments and result an entry keeps.
pub const MAX_DETAIL_CHARS: usize = 200;
/// Entries kept in memory, and read back from the file on start.
const KEEP: usize = 500;
/// The file is moved to `activity.1.jsonl` once it grows past this.
pub const ROTATE_BYTES: u64 = 1_000_000;

/// One tool call the agent made.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub seq: u64,
    /// Unix seconds.
    pub time: u64,
    pub tool: String,
    pub ok: bool,
    pub args: String,
    pub result: String,
}

/// The record of the agent's tool calls: the latest in memory and, when opened
/// on a file, every one appended to it.
pub struct ActivityLog {
    inner: Mutex<Inner>,
}

struct Inner {
    entries: VecDeque<Entry>,
    next_seq: u64,
    file: Option<PathBuf>,
}

/// What the file keeps of a call: everything but message contents. A sent
/// message keeps its topic and size, a read keeps how many messages came back.
fn redact(tool: &str, args: &Value, result: &str) -> (Value, String) {
    let mut args = args.clone();
    if tool == "messaging_send"
        && let Some(map) = args.as_object_mut()
    {
        // Whatever the type: a malformed payload is still someone's message.
        for key in ["text", "base64"] {
            if let Some(body) = map.get(key) {
                let size = match body {
                    Value::String(text) => text.chars().count(),
                    other => other.to_string().chars().count(),
                };
                map.insert(
                    key.to_string(),
                    Value::String(format!("<hidden, {size} chars>")),
                );
            }
        }
    }
    let result = match serde_json::from_str::<Value>(result) {
        Ok(read) if tool == "messaging_read" => match read["messages"].as_array() {
            Some(messages) => format!(
                "{} messages, next_after {}",
                messages.len(),
                read["next_after"]
            ),
            None => result.to_string(),
        },
        _ => result.to_string(),
    };
    (args, result)
}

fn cut(text: &str) -> String {
    let mut chars = text.chars();
    let mut out: String = chars.by_ref().take(MAX_DETAIL_CHARS).collect();
    if chars.next().is_some() {
        out.push('…');
    }
    out
}

impl ActivityLog {
    pub fn in_memory() -> Self {
        Self::from_parts(VecDeque::new(), None)
    }

    /// The log kept in `file`, with its latest entries read back, from the
    /// rotated file too. A line a crash left half written is skipped and closed
    /// off, so it neither stops the module starting nor swallows the next entry.
    pub fn open(file: PathBuf) -> Result<Self, String> {
        let mut entries = read_entries(&rotated(&file))?;
        entries.extend(read_entries(&file)?);
        if std::fs::read(&file).is_ok_and(|b| !b.is_empty() && !b.ends_with(b"\n")) {
            std::fs::OpenOptions::new()
                .append(true)
                .open(&file)
                .and_then(|mut f| f.write_all(b"\n"))
                .map_err(|e| format!("cannot repair {}: {e}", file.display()))?;
        }
        let skip = entries.len().saturating_sub(KEEP);
        Ok(Self::from_parts(
            entries.into_iter().skip(skip).collect(),
            Some(file),
        ))
    }

    fn from_parts(entries: VecDeque<Entry>, file: Option<PathBuf>) -> Self {
        let next_seq = entries.back().map_or(1, |e| e.seq + 1);
        Self {
            inner: Mutex::new(Inner {
                entries,
                next_seq,
                file,
            }),
        }
    }

    /// Record a call. The app's view keeps message text while the module runs;
    /// the file gets a copy without it, so it never outlives the session.
    pub fn record(&self, tool: &str, args: &Value, ok: bool, result: &str) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        let entry = Entry {
            seq: inner.next_seq,
            time: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            tool: cut(tool),
            ok,
            args: cut(&args.to_string()),
            result: cut(result),
        };
        inner.next_seq += 1;
        if let Some(file) = &inner.file {
            let (args, result) = redact(tool, args, result);
            let on_disk = Entry {
                args: cut(&args.to_string()),
                result: cut(&result),
                ..entry.clone()
            };
            if let Err(e) = append(file, &on_disk) {
                tracing::warn!("could not write the activity log: {e}");
            }
        }
        inner.entries.push_back(entry);
        if inner.entries.len() > KEEP {
            inner.entries.pop_front();
        }
    }

    /// The newest sequence number, or 0 when nothing was recorded.
    pub fn latest(&self) -> u64 {
        self.inner
            .lock()
            .map(|inner| inner.next_seq - 1)
            .unwrap_or_default()
    }

    /// Entries with a sequence number above `after`, oldest first.
    pub fn since(&self, after: u64) -> Vec<Entry> {
        self.inner
            .lock()
            .map(|inner| {
                inner
                    .entries
                    .iter()
                    .filter(|e| e.seq > after)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn rotated(file: &Path) -> PathBuf {
    file.with_file_name("activity.1.jsonl")
}

/// The entries in `file`, oldest first. A missing file has none.
fn read_entries(file: &Path) -> Result<Vec<Entry>, String> {
    match std::fs::read(file) {
        Ok(bytes) => Ok(String::from_utf8_lossy(&bytes)
            .lines()
            .filter_map(|line| serde_json::from_str::<Entry>(line).ok())
            .collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("cannot read {}: {e}", file.display())),
    }
}

fn append(file: &Path, entry: &Entry) -> std::io::Result<()> {
    if std::fs::metadata(file).is_ok_and(|m| m.len() >= ROTATE_BYTES) {
        std::fs::rename(file, rotated(file))?;
    }
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut line = serde_json::to_string(entry)?;
    line.push('\n');
    options.open(file)?.write_all(line.as_bytes())
}

#[cfg(test)]
mod tests;
