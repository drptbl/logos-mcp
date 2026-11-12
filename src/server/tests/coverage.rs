//! The read-only tools that round out node coverage: wallet accounts, labels
//! and transactions, the Logos chain, RLN memberships, chat health and peers.

use super::*;

/// The replies `ensure_wallet` reads when it opens the agent wallet, with the
/// sequencer at `sequencer`.
fn opened_with_sequencer(sequencer: &str) -> Vec<crate::logosctl::RawOutput> {
    vec![
        lez("get_sequencer_addr", r#""""#),
        daemon_status(80675, 300),
        lez("open", "0"),
        lez("get_sequencer_addr", &format!("{sequencer:?}")),
        lez("list_accounts", AGENT_ACCOUNTS),
        daemon_status(80675, 301),
    ]
}

const PROGRAM_HEX: &str = "f9eba477278c918fadafbcb4a6463e947d0fd8de344b5431ee0aa3cd41d7e3a4";

fn last_call(fake: &FakeRunner) -> Vec<String> {
    fake.calls.lock().unwrap().last().unwrap()[2..].to_vec()
}

#[tokio::test]
async fn an_account_shows_its_balance_nonce_and_program_data() {
    let mut replies = opened_by_server();
    let account = json!({
        "nonce": "01000000000000000000000000000000",
        "shards": [{ "program": PROGRAM_HEX, "data": "0a0b" }]
    })
    .to_string();
    replies.push(lez(
        "get_account_public",
        &serde_json::to_string(&account).unwrap(),
    ));
    replies.push(lez("get_balance", r#""250""#));
    let fake = FakeRunner::with(replies);
    let r = wallet_server(fake.clone())
        .wallet_account(Parameters(AccountParams {
            account: AGENT_FROM.into(),
        }))
        .await
        .unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    let b = body(&r);
    assert_eq!(b["address"], AGENT_FROM, "{b}");
    assert_eq!(b["account_id"], AGENT_ID, "{b}");
    assert_eq!(b["balance_lepta"], "250", "{b}");
    assert_eq!(b["nonce"], 1, "{b}");
    assert_eq!(b["programs"][0]["program_id"], PROGRAM_HEX, "{b}");
    assert_eq!(
        b["programs"][0]["program"], "HpavNGFYxjyhLsy171aXsvv146yUeXmRWiho22uvT43d",
        "{b}"
    );
    assert_eq!(b["programs"][0]["data_hex"], "0a0b", "{b}");
}

#[tokio::test]
async fn a_malformed_account_is_refused_without_touching_the_node() {
    let fake = FakeRunner::with(vec![]);
    let r = wallet_server(fake.clone())
        .wallet_account(Parameters(AccountParams {
            account: "not an account".into(),
        }))
        .await
        .unwrap();
    assert!(is_error(&r), "{}", text(&r));
    assert!(fake.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_label_resolves_to_its_account() {
    let mut replies = opened_by_server();
    // lez_core answers with the account's kind and its id in hex.
    replies.push(lez("resolve_label", &format!("\"Public/{AGENT_ID}\"")));
    let fake = FakeRunner::with(replies);
    let r = wallet_server(fake.clone())
        .wallet_labels(Parameters(LabelsParams {
            label: Some("alice".into()),
            account: None,
        }))
        .await
        .unwrap();
    let b = body(&r);
    assert_eq!(b["account"], AGENT_FROM, "{b}");
    assert_eq!(b["account_id"], AGENT_ID, "{b}");
    assert_eq!(b["private"], false, "{b}");
    assert_eq!(last_call(&fake)[2], "str:alice");
}

#[tokio::test]
async fn an_unknown_label_says_so() {
    let mut replies = opened_by_server();
    replies.push(lez("resolve_label", r#""""#));
    let r = wallet_server(FakeRunner::with(replies))
        .wallet_labels(Parameters(LabelsParams {
            label: Some("nobody".into()),
            account: None,
        }))
        .await
        .unwrap();
    let b = body(&r);
    assert_eq!(b["account"], Value::Null, "{b}");
}

#[tokio::test]
async fn an_account_lists_its_labels() {
    let mut replies = opened_by_server();
    replies.push(lez("get_all_labels_for_account", r#"["alice","shop"]"#));
    let fake = FakeRunner::with(replies);
    let r = wallet_server(fake.clone())
        .wallet_labels(Parameters(LabelsParams {
            label: None,
            account: Some(AGENT_FROM.into()),
        }))
        .await
        .unwrap();
    let b = body(&r);
    assert_eq!(b["labels"], json!(["alice", "shop"]), "{b}");
    let call = last_call(&fake);
    assert_eq!(call[2], format!("str:{AGENT_ID}"));
    assert_eq!(call[3], "false");
}

#[tokio::test]
async fn labels_take_a_label_or_an_account_not_both() {
    let fake = FakeRunner::with(vec![]);
    for (label, account) in [
        (None, None),
        (Some("a".to_string()), Some(AGENT_FROM.to_string())),
    ] {
        let r = wallet_server(fake.clone())
            .wallet_labels(Parameters(LabelsParams { label, account }))
            .await
            .unwrap();
        assert!(is_error(&r), "{}", text(&r));
    }
    assert!(fake.calls.lock().unwrap().is_empty());
}

/// A sequencer on localhost that answers one JSON-RPC call with `result`,
/// and hands back the request it saw.
async fn fake_sequencer(result: Value) -> (String, tokio::task::JoinHandle<String>) {
    fake_sequencer_reply(json!({ "jsonrpc": "2.0", "id": 1, "result": result })).await
}

/// A sequencer that answers its first request with `reply`, whole.
async fn fake_sequencer_reply(reply: Value) -> (String, tokio::task::JoinHandle<String>) {
    let reply = reply.to_string();
    fake_http(format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
        reply.len()
    ))
    .await
}

/// A server that answers its first request with the raw HTTP `response`.
async fn fake_http(response: String) -> (String, tokio::task::JoinHandle<String>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let served = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut seen = Vec::new();
        let mut buf = [0u8; 4096];
        // Read the headers and the JSON body after them.
        loop {
            let n = socket.read(&mut buf).await.unwrap();
            seen.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&seen).to_string();
            if let Some(end) = text.find("\r\n\r\n") {
                let length = text[..end]
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                if seen.len() >= end + 4 + length {
                    break;
                }
            }
            if n == 0 {
                break;
            }
        }
        socket.write_all(response.as_bytes()).await.unwrap();
        String::from_utf8_lossy(&seen).to_string()
    });
    (url, served)
}

const TX: &str = "1f0f6eafe1e058d5d657cd26972de2a0ea3de547eae66b758f6531a516deae1c";

#[tokio::test]
async fn a_landed_transaction_is_reported_included() {
    let (url, served) = fake_sequencer(json!(["AAEC"])).await;
    let s = wallet_server(FakeRunner::with(opened_with_sequencer(&url)));
    let r = s
        .wallet_tx_status(Parameters(TxParams { tx_hash: TX.into() }))
        .await
        .unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    assert_eq!(body(&r)["included"], true, "{}", text(&r));
    let request = served.await.unwrap();
    assert!(request.contains("getTransaction"), "{request}");
    assert!(request.contains(TX), "{request}");
}

#[tokio::test]
async fn a_transaction_not_landed_is_reported_not_included() {
    let (url, _served) = fake_sequencer(Value::Null).await;
    let s = wallet_server(FakeRunner::with(opened_with_sequencer(&url)));
    let r = s
        .wallet_tx_status(Parameters(TxParams { tx_hash: TX.into() }))
        .await
        .unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    assert_eq!(body(&r)["included"], false, "{}", text(&r));
}

#[tokio::test]
async fn a_reply_without_a_result_is_an_error_not_a_missing_transaction() {
    let (url, _served) = fake_sequencer_reply(json!({ "status": "ok" })).await;
    let s = wallet_server(FakeRunner::with(opened_with_sequencer(&url)));
    let r = s
        .wallet_tx_status(Parameters(TxParams { tx_hash: TX.into() }))
        .await
        .unwrap();
    assert!(is_error(&r), "{}", text(&r));
}

#[tokio::test]
async fn the_sequencer_cannot_redirect_the_lookup_elsewhere() {
    let (elsewhere, _) = fake_sequencer(json!(["AAEC"])).await;
    let (url, _served) = fake_http(format!(
        "HTTP/1.1 302 Found\r\nlocation: {elsewhere}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
    ))
    .await;
    let s = wallet_server(FakeRunner::with(opened_with_sequencer(&url)));
    let r = s
        .wallet_tx_status(Parameters(TxParams { tx_hash: TX.into() }))
        .await
        .unwrap();
    assert!(is_error(&r), "{}", text(&r));
}

#[tokio::test]
async fn sequencer_errors_keep_its_address_out() {
    // Nothing listens on port 1, and the address carries a credential.
    let url = "http://user:hunter2@127.0.0.1:1/";
    let s = wallet_server(FakeRunner::with(opened_with_sequencer(url)));
    let r = s
        .wallet_tx_status(Parameters(TxParams { tx_hash: TX.into() }))
        .await
        .unwrap();
    assert!(is_error(&r), "{}", text(&r));
    assert!(!text(&r).contains("hunter2"), "{}", text(&r));
}

#[tokio::test]
async fn a_malformed_tx_hash_is_refused_without_touching_the_node() {
    let fake = FakeRunner::with(vec![]);
    let r = wallet_server(fake.clone())
        .wallet_tx_status(Parameters(TxParams {
            tx_hash: "abc".into(),
        }))
        .await
        .unwrap();
    assert!(is_error(&r), "{}", text(&r));
    assert!(fake.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn wallet_status_shows_how_far_private_state_is_synced() {
    let mut replies = opened_by_server();
    replies.push(lez("get_current_block_height", "80205"));
    replies.push(lez("get_last_synced_block", "80100"));
    let r = wallet_server(FakeRunner::with(replies))
        .wallet_status()
        .await
        .unwrap();
    let b = body(&r);
    assert_eq!(b["block_height"], 80205, "{b}");
    assert_eq!(b["private_synced_block"], 80100, "{b}");
}

const BLOCKCHAIN_LOADED: &str = r#"[{"name":"blockchain_module","status":"loaded"}]"#;

/// A blockchain_module reply to `method`, wrapped as the module wraps results.
fn chain(method: &str, value: Value) -> crate::logosctl::RawOutput {
    out(&json!({
        "method": method,
        "module": "blockchain_module",
        "result": { "error": null, "success": true, "value": value },
        "status": "ok",
    })
    .to_string())
}

/// What blockchain_module answers while its node isn't running.
fn chain_not_running(method: &str) -> crate::logosctl::RawOutput {
    out(&json!({
        "method": method,
        "module": "blockchain_module",
        "result": { "error": "The node is not running.", "success": false, "value": null },
        "status": "ok",
    })
    .to_string())
}

#[tokio::test]
async fn chain_info_brings_the_chain_together() {
    let fake = FakeRunner::with(vec![
        out(BLOCKCHAIN_LOADED),
        chain("get_chain_id", json!("logos-devnet")),
        chain("get_cryptarchia_info", json!({ "tip": "ab", "height": 42 })),
        chain("get_time_info", json!({ "slot": 1000 })),
        chain("get_network_info", json!({ "peers": 7 })),
    ]);
    let b = body(&server(fake, None).chain_info().await.unwrap());
    assert_eq!(b["chain_id"], "logos-devnet", "{b}");
    assert_eq!(b["consensus"]["height"], 42, "{b}");
    assert_eq!(b["time"]["slot"], 1000, "{b}");
    assert_eq!(b["network"]["peers"], 7, "{b}");
}

#[tokio::test]
async fn chain_info_says_how_to_start_the_node() {
    let fake = FakeRunner::with(vec![
        out(BLOCKCHAIN_LOADED),
        chain_not_running("get_chain_id"),
    ]);
    let r = server(fake, None).chain_info().await.unwrap();
    assert!(is_error(&r), "{}", text(&r));
    assert!(text(&r).contains("start"), "{}", text(&r));
}

#[tokio::test]
async fn chain_info_says_how_to_install_the_module() {
    let fake = FakeRunner::with(vec![
        out(r#"[{"name":"chat_module","status":"loaded"}]"#),
        crate::logosctl::RawOutput {
            code: 3,
            stdout: r#"{"code":"MODULE_LOAD_FAILED","message":"Failed to load module 'blockchain_module'.","status":"error"}"#.into(),
            stderr: String::new(),
        },
    ]);
    let r = server(fake, None).chain_info().await.unwrap();
    assert!(is_error(&r), "{}", text(&r));
    assert!(
        text(&r).contains("package install blockchain_module"),
        "{}",
        text(&r)
    );
}

const HEADER: &str = "c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00";

#[tokio::test]
async fn a_block_is_read_by_its_header_id() {
    let fake = FakeRunner::with(vec![
        out(BLOCKCHAIN_LOADED),
        chain("get_block", json!({ "header": { "slot": 9 } })),
    ]);
    let b = body(
        &server(fake.clone(), None)
            .chain_block(Parameters(HeaderParams {
                header_id: HEADER.into(),
            }))
            .await
            .unwrap(),
    );
    assert_eq!(b["block"]["header"]["slot"], 9, "{b}");
    assert_eq!(last_call(&fake)[2], format!("str:{HEADER}"));
}

#[tokio::test]
async fn a_chain_transaction_is_read_by_its_hash() {
    let fake = FakeRunner::with(vec![
        out(BLOCKCHAIN_LOADED),
        chain("get_transaction", json!({ "ops": [] })),
    ]);
    let b = body(
        &server(fake.clone(), None)
            .chain_transaction(Parameters(ChainTxParams { tx_hash: TX.into() }))
            .await
            .unwrap(),
    );
    assert_eq!(b["transaction"]["ops"], json!([]), "{b}");
    assert_eq!(last_call(&fake)[2], format!("str:{TX}"));
}

#[tokio::test]
async fn malformed_chain_ids_are_refused_without_touching_the_node() {
    let fake = FakeRunner::with(vec![]);
    let s = server(fake.clone(), None);
    let r = s
        .chain_block(Parameters(HeaderParams {
            header_id: "zz".into(),
        }))
        .await
        .unwrap();
    assert!(is_error(&r), "{}", text(&r));
    let r = s
        .chain_transaction(Parameters(ChainTxParams {
            tx_hash: "zz".into(),
        }))
        .await
        .unwrap();
    assert!(is_error(&r), "{}", text(&r));
    assert!(fake.calls.lock().unwrap().is_empty());
}

#[test]
fn the_chain_tools_have_their_own_switch() {
    let mut groups = ToolGroups::default();
    assert!(groups.allows("chain_info"));
    groups.set("chain", false).unwrap();
    for tool in ["chain_info", "chain_block", "chain_transaction"] {
        assert!(!groups.allows(tool), "{tool}");
    }
    assert!(groups.allows("wallet_status"));
}

// get_memberships as liblogos_rln_module 0.12.0 answered it on logos.test.
const MEMBERSHIPS: &str = r#"{"method":"get_memberships","module":"liblogos_rln_module","result":"{\"memberships\":[{\"credential\":{\"identity_commitment\":\"e7a1\"},\"leaf_index\":17,\"membership_hash\":\"785e\",\"rate_limit\":100,\"registry_id\":\"logos:testnet:fd32\",\"rln_identifier\":\"5e26\",\"state\":\"active\",\"submitted_at\":1793984466,\"tx_result\":\"{}\"}]}","status":"ok"}"#;

#[tokio::test]
async fn rln_status_lists_the_memberships_of_the_registry() {
    let fake = FakeRunner::with(vec![
        out(RLN_READY),
        out(MEMBERSHIP_ACTIVE),
        out(QUOTA_99),
        out(MEMBERSHIPS),
    ]);
    let b = body(&server(fake.clone(), None).rln_status().await.unwrap());
    let m = &b["memberships"][0];
    assert_eq!(m["state"], "active", "{b}");
    assert_eq!(m["leaf_index"], 17, "{b}");
    assert_eq!(m["rate_limit"], 100, "{b}");
    assert!(m.get("credential").is_none(), "{b}");
    assert!(m.get("tx_result").is_none(), "{b}");
    assert_eq!(
        last_call_named(&fake, "get_memberships")[2],
        "str:logos:testnet:fd322d32a46ddf57eece9db8d68e9b722262d1f770760fca63b8cb9e7775b971"
    );
}

#[tokio::test]
async fn a_null_error_beside_the_memberships_is_no_error() {
    let with_null =
        MEMBERSHIPS.replace(r#"{\"memberships\""#, r#"{\"error\":null,\"memberships\""#);
    assert_ne!(with_null, MEMBERSHIPS);
    let fake = FakeRunner::with(vec![
        out(RLN_READY),
        out(MEMBERSHIP_ACTIVE),
        out(QUOTA_99),
        out(&with_null),
    ]);
    let b = body(&server(fake, None).rln_status().await.unwrap());
    assert_eq!(b["memberships"][0]["leaf_index"], 17, "{b}");
}

#[tokio::test]
async fn an_unexpected_membership_reply_keeps_the_credential_out() {
    let odd = MEMBERSHIPS.replace(r#"{\"memberships\":["#, r#"{\"entries\":["#);
    assert_ne!(odd, MEMBERSHIPS);
    let fake = FakeRunner::with(vec![
        out(RLN_READY),
        out(MEMBERSHIP_ACTIVE),
        out(QUOTA_99),
        out(&odd),
    ]);
    let b = body(&server(fake, None).rln_status().await.unwrap());
    assert!(b["memberships_error"].is_string(), "{b}");
    assert!(!b.to_string().contains("identity_commitment"), "{b}");
}

#[tokio::test]
async fn an_unreadable_membership_list_leaves_rln_status_standing() {
    let bad = r#"{"method":"get_memberships","module":"liblogos_rln_module","result":"{\"error\":{\"class\":\"permanent\",\"kind\":\"invalid_argument\",\"message\":\"bad registry\"}}","status":"ok"}"#;
    let fake = FakeRunner::with(vec![
        out(RLN_READY),
        out(MEMBERSHIP_ACTIVE),
        out(QUOTA_99),
        out(bad),
    ]);
    let r = server(fake, None).rln_status().await.unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    let b = body(&r);
    assert_eq!(b["can_send"], true, "{b}");
    assert!(
        b["memberships_error"]
            .as_str()
            .unwrap()
            .contains("bad registry"),
        "{b}"
    );
}

#[tokio::test]
async fn chat_status_says_whether_chat_is_healthy() {
    let fake = FakeRunner::with(vec![
        out(CHAT_LOADED),
        chat("get_address", &format!(r#""{SELF_ADDR}""#)),
        chat("status", CHAT_ONLINE),
        node_enr(ENR_LOGOS_TEST),
        chat("health", "true"),
    ]);
    let b = body(&server(fake, None).chat_status().await.unwrap());
    assert_eq!(b["healthy"], true, "{b}");
}

fn last_call_named(fake: &FakeRunner, method: &str) -> Vec<String> {
    fake.calls
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|c| c.get(3).map(String::as_str) == Some(method))
        .expect("called")[2..]
        .to_vec()
}

#[tokio::test]
async fn discovery_peers_lists_inbound_and_outbound_connections() {
    let fake = FakeRunner::with(vec![
        p2p("peerInfo", PEER_INFO),
        p2p("start", "null"),
        p2p("discoStart", "null"),
        p2p(
            "connectedPeers",
            r#"["12D3KooWR2tcuzVsLXZ9K26L8oTpr55b5bFQa13VVad9cn9huXzW"]"#,
        ),
        p2p(
            "connectedPeers",
            r#"["16Uiu2HAm1YeKNp72mcWY3GD2FdkGLk85XVBHMpuGooPNEuzW5Ajc","16Uiu2HAmGBJ6wbRC2QGJreX3bA25g5yhMFajNVVKM9483CgcPkSu"]"#,
        ),
    ]);
    let b = body(&wallet_server(fake.clone()).discovery_peers().await.unwrap());
    assert_eq!(
        b["peer_id"], "12D3KooWCkksEAUcgsSdPaznKfzcRtKgsEuXARXzog18fcNovQMh",
        "{b}"
    );
    assert_eq!(b["inbound"].as_array().unwrap().len(), 1, "{b}");
    assert_eq!(b["outbound"].as_array().unwrap().len(), 2, "{b}");
    let calls = fake.calls.lock().unwrap();
    // libp2p_module numbers the directions 0 for inbound and 1 for outbound.
    assert_eq!(calls[3][3..], ["connectedPeers", "0"]);
    assert_eq!(calls[4][3..], ["connectedPeers", "1"]);
}

#[tokio::test]
async fn discovery_peers_needs_bootstrap_peers_like_the_other_discovery_tools() {
    let fake = FakeRunner::with(vec![]);
    let r = server(fake.clone(), None).discovery_peers().await.unwrap();
    assert!(is_error(&r));
    assert!(text(&r).contains("--discovery-bootstrap"), "{}", text(&r));
    assert!(fake.calls.lock().unwrap().is_empty());
}

/// What blockchain_module 0.3.4 answers for an id it doesn't have, as seen live.
fn chain_refused(method: &str, error: &str) -> crate::logosctl::RawOutput {
    out(&json!({
        "method": method,
        "module": "blockchain_module",
        "result": { "error": error, "success": false, "value": null },
        "status": "ok",
    })
    .to_string())
}

#[tokio::test]
async fn a_transaction_the_node_no_longer_holds_points_to_its_block() {
    let fake = FakeRunner::with(vec![
        out(BLOCKCHAIN_LOADED),
        chain_refused(
            "get_transaction",
            "No transaction found for hash TxHash([31, 15, 110])",
        ),
    ]);
    let r = server(fake, None)
        .chain_transaction(Parameters(ChainTxParams { tx_hash: TX.into() }))
        .await
        .unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    let b = body(&r);
    assert_eq!(b["found"], false, "{b}");
    assert!(b["note"].as_str().unwrap().contains("chain_block"), "{b}");
}

#[tokio::test]
async fn a_block_the_node_lacks_is_not_found_rather_than_failed() {
    let fake = FakeRunner::with(vec![
        out(BLOCKCHAIN_LOADED),
        chain_refused("get_block", "No block found for header id [110, 130, 165]"),
    ]);
    let r = server(fake, None)
        .chain_block(Parameters(HeaderParams {
            header_id: HEADER.into(),
        }))
        .await
        .unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    let b = body(&r);
    assert_eq!(b["found"], false, "{b}");
    assert!(b["note"].as_str().unwrap().contains("chain_info"), "{b}");
}

#[tokio::test]
async fn wallet_status_keeps_sequencer_credentials_out() {
    let mut replies = opened_with_sequencer("https://user:hunter2@seq.example/rpc?key=k");
    replies.push(lez("get_current_block_height", "80205"));
    replies.push(lez("get_last_synced_block", "80100"));
    let b = body(
        &wallet_server(FakeRunner::with(replies))
            .wallet_status()
            .await
            .unwrap(),
    );
    assert_eq!(b["sequencer"], "https://seq.example/rpc", "{b}");
}

#[tokio::test]
async fn chain_blocks_reads_a_slot_range() {
    let fake = FakeRunner::with(vec![
        out(BLOCKCHAIN_LOADED),
        chain(
            "get_blocks",
            json!("[{\"header\":{\"slot\":101}},{\"header\":{\"slot\":105}}]"),
        ),
    ]);
    let b = body(
        &server(fake.clone(), None)
            .chain_blocks(Parameters(SlotRangeParams {
                from_slot: 100,
                to_slot: 110,
            }))
            .await
            .unwrap(),
    );
    assert_eq!(b["blocks"].as_array().unwrap().len(), 2, "{b}");
    assert_eq!(b["blocks"][1]["header"]["slot"], 105, "{b}");
    assert_eq!(last_call(&fake)[1..], ["get_blocks", "100", "110"]);
}

#[tokio::test]
async fn chain_blocks_refuses_a_backwards_or_huge_range_without_touching_the_node() {
    let fake = FakeRunner::with(vec![]);
    let s = server(fake.clone(), None);
    for (from_slot, to_slot) in [(10, 9), (0, MAX_SLOT_RANGE + 1), (u64::MAX - 1, u64::MAX)] {
        let r = s
            .chain_blocks(Parameters(SlotRangeParams { from_slot, to_slot }))
            .await
            .unwrap();
        assert!(is_error(&r), "{from_slot}..{to_slot}: {}", text(&r));
    }
    assert!(fake.calls.lock().unwrap().is_empty());
}

const CHAIN_KEY_A: &str = "3e2acc3dacb83036f111df8fa65df19d897a7022aa42fbd51dc52d8f79117e09";
const CHAIN_KEY_B: &str = "84f956ec0d63f5f7dac2eb85171c0b64fa2191d16d96515ba6f16ec89c78c61e";

#[tokio::test]
async fn chain_wallet_lists_the_node_accounts_with_balances() {
    let fake = FakeRunner::with(vec![
        out(BLOCKCHAIN_LOADED),
        // As blockchain_module 0.3.4 answers: a JSON string.
        chain(
            "wallet_get_known_addresses",
            json!(format!("[\"{CHAIN_KEY_A}\",\"{CHAIN_KEY_B}\"]")),
        ),
        chain("wallet_get_balance", json!("1500")),
        chain_refused(
            "wallet_get_balance",
            "Failed to get balance: Ledger state corresponding to block ab not found",
        ),
    ]);
    let b = body(
        &server(fake, None)
            .chain_wallet(Parameters(ChainWalletParams { address: None }))
            .await
            .unwrap(),
    );
    let accounts = b["accounts"].as_array().unwrap();
    assert_eq!(accounts.len(), 2, "{b}");
    assert_eq!(accounts[0]["address"], CHAIN_KEY_A, "{b}");
    assert_eq!(accounts[0]["balance"], "1500", "{b}");
    assert!(accounts[1]["balance"].is_null(), "{b}");
    assert!(
        accounts[1]["error"]
            .as_str()
            .unwrap()
            .contains("Ledger state"),
        "{b}"
    );
}

#[tokio::test]
async fn chain_wallet_shows_one_account_with_its_notes() {
    let fake = FakeRunner::with(vec![
        out(BLOCKCHAIN_LOADED),
        chain(
            "wallet_get_known_addresses",
            json!(format!("[\"{CHAIN_KEY_A}\"]")),
        ),
        chain("wallet_get_balance", json!("1500")),
        chain(
            "wallet_get_notes",
            json!("[{\"id\":\"ab\",\"value\":1500}]"),
        ),
    ]);
    let b = body(
        &server(fake.clone(), None)
            .chain_wallet(Parameters(ChainWalletParams {
                address: Some(CHAIN_KEY_A.to_uppercase()),
            }))
            .await
            .unwrap(),
    );
    assert_eq!(b["address"], CHAIN_KEY_A, "{b}");
    assert_eq!(b["notes"][0]["value"], 1500, "{b}");
    assert_eq!(
        last_call(&fake)[1..],
        ["wallet_get_notes", &format!("str:{CHAIN_KEY_A}"), "str:"]
    );
}

const OTHER_PEER: &str =
    "/ip4/138.68.122.137/tcp/30304/p2p/16Uiu2HAm1YeKNp72mcWY3GD2FdkGLk85XVBHMpuGooPNEuzW5Ajc";
const OTHER_PEER_ID: &str = "16Uiu2HAm1YeKNp72mcWY3GD2FdkGLk85XVBHMpuGooPNEuzW5Ajc";

/// The replies ensure_discovery reads for a node that already runs.
fn discovery_running() -> Vec<crate::logosctl::RawOutput> {
    vec![
        p2p("peerInfo", PEER_INFO),
        p2p("start", "null"),
        p2p("discoStart", "null"),
    ]
}

#[tokio::test]
async fn discovery_connect_dials_a_peer_by_its_multiaddr() {
    let mut replies = discovery_running();
    replies.push(p2p("connectPeer", "null"));
    let fake = FakeRunner::with(replies);
    let r = wallet_server(fake.clone())
        .discovery_connect(Parameters(PeerAddrParams {
            multiaddr: OTHER_PEER.into(),
        }))
        .await
        .unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    assert_eq!(body(&r)["connected"], OTHER_PEER_ID);
    assert_eq!(
        last_call(&fake)[1..],
        [
            "connectPeer",
            &format!("str:{OTHER_PEER_ID}"),
            "json:[\"/ip4/138.68.122.137/tcp/30304\"]",
            "10000"
        ]
    );
}

#[tokio::test]
async fn discovery_connect_refuses_a_malformed_multiaddr_without_touching_the_node() {
    let fake = FakeRunner::with(vec![]);
    let r = wallet_server(fake.clone())
        .discovery_connect(Parameters(PeerAddrParams {
            multiaddr: "/ip4/1.2.3.4/tcp/1".into(),
        }))
        .await
        .unwrap();
    assert!(is_error(&r), "{}", text(&r));
    assert!(fake.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn discovery_ping_reports_the_round_trip() {
    let mut replies = discovery_running();
    replies.push(p2p(
        "pingPeer",
        &format!(r#"{{"peerId":"{OTHER_PEER_ID}","rttMs":36.4}}"#),
    ));
    let fake = FakeRunner::with(replies);
    let b = body(
        &wallet_server(fake.clone())
            .discovery_ping(Parameters(PeerIdParams {
                peer_id: OTHER_PEER_ID.into(),
            }))
            .await
            .unwrap(),
    );
    assert_eq!(b["rtt_ms"], 36.4, "{b}");
    assert_eq!(
        last_call(&fake)[1..],
        ["pingPeer", &format!("str:{OTHER_PEER_ID}"), "5000"]
    );
}

#[tokio::test]
async fn discovery_disconnect_drops_a_peer() {
    let mut replies = discovery_running();
    replies.push(p2p("disconnectPeer", "null"));
    let fake = FakeRunner::with(replies);
    let r = wallet_server(fake.clone())
        .discovery_disconnect(Parameters(PeerIdParams {
            peer_id: OTHER_PEER_ID.into(),
        }))
        .await
        .unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    assert_eq!(body(&r)["disconnected"], OTHER_PEER_ID);
}

#[tokio::test]
async fn peer_tools_refuse_a_malformed_peer_id_without_touching_the_node() {
    let fake = FakeRunner::with(vec![]);
    let s = wallet_server(fake.clone());
    let bad = || {
        Parameters(PeerIdParams {
            peer_id: "not a peer".into(),
        })
    };
    assert!(is_error(&s.discovery_ping(bad()).await.unwrap()));
    assert!(is_error(&s.discovery_disconnect(bad()).await.unwrap()));
    assert!(fake.calls.lock().unwrap().is_empty());
}

const AGENT_ID: &str = "e9ce8b81376428e3fb1a8192026027ac25dc68bfafb100c82e6107f068c8213f";
const FAUCET_LOADED: &str = r#"[{"name":"lez_faucet","status":"loaded"}]"#;

/// A lez_faucet reply: its methods return a JSON document as a string.
fn faucet(method: &str, envelope: Value) -> crate::logosctl::RawOutput {
    out(&json!({
        "method": method,
        "module": "lez_faucet",
        "result": envelope.to_string(),
        "status": "ok",
    })
    .to_string())
}

fn agent_address() -> String {
    format!("Public/{}", guard::account_id_base58(AGENT_ID).unwrap())
}

#[tokio::test]
async fn the_faucet_pays_an_agent_account_and_waits_for_the_receipt() {
    let mut replies = opened_by_server();
    replies.extend([
        out(FAUCET_LOADED),
        faucet_info_on(SEQUENCER_V03)[0].clone(),
        faucet_info_on(SEQUENCER_V03)[1].clone(),
        faucet_info_on(SEQUENCER_V03)[2].clone(),
        faucet(
            "requestDrop",
            json!({ "ok": true, "job_id": "job-1", "status": "queued" }),
        ),
        faucet(
            "jobStatus",
            json!({
                "ok": true, "job_id": "job-1", "status": "succeeded",
                "result": { "amount": "100000000", "balance_before": "0",
                            "balance_after": "100000000", "tx_hash": TX },
            }),
        ),
        faucet("jobResultAck", json!({ "ok": true })),
    ]);
    let fake = FakeRunner::with(replies);
    let r = wallet_server(fake.clone())
        .wallet_faucet(
            CancellationToken::new(),
            Parameters(FaucetParams {
                account: Some(agent_address()),
                job_id: None,
            }),
        )
        .await
        .unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    let b = body(&r);
    assert_eq!(b["status"], "succeeded", "{b}");
    assert_eq!(b["receipt"]["tx_hash"], TX, "{b}");
    let calls = fake.calls.lock().unwrap();
    // It already serves the wallet's network: the Faucet app's choice is kept.
    assert!(
        !calls
            .iter()
            .any(|c| c.get(3).map(String::as_str) == Some("configure"))
    );
    let drop = calls
        .iter()
        .find(|c| c.get(3).map(String::as_str) == Some("requestDrop"))
        .unwrap();
    assert_eq!(drop[4], format!("str:{}", agent_address()));
    // A fresh lowercase UUID, so a retry can never claim twice.
    let key = drop[5].strip_prefix("str:").unwrap();
    assert_eq!(key.len(), 36, "{key}");
    assert!(
        key.chars()
            .all(|c| c == '-' || c.is_ascii_digit() || c.is_ascii_lowercase()),
        "{key}"
    );
    assert!(
        calls
            .iter()
            .any(|c| c.get(3).map(String::as_str) == Some("jobResultAck"))
    );
}

#[tokio::test]
async fn the_faucet_never_pays_an_account_outside_the_agent_wallet() {
    let fake = FakeRunner::with(opened_by_server());
    let other = format!(
        "Public/{}",
        guard::account_id_base58(
            "846d9a7014dbd1fd26ffc40e870df5af08f23ee2bf0c151bc2aa17a58b4481bc"
        )
        .unwrap()
    );
    let r = wallet_server(fake.clone())
        .wallet_faucet(
            CancellationToken::new(),
            Parameters(FaucetParams {
                account: Some(other),
                job_id: None,
            }),
        )
        .await
        .unwrap();
    assert!(is_error(&r), "{}", text(&r));
    assert!(
        !fake
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.get(2).map(String::as_str) == Some("lez_faucet"))
    );
}

#[tokio::test]
async fn a_refused_drop_says_why() {
    let mut replies = opened_by_server();
    replies.extend([
        out(FAUCET_LOADED),
        faucet_info_on(SEQUENCER_V03)[0].clone(),
        faucet_info_on(SEQUENCER_V03)[1].clone(),
        faucet_info_on(SEQUENCER_V03)[2].clone(),
        faucet(
            "requestDrop",
            json!({ "ok": false, "error": { "code": "rate_limited",
                    "message": "The faucet service is limiting claims right now. Try again later." } }),
        ),
    ]);
    let r = wallet_server(FakeRunner::with(replies))
        .wallet_faucet(
            CancellationToken::new(),
            Parameters(FaucetParams {
                account: Some(agent_address()),
                job_id: None,
            }),
        )
        .await
        .unwrap();
    assert!(is_error(&r), "{}", text(&r));
    assert!(text(&r).contains("limiting claims"), "{}", text(&r));
}

#[tokio::test]
async fn a_running_drop_is_checked_again_by_its_job_id() {
    let running = || {
        faucet(
            "jobStatus",
            json!({ "ok": true, "job_id": "job-1", "status": "running", "phase": "reconciling" }),
        )
    };
    let mut replies = opened_by_server();
    replies.extend([
        out(FAUCET_LOADED),
        faucet_info_on(SEQUENCER_V03)[0].clone(),
        faucet_info_on(SEQUENCER_V03)[1].clone(),
        faucet_info_on(SEQUENCER_V03)[2].clone(),
        faucet(
            "requestDrop",
            json!({ "ok": true, "job_id": "job-1", "status": "queued" }),
        ),
        running(),
        out(FAUCET_LOADED),
        running(),
    ]);
    let fake = FakeRunner::with(replies);
    let s = wallet_server(fake.clone());
    // The agent stops waiting at once: the drop goes on.
    let gave_up = CancellationToken::new();
    gave_up.cancel();
    let first = body(
        &s.wallet_faucet(
            gave_up,
            Parameters(FaucetParams {
                account: Some(agent_address()),
                job_id: None,
            }),
        )
        .await
        .unwrap(),
    );
    assert_eq!(first["job_id"], "job-1", "{first}");
    let b = body(
        &s.wallet_faucet(
            CancellationToken::new(),
            Parameters(FaucetParams {
                account: None,
                job_id: Some("job-1".into()),
            }),
        )
        .await
        .unwrap(),
    );
    assert_eq!(b["status"], "running", "{b}");
    assert_eq!(b["job_id"], "job-1", "{b}");
    // A drop still running is never acknowledged.
    assert!(!fake.calls.lock().unwrap().iter().any(|c| {
        c.get(3).map(String::as_str) == Some("jobResultAck")
            && c.get(4).map(String::as_str) == Some("str:job-1")
    }));
}

#[tokio::test]
async fn without_lez_faucet_the_faucet_says_where_to_get_it() {
    let mut replies = opened_by_server();
    replies.extend([
        out(r#"[{"name":"lez_core","status":"loaded"}]"#),
        crate::logosctl::RawOutput {
            code: 3,
            stdout: r#"{"code":"MODULE_LOAD_FAILED","message":"Failed to load module 'lez_faucet'.","status":"error"}"#.into(),
            stderr: String::new(),
        },
    ]);
    let r = wallet_server(FakeRunner::with(replies))
        .wallet_faucet(
            CancellationToken::new(),
            Parameters(FaucetParams {
                account: Some(agent_address()),
                job_id: None,
            }),
        )
        .await
        .unwrap();
    assert!(is_error(&r), "{}", text(&r));
    assert!(text(&r).contains("LEZ Faucet"), "{}", text(&r));
}

/// A server running inside Basecamp, where the AI Agent app answers approvals.
fn hosted_server(fake: Arc<FakeRunner>) -> LogosServer {
    let mut s = server(fake, None);
    s.settings.host_owns_messaging = true;
    s
}

/// The replies a chain transfer's checks read: the sender is known and holds 5000.
fn chain_sender_ok() -> [crate::logosctl::RawOutput; 2] {
    [
        chain(
            "wallet_get_known_addresses",
            json!(format!("[\"{CHAIN_KEY_A}\",\"{CHAIN_KEY_B}\"]")),
        ),
        chain("wallet_get_balance", json!("5000")),
    ]
}

fn chain_transfer_params(amount: &str) -> Parameters<ChainTransferParams> {
    Parameters(ChainTransferParams {
        from: CHAIN_KEY_A.into(),
        to: CHAIN_KEY_B.into(),
        amount_lepta: amount.into(),
    })
}

/// Start a chain transfer and wait until it is waiting for the user.
async fn request_chain_transfer(
    s: &LogosServer,
) -> (tokio::task::JoinHandle<CallToolResult>, String) {
    let agent = s.clone();
    let call = tokio::spawn(async move {
        agent
            .chain_transfer(CancellationToken::new(), chain_transfer_params("1500"))
            .await
            .unwrap()
    });
    for _ in 0..100 {
        if let Some(id) = s.approvals()["waiting"][0]["id"].as_str() {
            return (call, id.to_string());
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("no request appeared");
}

fn chain_sends(fake: &FakeRunner) -> Vec<Vec<String>> {
    fake.calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c.get(3).map(String::as_str) == Some("wallet_transfer_funds"))
        .cloned()
        .collect()
}

#[test]
fn chain_transfers_sit_behind_the_transfers_switch() {
    assert_eq!(ToolGroups::group_of("chain_transfer"), Some("transfers"));
}

#[tokio::test(start_paused = true)]
async fn an_approved_chain_transfer_sends_from_the_key_with_change_back_to_it() {
    let mut replies = chain_sender_ok().to_vec(); // checks before asking
    replies.extend(chain_sender_ok()); // checks again when approved
    replies.push(chain("wallet_transfer_funds", json!(TX)));
    let fake = FakeRunner::with(replies);
    // Approval is asked for even with "Ask me before each transfer" off.
    let s = hosted_server(fake.clone());
    let (call, id) = request_chain_transfer(&s).await;
    let waiting = &s.approvals()["waiting"][0];
    assert_eq!(waiting["chain"], true, "{waiting}");
    assert!(
        chain_sends(&fake).is_empty(),
        "nothing sent before the user answers"
    );
    s.approve_transfer(&id).await.unwrap();
    let r = call.await.unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    assert_eq!(body(&r)["tx_hash"], TX);
    assert_eq!(
        chain_sends(&fake)[0][4..],
        [
            format!("str:{CHAIN_KEY_A}"),
            format!("json:[\"{CHAIN_KEY_A}\"]"),
            format!("str:{CHAIN_KEY_B}"),
            "str:1500".to_string(),
            "str:".to_string(),
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn a_denied_chain_transfer_is_never_sent() {
    let fake = FakeRunner::with(chain_sender_ok().to_vec());
    let s = hosted_server(fake.clone());
    let (call, id) = request_chain_transfer(&s).await;
    s.deny_transfer(&id).unwrap();
    let r = call.await.unwrap();
    assert!(is_error(&r));
    assert!(chain_sends(&fake).is_empty());
}

#[tokio::test]
async fn chain_transfers_need_basecamp_to_approve_them() {
    let fake = FakeRunner::with(vec![]);
    let r = server(fake.clone(), None)
        .chain_transfer(CancellationToken::new(), chain_transfer_params("1"))
        .await
        .unwrap();
    assert!(is_error(&r), "{}", text(&r));
    assert!(text(&r).contains("Basecamp"), "{}", text(&r));
    assert!(fake.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_chain_transfer_from_a_key_the_node_lacks_never_reaches_the_user() {
    let fake = FakeRunner::with(vec![chain(
        "wallet_get_known_addresses",
        json!(format!("[\"{CHAIN_KEY_B}\"]")),
    )]);
    let s = hosted_server(fake.clone());
    let r = s
        .chain_transfer(CancellationToken::new(), chain_transfer_params("1"))
        .await
        .unwrap();
    assert!(is_error(&r), "{}", text(&r));
    assert!(s.approvals()["waiting"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn a_chain_transfer_beyond_the_balance_never_reaches_the_user() {
    let fake = FakeRunner::with(chain_sender_ok().to_vec());
    let s = hosted_server(fake.clone());
    let r = s
        .chain_transfer(CancellationToken::new(), chain_transfer_params("5001"))
        .await
        .unwrap();
    assert!(is_error(&r), "{}", text(&r));
    assert!(s.approvals()["waiting"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn malformed_chain_transfers_are_refused_without_touching_the_node() {
    let fake = FakeRunner::with(vec![]);
    let s = hosted_server(fake.clone());
    for p in [
        ChainTransferParams {
            from: "zz".into(),
            to: CHAIN_KEY_B.into(),
            amount_lepta: "1".into(),
        },
        ChainTransferParams {
            from: CHAIN_KEY_A.into(),
            to: "zz".into(),
            amount_lepta: "1".into(),
        },
        ChainTransferParams {
            from: CHAIN_KEY_A.into(),
            to: CHAIN_KEY_B.into(),
            amount_lepta: "0".into(),
        },
        ChainTransferParams {
            from: CHAIN_KEY_A.into(),
            to: CHAIN_KEY_B.into(),
            amount_lepta: "1.5".into(),
        },
    ] {
        let r = s
            .chain_transfer(CancellationToken::new(), Parameters(p))
            .await
            .unwrap();
        assert!(is_error(&r), "{}", text(&r));
    }
    assert!(fake.calls.lock().unwrap().is_empty());
}

/// Approve a chain transfer whose send gets `reply`, and return the agent's answer.
async fn approved_chain_send(reply: crate::logosctl::RawOutput) -> CallToolResult {
    let mut replies = chain_sender_ok().to_vec();
    replies.extend(chain_sender_ok());
    replies.push(reply);
    let s = hosted_server(FakeRunner::with(replies));
    let (call, id) = request_chain_transfer(&s).await;
    s.approve_transfer(&id).await.unwrap();
    call.await.unwrap()
}

#[tokio::test(start_paused = true)]
async fn a_refused_chain_send_may_still_have_gone_out() {
    // The node may refuse after broadcasting, so a refusal is never "not sent".
    let r = approved_chain_send(chain_refused("wallet_transfer_funds", "timed out")).await;
    assert!(is_error(&r));
    assert!(text(&r).contains("may have been sent"), "{}", text(&r));
}

#[tokio::test(start_paused = true)]
async fn a_chain_send_without_a_hash_may_have_gone_out() {
    let r = approved_chain_send(chain("wallet_transfer_funds", Value::Null)).await;
    assert!(is_error(&r));
    assert!(text(&r).contains("may have been sent"), "{}", text(&r));
}

#[tokio::test(start_paused = true)]
async fn a_chain_send_reporting_its_hash_in_an_object_is_sent() {
    let r = approved_chain_send(chain("wallet_transfer_funds", json!({ "tx_hash": TX }))).await;
    assert!(!is_error(&r), "{}", text(&r));
    assert_eq!(body(&r)["tx_hash"], TX);
}

#[tokio::test(start_paused = true)]
async fn a_chain_send_whose_balance_fell_meanwhile_is_not_sent() {
    let mut replies = chain_sender_ok().to_vec();
    replies.push(chain_sender_ok()[0].clone());
    replies.push(chain("wallet_get_balance", json!("10")));
    let fake = FakeRunner::with(replies);
    let s = hosted_server(fake.clone());
    let (call, id) = request_chain_transfer(&s).await;
    s.approve_transfer(&id).await.unwrap();
    let r = call.await.unwrap();
    assert!(is_error(&r));
    assert!(chain_sends(&fake).is_empty());
}

#[tokio::test(start_paused = true)]
async fn turning_transfers_off_stops_a_chain_transfer_already_waiting() {
    let mut replies = chain_sender_ok().to_vec();
    replies.extend(chain_sender_ok());
    replies.push(chain("wallet_transfer_funds", json!(TX)));
    let fake = FakeRunner::with(replies);
    let (switches, rx) = tokio::sync::watch::channel(ToolGroups::default());
    let s = hosted_server(fake.clone()).with_tool_groups(rx);
    let (call, id) = request_chain_transfer(&s).await;
    switches.send_modify(|g| g.set("transfers", false).unwrap());
    s.approve_transfer(&id).await.unwrap();
    let r = call.await.unwrap();
    assert!(is_error(&r), "{}", text(&r));
    assert!(chain_sends(&fake).is_empty());
}

const AGENT_AND_PRIVATE: &str = r#"[{"account_id":"e9ce8b81376428e3fb1a8192026027ac25dc68bfafb100c82e6107f068c8213f","is_public":true},{"account_id":"5b1f00d5c56e0a8a4f9d8e2ab5f1e6c5d4a3b2c1d0e9f8a7b6c5d4e3f2a1b0c9","is_public":false}]"#;

#[tokio::test]
async fn the_faucet_never_funds_the_public_form_of_a_private_account() {
    let mut replies = opened_by_server();
    replies[4] = lez("list_accounts", AGENT_AND_PRIVATE);
    let fake = FakeRunner::with(replies);
    let r = wallet_server(fake.clone())
        .wallet_faucet(
            CancellationToken::new(),
            Parameters(FaucetParams {
                account: Some(
                    "5b1f00d5c56e0a8a4f9d8e2ab5f1e6c5d4a3b2c1d0e9f8a7b6c5d4e3f2a1b0c9".into(),
                ),
                job_id: None,
            }),
        )
        .await
        .unwrap();
    assert!(is_error(&r), "{}", text(&r));
    assert!(
        !fake
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.get(2).map(String::as_str) == Some("lez_faucet"))
    );
}

#[tokio::test]
async fn a_drop_whose_status_cannot_be_read_still_hands_back_its_job_id() {
    let mut replies = opened_by_server();
    replies.extend([
        out(FAUCET_LOADED),
        faucet_info_on(SEQUENCER_V03)[0].clone(),
        faucet_info_on(SEQUENCER_V03)[1].clone(),
        faucet_info_on(SEQUENCER_V03)[2].clone(),
        faucet(
            "requestDrop",
            json!({ "ok": true, "job_id": "request_drop-7", "status": "queued" }),
        ),
        crate::logosctl::RawOutput {
            code: 4,
            stdout: r#"{"code":"RPC_FAILED","message":"call failed","status":"error"}"#.into(),
            stderr: String::new(),
        },
    ]);
    let r = wallet_server(FakeRunner::with(replies))
        .wallet_faucet(
            CancellationToken::new(),
            Parameters(FaucetParams {
                account: Some(agent_address()),
                job_id: None,
            }),
        )
        .await
        .unwrap();
    assert!(text(&r).contains("request_drop-7"), "{}", text(&r));
}

#[tokio::test]
async fn the_faucet_only_checks_drops_this_server_started() {
    let fake = FakeRunner::with(vec![]);
    let r = wallet_server(fake.clone())
        .wallet_faucet(
            CancellationToken::new(),
            Parameters(FaucetParams {
                account: None,
                job_id: Some("request_drop-1".into()),
            }),
        )
        .await
        .unwrap();
    assert!(is_error(&r), "{}", text(&r));
    assert!(fake.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_unconfirmed_drop_passes_on_the_faucets_reason() {
    let mut replies = opened_by_server();
    replies.extend([
        out(FAUCET_LOADED),
        faucet_info_on(SEQUENCER_V03)[0].clone(),
        faucet_info_on(SEQUENCER_V03)[1].clone(),
        faucet_info_on(SEQUENCER_V03)[2].clone(),
        faucet("requestDrop", json!({ "ok": true, "job_id": "job-2", "status": "queued" })),
        faucet(
            "jobStatus",
            json!({ "ok": true, "job_id": "job-2", "status": "outcome_unknown",
                    "error": { "code": "unconfirmed", "message": "The claim was submitted but its outcome could not be confirmed in time." } }),
        ),
        faucet("jobResultAck", json!({ "ok": true })),
    ]);
    let r = wallet_server(FakeRunner::with(replies))
        .wallet_faucet(
            CancellationToken::new(),
            Parameters(FaucetParams {
                account: Some(agent_address()),
                job_id: None,
            }),
        )
        .await
        .unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    let b = body(&r);
    assert_eq!(b["status"], "outcome_unknown", "{b}");
    assert!(
        b["reason"]
            .as_str()
            .unwrap()
            .contains("could not be confirmed"),
        "{b}"
    );
}

const SEQUENCER_V03: &str = "https://testnet.lez.logos.co";

/// The faucet module reporting, through a getFaucetInfo job, that it serves `sequencer`.
fn faucet_info_on(sequencer: &str) -> [crate::logosctl::RawOutput; 3] {
    [
        faucet(
            "getFaucetInfo",
            json!({ "ok": true, "job_id": "faucet_info-1", "status": "queued" }),
        ),
        faucet(
            "jobStatus",
            json!({ "ok": true, "job_id": "faucet_info-1", "status": "succeeded",
                    "result": { "network": "lez-v03-testnet", "sequencer_url": sequencer } }),
        ),
        faucet("jobResultAck", json!({ "ok": true })),
    ]
}

#[tokio::test]
async fn the_faucet_switches_networks_only_for_the_drop() {
    let legacy = "https://testnet-legacy.lez.logos.co";
    let mut replies = opened_by_server();
    replies.push(out(FAUCET_LOADED));
    replies.extend(faucet_info_on(legacy));
    replies.extend([
        faucet(
            "configure",
            json!({ "ok": true, "result": { "configured": true } }),
        ),
        faucet(
            "requestDrop",
            json!({ "ok": true, "job_id": "job-9", "status": "queued" }),
        ),
        faucet(
            "jobStatus",
            json!({ "ok": true, "job_id": "job-9", "status": "succeeded",
                    "result": { "amount": "100000000", "tx_hash": TX } }),
        ),
        faucet("jobResultAck", json!({ "ok": true })),
        faucet(
            "configure",
            json!({ "ok": true, "result": { "configured": true } }),
        ),
    ]);
    let fake = FakeRunner::with(replies);
    let r = wallet_server(fake.clone())
        .wallet_faucet(
            CancellationToken::new(),
            Parameters(FaucetParams {
                account: Some(agent_address()),
                job_id: None,
            }),
        )
        .await
        .unwrap();
    assert!(!is_error(&r), "{}", text(&r));
    let configures: Vec<String> = fake
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c.get(3).map(String::as_str) == Some("configure"))
        .map(|c| c[4].clone())
        .collect();
    assert_eq!(
        configures,
        [
            "str:https://testnet.lez.logos.co/".to_string(),
            format!("str:{legacy}")
        ],
        "switched for the drop, then back"
    );
    assert_eq!(body(&r)["faucet_network"], "restored", "{}", text(&r));
}

#[tokio::test]
async fn an_unfunded_node_key_reads_as_zero() {
    // blockchain_module 0.3.4 answers NotFound, not 0, for a known key with no notes.
    let fake = FakeRunner::with(vec![
        out(BLOCKCHAIN_LOADED),
        chain(
            "wallet_get_known_addresses",
            json!(format!("[\"{CHAIN_KEY_A}\",\"{CHAIN_KEY_B}\"]")),
        ),
        chain_refused("wallet_get_balance", "Unknown wallet address."),
        chain("wallet_get_balance", json!("1500")),
    ]);
    let b = body(
        &server(fake, None)
            .chain_wallet(Parameters(ChainWalletParams { address: None }))
            .await
            .unwrap(),
    );
    assert_eq!(b["accounts"][0]["balance"], "0", "{b}");
    assert!(b["accounts"][0].get("error").is_none(), "{b}");
    assert_eq!(b["accounts"][1]["balance"], "1500", "{b}");
}

#[tokio::test]
async fn one_unfunded_node_key_shows_zero_and_no_notes() {
    let fake = FakeRunner::with(vec![
        out(BLOCKCHAIN_LOADED),
        chain(
            "wallet_get_known_addresses",
            json!(format!("[\"{CHAIN_KEY_A}\"]")),
        ),
        chain_refused("wallet_get_balance", "Unknown wallet address."),
        chain_refused("wallet_get_notes", "Unknown wallet address."),
    ]);
    let b = body(
        &server(fake, None)
            .chain_wallet(Parameters(ChainWalletParams {
                address: Some(CHAIN_KEY_A.into()),
            }))
            .await
            .unwrap(),
    );
    assert_eq!(b["balance"], "0", "{b}");
    assert_eq!(b["notes"], json!([]), "{b}");
}

#[tokio::test]
async fn an_address_the_node_has_no_key_for_is_said_so() {
    let fake = FakeRunner::with(vec![
        out(BLOCKCHAIN_LOADED),
        chain(
            "wallet_get_known_addresses",
            json!(format!("[\"{CHAIN_KEY_A}\"]")),
        ),
    ]);
    let r = server(fake, None)
        .chain_wallet(Parameters(ChainWalletParams {
            address: Some(CHAIN_KEY_B.into()),
        }))
        .await
        .unwrap();
    assert!(is_error(&r), "{}", text(&r));
    assert!(
        text(&r).contains("not one of the chain node's"),
        "{}",
        text(&r)
    );
}

/// A libBlock event as blockchain_module emits it: the finalized tip, once per change.
fn lib_block(events: &EventStore, height: u64, header_id: &str) {
    events.apply_event(
        "libBlock",
        &json!({ "arg0": json!({ "height": height, "header_id": header_id }).to_string() }),
    );
}

#[tokio::test]
async fn chain_events_pages_finalized_blocks_by_seq() {
    let events = Arc::new(EventStore::default());
    let s = LogosServer::new(
        Arc::new(Logosctl::new(FakeRunner::with(vec![]))),
        events.clone(),
        None,
        server(FakeRunner::with(vec![]), None).settings.clone(),
    );
    lib_block(&events, 100, &"aa".repeat(32));
    lib_block(&events, 105, &"bb".repeat(32));
    events.apply_event("libBlock", &json!({ "arg0": "null" }));
    let b = body(
        &s.chain_events(Parameters(ChainEventsParams { after_seq: None }))
            .await
            .unwrap(),
    );
    let list = b["events"].as_array().unwrap();
    assert_eq!(list.len(), 2, "{b}");
    assert_eq!(list[1]["height"], 105, "{b}");
    assert_eq!(list[1]["header_id"], "bb".repeat(32), "{b}");
    let first = list[0]["seq"].as_u64().unwrap();
    let b = body(
        &s.chain_events(Parameters(ChainEventsParams {
            after_seq: Some(first),
        }))
        .await
        .unwrap(),
    );
    assert_eq!(b["events"].as_array().unwrap().len(), 1, "{b}");
}

#[test]
fn only_the_newest_finalized_blocks_are_kept() {
    let events = EventStore::default();
    for h in 0..(crate::events::MAX_CHAIN_EVENTS as u64 + 10) {
        lib_block(&events, h, &format!("{h:064x}"));
    }
    let kept = events.lib_blocks(0, usize::MAX);
    assert_eq!(kept.len(), crate::events::MAX_CHAIN_EVENTS);
    assert_eq!(
        kept.last().unwrap().height,
        crate::events::MAX_CHAIN_EVENTS as u64 + 9
    );
}

const ONLINE: &str = "{\"height\":84895,\"lib\":\"aa\",\"lib_slot\":2909742,\"mode\":\"Online\",\"slot\":2951727,\"tip\":\"bb\"}";

#[tokio::test]
async fn chain_rewards_lists_vouchers_and_mining_tickets() {
    let fake = FakeRunner::with(vec![
        out(BLOCKCHAIN_LOADED),
        chain("get_cryptarchia_info", json!(ONLINE)),
        chain(
            "wallet_get_claimable_vouchers",
            json!(
                "{\"tip\":\"c4\",\"vouchers\":[{\"commitment\":\"5e\",\"nullifier\":\"1d\"}],\"reward_amount\":\"4200\",\"total_claimable\":\"4200\"}"
            ),
        ),
        chain(
            "pow_claimable_rewards",
            json!("{\"claimable_tickets\":2,\"slots_until_expiry\":[271,12]}"),
        ),
    ]);
    let b = body(&server(fake, None).chain_rewards().await.unwrap());
    assert_eq!(b["leader"]["vouchers"], 1, "{b}");
    assert_eq!(b["leader"]["total_claimable"], "4200", "{b}");
    assert_eq!(b["mining"]["claimable_tickets"], 2, "{b}");
}

#[tokio::test]
async fn chain_rewards_waits_for_the_node_to_be_online() {
    // These reads block without a timeout while the node bootstraps.
    let bootstrapping = ONLINE.replace("Online", "Bootstrapping");
    let fake = FakeRunner::with(vec![
        out(BLOCKCHAIN_LOADED),
        chain("get_cryptarchia_info", json!(bootstrapping)),
    ]);
    let r = server(fake.clone(), None).chain_rewards().await.unwrap();
    assert!(is_error(&r), "{}", text(&r));
    assert!(text(&r).contains("Online"), "{}", text(&r));
    assert!(
        !fake
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.get(3).map(String::as_str) == Some("wallet_get_claimable_vouchers"))
    );
}

#[tokio::test]
async fn a_reward_claim_returns_its_transaction() {
    let fake = FakeRunner::with(vec![
        out(BLOCKCHAIN_LOADED),
        chain("get_cryptarchia_info", json!(ONLINE)),
        chain("leader_claim", json!(TX)),
    ]);
    let b = body(
        &server(fake.clone(), None)
            .chain_claim_reward()
            .await
            .unwrap(),
    );
    assert_eq!(b["tx_hash"], TX, "{b}");
    assert!(
        fake.calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.get(3).map(String::as_str) == Some("leader_claim"))
    );
}

#[tokio::test]
async fn a_claim_with_no_voucher_ready_says_so() {
    let fake = FakeRunner::with(vec![
        out(BLOCKCHAIN_LOADED),
        chain("get_cryptarchia_info", json!(ONLINE)),
        chain_refused(
            "leader_claim",
            "Failed to claim leader rewards: Wallet(NoClaimableVoucher)",
        ),
    ]);
    let r = server(fake, None).chain_claim_reward().await.unwrap();
    assert!(is_error(&r), "{}", text(&r));
    assert!(text(&r).contains("no reward voucher"), "{}", text(&r));
}

#[tokio::test]
async fn a_claim_waits_for_the_node_to_be_online() {
    // leader_claim blocks without a timeout while the node bootstraps.
    let bootstrapping = ONLINE.replace("Online", "Bootstrapping");
    let fake = FakeRunner::with(vec![
        out(BLOCKCHAIN_LOADED),
        chain("get_cryptarchia_info", json!(bootstrapping)),
    ]);
    let r = server(fake.clone(), None)
        .chain_claim_reward()
        .await
        .unwrap();
    assert!(is_error(&r), "{}", text(&r));
    assert!(
        !fake
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.get(3).map(String::as_str) == Some("leader_claim"))
    );
}

#[test]
fn reward_claims_sit_behind_the_chain_switch() {
    assert_eq!(ToolGroups::group_of("chain_claim_reward"), Some("chain"));
}
