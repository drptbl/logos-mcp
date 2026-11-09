//! The service_* tools, over a fake node that answers by module and method.

use std::collections::{BTreeMap, VecDeque};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use super::*;
use crate::logosctl::{RawOutput, RunFuture, Runner};
use crate::server::jobs::*;
use crate::services::{self as protocol, Body, Payload};

/// Answers by module and method (`lez_core.list_accounts`) or by command
/// (`status`, `module ls --loaded`), so a long flow needs no exact order. The last
/// reply queued for a key repeats.
#[derive(Default)]
struct Routed {
    replies: Mutex<BTreeMap<String, VecDeque<RawOutput>>>,
    calls: Mutex<Vec<Vec<String>>>,
}

fn key(argv: &[String]) -> String {
    if argv.get(1).map(String::as_str) == Some("call") {
        format!("{}.{}", argv[2], argv[3])
    } else {
        argv[1..].join(" ")
    }
}

impl Routed {
    fn with(entries: Vec<(&str, RawOutput)>) -> Arc<Self> {
        let r = Self::default();
        for (k, v) in entries {
            r.set(k, v);
        }
        Arc::new(r)
    }

    /// Queue `reply` for `key`, after any already queued.
    fn set(&self, key: &str, reply: RawOutput) {
        self.replies
            .lock()
            .unwrap()
            .entry(key.to_string())
            .or_default()
            .push_back(reply);
    }

    /// Replace everything queued for `key` with `reply`.
    fn reset(&self, key: &str, reply: RawOutput) {
        self.replies
            .lock()
            .unwrap()
            .insert(key.to_string(), VecDeque::from([reply]));
    }

    /// The arguments of each call made to `key`.
    fn calls_to(&self, key_name: &str) -> Vec<Vec<String>> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|argv| key(argv) == key_name)
            .map(|argv| argv[4..].to_vec())
            .collect()
    }
}

impl Runner for Routed {
    fn run(&self, args: Vec<String>) -> RunFuture<'_> {
        let k = key(&args);
        self.calls.lock().unwrap().push(args);
        let mut replies = self.replies.lock().unwrap();
        let next = match replies.get_mut(&k) {
            Some(queue) if queue.len() > 1 => queue.pop_front().unwrap(),
            Some(queue) if !queue.is_empty() => queue[0].clone(),
            _ => RawOutput {
                code: 1,
                stdout: String::new(),
                stderr: format!("no canned reply for {k}"),
            },
        };
        Box::pin(async move { Ok(next) })
    }
}

const SVC: &str = "demo/summarize/v1";
const JOB: &str = "0123456789abcdef0123456789abcdef";
const CONVO: &str = "17de45e2ff7f5cebdce3617c5a0d4f9f";
const NEW_ID: &str = "8126abb4eb9f0a26919bf51a57244a6b7d3a39811e6393c1885f9e07aa32fec1";
const NEW_ADDRESS: &str = "Public/9h9o2fwbcu7Mbuq8hzVf1UVDpMQpCdCr5ZnNVo3vTsjN";
const FAR: u64 = 4_000_000_000_000;

/// A node with chat online, discovery up, RLN off, on logos.dev, and an agent
/// wallet this server opens. Tests add or replace what they need.
fn node() -> Arc<Routed> {
    Routed::with(vec![
        ("module ls --loaded", out(CHAT_LOADED)),
        (
            "chat_module.get_address",
            chat("get_address", &format!(r#""{SELF_ADDR}""#)),
        ),
        ("chat_module.status", chat("status", CHAT_ONLINE)),
        ("chat_module.send_message", chat_ok("send_message", "null")),
        (
            "chat_module.create_conversation",
            chat_ok("create_conversation", &format!(r#""{CONVO}""#)),
        ),
        (
            "chat_module.list_conversations",
            chat(
                "list_conversations",
                &format!(
                    r#"[{{"convo_id":"{CONVO}","history_only":false,"kind":"direct","last_activity_ms":1,"message_count":1}}]"#
                ),
            ),
        ),
        ("delivery_module.rlnState", out(RLN_DISABLED)),
        ("delivery_module.getNodeInfo", node_enr(ENR_LOGOS_DEV)),
        ("libp2p_module.peerInfo", p2p("peerInfo", PEER_INFO)),
        ("libp2p_module.start", p2p("start", "null")),
        ("libp2p_module.discoStart", p2p("discoStart", "null")),
        ("libp2p_module.createXpr", p2p("createXpr", r#""WFBS""#)),
        (
            "libp2p_module.discoStopAdvertising",
            p2p("discoStopAdvertising", "null"),
        ),
        (
            "libp2p_module.discoStartAdvertising",
            p2p("discoStartAdvertising", "null"),
        ),
        (
            "libp2p_module.discoRegisterInterest",
            p2p("discoRegisterInterest", "null"),
        ),
        (
            "lez_core.get_sequencer_addr",
            lez("get_sequencer_addr", r#""""#),
        ),
        (
            "lez_core.get_sequencer_addr",
            lez("get_sequencer_addr", r#""https://testnet.lez.logos.co/""#),
        ),
        ("status", daemon_status(80675, 300)),
        ("lez_core.open", lez("open", "0")),
        (
            "lez_core.list_accounts",
            lez("list_accounts", AGENT_ACCOUNTS),
        ),
        (
            "lez_core.create_account_public",
            lez("create_account_public", &format!(r#""{NEW_ID}""#)),
        ),
        ("lez_core.save", lez("save", "0")),
        ("lez_core.get_balance", lez("get_balance", r#""0""#)),
        (
            "lez_core.transfer_public",
            lez(
                "transfer_public",
                r#""{\"error\":\"\",\"success\":true,\"tx_hash\":\"aa11\"}""#,
            ),
        ),
    ])
}

/// A server with its own agent wallet (in a temp dir) and discovery peers.
fn services_server(fake: Arc<Routed>) -> (LogosServer, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let s = services_server_in(fake, dir.path());
    (s, dir)
}

fn proto(id: u8, body: Body) -> String {
    let mut m = protocol::Message::new(JOB, body);
    m.id = format!("{id:032x}");
    protocol::encode(&m)
}

fn request_text() -> String {
    proto(
        1,
        Body::Request {
            service: SVC.into(),
            input: Payload::Text("summarise this".into()),
        },
    )
}

fn quote_text(network: &str, expires_at: u64) -> String {
    proto(
        2,
        Body::Quote {
            price_lepta: "1000000".into(),
            pay_to: NEW_ADDRESS.into(),
            network: network.into(),
            expires_at,
            deliver_within_s: 600,
            escrow: None,
        },
    )
}

/// The conversation as get_messages returns it: (sent by this side, text).
fn history(messages: &[(bool, &str)]) -> RawOutput {
    let list: Vec<Value> = messages
        .iter()
        .enumerate()
        .map(|(i, (mine, text))| {
            let mut m = json!({ "content": text, "from_self": mine, "timestamp_ms": i + 1 });
            if !mine {
                m["sender"] = json!(PEER);
            }
            m
        })
        .collect();
    chat("get_messages", &Value::Array(list).to_string())
}

/// The protocol message in a send_message call.
fn sent(call: &[String]) -> protocol::Message {
    protocol::parse(call[1].strip_prefix("str:").unwrap()).unwrap()
}

fn offer() -> Parameters<OfferParams> {
    Parameters(OfferParams {
        service_id: SVC.into(),
        tag: "v1".into(),
        price_lepta: "1000000".into(),
        deliver_within_s: None,
        escrow: None,
        escrow_account: None,
    })
}

fn job(id: &str) -> Parameters<JobParams> {
    Parameters(JobParams { job: id.into() })
}

#[tokio::test]
async fn offering_advertises_the_service_with_this_agents_chat_address() {
    let fake = node();
    let (s, _dir) = services_server(fake.clone());
    let r = s.service_offer(offer()).await.unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    assert_eq!(body(&r)["chat_address"], SELF_ADDR);

    let xpr = &fake.calls_to("libp2p_module.createXpr")[0];
    let entries: Value = serde_json::from_str(xpr[1].strip_prefix("json:").unwrap()).unwrap();
    let address: Vec<u8> = (0..32)
        .map(|i| u8::from_str_radix(&SELF_ADDR[2 * i..2 * i + 2], 16).unwrap())
        .collect();
    assert_eq!(
        entries[protocol::CHAT_ENTRY]["_bytes"],
        URL_SAFE_NO_PAD.encode(address)
    );
    assert_eq!(entries[SVC]["_bytes"], URL_SAFE_NO_PAD.encode("v1"));
    assert_eq!(
        fake.calls_to("libp2p_module.discoStartAdvertising")[0],
        [format!("str:{SVC}"), "str:v1".into(), "str:WFBS".into()]
    );
    // libp2p never advertises a service whose first call was a stop (seen live
    // on libp2p_module 1.1.0), so only a replacement stops the old one first.
    assert!(
        fake.calls_to("libp2p_module.discoStopAdvertising")
            .is_empty()
    );
    s.service_offer(offer()).await.unwrap();
    assert_eq!(fake.calls_to("libp2p_module.discoStopAdvertising").len(), 1);
}

#[tokio::test]
async fn offering_refuses_bad_terms_without_calls() {
    for p in [
        OfferParams {
            service_id: SVC.into(),
            tag: "x".repeat(34),
            price_lepta: "1".into(),
            deliver_within_s: None,
            escrow: None,
            escrow_account: None,
        },
        OfferParams {
            service_id: SVC.into(),
            tag: "v1".into(),
            price_lepta: "lots".into(),
            deliver_within_s: None,
            escrow: None,
            escrow_account: None,
        },
        OfferParams {
            service_id: "has space".into(),
            tag: "v1".into(),
            price_lepta: "1".into(),
            deliver_within_s: None,
            escrow: None,
            escrow_account: None,
        },
    ] {
        let fake = node();
        let (s, _dir) = services_server(fake.clone());
        assert!(is_error(&s.service_offer(Parameters(p)).await.unwrap()));
        assert!(fake.calls.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn finding_lists_providers_that_carry_a_chat_address() {
    let b64 = |bytes: &[u8]| STANDARD.encode(bytes);
    let address: Vec<u8> = (0..32)
        .map(|i| u8::from_str_radix(&PEER[2 * i..2 * i + 2], 16).unwrap())
        .collect();
    let records = json!([
        { "peerId": "P1", "services": [
            { "id": SVC, "data": b64(b"v1") },
            { "id": protocol::CHAT_ENTRY, "data": b64(&address) }
        ]},
        { "peerId": "P2", "services": [{ "id": SVC, "data": b64(b"v1") }] }
    ]);
    let fake = node();
    fake.set(
        "libp2p_module.discoLookup",
        p2p("discoLookup", &records.to_string()),
    );
    let (s, _dir) = services_server(fake);
    let b = body(
        &s.service_find(Parameters(ServiceParams {
            service_id: SVC.into(),
        }))
        .await
        .unwrap(),
    );
    let providers = b["providers"].as_array().unwrap();
    assert_eq!(providers.len(), 1, "{b}");
    assert_eq!(providers[0]["chat_address"], PEER);
    assert_eq!(providers[0]["tag"], "v1");
}

#[tokio::test(start_paused = true)]
async fn a_request_goes_to_the_provider_over_one_conversation() {
    let fake = node();
    let (s, _dir) = services_server(fake.clone());
    let ask = || {
        Parameters(RequestParams {
            chat_address: PEER.into(),
            service_id: SVC.into(),
            input_text: Some("summarise this".into()),
            input_cid: None,
        })
    };
    let r = s.service_request(ask()).await.unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    let b = body(&r);
    assert_eq!(b["conversation_id"], CONVO);
    let m = sent(&fake.calls_to("chat_module.send_message")[0]);
    assert_eq!(m.job, b["job"]);
    assert_eq!(
        m.body,
        Body::Request {
            service: SVC.into(),
            input: Payload::Text("summarise this".into()),
        }
    );
    s.service_request(ask()).await.unwrap();
    assert_eq!(fake.calls_to("chat_module.create_conversation").len(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_request_starts_chat_before_opening_a_conversation() {
    // A fresh server may find chat not started yet.
    let fake = node();
    fake.reset("chat_module.get_address", chat("get_address", r#""""#));
    fake.set(
        "chat_module.get_address",
        chat("get_address", &format!(r#""{SELF_ADDR}""#)),
    );
    fake.set("chat_module.init", chat_ok("init", "null"));
    fake.reset(
        "module ls --loaded",
        out(r#"[{"name":"chat_module","status":"loaded"},{"name":"delivery_module","status":"loaded"}]"#),
    );
    fake.set(
        "delivery_module.getConnectionStatus",
        out(r#"{"method":"getConnectionStatus","module":"delivery_module","result":{"error":null,"success":true,"value":"Connected"},"status":"ok"}"#),
    );
    let (s, _dir) = services_server(fake.clone());
    let r = s
        .service_request(Parameters(RequestParams {
            chat_address: PEER.into(),
            service_id: SVC.into(),
            input_text: Some("hi".into()),
            input_cid: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    let calls = fake.calls.lock().unwrap();
    let first = |k: &str| calls.iter().position(|argv| key(argv) == k);
    assert!(
        first("chat_module.init").unwrap() < first("chat_module.create_conversation").unwrap(),
        "{calls:?}"
    );
}

#[tokio::test]
async fn a_request_needs_exactly_one_input() {
    let fake = node();
    let (s, _dir) = services_server(fake.clone());
    for (text_in, cid) in [(None, None), (Some("a"), Some("zDv"))] {
        let r = s
            .service_request(Parameters(RequestParams {
                chat_address: PEER.into(),
                service_id: SVC.into(),
                input_text: text_in.map(Into::into),
                input_cid: cid.map(Into::into),
            }))
            .await
            .unwrap();
        assert!(is_error(&r));
    }
    assert!(fake.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn quoting_creates_a_fresh_account_for_the_job() {
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        history(&[(false, &request_text())]),
    );
    let (s, dir) = services_server(fake.clone());
    s.service_offer(offer()).await.unwrap();
    let r = s.service_quote(job(JOB)).await.unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    assert_eq!(fake.calls_to("lez_core.create_account_public").len(), 1);
    let m = sent(&fake.calls_to("chat_module.send_message")[0]);
    let Body::Quote {
        price_lepta,
        pay_to,
        network,
        ..
    } = m.body
    else {
        panic!("not a quote: {m:?}");
    };
    assert_eq!(
        (price_lepta.as_str(), pay_to.as_str(), network.as_str()),
        ("1000000", NEW_ADDRESS, "logos.dev")
    );
    // The account is kept as a job account, which wallet_accounts groups.
    let saved = std::fs::read_to_string(dir.path().join("job-accounts.json")).unwrap();
    assert!(saved.contains(NEW_ID) && saved.contains(JOB), "{saved}");
    fake.reset(
        "lez_core.list_accounts",
        lez(
            "list_accounts",
            &format!(r#"[{{"account_id":"{NEW_ID}","is_public":true}}]"#),
        ),
    );
    let accounts = body(&s.wallet_accounts().await.unwrap());
    let account = &accounts["accounts"][0];
    assert_eq!(account["job"]["job"], JOB, "{accounts}");
    assert_eq!(account["job"]["service"], SVC);
}

#[tokio::test]
async fn only_an_offered_service_is_quoted() {
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        history(&[(false, &request_text())]),
    );
    let (s, _dir) = services_server(fake.clone());
    let r = s.service_quote(job(JOB)).await.unwrap();
    assert!(is_error(&r));
    assert!(text(&r).contains("not offering"), "{}", text(&r));
    assert!(fake.calls_to("lez_core.create_account_public").is_empty());
}

#[tokio::test]
async fn rejecting_before_a_quote_and_failing_after_it() {
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        history(&[(false, &request_text())]),
    );
    let (s, _dir) = services_server(fake.clone());
    let reason = |why: &str| {
        Parameters(RejectParams {
            job: JOB.into(),
            reason: why.into(),
        })
    };
    s.service_reject(reason("busy")).await.unwrap();
    assert!(matches!(
        sent(&fake.calls_to("chat_module.send_message")[0]).body,
        Body::Reject { .. }
    ));

    let fake = node();
    fake.set(
        "chat_module.get_messages",
        history(&[
            (false, &request_text()),
            (true, &quote_text("logos.dev", FAR)),
        ]),
    );
    let (s, _dir) = services_server(fake.clone());
    s.service_reject(reason("broke")).await.unwrap();
    assert!(matches!(
        sent(&fake.calls_to("chat_module.send_message")[0]).body,
        Body::Failed { .. }
    ));
}

fn deliver_done() -> Parameters<DeliverParams> {
    Parameters(DeliverParams {
        job: JOB.into(),
        output_text: Some("done".into()),
        output_cid: None,
    })
}

#[tokio::test]
async fn delivery_waits_for_the_payment_to_land() {
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        history(&[
            (false, &request_text()),
            (true, &quote_text("logos.dev", FAR)),
        ]),
    );
    let (s, _dir) = services_server(fake.clone());
    let r = s.service_deliver(deliver_done()).await.unwrap();
    assert!(is_error(&r));
    assert!(text(&r).contains("not paid"), "{}", text(&r));
    assert!(fake.calls_to("chat_module.send_message").is_empty());

    fake.reset("lez_core.get_balance", lez("get_balance", r#""1000000""#));
    let r = s.service_deliver(deliver_done()).await.unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    assert_eq!(
        sent(&fake.calls_to("chat_module.send_message")[0]).body,
        Body::Result {
            output: Payload::Text("done".into())
        }
    );
}

fn pay() -> Parameters<PayParams> {
    Parameters(PayParams {
        job: JOB.into(),
        from: AGENT_FROM.into(),
    })
}

#[tokio::test]
async fn paying_sends_the_price_and_names_the_quote() {
    let q = quote_text("logos.dev", FAR);
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        history(&[(true, &request_text()), (false, &q)]),
    );
    let (s, _dir) = services_server(fake.clone());
    let r = s
        .service_pay(CancellationToken::new(), pay())
        .await
        .unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    let transfer = &fake.calls_to("lez_core.transfer_public")[0];
    assert_eq!(
        transfer[1],
        format!("str:{NEW_ID}"),
        "pays the quoted account"
    );
    assert_eq!(
        sent(&fake.calls_to("chat_module.send_message")[0]).body,
        Body::Paid {
            quote: protocol::quote_hash(&q),
            tx_hash: "aa11".into(),
            escrow_buyer: None,
        }
    );
}

#[tokio::test]
async fn paying_refuses_an_expired_or_foreign_quote() {
    for q in [quote_text("logos.dev", 1), quote_text("logos.test", FAR)] {
        let fake = node();
        fake.set(
            "chat_module.get_messages",
            history(&[(true, &request_text()), (false, &q)]),
        );
        let (s, _dir) = services_server(fake.clone());
        let r = s
            .service_pay(CancellationToken::new(), pay())
            .await
            .unwrap();
        assert!(is_error(&r), "{}", text(&r));
        assert!(fake.calls_to("lez_core.transfer_public").is_empty());
    }
}

#[tokio::test]
async fn a_quote_already_paid_on_chain_is_not_paid_twice() {
    let q = quote_text("logos.dev", FAR);
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        history(&[(true, &request_text()), (false, &q)]),
    );
    fake.reset("lez_core.get_balance", lez("get_balance", r#""1000000""#));
    let (s, _dir) = services_server(fake.clone());
    let r = s
        .service_pay(CancellationToken::new(), pay())
        .await
        .unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    assert!(fake.calls_to("lez_core.transfer_public").is_empty());
    assert!(matches!(
        sent(&fake.calls_to("chat_module.send_message")[0]).body,
        Body::Paid { .. }
    ));
}

#[tokio::test(start_paused = true)]
async fn a_payment_waiting_for_approval_is_not_asked_for_twice() {
    let q = quote_text("logos.dev", FAR);
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        history(&[(true, &request_text()), (false, &q)]),
    );
    let (s, _dir) = services_server(fake.clone());
    let (groups, rx) = tokio::sync::watch::channel(ToolGroups::default());
    groups.send_modify(|g| g.set("ask_before_transfer", true).unwrap());
    let s = s.with_tool_groups(rx);
    let b = body(
        &s.service_pay(CancellationToken::new(), pay())
            .await
            .unwrap(),
    );
    assert_eq!(b["status"], "awaiting_approval", "{b}");
    let b = body(
        &s.service_pay(CancellationToken::new(), pay())
            .await
            .unwrap(),
    );
    assert_eq!(b["status"], "awaiting_approval", "{b}");
    assert_eq!(s.approvals()["waiting"].as_array().unwrap().len(), 1);
    assert!(fake.calls_to("chat_module.send_message").is_empty());
}

#[tokio::test]
async fn cancelling_is_for_the_buyer_before_paying() {
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        history(&[
            (true, &request_text()),
            (false, &quote_text("logos.dev", FAR)),
        ]),
    );
    let (s, _dir) = services_server(fake.clone());
    let r = s
        .service_cancel(Parameters(CancelParams {
            job: JOB.into(),
            reason: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    assert!(matches!(
        sent(&fake.calls_to("chat_module.send_message")[0]).body,
        Body::Cancel { .. }
    ));
}

#[tokio::test]
async fn jobs_show_payment_on_chain_and_expiry() {
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        history(&[
            (false, &request_text()),
            (true, &quote_text("logos.dev", 1)),
        ]),
    );
    let (s, _dir) = services_server(fake.clone());
    let b = body(&s.service_jobs().await.unwrap());
    assert_eq!(b["jobs"][0]["state"], "expired", "{b}");
    assert_eq!(b["jobs"][0]["role"], "provider");

    // Paid in time: honoured though the quote has since expired.
    fake.reset("lez_core.get_balance", lez("get_balance", r#""1000000""#));
    let b = body(&s.service_jobs().await.unwrap());
    assert_eq!(b["jobs"][0]["state"], "paid", "{b}");
    assert!(
        b["jobs"][0]["next"]
            .as_str()
            .unwrap()
            .contains("service_deliver"),
        "{b}"
    );
}

#[tokio::test]
async fn services_follow_the_switches() {
    let fake = node();
    let (s, _dir) = services_server(fake.clone());
    let (groups, rx) = tokio::sync::watch::channel(ToolGroups::default());
    let client = connected(s.with_tool_groups(rx)).await;
    let names = |list: Vec<rmcp::model::Tool>| -> Vec<String> {
        list.into_iter().map(|t| t.name.to_string()).collect()
    };
    let listed = names(client.list_all_tools().await.unwrap());
    assert!(listed.iter().any(|n| n == "service_jobs"), "{listed:?}");

    groups.send_modify(|g| g.set("services", false).unwrap());
    let listed = names(client.list_all_tools().await.unwrap());
    assert!(
        !listed.iter().any(|n| n.starts_with("service_")),
        "{listed:?}"
    );

    // With chat off, services can't talk to anyone.
    groups.send_modify(|g| {
        g.set("services", true).unwrap();
        g.set("chat", false).unwrap();
    });
    let r = client
        .call_tool(rmcp::model::CallToolRequestParams::new("service_jobs"))
        .await
        .unwrap();
    assert_eq!(r.is_error, Some(true));
    assert!(text(&r).contains("Chat"), "{}", text(&r));
}

#[tokio::test]
async fn a_payment_whose_message_failed_is_not_sent_twice() {
    let q = quote_text("logos.dev", FAR);
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        history(&[(true, &request_text()), (false, &q)]),
    );
    fake.reset(
        "chat_module.send_message",
        chat_refused("send_message", "delivery hiccup"),
    );
    fake.set("chat_module.send_message", chat_ok("send_message", "null"));
    let (s, _dir) = services_server(fake.clone());
    let r = s
        .service_pay(CancellationToken::new(), pay())
        .await
        .unwrap();
    assert!(is_error(&r), "{}", text(&r));
    // The transfer went out but the provider wasn't told: telling it is all that's left.
    let r = s
        .service_pay(CancellationToken::new(), pay())
        .await
        .unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    assert_eq!(fake.calls_to("lez_core.transfer_public").len(), 1);
    assert_eq!(
        sent(&fake.calls_to("chat_module.send_message")[1]).body,
        Body::Paid {
            quote: protocol::quote_hash(&q),
            tx_hash: "aa11".into(),
            escrow_buyer: None,
        }
    );
}

#[tokio::test]
async fn a_payment_that_may_have_gone_out_is_not_retried() {
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        history(&[
            (true, &request_text()),
            (false, &quote_text("logos.dev", FAR)),
        ]),
    );
    fake.reset(
        "lez_core.transfer_public",
        RawOutput {
            code: 4,
            stdout: r#"{"code":"METHOD_FAILED","error":{"code":"timeout","message":"timed out"},"message":"timeout","status":"error"}"#.into(),
            stderr: String::new(),
        },
    );
    let (s, _dir) = services_server(fake.clone());
    assert!(is_error(
        &s.service_pay(CancellationToken::new(), pay())
            .await
            .unwrap()
    ));
    let r = s
        .service_pay(CancellationToken::new(), pay())
        .await
        .unwrap();
    assert!(is_error(&r));
    assert!(text(&r).contains("may"), "{}", text(&r));
    assert_eq!(fake.calls_to("lez_core.transfer_public").len(), 1);
}

#[tokio::test]
async fn a_job_id_in_two_conversations_is_refused() {
    let fake = node();
    fake.reset(
        "chat_module.list_conversations",
        chat(
            "list_conversations",
            r#"[{"convo_id":"aaaa","history_only":false,"kind":"direct","last_activity_ms":1,"message_count":1},{"convo_id":"bbbb","history_only":false,"kind":"direct","last_activity_ms":1,"message_count":1}]"#,
        ),
    );
    fake.set(
        "chat_module.get_messages",
        history(&[(false, &request_text())]),
    );
    let (s, _dir) = services_server(fake.clone());
    s.service_offer(offer()).await.unwrap();
    let r = s.service_quote(job(JOB)).await.unwrap();
    assert!(is_error(&r));
    assert!(text(&r).contains("more than one"), "{}", text(&r));
    assert!(fake.calls_to("lez_core.create_account_public").is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_conversation_that_stopped_working_is_opened_again() {
    let fake = node();
    let (s, _dir) = services_server(fake.clone());
    let ask = || {
        Parameters(RequestParams {
            chat_address: PEER.into(),
            service_id: SVC.into(),
            input_text: Some("hi".into()),
            input_cid: None,
        })
    };
    assert!(!is_error(&s.service_request(ask()).await.unwrap()));
    fake.reset(
        "chat_module.send_message",
        chat_refused("send_message", "conversation not found"),
    );
    assert!(is_error(&s.service_request(ask()).await.unwrap()));
    fake.reset("chat_module.send_message", chat_ok("send_message", "null"));
    assert!(!is_error(&s.service_request(ask()).await.unwrap()));
    assert_eq!(fake.calls_to("chat_module.create_conversation").len(), 2);
}

#[tokio::test]
async fn an_input_too_big_once_encoded_is_refused() {
    let fake = node();
    let (s, _dir) = services_server(fake.clone());
    let r = s
        .service_request(Parameters(RequestParams {
            chat_address: PEER.into(),
            service_id: SVC.into(),
            // Each quote doubles once escaped as JSON.
            input_text: Some("\"".repeat(40_000)),
            input_cid: None,
        }))
        .await
        .unwrap();
    assert!(is_error(&r));
    assert!(text(&r).contains("CID"), "{}", text(&r));
    assert!(fake.calls.lock().unwrap().is_empty());
}

/// The gas limit the test wallets' config sets, as the live ones do.
const GAS_LIMIT: u128 = 200_000;

fn services_server_in(fake: Arc<Routed>, dir: &std::path::Path) -> LogosServer {
    let config = dir.join("wallet_config.json");
    if !config.exists() {
        std::fs::write(&config, format!(r#"{{"gas_limit": {GAS_LIMIT}}}"#)).unwrap();
    }
    LogosServer::new(
        Arc::new(Logosctl::new(fake)),
        Arc::new(EventStore::default()),
        None,
        NodeSettings {
            preset: "logos.dev".into(),
            mode: "Edge".into(),
            config_dir: None,
            wallet: Some(WalletMode::Dedicated(WalletPaths {
                config: dir.join("wallet_config.json"),
                storage: dir.join("storage.json"),
            })),
            discovery_bootstrap: vec![FLEET_PEER.into()],
            host_owns_messaging: false,
        },
    )
}

fn now_ms() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

#[tokio::test]
async fn an_account_that_was_not_saved_is_never_quoted() {
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        history(&[(false, &request_text())]),
    );
    fake.reset("lez_core.save", lez("save", "1"));
    let (s, _dir) = services_server(fake.clone());
    s.service_offer(offer()).await.unwrap();
    let r = s.service_quote(job(JOB)).await.unwrap();
    assert!(is_error(&r), "{}", text(&r));
    assert!(fake.calls_to("chat_module.send_message").is_empty());
}

#[tokio::test]
async fn a_payment_not_yet_told_survives_a_restart() {
    let q = quote_text("logos.dev", FAR);
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        history(&[(true, &request_text()), (false, &q)]),
    );
    fake.reset(
        "chat_module.send_message",
        chat_refused("send_message", "delivery hiccup"),
    );
    fake.set("chat_module.send_message", chat_ok("send_message", "null"));
    let dir = tempfile::tempdir().unwrap();
    let first = services_server_in(fake.clone(), dir.path());
    assert!(is_error(
        &first
            .service_pay(CancellationToken::new(), pay())
            .await
            .unwrap()
    ));
    drop(first);
    // A new server over the same wallet only tells the provider.
    let fake2 = node();
    fake2.set(
        "chat_module.get_messages",
        history(&[(true, &request_text()), (false, &q)]),
    );
    let second = services_server_in(fake2.clone(), dir.path());
    let r = second
        .service_pay(CancellationToken::new(), pay())
        .await
        .unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    assert!(fake2.calls_to("lez_core.transfer_public").is_empty());
    assert!(matches!(
        sent(&fake2.calls_to("chat_module.send_message")[0]).body,
        Body::Paid { ref tx_hash, .. } if tx_hash == "aa11"
    ));
}

#[tokio::test]
async fn nothing_is_paid_while_the_job_account_cannot_be_read() {
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        history(&[
            (true, &request_text()),
            (false, &quote_text("logos.dev", FAR)),
        ]),
    );
    fake.reset("lez_core.get_balance", lez("get_balance", r#""""#));
    let (s, _dir) = services_server(fake.clone());
    let r = s
        .service_pay(CancellationToken::new(), pay())
        .await
        .unwrap();
    assert!(is_error(&r), "{}", text(&r));
    assert!(fake.calls_to("lez_core.transfer_public").is_empty());
}

#[tokio::test]
async fn a_lost_approval_receipt_is_never_taken_as_unpaid() {
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        history(&[
            (true, &request_text()),
            (false, &quote_text("logos.dev", FAR)),
        ]),
    );
    let (s, _dir) = services_server(fake.clone());
    s.services
        .lock()
        .await
        .pending
        .insert(JOB.into(), "evicted-request".into());
    let r = s
        .service_pay(CancellationToken::new(), pay())
        .await
        .unwrap();
    assert!(is_error(&r), "{}", text(&r));
    assert!(fake.calls_to("lez_core.transfer_public").is_empty());
}

#[tokio::test]
async fn a_paid_job_cannot_be_cancelled() {
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        history(&[
            (true, &request_text()),
            (false, &quote_text("logos.dev", FAR)),
        ]),
    );
    fake.reset("lez_core.get_balance", lez("get_balance", r#""1000000""#));
    let (s, _dir) = services_server(fake.clone());
    let r = s
        .service_cancel(Parameters(CancelParams {
            job: JOB.into(),
            reason: None,
        }))
        .await
        .unwrap();
    assert!(is_error(&r), "{}", text(&r));
    assert!(fake.calls_to("chat_module.send_message").is_empty());
}

#[tokio::test]
async fn a_claimed_payment_is_not_trusted_until_it_lands() {
    let q = quote_text("logos.dev", FAR);
    let forged = proto(
        3,
        Body::Paid {
            quote: protocol::quote_hash(&q),
            tx_hash: "made-up".into(),
            escrow_buyer: None,
        },
    );
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        history(&[(false, &request_text()), (true, &q), (false, &forged)]),
    );
    let (s, _dir) = services_server(fake.clone());
    let b = body(&s.service_jobs().await.unwrap());
    assert_eq!(b["jobs"][0]["state"], "quoted", "{b}");
    assert!(
        !b["jobs"][0]["next"].as_str().unwrap().contains("work"),
        "{b}"
    );
    fake.reset("lez_core.get_balance", lez("get_balance", r#""1000000""#));
    let b = body(&s.service_jobs().await.unwrap());
    assert_eq!(b["jobs"][0]["state"], "paid", "{b}");
}

#[tokio::test]
async fn with_approvals_on_a_quote_must_outlast_the_approval() {
    // Approval may take as long as a request lives: a quote expiring sooner
    // could be paid after it ended.
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        history(&[
            (true, &request_text()),
            (false, &quote_text("logos.dev", now_ms() + 5 * 60 * 1000)),
        ]),
    );
    let (s, _dir) = services_server(fake.clone());
    let (groups, rx) = tokio::sync::watch::channel(ToolGroups::default());
    groups.send_modify(|g| g.set("ask_before_transfer", true).unwrap());
    let s = s.with_tool_groups(rx);
    let r = s
        .service_pay(CancellationToken::new(), pay())
        .await
        .unwrap();
    assert!(is_error(&r), "{}", text(&r));
    assert!(s.approvals()["waiting"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn jobs_in_a_conversation_that_cannot_send_are_not_paid() {
    let fake = node();
    fake.reset(
        "chat_module.list_conversations",
        chat(
            "list_conversations",
            &format!(r#"[{{"convo_id":"{CONVO}","history_only":true,"kind":"direct","last_activity_ms":1,"message_count":2}}]"#),
        ),
    );
    fake.set(
        "chat_module.get_messages",
        history(&[
            (true, &request_text()),
            (false, &quote_text("logos.dev", FAR)),
        ]),
    );
    let (s, _dir) = services_server(fake.clone());
    let r = s
        .service_pay(CancellationToken::new(), pay())
        .await
        .unwrap();
    assert!(is_error(&r), "{}", text(&r));
    assert!(fake.calls_to("lez_core.transfer_public").is_empty());
}

#[tokio::test]
async fn group_conversations_carry_no_jobs() {
    let fake = node();
    fake.reset(
        "chat_module.list_conversations",
        chat(
            "list_conversations",
            &format!(r#"[{{"convo_id":"{CONVO}","history_only":false,"kind":"group","last_activity_ms":1,"message_count":1}}]"#),
        ),
    );
    fake.set(
        "chat_module.get_messages",
        history(&[(false, &request_text())]),
    );
    let (s, _dir) = services_server(fake);
    let b = body(&s.service_jobs().await.unwrap());
    assert_eq!(b["jobs"].as_array().unwrap().len(), 0, "{b}");
}

#[tokio::test]
async fn a_second_service_comes_with_a_warning() {
    let fake = node();
    let (s, _dir) = services_server(fake);
    let first = body(&s.service_offer(offer()).await.unwrap());
    assert!(first["warning"].is_null(), "{first}");
    let second = body(
        &s.service_offer(Parameters(OfferParams {
            service_id: "demo/translate/v1".into(),
            tag: "v1".into(),
            price_lepta: "1".into(),
            deliver_within_s: None,
            escrow: None,
            escrow_account: None,
        }))
        .await
        .unwrap(),
    );
    assert!(
        second["warning"]
            .as_str()
            .unwrap()
            .contains("first service"),
        "{second}"
    );
}

#[tokio::test]
async fn a_payment_that_landed_is_told_even_after_the_quote_expired() {
    let q = quote_text("logos.dev", 1);
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        history(&[(true, &request_text()), (false, &q)]),
    );
    fake.reset("lez_core.get_balance", lez("get_balance", r#""1000000""#));
    let (s, _dir) = services_server(fake.clone());
    let r = s
        .service_pay(CancellationToken::new(), pay())
        .await
        .unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    assert!(fake.calls_to("lez_core.transfer_public").is_empty());
    assert!(matches!(
        sent(&fake.calls_to("chat_module.send_message")[0]).body,
        Body::Paid { .. }
    ));
}

#[tokio::test]
async fn balances_are_read_with_the_wallet_open() {
    // lez_core reads no balance without an open wallet: a fresh server must
    // open it before it looks for a payment.
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        history(&[
            (false, &request_text()),
            (true, &quote_text("logos.dev", FAR)),
        ]),
    );
    fake.reset("lez_core.get_balance", lez("get_balance", r#""1000000""#));
    let (s, _dir) = services_server(fake.clone());
    let r = s.service_deliver(deliver_done()).await.unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    let calls = fake.calls.lock().unwrap();
    let first = |k: &str| calls.iter().position(|argv| key(argv) == k);
    let opened = first("lez_core.open").expect("the wallet was opened");
    assert!(opened < first("lez_core.get_balance").unwrap(), "{calls:?}");
}

// ---- escrow (protocol 1.1) ----

use crate::escrow;

fn id32(hex: &str) -> [u8; 32] {
    (0..32)
        .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap())
        .collect::<Vec<_>>()
        .try_into()
        .unwrap()
}

fn public_id(address: &str) -> [u8; 32] {
    id32(&crate::guard::public_account_hex(address).unwrap())
}

const PROVIDER_ACCOUNT: &str = "Public/9uwkVJuXScDwLKVtQMcHFU9Hvb3uZjiEDVvDv4PYjCfM";

fn escrow_quote_text(provider: &str, expires_at: u64, deadline: u64) -> String {
    proto(
        2,
        Body::Quote {
            price_lepta: "1000000".into(),
            pay_to: NEW_ADDRESS.into(),
            network: "logos.dev".into(),
            expires_at,
            deliver_within_s: 600,
            escrow: Some(protocol::EscrowTerms {
                program: escrow::PROGRAM_ID.into(),
                provider: provider.into(),
                deadline,
            }),
        },
    )
}

/// get_account_public's reply for a job account holding this record.
fn job_record(buyer: &str, provider: &str, amount: u128, deadline: u64, state: u8) -> RawOutput {
    job_record_with(buyer, provider, amount, deadline, state, [9; 32])
}

fn job_record_with(
    buyer: &str,
    provider: &str,
    amount: u128,
    deadline: u64,
    state: u8,
    result: [u8; 32],
) -> RawOutput {
    // As logos-mcp opens it: the provider is paid to its own account, and the
    // deposit goes back to the buyer's.
    job_record_paying(
        buyer, provider, provider, buyer, amount, deadline, state, result,
    )
}

#[allow(clippy::too_many_arguments)]
fn job_record_paying(
    buyer: &str,
    provider: &str,
    pay_to: &str,
    refund_to: &str,
    amount: u128,
    deadline: u64,
    state: u8,
    result: [u8; 32],
) -> RawOutput {
    let mut raw = Vec::new();
    raw.extend(public_id(buyer));
    raw.extend(public_id(provider));
    raw.extend(public_id(pay_to));
    raw.extend(public_id(refund_to));
    raw.extend(amount.to_le_bytes());
    raw.extend(deadline.to_le_bytes());
    // Delivered and Claimed carry the result; a Released job may not.
    if matches!(state, 1 | 2) {
        raw.push(1);
        raw.extend(result);
    } else {
        raw.push(0);
    }
    raw.push(state);
    let data: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    let account =
        json!({ "nonce": "0", "shards": [{ "program": escrow::PROGRAM_ID_HEX, "data": data }] });
    lez(
        "get_account_public",
        &serde_json::to_string(&account.to_string()).unwrap(),
    )
}

fn no_job_record() -> RawOutput {
    let account = json!({ "nonce": "0", "shards": [] });
    lez(
        "get_account_public",
        &serde_json::to_string(&account.to_string()).unwrap(),
    )
}

fn generic_ok(tx: &str) -> RawOutput {
    lez(
        "send_generic_public_transaction",
        &format!(r#""{{\"error\":\"\",\"success\":true,\"tx_hash\":\"{tx}\"}}""#),
    )
}

/// The escrow instruction in a send_generic_public_transaction call.
fn instruction(call: &[String]) -> Vec<u8> {
    let tagged: Value = serde_json::from_str(call[2].strip_prefix("json:").unwrap()).unwrap();
    URL_SAFE_NO_PAD
        .decode(tagged["_bytes"].as_str().unwrap())
        .unwrap()
}

#[tokio::test]
async fn offers_come_with_escrow_unless_turned_off() {
    let fake = node();
    let (s, _dir) = services_server(fake.clone());
    let b = body(&s.service_offer(offer()).await.unwrap());
    assert_eq!(b["escrow_account"], AGENT_FROM, "{b}");
    let b = body(
        &s.service_offer(Parameters(OfferParams {
            escrow: Some(false),
            ..offer().0
        }))
        .await
        .unwrap(),
    );
    assert!(b["escrow_account"].is_null(), "{b}");
}

#[tokio::test]
async fn an_escrow_offer_quotes_its_terms() {
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        history(&[(false, &request_text())]),
    );
    let (s, _dir) = services_server(fake.clone());
    s.service_offer(offer()).await.unwrap();
    s.service_quote(job(JOB)).await.unwrap();
    let m = sent(&fake.calls_to("chat_module.send_message")[0]);
    let Body::Quote {
        expires_at,
        deliver_within_s,
        escrow: Some(terms),
        ..
    } = m.body
    else {
        panic!("no escrow terms: {m:?}");
    };
    assert_eq!(terms.program, escrow::PROGRAM_ID);
    assert_eq!(terms.provider, AGENT_FROM);
    assert_eq!(terms.deadline, expires_at + deliver_within_s * 1000);
}

#[tokio::test]
async fn paying_an_escrow_quote_opens_it_and_tells_once_it_landed() {
    let q = escrow_quote_text(PROVIDER_ACCOUNT, FAR, FAR + 600_000);
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        history(&[(true, &request_text()), (false, &q)]),
    );
    fake.reset("lez_core.get_balance", lez("get_balance", r#""5000000""#));
    fake.set("lez_core.get_account_public", no_job_record());
    fake.set(
        "lez_core.send_generic_public_transaction",
        generic_ok("open1"),
    );
    let dir = tempfile::tempdir().unwrap();
    let s = services_server_in(fake.clone(), dir.path());
    let r = s
        .service_pay(CancellationToken::new(), pay())
        .await
        .unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    assert!(fake.calls_to("lez_core.transfer_public").is_empty());
    let call = &fake.calls_to("lez_core.send_generic_public_transaction")[0];
    let job_ref = id32(&protocol::quote_hash(&q));
    let job_account = escrow::job_account(&public_id(AGENT_FROM), &job_ref);
    // Paid out to the provider's escrow account, refunded to the paying one.
    let want = escrow::Instruction::Open {
        job_ref,
        provider: public_id(PROVIDER_ACCOUNT),
        pay_to: public_id(PROVIDER_ACCOUNT),
        refund_to: public_id(AGENT_FROM),
        amount: 1_000_000,
        deadline: FAR + 600_000,
    };
    assert_eq!(instruction(call), want.encode());
    assert!(call[0].contains(&hex(&job_account)), "{call:?}");
    // Not told before the escrow holds it.
    assert!(fake.calls_to("chat_module.send_message").is_empty());

    // Once it landed, a new server over the same wallet tells the provider.
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        history(&[(true, &request_text()), (false, &q)]),
    );
    fake.set(
        "lez_core.get_account_public",
        job_record(AGENT_FROM, PROVIDER_ACCOUNT, 1_000_000, FAR + 600_000, 0),
    );
    let s = services_server_in(fake.clone(), dir.path());
    let r = s
        .service_pay(CancellationToken::new(), pay())
        .await
        .unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    assert!(
        fake.calls_to("lez_core.send_generic_public_transaction")
            .is_empty()
    );
    assert_eq!(
        sent(&fake.calls_to("chat_module.send_message")[0]).body,
        Body::Paid {
            quote: protocol::quote_hash(&q),
            tx_hash: "open1".into(),
            escrow_buyer: Some(AGENT_FROM.into()),
        }
    );
}

#[tokio::test]
async fn an_open_refused_on_chain_can_be_paid_again() {
    let q = escrow_quote_text(PROVIDER_ACCOUNT, FAR, FAR + 600_000);
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        history(&[(true, &request_text()), (false, &q)]),
    );
    fake.reset("lez_core.get_balance", lez("get_balance", r#""5000000""#));
    fake.set("lez_core.get_account_public", no_job_record());
    fake.set(
        "lez_core.send_generic_public_transaction",
        generic_ok("open1"),
    );
    // Included, yet no job: refused on chain.
    fake.set(
        "lez_core.poll_transaction_status",
        lez("poll_transaction_status", "true"),
    );
    let (s, _dir) = services_server(fake.clone());
    s.service_pay(CancellationToken::new(), pay())
        .await
        .unwrap();
    let r = s
        .service_pay(CancellationToken::new(), pay())
        .await
        .unwrap();
    assert!(is_error(&r), "{}", text(&r));
    assert!(text(&r).contains("refused"), "{}", text(&r));
    let r = s
        .service_pay(CancellationToken::new(), pay())
        .await
        .unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    assert_eq!(
        fake.calls_to("lez_core.send_generic_public_transaction")
            .len(),
        2
    );
}

#[tokio::test]
async fn an_escrow_payment_keeps_to_its_first_account() {
    let q = escrow_quote_text(PROVIDER_ACCOUNT, FAR, FAR + 600_000);
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        history(&[(true, &request_text()), (false, &q)]),
    );
    fake.reset("lez_core.get_balance", lez("get_balance", r#""5000000""#));
    fake.set("lez_core.get_account_public", no_job_record());
    fake.set(
        "lez_core.send_generic_public_transaction",
        generic_ok("open1"),
    );
    let (s, _dir) = services_server(fake.clone());
    let first = s
        .service_pay(CancellationToken::new(), pay())
        .await
        .unwrap();
    assert!(!is_error(&first), "first: {}", text(&first));
    let r = s
        .service_pay(
            CancellationToken::new(),
            Parameters(PayParams {
                job: JOB.into(),
                from: NEW_ADDRESS.into(),
            }),
        )
        .await
        .unwrap();
    assert!(is_error(&r), "{}", text(&r));
    assert_eq!(
        fake.calls_to("lez_core.send_generic_public_transaction")
            .len(),
        1
    );
}

#[tokio::test]
async fn an_escrow_quote_must_keep_the_protocols_deadline() {
    for deadline in [u64::MAX, FAR + 600_001, FAR + 1] {
        let q = escrow_quote_text(PROVIDER_ACCOUNT, FAR, deadline);
        let fake = node();
        fake.set(
            "chat_module.get_messages",
            history(&[(true, &request_text()), (false, &q)]),
        );
        fake.reset("lez_core.get_balance", lez("get_balance", r#""5000000""#));
        fake.set("lez_core.get_account_public", no_job_record());
        fake.set(
            "lez_core.send_generic_public_transaction",
            generic_ok("open1"),
        );
        let (s, _dir) = services_server(fake.clone());
        let r = s
            .service_pay(CancellationToken::new(), pay())
            .await
            .unwrap();
        assert!(is_error(&r), "{deadline}: {}", text(&r));
        assert!(
            fake.calls_to("lez_core.send_generic_public_transaction")
                .is_empty()
        );
    }
}

#[tokio::test]
async fn an_escrow_is_not_opened_without_the_funds() {
    let q = escrow_quote_text(PROVIDER_ACCOUNT, FAR, FAR + 600_000);
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        history(&[(true, &request_text()), (false, &q)]),
    );
    fake.reset("lez_core.get_balance", lez("get_balance", r#""999999""#));
    fake.set("lez_core.get_account_public", no_job_record());
    let (s, _dir) = services_server(fake.clone());
    let r = s
        .service_pay(CancellationToken::new(), pay())
        .await
        .unwrap();
    assert!(is_error(&r), "{}", text(&r));
    assert!(
        fake.calls_to("lez_core.send_generic_public_transaction")
            .is_empty()
    );

    // The price, but not the network's fee reserve on top: LEZ would refuse
    // the open and keep the whole reserve, as it did live with 2.28M lepta.
    fake.reset("lez_core.get_balance", lez("get_balance", r#""2280000""#));
    let r = s
        .service_pay(CancellationToken::new(), pay())
        .await
        .unwrap();
    assert!(is_error(&r), "{}", text(&r));
    assert!(text(&r).contains("fee reserve"), "{}", text(&r));
    assert!(
        fake.calls_to("lez_core.send_generic_public_transaction")
            .is_empty()
    );
}

#[tokio::test]
async fn a_job_being_paid_cannot_be_cancelled_meanwhile() {
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        history(&[
            (true, &request_text()),
            (false, &quote_text("logos.dev", FAR)),
        ]),
    );
    let (s, _dir) = services_server(fake.clone());
    s.services.lock().await.busy.insert(JOB.into());
    let r = s
        .service_cancel(Parameters(CancelParams {
            job: JOB.into(),
            reason: None,
        }))
        .await
        .unwrap();
    assert!(is_error(&r), "{}", text(&r));
    assert!(fake.calls_to("chat_module.send_message").is_empty());
}

#[tokio::test]
async fn escrow_money_moves_even_after_chat_restarted() {
    // Refund and claim only touch the chain: a conversation that can't send
    // any more must not strand the money.
    let past = 1_000_000;
    let q = escrow_quote_text(PROVIDER_ACCOUNT, past - 600_000, past);
    let paid = proto(
        3,
        Body::Paid {
            quote: protocol::quote_hash(&q),
            tx_hash: "open1".into(),
            escrow_buyer: Some(AGENT_FROM.into()),
        },
    );
    let fake = node();
    fake.reset(
        "chat_module.list_conversations",
        chat(
            "list_conversations",
            &format!(r#"[{{"convo_id":"{CONVO}","history_only":true,"kind":"direct","last_activity_ms":1,"message_count":3}}]"#),
        ),
    );
    fake.set(
        "chat_module.get_messages",
        history(&[(true, &request_text()), (false, &q), (true, &paid)]),
    );
    fake.set(
        "lez_core.get_account_public",
        job_record(AGENT_FROM, PROVIDER_ACCOUNT, 1_000_000, past, 0),
    );
    fake.set(
        "lez_core.send_generic_public_transaction",
        generic_ok("refund1"),
    );
    let (s, _dir) = services_server(fake.clone());
    let r = s.service_refund(job(JOB)).await.unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    let b = body(&s.service_jobs().await.unwrap());
    assert!(
        b["jobs"][0]["next"]
            .as_str()
            .unwrap_or_default()
            .contains("service_refund"),
        "{b}"
    );
}
#[tokio::test]
async fn an_unknown_escrow_program_is_never_paid() {
    let q = escrow_quote_text(PROVIDER_ACCOUNT, FAR, FAR + 600_000)
        .replace(escrow::PROGRAM_ID, "11111111111111111111111111111111");
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        history(&[(true, &request_text()), (false, &q)]),
    );
    // Everything else would let it through.
    fake.set("lez_core.get_account_public", no_job_record());
    fake.set(
        "lez_core.send_generic_public_transaction",
        generic_ok("open1"),
    );
    let (s, _dir) = services_server(fake.clone());
    let r = s
        .service_pay(CancellationToken::new(), pay())
        .await
        .unwrap();
    assert!(is_error(&r), "{}", text(&r));
    assert!(text(&r).contains("program"), "{}", text(&r));
    assert!(
        fake.calls_to("lez_core.send_generic_public_transaction")
            .is_empty()
    );
    assert!(fake.calls_to("lez_core.transfer_public").is_empty());
}

#[tokio::test]
async fn an_escrow_job_already_opened_is_not_opened_again() {
    let q = escrow_quote_text(PROVIDER_ACCOUNT, FAR, FAR + 600_000);
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        history(&[(true, &request_text()), (false, &q)]),
    );
    fake.set(
        "lez_core.get_account_public",
        job_record(AGENT_FROM, PROVIDER_ACCOUNT, 1_000_000, FAR + 600_000, 0),
    );
    let (s, _dir) = services_server(fake.clone());
    let r = s
        .service_pay(CancellationToken::new(), pay())
        .await
        .unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    assert!(
        fake.calls_to("lez_core.send_generic_public_transaction")
            .is_empty()
    );
    assert!(matches!(
        sent(&fake.calls_to("chat_module.send_message")[0]).body,
        Body::Paid {
            escrow_buyer: Some(_),
            ..
        }
    ));
}

/// The provider's view of an escrow job the buyer says it opened.
fn provider_escrow_history(deadline: u64) -> RawOutput {
    let q = escrow_quote_text(AGENT_FROM, deadline - 600_000, deadline);
    let paid = proto(
        3,
        Body::Paid {
            quote: protocol::quote_hash(&q),
            tx_hash: "open1".into(),
            escrow_buyer: Some(PROVIDER_ACCOUNT.into()),
        },
    );
    history(&[(false, &request_text()), (true, &q), (false, &paid)])
}

#[tokio::test]
async fn a_provider_counts_an_escrow_payment_only_on_the_right_terms() {
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        provider_escrow_history(FAR + 600_000),
    );
    // Locked, but for less than the price.
    fake.set(
        "lez_core.get_account_public",
        job_record(PROVIDER_ACCOUNT, AGENT_FROM, 999_999, FAR + 600_000, 0),
    );
    let (s, _dir) = services_server(fake.clone());
    let b = body(&s.service_jobs().await.unwrap());
    assert_eq!(b["jobs"][0]["state"], "quoted", "{b}");
    // The full price, paid out to someone else's account.
    fake.reset(
        "lez_core.get_account_public",
        job_record_paying(
            PROVIDER_ACCOUNT,
            AGENT_FROM,
            PROVIDER_ACCOUNT,
            PROVIDER_ACCOUNT,
            1_000_000,
            FAR + 600_000,
            0,
            [0; 32],
        ),
    );
    let b = body(&s.service_jobs().await.unwrap());
    assert_eq!(b["jobs"][0]["state"], "quoted", "{b}");
    fake.reset(
        "lez_core.get_account_public",
        job_record(PROVIDER_ACCOUNT, AGENT_FROM, 1_000_000, FAR + 600_000, 0),
    );
    let b = body(&s.service_jobs().await.unwrap());
    assert_eq!(b["jobs"][0]["state"], "paid", "{b}");
    assert_eq!(b["jobs"][0]["escrow"]["state"], "open", "{b}");
}

#[tokio::test]
async fn delivering_an_escrow_job_records_the_result_before_sending_it() {
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        provider_escrow_history(FAR + 600_000),
    );
    fake.set(
        "lez_core.get_account_public",
        job_record(PROVIDER_ACCOUNT, AGENT_FROM, 1_000_000, FAR + 600_000, 0),
    );
    fake.set(
        "lez_core.send_generic_public_transaction",
        generic_ok("deliver1"),
    );
    let (s, _dir) = services_server(fake.clone());
    let r = s.service_deliver(deliver_done()).await.unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    assert!(
        fake.calls_to("chat_module.send_message").is_empty(),
        "not sent before the delivery lands"
    );
    let call = &fake.calls_to("lez_core.send_generic_public_transaction")[0];
    let recorded = instruction(call);
    // The provider signs as the escrow's provider account.
    assert!(call[1].starts_with("json:[true"), "{call:?}");

    // Once it landed, the same result goes out, and its text hashes to what
    // was recorded.
    let recorded_hash: [u8; 32] = recorded[1..33].try_into().unwrap();
    fake.reset(
        "lez_core.get_account_public",
        job_record_with(
            PROVIDER_ACCOUNT,
            AGENT_FROM,
            1_000_000,
            FAR + 600_000,
            1,
            recorded_hash,
        ),
    );
    let r = s.service_deliver(deliver_done()).await.unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    assert_eq!(
        fake.calls_to("lez_core.send_generic_public_transaction")
            .len(),
        1
    );
    let result_text = fake.calls_to("chat_module.send_message")[0][1]
        .strip_prefix("str:")
        .unwrap()
        .to_string();
    let want = escrow::Instruction::Deliver {
        result: id32(&protocol::quote_hash(&result_text)),
        deadline: FAR + 600_000,
    };
    assert_eq!(recorded, want.encode());
}
#[tokio::test]
async fn claiming_waits_for_the_delivery_to_land() {
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        provider_escrow_history(FAR + 600_000),
    );
    fake.set(
        "lez_core.get_account_public",
        job_record(PROVIDER_ACCOUNT, AGENT_FROM, 1_000_000, FAR + 600_000, 0),
    );
    fake.set(
        "lez_core.send_generic_public_transaction",
        generic_ok("claim1"),
    );
    let (s, _dir) = services_server(fake.clone());
    let r = s.service_claim(job(JOB)).await.unwrap();
    assert!(is_error(&r), "{}", text(&r));
    assert!(
        fake.calls_to("lez_core.send_generic_public_transaction")
            .is_empty()
    );

    fake.reset(
        "lez_core.get_account_public",
        job_record(PROVIDER_ACCOUNT, AGENT_FROM, 1_000_000, FAR + 600_000, 1),
    );
    let r = s.service_claim(job(JOB)).await.unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    let call = &fake.calls_to("lez_core.send_generic_public_transaction")[0];
    let q = escrow_quote_text(AGENT_FROM, FAR, FAR + 600_000);
    let job_ref = id32(&protocol::quote_hash(&q));
    let want = escrow::Instruction::Claim {
        buyer: public_id(PROVIDER_ACCOUNT),
        job_ref,
        amount: 1_000_000,
    };
    assert_eq!(instruction(call), want.encode());
    // [provider, job]: pay_to is the provider itself, so it is left out.
    let job_account = escrow::job_account(&public_id(PROVIDER_ACCOUNT), &job_ref);
    assert_eq!(
        call[0],
        format!(
            r#"json:["{}","{}"]"#,
            hex(&public_id(AGENT_FROM)),
            hex(&job_account)
        )
    );
}

#[tokio::test]
async fn a_released_payment_is_claimed_without_delivering() {
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        provider_escrow_history(FAR + 600_000),
    );
    fake.set(
        "lez_core.get_account_public",
        job_record(PROVIDER_ACCOUNT, AGENT_FROM, 1_000_000, FAR + 600_000, 3),
    );
    fake.set(
        "lez_core.send_generic_public_transaction",
        generic_ok("claim1"),
    );
    let (s, _dir) = services_server(fake.clone());
    let b = body(&s.service_jobs().await.unwrap());
    assert!(
        b["jobs"][0]["next"]
            .as_str()
            .unwrap_or_default()
            .contains("service_claim"),
        "{b}"
    );
    let r = s.service_claim(job(JOB)).await.unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    assert_eq!(
        instruction(&fake.calls_to("lez_core.send_generic_public_transaction")[0])[0],
        2
    );

    // Once claimed, nothing more is sent.
    fake.reset(
        "lez_core.get_account_public",
        job_record(PROVIDER_ACCOUNT, AGENT_FROM, 1_000_000, FAR + 600_000, 2),
    );
    let r = s.service_claim(job(JOB)).await.unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    assert_eq!(
        fake.calls_to("lez_core.send_generic_public_transaction")
            .len(),
        1
    );
}

#[tokio::test]
async fn a_refund_comes_only_after_the_deadline() {
    let past = 1_000_000;
    let q = escrow_quote_text(PROVIDER_ACCOUNT, past - 600_000, past);
    let paid = proto(
        3,
        Body::Paid {
            quote: protocol::quote_hash(&q),
            tx_hash: "open1".into(),
            escrow_buyer: Some(AGENT_FROM.into()),
        },
    );
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        history(&[(true, &request_text()), (false, &q), (true, &paid)]),
    );
    fake.set(
        "lez_core.get_account_public",
        job_record(AGENT_FROM, PROVIDER_ACCOUNT, 1_000_000, past, 0),
    );
    fake.set(
        "lez_core.send_generic_public_transaction",
        generic_ok("refund1"),
    );
    let (s, _dir) = services_server(fake.clone());
    let r = s.service_refund(job(JOB)).await.unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    let call = &fake.calls_to("lez_core.send_generic_public_transaction")[0];
    let want = escrow::Instruction::Refund {
        job_ref: id32(&protocol::quote_hash(&q)),
        amount: 1_000_000,
        deadline: past,
    };
    assert_eq!(instruction(call), want.encode());

    // [buyer, job]: refund_to is the buyer itself, so it is left out.
    let job_account = escrow::job_account(&public_id(AGENT_FROM), &id32(&protocol::quote_hash(&q)));
    assert_eq!(
        call[0],
        format!(
            r#"json:["{}","{}"]"#,
            hex(&public_id(AGENT_FROM)),
            hex(&job_account)
        )
    );

    // Before the deadline, once delivered, or once released, nothing is sent.
    for (deadline, state) in [(FAR, 0u8), (past, 1u8), (past, 3u8)] {
        let q = escrow_quote_text(PROVIDER_ACCOUNT, deadline - 600_000, deadline);
        let paid = proto(
            3,
            Body::Paid {
                quote: protocol::quote_hash(&q),
                tx_hash: "open1".into(),
                escrow_buyer: Some(AGENT_FROM.into()),
            },
        );
        let fake = node();
        fake.set(
            "chat_module.get_messages",
            history(&[(true, &request_text()), (false, &q), (true, &paid)]),
        );
        fake.set(
            "lez_core.get_account_public",
            job_record(AGENT_FROM, PROVIDER_ACCOUNT, 1_000_000, deadline, state),
        );
        let (s, _dir) = services_server(fake.clone());
        assert!(is_error(&s.service_refund(job(JOB)).await.unwrap()));
        assert!(
            fake.calls_to("lez_core.send_generic_public_transaction")
                .is_empty()
        );
    }
}

#[tokio::test]
async fn deadlines_keep_a_margin_for_block_time() {
    // Block timestamps were seen 138 s behind: a refund three minutes after
    // the deadline could still meet a block stamped before it, and a refused
    // call costs its whole fee.
    let just_past = now_ms() - 3 * 60_000;
    let q = escrow_quote_text(PROVIDER_ACCOUNT, just_past - 600_000, just_past);
    let paid = proto(
        3,
        Body::Paid {
            quote: protocol::quote_hash(&q),
            tx_hash: "open1".into(),
            escrow_buyer: Some(AGENT_FROM.into()),
        },
    );
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        history(&[(true, &request_text()), (false, &q), (true, &paid)]),
    );
    fake.set(
        "lez_core.get_account_public",
        job_record(AGENT_FROM, PROVIDER_ACCOUNT, 1_000_000, just_past, 0),
    );
    let (s, _dir) = services_server(fake.clone());
    assert!(is_error(&s.service_refund(job(JOB)).await.unwrap()));

    // Likewise a delivery three minutes before it.
    let soon = now_ms() + 3 * 60_000;
    let fake = node();
    fake.set("chat_module.get_messages", provider_escrow_history(soon));
    fake.set(
        "lez_core.get_account_public",
        job_record(PROVIDER_ACCOUNT, AGENT_FROM, 1_000_000, soon, 0),
    );
    let (s, _dir) = services_server(fake.clone());
    assert!(is_error(&s.service_deliver(deliver_done()).await.unwrap()));
    assert!(
        fake.calls_to("lez_core.send_generic_public_transaction")
            .is_empty()
    );
}

#[tokio::test]
async fn a_refunded_escrow_is_not_a_paid_job() {
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        provider_escrow_history(FAR + 600_000),
    );
    fake.set(
        "lez_core.get_account_public",
        job_record(PROVIDER_ACCOUNT, AGENT_FROM, 1_000_000, FAR + 600_000, 4),
    );
    let (s, _dir) = services_server(fake);
    let b = body(&s.service_jobs().await.unwrap());
    assert_ne!(b["jobs"][0]["state"], "paid", "{b}");
}

#[tokio::test]
async fn declining_a_paid_escrow_job_returns_the_money() {
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        provider_escrow_history(FAR + 600_000),
    );
    fake.set(
        "lez_core.get_account_public",
        job_record(PROVIDER_ACCOUNT, AGENT_FROM, 1_000_000, FAR + 600_000, 0),
    );
    fake.set(
        "lez_core.send_generic_public_transaction",
        generic_ok("return1"),
    );
    let (s, _dir) = services_server(fake.clone());
    let r = s
        .service_reject(Parameters(RejectParams {
            job: JOB.into(),
            reason: "can't do it".into(),
        }))
        .await
        .unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    let call = &fake.calls_to("lez_core.send_generic_public_transaction")[0];
    let q = escrow_quote_text(AGENT_FROM, FAR, FAR + 600_000);
    let job_ref = id32(&protocol::quote_hash(&q));
    assert_eq!(instruction(call), escrow::Instruction::Return.encode());
    // [provider, job], the provider signing: a return moves nothing.
    let job_account = escrow::job_account(&public_id(PROVIDER_ACCOUNT), &job_ref);
    assert_eq!(
        call[0],
        format!(
            r#"json:["{}","{}"]"#,
            hex(&public_id(AGENT_FROM)),
            hex(&job_account)
        )
    );
    assert_eq!(call[1], "json:[true,false]");
    assert!(matches!(
        sent(&fake.calls_to("chat_module.send_message")[0]).body,
        Body::Failed { .. }
    ));
}

#[tokio::test]
async fn a_recorded_delivery_cannot_be_declined() {
    // The money is the provider's once its delivery is on chain: declining
    // then would tell the buyer it failed while the provider can still claim.
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        provider_escrow_history(FAR + 600_000),
    );
    fake.set(
        "lez_core.get_account_public",
        job_record(PROVIDER_ACCOUNT, AGENT_FROM, 1_000_000, FAR + 600_000, 1),
    );
    let (s, _dir) = services_server(fake.clone());
    let r = s
        .service_reject(Parameters(RejectParams {
            job: JOB.into(),
            reason: "can't do it".into(),
        }))
        .await
        .unwrap();
    assert!(is_error(&r), "{}", text(&r));
    assert!(fake.calls_to("chat_module.send_message").is_empty());
    assert!(
        fake.calls_to("lez_core.send_generic_public_transaction")
            .is_empty()
    );
}

#[tokio::test]
async fn a_failed_job_still_open_in_escrow_points_to_the_refund() {
    // The provider said it failed, but its return never landed.
    let past = 1_000_000;
    let q = escrow_quote_text(PROVIDER_ACCOUNT, past - 600_000, past);
    let paid = proto(
        3,
        Body::Paid {
            quote: protocol::quote_hash(&q),
            tx_hash: "open1".into(),
            escrow_buyer: Some(AGENT_FROM.into()),
        },
    );
    let failed = proto(
        4,
        Body::Failed {
            reason: "broke".into(),
        },
    );
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        history(&[
            (true, &request_text()),
            (false, &q),
            (true, &paid),
            (false, &failed),
        ]),
    );
    fake.set(
        "lez_core.get_account_public",
        job_record(AGENT_FROM, PROVIDER_ACCOUNT, 1_000_000, past, 0),
    );
    let (s, _dir) = services_server(fake);
    let b = body(&s.service_jobs().await.unwrap());
    assert!(
        b["jobs"][0]["next"]
            .as_str()
            .unwrap_or_default()
            .contains("service_refund"),
        "{b}"
    );
}

#[tokio::test]
async fn a_returned_job_is_refunded_at_once() {
    // Declined: the provider returned it long before the deadline.
    let q = escrow_quote_text(PROVIDER_ACCOUNT, FAR, FAR + 600_000);
    let paid = proto(
        3,
        Body::Paid {
            quote: protocol::quote_hash(&q),
            tx_hash: "open1".into(),
            escrow_buyer: Some(AGENT_FROM.into()),
        },
    );
    let failed = proto(
        4,
        Body::Failed {
            reason: "can't do it".into(),
        },
    );
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        history(&[
            (true, &request_text()),
            (false, &q),
            (true, &paid),
            (false, &failed),
        ]),
    );
    fake.set(
        "lez_core.get_account_public",
        job_record(AGENT_FROM, PROVIDER_ACCOUNT, 1_000_000, FAR + 600_000, 5),
    );
    fake.set(
        "lez_core.send_generic_public_transaction",
        generic_ok("refund1"),
    );
    let (s, _dir) = services_server(fake.clone());
    let b = body(&s.service_jobs().await.unwrap());
    assert!(
        b["jobs"][0]["next"]
            .as_str()
            .unwrap_or_default()
            .contains("service_refund"),
        "{b}"
    );
    let r = s.service_refund(job(JOB)).await.unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    let call = &fake.calls_to("lez_core.send_generic_public_transaction")[0];
    // After a return the deadline no longer applies: the program takes 0.
    let job_ref = id32(&protocol::quote_hash(&q));
    let want = escrow::Instruction::Refund {
        job_ref,
        amount: 1_000_000,
        deadline: 0,
    };
    assert_eq!(instruction(call), want.encode());
    // [buyer, job], the buyer signing and paid back to itself.
    let job_account = escrow::job_account(&public_id(AGENT_FROM), &job_ref);
    assert_eq!(
        call[0],
        format!(
            r#"json:["{}","{}"]"#,
            hex(&public_id(AGENT_FROM)),
            hex(&job_account)
        )
    );
    assert_eq!(call[1], "json:[true,false]");
}

#[tokio::test]
async fn declining_after_the_deadline_sends_no_return() {
    // The buyer can refund by then, and a refund landing first would make
    // the return fail at the provider's cost.
    let past = 1_000_000;
    let fake = node();
    fake.set("chat_module.get_messages", provider_escrow_history(past));
    fake.set(
        "lez_core.get_account_public",
        job_record(PROVIDER_ACCOUNT, AGENT_FROM, 1_000_000, past, 0),
    );
    let (s, _dir) = services_server(fake.clone());
    let r = s
        .service_reject(Parameters(RejectParams {
            job: JOB.into(),
            reason: "can't do it".into(),
        }))
        .await
        .unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    assert!(
        fake.calls_to("lez_core.send_generic_public_transaction")
            .is_empty()
    );
    assert!(matches!(
        sent(&fake.calls_to("chat_module.send_message")[0]).body,
        Body::Failed { .. }
    ));
}

#[tokio::test]
async fn a_returned_escrow_is_not_a_paid_job() {
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        provider_escrow_history(FAR + 600_000),
    );
    fake.set(
        "lez_core.get_account_public",
        job_record(PROVIDER_ACCOUNT, AGENT_FROM, 1_000_000, FAR + 600_000, 5),
    );
    let (s, _dir) = services_server(fake.clone());
    let b = body(&s.service_jobs().await.unwrap());
    assert_ne!(b["jobs"][0]["state"], "paid", "{b}");
    // Nor can it be delivered.
    assert!(is_error(&s.service_deliver(deliver_done()).await.unwrap()));
    assert!(
        fake.calls_to("lez_core.send_generic_public_transaction")
            .is_empty()
    );
}

#[tokio::test]
async fn an_escrow_call_still_on_its_way_is_not_sent_again() {
    // A second claim would be refused on chain once the first lands, at the
    // cost of its whole fee.
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        provider_escrow_history(FAR + 600_000),
    );
    fake.set(
        "lez_core.get_account_public",
        job_record(PROVIDER_ACCOUNT, AGENT_FROM, 1_000_000, FAR + 600_000, 1),
    );
    fake.set(
        "lez_core.send_generic_public_transaction",
        generic_ok("claim1"),
    );
    fake.set(
        "lez_core.poll_transaction_status",
        lez("poll_transaction_status", "false"),
    );
    let (s, _dir) = services_server(fake.clone());
    assert!(!is_error(&s.service_claim(job(JOB)).await.unwrap()));
    let r = s.service_claim(job(JOB)).await.unwrap();
    assert!(is_error(&r), "{}", text(&r));
    assert!(text(&r).contains("claim1"), "{}", text(&r));
    assert_eq!(
        fake.calls_to("lez_core.send_generic_public_transaction")
            .len(),
        1
    );

    // Included, yet the job is still only delivered: that claim was refused
    // on chain, so it may go again.
    fake.reset(
        "lez_core.poll_transaction_status",
        lez("poll_transaction_status", "true"),
    );
    assert!(!is_error(&s.service_claim(job(JOB)).await.unwrap()));
    assert_eq!(
        fake.calls_to("lez_core.send_generic_public_transaction")
            .len(),
        2
    );
}

#[tokio::test]
async fn a_refund_keeps_the_paying_account_until_it_lands() {
    // The `paid` message never went out: only this server's note says which
    // account opened the escrow.
    let past = 1_000_000;
    let q = escrow_quote_text(PROVIDER_ACCOUNT, past - 600_000, past);
    let fake_with = |state: u8| {
        let fake = node();
        fake.set(
            "chat_module.get_messages",
            history(&[(true, &request_text()), (false, &q)]),
        );
        fake.set(
            "lez_core.get_account_public",
            job_record(AGENT_FROM, PROVIDER_ACCOUNT, 1_000_000, past, state),
        );
        fake.set(
            "lez_core.send_generic_public_transaction",
            generic_ok("refund1"),
        );
        fake
    };
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("service-payments.json"),
        json!({ JOB: { "tx_hash": "open1", "escrow_buyer": AGENT_FROM } }).to_string(),
    )
    .unwrap();
    let s = services_server_in(fake_with(0), dir.path());
    assert!(!is_error(&s.service_refund(job(JOB)).await.unwrap()));

    // After a restart the refund hasn't landed: the account is still known.
    let s = services_server_in(fake_with(0), dir.path());
    let r = s.service_refund(job(JOB)).await.unwrap();
    assert!(!is_error(&r), "{}", text(&r));

    // Once it landed, the note goes.
    let s = services_server_in(fake_with(4), dir.path());
    let r = s.service_refund(job(JOB)).await.unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    let s = services_server_in(fake_with(4), dir.path());
    let r = s.service_refund(job(JOB)).await.unwrap();
    assert!(is_error(&r), "{}", text(&r));
}

#[tokio::test]
async fn a_job_too_late_to_deliver_is_not_worked_on() {
    // Paid, but the delivery couldn't be recorded before the deadline any more.
    let soon = now_ms() + 3 * 60_000;
    let fake = node();
    fake.set("chat_module.get_messages", provider_escrow_history(soon));
    fake.set(
        "lez_core.get_account_public",
        job_record(PROVIDER_ACCOUNT, AGENT_FROM, 1_000_000, soon, 0),
    );
    let (s, _dir) = services_server(fake);
    let b = body(&s.service_jobs().await.unwrap());
    let next = b["jobs"][0]["next"].as_str().unwrap_or_default();
    assert!(!next.contains("do the work"), "{b}");
    assert!(next.contains("service_reject"), "{b}");
}

#[tokio::test]
async fn an_escrow_too_close_to_its_deadline_is_not_opened() {
    // A one-minute delivery window: by the time the open lands, the
    // provider couldn't deliver, and an open after the deadline is refused
    // at the cost of its fee.
    let expires = now_ms() + 60_000;
    let q = proto(
        2,
        Body::Quote {
            price_lepta: "1000000".into(),
            pay_to: NEW_ADDRESS.into(),
            network: "logos.dev".into(),
            expires_at: expires,
            deliver_within_s: 60,
            escrow: Some(protocol::EscrowTerms {
                program: escrow::PROGRAM_ID.into(),
                provider: PROVIDER_ACCOUNT.into(),
                deadline: expires + 60_000,
            }),
        },
    );
    let fake = node();
    fake.set(
        "chat_module.get_messages",
        history(&[(true, &request_text()), (false, &q)]),
    );
    fake.reset("lez_core.get_balance", lez("get_balance", r#""5000000""#));
    fake.set("lez_core.get_account_public", no_job_record());
    let (s, _dir) = services_server(fake.clone());
    let r = s
        .service_pay(CancellationToken::new(), pay())
        .await
        .unwrap();
    assert!(is_error(&r), "{}", text(&r));
    assert!(
        fake.calls_to("lez_core.send_generic_public_transaction")
            .is_empty()
    );
}

#[tokio::test]
async fn a_bad_delivery_window_is_refused_before_advertising() {
    let fake = node();
    let (s, _dir) = services_server(fake.clone());
    let mut p = offer();
    p.0.deliver_within_s = Some(0);
    let r = s.service_offer(p).await.unwrap();
    assert!(is_error(&r), "{}", text(&r));
    assert!(fake.calls_to("libp2p_module.createXpr").is_empty());
}
