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

/// A seat that outlives the test, as the module's does.
fn seat(runner: Arc<dyn Runner>) -> &'static Seat {
    Box::leak(Box::new(Seat::new(runner)))
}

/// Set `seat` up on this test's runtime, remembering it with `remember`.
fn set_up(
    seat: &'static Seat,
    remember: impl FnOnce() -> Result<(), String> + Send + 'static,
) -> Result<(), String> {
    seat.set_up_in_background(&tokio::runtime::Handle::current(), remember)
}

fn setting_up(seat: &Seat) -> bool {
    matches!(*seat.setup.lock().unwrap(), Setup::Running)
}

/// Until the background setup ends.
async fn settled(seat: &Seat) {
    while setting_up(seat) {
        tokio::task::yield_now().await;
    }
}

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
    let seat = seat(fake.clone());
    set_up(seat, || Ok(())).unwrap();
    settled(seat).await;
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
    let seat = seat(Arc::new(SlowInstall {
        fake: fake.clone(),
        gate: gate.clone(),
    }));
    let remembered = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = remembered.clone();
    set_up(seat, move || {
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    })
    .unwrap();
    // Busy as soon as the call returns, before anything ran.
    assert!(set_up(seat, || Ok(())).is_err());
    while !argv(&fake).iter().any(|c| c.contains("install")) {
        tokio::task::yield_now().await;
    }
    assert_eq!(seat.status().await["setting_up"], true);
    gate.notify_one();
    settled(seat).await;
    assert!(remembered.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(seat.status().await["setup_error"], serde_json::Value::Null);
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
    let seat = seat(fake);
    set_up(seat, || panic!("a failed setup is never remembered")).unwrap();
    settled(seat).await;
    let status = seat.status().await;
    assert_eq!(status["setting_up"], serde_json::Value::Null);
    assert!(
        status["setup_error"]
            .as_str()
            .is_some_and(|e| e.contains("could not install Muster")),
        "{status}"
    );
}

#[tokio::test]
async fn a_seat_installed_but_not_remembered_says_why() {
    let fake = FakeRunner::with(vec![
        out(RUNNING),
        out(r#"[{"name":"muster","enabled":true}]"#),
        out(r#"{"status":"ok"}"#),
        out(r#"{"status":"ok"}"#),
        muster(
            "coordinate_start_inbox",
            serde_json::json!({"inbox": "x", "rooms": []}),
        ),
    ]);
    let seat = seat(fake);
    set_up(seat, || Err("could not save the config: disk full".into())).unwrap();
    settled(seat).await;
    assert_eq!(
        seat.status().await["setup_error"],
        "could not save the config: disk full"
    );
}

#[test]
fn a_setup_stopped_midway_can_be_started_again() {
    let fake = FakeRunner::with(vec![
        out(NO_DAEMON),
        out("Daemon started (pid 4242)"),
        out(r#"[{"name":"muster","enabled":true}]"#),
    ]);
    let seat = seat(Arc::new(SlowInstall {
        fake: fake.clone(),
        gate: Arc::new(tokio::sync::Notify::new()), // never opens
    }));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    seat.set_up_in_background(runtime.handle(), || Ok(()))
        .unwrap();
    runtime.block_on(async {
        while !argv(&fake).iter().any(|c| c.contains("install")) {
            tokio::task::yield_now().await;
        }
    });
    drop(runtime); // drops the setup mid-install
    assert!(!setting_up(seat));
    assert!(matches!(*seat.setup.lock().unwrap(), Setup::Failed(_)));
}

#[tokio::test]
async fn stopping_mid_setup_ends_the_setup_before_the_daemon() {
    let fake = FakeRunner::with(vec![
        out(NO_DAEMON),
        out("Daemon started (pid 4242)"),
        out(r#"[{"name":"muster","enabled":true}]"#),
        out(r#"{"status":"ok"}"#), // install: never answered
        out(r#"{"message":"Daemon shutting down.","status":"ok"}"#),
    ]);
    let seat = seat(Arc::new(SlowInstall {
        fake: fake.clone(),
        gate: Arc::new(tokio::sync::Notify::new()), // never opens
    }));
    set_up(seat, || panic!("a stopped setup is never remembered")).unwrap();
    while !argv(&fake).iter().any(|c| c.contains("install")) {
        tokio::task::yield_now().await;
    }
    seat.stop().await;
    assert_eq!(argv(&fake).last().unwrap(), "--json daemon stop");
    assert!(matches!(*seat.setup.lock().unwrap(), Setup::Failed(_)));
}

#[tokio::test]
async fn a_seat_room_approval_doesnt_use_reports_its_setup_and_asks_nothing() {
    let fake = FakeRunner::with(vec![]); // every logosctl call fails
    let seat = seat(fake.clone());
    assert_eq!(seat.setup_status(), serde_json::json!({"running": false}));
    assert!(argv(&fake).is_empty());
    set_up(seat, || Ok(())).unwrap();
    settled(seat).await;
    let calls = argv(&fake).len();
    let status = seat.setup_status();
    assert_eq!(status["running"], false);
    assert!(status["setup_error"].is_string(), "{status}");
    assert_eq!(argv(&fake).len(), calls);
}
