use super::*;
use crate::logosctl::tests::{FakeRunner, ok as out};

const ED: &str = "e1d84ff9222620551d7a1c15ce821c5ce97fd51e81fca106ec11f244b65a45f4";
const X: &str = "8b97aef6d517092edca074fad0e3aad0eff3f1e1e8b581b0b2d711ecafddcb08";
const HUMAN: &str = "0b00086682a0d169f90095b9d58e6c003ee591531ccf8b59c712c1cb70603269";
const TOPIC: &str = "/muster/1/agent-spike2-1794773136/proto";

fn muster(method: &str, value: serde_json::Value) -> RawOutput {
    let result = match value {
        serde_json::Value::String(s) => s,
        other => other.to_string(),
    };
    out(&serde_json::json!({"method": method, "module": "muster_module", "result": result, "status": "ok"}).to_string())
}

const NO_DAEMON: &str = r#"{"daemon":{"reason":"No daemon running"}}"#;
const RUNNING: &str = r#"{"daemon":{"pid":4242,"status":"running"},"modules":[]}"#;

fn argv(fake: &FakeRunner) -> Vec<String> {
    fake.calls
        .lock()
        .unwrap()
        .iter()
        .map(|c| c.join(" "))
        .collect()
}

#[tokio::test]
async fn setting_up_installs_muster_without_its_optional_extras_and_opens_the_inbox() {
    let fake = FakeRunner::with(vec![
        out(NO_DAEMON),
        out("Daemon started (pid 4242)"),
        out(r#"[{"name":"logos-modules-official","enabled":true}]"#),
        out(r#"{"success":true}"#),
        out(r#"{"status":"ok"}"#),
        out(r#"{"status":"ok"}"#),
        muster(
            "coordinate_start_inbox",
            serde_json::json!({"inbox": "/muster/1/muster.inbox.9/proto", "rooms": []}),
        ),
    ]);
    Seat::new(fake.clone()).set_up().await.unwrap();
    assert_eq!(
        argv(&fake),
        [
            "--json status".to_string(),
            "-D -d".into(),
            "--json catalog ls".into(),
            format!("--json catalog add {MUSTER_CATALOG}"),
            "--json package install muster_module --no-optional -y".into(),
            "--json module load muster_module --no-optional".into(),
            "--json call muster_module coordinate_start_inbox".into(),
        ]
    );
}

#[tokio::test]
async fn starting_a_running_seat_leaves_its_daemon_and_catalog_alone() {
    let fake = FakeRunner::with(vec![
        out(RUNNING),
        out(r#"{"status":"ok"}"#),
        muster(
            "coordinate_start_inbox",
            serde_json::json!({"inbox": "x", "rooms": [TOPIC]}),
        ),
    ]);
    Seat::new(fake.clone()).start().await.unwrap();
    assert_eq!(
        argv(&fake),
        [
            "--json status".to_string(),
            "--json module load muster_module --no-optional".into(),
            "--json call muster_module coordinate_start_inbox".into(),
        ]
    );
}

#[tokio::test]
async fn the_status_gives_the_invite_id_rooms_and_invites() {
    let fake = FakeRunner::with(vec![
        out(RUNNING),
        muster("identity", serde_json::json!({"ed25519": ED, "x25519": X})),
        muster(
            "coordinate_conversations",
            serde_json::json!([{"topic": TOPIC, "title": "Treasury", "active": true, "people": ["x"]}]),
        ),
        muster(
            "coordinate_invites",
            serde_json::json!([{"topic": "/muster/1/other/proto", "from": format!("{HUMAN}{X}"), "fromAlias": "", "note": "hi", "ts": 1}]),
        ),
    ]);
    let status = Seat::new(fake).status().await;
    assert_eq!(status["running"], true);
    assert_eq!(status["key"], format!("ed:{ED}"));
    assert_eq!(status["invite_id"], format!("{ED}{X}"));
    assert_eq!(status["rooms"][0]["topic"], TOPIC);
    assert_eq!(status["rooms"][0]["title"], "Treasury");
    assert_eq!(status["invites"][0]["topic"], "/muster/1/other/proto");
    assert_eq!(status["invites"][0]["from"], format!("ed:{HUMAN}"));
}

#[tokio::test]
async fn a_seat_not_running_says_so() {
    let fake = FakeRunner::with(vec![out(NO_DAEMON)]);
    let status = Seat::new(fake).status().await;
    assert_eq!(status["running"], false);
}

#[tokio::test]
async fn stopping_stops_the_daemon() {
    let fake = FakeRunner::with(vec![out(
        r#"{"message":"Daemon shutting down.","status":"ok"}"#,
    )]);
    Seat::new(fake.clone()).stop().await;
    assert_eq!(argv(&fake), ["--json daemon stop"]);
}

/// The fake, except that installing waits for `gate`.
struct SlowInstall {
    fake: Arc<FakeRunner>,
    gate: Arc<tokio::sync::Notify>,
}

impl Runner for SlowInstall {
    fn run(&self, args: Vec<String>) -> crate::logosctl::RunFuture<'_> {
        let installing = args.iter().any(|a| a == "install");
        let reply = self.fake.run(args);
        Box::pin(async move {
            if installing {
                self.gate.notified().await;
            }
            reply.await
        })
    }
}

#[tokio::test]
async fn while_setting_up_the_status_says_so_and_another_set_up_is_refused() {
    let fake = FakeRunner::with(vec![
        out(NO_DAEMON),
        out("Daemon started (pid 4242)"),
        out(r#"[{"name":"muster","enabled":true}]"#),
        out(r#"{"status":"ok"}"#), // install, once the gate opens
        out(NO_DAEMON),            // status, meanwhile
        out(r#"{"status":"ok"}"#), // module load
        muster(
            "coordinate_start_inbox",
            serde_json::json!({"inbox": "x", "rooms": []}),
        ),
    ]);
    let gate = Arc::new(tokio::sync::Notify::new());
    let seat = Arc::new(Seat::new(Arc::new(SlowInstall {
        fake: fake.clone(),
        gate: gate.clone(),
    })));
    let setting_up = tokio::spawn({
        let seat = seat.clone();
        async move { seat.set_up().await }
    });
    while !argv(&fake).iter().any(|c| c.contains("install")) {
        tokio::task::yield_now().await;
    }
    assert_eq!(seat.status().await["setting_up"], true);
    assert!(seat.set_up().await.is_err());
    gate.notify_one();
    setting_up.await.unwrap().unwrap();
    assert_eq!(
        argv(&fake).iter().filter(|c| c.contains("install")).count(),
        1
    );
}

#[tokio::test]
async fn a_failed_set_up_leaves_its_error_in_the_status() {
    let fake = FakeRunner::with(vec![
        out(NO_DAEMON),
        out("Daemon started (pid 4242)"),
        out(r#"[{"name":"muster","enabled":true}]"#),
        // installing gets no reply: it fails
    ]);
    let seat = Seat::new(fake);
    let error = seat.set_up().await.unwrap_err();
    let status = seat.status().await;
    assert_eq!(status["setting_up"], serde_json::Value::Null);
    assert_eq!(status["setup_error"], error);
}
