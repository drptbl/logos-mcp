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

/// Muster's own package catalogue.
pub const MUSTER_CATALOG: &str =
    "https://raw.githubusercontent.com/corpetty/muster/main/catalog/logos-repo.json";

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
        let mut setup = seat.setup.lock().unwrap();
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
        *self.seat.setup.lock().unwrap_or_else(|e| e.into_inner()) = ended;
    }
}

/// A logosctl session holding the agent's Muster.
pub struct Seat {
    runner: Arc<dyn Runner>,
    ctl: Logosctl,
    /// For installing, which takes far longer than any other call.
    slow: Logosctl,
    setup: std::sync::Mutex<Setup>,
}

impl Seat {
    pub fn new(runner: Arc<dyn Runner>) -> Self {
        Self {
            ctl: Logosctl::new(runner.clone()),
            slow: Logosctl::new(runner.clone()),
            runner,
            setup: Default::default(),
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

    /// Load Muster with only what it needs, and listen for room invites.
    async fn load_muster(&self) -> Result<(), String> {
        self.ctl
            .command(&["module", "load", "muster_module", "--no-optional"])
            .await
            .map_err(|e| format!("could not load Muster in the agent seat: {e}"))?;
        muster(self.ctl_node(), "coordinate_start_inbox", &[]).await?;
        Ok(())
    }

    /// Start a seat that is set up: its daemon, Muster and its inbox.
    pub async fn start(&self) -> Result<(), String> {
        self.ensure_daemon().await?;
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
        runtime.spawn(async move {
            let result = match self.prepare_session().await {
                Ok(()) => remember(),
                Err(e) => Err(e),
            };
            claim.finish(result);
        });
        Ok(())
    }

    /// Make the session: add Muster's catalogue, install Muster without its
    /// optional extras, and start it.
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
        self.slow
            .command(&["package", "install", "muster_module", "--no-optional", "-y"])
            .await
            .map_err(|e| format!("could not install Muster: {e}"))?;
        self.load_muster().await
    }

    /// Stop the session's daemon. Room invites wait in the store meanwhile.
    pub async fn stop(&self) {
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
        let mut status = self.session_status().await;
        match &*self.setup.lock().unwrap() {
            Setup::Idle => {}
            Setup::Running => status["setting_up"] = json!(true),
            Setup::Failed(e) => status["setup_error"] = json!(e),
        }
        status
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
