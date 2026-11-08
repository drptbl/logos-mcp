//! The agent services protocol (`docs/services-protocol.md`): its messages, and
//! each job's state rebuilt from the conversation that carries it.
//!
//! Chat history is the only record of a job: replaying a conversation gives
//! the same state on every call and after a restart.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The version this side writes. It reads any `1.x`.
pub const VERSION: &str = "1.1";
/// The discovery entry that carries a provider's chat address.
pub const CHAT_ENTRY: &str = "logos-mcp/chat/v1";

/// One protocol message, as the text of a chat message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub v: String,
    pub job: String,
    pub id: String,
    #[serde(flatten)]
    pub body: Body,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Body {
    Request {
        service: String,
        input: Payload,
    },
    Quote {
        price_lepta: String,
        pay_to: String,
        network: String,
        expires_at: u64,
        deliver_within_s: u64,
        /// Since 1.1: the buyer may lock the price in this escrow instead.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        escrow: Option<EscrowTerms>,
    },
    Reject {
        reason: String,
    },
    Paid {
        quote: String,
        tx_hash: String,
        /// Since 1.1: the account that opened the escrow, when it was used.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        escrow_buyer: Option<String>,
    },
    Cancel {
        #[serde(default)]
        reason: String,
    },
    Result {
        output: Payload,
    },
    Failed {
        reason: String,
    },
}

/// Where and on what terms a quote's price may be locked in escrow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EscrowTerms {
    /// The escrow program's id, base58.
    pub program: String,
    /// The provider's account the escrow pays, `Public/<base58>`.
    pub provider: String,
    /// Delivery deadline, milliseconds since the Unix epoch.
    pub deadline: u64,
}

/// A job's input or output: text, or content in Logos Storage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Payload {
    Text(String),
    Cid(String),
}

impl Message {
    /// A message for `job` with a fresh message id.
    pub fn new(job: &str, body: Body) -> Self {
        Self {
            v: VERSION.to_string(),
            job: job.to_string(),
            id: random_hex(),
            body,
        }
    }
}

/// A fresh job id for a new request.
pub fn new_job_id() -> String {
    random_hex()
}

fn random_hex() -> String {
    let mut bytes = [0u8; 16];
    // The OS random source only fails on a broken system; a zeroed id then
    // still works, it just isn't unpredictable.
    let _ = getrandom::fill(&mut bytes);
    hex(&bytes)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn is_id(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A v1 protocol message, or `None` for any other text: another major
/// version, an unknown type, or fields out of shape.
pub fn parse(text: &str) -> Option<Message> {
    let m: Message = serde_json::from_str(text).ok()?;
    let major = m.v.split('.').next()?;
    if major != "1" || !is_id(&m.job) || !is_id(&m.id) {
        return None;
    }
    let well_formed = match &m.body {
        Body::Request { service, .. } => !service.is_empty(),
        Body::Quote {
            price_lepta,
            pay_to,
            network,
            escrow,
            ..
        } => {
            !price_lepta.is_empty()
                && price_lepta.len() <= 39
                && price_lepta.bytes().all(|b| b.is_ascii_digit())
                && pay_to.starts_with("Public/")
                && !network.is_empty()
                && escrow.as_ref().is_none_or(|e| {
                    !e.program.is_empty() && e.provider.starts_with("Public/") && e.deadline > 0
                })
        }
        Body::Paid { quote, .. } => quote.len() == 64,
        _ => true,
    };
    well_formed.then_some(m)
}

/// The text to send for `m`.
pub fn encode(m: &Message) -> String {
    serde_json::to_string(m).unwrap_or_default()
}

/// What `paid` names a quote by: SHA-256 of its text exactly as received.
pub fn quote_hash(text: &str) -> String {
    hex(&Sha256::digest(text.as_bytes()))
}

/// A chat message, as the conversation holds it.
pub struct Seen<'a> {
    /// This side sent it.
    pub from_self: bool,
    pub text: &'a str,
    pub timestamp_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Buyer,
    Provider,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Requested,
    Quoted,
    Paid,
    Delivered,
    Rejected,
    Canceled,
    Expired,
    Failed,
}

impl State {
    pub fn is_final(self) -> bool {
        !matches!(self, Self::Requested | Self::Quoted | Self::Paid)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Quote {
    pub price_lepta: String,
    pub pay_to: String,
    pub network: String,
    pub expires_at: u64,
    pub deliver_within_s: u64,
    pub escrow: Option<EscrowTerms>,
    /// What a `paid` message must name.
    pub hash: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Job {
    pub job: String,
    pub role: Role,
    pub service: String,
    pub input: Payload,
    /// As the messages left it. `state_at` adds expiry and payment on chain.
    pub state: State,
    pub quote: Option<Quote>,
    pub tx_hash: Option<String>,
    /// The account that opened the escrow, as the buyer's `paid` names it.
    pub escrow_buyer: Option<String>,
    pub output: Option<Payload>,
    pub reason: Option<String>,
    pub updated_ms: u64,
}

impl Job {
    /// The state now: an unpaid quote expires, and a payment that landed on
    /// `pay_to` counts even without a `paid` message, or after expiry. A
    /// provider takes a buyer's `paid` only as a claim until the money lands.
    pub fn state_at(&self, now_ms: u64, paid_on_chain: bool) -> State {
        let unpaid = match (self.role, self.state) {
            (_, State::Quoted) | (Role::Provider, State::Paid) => !paid_on_chain,
            _ => return self.state,
        };
        match &self.quote {
            _ if !unpaid => State::Paid,
            Some(q) if now_ms >= q.expires_at => State::Expired,
            _ => State::Quoted,
        }
    }
}

/// Every job in a conversation, in the order they started. A message that
/// doesn't fit (wrong side, wrong state, a repeat) is ignored, as the spec says.
pub fn replay<'a>(messages: impl IntoIterator<Item = Seen<'a>>) -> Vec<Job> {
    let mut jobs: Vec<Job> = Vec::new();
    let mut seen_ids = std::collections::BTreeSet::new();
    for seen in messages {
        let Some(m) = parse(seen.text) else {
            continue;
        };
        if !seen_ids.insert(m.id.clone()) {
            continue;
        }
        let index = jobs.iter().position(|j| j.job == m.job);
        let Some(index) = index else {
            if let Body::Request { service, input } = m.body {
                jobs.push(Job {
                    job: m.job,
                    role: if seen.from_self {
                        Role::Buyer
                    } else {
                        Role::Provider
                    },
                    service,
                    input,
                    state: State::Requested,
                    quote: None,
                    tx_hash: None,
                    escrow_buyer: None,
                    output: None,
                    reason: None,
                    updated_ms: seen.timestamp_ms,
                });
            }
            continue;
        };
        let job = &mut jobs[index];
        // Who sent it: the buyer or the provider.
        let from_buyer = seen.from_self == (job.role == Role::Buyer);
        let next = match (m.body, job.state, from_buyer) {
            (
                Body::Quote {
                    price_lepta,
                    pay_to,
                    network,
                    expires_at,
                    deliver_within_s,
                    escrow,
                },
                State::Requested,
                false,
            ) => {
                job.quote = Some(Quote {
                    price_lepta,
                    pay_to,
                    network,
                    expires_at,
                    deliver_within_s,
                    escrow,
                    hash: quote_hash(seen.text),
                });
                Some(State::Quoted)
            }
            (Body::Reject { reason }, State::Requested, false)
            | (Body::Failed { reason }, State::Quoted | State::Paid, false) => {
                let failed = job.state != State::Requested;
                job.reason = Some(reason);
                Some(if failed {
                    State::Failed
                } else {
                    State::Rejected
                })
            }
            (
                Body::Paid {
                    quote,
                    tx_hash,
                    escrow_buyer,
                },
                State::Quoted,
                true,
            ) if job.quote.as_ref().is_some_and(|q| q.hash == quote) => {
                job.tx_hash = Some(tx_hash);
                job.escrow_buyer = escrow_buyer;
                Some(State::Paid)
            }
            (Body::Cancel { reason }, State::Requested | State::Quoted, true) => {
                job.reason = (!reason.is_empty()).then_some(reason);
                Some(State::Canceled)
            }
            (Body::Result { output }, State::Quoted | State::Paid, false) => {
                job.output = Some(output);
                Some(State::Delivered)
            }
            _ => None,
        };
        if let Some(state) = next {
            job.state = state;
            job.updated_ms = seen.timestamp_ms;
        }
    }
    jobs
}

#[cfg(test)]
mod tests;
