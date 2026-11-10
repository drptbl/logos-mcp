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
