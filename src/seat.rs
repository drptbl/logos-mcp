//! The agent's own seat in Muster: a headless logosctl session with its own
//! identity, a member of the room like any person. In Basecamp the module sets
//! it up and runs it, since Basecamp's one Muster is the user's.

use std::{path::PathBuf, sync::Arc, time::Duration};

use serde_json::{Value, json};

use crate::{
    logosctl::{Logosctl, ProcessRunner, RawOutput, Runner},
    node::Node,
    room::{member_key, muster, text},
};

/// Muster's own package catalogue. It follows Muster's main branch and its
/// packages are unsigned, so what it serves is checked against the pin below.
pub const MUSTER_CATALOG: &str =
    "https://raw.githubusercontent.com/corpetty/muster/main/catalog/logos-repo.json";

/// The Muster build the seat runs: this version, and only with this root
/// hash, which logosctl checks against the package's files when it installs
/// it. A newer Muster needs a logos-mcp release that pins it.
pub const MUSTER_VERSION: &str = "0.3.5";
pub const MUSTER_ROOT: &str = "ed0aaa46399fd9e5edae20336c8a01d938e1cfae1b8ef5dcf66ab55420194917";
/// What that build's manifest names, which the root hash doesn't cover: the
/// modules it needs, sorted, and the file it loads, without its extension.
pub const MUSTER_DEPENDENCIES: [&str; 5] = [
    "delivery_module",
    "eth_rpc_module",
    "keystore_module",
    "lez_core",
    "tx_sender_module",
];
const MUSTER_PLUGIN: &str = "muster_module_plugin";

/// Which Muster the seat holds.
enum Installed {
    Pinned,
    Other,
    /// It couldn't be read: nothing is known, so nothing is removed.
    Unknown(String),
}

/// The logosctl binary: on `PATH`, then `~/.local/logosctl/bin/logosctl`.
pub fn find_logosctl() -> Option<PathBuf> {
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

/// `mutex`, even if a panic poisoned it: its state is always whole.
fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// How long installing Muster may take: it downloads seven packages.
const INSTALL_TIMEOUT: Duration = Duration::from_secs(300);

/// Where setting the seat up stands.
#[derive(Default)]
enum Setup {
    #[default]
    Idle,
    Running,
    Failed(String),
}

/// The one setup under way. It records how setup ended when dropped: if
/// dropped before it finishes, as when its runtime shuts down, the seat is
/// free to be set up again.
struct SetupClaim {
    seat: &'static Seat,
    result: Option<Result<(), String>>,
}

impl SetupClaim {
    fn take(seat: &'static Seat) -> Result<Self, String> {
        let mut setup = seat.setup();
        if matches!(*setup, Setup::Running) {
            return Err("the agent seat is already being set up".into());
        }
        *setup = Setup::Running;
        Ok(Self { seat, result: None })
    }

    fn finish(mut self, result: Result<(), String>) {
        self.result = Some(result);
    }
}

impl Drop for SetupClaim {
    fn drop(&mut self) {
        let ended = match self.result.take() {
            Some(Ok(())) => Setup::Idle,
            Some(Err(e)) => Setup::Failed(e),
            None => Setup::Failed("setting the seat up stopped before it finished".into()),
        };
        *self.seat.setup() = ended;
    }
}

/// A logosctl session holding the agent's Muster.
pub struct Seat {
    runner: Arc<dyn Runner>,
    ctl: Logosctl,
    /// For installing, which takes far longer than any other call.
    slow: Logosctl,
    setup: std::sync::Mutex<Setup>,
    /// The setup under way, for `stop` to end first.
    setup_task: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Seat {
    pub fn new(runner: Arc<dyn Runner>) -> Self {
        Self {
            ctl: Logosctl::new(runner.clone()),
            slow: Logosctl::new(runner.clone()),
            runner,
            setup: Default::default(),
            setup_task: Default::default(),
        }
    }

    /// The session in `dir`, driven by the logosctl binary `bin`, each call
    /// within `timeout`, but installing within five minutes.
    pub fn in_dir(bin: PathBuf, dir: PathBuf, timeout: Duration) -> Self {
        let runner = |timeout| {
            Arc::new(ProcessRunner {
                bin: bin.clone(),
                config_dir: Some(dir.clone()),
                timeout,
            })
        };
        let ctl = runner(timeout);
        Self {
            ctl: Logosctl::new(ctl.clone()),
            slow: Logosctl::new(runner(INSTALL_TIMEOUT)),
            runner: ctl,
            setup: Default::default(),
            setup_task: Default::default(),
        }
    }

    /// Where the seat's Muster calls go.
    pub fn node(&self) -> Arc<dyn Node> {
        Arc::new(self.ctl.clone())
    }

    async fn running(&self) -> bool {
        self.ctl
            .command(&["status"])
            .await
            .is_ok_and(|s| s.pointer("/daemon/pid").is_some())
    }

    /// Start the session's daemon if it isn't running.
    async fn ensure_daemon(&self) -> Result<(), String> {
        if self.running().await {
            return Ok(());
        }
        let RawOutput { code, stderr, .. } = self
            .runner
            .run(vec!["-D".into(), "-d".into()])
            .await
            .map_err(|e| format!("could not start the agent seat: {e}"))?;
        if code != 0 {
            return Err(format!("could not start the agent seat: {stderr}"));
        }
        Ok(())
    }

    /// Load Muster with only what it needs, and listen for room invites. The
    /// caller has checked it is the pinned build.
    async fn load_muster(&self) -> Result<(), String> {
        self.ctl
            .command(&["module", "load", "muster_module", "--no-optional"])
            .await
            .map_err(|e| format!("could not load Muster in the agent seat: {e}"))?;
        muster(self.ctl_node(), "coordinate_start_inbox", &[]).await?;
        Ok(())
    }

    /// Start a seat that is set up: its daemon, Muster and its inbox.
    /// Only the pinned Muster build is loaded, even in a seat set up before
    /// it was pinned.
    pub async fn start(&self) -> Result<(), String> {
        self.ensure_daemon().await?;
        if let Err(e) = self.check_installed().await {
            // A Muster already loaded in a running daemon would stay callable.
            if let Err(stop) = self.ctl.command(&["daemon", "stop"]).await {
                tracing::warn!("could not stop the agent seat: {stop}");
            }
            return Err(e);
        }
        self.load_muster().await
    }

    /// Start setting the seat up on `runtime`, and return: installing
    /// outlasts what a caller may wait. Then `remember` records the seat as
    /// set up, and its error counts as setup's. One setup at a time: `status`
    /// says while it runs, and why it failed if it did.
    pub fn set_up_in_background(
        &'static self,
        runtime: &tokio::runtime::Handle,
        remember: impl FnOnce() -> Result<(), String> + Send + 'static,
    ) -> Result<(), String> {
        let claim = SetupClaim::take(self)?;
        let task = runtime.spawn(async move {
            let result = match self.prepare_session().await {
                Ok(()) => remember(),
                Err(e) => Err(e),
            };
            claim.finish(result);
        });
        *lock(&self.setup_task) = Some(task);
        Ok(())
    }

    fn setup(&self) -> std::sync::MutexGuard<'_, Setup> {
        lock(&self.setup)
    }

    /// Which Muster the session holds: the pinned build has its version, its
    /// root hash, which logosctl checks against the files, and the manifest
    /// the root hash doesn't cover, naming its dependencies and its plugin.
    async fn installed(&self) -> Installed {
        let shown = match self
            .ctl
            .command(&["package", "show", "muster_module"])
            .await
        {
            Ok(shown) => shown,
            Err(e) => return Installed::Unknown(e.to_string()),
        };
        let mut deps: Vec<&str> = shown["dependencies"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        deps.sort_unstable();
        let plugin = std::path::Path::new(text(&shown, "mainFilePath"))
            .file_stem()
            .and_then(|s| s.to_str());
        let pinned = text(&shown, "version") == MUSTER_VERSION
            && shown.pointer("/hashes/root").and_then(Value::as_str) == Some(MUSTER_ROOT)
            && deps == MUSTER_DEPENDENCIES
            && plugin == Some(MUSTER_PLUGIN);
        if pinned {
            Installed::Pinned
        } else {
            Installed::Other
        }
    }

    /// Whether the Muster installed in the session is the pinned build.
    async fn check_installed(&self) -> Result<(), String> {
        match self.installed().await {
            Installed::Pinned => Ok(()),
            Installed::Other => Err(Self::not_pinned()),
            Installed::Unknown(e) => Err(format!("could not check the agent seat's Muster: {e}")),
        }
    }

    fn not_pinned() -> String {
        format!(
            "the agent seat holds a Muster build other than the pinned {MUSTER_VERSION}, so it isn't loaded: turn room approval off and on to install the pinned one, which drops the room settings, so pick the room again after"
        )
    }

    /// Refuse an install plan, or what an install did, unless it brings the
    /// pinned Muster, and every other package from a named catalogue other
    /// than Muster's unsigned one. Returns the packages that broke the rule.
    fn check_plan(plan: &Value) -> Result<(), (String, Vec<String>)> {
        let Some(changes) = plan["changes"].as_array() else {
            return Err((
                format!("logosctl's install plan can't be read: {plan}"),
                vec![],
            ));
        };
        let mut wrong = Vec::new();
        let mut why = None;
        for change in changes {
            let name = text(change, "name");
            let refused = if name == "muster_module" {
                (text(change, "toVersion") != MUSTER_VERSION
                    || text(change, "rootHash") != MUSTER_ROOT)
                    .then(|| {
                        format!(
                            "Muster's catalogue offers a build other than the pinned {MUSTER_VERSION}"
                        )
                    })
            } else {
                match text(change, "repository") {
                    "" => Some(format!("{name} comes from no named catalogue")),
                    MUSTER_CATALOG => Some(format!(
                        "Muster's catalogue would supply {name}, which only the signed Logos catalogue may"
                    )),
                    _ => None,
                }
            };
            if let Some(refused) = refused {
                why.get_or_insert(refused);
                wrong.push(name.to_string());
            }
        }
        match why {
            Some(why) => Err((why, wrong)),
            None => Ok(()),
        }
    }

    /// Remove `packages` from the session, best effort.
    async fn remove(&self, packages: &[String]) {
        for package in packages {
            if let Err(e) = self
                .ctl
                .command(&["package", "remove", package, "-y"])
                .await
            {
                tracing::warn!("could not remove {package} from the agent seat: {e}");
            }
        }
    }

    /// Make the session: add Muster's catalogue, install the pinned Muster
    /// without its optional extras, and start it.
    async fn prepare_session(&self) -> Result<(), String> {
        self.ensure_daemon().await?;
        let catalogs = self
            .ctl
            .command(&["catalog", "ls"])
            .await
            .map_err(|e| e.to_string())?;
        let has_muster = catalogs
            .as_array()
            .is_some_and(|c| c.iter().any(|c| text(c, "name") == "muster"));
        if !has_muster {
            self.ctl
                .command(&["catalog", "add", MUSTER_CATALOG])
                .await
                .map_err(|e| format!("could not add Muster's catalogue: {e}"))?;
        }
        let install = [
            "package",
            "install",
            "muster_module",
            "--version",
            MUSTER_VERSION,
            "--no-optional",
            "-y",
        ];
        // What it would install, checked before anything is.
        let plan = self
            .slow
            .command(&[&install[..], &["--dry-run"]].concat())
            .await
            .map_err(|e| format!("could not install Muster: {e}"))?;
        Self::check_plan(&plan).map_err(|(why, _)| format!("{why}: nothing was installed"))?;
        let done = self
            .slow
            .command(&install)
            .await
            .map_err(|e| format!("could not install Muster: {e}"))?;
        // The catalogue may have changed since the plan: what landed counts.
        if let Err((why, mut wrong)) = Self::check_plan(&done) {
            wrong.retain(|p| p != "muster_module");
            wrong.push("muster_module".into());
            self.remove(&wrong).await;
            return Err(format!("{why}: it was removed and nothing was loaded"));
        }
        match self.installed().await {
            Installed::Pinned => self.load_muster().await,
            Installed::Other => {
                self.remove(&["muster_module".into()]).await;
                Err(Self::not_pinned())
            }
            // Maybe fine: left installed, and checked again before it loads.
            Installed::Unknown(e) => Err(format!("could not check the agent seat's Muster: {e}")),
        }
    }

    /// Stop the session's daemon, ending any setup under way first so it
    /// can't start the daemon again. Room invites wait in the store meanwhile.
    pub async fn stop(&self) {
        let setup = lock(&self.setup_task).take();
        if let Some(setup) = setup {
            setup.abort();
            let _ = setup.await;
        }
        if let Err(e) = self.ctl.command(&["daemon", "stop"]).await {
            tracing::warn!("could not stop the agent seat: {e}");
        }
    }

    fn ctl_node(&self) -> &dyn Node {
        &self.ctl
    }

    /// Whether the seat runs, its key and the id people invite it by, the
    /// rooms it is in and the invites waiting for it, and whether it is being
    /// set up or why that failed.
    pub async fn status(&self) -> Value {
        // Read first: a setup that ends meanwhile then shows as still running
        // for one more look, never as not set up.
        let setup = self.setup_progress();
        let mut status = self.session_status().await;
        if let Some((key, value)) = setup {
            status[key] = value;
        }
        status
    }

    /// The status of a seat room approval doesn't use: only how its setup
    /// goes. It asks nothing of logosctl or Muster.
    pub fn setup_status(&self) -> Value {
        let mut status = json!({ "running": false });
        if let Some((key, value)) = self.setup_progress() {
            status[key] = value;
        }
        status
    }

    fn setup_progress(&self) -> Option<(&'static str, Value)> {
        match &*self.setup() {
            Setup::Idle => None,
            Setup::Running => Some(("setting_up", json!(true))),
            Setup::Failed(e) => Some(("setup_error", json!(e))),
        }
    }

    async fn session_status(&self) -> Value {
        if !self.running().await {
            return json!({ "running": false });
        }
        let node = self.ctl_node();
        let identity = match muster(node, "identity", &[]).await {
            Ok(identity) => identity,
            Err(e) => return json!({ "running": true, "error": e }),
        };
        let (ed, x) = (text(&identity, "ed25519"), text(&identity, "x25519"));
        let rooms: Vec<Value> = muster(node, "coordinate_conversations", &[])
            .await
            .ok()
            .and_then(|r| r.as_array().cloned())
            .unwrap_or_default()
            .iter()
            .map(|r| json!({ "topic": r["topic"], "title": r["title"], "active": r["active"] }))
            .collect();
        let invites: Vec<Value> = muster(node, "coordinate_invites", &[])
            .await
            .ok()
            .and_then(|r| r.as_array().cloned())
            .unwrap_or_default()
            .iter()
            .map(|i| {
                json!({
                    "topic": i["topic"],
                    "from": member_key(text(i, "from")).map(|k| format!("ed:{k}")),
                    "from_alias": i["fromAlias"],
                    "note": i["note"],
                })
            })
            .collect();
        json!({
            "running": true,
            "key": format!("ed:{ed}"),
            "invite_id": format!("{ed}{x}"),
            "rooms": rooms,
            "invites": invites,
        })
    }
}

#[cfg(test)]
mod tests;
