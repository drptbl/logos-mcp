//! Room approval through wallet_transfer: the agent's Muster faked at the
//! logosctl seam, answering by method, and lez_core replayed in order.

use super::*;
use crate::logosctl::{RawOutput, RunFuture, Runner};
use crate::room::RoomApproval;
use std::collections::{BTreeMap, VecDeque};

const HUMAN: &str = "0b00086682a0d169f90095b9d58e6c003ee591531ccf8b59c712c1cb70603269";
const AGENT_KEY: &str = "d8f76a7a69e956b81bf3c8abfcebec21c2c65aeccfa6f7bfe58685ccee9b1e6b";
const X25519: &str = "1e04b737de62a329682bb6971b7dd70eb7c6b01f68eb1992bcc8d1142d881d20";
const TOPIC: &str = "/muster/1/agent-room/proto";
const INTENT: &str = "0x00000000000000aa";
const OVER: &str = "5000000";

/// Muster answers by method, each from a queue whose last answer repeats;
/// everything else goes to the ordered lez_core replies.
struct RoomRunner {
    lez: Arc<FakeRunner>,
    muster: Mutex<BTreeMap<String, VecDeque<Value>>>,
    calls: Mutex<Vec<Vec<String>>>,
}

impl RoomRunner {
    fn new(lez: Vec<RawOutput>) -> Arc<Self> {
        let runner = Arc::new(Self {
            lez: FakeRunner::with(lez),
            muster: Mutex::default(),
            calls: Mutex::default(),
        });
        runner.answer("identity", json!({"ed25519": AGENT_KEY, "x25519": X25519}));
        runner.answer(
            "connectivity",
            json!({"rows": [{"key": "delivery", "level": "ok"}]}),
        );
        runner.answer(
            "coordinate_conversations",
            json!([{"topic": TOPIC, "active": true}]),
        );
        runner.answer(
            "coordinate_members",
            json!([
                {"identity": format!("{HUMAN}{X25519}"), "self": false},
                {"identity": format!("{AGENT_KEY}{X25519}"), "self": true},
            ]),
        );
        runner.answer(
            "coordinate_set_policy",
            json!({"policy": "invoke", "threshold": 2}),
        );
        runner.answer("coordinate_propose", json!(INTENT));
        runner.answer("coordinate_contribute", json!("collecting"));
        runner.answer("coordinate_intents", json!([]));
        runner.answer("coordinate_provenance", json!([]));
        runner
    }

    /// From now on `method` answers `value`, after any answers still queued.
    fn answer(&self, method: &str, value: Value) {
        self.muster
            .lock()
            .unwrap()
            .entry(method.into())
            .or_default()
            .push_back(value);
    }

    /// `method` answers `value` from now on, whatever was queued.
    fn always(&self, method: &str, value: Value) {
        self.muster
            .lock()
            .unwrap()
            .insert(method.into(), VecDeque::from([value]));
    }

    fn muster_calls(&self, method: &str) -> Vec<Vec<String>> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.get(2).map(String::as_str) == Some("muster_module") && c[3] == method)
            .cloned()
            .collect()
    }

    fn sends(&self) -> usize {
        sends(&self.lez)
    }
}

impl Runner for RoomRunner {
    fn run(&self, args: Vec<String>) -> RunFuture<'_> {
        self.calls.lock().unwrap().push(args.clone());
        if args.get(1).map(String::as_str) == Some("module") {
            return Box::pin(async { Ok(out(r#"[{"name":"muster_module","status":"loaded"}]"#)) });
        }
        if args.get(2).map(String::as_str) != Some("muster_module") {
            return self.lez.run(args);
        }
        let method = args[3].clone();
        let value = {
            let mut muster = self.muster.lock().unwrap();
            let queue = muster.entry(method.clone()).or_default();
            if queue.len() > 1 {
                queue.pop_front()
            } else {
                queue.front().cloned()
            }
        }
        .unwrap_or(Value::Null);
        // Muster returns its JSON as a string, as recorded.
        let result = match value {
            Value::String(s) => s,
            other => other.to_string(),
        };
        let reply =
            json!({"method": method, "module": "muster_module", "result": result, "status": "ok"});
        Box::pin(async move { Ok(out(&reply.to_string())) })
    }
}

fn room_server(runner: Arc<RoomRunner>, approvers: &[&str]) -> (LogosServer, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("agent-accounts.json");
    std::fs::write(&file, serde_json::to_string(&[AGENT_ACCOUNT]).unwrap()).unwrap();
    let room = RoomApproval::new(
        TOPIC,
        &approvers
            .iter()
            .map(|a| format!("ed:{a}"))
            .collect::<Vec<_>>(),
        1,
        3_000_000,
        60,
    )
    .unwrap();
    let server = LogosServer::new(
        Arc::new(Logosctl::new(runner)),
        Arc::new(EventStore::default()),
        None,
        NodeSettings {
            preset: "logos.test".into(),
            mode: "Edge".into(),
            config_dir: None,
            wallet: Some(WalletMode::Shared {
                agent_accounts_file: file,
            }),
            discovery_bootstrap: vec![],
            host_owns_messaging: false,
            delivery_config: None,
        },
    )
    .with_room_approval(room);
    (server, dir)
}

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

/// The txhash Muster folds for an invoke of lez_core.transfer_public(`args`).
fn txhash(args: &Value) -> String {
    let mut b = vec![0x83];
    cbor_text(&mut b, "muster.invoke.v1");
    cbor_text(&mut b, "muster.invoke.lez_core.transfer_public.v1");
    b.push(0xa3);
    for (k, v) in [
        ("args", args.to_string()),
        ("method", "transfer_public".into()),
        ("module", "lez_core".into()),
    ] {
        cbor_text(&mut b, k);
        cbor_text(&mut b, &v);
    }
    format!(
        "0x{}",
        b.iter().map(|x| format!("{x:02x}")).collect::<String>()
    )
}

/// The agent's Muster after the room folded the proposal of `args` to `state`
/// with these approvers' committed approvals and these decliners.
fn room_folded(
    runner: &RoomRunner,
    state: &str,
    args: Value,
    approvers: &[&str],
    decliners: &[&str],
) {
    let record: Vec<Value> = approvers
        .iter()
        .map(|a| json!({"class": "driver-contribution", "account": format!("ed:{a}"), "attestation": "committed"}))
        .collect();
    runner.always(
        "coordinate_intents",
        json!([{
            "id": INTENT,
            "state": state,
            "approvals": approvers.len(),
            "txhash": txhash(&args),
            "decliners": decliners.iter().map(|d| format!("{d}{X25519}")).collect::<Vec<_>>(),
            "provenance": record,
        }]),
    );
    let mut rows = vec![json!({"kind": "context", "intentId": INTENT,
        "detail": json!({"environment": "room", "account": TOPIC, "expiry": "4102444800"}).to_string()})];
    rows.extend(
        approvers
            .iter()
            .map(|a| json!({"kind": "sig", "intentId": INTENT, "account": format!("ed:{a}")})),
    );
    runner.always("coordinate_provenance", Value::Array(rows));
}

fn the_call(amount: &str) -> Value {
    json!([AGENT_FROM, RLN_PAYER, amount])
}

/// Start an over-limit transfer; returns its call once the room has its proposal.
async fn propose(s: &LogosServer, runner: &RoomRunner) -> tokio::task::JoinHandle<CallToolResult> {
    let agent = s.clone();
    let call = tokio::spawn(async move {
        agent
            .wallet_transfer(
                CancellationToken::new(),
                transfer(AGENT_FROM, RLN_PAYER, OVER),
            )
            .await
            .unwrap()
    });
    // Paused time: long enough for Muster's delivery node to come up.
    for _ in 0..12_000 {
        if !runner.muster_calls("coordinate_contribute").is_empty() {
            return call;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("nothing was proposed");
}

async fn status_of(s: &LogosServer, id: &str) -> Value {
    body(
        &s.wallet_transfer_status(Parameters(TransferStatusParams {
            request_id: id.into(),
        }))
        .await
        .unwrap(),
    )
}

#[tokio::test(start_paused = true)]
async fn an_over_limit_transfer_becomes_a_room_proposal_and_waits() {
    let runner = RoomRunner::new(wallet_open().to_vec());
    let (s, _dir) = room_server(runner.clone(), &[HUMAN]);
    let call = propose(&s, &runner).await;
    let r = call.await.unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    assert_eq!(body(&r)["status"], "awaiting_approval");
    let id = body(&r)["request_id"].as_str().unwrap().to_string();

    let policy = runner.muster_calls("coordinate_set_policy");
    assert_eq!(policy[0][4], "str:invoke");
    let proposed = runner.muster_calls("coordinate_propose");
    let effect: Value = serde_json::from_str(proposed[0][4].strip_prefix("str:").unwrap()).unwrap();
    assert_eq!(effect["effect"], "invoke");
    assert_eq!(effect["module"], "lez_core");
    assert_eq!(effect["method"], "transfer_public");
    assert_eq!(effect["args"], the_call(OVER));
    assert_eq!(effect["ref"], format!("logos-mcp/{id}"));
    assert_eq!(
        runner.muster_calls("coordinate_contribute")[0][4..],
        [format!("str:{INTENT}"), "str:".into(), "str:".into()]
    );
    assert_eq!(
        runner.sends(),
        0,
        "nothing is sent before the room approves"
    );
    assert!(
        s.approvals()["waiting"].as_array().unwrap().is_empty(),
        "the app isn't asked"
    );

    let status = status_of(&s, &id).await;
    assert_eq!(status["state"], "waiting");
    assert_eq!(status["room"]["intent_id"], INTENT);
    assert_eq!(status["room"]["topic"], TOPIC);
    assert_eq!(status["room"]["required"], 1);
}

#[tokio::test(start_paused = true)]
async fn a_transfer_at_or_under_the_limit_is_sent_without_the_room() {
    let mut replies = wallet_open().to_vec();
    replies.push(sent("ab12"));
    let runner = RoomRunner::new(replies);
    let (s, _dir) = room_server(runner.clone(), &[HUMAN]);
    let r = s
        .wallet_transfer(
            CancellationToken::new(),
            transfer(AGENT_FROM, RLN_PAYER, "3000000"),
        )
        .await
        .unwrap();
    assert_eq!(body(&r)["tx_hash"], "ab12", "{}", text(&r));
    assert!(runner.muster_calls("coordinate_propose").is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_room_approval_sends_the_transfer_exactly_once() {
    let mut replies = wallet_open().to_vec(); // checks before proposing
    replies.extend(wallet_open()); // checks again before sending
    replies.push(sent("cd34"));
    let runner = RoomRunner::new(replies);
    let (s, _dir) = room_server(runner.clone(), &[HUMAN]);
    let call = propose(&s, &runner).await;
    room_folded(
        &runner,
        "executable",
        the_call(OVER),
        &[AGENT_KEY, HUMAN],
        &[],
    );
    let r = call.await.unwrap();
    assert_eq!(body(&r)["tx_hash"], "cd34", "{}", text(&r));
    tokio::time::sleep(std::time::Duration::from_secs(600)).await;
    assert_eq!(runner.sends(), 1);
    let sent_args = runner
        .calls
        .lock()
        .unwrap()
        .iter()
        .find(|c| c.get(3).map(String::as_str) == Some("transfer_public"))
        .cloned()
        .unwrap();
    assert_eq!(sent_args[4], format!("str:{AGENT_ACCOUNT}"));
}

#[tokio::test(start_paused = true)]
async fn a_room_proposal_without_a_trusted_approval_is_never_sent() {
    let runner = RoomRunner::new(wallet_open().to_vec());
    let (s, _dir) = room_server(runner.clone(), &[HUMAN]);
    let call = propose(&s, &runner).await;
    // Executable on the agent's and an untrusted member's approvals alone.
    let stranger = "7a1c000000000000000000000000000000000000000000000000000000e0f2aa";
    room_folded(
        &runner,
        "executable",
        the_call(OVER),
        &[AGENT_KEY, stranger],
        &[],
    );
    let id = body(&call.await.unwrap())["request_id"]
        .as_str()
        .unwrap()
        .to_string();
    tokio::time::sleep(std::time::Duration::from_secs(61 * 60)).await;
    assert_eq!(status_of(&s, &id).await["state"], "expired");
    assert_eq!(runner.sends(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_room_approval_of_another_amount_is_refused() {
    let runner = RoomRunner::new(wallet_open().to_vec());
    let (s, _dir) = room_server(runner.clone(), &[HUMAN]);
    let call = propose(&s, &runner).await;
    room_folded(
        &runner,
        "executable",
        the_call("5000001"),
        &[AGENT_KEY, HUMAN],
        &[],
    );
    let r = call.await.unwrap();
    assert!(is_error(&r));
    assert!(text(&r).contains("not this transfer"), "{}", text(&r));
    assert_eq!(runner.sends(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_declined_room_proposal_is_never_sent() {
    let runner = RoomRunner::new(wallet_open().to_vec());
    let (s, _dir) = room_server(runner.clone(), &[HUMAN]);
    let call = propose(&s, &runner).await;
    room_folded(&runner, "declined", the_call(OVER), &[AGENT_KEY], &[HUMAN]);
    let r = call.await.unwrap();
    assert!(is_error(&r));
    assert!(text(&r).contains("declined"), "{}", text(&r));
    assert_eq!(runner.sends(), 0);
}

#[tokio::test(start_paused = true)]
async fn the_agents_own_key_as_an_approver_stops_the_proposal() {
    let runner = RoomRunner::new(wallet_open().to_vec());
    let (s, _dir) = room_server(runner.clone(), &[HUMAN, AGENT_KEY]);
    let r = s
        .wallet_transfer(
            CancellationToken::new(),
            transfer(AGENT_FROM, RLN_PAYER, OVER),
        )
        .await
        .unwrap();
    assert!(is_error(&r));
    assert!(text(&r).contains("own Muster key"), "{}", text(&r));
    assert!(runner.muster_calls("coordinate_propose").is_empty());
}

#[tokio::test(start_paused = true)]
async fn the_first_proposal_waits_for_musters_delivery_node() {
    let runner = RoomRunner::new(wallet_open().to_vec());
    runner.always(
        "connectivity",
        json!({"rows": [{"key": "delivery", "level": "down"}]}),
    );
    runner.answer(
        "connectivity",
        json!({"rows": [{"key": "delivery", "level": "down"}]}),
    );
    runner.answer(
        "connectivity",
        json!({"rows": [{"key": "delivery", "level": "ok"}]}),
    );
    let (s, _dir) = room_server(runner.clone(), &[HUMAN]);
    let _call = propose(&s, &runner).await;
    let calls = runner.calls.lock().unwrap().clone();
    let at = |m: &str| {
        calls
            .iter()
            .position(|c| c.get(3).map(String::as_str) == Some(m))
            .unwrap()
    };
    assert_eq!(runner.muster_calls("connectivity").len(), 3);
    assert!(
        at("coordinate_propose")
            > calls
                .iter()
                .rposition(|c| c.get(3).map(String::as_str) == Some("connectivity"))
                .unwrap()
    );
    assert!(at("coordinate_propose") > at("identity"));
}

#[tokio::test(start_paused = true)]
async fn a_program_call_over_the_limit_is_refused() {
    let runner = RoomRunner::new(wallet_open().to_vec());
    let (s, _dir) = room_server(runner.clone(), &[HUMAN]);
    let mut p = transfer(AGENT_FROM, RLN_PAYER, OVER);
    p.0.call = Some(crate::control::ProgramCall {
        program_id_hex: "11".repeat(32),
        accounts: vec![guard::public_account_hex(RLN_PAYER).unwrap()],
        instruction: vec![1],
        description: "into escrow".into(),
        send_by_ms: None,
    });
    let r = s
        .wallet_transfer(CancellationToken::new(), p)
        .await
        .unwrap();
    assert!(is_error(&r));
    assert!(text(&r).contains("room"), "{}", text(&r));
    assert_eq!(runner.sends(), 0);
    assert!(runner.muster_calls("coordinate_propose").is_empty());
}

#[tokio::test(start_paused = true)]
async fn the_app_cannot_answer_a_room_request() {
    let runner = RoomRunner::new(wallet_open().to_vec());
    let (s, _dir) = room_server(runner.clone(), &[HUMAN]);
    let id = body(&propose(&s, &runner).await.await.unwrap())["request_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(s.approve_transfer(&id).await.is_err());
    assert!(s.deny_transfer(&id).is_err());
    assert_eq!(runner.sends(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_muster_read_error_is_retried_never_taken_as_approval() {
    let mut replies = wallet_open().to_vec();
    replies.extend(wallet_open());
    replies.push(sent("ef56"));
    let runner = RoomRunner::new(replies);
    let (s, _dir) = room_server(runner.clone(), &[HUMAN]);
    let call = propose(&s, &runner).await;
    runner.always(
        "coordinate_intents",
        json!({"error": "store node unreachable"}),
    );
    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    assert_eq!(runner.sends(), 0);
    room_folded(
        &runner,
        "executable",
        the_call(OVER),
        &[AGENT_KEY, HUMAN],
        &[],
    );
    let r = call.await.unwrap();
    assert_eq!(body(&r)["tx_hash"], "ef56", "{}", text(&r));
    assert_eq!(runner.sends(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_proposal_that_already_had_approvals_is_refused() {
    let runner = RoomRunner::new(wallet_open().to_vec());
    runner.always(
        "coordinate_intents",
        json!([{"id": INTENT, "state": "executable", "approvals": 2}]),
    );
    let (s, _dir) = room_server(runner.clone(), &[HUMAN]);
    let r = s
        .wallet_transfer(
            CancellationToken::new(),
            transfer(AGENT_FROM, RLN_PAYER, OVER),
        )
        .await
        .unwrap();
    assert!(is_error(&r));
    assert!(text(&r).contains("already had approvals"), "{}", text(&r));
    assert!(runner.muster_calls("coordinate_contribute").is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_room_without_enough_trusted_members_or_not_active_is_refused() {
    let runner = RoomRunner::new(wallet_open().to_vec());
    runner.always(
        "coordinate_members",
        json!([{"identity": format!("{AGENT_KEY}{X25519}"), "self": true}]),
    );
    let (s, _dir) = room_server(runner.clone(), &[HUMAN]);
    let r = s
        .wallet_transfer(
            CancellationToken::new(),
            transfer(AGENT_FROM, RLN_PAYER, OVER),
        )
        .await
        .unwrap();
    assert!(
        text(&r).contains("only 0 of the trusted approvers"),
        "{}",
        text(&r)
    );

    let runner = RoomRunner::new(wallet_open().to_vec());
    runner.always(
        "coordinate_conversations",
        json!([{"topic": TOPIC, "active": false}]),
    );
    let (s, _dir) = room_server(runner.clone(), &[HUMAN]);
    let r = s
        .wallet_transfer(
            CancellationToken::new(),
            transfer(AGENT_FROM, RLN_PAYER, OVER),
        )
        .await
        .unwrap();
    assert!(text(&r).contains("isn't the active room"), "{}", text(&r));
    assert!(runner.muster_calls("coordinate_join").is_empty());
    assert!(runner.muster_calls("coordinate_propose").is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_request_that_ends_unsent_withdraws_the_agents_approval() {
    // Expired: the agent declines its own proposal, so the card closes for the room.
    let runner = RoomRunner::new(wallet_open().to_vec());
    let (s, _dir) = room_server(runner.clone(), &[HUMAN]);
    let call = propose(&s, &runner).await;
    call.await.unwrap();
    tokio::time::sleep(std::time::Duration::from_secs(61 * 60)).await;
    assert_eq!(
        runner.muster_calls("coordinate_decline")[0][4],
        format!("str:{INTENT}")
    );

    // Cancelled by the agent.
    let runner = RoomRunner::new(wallet_open().to_vec());
    let (s, _dir) = room_server(runner.clone(), &[HUMAN]);
    let agent = s.clone();
    let ct = CancellationToken::new();
    let cancel = ct.clone();
    let call = tokio::spawn(async move {
        agent
            .wallet_transfer(ct, transfer(AGENT_FROM, RLN_PAYER, OVER))
            .await
            .unwrap()
    });
    for _ in 0..12_000 {
        if !runner.muster_calls("coordinate_contribute").is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    cancel.cancel();
    assert!(text(&call.await.unwrap()).contains("cancelled"));
    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    assert_eq!(runner.muster_calls("coordinate_decline").len(), 1);
    assert_eq!(runner.sends(), 0);
}
