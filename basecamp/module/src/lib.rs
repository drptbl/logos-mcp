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
    module::{router, sdk_error, ModuleConfig, Token},
    node::{Node, NodeFuture, NodeStatus},
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
        ],
    ),
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

/// What `status()` reports, set once the server is up (or failed to start).
static STATE: std::sync::Mutex<Option<Value>> = std::sync::Mutex::new(None);
static STOP: std::sync::OnceLock<tokio_util::sync::CancellationToken> = std::sync::OnceLock::new();

fn start(data_dir: PathBuf) -> Result<Value, String> {
    let config = ModuleConfig::load_or_create(&data_dir)?;
    let token = Token::load_or_create(&data_dir).map_err(|e| e.to_string())?;
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
    };
    let events = Arc::new(EventStore::default());
    let node: Arc<dyn Node> = Arc::new(InProcessNode);
    let server = LogosServer::new(node.clone(), events.clone(), files, settings);
    let app = router(move || Ok(server.clone()), &token);

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
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(stop.cancelled_owned())
                .await;
        });
    });
    rx.recv().map_err(|e| e.to_string())??;
    // Only once serving: a module that failed to start keeps no subscriptions.
    for (module, _) in WATCHED {
        node.watch(module, events.clone());
    }
    Ok(json!({
        "url": format!("http://127.0.0.1:{port}/mcp"),
        "token_file": data_dir.join(logos_mcp::module::TOKEN_FILE),
        "config_file": data_dir.join(logos_mcp::module::CONFIG_FILE),
    }))
}

#[derive(Default)]
struct ModuleImpl;

impl LogosMcpModule for ModuleImpl {
    fn on_context_ready(&self, ctx: &RustModuleContext) {
        let state = match start(PathBuf::from(&ctx.instance_persistence_path)) {
            Ok(v) => v,
            Err(e) => json!({ "error": e }),
        };
        if let Ok(mut s) = STATE.lock() {
            *s = Some(state);
        }
    }

    fn about_to_unload(&self) -> logos_rust_sdk::Shutdown {
        if let Some(stop) = STOP.get() {
            stop.cancel();
        }
        logos_rust_sdk::Shutdown::Synchronous
    }

    fn status(&self) -> String {
        STATE
            .lock()
            .ok()
            .and_then(|s| s.clone())
            .unwrap_or_else(|| json!({ "state": "starting" }))
            .to_string()
    }
}

#[no_mangle]
pub extern "Rust" fn logos_module_install() {
    install::<ModuleImpl>();
}
