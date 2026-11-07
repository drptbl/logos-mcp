use super::*;
use std::sync::Mutex;

/// Replays canned outputs in order and records every argv it was given.
#[derive(Default)]
pub struct FakeRunner {
    pub calls: Mutex<Vec<Vec<String>>>,
    pub replies: Mutex<Vec<RawOutput>>,
}

impl FakeRunner {
    pub fn with(replies: Vec<RawOutput>) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::default(),
            replies: Mutex::new(replies),
        })
    }
}

impl Runner for FakeRunner {
    fn run(&self, args: Vec<String>) -> RunFuture<'_> {
        self.calls.lock().unwrap().push(args);
        let mut replies = self.replies.lock().unwrap();
        let next = if replies.is_empty() {
            RawOutput {
                code: 1,
                stdout: String::new(),
                stderr: "no canned reply".into(),
            }
        } else {
            replies.remove(0)
        };
        Box::pin(async move { Ok(next) })
    }
}

pub fn ok(stdout: &str) -> RawOutput {
    RawOutput {
        code: 0,
        stdout: stdout.into(),
        stderr: String::new(),
    }
}

#[test]
fn encodes_args_as_logosctl_expects() {
    assert_eq!(
        Arg::str(r#"{"mode":"Edge"}"#).encode(),
        r#"str:{"mode":"Edge"}"#
    );
    assert_eq!(Arg::str("42").encode(), "str:42");
    assert_eq!(Arg::Int(65536).encode(), "65536");
    assert_eq!(Arg::Bool(false).encode(), "false");
    // "hello logos" in unpadded base64url, inside the tagged-bytes form.
    assert_eq!(
        Arg::Bytes(b"hello logos".to_vec()).encode(),
        r#"json:{"_bytes":"aGVsbG8gbG9nb3M"}"#
    );
}

#[test]
fn encodes_args_as_json_values_for_in_process_calls() {
    // A string stays a string however it looks: no prefix, no coercion.
    assert_eq!(Arg::str("42").to_json(), json!("42"));
    assert_eq!(Arg::Int(65536).to_json(), json!(65536));
    assert_eq!(Arg::Bool(false).to_json(), json!(false));
    assert_eq!(
        Arg::Bytes(b"hello logos".to_vec()).to_json(),
        json!({ "_bytes": "aGVsbG8gbG9nb3M" })
    );
}

#[test]
fn unwraps_ok_envelope_from_real_output() {
    let raw = ok(r#"{"method":"isRunning","module":"storage_module","result":true,"status":"ok"}"#);
    assert_eq!(parse_output(&raw).unwrap(), Value::Bool(true));
}

#[test]
fn unwraps_inner_success_value() {
    let raw = ok(
        r#"{"method":"send","module":"delivery_module","result":{"error":null,"success":true,"value":"8b919ca2ada465bb4670"},"status":"ok"}"#,
    );
    let v = unwrap_std_result(parse_output(&raw).unwrap()).unwrap();
    assert_eq!(v, Value::String("8b919ca2ada465bb4670".into()));
}

#[test]
fn inner_failure_with_null_error_is_a_refusal_not_a_panic() {
    let raw = ok(
        r#"{"method":"createNode","module":"delivery_module","result":{"error":null,"success":false,"value":null},"status":"ok"}"#,
    );
    let err = unwrap_std_result(parse_output(&raw).unwrap()).unwrap_err();
    assert!(
        matches!(err, LogosError::Refused(ref m) if m == "no detail given"),
        "{err}"
    );
}

#[test]
fn inner_failure_carries_the_module_error() {
    let raw = ok(
        r#"{"method":"start","module":"delivery_module","result":{"error":"Context not initialized","success":false,"value":null},"status":"ok"}"#,
    );
    let err = unwrap_std_result(parse_output(&raw).unwrap()).unwrap_err();
    assert!(matches!(err, LogosError::Refused(ref m) if m == "Context not initialized"));
}

#[test]
fn maps_exit_codes_to_typed_errors() {
    let no_daemon = RawOutput {
        code: 2,
        stdout: r#"{"status":"error","code":"NO_DAEMON","message":"No daemon running (no local endpoint at x)."}"#.into(),
        stderr: String::new(),
    };
    assert!(
        matches!(parse_output(&no_daemon), Err(LogosError::DaemonUnavailable(m)) if m.starts_with("NO_DAEMON"))
    );

    let method = RawOutput {
        code: 4,
        stdout: r#"{"status":"error","code":"METHOD_FAILED","error":{"code":"invalid_args","message":"bad","origin":"x"}}"#.into(),
        stderr: String::new(),
    };
    assert!(
        matches!(parse_output(&method), Err(LogosError::Method(m)) if m == "METHOD_FAILED (invalid_args)")
    );

    let not_loaded = RawOutput {
        code: 3,
        stdout: r#"{"code":"MODULE_NOT_LOADED","message":"Module 'delivery_module' is not loaded.","status":"error"}"#.into(),
        stderr: String::new(),
    };
    assert!(matches!(
        parse_output(&not_loaded),
        Err(LogosError::Module(_))
    ));
}

#[test]
fn decodes_double_encoded_json_results() {
    let raw = ok(
        r#"{"method":"wallet_status","module":"liblogos_lez_rln_module","result":"{\"network\":\"testnet\",\"ready\":true}","status":"ok"}"#,
    );
    let v = embedded_json(parse_output(&raw).unwrap());
    assert_eq!(v["ready"], Value::Bool(true));
    assert_eq!(
        embedded_json(Value::String("plain".into())),
        Value::String("plain".into())
    );
}

#[tokio::test]
async fn call_builds_argv_with_json_flag_first() {
    let fake = FakeRunner::with(vec![ok(
        r#"{"method":"send","module":"delivery_module","result":{"error":null,"success":true,"value":"id1"},"status":"ok"}"#,
    )]);
    let ctl = Logosctl::new(fake.clone());
    let v = ctl
        .call(
            "delivery_module",
            "send",
            &[Arg::str("/a/1/b/proto"), Arg::Bytes(b"hi".to_vec())],
        )
        .await
        .unwrap();
    assert_eq!(v, Value::String("id1".into()));
    assert_eq!(
        fake.calls.lock().unwrap()[0],
        vec![
            "--json",
            "call",
            "delivery_module",
            "send",
            "str:/a/1/b/proto",
            r#"json:{"_bytes":"aGk"}"#
        ]
    );
}

#[tokio::test]
async fn ensure_loaded_only_loads_missing_modules() {
    let fake = FakeRunner::with(vec![
        ok(r#"[{"name":"storage_module","status":"loaded"}]"#),
        ok(
            r#"{"dependencies_loaded":[],"module":"delivery_module","status":"ok","version":"0.3.3"}"#,
        ),
    ]);
    let ctl = Logosctl::new(fake.clone());
    assert!(ctl.ensure_loaded("delivery_module").await.unwrap());
    let calls = fake.calls.lock().unwrap();
    assert_eq!(
        calls[1],
        vec!["--json", "module", "load", "delivery_module"]
    );
}
