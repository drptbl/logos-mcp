//! Room approval through wallet_transfer: the agent's Muster faked at the
//! logosctl seam, answering by method, and lez_core replayed in order.

use super::*;
use crate::logosctl::{RawOutput, RunFuture, Runner};
use crate::policy::{self, ApprovalPolicy, RoomSettings, Tiers};
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

/// The policy the CLI builds from `--room-*` flags with a 3,000,000 limit:
/// the agent wallet sends up to it and asks the room above it, and chain
/// transfers always ask the room.
fn cli_policy(approvers: &[&str]) -> ApprovalPolicy {
    ApprovalPolicy {
        agent: Tiers {
            limit_lepta: 3_000_000,
            up_to: policy::Approver::Send,
            above: policy::Approver::Room,
        },
        node: Tiers {
            limit_lepta: 0,
            up_to: policy::Approver::Room,
            above: policy::Approver::Room,
        },
        room: Some(RoomSettings {
            topic: TOPIC.into(),
            approvers: approvers.iter().map(|a| format!("ed:{a}")).collect(),
            approvals: 1,
            expires_mins: 60,
        }),
    }
}

fn room_server(runner: Arc<RoomRunner>, approvers: &[&str]) -> (LogosServer, tempfile::TempDir) {
    let (server, dir, _policy) = policy_server(runner, cli_policy(approvers), false);
    (server, dir)
}

/// A shared-wallet server following `policy`, which the returned sender
/// changes live. `in_basecamp` says whether there is an app to ask the user in.
fn policy_server(
    runner: Arc<RoomRunner>,
    policy: ApprovalPolicy,
    in_basecamp: bool,
) -> (
    LogosServer,
    tempfile::TempDir,
    tokio::sync::watch::Sender<ApprovalPolicy>,
) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("agent-accounts.json");
    std::fs::write(&file, serde_json::to_string(&[AGENT_ACCOUNT]).unwrap()).unwrap();
    let (tx, rx) = tokio::sync::watch::channel(policy);
    let node: Arc<dyn Node> = Arc::new(Logosctl::new(runner));
    let server = LogosServer::new(
        node.clone(),
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
            host_owns_messaging: in_basecamp,
            delivery_config: None,
        },
    )
    .with_approval_policy(rx);
    // In Basecamp the agent's Muster is its own seat, here the same fake.
    let server = if in_basecamp {
        server.with_muster(node)
    } else {
        server
    };
    (server, dir, tx)
}

/// The agent's Muster after the room folded the transfer proposal of `args`
/// to `state` with these approvers' committed approvals and these decliners.
fn room_folded(
    runner: &RoomRunner,
    state: &str,
    args: Value,
    approvers: &[&str],
    decliners: &[&str],
) {
    room_folded_call(runner, state, "transfer_public", args, approvers, decliners);
}

/// The same, for a proposal of lez_core.`method`(`args`).
fn room_folded_call(
    runner: &RoomRunner,
    state: &str,
    method: &str,
    args: Value,
    approvers: &[&str],
    decliners: &[&str],
) {
    room_folded_in(
        runner, state, "lez_core", method, args, approvers, decliners,
    );
}

/// The same, for a proposal of `module`.`method`(`args`).
fn room_folded_in(
    runner: &RoomRunner,
    state: &str,
    module: &str,
    method: &str,
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
            "txhash": crate::room::tests::invoke_txhash_in(module, method, &args),
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
    propose_this(s, runner, transfer(AGENT_FROM, RLN_PAYER, OVER)).await
}

/// Start `p`; returns its call once the room has its proposal.
async fn propose_this(
    s: &LogosServer,
    runner: &RoomRunner,
    p: Parameters<TransferParams>,
) -> tokio::task::JoinHandle<CallToolResult> {
    let agent = s.clone();
    let call = tokio::spawn(async move {
        agent
            .wallet_transfer(CancellationToken::new(), p)
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
        effect["note"],
        format!("Pay {OVER} lepta from {AGENT_FROM} to {RLN_PAYER}")
    );
    assert_eq!(
        runner.muster_calls("coordinate_post_message")[0][4],
        format!(
            "str:[logos-mcp] proposal {INTENT}: Pay {OVER} lepta from {AGENT_FROM} to {RLN_PAYER}"
        )
    );
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

const ESCROW_NOTE: &str =
    "into escrow for Public/p, refunded if nothing is delivered by 2026-11-16 12:00 UTC";

/// An over-limit escrow open, as logos-agents builds one.
fn escrow_payment(send_by_ms: Option<u64>) -> Parameters<TransferParams> {
    let mut p = transfer(AGENT_FROM, CALL_TO, OVER);
    p.0.call = Some(crate::control::ProgramCall {
        program_id_hex: TEST_PROGRAM.into(),
        accounts: vec![CALL_TO.into()],
        instruction: vec![1, 2, 3],
        description: ESCROW_NOTE.into(),
        send_by_ms,
    });
    p
}

/// The escrow open exactly as logos-mcp sends it, in-process form.
fn escrow_args(instruction: &str) -> Value {
    json!([[AGENT_ACCOUNT, CALL_TO], [true, false], {"_bytes": instruction}, TEST_PROGRAM, "", []])
}

/// The checks a program call passes before it is proposed, and again before
/// it is sent: the wallet, the sender's balance, and the fee reserve.
fn call_checks() -> Vec<RawOutput> {
    let mut replies = wallet_open().to_vec();
    replies.push(lez("get_balance", r#""100000000000""#));
    replies.push(lez("wallet_dir", r#""""#));
    replies
}

fn generic_sent(hash: &str) -> RawOutput {
    lez(
        "send_generic_public_transaction",
        &format!(r#""{{\"error\":\"\",\"success\":true,\"tx_hash\":\"{hash}\"}}""#),
    )
}

fn generic_sends(runner: &RoomRunner) -> Vec<Vec<String>> {
    runner
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c.get(3).map(String::as_str) == Some("send_generic_public_transaction"))
        .cloned()
        .collect()
}

#[tokio::test(start_paused = true)]
async fn an_escrowed_payment_waits_for_the_room_and_sends_exactly_the_approved_call() {
    let mut replies = call_checks();
    replies.extend(call_checks());
    replies.push(generic_sent("bb22"));
    let runner = RoomRunner::new(replies);
    let (s, _dir) = room_server(runner.clone(), &[HUMAN]);
    let call = propose_this(&s, &runner, escrow_payment(None)).await;

    let proposed = runner.muster_calls("coordinate_propose");
    let effect: Value = serde_json::from_str(proposed[0][4].strip_prefix("str:").unwrap()).unwrap();
    assert_eq!(effect["method"], "send_generic_public_transaction");
    assert_eq!(effect["args"], escrow_args("AQID"));
    let note = effect["note"].as_str().unwrap();
    assert!(note.contains(OVER) && note.contains(ESCROW_NOTE), "{note}");
    // The room reads what it does in its chat too.
    let posted = runner.muster_calls("coordinate_post_message");
    assert!(
        posted[0][4].contains(INTENT) && posted[0][4].contains(ESCROW_NOTE),
        "{posted:?}"
    );
    assert!(generic_sends(&runner).is_empty());

    room_folded_call(
        &runner,
        "executable",
        "send_generic_public_transaction",
        escrow_args("AQID"),
        &[AGENT_KEY, HUMAN],
        &[],
    );
    let r = call.await.unwrap();
    assert_eq!(body(&r)["tx_hash"], "bb22", "{}", text(&r));
    let sends = generic_sends(&runner);
    assert_eq!(sends.len(), 1);
    assert_eq!(
        sends[0][4..],
        [
            format!(r#"json:["{AGENT_ACCOUNT}","{CALL_TO}"]"#),
            "json:[true,false]".into(),
            r#"json:{"_bytes":"AQID"}"#.into(),
            format!("str:{TEST_PROGRAM}"),
            "str:".into(),
            "json:[]".into(),
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn a_room_approval_of_another_instruction_is_refused() {
    let runner = RoomRunner::new(call_checks());
    let (s, _dir) = room_server(runner.clone(), &[HUMAN]);
    let call = propose_this(&s, &runner, escrow_payment(None)).await;
    room_folded_call(
        &runner,
        "executable",
        "send_generic_public_transaction",
        escrow_args("AQIE"),
        &[AGENT_KEY, HUMAN],
        &[],
    );
    let r = call.await.unwrap();
    assert!(text(&r).contains("not this transfer"), "{}", text(&r));
    assert!(generic_sends(&runner).is_empty());
}

#[tokio::test(start_paused = true)]
async fn an_escrowed_payment_waits_no_longer_than_its_send_by_time() {
    let runner = RoomRunner::new(call_checks());
    let (s, _dir) = room_server(runner.clone(), &[HUMAN]);
    let send_by = now_ms() + 10 * 60 * 1000;
    let call = propose_this(&s, &runner, escrow_payment(Some(send_by))).await;
    let id = body(&call.await.unwrap())["request_id"]
        .as_str()
        .unwrap()
        .to_string();
    tokio::time::sleep(std::time::Duration::from_secs(11 * 60)).await;
    assert_eq!(status_of(&s, &id).await["state"], "expired");
    assert!(generic_sends(&runner).is_empty());
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
async fn a_room_short_of_trusted_members_is_refused_and_an_inactive_one_is_joined() {
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
    // The seat is the agent's own Muster: it makes its room active and goes on.
    assert_eq!(
        runner.muster_calls("coordinate_join")[0][4],
        format!("str:{TOPIC}")
    );
    assert!(
        !runner.muster_calls("coordinate_propose").is_empty(),
        "{}",
        text(&r)
    );
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

#[tokio::test(start_paused = true)]
async fn the_first_proposal_after_a_restart_declines_the_cards_left_open() {
    let runner = RoomRunner::new(wallet_open().to_vec());
    let mine = json!([{"who": format!("{AGENT_KEY}{X25519}"), "mine": true}]);
    let theirs = json!([{"who": format!("{HUMAN}{X25519}"), "mine": false}]);
    let ours = |id: &str, state: &str, proposed_by: &Value, reference: &str| {
        json!({"id": id, "state": state, "approvals": 1, "proposedBy": proposed_by,
               "effect": {"effect": "invoke", "ref": reference}})
    };
    // What an earlier run left: only its own still-open proposals are declined.
    runner.always(
        "coordinate_intents",
        json!([
            ours("0x01", "collecting", &mine, "logos-mcp/old-request"),
            ours("0x02", "executable", &mine, "logos-mcp/paid-request"),
            ours("0x03", "collecting", &theirs, "logos-mcp/spoofed"),
            ours("0x04", "collecting", &mine, "someone-else"),
            ours("0x05", "declined", &mine, "logos-mcp/declined"),
        ]),
    );
    runner.answer("coordinate_intents", json!([]));
    let (s, _dir) = room_server(runner.clone(), &[HUMAN]);
    let _call = propose(&s, &runner).await;
    let declined: Vec<String> = runner
        .muster_calls("coordinate_decline")
        .iter()
        .map(|c| c[4].clone())
        .collect();
    assert_eq!(declined, ["str:0x01"]);
    let calls = runner.calls.lock().unwrap().clone();
    let at = |m: &str| {
        calls
            .iter()
            .position(|c| c.get(3).map(String::as_str) == Some(m))
            .unwrap()
    };
    assert!(at("coordinate_decline") < at("coordinate_propose"));
}

#[tokio::test(start_paused = true)]
async fn a_proposal_whose_explanation_cant_be_posted_is_withdrawn_unapproved() {
    let runner = RoomRunner::new(call_checks());
    runner.always("coordinate_post_message", json!({"error": "not delivered"}));
    let (s, _dir) = room_server(runner.clone(), &[HUMAN]);
    let r = s
        .wallet_transfer(CancellationToken::new(), escrow_payment(None))
        .await
        .unwrap();
    assert!(is_error(&r));
    assert!(
        text(&r).contains("couldn't say in the room"),
        "{}",
        text(&r)
    );
    assert!(runner.muster_calls("coordinate_contribute").is_empty());
    assert_eq!(
        runner.muster_calls("coordinate_decline")[0][4],
        format!("str:{INTENT}")
    );
}

#[tokio::test(start_paused = true)]
async fn a_payment_too_close_to_its_deadline_is_refused_before_proposing() {
    let runner = RoomRunner::new(call_checks());
    let (s, _dir) = room_server(runner.clone(), &[HUMAN]);
    let r = s
        .wallet_transfer(
            CancellationToken::new(),
            escrow_payment(Some(now_ms() + 30_000)),
        )
        .await
        .unwrap();
    assert!(is_error(&r));
    assert!(
        text(&r).contains("too close to its deadline"),
        "{}",
        text(&r)
    );
    assert!(runner.muster_calls("coordinate_propose").is_empty());
}

#[tokio::test(start_paused = true)]
async fn an_approved_transfer_the_wallet_refuses_withdraws_its_proposal() {
    let mut replies = wallet_open().to_vec();
    replies.extend(wallet_open());
    replies.push(lez(
        "transfer_public",
        r#""{\"error\":\"insufficient funds\",\"success\":false}""#,
    ));
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
    assert!(text(&r).contains("not sent"), "{}", text(&r));
    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    assert_eq!(
        runner.muster_calls("coordinate_decline")[0][4],
        format!("str:{INTENT}")
    );
}

// ---- the approval policy --------------------------------------------------

const CHAIN_A: &str = "3e2acc3dacb83036f111df8fa65df19d897a7022aa42fbd51dc52d8f79117e09";
const CHAIN_B: &str = "84f956ec0d63f5f7dac2eb85171c0b64fa2191d16d96515ba6f16ec89c78c61e";
const CHAIN_TX: &str = "1f0f6eafe1e058d5d657cd26972de2a0ea3de547eae66b758f6531a516deae1c";

fn chain_reply(method: &str, value: Value) -> RawOutput {
    out(&json!({
        "method": method,
        "module": "blockchain_module",
        "result": { "error": null, "success": true, "value": value },
        "status": "ok",
    })
    .to_string())
}

/// What a chain transfer's checks read: the sender is the node's and holds 5000.
fn chain_checks() -> Vec<RawOutput> {
    vec![
        chain_reply(
            "wallet_get_known_addresses",
            json!(format!("[\"{CHAIN_A}\",\"{CHAIN_B}\"]")),
        ),
        chain_reply("wallet_get_balance", json!("5000")),
    ]
}

fn chain_args() -> Value {
    json!([CHAIN_A, [CHAIN_A], CHAIN_B, "1500", ""])
}

#[tokio::test(start_paused = true)]
async fn a_cli_chain_transfer_goes_to_the_room_and_sends_exactly_that_call() {
    let mut replies = chain_checks(); // before proposing
    replies.extend(chain_checks()); // again before sending
    replies.push(chain_reply("wallet_transfer_funds", json!(CHAIN_TX)));
    let runner = RoomRunner::new(replies);
    let (s, _dir) = room_server(runner.clone(), &[HUMAN]);
    let agent = s.clone();
    let call = tokio::spawn(async move {
        agent
            .chain_transfer(
                CancellationToken::new(),
                Parameters(ChainTransferParams {
                    from: CHAIN_A.into(),
                    to: CHAIN_B.into(),
                    amount_lepta: "1500".into(),
                }),
            )
            .await
            .unwrap()
    });
    for _ in 0..12_000 {
        if !runner.muster_calls("coordinate_contribute").is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let proposed = runner.muster_calls("coordinate_propose");
    let effect: Value = serde_json::from_str(proposed[0][4].strip_prefix("str:").unwrap()).unwrap();
    assert_eq!(effect["module"], "blockchain_module");
    assert_eq!(effect["method"], "wallet_transfer_funds");
    assert_eq!(effect["args"], chain_args());
    assert!(effect["note"].as_str().unwrap().contains("Logos chain"));
    room_folded_in(
        &runner,
        "executable",
        "blockchain_module",
        "wallet_transfer_funds",
        chain_args(),
        &[AGENT_KEY, HUMAN],
        &[],
    );
    let r = call.await.unwrap();
    assert_eq!(body(&r)["tx_hash"], CHAIN_TX, "{}", text(&r));
    let sent: Vec<Vec<String>> = runner
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c.get(3).map(String::as_str) == Some("wallet_transfer_funds"))
        .cloned()
        .collect();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0][4..],
        [
            format!("str:{CHAIN_A}"),
            format!("json:[\"{CHAIN_A}\"]"),
            format!("str:{CHAIN_B}"),
            "str:1500".into(),
            "str:".into(),
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn each_tier_routes_the_agents_transfers_in_basecamp() {
    let mut replies = wallet_open().to_vec(); // the small one, checked before asking
    replies.extend(wallet_open()); // the big one, checked before proposing
    let runner = RoomRunner::new(replies);
    let mut policy = cli_policy(&[HUMAN]);
    policy.agent.up_to = policy::Approver::Ask;
    let (s, _dir, _tx) = policy_server(runner.clone(), policy, true);
    let agent = s.clone();
    let small = tokio::spawn(async move {
        agent
            .wallet_transfer(
                CancellationToken::new(),
                transfer(AGENT_FROM, RLN_PAYER, "1000"),
            )
            .await
            .unwrap()
    });
    for _ in 0..200 {
        if !s.approvals()["waiting"].as_array().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(
        s.approvals()["waiting"][0]["amount_lepta"],
        "1000",
        "the user is asked"
    );
    assert!(runner.muster_calls("coordinate_propose").is_empty());
    let _big = propose(&s, &runner).await;
    assert_eq!(
        runner.muster_calls("coordinate_propose").len(),
        1,
        "the room is asked"
    );
    drop(small);
}

#[tokio::test(start_paused = true)]
async fn changing_the_room_cancels_a_waiting_request_and_closes_its_card() {
    let runner = RoomRunner::new(wallet_open().to_vec());
    let (s, _dir, policy) = policy_server(runner.clone(), cli_policy(&[HUMAN]), false);
    let id = body(&propose(&s, &runner).await.await.unwrap())["request_id"]
        .as_str()
        .unwrap()
        .to_string();
    policy.send_modify(|p| {
        p.room.as_mut().unwrap().topic = "/muster/1/another-room/proto".into();
    });
    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    assert_eq!(status_of(&s, &id).await["state"], "cancelled");
    assert_eq!(
        runner.muster_calls("coordinate_decline")[0][4],
        format!("str:{INTENT}")
    );
    assert_eq!(runner.sends(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_sent_room_request_reports_the_approvals_that_sent_it() {
    let mut replies = wallet_open().to_vec(); // checks before proposing
    replies.extend(wallet_open()); // checks again before sending
    replies.push(sent("cd34"));
    let runner = RoomRunner::new(replies);
    let (s, _dir, _policy) = policy_server(runner.clone(), cli_policy(&[HUMAN]), false);
    let id = body(&propose(&s, &runner).await.await.unwrap())["request_id"]
        .as_str()
        .unwrap()
        .to_string();
    room_folded(
        &runner,
        "executable",
        the_call(OVER),
        &[AGENT_KEY, HUMAN],
        &[],
    );
    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    let status = status_of(&s, &id).await;
    assert_eq!(status["state"], "sent", "{status}");
    assert_eq!(status["room"]["approvals"], 1, "{status}");
}

#[tokio::test(start_paused = true)]
async fn settings_changed_while_an_approved_transfer_waits_for_the_wallet_stop_it() {
    let mut replies = wallet_open().to_vec(); // checks before proposing
    replies.extend(wallet_open()); // checks again before sending
    replies.push(sent("cd34"));
    let runner = RoomRunner::new(replies);
    let other = "7a1c000000000000000000000000000000000000000000000000000000e0f2aa";
    let (s, _dir, policy) = policy_server(runner.clone(), cli_policy(&[HUMAN]), false);
    let id = body(&propose(&s, &runner).await.await.unwrap())["request_id"]
        .as_str()
        .unwrap()
        .to_string();
    // Another transfer holds the wallet while the room approves this one.
    let wallet = s.agent_accounts.lock().await;
    room_folded(
        &runner,
        "executable",
        the_call(OVER),
        &[AGENT_KEY, HUMAN],
        &[],
    );
    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    policy.send_modify(|p| p.room.as_mut().unwrap().approvers = vec![format!("ed:{other}")]);
    drop(wallet);
    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    let status = status_of(&s, &id).await;
    assert_eq!(status["state"], "failed", "{status}");
    assert_eq!(runner.sends(), 0);
    assert_eq!(
        runner.muster_calls("coordinate_decline")[0][4],
        format!("str:{INTENT}")
    );
}

#[tokio::test(start_paused = true)]
async fn an_unticked_approver_stops_counting_for_a_waiting_request() {
    let runner = RoomRunner::new(wallet_open().to_vec());
    let other = "7a1c000000000000000000000000000000000000000000000000000000e0f2aa";
    let (s, _dir, policy) = policy_server(runner.clone(), cli_policy(&[HUMAN, other]), false);
    let id = body(&propose(&s, &runner).await.await.unwrap())["request_id"]
        .as_str()
        .unwrap()
        .to_string();
    policy.send_modify(|p| p.room.as_mut().unwrap().approvers = vec![format!("ed:{other}")]);
    room_folded(
        &runner,
        "executable",
        the_call(OVER),
        &[AGENT_KEY, HUMAN],
        &[],
    );
    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    assert_eq!(status_of(&s, &id).await["state"], "waiting");
    assert_eq!(runner.sends(), 0);
}

#[tokio::test(start_paused = true)]
async fn the_agents_own_key_made_an_approver_mid_request_ends_it_unsent() {
    let runner = RoomRunner::new(wallet_open().to_vec());
    let (s, _dir, policy) = policy_server(runner.clone(), cli_policy(&[HUMAN]), false);
    let call = propose(&s, &runner).await;
    policy.send_modify(|p| {
        p.room
            .as_mut()
            .unwrap()
            .approvers
            .push(format!("ed:{AGENT_KEY}"))
    });
    room_folded(
        &runner,
        "executable",
        the_call(OVER),
        &[AGENT_KEY, HUMAN],
        &[],
    );
    let r = call.await.unwrap();
    assert!(text(&r).contains("own Muster key"), "{}", text(&r));
    assert_eq!(runner.sends(), 0);
}

#[tokio::test]
async fn transfers_may_wait_whenever_a_tier_asks_or_the_room_approves() {
    let runner = RoomRunner::new(vec![]);
    let (s, _dir, policy) = policy_server(runner, ApprovalPolicy::default(), true);
    assert!(!s.asks_before_transfer());
    policy.send_modify(|p| *p = cli_policy(&[HUMAN]));
    assert!(s.asks_before_transfer());
}

#[tokio::test(start_paused = true)]
async fn basecamp_without_the_agent_seat_never_proposes_through_the_users_muster() {
    let runner = RoomRunner::new(wallet_open().to_vec());
    let (s, _dir, _policy) = policy_server(runner.clone(), cli_policy(&[HUMAN]), true);
    // The same server without its seat: Basecamp's own node is the user's Muster.
    let s = LogosServer { muster: None, ..s };
    let r = s
        .wallet_transfer(
            CancellationToken::new(),
            transfer(AGENT_FROM, RLN_PAYER, OVER),
        )
        .await
        .unwrap();
    assert!(text(&r).contains("agent's own seat"), "{}", text(&r));
    for method in ["coordinate_propose", "coordinate_contribute", "identity"] {
        assert!(runner.muster_calls(method).is_empty(), "{method}");
    }
}

#[tokio::test(start_paused = true)]
async fn the_app_lists_room_requests_and_can_cancel_one() {
    let runner = RoomRunner::new(wallet_open().to_vec());
    let (s, _dir) = room_server(runner.clone(), &[HUMAN]);
    let id = body(&propose(&s, &runner).await.await.unwrap())["request_id"]
        .as_str()
        .unwrap()
        .to_string();
    let listed = s.approvals();
    assert!(listed["waiting"].as_array().unwrap().is_empty());
    assert_eq!(listed["room"][0]["id"], id);
    assert_eq!(listed["room"][0]["room"]["intent_id"], INTENT);
    s.cancel_room_request(&id).unwrap();
    assert!(s.approvals()["room"].as_array().unwrap().is_empty());
    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    assert_eq!(
        runner.muster_calls("coordinate_decline")[0][4],
        format!("str:{INTENT}")
    );
    assert_eq!(runner.sends(), 0);
}

#[tokio::test(start_paused = true)]
async fn the_app_cannot_cancel_a_request_the_user_answers() {
    let runner = RoomRunner::new(wallet_open().to_vec());
    let mut policy = cli_policy(&[HUMAN]);
    policy.agent.up_to = policy::Approver::Ask;
    let (s, _dir, _tx) = policy_server(runner.clone(), policy, true);
    let agent = s.clone();
    let _small = tokio::spawn(async move {
        agent
            .wallet_transfer(
                CancellationToken::new(),
                transfer(AGENT_FROM, RLN_PAYER, "1000"),
            )
            .await
            .unwrap()
    });
    for _ in 0..200 {
        if !s.approvals()["waiting"].as_array().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let id = s.approvals()["waiting"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(s.cancel_room_request(&id).is_err());
}

#[tokio::test(start_paused = true)]
async fn a_waiting_request_whose_tier_no_longer_asks_the_room_is_cancelled() {
    let runner = RoomRunner::new(wallet_open().to_vec());
    let (s, _dir, policy) = policy_server(runner.clone(), cli_policy(&[HUMAN]), false);
    let id = body(&propose(&s, &runner).await.await.unwrap())["request_id"]
        .as_str()
        .unwrap()
        .to_string();
    // The room stays set, but this amount now goes to the user instead.
    policy.send_modify(|p| p.agent.above = policy::Approver::Ask);
    room_folded(
        &runner,
        "executable",
        the_call(OVER),
        &[AGENT_KEY, HUMAN],
        &[],
    );
    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    assert_eq!(status_of(&s, &id).await["state"], "cancelled");
    assert_eq!(runner.sends(), 0);
    assert_eq!(
        runner.muster_calls("coordinate_decline")[0][4],
        format!("str:{INTENT}")
    );
}

#[tokio::test(start_paused = true)]
async fn the_user_cannot_approve_a_request_the_room_now_decides() {
    let runner = RoomRunner::new(wallet_open().to_vec());
    let mut asking = cli_policy(&[HUMAN]);
    asking.agent.up_to = policy::Approver::Ask;
    let (s, _dir, policy) = policy_server(runner.clone(), asking, true);
    let agent = s.clone();
    let _small = tokio::spawn(async move {
        agent
            .wallet_transfer(
                CancellationToken::new(),
                transfer(AGENT_FROM, RLN_PAYER, "1000"),
            )
            .await
            .unwrap()
    });
    for _ in 0..200 {
        if !s.approvals()["waiting"].as_array().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let id = s.approvals()["waiting"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    policy.send_modify(|p| p.agent.up_to = policy::Approver::Room);
    let e = s.approve_transfer(&id).await.unwrap_err();
    assert!(e.contains("room approves"), "{e}");
    assert_eq!(runner.sends(), 0);
}

#[tokio::test(start_paused = true)]
async fn the_seat_lists_a_rooms_members_with_the_agent_marked() {
    let runner = RoomRunner::new(vec![]);
    runner.always(
        "coordinate_conversations",
        json!([{"topic": TOPIC, "active": false}]),
    );
    let (s, _dir, _policy) = policy_server(runner.clone(), cli_policy(&[HUMAN]), true);
    let members = s.room_members(TOPIC).await.unwrap();
    assert_eq!(
        runner.muster_calls("coordinate_join")[0][4],
        format!("str:{TOPIC}")
    );
    assert_eq!(members[0]["key"], format!("ed:{HUMAN}"));
    assert_eq!(members[0]["agent"], false);
    assert_eq!(members[1]["key"], format!("ed:{AGENT_KEY}"));
    assert_eq!(members[1]["agent"], true);
}

#[tokio::test(start_paused = true)]
async fn a_node_wallet_set_to_send_sends_without_asking() {
    let mut replies = chain_checks();
    replies.extend(chain_checks());
    replies.push(chain_reply("wallet_transfer_funds", json!(CHAIN_TX)));
    let runner = RoomRunner::new(replies);
    let mut sending = cli_policy(&[HUMAN]);
    sending.node.up_to = policy::Approver::Send;
    sending.node.above = policy::Approver::Send;
    let (s, _dir, _policy) = policy_server(runner.clone(), sending, true);
    let r = s
        .chain_transfer(
            CancellationToken::new(),
            Parameters(ChainTransferParams {
                from: CHAIN_A.into(),
                to: CHAIN_B.into(),
                amount_lepta: "1500".into(),
            }),
        )
        .await
        .unwrap();
    assert_eq!(body(&r)["tx_hash"], CHAIN_TX, "{}", text(&r));
    assert!(runner.muster_calls("coordinate_propose").is_empty());
    assert!(s.approvals()["waiting"].as_array().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_direct_chain_transfer_cancelled_before_sending_is_never_sent() {
    let runner = RoomRunner::new(chain_checks().into_iter().chain(chain_checks()).collect());
    let mut sending = cli_policy(&[HUMAN]);
    sending.node.up_to = policy::Approver::Send;
    sending.node.above = policy::Approver::Send;
    let (s, _dir, _policy) = policy_server(runner.clone(), sending, true);
    let ct = CancellationToken::new();
    ct.cancel();
    let r = s
        .chain_transfer(
            ct,
            Parameters(ChainTransferParams {
                from: CHAIN_A.into(),
                to: CHAIN_B.into(),
                amount_lepta: "1500".into(),
            }),
        )
        .await
        .unwrap();
    assert!(text(&r).contains("cancelled"), "{}", text(&r));
    assert!(
        !runner
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.get(3).map(String::as_str) == Some("wallet_transfer_funds"))
    );
}

#[tokio::test(start_paused = true)]
async fn a_restarted_seat_joins_its_room_before_waiting_for_delivery() {
    // A seat starts its delivery node only once a room is active.
    let runner = RoomRunner::new(wallet_open().to_vec());
    runner.always(
        "coordinate_conversations",
        json!([{"topic": TOPIC, "active": false}]),
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
    assert!(at("coordinate_join") < at("connectivity"), "{calls:?}");
}
