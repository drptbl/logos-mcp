//! logos-mcp inside Basecamp: the library's MCP tools served over local HTTP,
//! reaching Basecamp's own modules in-process. This crate is only the shell:
//! the in-process Node adapter and the module's hooks.

include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/generated/provider_gen.rs"
));

use std::{path::PathBuf, sync::Arc};

use logos_mcp::{
    events::EventStore,
    logosctl::{unwrap_std_result, Arg, LogosError},
    module::{router, sdk_error, Control},
    node::{Node, NodeFuture, NodeStatus},
    seat::{find_logosctl, Seat},
    server::{LogosServer, NodeSettings, WalletMode},
};
use logos_rust_sdk::LogosModuleSDK;
use serde_json::{json, Value};

/// Events the tools read, per module.
const WATCHED: &[(&str, &[&str])] = &[
    (
        "delivery_module",
        &[
            "messageReceived",
            "messageQueued",
            "messageSent",
            "messagePropagated",
            "messageError",
        ],
    ),
    (
        "storage_module",
        &[
            "storageUploadProgress",
            "storageUploadDone",
            "storageDownloadProgress",
            "storageDownloadDone",
            // Ends the agent's claim on content anyone removes.
            "storageRemoveDone",
        ],
    ),
    // Each new finalized tip, for chain_events. Subscribed again until the
    // Blockchain app has loaded it.
    ("blockchain_module", &["libBlock"]),
];

/// Per-call bound, the same as the standalone binary's logosctl default.
const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Calls Basecamp's modules in-process through the Logos Rust SDK.
struct InProcessNode;

impl Node for InProcessNode {
    fn call<'a>(
        &'a self,
        module: &'a str,
        method: &'a str,
        args: &'a [Arg],
    ) -> NodeFuture<'a, Value> {
        let (module, method) = (module.to_string(), method.to_string());
        let name = module.clone();
        let args = Value::Array(args.iter().map(Arg::to_json).collect());
        Box::pin(async move {
            // The SDK call blocks: keep it off the async workers.
            let raw = tokio::task::spawn_blocking(move || {
                LogosModuleSDK::new()
                    .plugin(&module)
                    .call_json_with_timeout(&method, &args, CALL_TIMEOUT)
            })
            .await
            .map_err(|e| LogosError::Method(e.to_string()))?
            .map_err(|e| sdk_error(&name, e.to_string()))?;
            unwrap_std_result(raw)
        })
    }

    fn status(&self) -> NodeFuture<'_, NodeStatus> {
        // Basecamp has no daemon to report on: say so rather than invent one.
        Box::pin(async {
            Ok(NodeStatus {
                details: json!({ "daemon": { "host": "basecamp" }, "modules_summary": null }),
                ..NodeStatus::default()
            })
        })
    }

    fn ensure_loaded<'a>(&'a self, _module: &'a str) -> NodeFuture<'a, bool> {
        // Basecamp loads modules; a missing one shows up as a failed call.
        Box::pin(async { Ok(false) })
    }

    fn watch(&self, module: &'static str, store: Arc<EventStore>) {
        let Some((_, events)) = WATCHED.iter().find(|(m, _)| *m == module) else {
            return;
        };
        for event in events.iter().copied() {
            let store = store.clone();
            std::thread::spawn(move || {
                // The module may not be loaded yet: keep trying, but say so once.
                let mut warned = false;
                loop {
                    match LogosModuleSDK::new().plugin(module).on(event) {
                        Ok(sub) => {
                            warned = false;
                            for ev in sub {
                                store.apply_event(&ev.event, &ev.data);
                            }
                        }
                        Err(e) if !warned => {
                            warned = true;
                            eprintln!("logos_mcp: cannot subscribe to {module}.{event} yet: {e}");
                        }
                        Err(_) => {}
                    }
                    std::thread::sleep(std::time::Duration::from_secs(5));
                }
            });
        }
    }
}

/// The module's control surface, once its data folder has been read.
static CONTROL: std::sync::OnceLock<Control> = std::sync::OnceLock::new();
/// The MCP server and the runtime it runs on, for the app's wallet calls. Clones
/// share the server's wallet lock and state, so the app and agents never race.
static WALLET: std::sync::OnceLock<(LogosServer, tokio::runtime::Handle)> =
    std::sync::OnceLock::new();
/// Why the endpoint isn't serving, when it isn't.
static ERROR: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
/// The agent's own seat in Muster: Basecamp's one Muster is the user's.
static SEAT: std::sync::OnceLock<Seat> = std::sync::OnceLock::new();
/// How long one logosctl call to the agent's seat may take. Installing Muster
/// has its own, longer limit.
const SEAT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
static STOP: std::sync::OnceLock<tokio_util::sync::CancellationToken> = std::sync::OnceLock::new();

fn start(data_dir: PathBuf) -> Result<(), String> {
    let control = CONTROL.get().ok_or("the data folder was not opened")?;
    let config = control.config();
    let files = config.files_root(&data_dir)?;
    for peer in &config.discovery_bootstrap {
        logos_mcp::guard::bootstrap_node(peer)?;
    }
    let settings = NodeSettings {
        preset: "chosen by Basecamp".into(),
        mode: "Edge".into(),
        config_dir: None,
        wallet: Some(WalletMode::Shared {
            agent_accounts_file: data_dir.join("agent-accounts.json"),
        }),
        discovery_bootstrap: config.discovery_bootstrap.clone(),
        host_owns_messaging: true,
        delivery_config: None,
    };
    let events = Arc::new(EventStore::default());
    let node: Arc<dyn Node> = Arc::new(InProcessNode);
    // Room approval goes only through the agent's own seat, never the user's
    // Muster, even before the seat is set up: then it is refused.
    let seat = SEAT.get_or_init(|| {
        Seat::in_dir(
            find_logosctl().unwrap_or_else(|| PathBuf::from("logosctl")),
            control.agent_seat_dir(),
            SEAT_TIMEOUT,
        )
    });
    let server = control
        .attach(LogosServer::new(
            node.clone(),
            events.clone(),
            files,
            settings,
        ))
        .with_muster(seat.node());
    let for_wallet = server.clone();
    let app = router(move || Ok(server.clone()), control.tokens());

    let stop = STOP
        .get_or_init(tokio_util::sync::CancellationToken::new)
        .clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let port = config.port;
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => return tx.send(Err(e.to_string())).unwrap_or(()),
        };
        let _ = WALLET.set((for_wallet, rt.handle().clone()));
        // A panic in the serving loop must reach `status` too, not just end the thread.
        let serving = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            rt.block_on(async move {
                let listener = match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
                    Ok(l) => l,
                    Err(e) => {
                        return tx
                            .send(Err(format!("cannot listen on 127.0.0.1:{port}: {e}")))
                            .unwrap_or(())
                    }
                };
                let _ = tx.send(Ok(()));
                let served = axum::serve(listener, app)
                    .with_graceful_shutdown(stop.clone().cancelled_owned())
                    .await;
                // Anything but our own unload stopping the endpoint is a failure
                // `status` must report, not keep calling "running".
                if !stop.is_cancelled() {
                    not_serving(match served {
                        Ok(()) => "the MCP endpoint stopped unexpectedly".to_string(),
                        Err(e) => format!("the MCP endpoint stopped: {e}"),
                    });
                }
            })
        }));
        if serving.is_err() {
            not_serving("the MCP endpoint crashed".to_string());
        }
    });
    rx.recv().map_err(|e| e.to_string())??;
    if control.agent_seat() {
        if let Some((_, runtime)) = WALLET.get() {
            runtime.spawn(async move {
                if let Err(e) = seat.start().await {
                    eprintln!("logos_mcp: the agent's Muster seat didn't start: {e}");
                }
            });
        }
    }
    // Only once serving: a module that failed to start keeps no subscriptions.
    for (module, _) in WATCHED {
        node.watch(module, events.clone());
    }
    Ok(())
}

/// Record why the endpoint isn't serving, for `status` and Basecamp's log.
fn not_serving(why: String) {
    eprintln!("logos_mcp: {why}");
    if let Ok(mut err) = ERROR.lock() {
        *err = Some(why);
    }
}

fn error_json(e: impl std::fmt::Display) -> String {
    json!({ "error": e.to_string() }).to_string()
}

/// The only module allowed to read the token, change permissions or read activity:
/// the Agent Access app's backend. Anything else, including an unnamed caller, is refused.
const APP_BACKEND: &str = "logos_mcp_app_backend";

/// `f` on the control surface, for the Agent Access app only.
fn for_app(f: impl FnOnce(&Control) -> Result<Value, String>) -> String {
    if !logos_rust_sdk::current_caller().is_module(APP_BACKEND) {
        // Visible in Basecamp's log, so a module probing for the token shows up.
        let who = logos_rust_sdk::current_caller_json().unwrap_or_else(|| "unknown".into());
        eprintln!("logos_mcp: refused a control call from {who}");
        return error_json(format!("only {APP_BACKEND} may call this"));
    }
    with_control(f)
}

/// Run one of the server's wallet calls on its runtime, from the SDK's thread.
fn on_server<F, Fut>(f: F) -> Result<Value, String>
where
    F: FnOnce(LogosServer) -> Fut,
    Fut: std::future::Future<Output = Result<Value, String>>,
{
    let (server, runtime) = WALLET.get().ok_or("the MCP server isn't running")?;
    runtime.block_on(f(server.clone()))
}

/// The agent's seat and the runtime its calls run on, or why there are none.
fn seat() -> Result<(&'static Seat, &'static tokio::runtime::Handle), String> {
    let seat = SEAT.get().ok_or("the agent seat isn't ready")?;
    let (_, runtime) = WALLET.get().ok_or("the MCP server isn't running")?;
    Ok((seat, runtime))
}

/// `f` on the agent's seat, or why there is none.
fn on_seat<F, Fut>(f: F) -> Result<Value, String>
where
    F: FnOnce(&'static Seat) -> Fut,
    Fut: std::future::Future<Output = Result<Value, String>>,
{
    let (seat, runtime) = seat()?;
    runtime.block_on(f(seat))
}

/// The user's own Ed25519 key in Basecamp's Muster, when Muster runs there.
fn users_muster_key() -> Option<String> {
    let raw = LogosModuleSDK::new()
        .plugin("muster_module")
        .call_json_with_timeout("identity", &json!([]), CALL_TIMEOUT)
        .ok()?;
    let identity: Value = match unwrap_std_result(raw).ok()? {
        Value::String(text) => serde_json::from_str(&text).ok()?,
        other => other,
    };
    identity
        .get("ed25519")
        .and_then(Value::as_str)
        .map(|k| format!("ed:{}", k.to_ascii_lowercase()))
}

fn with_control(f: impl FnOnce(&Control) -> Result<Value, String>) -> String {
    match CONTROL.get() {
        Some(control) => f(control).map_or_else(error_json, |v| v.to_string()),
        None => error_json(
            ERROR
                .lock()
                .ok()
                .and_then(|e| e.clone())
                .unwrap_or_else(|| "starting".into()),
        ),
    }
}

#[derive(Default)]
struct ModuleImpl;

impl LogosMcpModule for ModuleImpl {
    fn on_context_ready(&self, ctx: &RustModuleContext) {
        let dir = PathBuf::from(&ctx.instance_persistence_path);
        // Open the data folder first, so a broken config is reported, not a panic.
        let result = Control::open(&dir).and_then(|control| {
            let _ = CONTROL.set(control);
            start(dir)
        });
        if let Err(e) = result {
            eprintln!("logos_mcp: not serving: {e}");
            if let Ok(mut err) = ERROR.lock() {
                *err = Some(e);
            }
        }
    }

    fn about_to_unload(&self) -> logos_rust_sdk::Shutdown {
        // The seat runs while the module does, stopped first, while the runtime
        // still runs. Invites wait in the store meanwhile.
        if let (Some(seat), Some((_, runtime))) = (SEAT.get(), WALLET.get()) {
            runtime.block_on(seat.stop());
        }
        if let Some(stop) = STOP.get() {
            stop.cancel();
        }
        logos_rust_sdk::Shutdown::Synchronous
    }

    fn status(&self) -> String {
        let error = ERROR.lock().ok().and_then(|e| e.clone());
        with_control(|c| Ok(c.status(error.as_deref())))
    }

    fn token(&self) -> String {
        for_app(|c| Ok(json!({ "token": c.tokens().current().as_str() })))
    }

    fn rotate_token(&self) -> String {
        for_app(|c| {
            let token = c.tokens().rotate().map_err(|e| e.to_string())?;
            Ok(json!({ "token": token.as_str() }))
        })
    }

    fn permissions(&self) -> String {
        with_control(|c| Ok(c.permissions()))
    }

    fn set_permission(&self, group: String, enabled: bool) -> String {
        for_app(|c| c.set_permission(&group, enabled))
    }

    fn wallet(&self) -> String {
        for_app(|_| on_server(|s| async move { s.wallet_overview().await }))
    }

    fn set_agent_account(&self, account_id: String, agent: bool) -> String {
        for_app(|_| on_server(|s| async move { s.set_agent_account(&account_id, agent).await }))
    }

    fn new_agent_account(&self) -> String {
        for_app(|_| on_server(|s| async move { s.new_agent_account().await }))
    }

    fn approvals(&self) -> String {
        for_app(|_| on_server(|s| async move { Ok(s.approvals()) }))
    }

    fn approve(&self, request_id: String) -> String {
        for_app(|_| on_server(|s| async move { s.approve_transfer(&request_id).await }))
    }

    fn deny(&self, request_id: String) -> String {
        for_app(|_| on_server(|s| async move { s.deny_transfer(&request_id) }))
    }

    fn cancel_room_request(&self, request_id: String) -> String {
        for_app(|_| on_server(|s| async move { s.cancel_room_request(&request_id) }))
    }

    fn approval_policy(&self) -> String {
        for_app(|c| Ok(c.approval_policy()))
    }

    fn set_approval_policy(&self, policy: String) -> String {
        for_app(|c| c.set_approval_policy(&policy))
    }

    /// The seat's status. While room approval is off it asks nothing of
    /// logosctl or Muster: only how a setup goes.
    fn seat_status(&self) -> String {
        for_app(|c| {
            let (seat, _) = seat()?;
            // Before reading whether it is set up: a setup that finishes in
            // between then shows as set up, never as not set up.
            let setup = seat.setup_status();
            let set_up = c.agent_seat();
            let mut status = if set_up {
                let mut status = on_seat(|seat| async move { Ok(seat.status().await) })?;
                status["you"] = json!(users_muster_key());
                status
            } else {
                setup
            };
            status["set_up"] = json!(set_up);
            status["logosctl"] = json!(find_logosctl());
            Ok(status)
        })
    }

    /// Turns room approval off: room tiers ask the user instead, and waiting
    /// room requests are cancelled. Then, in the background, as the seat may
    /// be busy proposing: the agent's open cards are closed and the seat stops.
    /// Its folder stays, so turning it on again reuses the same seat.
    fn seat_turn_off(&self) -> String {
        for_app(|c| {
            let (seat, runtime) = seat()?;
            let (server, _) = WALLET.get().ok_or("the MCP server isn't running")?;
            let policy = c.turn_off_room_approval()?;
            server.cancel_room_requests();
            let server = server.clone();
            runtime.spawn(async move { server.end_room_approval(seat.stop()).await });
            Ok(policy)
        })
    }

    /// Starts setting the seat up and returns: installing Muster outlasts
    /// Basecamp's call limit. `seat_status` says how it goes.
    fn seat_set_up(&self) -> String {
        for_app(|_| {
            let (seat, runtime) = seat()?;
            seat.set_up_in_background(runtime, || {
                CONTROL
                    .get()
                    .ok_or("the data folder was not opened")?
                    .remember_agent_seat()
            })?;
            Ok(json!({ "setting_up": true }))
        })
    }

    fn seat_join(&self, topic: String) -> String {
        for_app(|_| on_server(|s| async move { s.seat_join(&topic).await.map(|()| json!({ "joined": topic })) }))
    }

    fn room_members(&self, topic: String) -> String {
        for_app(|_| {
            let you = users_muster_key();
            let members = on_server(|s| async move { s.room_members(&topic).await.map(Value::from) })?;
            let marked: Vec<Value> = members
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|mut m| {
                    m["you"] = json!(you.as_deref() == m["key"].as_str());
                    m
                })
                .collect();
            Ok(json!({ "members": marked }))
        })
    }

    fn activity(&self, after: i64) -> String {
        for_app(|c| Ok(c.activity(u64::try_from(after).unwrap_or(0))))
    }
}

#[no_mangle]
pub extern "Rust" fn logos_module_install() {
    install::<ModuleImpl>();
}
