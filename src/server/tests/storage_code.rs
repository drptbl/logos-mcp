//! Storage from code: uploads and downloads that wait for the transfer to
//! finish, for callers such as logos-agents' providers.

use super::*;
use std::time::Duration;

const CID: &str = "zDvZRwzmDMMpY8Qd5LWZGWworQRVDHEZ22co3GX5B8of7k3oGrvU";

fn storage_server(fake: Arc<FakeRunner>, root: &std::path::Path) -> (LogosServer, Arc<EventStore>) {
    let events = Arc::new(EventStore::default());
    let s = LogosServer::new(
        Arc::new(Logosctl::new(fake)),
        events.clone(),
        Some(FilesRoot::new(root).unwrap()),
        wallet_server(FakeRunner::with(vec![])).settings.clone(),
    );
    (s, events)
}

fn storage(method: &str, value: &str) -> crate::logosctl::RawOutput {
    out(&format!(
        r#"{{"method":"{method}","module":"storage_module","result":{{"error":null,"success":true,"value":{value}}},"status":"ok"}}"#
    ))
}

/// A storage event as the module emits it: one JSON document in arg0.
fn emit(events: &EventStore, event: &str, body: Value) {
    events.apply_event(event, &json!({ "arg0": body.to_string() }));
}

fn called(fake: &FakeRunner, method: &str) -> bool {
    fake.calls
        .lock()
        .unwrap()
        .iter()
        .any(|c| c.get(3).map(String::as_str) == Some(method))
}

#[tokio::test(start_paused = true)]
async fn an_upload_waits_for_its_cid() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("out.txt"), b"result").unwrap();
    let fake = FakeRunner::with(vec![storage("uploadUrl", r#""79""#)]);
    let (s, events) = storage_server(fake, dir.path());
    let upload = tokio::spawn({
        let s = s.clone();
        async move { s.upload_file("out.txt", Duration::from_secs(30)).await }
    });
    tokio::time::sleep(Duration::from_secs(1)).await;
    emit(
        &events,
        "storageUploadDone",
        json!({ "sessionId": "79", "success": true, "cid": CID }),
    );
    assert_eq!(upload.await.unwrap(), Ok(CID.to_string()));
}

#[tokio::test(start_paused = true)]
async fn a_failed_upload_says_why() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("out.txt"), b"result").unwrap();
    let fake = FakeRunner::with(vec![storage("uploadUrl", r#""80""#)]);
    let (s, events) = storage_server(fake, dir.path());
    let upload = tokio::spawn({
        let s = s.clone();
        async move { s.upload_file("out.txt", Duration::from_secs(30)).await }
    });
    tokio::time::sleep(Duration::from_secs(1)).await;
    emit(
        &events,
        "storageUploadDone",
        json!({ "sessionId": "80", "success": false, "error": "disk full" }),
    );
    let err = upload.await.unwrap().unwrap_err();
    assert!(err.contains("disk full"), "{err}");
}

#[tokio::test(start_paused = true)]
async fn an_upload_that_outlasts_its_timeout_is_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("out.txt"), b"result").unwrap();
    let fake = FakeRunner::with(vec![
        storage("uploadUrl", r#""81""#),
        storage("uploadCancel", "null"),
    ]);
    let (s, _events) = storage_server(fake.clone(), dir.path());
    let err = s
        .upload_file("out.txt", Duration::from_secs(5))
        .await
        .unwrap_err();
    assert!(err.contains("5 s"), "{err}");
    assert!(called(&fake, "uploadCancel"));
}

#[tokio::test(start_paused = true)]
async fn a_download_waits_for_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeRunner::with(vec![storage("downloadToUrl", &format!("{CID:?}"))]);
    let (s, events) = storage_server(fake, dir.path());
    let download = tokio::spawn({
        let s = s.clone();
        async move {
            s.download_to(CID, "in.bin", Some(1_000), Duration::from_secs(30))
                .await
        }
    });
    tokio::time::sleep(Duration::from_secs(1)).await;
    // storage_module writes the file, then says it's done.
    std::fs::write(dir.path().join("in.bin"), vec![7u8; 300]).unwrap();
    emit(
        &events,
        "storageDownloadProgress",
        json!({ "sessionId": CID, "bytes": 300, "total": 300, "success": true }),
    );
    emit(
        &events,
        "storageDownloadDone",
        json!({ "sessionId": CID, "success": true }),
    );
    let path = download.await.unwrap().unwrap();
    assert_eq!(path, dir.path().canonicalize().unwrap().join("in.bin"));
}

#[tokio::test(start_paused = true)]
async fn a_download_bigger_than_the_cap_is_stopped_and_removed() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeRunner::with(vec![
        storage("downloadToUrl", &format!("{CID:?}")),
        storage("downloadCancel", "null"),
    ]);
    let (s, events) = storage_server(fake.clone(), dir.path());
    let download = tokio::spawn({
        let s = s.clone();
        async move {
            s.download_to(CID, "in.bin", Some(1_000), Duration::from_secs(30))
                .await
        }
    });
    tokio::time::sleep(Duration::from_secs(1)).await;
    std::fs::write(dir.path().join("in.bin"), vec![7u8; 10]).unwrap();
    emit(
        &events,
        "storageDownloadProgress",
        json!({ "sessionId": CID, "bytes": 65536, "total": 5_000_000, "success": true }),
    );
    let err = download.await.unwrap().unwrap_err();
    assert!(err.contains("1000"), "{err}");
    assert!(called(&fake, "downloadCancel"));
    assert!(
        !dir.path().join("in.bin").exists(),
        "the partial file is removed"
    );
}

#[tokio::test(start_paused = true)]
async fn a_finished_download_over_the_cap_is_removed_too() {
    // No total was ever reported: the file's size decides.
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeRunner::with(vec![storage("downloadToUrl", &format!("{CID:?}"))]);
    let (s, events) = storage_server(fake, dir.path());
    let download = tokio::spawn({
        let s = s.clone();
        async move {
            s.download_to(CID, "in.bin", Some(100), Duration::from_secs(30))
                .await
        }
    });
    tokio::time::sleep(Duration::from_secs(1)).await;
    std::fs::write(dir.path().join("in.bin"), vec![7u8; 300]).unwrap();
    emit(
        &events,
        "storageDownloadDone",
        json!({ "sessionId": CID, "success": true }),
    );
    assert!(download.await.unwrap().is_err());
    assert!(!dir.path().join("in.bin").exists());
}

#[tokio::test]
async fn storage_from_code_keeps_the_files_root_rules() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeRunner::with(vec![]);
    let (s, _events) = storage_server(fake.clone(), dir.path());
    assert!(
        s.upload_file("../etc/passwd", Duration::from_secs(1))
            .await
            .is_err()
    );
    assert!(
        s.download_to("not a cid", "in.bin", None, Duration::from_secs(1))
            .await
            .is_err()
    );
    assert!(
        s.download_to(CID, "/tmp/x", None, Duration::from_secs(1))
            .await
            .is_err()
    );
    assert!(fake.calls.lock().unwrap().is_empty());
    let no_root = server(FakeRunner::with(vec![]), None);
    let err = no_root
        .upload_file("out.txt", Duration::from_secs(1))
        .await
        .unwrap_err();
    assert!(err.contains("--files-root"), "{err}");
}

#[test]
fn bytes_are_written_only_into_a_new_file_under_the_root() {
    let dir = tempfile::tempdir().unwrap();
    let root = FilesRoot::new(dir.path()).unwrap();
    let path = root.write_new("job-1/input.txt", b"hello").unwrap_err();
    assert!(path.contains("parent"), "{path}");
    std::fs::create_dir(dir.path().join("job-1")).unwrap();
    let path = root.write_new("job-1/input.txt", b"hello").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"hello");
    assert!(
        root.write_new("job-1/input.txt", b"again").is_err(),
        "never overwrites"
    );
    assert!(root.write_new("../escape.txt", b"x").is_err());
}
