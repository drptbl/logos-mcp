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
        let resp = router(|| Ok(Probe), &token).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 401);
    }
}

#[tokio::test]
async fn the_right_token_reaches_the_mcp_server() {
    let token = Token::from_hex("ab".repeat(32)).unwrap();
    let resp = router(|| Ok(Probe), &token)
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
    let resp = router(|| Ok(Probe), &token).oneshot(req).await.unwrap();
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
