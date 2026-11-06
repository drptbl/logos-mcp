use super::*;
use crate::logosctl::tests::{FakeRunner, ok as out};

fn server(fake: Arc<FakeRunner>, files: Option<FilesRoot>) -> LogosServer {
    LogosServer::new(
        Logosctl::new(fake),
        Arc::new(EventStore::default()),
        files,
        NodeSettings {
            preset: "logos.dev".into(),
            mode: "Edge".into(),
            config_dir: None,
            wallet: None,
        },
    )
}

/// A server configured with the agent wallet at fixed (fake) paths.
fn wallet_server(fake: Arc<FakeRunner>) -> LogosServer {
    LogosServer::new(
        Logosctl::new(fake),
        Arc::new(EventStore::default()),
        None,
        NodeSettings {
            preset: "logos.test".into(),
            mode: "Edge".into(),
            config_dir: None,
            wallet: Some(WalletPaths {
                config: "/agent/wallet_config.json".into(),
                storage: "/agent/storage.json".into(),
            }),
        },
    )
}

fn is_error(r: &CallToolResult) -> bool {
    r.is_error == Some(true)
}

#[tokio::test]
async fn send_rejects_bad_input_without_touching_the_node() {
    let fake = FakeRunner::with(vec![]);
    let s = server(fake.clone(), None);
    for p in [
        SendParams {
            topic: "no-slash".into(),
            text: Some("hi".into()),
            base64: None,
        },
        SendParams {
            topic: "/a/1/b/proto".into(),
            text: None,
            base64: None,
        },
        SendParams {
            topic: "/a/1/b/proto".into(),
            text: Some("x".into()),
            base64: Some("eA==".into()),
        },
        SendParams {
            topic: "/a/1/b/proto".into(),
            text: Some("x".repeat(guard::MAX_PAYLOAD_BYTES + 1)),
            base64: None,
        },
    ] {
        assert!(is_error(&s.messaging_send(Parameters(p)).await.unwrap()));
    }
    assert!(fake.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn send_passes_payload_as_tagged_bytes() {
    let fake = FakeRunner::with(vec![out(
        r#"{"method":"send","module":"delivery_module","result":{"error":null,"success":true,"value":"req-9"},"status":"ok"}"#,
    )]);
    let s = server(fake.clone(), None);
    let r = s
        .messaging_send(Parameters(SendParams {
            topic: "/a/1/b/proto".into(),
            text: Some("hi".into()),
            base64: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&r));
    assert_eq!(
        fake.calls.lock().unwrap()[0].last().unwrap(),
        r#"json:{"_bytes":"aGk"}"#
    );
}

#[tokio::test]
async fn connect_creates_node_only_when_uninitialized() {
    let fake = FakeRunner::with(vec![
        out(r#"[{"name":"delivery_module","status":"loaded"}]"#),
        out(
            r#"{"method":"getConnectionStatus","module":"delivery_module","result":{"error":"Context not initialized","success":false,"value":null},"status":"ok"}"#,
        ),
        out(
            r#"{"method":"createNode","module":"delivery_module","result":{"error":null,"success":true,"value":null},"status":"ok"}"#,
        ),
        out(
            r#"{"method":"start","module":"delivery_module","result":{"error":null,"success":true,"value":null},"status":"ok"}"#,
        ),
    ]);
    let s = server(fake.clone(), None);
    assert!(!is_error(&s.messaging_connect().await.unwrap()));
    {
        let calls = fake.calls.lock().unwrap();
        assert_eq!(calls[2][3], "createNode");
        assert_eq!(calls[2][4], r#"str:{"mode":"Edge","preset":"logos.dev"}"#);
    }

    let fake = FakeRunner::with(vec![
        out(r#"[{"name":"delivery_module","status":"loaded"}]"#),
        out(
            r#"{"method":"getConnectionStatus","module":"delivery_module","result":{"error":null,"success":true,"value":"Connected"},"status":"ok"}"#,
        ),
    ]);
    let s = server(fake.clone(), None);
    assert!(!is_error(&s.messaging_connect().await.unwrap()));
    assert_eq!(
        fake.calls.lock().unwrap().len(),
        2,
        "a connected node is left alone"
    );
}

#[tokio::test]
async fn storage_tools_refuse_without_files_root_or_outside_it() {
    let fake = FakeRunner::with(vec![]);
    let s = server(fake.clone(), None);
    let r = s
        .storage_upload(Parameters(UploadParams {
            path: "a.txt".into(),
        }))
        .await
        .unwrap();
    assert!(is_error(&r));

    let dir = tempfile::tempdir().unwrap();
    let s = server(fake.clone(), Some(FilesRoot::new(dir.path()).unwrap()));
    let r = s
        .storage_upload(Parameters(UploadParams {
            path: "../../etc/passwd".into(),
        }))
        .await
        .unwrap();
    assert!(is_error(&r));
    let r = s
        .storage_download(Parameters(DownloadParams {
            cid: "zAbc".into(),
            path: "/tmp/x".into(),
            private: None,
        }))
        .await
        .unwrap();
    assert!(is_error(&r));
    assert!(fake.calls.lock().unwrap().is_empty());
}

/// The text of a tool result's first content block.
fn text(r: &CallToolResult) -> String {
    let v = serde_json::to_value(r).unwrap();
    v["content"][0]["text"].as_str().unwrap().to_string()
}

/// The JSON body of a successful tool result.
fn body(r: &CallToolResult) -> Value {
    serde_json::from_str(&text(r)).unwrap()
}

#[tokio::test]
async fn rln_status_explains_an_unreadable_quota() {
    let bad = r#"{"method":"get_epoch_quota","module":"liblogos_rln_module","result":{"error":"{\"class\":\"permanent\",\"kind\":\"invalid_argument\",\"message\":\"timestamp is outside the acceptable epoch window (now ± max_epoch_gap)\"}","success":false,"value":null},"status":"ok"}"#;
    let fake = FakeRunner::with(vec![out(RLN_READY), out(MEMBERSHIP_ACTIVE), out(bad)]);
    let b = body(&server(fake, None).rln_status().await.unwrap());
    assert_eq!(b["can_send"], false);
    let reason = b["reason"].as_str().unwrap();
    assert!(reason.contains("epoch window"), "{reason}");
}

// rlnState as delivery_module 0.3.3 reports it on logos.dev.
const RLN_DISABLED: &str = r#"{"method":"rlnState","module":"delivery_module","result":{"error":null,"success":true,"value":{"message":"","state":"Disabled"}},"status":"ok"}"#;

#[tokio::test]
async fn rln_status_on_a_network_without_rln_allows_sending() {
    let fake = FakeRunner::with(vec![out(RLN_DISABLED)]);
    let r = server(fake.clone(), None).rln_status().await.unwrap();
    assert!(!is_error(&r));
    let b = body(&r);
    assert_eq!(b["rln"], "off");
    assert_eq!(b["can_send"], true);
    assert_eq!(
        fake.calls.lock().unwrap().len(),
        1,
        "no membership lookups when RLN is off"
    );
}

// Recorded from the logos.test node on 2026-11-06 after the membership went active.
const RLN_READY: &str = r#"{"method":"rlnState","module":"delivery_module","result":{"error":null,"success":true,"value":{"epochSizeSec":600,"message":"","registryId":"logos:testnet:fd322d32a46ddf57eece9db8d68e9b722262d1f770760fca63b8cb9e7775b971","rlnIdentifier":"5e269b6a19fce081f5808b13442dcbc3522197638dd38df5a28bc4e55236b977","state":"Ready"}},"status":"ok"}"#;
const MEMBERSHIP_ACTIVE: &str = r#"{"method":"get_membership_state","module":"liblogos_rln_module","result":"{\"leaf_index\":17,\"membership_hash\":\"785ea26ffa6bb73408575f90ad3baf4548c72774d8d8ca9ec1697f093e4490b3\",\"rate_limit\":100,\"registry_id\":\"logos:testnet:fd322d32a46ddf57eece9db8d68e9b722262d1f770760fca63b8cb9e7775b971\",\"state\":\"active\"}","status":"ok"}"#;
const QUOTA_99: &str = r#"{"method":"get_epoch_quota","module":"liblogos_rln_module","result":{"error":null,"success":true,"value":{"epoch_index":2989975,"rate_limit":100,"remaining":99}},"status":"ok"}"#;

#[tokio::test]
async fn rln_status_reports_active_membership_and_remaining_quota() {
    let fake = FakeRunner::with(vec![out(RLN_READY), out(MEMBERSHIP_ACTIVE), out(QUOTA_99)]);
    let r = server(fake.clone(), None).rln_status().await.unwrap();
    assert!(!is_error(&r));
    let b = body(&r);
    assert_eq!(b["rln"], "on");
    assert_eq!(b["can_send"], true);
    assert_eq!(b["membership"]["state"], "active");
    assert_eq!(b["membership"]["rate_limit"], 100);
    assert_eq!(b["quota"]["remaining"], 99);
    assert_eq!(b["epoch_seconds"], 600);
    let calls = fake.calls.lock().unwrap();
    let scope = [
        "str:logos:testnet:fd322d32a46ddf57eece9db8d68e9b722262d1f770760fca63b8cb9e7775b971",
        "str:5e269b6a19fce081f5808b13442dcbc3522197638dd38df5a28bc4e55236b977",
    ];
    assert_eq!(
        calls[1][2..],
        [
            "liblogos_rln_module",
            "get_membership_state",
            scope[0],
            scope[1]
        ]
    );
    assert_eq!(
        calls[2][2..5],
        ["liblogos_rln_module", "get_epoch_quota", scope[0]]
    );
    let ts: u64 = calls[2][6].strip_prefix("str:").unwrap().parse().unwrap();
    assert!(
        ts > 1_700_000_000,
        "quota is asked for the current unix time"
    );
}

#[tokio::test]
async fn rln_status_explains_a_membership_still_waiting_for_funds() {
    // Recorded before the payer was funded.
    let waiting = r#"{"method":"get_membership_state","module":"liblogos_rln_module","result":"{\"provisioning\":{\"detail\":\"846d needs 182800000 native (1000000 price + 181800000 fee reserve); it holds 100000000\",\"step\":\"awaiting_funding\"},\"registry_id\":\"logos:testnet:fd32\",\"state\":\"unknown\"}","status":"ok"}"#;
    let no_quota = r#"{"method":"get_epoch_quota","module":"liblogos_rln_module","result":{"error":null,"success":true,"value":{"epoch_index":2989975,"rate_limit":0,"remaining":0}},"status":"ok"}"#;
    let fake = FakeRunner::with(vec![out(RLN_READY), out(waiting), out(no_quota)]);
    let b = body(&server(fake, None).rln_status().await.unwrap());
    assert_eq!(b["can_send"], false);
    let reason = b["reason"].as_str().unwrap();
    assert!(reason.starts_with("membership is unknown"), "{reason}");
    assert!(
        reason.contains("it holds 100000000"),
        "the funding detail reaches the agent: {reason}"
    );
}

#[tokio::test]
async fn rln_status_reports_a_used_up_epoch() {
    let spent = QUOTA_99.replace(r#""remaining":99"#, r#""remaining":0"#);
    let fake = FakeRunner::with(vec![out(RLN_READY), out(MEMBERSHIP_ACTIVE), out(&spent)]);
    let b = body(&server(fake, None).rln_status().await.unwrap());
    assert_eq!(b["can_send"], false);
    assert!(b["reason"].as_str().unwrap().contains("next epoch"));
}

#[tokio::test]
async fn rln_status_before_the_node_exists_points_to_connect() {
    let uninit = r#"{"method":"rlnState","module":"delivery_module","result":{"error":"Context not initialized","success":false,"value":null},"status":"ok"}"#;
    let fake = FakeRunner::with(vec![out(uninit)]);
    let r = server(fake, None).rln_status().await.unwrap();
    assert!(is_error(&r));
    let msg = text(&r);
    assert!(msg.contains("messaging_connect"), "{msg}");
}

#[tokio::test]
async fn wallet_tools_are_off_without_a_configured_wallet() {
    let fake = FakeRunner::with(vec![]);
    let s = server(fake.clone(), None);
    for r in [
        s.wallet_status().await.unwrap(),
        s.wallet_accounts().await.unwrap(),
    ] {
        assert!(is_error(&r));
        assert!(text(&r).contains("--wallet-config"), "{}", text(&r));
    }
    assert!(fake.calls.lock().unwrap().is_empty());
}

fn lez(method: &str, result: &str) -> crate::logosctl::RawOutput {
    out(&format!(
        r#"{{"method":"{method}","module":"lez_core","result":{result},"status":"ok"}}"#
    ))
}

#[tokio::test]
async fn wallet_status_opens_the_configured_wallet_when_none_is_open() {
    let fake = FakeRunner::with(vec![
        lez("get_sequencer_addr", r#""""#),
        lez("open", "0"),
        lez("get_sequencer_addr", r#""https://testnet.lez.logos.co/""#),
        lez("list_accounts", AGENT_ACCOUNTS),
        lez("get_current_block_height", "78065"),
    ]);
    let r = wallet_server(fake.clone()).wallet_status().await.unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    let b = body(&r);
    assert_eq!(b["open"], true);
    assert_eq!(b["opened_now"], true);
    assert_eq!(b["sequencer"], "https://testnet.lez.logos.co/");
    assert_eq!(b["block_height"], 78065);
    let calls = fake.calls.lock().unwrap();
    assert_eq!(
        calls[1][2..],
        [
            "lez_core",
            "open",
            "str:/agent/wallet_config.json",
            "str:/agent/storage.json"
        ]
    );
}

#[tokio::test]
async fn wallet_accounts_lists_addresses_and_balances() {
    let fake = FakeRunner::with(vec![
        lez("get_sequencer_addr", r#""https://testnet.lez.logos.co/""#),
        lez(
            "list_accounts",
            r#"[{"account_id":"e9ce8b81376428e3fb1a8192026027ac25dc68bfafb100c82e6107f068c8213f","is_public":true},{"account_id":"8126abb4eb9f0a26919bf51a57244a6b7d3a39811e6393c1885f9e07aa32fec1","is_public":false}]"#,
        ),
        lez("get_balance", r#""250""#),
        // lez_core reports a failed balance read as an empty string.
        lez("get_balance", r#""""#),
    ]);
    let r = wallet_server(fake.clone()).wallet_accounts().await.unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    let accounts = body(&r)["accounts"].clone();
    assert_eq!(
        accounts[0]["address"],
        "Public/GjgfTEYoYVFjomLH7zRzFHHGpLNEwD4iGtf53JhwSdMY"
    );
    assert_eq!(accounts[0]["balance_lepta"], "250");
    assert_eq!(
        accounts[1]["address"],
        "Private/9h9o2fwbcu7Mbuq8hzVf1UVDpMQpCdCr5ZnNVo3vTsjN"
    );
    assert_eq!(accounts[1]["balance_lepta"], Value::Null);
    assert!(
        accounts[1]["note"]
            .as_str()
            .unwrap()
            .contains("unavailable")
    );
    let calls = fake.calls.lock().unwrap();
    assert_eq!(
        calls[2][4..],
        [
            "str:e9ce8b81376428e3fb1a8192026027ac25dc68bfafb100c82e6107f068c8213f",
            "true"
        ]
    );
    assert_eq!(calls[3][5], "false", "private balances are read as private");
    assert_eq!(calls.len(), 4, "addresses are encoded locally");
    assert!(
        calls.iter().all(|c| c[3] != "open"),
        "an already open wallet is not reopened"
    );
}

#[tokio::test]
async fn wallet_tools_report_a_wallet_that_will_not_open() {
    let fake = FakeRunner::with(vec![lez("get_sequencer_addr", r#""""#), lez("open", "1")]);
    let r = wallet_server(fake).wallet_accounts().await.unwrap();
    assert!(is_error(&r));
    assert!(
        text(&r).contains("could not open the agent wallet"),
        "{}",
        text(&r)
    );
}

/// A lez_core stand-in that keeps the "is a wallet open" state like the real
/// module and yields before every reply, so concurrent tool calls interleave.
#[derive(Default)]
struct StatefulLez {
    open: std::sync::atomic::AtomicBool,
    opens: std::sync::atomic::AtomicUsize,
}

impl crate::logosctl::Runner for StatefulLez {
    fn run(&self, args: Vec<String>) -> crate::logosctl::RunFuture<'_> {
        use std::sync::atomic::Ordering::SeqCst;
        Box::pin(async move {
            tokio::task::yield_now().await;
            let result = match args[3].as_str() {
                "get_sequencer_addr" if self.open.load(SeqCst) => {
                    r#""https://testnet.lez.logos.co/""#
                }
                "get_sequencer_addr" => r#""""#,
                "open" => {
                    self.opens.fetch_add(1, SeqCst);
                    // The real module refuses a second open with INTERNAL_ERROR.
                    if self.open.swap(true, SeqCst) {
                        "1"
                    } else {
                        "0"
                    }
                }
                "get_current_block_height" => "78065",
                "list_accounts" => AGENT_ACCOUNTS,
                other => panic!("unexpected call {other}"),
            };
            Ok(lez(&args[3], result))
        })
    }
}

#[tokio::test]
async fn concurrent_wallet_calls_open_the_wallet_once() {
    let lez_core = Arc::new(StatefulLez::default());
    let s = LogosServer::new(
        Logosctl::new(lez_core.clone()),
        Arc::new(EventStore::default()),
        None,
        wallet_server(FakeRunner::with(vec![])).settings.clone(),
    );
    let (a, b) = tokio::join!(s.wallet_status(), s.wallet_status());
    for r in [a.unwrap(), b.unwrap()] {
        assert!(!is_error(&r), "{}", text(&r));
    }
    assert_eq!(lez_core.opens.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn wallet_status_warns_about_a_wallet_opened_elsewhere() {
    let fake = FakeRunner::with(vec![
        lez("get_sequencer_addr", r#""https://testnet.lez.logos.co/""#),
        lez("list_accounts", AGENT_ACCOUNTS),
        lez("get_current_block_height", "78065"),
    ]);
    let b = body(&wallet_server(fake).wallet_status().await.unwrap());
    assert_eq!(b["opened_by_this_server"], false);
    assert!(
        b["warning"]
            .as_str()
            .unwrap()
            .contains("may not be the agent wallet")
    );
}

const AGENT_ACCOUNTS: &str = r#"[{"account_id":"e9ce8b81376428e3fb1a8192026027ac25dc68bfafb100c82e6107f068c8213f","is_public":true}]"#;
const OTHER_ACCOUNTS: &str = r#"[{"account_id":"846d9a7014dbd1fd26ffc40e870df5af08f23ee2bf0c151bc2aa17a58b4481bc","is_public":true}]"#;

#[tokio::test]
async fn a_wallet_swapped_after_opening_is_flagged_in_both_tools() {
    let fake = FakeRunner::with(vec![
        // first call: nothing open, the server opens the agent wallet and remembers its accounts
        lez("get_sequencer_addr", r#""""#),
        lez("open", "0"),
        lez("get_sequencer_addr", r#""https://testnet.lez.logos.co/""#),
        lez("list_accounts", AGENT_ACCOUNTS),
        lez("get_current_block_height", "78065"),
        // later the daemon restarted and someone else opened a different wallet
        lez("get_sequencer_addr", r#""https://testnet.lez.logos.co/""#),
        lez("list_accounts", OTHER_ACCOUNTS),
        lez("get_balance", r#""95212392""#),
    ]);
    let s = wallet_server(fake);
    let first = body(&s.wallet_status().await.unwrap());
    assert_eq!(first["opened_by_this_server"], true);
    assert_eq!(first["warning"], Value::Null);
    let later = body(&s.wallet_accounts().await.unwrap());
    assert!(
        later["warning"]
            .as_str()
            .unwrap()
            .contains("may not be the agent wallet"),
        "{later}"
    );
}

#[tokio::test]
async fn zero_height_and_empty_account_lists_are_not_taken_as_data() {
    let fake = FakeRunner::with(vec![
        lez("get_sequencer_addr", r#""https://testnet.lez.logos.co/""#),
        lez("list_accounts", "[]"),
        lez("get_current_block_height", "0"),
        lez("get_sequencer_addr", r#""https://testnet.lez.logos.co/""#),
        lez("list_accounts", "[]"),
    ]);
    let s = wallet_server(fake);
    let status = body(&s.wallet_status().await.unwrap());
    assert_eq!(
        status["block_height"],
        Value::Null,
        "lez_core returns 0 when it cannot read the height"
    );
    let accounts = body(&s.wallet_accounts().await.unwrap());
    assert!(
        accounts["note"]
            .as_str()
            .unwrap()
            .contains("listing failed"),
        "{accounts}"
    );
}
