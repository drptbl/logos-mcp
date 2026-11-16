//! Room approval: a transfer over a limit waits for trusted humans in a Muster
//! room to approve exactly that call, read from the agent's own Muster.

use std::{collections::BTreeSet, time::Duration};

use serde_json::{Value, json};

use crate::{
    logosctl::{Arg, embedded_json},
    node::Node,
};

const MUSTER: &str = "muster_module";
/// What every proposal's `ref` starts with, the request id following it.
pub const REF_PREFIX: &str = "logos-mcp/";
/// How long the first proposal waits for Muster's delivery node: a send made
/// while it starts is lost (corpetty/muster#373).
const DELIVERY_WAIT: Duration = Duration::from_secs(90);
const DELIVERY_POLL: Duration = Duration::from_secs(2);

/// What the user set for room approval: which room, whose approval counts, how
/// many are needed, and which transfers need them.
#[derive(Debug, Clone, PartialEq)]
pub struct RoomApproval {
    /// The Muster room's content topic.
    pub topic: String,
    /// Trusted humans' Ed25519 keys, 64 lowercase hex characters each.
    approvers: BTreeSet<String>,
    /// Distinct trusted approvals a transfer needs.
    pub approvals: usize,
    /// How long a proposal waits for the room before it expires unsent.
    pub expires: Duration,
}

/// An Ed25519 key as Muster names it, `ed:<64 hex>` or the bare hex: the hex,
/// lowercased.
fn ed_key(input: &str) -> Option<String> {
    let hex = input.trim();
    let hex = hex.strip_prefix("ed:").unwrap_or(hex);
    (hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| hex.to_ascii_lowercase())
}

/// The Ed25519 key in an account as Muster names it: `ed:<hex>`, the bare hex,
/// or a 128-hex roster identity, whose first half is the Ed25519 key.
pub(crate) fn member_key(account: &str) -> Option<String> {
    let account = account.trim();
    match account.len() {
        128 => account.get(..64).and_then(ed_key),
        _ => ed_key(account),
    }
}

impl RoomApproval {
    pub fn new(
        topic: &str,
        approvers: &[String],
        approvals: usize,
        expires_mins: u64,
    ) -> Result<Self, String> {
        crate::guard::validate_topic(topic).map_err(|e| format!("room topic: {e}"))?;
        let approvers = approvers
            .iter()
            .map(|a| {
                ed_key(a).ok_or_else(|| {
                    format!(
                        "room approver `{a}` is not an Ed25519 key: give ed:<64 hex characters>"
                    )
                })
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        if approvers.is_empty() {
            return Err("room approval needs at least one approver".into());
        }
        if approvals == 0 {
            return Err("room approval needs at least 1 approval".into());
        }
        if approvals > approvers.len() {
            return Err(format!(
                "{approvals} approvals can never come from {} distinct approvers",
                approvers.len()
            ));
        }
        if expires_mins == 0 {
            return Err("a room proposal must be allowed at least a minute".into());
        }
        Ok(Self {
            topic: topic.to_string(),
            approvers,
            approvals,
            expires: Duration::from_secs(expires_mins.saturating_mul(60)),
        })
    }

    /// Whether `account` (`ed:<hex>`, bare hex, or Muster's 128-hex roster
    /// identity, whose first half is the Ed25519 key) is a trusted approver.
    pub fn trusts(&self, account: &str) -> bool {
        member_key(account).is_some_and(|k| self.approvers.contains(&k))
    }
}

/// The call a Muster invoke intent's signed bytes commit to.
#[derive(Debug, Clone, PartialEq)]
pub struct SignedCall {
    pub schema_id: String,
    pub module: String,
    pub method: String,
    pub args: Value,
}

/// The domain Muster's invoke driver signs under.
const INVOKE_DOMAIN: &str = "muster.invoke.v1";

/// The few CBOR shapes an invoke materialization uses.
#[derive(Debug, Clone, PartialEq)]
enum Cbor {
    Uint(u64),
    Bytes(Vec<u8>),
    Text(String),
    Array(Vec<Cbor>),
    Map(Vec<(Cbor, Cbor)>),
}

/// Decode one CBOR item at `*at`. Definite lengths only, no tags: anything an
/// invoke materialization doesn't use is refused rather than guessed at.
fn cbor(b: &[u8], at: &mut usize, depth: usize) -> Result<Cbor, String> {
    if depth > 8 {
        return Err("CBOR nested too deep".into());
    }
    let head = *b.get(*at).ok_or("CBOR ends early")?;
    *at += 1;
    let (major, info) = (head >> 5, head & 31);
    let n: u64 = match info {
        0..=23 => u64::from(info),
        24..=27 => {
            let width = 1usize << (info - 24);
            let bytes = b.get(*at..*at + width).ok_or("CBOR ends early")?;
            *at += width;
            bytes.iter().fold(0, |n, x| (n << 8) | u64::from(*x))
        }
        _ => return Err("indefinite or reserved CBOR length".into()),
    };
    let len = usize::try_from(n).map_err(|_| "CBOR length too large")?;
    let take = |at: &mut usize| -> Result<Vec<u8>, String> {
        let end = at.checked_add(len).ok_or("CBOR length too large")?;
        let bytes = b.get(*at..end).ok_or("CBOR ends early")?.to_vec();
        *at = end;
        Ok(bytes)
    };
    Ok(match major {
        0 => Cbor::Uint(n),
        2 => Cbor::Bytes(take(at)?),
        3 => Cbor::Text(String::from_utf8(take(at)?).map_err(|_| "CBOR text is not UTF-8")?),
        4 => Cbor::Array(
            (0..len.min(b.len()))
                .map(|_| cbor(b, at, depth + 1))
                .collect::<Result<_, _>>()?,
        ),
        5 => Cbor::Map(
            (0..len.min(b.len()))
                .map(|_| Ok((cbor(b, at, depth + 1)?, cbor(b, at, depth + 1)?)))
                .collect::<Result<_, String>>()?,
        ),
        _ => return Err(format!("CBOR major type {major} is not used by an invoke")),
    })
}

/// The call an invoke intent's `txhash` commits to: Muster's raw signed bytes,
/// `["muster.invoke.v1", schemaId, {args: <JSON text>, method, module}]`, with
/// nothing else in them.
pub fn signed_call(txhash: &str) -> Result<SignedCall, String> {
    let hex = txhash.strip_prefix("0x").unwrap_or(txhash);
    if !hex.len().is_multiple_of(2) {
        return Err("txhash is not hex".into());
    }
    let bytes = (0..hex.len() / 2)
        .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16))
        .collect::<Result<Vec<u8>, _>>()
        .map_err(|_| "txhash is not hex")?;
    let mut at = 0;
    let item = cbor(&bytes, &mut at, 0)?;
    if at != bytes.len() {
        return Err("txhash has bytes after the call".into());
    }
    let Cbor::Array(parts) = item else {
        return Err("txhash is not an invoke materialization".into());
    };
    let [Cbor::Text(domain), Cbor::Text(schema_id), Cbor::Map(fields)] = parts.as_slice() else {
        return Err("txhash is not an invoke materialization".into());
    };
    if domain != INVOKE_DOMAIN {
        return Err(format!(
            "txhash is signed under {domain}, not {INVOKE_DOMAIN}"
        ));
    }
    let mut found = std::collections::BTreeMap::new();
    for (key, value) in fields {
        let (Cbor::Text(key), Cbor::Text(value)) = (key, value) else {
            return Err("txhash fields are not text".into());
        };
        if found.insert(key.as_str(), value.as_str()).is_some() {
            return Err(format!("txhash repeats the field {key}"));
        }
    }
    let keys: Vec<&str> = found.keys().copied().collect();
    if keys != ["args", "method", "module"] {
        return Err(format!(
            "txhash carries the fields {keys:?}, not exactly args, method and module"
        ));
    }
    Ok(SignedCall {
        schema_id: schema_id.clone(),
        module: found["module"].to_string(),
        method: found["method"].to_string(),
        args: serde_json::from_str(found["args"]).map_err(|_| "txhash args are not JSON")?,
    })
}

/// A lez_core call put to the room: what the room signs. logos-mcp sends the
/// same call, built from the same request, once the room approves it.
#[derive(Debug, Clone, PartialEq)]
pub struct RoomCall {
    pub module: &'static str,
    pub method: &'static str,
    /// The call's arguments, as the room signs them.
    pub args: Value,
    /// What the call does, in words, for the people approving it. Code builds
    /// it from the same terms as the call, never the agent.
    pub note: String,
}

/// What the agent's Muster says about a proposed transfer.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Trusted humans approved exactly this call: send it.
    Send,
    /// Not yet: Muster's state, and the trusted approvals counted so far.
    Wait { state: String, approvals: usize },
    /// The room declined it, or too many trusted humans did for it to pass.
    Declined,
    /// It can never be sent, and why.
    Refused(String),
}

pub(crate) fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// Judge proposal `intent_id` for `call` from the agent's own
/// Muster: `intents` from `coordinate_intents`, `provenance` from
/// `coordinate_provenance`, at `now` (Unix seconds).
///
/// It passes only when Muster folded it executable, it is bound to the
/// configured room and unexpired, its signed bytes are exactly this call, and
/// enough distinct trusted humans approved it. A trusted approval counts only
/// when the room's provenance has its `sig` row and the intent's own record
/// grades it `committed`, attested in-app for this intent: a signature pasted
/// in from another proposal for the same call is graded `unattested`.
pub fn judge(
    room: &RoomApproval,
    intent_id: &str,
    call: &RoomCall,
    intents: &Value,
    provenance: &Value,
    now: u64,
) -> Verdict {
    let empty = Vec::new();
    let Some(intent) = intents
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .find(|i| text(i, "id") == intent_id)
    else {
        return Verdict::Wait {
            state: "not seen yet".into(),
            approvals: 0,
        };
    };
    let state = text(intent, "state");
    match state {
        "declined" => return Verdict::Declined,
        "proposed" | "collecting" | "executable" => {}
        other => {
            return Verdict::Refused(format!(
                "the room's proposal is {other}, so it can't be sent from here"
            ));
        }
    }
    let declined: BTreeSet<String> = intent
        .get("decliners")
        .and_then(Value::as_array)
        .unwrap_or(&empty)
        .iter()
        .filter_map(Value::as_str)
        .filter_map(member_key)
        .filter(|k| room.approvers.contains(k))
        .collect();
    if room.approvers.len() - declined.len() < room.approvals {
        return Verdict::Declined;
    }
    match signed_call(text(intent, "txhash")) {
        Ok(signed)
            if signed.schema_id == format!("muster.invoke.{}.{}.v1", call.module, call.method)
                && signed.module == call.module
                && signed.method == call.method
                && signed.args == call.args => {}
        Ok(signed) => {
            return Verdict::Refused(format!(
                "the room's proposal is not this transfer: it calls {}.{}{}",
                signed.module, signed.method, signed.args
            ));
        }
        Err(e) => {
            return Verdict::Refused(format!("the room's proposal is not this transfer: {e}"));
        }
    }
    let rows = provenance.as_array().unwrap_or(&empty);
    let of_intent = |kind: &'static str| {
        rows.iter()
            .filter(move |r| text(r, "kind") == kind && text(r, "intentId") == intent_id)
    };
    let mut contexts = of_intent("context").peekable();
    if contexts.peek().is_none() {
        return Verdict::Wait {
            state: state.into(),
            approvals: 0,
        };
    }
    for row in contexts {
        let context: Value = serde_json::from_str(text(row, "detail")).unwrap_or_default();
        if text(&context, "account") != room.topic {
            return Verdict::Refused("the proposal is bound to another room".into());
        }
        match text(&context, "expiry").parse::<u64>() {
            Ok(expiry) if now < expiry => {}
            _ => return Verdict::Refused("the proposal expired in Muster".into()),
        }
    }
    let signed: BTreeSet<String> = of_intent("sig")
        .filter_map(|r| member_key(text(r, "account")))
        .collect();
    let committed: BTreeSet<String> = intent
        .get("provenance")
        .and_then(Value::as_array)
        .unwrap_or(&empty)
        .iter()
        .filter(|p| {
            text(p, "class") == "driver-contribution" && text(p, "attestation") == "committed"
        })
        .filter_map(|p| member_key(text(p, "account")))
        .collect();
    // A decline is final in Muster: that member's approval counts toward
    // nothing, whichever came first.
    let approvals = signed
        .intersection(&committed)
        .filter(|k| room.approvers.contains(*k) && !declined.contains(*k))
        .count();
    if state == "executable" && approvals >= room.approvals {
        Verdict::Send
    } else {
        Verdict::Wait {
            state: state.into(),
            approvals,
        }
    }
}

/// The agent's seat in Muster, as this server found it. The room settings
/// come with each call: they may change between calls.
#[derive(Default)]
pub struct Room {
    /// Held from checking Muster through proposing: the policy Muster proposes
    /// under is instance-wide. The agent's Ed25519 key once Muster was found
    /// loaded, with its delivery node up.
    agent: tokio::sync::Mutex<Option<String>>,
    /// Proposals a transfer was sent for: never again.
    used: std::sync::Mutex<BTreeSet<String>>,
    /// Rooms this server looked through for proposals a restart left open.
    swept: std::sync::Mutex<BTreeSet<String>>,
}

/// One Muster call, its JSON-in-a-string result decoded. Muster reports a
/// refusal as `{"error": …}` inside a successful call.
pub(crate) async fn muster(node: &dyn Node, method: &str, args: &[&str]) -> Result<Value, String> {
    let args: Vec<Arg> = args.iter().map(|a| Arg::str(*a)).collect();
    let value = node
        .call(MUSTER, method, &args)
        .await
        .map(embedded_json)
        .map_err(|e| format!("Muster {method}: {e}"))?;
    match value.get("error") {
        Some(e) if !e.is_null() => Err(format!("Muster {method} refused: {e}")),
        _ => Ok(value),
    }
}

/// Make `topic` the agent Muster's active room: Muster reads and writes only
/// its active room, and only that room takes in new messages. The seat is the
/// agent's own, so this disturbs nobody.
pub(crate) async fn activate(node: &dyn Node, topic: &str) -> Result<(), String> {
    let rooms = muster(node, "coordinate_conversations", &[]).await?;
    let joined = rooms
        .as_array()
        .and_then(|r| r.iter().find(|r| text(r, "topic") == topic))
        .ok_or_else(|| {
            format!(
                "the agent's Muster isn't in the room {topic}: a member must invite it, and it must join"
            )
        })?;
    if joined.get("active").and_then(Value::as_bool) != Some(true) {
        muster(node, "coordinate_join", &[topic]).await?;
    }
    Ok(())
}

impl Room {
    pub fn new() -> Self {
        Self::default()
    }

    /// Make sure the agent's Muster can propose into `room`: loaded, its
    /// delivery node up, its own key not trusted, the room active, and enough
    /// trusted humans in it.
    async fn check(
        &self,
        node: &dyn Node,
        room: &RoomApproval,
        agent: &mut Option<String>,
    ) -> Result<(), String> {
        if agent.is_none() {
            node.ensure_loaded(MUSTER).await.map_err(|e| {
                format!("room approval needs muster_module in this logosctl session: {e}")
            })?;
            let identity = muster(node, "identity", &[]).await?;
            let key = identity
                .get("ed25519")
                .and_then(Value::as_str)
                .and_then(ed_key)
                .ok_or("Muster gave no Ed25519 identity")?;
            let deadline = tokio::time::Instant::now() + DELIVERY_WAIT;
            loop {
                let up = muster(node, "connectivity", &[]).await.is_ok_and(|c| {
                    c["rows"].as_array().is_some_and(|rows| {
                        rows.iter()
                            .any(|r| text(r, "key") == "delivery" && text(r, "level") == "ok")
                    })
                });
                if up {
                    break;
                }
                if tokio::time::Instant::now() >= deadline {
                    return Err("Muster's delivery node isn't up yet: nothing was proposed, try again shortly".into());
                }
                tokio::time::sleep(DELIVERY_POLL).await;
            }
            *agent = Some(key);
        }
        if let Some(key) = agent.as_deref()
            && room.trusts(key)
        {
            return Err(format!(
                "the agent's own Muster key ed:{key} is listed as a room approver: it must never approve its own transfers"
            ));
        }
        activate(node, &room.topic).await?;
        let first_look = self
            .swept
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(room.topic.clone());
        if first_look {
            self.sweep(node, &room.topic).await;
        }
        let members = muster(node, "coordinate_members", &[]).await?;
        let trusted: BTreeSet<String> = members
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter_map(|m| member_key(text(m, "identity")))
            .filter(|k| room.approvers.contains(k))
            .collect();
        if trusted.len() < room.approvals {
            return Err(format!(
                "only {} of the trusted approvers are in the room, and {} approvals are needed",
                trusted.len(),
                room.approvals
            ));
        }
        Ok(())
    }

    /// Propose `call` to the room, say in the room's chat what it does (Muster's
    /// card shows only the method), then endorse it as the agent, which only
    /// counts toward Muster's own threshold, never toward the trusted
    /// approvals. A proposal the room can't be told about is withdrawn
    /// unendorsed. `reference` makes the proposal new: Muster names
    /// a proposal by its effect, so a repeated payment would otherwise land on
    /// an old one. Returns the proposal's id.
    pub async fn propose(
        &self,
        node: &dyn Node,
        room: &RoomApproval,
        call: &RoomCall,
        reference: &str,
    ) -> Result<String, String> {
        let mut agent = self.agent.lock().await;
        self.check(node, room, &mut agent).await?;
        let policy = muster(node, "coordinate_set_policy", &["invoke"]).await?;
        if text(&policy, "policy") != "invoke" {
            return Err(format!(
                "Muster would not propose under the invoke policy: {policy}"
            ));
        }
        let effect = json!({
            "effect": "invoke",
            "module": call.module,
            "method": call.method,
            "args": call.args,
            "ref": reference,
            "note": call.note,
        });
        let id = muster(node, "coordinate_propose", &[&effect.to_string()]).await?;
        let id = id
            .as_str()
            .filter(|id| {
                id.strip_prefix("0x")
                    .is_some_and(|h| !h.is_empty() && h.chars().all(|c| c.is_ascii_hexdigit()))
            })
            .ok_or_else(|| format!("Muster gave no proposal id: {id}"))?
            .to_string();
        let intents = muster(node, "coordinate_intents", &[]).await?;
        let approved_before = intents
            .as_array()
            .and_then(|all| all.iter().find(|i| text(i, "id") == id))
            .and_then(|i| i.get("approvals").and_then(Value::as_u64))
            .unwrap_or(0);
        if approved_before > 0 {
            return Err(format!(
                "the room proposal {id} already had approvals before this one: nothing was proposed"
            ));
        }
        let said = format!("[logos-mcp] proposal {id}: {}", call.note);
        if let Err(e) = muster(node, "coordinate_post_message", &[&said]).await {
            self.withdraw(node, &room.topic, &id).await;
            return Err(format!(
                "couldn't say in the room what the proposal does, so it was withdrawn: nothing was sent ({e})"
            ));
        }
        muster(node, "coordinate_contribute", &[&id, "", ""]).await?;
        Ok(id)
    }

    /// Where proposal `intent_id` for `call` stands in `room`, at `now` (Unix
    /// seconds), judged by the settings as they are now.
    pub async fn look(
        &self,
        node: &dyn Node,
        room: &RoomApproval,
        intent_id: &str,
        call: &RoomCall,
        now: u64,
    ) -> Result<Verdict, String> {
        if self
            .used
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(intent_id)
        {
            return Ok(Verdict::Refused(
                "this room approval was already used".into(),
            ));
        }
        if let Some(key) = self.agent.lock().await.as_deref()
            && room.trusts(key)
        {
            return Ok(Verdict::Refused(format!(
                "the agent's own Muster key ed:{key} is now listed as a room approver"
            )));
        }
        // Only the active room takes in new messages: a restarted seat has none.
        activate(node, &room.topic).await?;
        let intents = muster(node, "coordinate_intents", &[]).await?;
        let provenance = muster(node, "coordinate_provenance", &[]).await?;
        Ok(judge(room, intent_id, call, &intents, &provenance, now))
    }

    /// Decline what an earlier run of this server left open: the agent's own
    /// proposals, still collecting, for requests a restart dropped and so never
    /// sends. An executable one is left alone, as it was most likely sent.
    /// Best effort: a Muster that can't be read leaves them open.
    async fn sweep(&self, node: &dyn Node, topic: &str) {
        let intents = match muster(node, "coordinate_intents", &[]).await {
            Ok(intents) => intents,
            Err(e) => {
                tracing::warn!("could not look for room proposals left open: {e}");
                return;
            }
        };
        let left_open = intents
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter(|i| {
                matches!(text(i, "state"), "proposed" | "collecting")
                    && i.pointer("/effect/ref")
                        .and_then(Value::as_str)
                        .is_some_and(|r| r.starts_with(REF_PREFIX))
                    && i.get("proposedBy")
                        .and_then(Value::as_array)
                        .is_some_and(|by| {
                            by.iter()
                                .any(|p| p.get("mine").and_then(Value::as_bool) == Some(true))
                        })
            });
        for intent in left_open {
            tracing::info!(
                intent_id = text(intent, "id"),
                "declining a room proposal a restart left open"
            );
            self.withdraw(node, topic, text(intent, "id")).await;
        }
    }

    /// Take back the agent's approval of a proposal that will never be sent, so
    /// the room sees it closed. Best effort: nothing is sent either way.
    pub async fn withdraw(&self, node: &dyn Node, topic: &str, intent_id: &str) {
        let declined = match activate(node, topic).await {
            Ok(()) => muster(node, "coordinate_decline", &[intent_id])
                .await
                .map(|_| ()),
            Err(e) => Err(e),
        };
        if let Err(e) = declined {
            tracing::warn!(%intent_id, "could not withdraw the room proposal: {e}");
        }
    }

    /// Spend proposal `intent_id`'s approval: false when it was already spent.
    pub fn use_once(&self, intent_id: &str) -> bool {
        self.used
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(intent_id.to_string())
    }
}

#[cfg(test)]
pub(crate) mod tests;
