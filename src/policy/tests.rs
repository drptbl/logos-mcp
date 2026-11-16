use super::*;
use serde_json::json;

fn tiers(limit: u128, up_to: Approver, above: Approver) -> Tiers {
    Tiers {
        limit_lepta: limit,
        up_to,
        above,
    }
}

fn room() -> RoomSettings {
    RoomSettings {
        topic: "/muster/1/r/proto".into(),
        approvers: vec![
            "ed:0b00086682a0d169f90095b9d58e6c003ee591531ccf8b59c712c1cb70603269".into(),
        ],
        approvals: 1,
        expires_mins: 60,
    }
}

#[test]
fn the_limit_splits_two_tiers_and_zero_makes_one() {
    let t = tiers(3_000_000, Approver::Send, Approver::Room);
    assert_eq!(t.approver("3000000"), Approver::Send);
    assert_eq!(t.approver("3000001"), Approver::Room);
    let one = tiers(0, Approver::Send, Approver::Ask);
    assert_eq!(one.approver("1"), Approver::Ask);
    // Whatever can't be read as an amount is treated as above the limit.
    assert_eq!(t.approver("lots"), Approver::Room);
}

#[test]
fn the_upper_tier_is_never_looser_than_the_lower() {
    let looser = ApprovalPolicy {
        agent: tiers(5, Approver::Ask, Approver::Send),
        ..ApprovalPolicy::default()
    };
    assert!(looser.check().unwrap_err().contains("looser"));
    let room_below_ask = ApprovalPolicy {
        agent: tiers(5, Approver::Room, Approver::Ask),
        room: Some(room()),
        ..ApprovalPolicy::default()
    };
    assert!(room_below_ask.check().is_err());
}

#[test]
fn a_room_tier_needs_a_sound_room() {
    let no_room = ApprovalPolicy {
        node: tiers(0, Approver::Ask, Approver::Room),
        ..ApprovalPolicy::default()
    };
    assert!(no_room.check().unwrap_err().contains("room"));
    let mut bad = room();
    bad.approvals = 2;
    let bad_room = ApprovalPolicy {
        agent: tiers(0, Approver::Send, Approver::Room),
        room: Some(bad),
        ..ApprovalPolicy::default()
    };
    assert!(bad_room.check().is_err());
    let good = ApprovalPolicy {
        agent: tiers(0, Approver::Send, Approver::Room),
        room: Some(room()),
        ..ApprovalPolicy::default()
    };
    good.check().unwrap();
    assert_eq!(
        good.room_approval().unwrap().unwrap().topic,
        "/muster/1/r/proto"
    );
}

#[test]
fn by_default_the_agent_sends_and_the_node_wallet_asks() {
    let p = ApprovalPolicy::default();
    assert_eq!(p.approver(Wallet::Agent, "9"), Approver::Send);
    assert_eq!(p.approver(Wallet::Node, "9"), Approver::Ask);
    p.check().unwrap();
}

#[test]
fn the_node_wallet_may_be_set_to_send() {
    let p = ApprovalPolicy {
        node: tiers(10, Approver::Send, Approver::Ask),
        ..ApprovalPolicy::default()
    };
    p.check().unwrap();
    assert_eq!(p.approver(Wallet::Node, "10"), Approver::Send);
}

#[test]
fn the_old_ask_switch_carries_over() {
    let asked = ApprovalPolicy::from_ask_before_transfer(true);
    assert_eq!(asked.agent, tiers(0, Approver::Ask, Approver::Ask));
    assert_eq!(asked.node, tiers(0, Approver::Ask, Approver::Ask));
    assert_eq!(
        ApprovalPolicy::from_ask_before_transfer(false),
        ApprovalPolicy::default()
    );
}

#[test]
fn switching_ask_keeps_a_room_tier() {
    let mut p = ApprovalPolicy {
        agent: tiers(3, Approver::Send, Approver::Room),
        room: Some(room()),
        ..ApprovalPolicy::default()
    };
    p.set_ask_before_transfer(true);
    assert_eq!(p.agent, tiers(3, Approver::Ask, Approver::Room));
    p.set_ask_before_transfer(false);
    assert_eq!(p.agent, tiers(3, Approver::Send, Approver::Room));
    assert!(p.asks_before_transfer(), "a room tier still waits");
    let mut plain = ApprovalPolicy::default();
    plain.set_ask_before_transfer(true);
    assert_eq!(plain.agent, tiers(0, Approver::Ask, Approver::Ask));
    plain.set_ask_before_transfer(false);
    assert!(!plain.asks_before_transfer());
}

#[test]
fn the_policy_reads_and_writes_the_app_shape() {
    let shape = json!({
        "agent": {"limit_lepta": "3000000", "up_to": "send", "above": "room"},
        "node": {"limit_lepta": "0", "up_to": "ask", "above": "ask"},
        "room": {"topic": "/muster/1/r/proto",
                 "approvers": ["ed:0b00086682a0d169f90095b9d58e6c003ee591531ccf8b59c712c1cb70603269"],
                 "approvals": 1, "expires_mins": 60}
    });
    let p: ApprovalPolicy = serde_json::from_value(shape.clone()).unwrap();
    assert_eq!(p.agent, tiers(3_000_000, Approver::Send, Approver::Room));
    assert_eq!(serde_json::to_value(&p).unwrap(), shape);
    // Missing parts take their defaults: the node wallet keeps asking.
    let partial: ApprovalPolicy = serde_json::from_value(json!({"agent": shape["agent"]})).unwrap();
    assert_eq!(partial.node, ApprovalPolicy::default().node);
    assert!(
        serde_json::from_value::<ApprovalPolicy>(json!({"agent": {"limit_lepta": "3e6"}})).is_err()
    );
}
