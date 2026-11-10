//! The standalone binary's startup, for logos-mcp and for binaries that build
//! on its server, such as logos-agents: the same options, wallet checks and
//! logosctl wiring, serving whatever handler wraps the server.

use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Context, bail};
use clap::{CommandFactory, FromArgMatches, Parser};
use rmcp::{ServerHandler, ServiceExt, transport::stdio};

use crate::{
    events::EventStore,
    guard::FilesRoot,
    logosctl::{Logosctl, ProcessRunner},
    node::Node,
    server::{LogosServer, NodeSettings, WalletMode, WalletPaths},
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

    /// Timeout for each logosctl call, in seconds.
    #[arg(long, default_value_t = 60)]
    timeout_secs: u64,
}

fn find_logosctl() -> Option<PathBuf> {
    let on_path = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join("logosctl"))
            .find(|p| p.is_file())
    });
    on_path.or_else(|| {
        let home = std::env::var_os("HOME")?;
        let p = PathBuf::from(home).join(".local/logosctl/bin/logosctl");
        p.is_file().then_some(p)
    })
}

/// Parse the options, connect to the node through logosctl and serve
/// `wrap(server)` over stdio until the client leaves.
pub async fn run<H, F>(app: App, wrap: F) -> anyhow::Result<()>
where
    H: ServerHandler,
    F: FnOnce(LogosServer) -> H,
{
    // stdout is the MCP channel: logs must go to stderr.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| app.log_filter.into()),
        )
        .with_ansi(false)
        .init();

    let matches = Cli::command()
        .name(app.name)
        .version(app.version)
        .about(app.about)
        .get_matches();
    let cli = Cli::from_arg_matches(&matches)?;
    let bin = match cli.logosctl.or_else(find_logosctl) {
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
    };
    tracing::info!(logosctl = %bin.display(), ?settings, "starting {}", app.name);

    let node: Arc<dyn Node> =
        Arc::new(Logosctl::new(Arc::new(runner)).with_watch(bin.clone(), cli.config_dir.clone()));
    node.watch("delivery_module", events.clone());
    node.watch("storage_module", events.clone());

    let server = LogosServer::new(node, events, files, settings);
    let service = wrap(server)
        .serve(stdio())
        .await
        .context("MCP handshake failed")?;
    service.waiting().await?;
    Ok(())
}
