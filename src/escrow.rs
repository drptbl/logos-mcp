//! The lez-escrow program (github.com/drptbl/lez-escrow): a buyer locks a job's
//! payment, the provider claims it after delivering, and the buyer is refunded
//! when nothing is delivered by the deadline. This module only encodes and
//! decodes; the server sends and reads through `lez_core`.

use sha2::{Digest, Sha256};

/// The program deployed on the LEZ testnet (v0.3.1), immutable: the deploy
/// that refuses calls coming from other programs (image e8a93458…).
pub const PROGRAM_ID: &str = "CAZ8AvrdAeZ4rQjHqhAucqSLqrMK8TFQ7Z8b2jg5Sb4G";
pub const PROGRAM_ID_HEX: &str = "a5e31b14f1304b129f108abb457eb389ed0f88103fa91b069208af5e9df6560d";

const JOB_SEED: &[u8; 32] = b"/lez-escrow/v1/JobSeed/000000000";
const PDA_PREFIX: &[u8; 32] = b"/LEE/v0.2/AccountId/PDA/\0\0\0\0\0\0\0\0";

pub type Id = [u8; 32];

fn program_id() -> Id {
    let mut id = [0u8; 32];
    for (i, b) in id.iter_mut().enumerate() {
        *b = u8::from_str_radix(&PROGRAM_ID_HEX[2 * i..2 * i + 2], 16).unwrap_or_default();
    }
    id
}

/// The account a buyer's job lives in. Deriving from the buyer too means no
/// one who learns `job_ref` can open that job first.
pub fn job_account(buyer: &Id, job_ref: &Id) -> Id {
    job_account_in(&program_id(), buyer, job_ref)
}

/// The job account under `program`.
fn job_account_in(program: &Id, buyer: &Id, job_ref: &Id) -> Id {
    let seed: Id = Sha256::new()
        .chain_update(JOB_SEED)
        .chain_update(buyer)
        .chain_update(job_ref)
        .finalize()
        .into();
    // The PDA prefix is 24 bytes of text and 8 zero bytes: 32 in all.
    Sha256::new()
        .chain_update(PDA_PREFIX)
        .chain_update(program)
        .chain_update(seed)
        .finalize()
        .into()
}

/// The program's instructions, Borsh-encoded: a variant byte, then the fields.
#[derive(Debug, Clone, PartialEq)]
pub enum Instruction {
    Open {
        job_ref: Id,
        provider: Id,
        amount: u128,
        deadline: u64,
    },
    Deliver {
        result: Id,
        deadline: u64,
    },
    Claim {
        buyer: Id,
        job_ref: Id,
        amount: u128,
    },
    Refund {
        job_ref: Id,
        amount: u128,
        deadline: u64,
    },
}

impl Instruction {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(89);
        match self {
            Self::Open {
                job_ref,
                provider,
                amount,
                deadline,
            } => {
                out.push(0);
                out.extend(job_ref);
                out.extend(provider);
                out.extend(amount.to_le_bytes());
                out.extend(deadline.to_le_bytes());
            }
            Self::Deliver { result, deadline } => {
                out.push(1);
                out.extend(result);
                out.extend(deadline.to_le_bytes());
            }
            Self::Claim {
                buyer,
                job_ref,
                amount,
            } => {
                out.push(2);
                out.extend(buyer);
                out.extend(job_ref);
                out.extend(amount.to_le_bytes());
            }
            Self::Refund {
                job_ref,
                amount,
                deadline,
            } => {
                out.push(4);
                out.extend(job_ref);
                out.extend(amount.to_le_bytes());
                out.extend(deadline.to_le_bytes());
            }
        }
        out
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Open,
    Delivered,
    Claimed,
    Released,
    Refunded,
}

/// A job as the program records it.
#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub buyer: Id,
    pub provider: Id,
    pub amount: u128,
    /// Milliseconds since the Unix epoch.
    pub deadline: u64,
    pub result: Option<Id>,
    pub state: JobState,
}

impl Record {
    pub fn decode(raw: &[u8]) -> Option<Self> {
        let id = |at: usize| -> Option<Id> { raw.get(at..at + 32)?.try_into().ok() };
        let buyer = id(0)?;
        let provider = id(32)?;
        let amount = u128::from_le_bytes(raw.get(64..80)?.try_into().ok()?);
        let deadline = u64::from_le_bytes(raw.get(80..88)?.try_into().ok()?);
        let (result, at) = match raw.get(88)? {
            0 => (None, 89),
            1 => (Some(id(89)?), 121),
            _ => return None,
        };
        let state = match raw.get(at)? {
            0 => JobState::Open,
            1 => JobState::Delivered,
            2 => JobState::Claimed,
            3 => JobState::Released,
            4 => JobState::Refunded,
            _ => return None,
        };
        (raw.len() == at + 1).then_some(Self {
            buyer,
            provider,
            amount,
            deadline,
            result,
            state,
        })
    }
}

/// The job record in an account, as `lez_core`'s `get_account_public` returns
/// it: `Ok(None)` when the account holds no job.
pub fn record_in(account: &str) -> Result<Option<Record>, String> {
    let v: serde_json::Value =
        serde_json::from_str(account).map_err(|e| format!("unreadable account: {e}"))?;
    let shard = v["shards"]
        .as_array()
        .and_then(|shards| shards.iter().find(|s| s["program"] == PROGRAM_ID_HEX));
    let Some(data) = shard.and_then(|s| s["data"].as_str()) else {
        return Ok(None);
    };
    let raw: Option<Vec<u8>> = (0..data.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(data.get(i..i + 2)?, 16).ok())
        .collect();
    raw.as_deref()
        .and_then(Record::decode)
        .map(Some)
        .ok_or_else(|| "the escrow record doesn't decode".to_string())
}

#[cfg(test)]
mod tests;
