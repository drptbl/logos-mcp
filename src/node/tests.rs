use super::*;
use serde_json::json;

// Recorded `logosctl --json status` (logosctl 0.3.2), trimmed to three modules.
const STATUS: &str = r#"{"daemon":{"pid":80675,"status":"running","version":"1.0.0"},"modules":[{"name":"lez_core","status":"loaded","uptime_seconds":320,"version":"0.5.1"},{"name":"delivery_module","status":"loaded","uptime_seconds":1200,"version":"0.3.3"},{"name":"storage_module","status":"not_loaded"}],"modules_summary":{"crashed":0,"loaded":2,"not_loaded":1}}"#;

#[test]
fn reads_logosctl_status_into_a_typed_status() {
    let s = NodeStatus::from_logosctl(&serde_json::from_str(STATUS).unwrap());
    assert_eq!(s.instance.as_deref(), Some("80675"));
    assert_eq!(
        s.module("lez_core").and_then(|m| m.uptime_seconds),
        Some(320)
    );
    assert_eq!(
        s.module("storage_module").map(|m| m.status.as_str()),
        Some("not_loaded")
    );
    assert_eq!(
        s.module("storage_module").and_then(|m| m.uptime_seconds),
        None
    );
    assert!(s.module("libp2p_module").is_none());
    assert_eq!(s.details["modules_summary"]["loaded"], 2);
}

#[test]
fn a_status_with_nothing_observable_stays_empty() {
    let s = NodeStatus::from_logosctl(&json!({}));
    assert!(s.instance.is_none() && s.modules.is_empty());
}
