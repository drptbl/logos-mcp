//! The standalone binary's startup, for logos-mcp and for binaries that build
//! on its server, such as logos-agents: the same options, wallet checks and
//! logosctl wiring, serving whatever handler wraps the server.

use std::{ffi::OsString, path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Context, bail};
use clap::{CommandFactory, FromArgMatches, Parser};
use rmcp::{ServerHandler, ServiceExt, transport::stdio};

use crate::{
    events::EventStore,
    guard::FilesRoot,
    logosctl::{Logosctl, ProcessRunner},
    node::Node,
    policy::{ApprovalPolicy, Approver, RoomSettings, Tiers},
    server::{DeliveryConfig, LogosServer, NodeSettings, WalletMode, WalletPaths},
};

/// What the binary calls itself.
pub struct App {
    pub name: &'static str,
    pub version: &'static str,
    pub about: &'static str,
    /// The default `RUST_LOG` filter, e.g. `logos_mcp=info`.
    pub log_filter: &'static str,
}

/// MCP server that lets AI agents use a running Logos node through logosctl.
#[derive(Parser, Debug)]
struct Cli {
    /// Path to the logosctl binary (default: logosctl on PATH, then ~/.local/logosctl/bin/logosctl).
    #[arg(long, env = "LOGOS_MCP_LOGOSCTL")]
    logosctl: Option<PathBuf>,

    /// logosctl session directory (default: logosctl's own default, ~/.logosctl).
    #[arg(long, env = "LOGOSCTL_CONFIG_DIR")]
    config_dir: Option<PathBuf>,

    /// Messaging network preset: logos.test (RLN on) or logos.dev (RLN off).
    #[arg(long, env = "LOGOS_MCP_PRESET", default_value = "logos.test")]
    preset: String,

    /// Messaging node mode: Edge (light node) or Core (relay node).
    #[arg(long, env = "LOGOS_MCP_MODE", default_value = "Edge", value_parser = ["Edge", "Core"])]
    mode: String,

    /// Directory storage tools may read from and write to. Storage file access is off without it.
    #[arg(long, env = "LOGOS_MCP_FILES_ROOT")]
    files_root: Option<PathBuf>,

    /// The agent wallet's lez_core config file. Wallet tools are off without it.
    #[arg(long, env = "LOGOS_MCP_WALLET_CONFIG", requires = "wallet_storage")]
    wallet_config: Option<PathBuf>,

    /// The agent wallet's key storage file, created with `lez_core create_new`.
    #[arg(long, env = "LOGOS_MCP_WALLET_STORAGE", requires = "wallet_config")]
    wallet_storage: Option<PathBuf>,

    /// Discovery peers as `/.../p2p/<peer id>` multiaddrs, comma separated. Discovery tools are off without them.
    #[arg(long, env = "LOGOS_MCP_DISCOVERY_BOOTSTRAP", value_delimiter = ',')]
    discovery_bootstrap: Vec<String>,

    /// delivery_module's full createNode config as JSON, in place of --mode and
    /// --preset: for a network no preset names, such as a local devnet hub.
    // It may carry a node key: --help must not print it.
    #[arg(long, env = "LOGOS_MCP_DELIVERY_CONFIG", hide_env_values = true)]
    delivery_config: Option<String>,

    /// The Muster room whose trusted humans approve transfers over --room-above-lepta,
    /// e.g. /muster/1/my-room/proto. The agent's own muster_module, in this logosctl
    /// session, must have joined it.
    #[arg(long, env = "LOGOS_MCP_ROOM_TOPIC", requires_all = ["room_approvers", "wallet_config"])]
    room_topic: Option<String>,

    /// Trusted humans whose approval counts in the room, as ed:<Ed25519 key hex>,
    /// comma separated. Never the agent's own key.
    #[arg(
        long,
        env = "LOGOS_MCP_ROOM_APPROVERS",
        value_delimiter = ',',
        requires = "room_topic"
    )]
    room_approvers: Vec<String>,

    /// How many distinct trusted humans must approve a transfer.
    #[arg(
        long,
        env = "LOGOS_MCP_ROOM_APPROVALS",
        default_value_t = 1,
        requires = "room_topic"
    )]
    room_approvals: usize,

    /// Transfers strictly above this many lepta need the room; 0 sends every one to it.
    #[arg(
        long,
        env = "LOGOS_MCP_ROOM_ABOVE_LEPTA",
        default_value_t = 0,
        requires = "room_topic"
    )]
    room_above_lepta: u128,

    /// Minutes a room proposal waits for approval before it expires unsent.
    #[arg(
        long,
        env = "LOGOS_MCP_ROOM_EXPIRES_MINS",
        default_value_t = 60,
        requires = "room_topic"
    )]
    room_expires_mins: u64,

    /// Timeout for each logosctl call, in seconds.
    #[arg(long, default_value_t = 60)]
    timeout_secs: u64,
}

/// The approval policy the room options set up, if any: the agent wallet
/// sends up to the limit and asks the room above it. Chain transfers, which
/// spend the node's own wallet, always go to the room: the CLI has no app to
/// ask the user in.
fn approval_policy(cli: &Cli) -> anyhow::Result<Option<ApprovalPolicy>> {
    let Some(topic) = &cli.room_topic else {
        return Ok(None);
    };
    let policy = ApprovalPolicy {
        agent: Tiers {
            limit_lepta: cli.room_above_lepta,
            up_to: Approver::Send,
            above: Approver::Room,
        },
        node: Tiers {
            limit_lepta: 0,
            up_to: Approver::Room,
            above: Approver::Room,
        },
        room: Some(RoomSettings {
            topic: topic.clone(),
            approvers: cli.room_approvers.clone(),
            approvals: cli.room_approvals,
            expires_mins: cli.room_expires_mins,
        }),
    };
    policy.check().map_err(anyhow::Error::msg)?;
    Ok(Some(policy))
}


/// Parse `args` (the program name first, as `std::env::args_os` gives them),
/// set up logging to stderr and connect to the node through logosctl: the
/// server `run` serves, for callers that drive it themselves, such as a
/// provider loop with no MCP client. Call it inside a Tokio runtime, since it
/// starts the node's event watchers. `--help` and bad options print and exit.
pub fn server<I, T>(app: &App, args: I) -> anyhow::Result<LogosServer>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    // stdout is the MCP channel: logs must go to stderr. A second call keeps
    // the logging the first set up.
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| app.log_filter.into()),
        )
        .with_ansi(false)
        .try_init();

    let matches = Cli::command()
        .name(app.name)
        .version(app.version)
        .about(app.about)
        .get_matches_from(args);
    let cli = Cli::from_arg_matches(&matches)?;
    let policy = approval_policy(&cli)?;
    let bin = match cli.logosctl.or_else(crate::seat::find_logosctl) {
        Some(b) if b.is_file() => b,
        Some(b) => bail!("logosctl not found at {}", b.display()),
        None => bail!("logosctl not found; pass --logosctl or set LOGOS_MCP_LOGOSCTL"),
    };
    let files = cli
        .files_root
        .as_deref()
        .map(FilesRoot::new)
        .transpose()
        .context("--files-root must be an existing directory")?;
    let wallet = match (cli.wallet_config, cli.wallet_storage) {
        (Some(config), Some(storage)) => {
            // The daemon opens these from its own working directory: pass absolute paths.
            let absolute = |p: &PathBuf| {
                let full = p
                    .canonicalize()
                    .ok()
                    .filter(|p| p.is_file())
                    .with_context(|| format!("wallet file not found: {}", p.display()))?;
                // Paths reach lez_core as text: refuse ones that wouldn't survive that.
                if full.to_str().is_none() {
                    bail!("wallet path is not valid UTF-8: {}", full.display());
                }
                anyhow::Ok(full)
            };
            Some(WalletMode::Dedicated(WalletPaths {
                config: absolute(&config)?,
                storage: absolute(&storage)?,
            }))
        }
        _ => None,
    };

    let delivery_config = cli
        .delivery_config
        .as_deref()
        .map(DeliveryConfig::parse)
        .transpose()
        .map_err(anyhow::Error::msg)?;

    for peer in &cli.discovery_bootstrap {
        crate::guard::bootstrap_node(peer).map_err(anyhow::Error::msg)?;
    }

    let events = Arc::new(EventStore::default());

    let runner = ProcessRunner {
        bin: bin.clone(),
        config_dir: cli.config_dir.clone(),
        timeout: Duration::from_secs(cli.timeout_secs),
    };
    let settings = NodeSettings {
        preset: cli.preset,
        mode: cli.mode,
        config_dir: cli.config_dir.as_ref().map(|d| d.display().to_string()),
        wallet,
        discovery_bootstrap: cli.discovery_bootstrap,
        host_owns_messaging: false,
        delivery_config,
    };
    tracing::info!(logosctl = %bin.display(), ?settings, "starting {}", app.name);

    let node: Arc<dyn Node> =
        Arc::new(Logosctl::new(Arc::new(runner)).with_watch(bin.clone(), cli.config_dir.clone()));
    node.watch("delivery_module", events.clone());
    node.watch("storage_module", events.clone());

    let server = LogosServer::new(node, events, files, settings);
    Ok(match policy {
        Some(policy) => {
            tracing::info!(?policy, "room approval on");
            server.with_approval_policy(tokio::sync::watch::channel(policy).1)
        }
        None => server,
    })
}

/// Parse the process's options, connect to the node through logosctl and serve
/// `wrap(server)` over stdio until the client leaves.
pub async fn run<H, F>(app: App, wrap: F) -> anyhow::Result<()>
where
    H: ServerHandler,
    F: FnOnce(LogosServer) -> H,
{
    let server = server(&app, std::env::args_os())?;
    let service = wrap(server)
        .serve(stdio())
        .await
        .context("MCP handshake failed")?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_never_prints_the_delivery_config() {
        let cli = Cli::command();
        let arg = cli
            .get_arguments()
            .find(|a| a.get_id() == "delivery_config")
            .unwrap();
        assert!(arg.is_hide_env_values_set());
    }

    const APP: App = App {
        name: "logos-mcp-test",
        version: "0",
        about: "test",
        log_filter: "off",
    };

    #[tokio::test]
    async fn the_server_is_built_from_the_given_arguments() {
        let err = server(
            &APP,
            ["logos-mcp-test", "--logosctl", "/nonexistent/logosctl"],
        )
        .err()
        .expect("a missing logosctl is refused");
        assert!(err.to_string().contains("/nonexistent/logosctl"), "{err}");
    }

    fn fake_logosctl(dir: &std::path::Path) -> OsString {
        let logosctl = dir.join("logosctl");
        std::fs::write(&logosctl, "").unwrap();
        logosctl.into_os_string()
    }

    use crate::policy::Wallet;

    #[tokio::test]
    async fn room_approval_needs_the_agent_wallet_and_a_sound_config() {
        let dir = tempfile::tempdir().unwrap();
        let logosctl = fake_logosctl(dir.path());
        let room = |extra: &[&str]| {
            let mut args: Vec<OsString> = vec![
                "logos-mcp-test".into(),
                "--logosctl".into(),
                logosctl.clone(),
            ];
            args.extend(extra.iter().map(OsString::from));
            Cli::try_parse_from(args)
        };
        let approver = "ed:0b00086682a0d169f90095b9d58e6c003ee591531ccf8b59c712c1cb70603269";
        // A room without the agent wallet, or approvers without a room, is refused.
        assert!(
            room(&[
                "--room-topic",
                "/muster/1/r/proto",
                "--room-approvers",
                approver
            ])
            .is_err()
        );
        assert!(room(&["--room-approvers", approver]).is_err());
        let wallet = dir.path().join("w.json");
        std::fs::write(&wallet, "{}").unwrap();
        let wallet = wallet.to_str().unwrap();
        let cli = room(&[
            "--wallet-config",
            wallet,
            "--wallet-storage",
            wallet,
            "--room-topic",
            "/muster/1/r/proto",
            "--room-approvers",
            approver,
            "--room-above-lepta",
            "3000000",
        ])
        .unwrap();
        // Exactly as 0.9.0 routed them.
        let policy = approval_policy(&cli).unwrap().unwrap();
        let settings = policy.room_approval().unwrap().unwrap();
        assert_eq!(settings.approvals, 1);
        assert_eq!(settings.expires, Duration::from_secs(3600));
        assert_eq!(policy.approver(Wallet::Agent, "3000000"), Approver::Send);
        assert_eq!(policy.approver(Wallet::Agent, "3000001"), Approver::Room);
        assert_eq!(policy.approver(Wallet::Node, "1"), Approver::Room);
        let too_many = room(&[
            "--wallet-config",
            wallet,
            "--wallet-storage",
            wallet,
            "--room-topic",
            "/muster/1/r/proto",
            "--room-approvers",
            approver,
            "--room-approvals",
            "2",
        ])
        .unwrap();
        assert!(approval_policy(&too_many).is_err());
        // A limit without a room would leave transfers ungated: refused.
        for flag in [
            ["--room-above-lepta", "5"],
            ["--room-approvals", "2"],
            ["--room-expires-mins", "5"],
        ] {
            assert!(
                room(&[
                    "--wallet-config",
                    wallet,
                    "--wallet-storage",
                    wallet,
                    flag[0],
                    flag[1],
                ])
                .is_err()
            );
        }
        let none = room(&[]).unwrap();
        assert!(approval_policy(&none).unwrap().is_none());
    }

    #[tokio::test]
    async fn a_server_comes_up_without_serving_anything() {
        let dir = tempfile::tempdir().unwrap();
        let logosctl = dir.path().join("logosctl");
        std::fs::write(&logosctl, "").unwrap();
        let built = server(
            &APP,
            [
                "logos-mcp-test".into(),
                "--logosctl".into(),
                logosctl.into_os_string(),
                "--preset".into(),
                "logos.dev".into(),
            ],
        );
        assert!(built.is_ok(), "{:?}", built.err());
    }
}
