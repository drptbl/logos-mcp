use super::*;
use serde_json::json;

/// The `txhash` Muster 0.3.5 folded for intent 0xfb711723fa071cc0, recorded
/// live on 2026-11-15: lez_core.transfer_public(HSgev…, 9uwk…, "7000000").
const TXHASH_7M: &str = "0x83706d75737465722e696e766f6b652e763178296d75737465722e696e766f6b652e6c657a5f636f72652e7472616e736665725f7075626c69632e7631a3646172677378775b225075626c69632f485367657655486952627a6d42765372336a39525545424563483938416539713541535052717a6644554852222c225075626c69632f3975776b564a7558536344774c4b5674514d634846553948766233755a6a694544567644763450596a43664d222c2237303030303030225d666d6574686f646f7472616e736665725f7075626c6963666d6f64756c65686c657a5f636f7265";
const FROM: &str = "Public/HSgevUHiRbzmBvSr3j9RUEBEcH98Ae9q5ASPRqzfDUHR";
const TO: &str = "Public/9uwkVJuXScDwLKVtQMcHFU9Hvb3uZjiEDVvDv4PYjCfM";

#[test]
fn a_recorded_txhash_decodes_to_the_call_the_room_signed() {
    assert_eq!(
        signed_call(TXHASH_7M).unwrap(),
        SignedCall {
            schema_id: "muster.invoke.lez_core.transfer_public.v1".into(),
            module: "lez_core".into(),
            method: "transfer_public".into(),
            args: json!([FROM, TO, "7000000"]),
        }
    );
}

const HUMAN: &str = "0b00086682a0d169f90095b9d58e6c003ee591531ccf8b59c712c1cb70603269";
const AGENT: &str = "d8f76a7a69e956b81bf3c8abfcebec21c2c65aeccfa6f7bfe58685ccee9b1e6b";
const OTHER: &str = "7a1c000000000000000000000000000000000000000000000000000000e0f2aa";
const TOPIC: &str = "/muster/1/agent-spike2-1794773136/proto";

fn config(approvers: &[&str], approvals: usize) -> RoomApproval {
    RoomApproval::new(
        TOPIC,
        &approvers
            .iter()
            .map(|a| format!("ed:{a}"))
            .collect::<Vec<_>>(),
        approvals,
        60,
    )
    .unwrap()
}

#[test]
fn approvers_are_ed25519_keys_with_or_without_the_ed_prefix() {
    let room = RoomApproval::new(
        TOPIC,
        &[format!("ed:{}", HUMAN.to_uppercase()), OTHER.into()],
        2,
        60,
    )
    .unwrap();
    assert!(room.trusts(&format!("ed:{HUMAN}")));
    assert!(room.trusts(OTHER));
    assert!(!room.trusts(AGENT));
}

#[test]
fn a_config_that_could_never_approve_is_refused() {
    let bad = |topic: &str, approvers: &[String], approvals: usize, mins: u64| {
        RoomApproval::new(topic, approvers, approvals, mins).unwrap_err()
    };
    let one = [format!("ed:{HUMAN}")];
    assert!(bad(TOPIC, &[], 1, 60).contains("approver"));
    assert!(bad(TOPIC, &["ed:1234".into()], 1, 60).contains("64 hex"));
    assert!(bad(TOPIC, &one, 2, 60).contains("2 approvals"));
    assert!(bad(TOPIC, &one, 0, 60).contains("at least 1"));
    assert!(bad("no-slash", &one, 1, 60).contains("topic"));
    assert!(bad(TOPIC, &one, 1, 0).contains("minute"));
    // The same key twice is one approver, not two.
    assert!(bad(TOPIC, &[one[0].clone(), HUMAN.into()], 2, 60).contains("2 approvals"));
}

// ---- the gate --------------------------------------------------------------

const ID: &str = "0xfb711723fa071cc0";
const NOW: u64 = 1_794_775_000;
const EXPIRY: u64 = 1_795_378_960;

/// One approval as Muster 0.3.5 folds it: who signed, and the intent record's
/// grade for it ("committed" when attested in-app for this intent).
struct Sig(&'static str, &'static str);

/// What the agent's Muster shows for the 7M intent, shaped as recorded live:
/// `coordinate_intents` and `coordinate_provenance`.
fn muster(
    state: &str,
    sigs: &[Sig],
    decliners: &[&str],
    topic: &str,
    expiry: u64,
) -> (Value, Value) {
    let record: Vec<Value> = std::iter::once(json!({"class": "peer-message", "account": "", "attestation": ""}))
        .chain(sigs.iter().map(|Sig(who, grade)| {
            json!({"class": "driver-contribution", "account": format!("ed:{who}"), "attestation": grade})
        }))
        .collect();
    let intents = json!([{
        "id": ID,
        "state": state,
        "approvals": sigs.len(),
        "threshold": 2,
        "policy": "invoke",
        "domain": "muster.invoke.v1",
        "txhash": TXHASH_7M,
        "decliners": decliners.iter().map(|d| format!("{d}1e04b737de62a329682bb6971b7dd70eb7c6b01f68eb1992bcc8d1142d881d20")).collect::<Vec<_>>(),
        "provenance": record,
    }]);
    let mut rows = vec![
        json!({"seq": 2, "kind": "propose", "intentId": ID, "account": ""}),
        json!({"seq": 4, "kind": "context", "intentId": ID, "account": "",
               "detail": json!({"environment": "room", "account": topic, "expiry": expiry.to_string()}).to_string()}),
    ];
    for Sig(who, _) in sigs {
        rows.push(json!({"seq": 5, "kind": "sig", "intentId": ID, "account": format!("ed:{who}")}));
    }
    (intents, Value::Array(rows))
}

fn verdict(room: &RoomApproval, args: Value, (intents, provenance): (Value, Value)) -> Verdict {
    let call = RoomCall {
        module: "lez_core",
        method: "transfer_public",
        args,
        note: String::new(),
    };
    judge(room, ID, &call, &intents, &provenance, NOW)
}

fn the_call() -> Value {
    json!([FROM, TO, "7000000"])
}

const COMMITTED: &str = "committed";

#[test]
fn a_trusted_humans_approval_of_exactly_this_call_passes() {
    let room = config(&[HUMAN], 1);
    let seen = muster(
        "executable",
        &[Sig(AGENT, COMMITTED), Sig(HUMAN, COMMITTED)],
        &[],
        TOPIC,
        EXPIRY,
    );
    assert_eq!(
        verdict(&room, the_call(), seen),
        Verdict::Send { approvals: 1 }
    );
}

#[test]
fn an_untrusted_signer_never_counts() {
    let room = config(&[HUMAN], 1);
    let seen = muster(
        "executable",
        &[Sig(AGENT, COMMITTED), Sig(OTHER, COMMITTED)],
        &[],
        TOPIC,
        EXPIRY,
    );
    assert!(matches!(
        verdict(&room, the_call(), seen),
        Verdict::Wait { approvals: 0, .. }
    ));
}

#[test]
fn the_agents_own_signature_never_counts() {
    let room = config(&[HUMAN], 1);
    let seen = muster("executable", &[Sig(AGENT, COMMITTED)], &[], TOPIC, EXPIRY);
    assert!(matches!(
        verdict(&room, the_call(), seen),
        Verdict::Wait { approvals: 0, .. }
    ));
}

#[test]
fn a_humans_signature_pasted_from_another_proposal_never_counts() {
    // Recorded live: re-publishing a human's old signature under a new proposal
    // for the same call makes it executable, graded "unattested".
    let room = config(&[HUMAN], 1);
    let seen = muster(
        "executable",
        &[Sig(AGENT, COMMITTED), Sig(HUMAN, "unattested")],
        &[],
        TOPIC,
        EXPIRY,
    );
    assert!(matches!(
        verdict(&room, the_call(), seen),
        Verdict::Wait { approvals: 0, .. }
    ));
}

#[test]
fn every_required_approval_must_come_from_a_distinct_trusted_human() {
    let room = config(&[HUMAN, OTHER], 2);
    let seen = muster(
        "executable",
        &[Sig(AGENT, COMMITTED), Sig(HUMAN, COMMITTED)],
        &[],
        TOPIC,
        EXPIRY,
    );
    assert!(matches!(
        verdict(&room, the_call(), seen),
        Verdict::Wait { approvals: 1, .. }
    ));
    let both = muster(
        "executable",
        &[Sig(HUMAN, COMMITTED), Sig(OTHER, COMMITTED)],
        &[],
        TOPIC,
        EXPIRY,
    );
    assert_eq!(
        verdict(&room, the_call(), both),
        Verdict::Send { approvals: 2 }
    );
}

#[test]
fn a_proposal_the_room_has_not_made_executable_waits() {
    let room = config(&[HUMAN], 1);
    let seen = muster("collecting", &[Sig(HUMAN, COMMITTED)], &[], TOPIC, EXPIRY);
    assert_eq!(
        verdict(&room, the_call(), seen),
        Verdict::Wait {
            state: "collecting".into(),
            approvals: 1
        }
    );
    let missing = (json!([]), json!([]));
    assert!(matches!(
        verdict(&room, the_call(), missing),
        Verdict::Wait { approvals: 0, .. }
    ));
}

#[test]
fn a_room_approval_for_another_amount_or_recipient_is_refused() {
    let room = config(&[HUMAN], 1);
    let approved = || {
        muster(
            "executable",
            &[Sig(AGENT, COMMITTED), Sig(HUMAN, COMMITTED)],
            &[],
            TOPIC,
            EXPIRY,
        )
    };
    for other in [
        json!([FROM, TO, "7000001"]),
        json!([FROM, FROM, "7000000"]),
        json!([TO, TO, "7000000"]),
    ] {
        assert!(
            matches!(verdict(&room, other, approved()), Verdict::Refused(ref r) if r.contains("not this transfer")),
        );
    }
}

#[test]
fn a_declined_proposal_ends_unsent() {
    let room = config(&[HUMAN], 1);
    let seen = muster(
        "declined",
        &[Sig(AGENT, COMMITTED)],
        &[HUMAN],
        TOPIC,
        EXPIRY,
    );
    assert_eq!(verdict(&room, the_call(), seen), Verdict::Declined);
    // Too many trusted humans declined for the rest to approve, whatever Muster's
    // own threshold says.
    let room = config(&[HUMAN, OTHER], 2);
    let seen = muster(
        "collecting",
        &[Sig(AGENT, COMMITTED), Sig(OTHER, COMMITTED)],
        &[HUMAN],
        TOPIC,
        EXPIRY,
    );
    assert_eq!(verdict(&room, the_call(), seen), Verdict::Declined);
    // A decline from someone untrusted changes nothing.
    let room = config(&[HUMAN], 1);
    let seen = muster(
        "executable",
        &[Sig(HUMAN, COMMITTED)],
        &[OTHER],
        TOPIC,
        EXPIRY,
    );
    assert_eq!(
        verdict(&room, the_call(), seen),
        Verdict::Send { approvals: 1 }
    );
}

#[test]
fn a_proposal_bound_to_another_room_or_past_its_expiry_is_refused() {
    let room = config(&[HUMAN], 1);
    let sigs = [Sig(AGENT, COMMITTED), Sig(HUMAN, COMMITTED)];
    let elsewhere = muster(
        "executable",
        &sigs,
        &[],
        "/muster/1/elsewhere/proto",
        EXPIRY,
    );
    assert!(
        matches!(verdict(&room, the_call(), elsewhere), Verdict::Refused(ref r) if r.contains("another room"))
    );
    let stale = muster("executable", &sigs, &[], TOPIC, NOW - 1);
    assert!(
        matches!(verdict(&room, the_call(), stale), Verdict::Refused(ref r) if r.contains("expired"))
    );
}

#[test]
fn a_proposal_someone_already_executed_in_muster_is_refused() {
    let room = config(&[HUMAN], 1);
    for state in ["submitted", "final", "failed"] {
        let seen = muster(
            state,
            &[Sig(AGENT, COMMITTED), Sig(HUMAN, COMMITTED)],
            &[],
            TOPIC,
            EXPIRY,
        );
        assert!(
            matches!(verdict(&room, the_call(), seen), Verdict::Refused(_)),
            "{state}"
        );
    }
}

#[test]
fn an_approval_missing_from_the_rooms_provenance_never_counts() {
    // The intent record grades it committed, but coordinate_provenance has no
    // sig row for it: both must agree.
    let room = config(&[HUMAN], 1);
    let (intents, mut provenance) = muster(
        "executable",
        &[Sig(AGENT, COMMITTED), Sig(HUMAN, COMMITTED)],
        &[],
        TOPIC,
        EXPIRY,
    );
    provenance
        .as_array_mut()
        .unwrap()
        .retain(|r| r["account"] != format!("ed:{HUMAN}"));
    assert!(matches!(
        verdict(&room, the_call(), (intents, provenance)),
        Verdict::Wait { approvals: 0, .. }
    ));
}

#[tokio::test]
async fn a_room_approval_is_used_once() {
    // A used approval is refused before Muster is even asked.
    let fake = crate::logosctl::tests::FakeRunner::with(vec![]);
    let node = crate::logosctl::Logosctl::new(fake.clone());
    let room = Room::new();
    let settings = config(&[HUMAN], 1);
    assert!(room.use_once(ID));
    assert!(!room.use_once(ID));
    let call = RoomCall {
        module: "lez_core",
        method: "transfer_public",
        args: the_call(),
        note: String::new(),
    };
    let seen = room.look(&node, &settings, ID, &call, NOW).await.unwrap();
    assert!(
        matches!(seen, Verdict::Refused(ref r) if r.contains("already used")),
        "{seen:?}"
    );
    assert!(fake.calls.lock().unwrap().is_empty());
}

#[test]
fn an_approval_named_by_a_roster_identity_counts_like_its_key() {
    let room = config(&[HUMAN], 1);
    let (mut intents, mut provenance) = muster(
        "executable",
        &[Sig(AGENT, COMMITTED), Sig(HUMAN, COMMITTED)],
        &[],
        TOPIC,
        EXPIRY,
    );
    let roster = format!("{HUMAN}1e04b737de62a329682bb6971b7dd70eb7c6b01f68eb1992bcc8d1142d881d20");
    for row in provenance.as_array_mut().unwrap() {
        if row["account"] == format!("ed:{HUMAN}") {
            row["account"] = json!(roster);
        }
    }
    for row in intents[0]["provenance"].as_array_mut().unwrap() {
        if row["account"] == format!("ed:{HUMAN}") {
            row["account"] = json!(roster);
        }
    }
    assert_eq!(
        verdict(&room, the_call(), (intents, provenance)),
        Verdict::Send { approvals: 1 }
    );
}

#[test]
fn a_trusted_human_who_declined_never_counts_even_after_approving() {
    // Muster: a decline is final, whichever arrived first.
    let room = config(&[HUMAN, OTHER], 1);
    let seen = muster(
        "executable",
        &[Sig(AGENT, COMMITTED), Sig(HUMAN, COMMITTED)],
        &[HUMAN],
        TOPIC,
        EXPIRY,
    );
    assert!(matches!(
        verdict(&room, the_call(), seen),
        Verdict::Wait { approvals: 0, .. }
    ));
}

// ---- program calls ------------------------------------------------------------

/// CBOR text, as Muster's dCBOR writes it.
fn cbor_text(out: &mut Vec<u8>, s: &str) {
    let n = s.len();
    match n {
        0..=23 => out.push(0x60 | n as u8),
        24..=255 => out.extend([0x78, n as u8]),
        _ => out.extend([0x79, (n >> 8) as u8, n as u8]),
    }
    out.extend(s.as_bytes());
}

/// The txhash Muster folds for an invoke of lez_core.`method`(`args`).
pub(crate) fn invoke_txhash(method: &str, args: &Value) -> String {
    invoke_txhash_in("lez_core", method, args)
}

/// The txhash Muster folds for an invoke of `module`.`method`(`args`).
pub(crate) fn invoke_txhash_in(module: &str, method: &str, args: &Value) -> String {
    let mut b = vec![0x83];
    cbor_text(&mut b, "muster.invoke.v1");
    cbor_text(&mut b, &format!("muster.invoke.{module}.{method}.v1"));
    b.push(0xa3);
    for (k, v) in [
        ("args", args.to_string()),
        ("method", method.to_string()),
        ("module", module.to_string()),
    ] {
        cbor_text(&mut b, k);
        cbor_text(&mut b, &v);
    }
    format!(
        "0x{}",
        b.iter().map(|x| format!("{x:02x}")).collect::<String>()
    )
}

#[test]
fn the_test_encoder_matches_what_muster_folded() {
    assert_eq!(invoke_txhash("transfer_public", &the_call()), TXHASH_7M);
}

const PROGRAM: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const ESCROW: &str = "2222222222222222222222222222222222222222222222222222222222222222";

/// An escrow open as logos-mcp sends it: signer and escrow account, who signs,
/// the instruction, the program, then nothing.
fn program_args(program: &str, accounts: &[&str], signs: &[bool], instruction: &str) -> Value {
    json!([accounts, signs, {"_bytes": instruction}, program, "", []])
}

#[test]
fn a_program_call_passes_only_when_the_room_signed_exactly_it() {
    let room = config(&[HUMAN], 1);
    let signer = "f44f59079d950ccb7ad230e5fa6e0668adf83eab47785d1bb0b9bab746b0cd64";
    let sent = program_args(PROGRAM, &[signer, ESCROW], &[true, false], "AQID");
    let call = RoomCall {
        module: "lez_core",
        method: "send_generic_public_transaction",
        args: sent.clone(),
        note: String::new(),
    };
    let folded = |method: &str, args: &Value| {
        let (mut intents, provenance) = muster(
            "executable",
            &[Sig(AGENT, COMMITTED), Sig(HUMAN, COMMITTED)],
            &[],
            TOPIC,
            EXPIRY,
        );
        intents[0]["txhash"] = json!(invoke_txhash(method, args));
        (intents, provenance)
    };
    let judged =
        |(intents, provenance): (Value, Value)| judge(&room, ID, &call, &intents, &provenance, NOW);
    assert_eq!(
        judged(folded("send_generic_public_transaction", &sent)),
        Verdict::Send { approvals: 1 }
    );
    for (method, other) in [
        (
            "send_generic_public_transaction",
            program_args(PROGRAM, &[signer, ESCROW], &[true, false], "AQIE"),
        ),
        (
            "send_generic_public_transaction",
            program_args(ESCROW, &[signer, ESCROW], &[true, false], "AQID"),
        ),
        (
            "send_generic_public_transaction",
            program_args(PROGRAM, &[signer, PROGRAM], &[true, false], "AQID"),
        ),
        (
            "send_generic_public_transaction",
            program_args(PROGRAM, &[signer, ESCROW], &[true, true], "AQID"),
        ),
        ("transfer_public", sent.clone()),
    ] {
        assert!(
            matches!(judged(folded(method, &other)), Verdict::Refused(ref r) if r.contains("not this transfer")),
            "{method} {other}"
        );
    }
}
