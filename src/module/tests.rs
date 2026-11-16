use super::*;
use axum::body::Body;
use http_body_util::BodyExt as _;
use tower::ServiceExt as _;

fn initialize() -> axum::http::Request<Body> {
    axum::http::Request::post("/mcp")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("host", "127.0.0.1:8765")
        .body(Body::from(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#,
        ))
        .unwrap()
}

fn with_token(mut req: axum::http::Request<Body>, token: &str) -> axum::http::Request<Body> {
    req.headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    req
}

/// A minimal MCP server, so the router has something real behind it.
#[derive(Clone)]
struct Probe;
impl rmcp::ServerHandler for Probe {}

#[tokio::test]
async fn requests_without_the_right_token_are_refused() {
    let token = Token::from_hex("ab".repeat(32)).unwrap();
    for req in [
        initialize(),
        with_token(initialize(), &"cd".repeat(32)),
        with_token(initialize(), ""),
    ] {
        let resp = router(|| Ok(Probe), &Tokens::from(token.clone()))
            .oneshot(req)
            .await
            .unwrap();
        assert_eq!(resp.status(), 401);
    }
}

#[tokio::test]
async fn the_right_token_reaches_the_mcp_server() {
    let token = Token::from_hex("ab".repeat(32)).unwrap();
    let resp = router(|| Ok(Probe), &Tokens::from(token.clone()))
        .oneshot(with_token(initialize(), &"ab".repeat(32)))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(String::from_utf8_lossy(&body).contains("protocolVersion"));
}

#[test]
fn the_token_is_created_once_private_and_reused() {
    let dir = tempfile::tempdir().unwrap();
    let first = Token::load_or_create(dir.path()).unwrap();
    let again = Token::load_or_create(dir.path()).unwrap();
    assert_eq!(first.as_str(), again.as_str());
    assert_eq!(first.as_str().len(), 64);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(dir.path().join(TOKEN_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[test]
fn config_is_written_with_defaults_and_read_back_with_edits() {
    let dir = tempfile::tempdir().unwrap();
    let defaults = ModuleConfig::load_or_create(dir.path()).unwrap();
    assert_eq!(defaults.port, DEFAULT_PORT);
    assert!(defaults.files_root.is_none() && defaults.discovery_bootstrap.is_empty());
    let mut edited: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join(CONFIG_FILE)).unwrap())
            .unwrap();
    edited["port"] = serde_json::json!(9999);
    std::fs::write(dir.path().join(CONFIG_FILE), edited.to_string()).unwrap();
    assert_eq!(ModuleConfig::load_or_create(dir.path()).unwrap().port, 9999);
}

#[test]
fn a_broken_config_is_an_error_not_silent_defaults() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(CONFIG_FILE), "{ not json").unwrap();
    assert!(ModuleConfig::load_or_create(dir.path()).is_err());
}

#[tokio::test]
async fn a_web_page_is_refused_even_with_the_token() {
    let token = Token::from_hex("ab".repeat(32)).unwrap();
    let mut req = with_token(initialize(), &"ab".repeat(32));
    req.headers_mut()
        .insert("origin", "https://evil.example".parse().unwrap());
    let resp = router(|| Ok(Probe), &Tokens::from(token.clone()))
        .oneshot(req)
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
}

#[test]
fn a_files_root_overlapping_the_module_data_is_refused() {
    // Storage downloads could otherwise write the token, config or agent accounts.
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join("data");
    let inner = data.join("downloads");
    let elsewhere = home.path().join("files");
    for dir in [&data, &inner, &elsewhere] {
        std::fs::create_dir_all(dir).unwrap();
    }
    for root in [home.path(), &data, &inner] {
        let config = ModuleConfig {
            files_root: Some(root.to_path_buf()),
            ..ModuleConfig::default()
        };
        let err = config.files_root(&data).unwrap_err();
        assert!(err.contains("data folder"), "{err}");
    }
    let config = ModuleConfig {
        files_root: Some(elsewhere.clone()),
        ..ModuleConfig::default()
    };
    let root = config.files_root(&data).unwrap().unwrap();
    assert_eq!(root.path(), elsewhere.canonicalize().unwrap());
    assert!(ModuleConfig::default().files_root(&data).unwrap().is_none());
}

#[test]
fn a_module_basecamp_has_not_loaded_is_named_plainly() {
    let raw = r#"Method call delivery_module.rlnState() failed: {"code":"object_unavailable","message":"failed to acquire remote object 'delivery_module' (module not loaded, not published, or transport failure)","origin":"delivery_module"}"#;
    let e = sdk_error("delivery_module", raw.to_string());
    assert!(matches!(e, LogosError::Module(_)), "{e:?}");
    assert!(
        e.to_string()
            .contains("delivery_module isn't running in Basecamp"),
        "{e}"
    );
    // Anything else keeps its detail.
    let other = sdk_error("lez_core", "timed out".into());
    assert!(matches!(other, LogosError::Method(m) if m == "timed out"));
}

#[tokio::test]
async fn a_rotated_token_replaces_the_old_one_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let tokens = Tokens::load_or_create(dir.path()).unwrap();
    let old = tokens.current();
    let app = router(|| Ok(Probe), &tokens);
    let status = |t: &str| {
        let app = app.clone();
        let req = with_token(initialize(), t);
        async move { app.oneshot(req).await.unwrap().status() }
    };
    assert_eq!(status(old.as_str()).await, 200);

    let new = tokens.rotate().unwrap();
    assert_ne!(new.as_str(), old.as_str());
    assert_eq!(status(old.as_str()).await, 401);
    assert_eq!(status(new.as_str()).await, 200);
    // The file holds the new one, so a restart keeps it.
    assert_eq!(
        Tokens::load_or_create(dir.path())
            .unwrap()
            .current()
            .as_str(),
        new.as_str()
    );
}

#[test]
fn a_permission_change_is_saved_and_applies_live() {
    let dir = tempfile::tempdir().unwrap();
    let control = Control::open(dir.path()).unwrap();
    let live = control.tool_groups();
    // A hand edit made after the module started is kept.
    let path = dir.path().join(CONFIG_FILE);
    let edited = std::fs::read_to_string(&path)
        .unwrap()
        .replace("8765", "9000")
        .replace("\"storage\": true", "\"storage\": false");
    std::fs::write(&path, edited).unwrap();

    let now = control.set_permission("transfers", false).unwrap();
    assert_eq!(now["transfers"], false);
    assert!(!live.borrow().allows("wallet_transfer"));
    let saved = ModuleConfig::load_or_create(dir.path()).unwrap();
    assert!(!saved.tools.transfers);
    assert_eq!(saved.port, 9000);
    assert!(!saved.tools.storage, "a hand edit to another group is kept");
    assert!(!live.borrow().allows("storage_upload"), "and applies live");

    assert!(control.set_permission("everything", false).is_err());
    assert_eq!(control.permissions()["transfers"], false);
}

#[test]
fn activity_is_reported_after_a_sequence_number() {
    let dir = tempfile::tempdir().unwrap();
    let control = Control::open(dir.path()).unwrap();
    let log = control.activity_log();
    log.record("node_status", &serde_json::json!({}), true, "ok");
    log.record("wallet_status", &serde_json::json!({}), false, "no wallet");
    let page = control.activity(1);
    assert_eq!(page["entries"].as_array().unwrap().len(), 1);
    assert_eq!(page["entries"][0]["tool"], "wallet_status");
    assert_eq!(page["next"], 2);
    assert_eq!(
        control.activity(2)["next"],
        2,
        "nothing newer keeps the cursor"
    );
}

#[test]
fn status_names_the_endpoint_and_where_its_files_are() {
    let dir = tempfile::tempdir().unwrap();
    let control = Control::open(dir.path()).unwrap();
    let s = control.status(None);
    assert_eq!(s["url"], "http://127.0.0.1:8765/mcp");
    assert_eq!(s["running"], true);
    assert!(s["token_file"].as_str().unwrap().ends_with(TOKEN_FILE));
    assert!(
        s.get("token").is_none(),
        "the token itself is never in status"
    );
    let failed = control.status(Some("port in use"));
    assert_eq!(
        (failed["running"].clone(), failed["error"].clone()),
        (false.into(), "port in use".into())
    );
}

#[test]
fn a_cursor_past_the_log_starts_over() {
    // The activity file was deleted and the sequence restarted below the app's cursor.
    let dir = tempfile::tempdir().unwrap();
    let control = Control::open(dir.path()).unwrap();
    control
        .activity_log()
        .record("node_status", &serde_json::json!({}), true, "ok");
    let page = control.activity(57);
    assert_eq!(page["entries"].as_array().unwrap().len(), 1);
    assert_eq!(page["next"], 1);
}

#[test]
fn asking_before_each_transfer_is_off_until_switched_on() {
    let dir = tempfile::tempdir().unwrap();
    let control = Control::open(dir.path()).unwrap();
    assert_eq!(control.permissions()["ask_before_transfer"], false);
    let now = control.set_permission("ask_before_transfer", true).unwrap();
    assert_eq!(now["ask_before_transfer"], true);
    assert!(control.tool_groups().borrow().ask_before_transfer);
    assert!(
        ModuleConfig::load_or_create(dir.path())
            .unwrap()
            .tools
            .ask_before_transfer
    );
}

// ---- the approval policy ---------------------------------------------------

use crate::policy::{ApprovalPolicy, Approver, Wallet};

const HUMAN_KEY: &str = "ed:0b00086682a0d169f90095b9d58e6c003ee591531ccf8b59c712c1cb70603269";

fn room_policy() -> serde_json::Value {
    serde_json::json!({
        "agent": {"limit_lepta": "3000000", "up_to": "ask", "above": "room"},
        "node": {"limit_lepta": "0", "up_to": "ask", "above": "ask"},
        "room": {"topic": "/muster/1/r/proto", "approvers": [HUMAN_KEY], "approvals": 1, "expires_mins": 60}
    })
}

#[test]
fn an_old_config_carries_its_ask_switch_over_and_keeps_the_node_wallet_asking() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(CONFIG_FILE),
        r#"{"port": 8765, "tools": {"ask_before_transfer": true}}"#,
    )
    .unwrap();
    let control = Control::open(dir.path()).unwrap();
    let policy: ApprovalPolicy = serde_json::from_value(control.approval_policy()).unwrap();
    assert_eq!(policy.approver(Wallet::Agent, "1"), Approver::Ask);
    assert_eq!(policy.approver(Wallet::Node, "1"), Approver::Ask);

    let fresh = tempfile::tempdir().unwrap();
    let control = Control::open(fresh.path()).unwrap();
    let policy: ApprovalPolicy = serde_json::from_value(control.approval_policy()).unwrap();
    assert_eq!(policy, ApprovalPolicy::default());
}

#[test]
fn a_policy_change_is_checked_saved_and_applied_live() {
    let dir = tempfile::tempdir().unwrap();
    let control = Control::open(dir.path()).unwrap();
    let live = control.approval_policy_watch();
    control
        .set_approval_policy(&room_policy().to_string())
        .unwrap();
    assert_eq!(
        live.borrow().approver(Wallet::Agent, "3000001"),
        Approver::Room
    );
    assert_eq!(
        live.borrow().approver(Wallet::Agent, "3000000"),
        Approver::Ask
    );
    // Saved: a restart reads it back.
    let reopened = Control::open(dir.path()).unwrap();
    assert_eq!(reopened.approval_policy(), room_policy());
    // The old switch shows what the policy does.
    assert_eq!(reopened.permissions()["ask_before_transfer"], true);
}

#[test]
fn a_policy_that_cant_be_followed_is_refused_and_not_saved() {
    let dir = tempfile::tempdir().unwrap();
    let control = Control::open(dir.path()).unwrap();
    let mut looser = room_policy();
    looser["agent"]["above"] = "send".into();
    assert!(control.set_approval_policy(&looser.to_string()).is_err());
    let mut no_room = room_policy();
    no_room["room"] = serde_json::Value::Null;
    assert!(control.set_approval_policy(&no_room.to_string()).is_err());
    assert!(control.set_approval_policy("not json").is_err());
    assert_eq!(
        Control::open(dir.path()).unwrap().approval_policy(),
        serde_json::to_value(ApprovalPolicy::default()).unwrap()
    );
}

#[test]
fn the_old_ask_switch_writes_through_and_keeps_the_room_tier() {
    let dir = tempfile::tempdir().unwrap();
    let control = Control::open(dir.path()).unwrap();
    control
        .set_approval_policy(&room_policy().to_string())
        .unwrap();
    control
        .set_permission("ask_before_transfer", false)
        .unwrap();
    let policy: ApprovalPolicy = serde_json::from_value(control.approval_policy()).unwrap();
    assert_eq!(policy.approver(Wallet::Agent, "1"), Approver::Send);
    assert_eq!(policy.approver(Wallet::Agent, "3000001"), Approver::Room);
    let reopened: ApprovalPolicy =
        serde_json::from_value(Control::open(dir.path()).unwrap().approval_policy()).unwrap();
    assert_eq!(reopened, policy);
}

#[test]
fn the_agent_seat_is_remembered_once_set_up() {
    let dir = tempfile::tempdir().unwrap();
    let control = Control::open(dir.path()).unwrap();
    assert!(!control.config().agent_seat);
    control.remember_agent_seat().unwrap();
    assert!(Control::open(dir.path()).unwrap().config().agent_seat);
}

#[test]
fn a_saved_policy_that_cant_be_followed_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let mut looser = room_policy();
    looser["agent"]["above"] = "send".into();
    std::fs::write(
        dir.path().join(CONFIG_FILE),
        serde_json::json!({"approvals": looser}).to_string(),
    )
    .unwrap();
    let control = Control::open(dir.path()).unwrap();
    let policy: ApprovalPolicy = serde_json::from_value(control.approval_policy()).unwrap();
    assert_eq!(policy, ApprovalPolicy::ask_for_everything());
}

#[test]
fn a_policy_missing_a_wallet_or_misspelt_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let control = Control::open(dir.path()).unwrap();
    assert!(control.set_approval_policy("{}").is_err());
    let mut only_agent = room_policy();
    only_agent.as_object_mut().unwrap().remove("node");
    assert!(
        control
            .set_approval_policy(&only_agent.to_string())
            .is_err()
    );
    let mut misspelt = room_policy();
    misspelt["agent"]["abvoe"] = "room".into();
    assert!(control.set_approval_policy(&misspelt.to_string()).is_err());
}
