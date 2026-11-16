//! The agent's own seat in Muster: a headless logosctl session with its own
//! identity, a member of the room like any person. In Basecamp the module sets
//! it up and runs it, since Basecamp's one Muster is the user's.

use std::{path::PathBuf, sync::Arc, time::Duration};

use serde_json::{Value, json};

use crate::{
    logosctl::{Logosctl, ProcessRunner, RawOutput, Runner},
    node::Node,
    room::{activate, member_key, muster, text},
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

/// A logosctl session holding the agent's Muster.
pub struct Seat {
    runner: Arc<dyn Runner>,
    ctl: Logosctl,
}

impl Seat {
    pub fn new(runner: Arc<dyn Runner>) -> Self {
        Self {
            ctl: Logosctl::new(runner.clone()),
            runner,
        }
    }

    /// The session in `dir`, driven by the logosctl binary `bin`.
    pub fn in_dir(bin: PathBuf, dir: PathBuf, timeout: Duration) -> Self {
        Self::new(Arc::new(ProcessRunner {
            bin,
            config_dir: Some(dir),
            timeout,
        }))
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

    /// Make the session: add Muster's catalogue, install Muster without its
    /// optional extras, and start it.
    pub async fn set_up(&self) -> Result<(), String> {
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
        self.ctl
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
    /// rooms it is in and the invites waiting for it.
    pub async fn status(&self) -> Value {
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

    /// Join `topic`: accept an invite, or make a joined room active.
    pub async fn join(&self, topic: &str) -> Result<(), String> {
        crate::guard::validate_topic(topic)?;
        muster(self.ctl_node(), "coordinate_join", &[topic])
            .await
            .map(|_| ())
    }

    /// The members of `topic`, a room the seat is in, as their `ed:` keys with
    /// any alias, and which of them is the agent.
    pub async fn members(&self, topic: &str) -> Result<Vec<Value>, String> {
        let node = self.ctl_node();
        let identity = muster(node, "identity", &[]).await?;
        let agent = text(&identity, "ed25519").to_ascii_lowercase();
        activate(node, topic).await?;
        let members = muster(node, "coordinate_members", &[]).await?;
        Ok(members
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter_map(|m| {
                let key = member_key(text(m, "identity"))?;
                Some(json!({
                    "key": format!("ed:{key}"),
                    "alias": m["alias"],
                    "agent": key == agent,
                }))
            })
            .collect())
    }
}

#[cfg(test)]
mod tests;
