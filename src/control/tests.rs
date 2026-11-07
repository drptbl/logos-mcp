use super::*;

#[test]
fn every_tool_but_node_status_belongs_to_a_group() {
    let groups = ToolGroups::default();
    for (tool, group) in [
        ("messaging_send", "messaging"),
        ("rln_status", "messaging"),
        ("storage_upload", "storage"),
        ("wallet_accounts", "wallet"),
        ("wallet_new_account", "wallet"),
        ("wallet_transfer", "transfers"),
        ("discovery_lookup", "discovery"),
    ] {
        assert_eq!(ToolGroups::group_of(tool), Some(group), "{tool}");
        assert!(groups.allows(tool), "{tool} is on by default");
    }
    assert_eq!(ToolGroups::group_of("node_status"), None);
}

#[test]
fn a_group_turned_off_disallows_only_its_tools() {
    let mut groups = ToolGroups::default();
    groups.set("transfers", false).unwrap();
    assert!(!groups.allows("wallet_transfer"));
    assert!(groups.allows("wallet_accounts"));
    assert!(groups.allows("node_status"), "never gated");
    assert!(groups.set("everything", false).is_err());
}

#[test]
fn groups_round_trip_through_json_with_missing_ones_on() {
    let groups: ToolGroups = serde_json::from_str(r#"{"storage":false}"#).unwrap();
    assert!(!groups.allows("storage_upload"));
    assert!(groups.allows("messaging_send"));
    let back: ToolGroups = serde_json::from_value(serde_json::to_value(&groups).unwrap()).unwrap();
    assert_eq!(back, groups);
}

#[test]
fn activity_is_listed_after_a_sequence_number() {
    let log = ActivityLog::in_memory();
    log.record(
        "wallet_status",
        &serde_json::json!({}),
        true,
        "{\"open\":true}",
    );
    log.record(
        "wallet_transfer",
        &serde_json::json!({ "to": "Public/abc", "amount_lepta": "5" }),
        false,
        "refusing to send",
    );
    let all = log.since(0);
    assert_eq!(all.len(), 2);
    assert_eq!((all[0].seq, all[1].seq), (1, 2));
    assert_eq!(all[1].tool, "wallet_transfer");
    assert!(!all[1].ok);
    assert!(all[1].args.contains("Public/abc"), "{}", all[1].args);
    assert_eq!(all[1].result, "refusing to send");
    assert!(all[1].time > 1_700_000_000, "unix seconds");
    let newer = log.since(1);
    assert_eq!(newer.len(), 1);
    assert_eq!(newer[0].seq, 2);
}

#[test]
fn long_arguments_and_results_are_cut() {
    let log = ActivityLog::in_memory();
    let long = "é".repeat(1000);
    log.record(
        "messaging_send",
        &serde_json::json!({ "text": long }),
        true,
        &long,
    );
    let e = &log.since(0)[0];
    assert!(
        e.args.chars().count() <= MAX_DETAIL_CHARS + 1,
        "{}",
        e.args.len()
    );
    assert!(e.result.ends_with('…'));
}

#[test]
fn activity_survives_a_restart_and_keeps_counting() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("activity.jsonl");
    {
        let log = ActivityLog::open(file.clone()).unwrap();
        log.record("node_status", &serde_json::json!({}), true, "ok");
        log.record("wallet_status", &serde_json::json!({}), true, "ok");
    }
    let log = ActivityLog::open(file).unwrap();
    assert_eq!(log.since(0).len(), 2);
    log.record("storage_space", &serde_json::json!({}), true, "ok");
    assert_eq!(log.since(2)[0].seq, 3);
}

#[test]
fn a_full_activity_file_is_rotated() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("activity.jsonl");
    let log = ActivityLog::open(file.clone()).unwrap();
    let big = "x".repeat(MAX_DETAIL_CHARS);
    let lines = ROTATE_BYTES / (2 * MAX_DETAIL_CHARS as u64) + 2;
    for _ in 0..lines {
        log.record(
            "messaging_send",
            &serde_json::json!({ "t": big }),
            true,
            &big,
        );
    }
    assert!(dir.path().join("activity.1.jsonl").exists());
    assert!(std::fs::metadata(&file).unwrap().len() < ROTATE_BYTES);
}

#[test]
fn a_torn_last_line_neither_blocks_startup_nor_eats_the_next_entry() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("activity.jsonl");
    {
        let log = ActivityLog::open(file.clone()).unwrap();
        log.record("node_status", &serde_json::json!({}), true, "ok");
    }
    // A crash mid-append: half a line, cut inside a multi-byte character.
    let mut bytes = std::fs::read(&file).unwrap();
    bytes.extend_from_slice("{\"seq\":2,\"tool\":\"é".as_bytes());
    bytes.pop();
    std::fs::write(&file, bytes).unwrap();

    let log = ActivityLog::open(file.clone()).unwrap();
    log.record("wallet_status", &serde_json::json!({}), true, "ok");
    let reopened = ActivityLog::open(file).unwrap();
    let tools: Vec<String> = reopened.since(0).into_iter().map(|e| e.tool).collect();
    assert_eq!(tools, ["node_status", "wallet_status"]);
}

#[test]
fn numbering_continues_from_the_rotated_file() {
    // A crash right after rotating leaves only activity.1.jsonl.
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("activity.jsonl");
    {
        let log = ActivityLog::open(file.clone()).unwrap();
        log.record("node_status", &serde_json::json!({}), true, "ok");
        log.record("node_status", &serde_json::json!({}), true, "ok");
    }
    std::fs::rename(&file, dir.path().join("activity.1.jsonl")).unwrap();
    let log = ActivityLog::open(file).unwrap();
    log.record("wallet_status", &serde_json::json!({}), true, "ok");
    assert_eq!(log.latest(), 3);
}

#[test]
fn an_oversized_tool_name_is_cut_too() {
    let log = ActivityLog::in_memory();
    log.record(
        &"x".repeat(100_000),
        &serde_json::json!({}),
        false,
        "unknown tool",
    );
    assert!(log.since(0)[0].tool.chars().count() <= MAX_DETAIL_CHARS + 1);
}

#[test]
fn message_text_shows_in_the_app_but_never_reaches_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("activity.jsonl");
    let log = ActivityLog::open(file.clone()).unwrap();
    log.record(
        "messaging_send",
        &serde_json::json!({ "topic": "/a/1/b/proto", "text": "meet at the old mill" }),
        true,
        r#"{"request_id":"r1"}"#,
    );
    log.record(
        "messaging_send",
        &serde_json::json!({ "topic": "/a/1/b/proto", "base64": "c2VjcmV0" }),
        true,
        r#"{"request_id":"r2"}"#,
    );
    let read = serde_json::json!({
        "messages": [
            { "text": "the password is swordfish", "payload_base64": "eA==", "seq": 1 },
            { "text": "see you", "payload_base64": "eQ==", "seq": 2 }
        ],
        "next_after": 2
    });
    log.record(
        "messaging_read",
        &serde_json::json!({ "topic": "/a/1/b/proto" }),
        true,
        &read.to_string(),
    );
    // A malformed payload is still someone's message.
    log.record(
        "messaging_send",
        &serde_json::json!({ "topic": "/a/1/b/proto", "text": ["private note"] }),
        false,
        "give exactly one of `text` or `base64`",
    );

    // The app sees what the agent said, while the module runs.
    let live = format!("{:?}", log.since(0));
    for said in ["old mill", "swordfish", "private note"] {
        assert!(live.contains(said), "{said} missing from the app view");
    }

    // The file, and so everything after a restart, keeps only what happened.
    let on_disk = std::fs::read_to_string(&file).unwrap();
    for secret in [
        "old mill",
        "c2VjcmV0",
        "swordfish",
        "see you",
        "private note",
    ] {
        assert!(
            !on_disk.contains(secret),
            "{secret} reached the file: {on_disk}"
        );
    }
    let reopened = ActivityLog::open(file).unwrap().since(0);
    assert!(
        reopened[0].args.contains("/a/1/b/proto"),
        "{}",
        reopened[0].args
    );
    assert!(
        reopened[0].args.contains("<hidden, 20 chars>"),
        "{}",
        reopened[0].args
    );
    assert_eq!(reopened[2].result, "2 messages, next_after 2");
}

#[tokio::test]
async fn waiting_requests_are_capped() {
    let approvals = Approvals::default();
    for _ in 0..MAX_WAITING {
        approvals.request("Public/a", "Public/b", "1").unwrap();
    }
    let e = approvals.request("Public/a", "Public/b", "1").unwrap_err();
    assert!(e.contains("waiting"), "{e}");
}
