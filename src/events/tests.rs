use super::*;

// Recorded verbatim from a logos.dev round trip (logosctl 0.3.2, delivery 0.3.3).
const RECEIVED: &str = r#"{"data":{"arg0":"0xe66238ad169a5a65d41eb3f10ef91312d63d9b618ec1f4cc139266e049e749d0","arg1":"/logos-mcp/1/smoke/proto","arg2":{"_bytes":"aGVsbG8gbG9nb3MuZGV2IGZyb20gbG9nb3MtbWNw"},"arg3":"live","arg4":1793978929196749824},"event":"messageReceived","module":"delivery_module","timestamp":"2026-11-06T15:28:49Z"}"#;
const PROPAGATED: &str = r#"{"data":{"arg0":"668f0020ca757c52784e","arg1":"0xe66238ad169a5a65d41eb3f10ef91312d63d9b618ec1f4cc139266e049e749d0","arg2":1793978929440760000},"event":"messagePropagated","module":"delivery_module","timestamp":"2026-11-06T15:28:49Z"}"#;
const SENT: &str = r#"{"data":{"arg0":"668f0020ca757c52784e","arg1":"0xe66238ad169a5a65d41eb3f10ef91312d63d9b618ec1f4cc139266e049e749d0","arg2":1793978933825487000},"event":"messageSent","module":"delivery_module","timestamp":"2026-11-06T15:28:53Z"}"#;
const CONNECTED: &str = r#"{"data":{"arg0":"Connected","arg1":1793978923805405000},"event":"connectionStateChanged","module":"delivery_module","timestamp":"2026-11-06T15:28:43Z"}"#;

#[test]
fn stores_received_message_with_decoded_text() {
    let s = EventStore::default();
    s.ingest_line(RECEIVED);
    let msgs = s.read("/logos-mcp/1/smoke/proto", 0, 10);
    assert_eq!(msgs.len(), 1);
    assert_eq!(
        msgs[0].text.as_deref(),
        Some("hello logos.dev from logos-mcp")
    );
    assert_eq!(msgs[0].source, "live");
    assert_eq!(msgs[0].seq, 1);
    assert!(s.read("/other/1/topic/proto", 0, 10).is_empty());
}

#[test]
fn duplicate_deliveries_of_one_message_are_stored_once() {
    let s = EventStore::default();
    s.ingest_line(RECEIVED);
    s.ingest_line(RECEIVED);
    s.ingest_line(RECEIVED);
    assert_eq!(s.read("/logos-mcp/1/smoke/proto", 0, 10).len(), 1);
}

#[test]
fn send_status_merges_events_in_any_order() {
    let s = EventStore::default();
    s.ingest_line(PROPAGATED); // propagated arrived before sent in the real run
    s.ingest_line(SENT);
    let st = s.send_status("668f0020ca757c52784e").unwrap();
    assert!(st.sent && st.propagated && st.error.is_none());
    assert_eq!(
        st.message_hash.as_deref(),
        Some("0xe66238ad169a5a65d41eb3f10ef91312d63d9b618ec1f4cc139266e049e749d0")
    );
}

#[test]
fn records_send_errors() {
    let s = EventStore::default();
    s.ingest_line(r#"{"data":{"arg0":"r1","arg1":"0xabc","arg2":"no_usable_membership","arg3":1},"event":"messageError","module":"delivery_module"}"#);
    assert_eq!(
        s.send_status("r1").unwrap().error.as_deref(),
        Some("no_usable_membership")
    );
}

#[test]
fn ignores_garbage_and_unrelated_events() {
    let s = EventStore::default();
    s.ingest_line("not json");
    s.ingest_line(r#"{"event":"messageReceived"}"#);
    s.ingest_line(r#"{"event":"messageReceived","data":{"arg0":"0x1"}}"#);
    s.ingest_line(CONNECTED);
    assert!(s.read("/logos-mcp/1/smoke/proto", 0, 10).is_empty());
}

#[test]
fn read_cursor_and_ring_buffer_bound() {
    let s = EventStore::default();
    for i in 0..(MAX_MESSAGES_PER_TOPIC + 10) {
        s.ingest_line(&RECEIVED.replace("0xe662", &format!("0x{i:04}")));
    }
    let all = s.read("/logos-mcp/1/smoke/proto", 0, usize::MAX);
    assert_eq!(all.len(), MAX_MESSAGES_PER_TOPIC);
    assert_eq!(all[0].seq, 11, "oldest entries are evicted first");
    let after = s.read("/logos-mcp/1/smoke/proto", all[100].seq, 5);
    assert_eq!(after.len(), 5);
    assert_eq!(after[0].seq, all[101].seq);
}

#[test]
fn tracks_upload_to_completion_with_cid() {
    let s = EventStore::default();
    s.expect_transfer("sess-1", "upload");
    assert!(!s.transfer("sess-1").unwrap().done);
    s.ingest_line(r#"{"data":{"arg0":"{\"success\":true,\"sessionId\":\"sess-1\",\"bytes\":10,\"total\":20}"},"event":"storageUploadProgress","module":"storage_module"}"#);
    s.ingest_line(r#"{"data":{"arg0":"{\"success\":true,\"sessionId\":\"sess-1\",\"cid\":\"zDvZRwzm\"}"},"event":"storageUploadDone","module":"storage_module"}"#);
    let t = s.transfer("sess-1").unwrap();
    assert_eq!(
        (t.done, t.success, t.cid.as_deref(), t.bytes),
        (true, Some(true), Some("zDvZRwzm"), Some(10))
    );
}

#[test]
fn reset_transfer_clears_a_finished_run_under_the_same_id() {
    let s = EventStore::default();
    s.reset_transfer("zCid", "download");
    s.ingest_line(r#"{"data":{"arg0":"{\"success\":true,\"sessionId\":\"zCid\"}"},"event":"storageDownloadDone","module":"storage_module"}"#);
    assert!(s.transfer("zCid").unwrap().done);
    s.reset_transfer("zCid", "download");
    let t = s.transfer("zCid").unwrap();
    assert!(!t.done && t.success.is_none());
}

#[test]
fn evicts_oldest_tracked_sends() {
    let s = EventStore::default();
    for i in 0..(MAX_TRACKED_SENDS + 1) {
        s.ingest_line(&SENT.replace("668f0020ca757c52784e", &format!("req{i}")));
    }
    assert!(s.send_status("req0").is_none());
    assert!(s.send_status(&format!("req{MAX_TRACKED_SENDS}")).is_some());
}
