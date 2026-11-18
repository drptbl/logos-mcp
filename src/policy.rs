//! Who approves a transfer: per wallet, a limit splits two tiers, and each
//! tier sends, asks the user, or asks a Muster room.

use serde::{Deserialize, Serialize};

use crate::room::RoomApproval;

/// Who approves a transfer, from the loosest to the strictest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Approver {
    /// Nobody: the agent sends it.
    Send,
    /// The user, in the AI Agent app.
    Ask,
    /// Trusted people in the Muster room.
    Room,
}

/// The wallet a transfer spends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wallet {
    /// The agent wallet: LEZ transfers and program calls such as escrow opens.
    Agent,
    /// The chain node's own wallet: Logos chain transfers.
    Node,
}

/// A limit and who approves at or under it, and above it. A limit of 0 makes
/// one tier: every transfer is above it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tiers {
    #[serde(with = "lepta")]
    pub limit_lepta: u128,
    pub up_to: Approver,
    pub above: Approver,
}

impl Tiers {
    const fn both(approver: Approver) -> Self {
        Self {
            limit_lepta: 0,
            up_to: approver,
            above: approver,
        }
    }

    /// Who approves `amount_lepta`. An amount that can't be read is treated
    /// as above the limit, the stricter side.
    pub fn approver(&self, amount_lepta: &str) -> Approver {
        match amount_lepta.trim().parse::<u128>() {
            Ok(amount) if amount <= self.limit_lepta => self.up_to,
            _ => self.above,
        }
    }

    fn check(&self, wallet: &str) -> Result<(), String> {
        if self.above < self.up_to {
            return Err(format!(
                "the {wallet}'s tier above the limit is looser than the one under it"
            ));
        }
        Ok(())
    }

    /// Whether either tier is `approver`.
    pub fn uses(&self, approver: Approver) -> bool {
        self.up_to == approver || self.above == approver
    }

    /// These tiers with the room's replaced by the user's.
    fn without_room(&self) -> Self {
        let ask = |a| {
            if a == Approver::Room {
                Approver::Ask
            } else {
                a
            }
        };
        Self {
            limit_lepta: self.limit_lepta,
            up_to: ask(self.up_to),
            above: ask(self.above),
        }
    }
}

/// The Muster room both wallets' room tiers use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomSettings {
    /// The room's content topic.
    pub topic: String,
    /// Trusted approvers' Ed25519 keys, `ed:<hex>`.
    pub approvers: Vec<String>,
    /// Distinct trusted approvals a transfer needs.
    pub approvals: usize,
    /// Minutes a proposal waits before it expires unsent.
    pub expires_mins: u64,
}

impl RoomSettings {
    pub fn room_approval(&self) -> Result<RoomApproval, String> {
        RoomApproval::new(
            &self.topic,
            &self.approvers,
            self.approvals,
            self.expires_mins,
        )
    }
}

/// Who approves each wallet's transfers. Read on every transfer: a change
/// applies to the next one, and to room requests still waiting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ApprovalPolicy {
    pub agent: Tiers,
    pub node: Tiers,
    pub room: Option<RoomSettings>,
}

impl Default for ApprovalPolicy {
    /// The agent sends on its own; the node wallet, which is the user's,
    /// always asks.
    fn default() -> Self {
        Self {
            agent: Tiers::both(Approver::Send),
            node: Tiers::both(Approver::Ask),
            room: None,
        }
    }
}

impl ApprovalPolicy {
    /// The policy the old "Ask me before each transfer" switch meant.
    pub fn from_ask_before_transfer(ask: bool) -> Self {
        let mut policy = Self::default();
        policy.set_ask_before_transfer(ask);
        policy
    }

    /// The old switch, on the agent wallet's tiers: on asks for every
    /// transfer a room doesn't approve, off sends them. A room tier stays.
    pub fn set_ask_before_transfer(&mut self, ask: bool) {
        let to = if ask { Approver::Ask } else { Approver::Send };
        let tiers = &mut self.agent;
        if tiers.up_to != Approver::Room {
            tiers.up_to = to;
        }
        if tiers.above != Approver::Room {
            tiers.above = to;
        }
    }

    /// Whether an agent wallet transfer may wait for approval, the user's or
    /// the room's.
    pub fn asks_before_transfer(&self) -> bool {
        self.agent.uses(Approver::Ask) || self.agent.uses(Approver::Room)
    }

    pub fn approver(&self, wallet: Wallet, amount_lepta: &str) -> Approver {
        match wallet {
            Wallet::Agent => self.agent.approver(amount_lepta),
            Wallet::Node => self.node.approver(amount_lepta),
        }
    }

    /// Whether the policy can be followed: no upper tier looser than its
    /// lower one, and a sound room wherever a room approves.
    pub fn check(&self) -> Result<(), String> {
        self.agent.check("agent wallet")?;
        self.node.check("node wallet")?;
        match (&self.room, self.uses_room()) {
            (None, true) => Err("a tier asks the room, but no room is set".into()),
            (Some(room), _) => room.room_approval().map(|_| ()),
            (None, false) => Ok(()),
        }
    }

    /// Whether a tier of either wallet asks the room.
    pub fn uses_room(&self) -> bool {
        self.agent.uses(Approver::Room) || self.node.uses(Approver::Room)
    }

    /// This policy with room approval off: every room tier asks the user
    /// instead, which is never looser, and no room is set.
    pub fn without_room(&self) -> Self {
        Self {
            agent: self.agent.without_room(),
            node: self.node.without_room(),
            room: None,
        }
    }

    /// The policy to fall back on when a saved one can't be followed: every
    /// transfer, from either wallet, waits for the user.
    pub fn ask_for_everything() -> Self {
        Self {
            agent: Tiers::both(Approver::Ask),
            node: Tiers::both(Approver::Ask),
            room: None,
        }
    }

    /// The room, checked, when one is set.
    pub fn room_approval(&self) -> Result<Option<RoomApproval>, String> {
        self.room
            .as_ref()
            .map(RoomSettings::room_approval)
            .transpose()
    }
}

/// Lepta as a decimal string in JSON: amounts outgrow JSON numbers.
mod lepta {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(value: &u128, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&value.to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u128, D::Error> {
        let text = String::deserialize(d)?;
        if text.is_empty() || !text.chars().all(|c| c.is_ascii_digit()) {
            return Err(D::Error::custom(
                "a limit is a whole number of lepta, as digits",
            ));
        }
        text.parse()
            .map_err(|_| D::Error::custom("the limit is too large"))
    }
}

#[cfg(test)]
mod tests;
