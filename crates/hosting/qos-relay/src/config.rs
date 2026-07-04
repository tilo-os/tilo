//! On-disk config schema for `qos-relay`.
//!
//! The daemon reads a TOML file — path is passed via `--config` and
//! defaults to `/etc/qos/relay.toml`. The file is deliberately small:
//! filesystem paths + a listen address + policy + a list of
//! workspace bindings. Any secrets stay on disk under those paths
//! and are never inlined here.
//!
//! Example:
//!
//! ```toml
//! # /etc/qos/relay.toml
//!
//! # Where the user root public key + this device's keypair + cert live.
//! # Matches `qos_user_identity::IdentityPaths::default_system()`.
//! identity_root = "/var/lib/qos"
//!
//! # Where relay state (workspace pins, pending deltas) lives.
//! state_root = "/var/lib/qos-relay"
//!
//! # Iroh endpoint bind hint. `"0.0.0.0:0"` lets iroh pick a port
//! # (default) — a fixed port makes firewall rules easier.
//! iroh_listen = "0.0.0.0:0"
//!
//! # Trust policy. See `TrustPolicy` for values.
//! trust_policy = "allow_matching_user"
//!
//! # Which workspaces this relay pins. Every entry becomes a
//! # persistent host slot in the WorkspaceDirectory.
//! [[workspace]]
//! id = 1
//! pin = true
//!
//! [[workspace]]
//! id = 2
//! pin = false   # relay forwards but does not persist history
//! ```

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::policy::TrustPolicy;

/// Top-level relay configuration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelayConfig {
    /// Root under which the user + device identity files live.
    /// Passed straight into
    /// [`qos_user_identity::IdentityPaths::under`].
    #[serde(default = "default_identity_root")]
    pub identity_root: PathBuf,

    /// Root under which the daemon persists workspace state (pinned
    /// automerge documents, per-peer pending-delta queues, metrics
    /// snapshots).
    #[serde(default = "default_state_root")]
    pub state_root: PathBuf,

    /// Iroh endpoint bind hint. `0.0.0.0:0` = OS-chosen port.
    #[serde(default = "default_iroh_listen")]
    pub iroh_listen: SocketAddr,

    /// Which peers this relay accepts. Defaults to the matching-
    /// user policy — only peers whose cert chains to the same user
    /// identity as this relay's cert.
    #[serde(default)]
    pub trust_policy: TrustPolicy,

    /// Workspaces the relay pins and forwards for. Populated by
    /// `qos-relay-admin bind-workspace`; hand-editable.
    #[serde(default, rename = "workspace")]
    pub workspaces: Vec<WorkspaceBinding>,
}

/// A single workspace this relay is bound to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceBinding {
    /// Workspace id (matches
    /// [`qos_user_identity::WorkspaceId`]).
    pub id: u64,

    /// When true, the relay holds a full replica of the
    /// workspace's CRDT (a `WorkspaceDirectory` shard). When false,
    /// the relay only stores-and-forwards for offline peers but
    /// does not itself materialize history.
    #[serde(default = "true_default")]
    pub pin: bool,
}

fn true_default() -> bool {
    true
}

fn default_identity_root() -> PathBuf {
    PathBuf::from("/var/lib/qos")
}

fn default_state_root() -> PathBuf {
    PathBuf::from("/var/lib/qos-relay")
}

fn default_iroh_listen() -> SocketAddr {
    "0.0.0.0:0"
        .parse()
        .expect("valid default iroh listen socket")
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            identity_root: default_identity_root(),
            state_root: default_state_root(),
            iroh_listen: default_iroh_listen(),
            trust_policy: TrustPolicy::default(),
            workspaces: Vec::new(),
        }
    }
}

/// Failure modes for config load/store.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// The file could not be read.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// The file was read but TOML parsing failed.
    #[error("toml parse: {0}")]
    Parse(#[from] toml::de::Error),
    /// TOML serialisation failed (shouldn't happen for our own shape).
    #[error("toml encode: {0}")]
    Encode(#[from] toml::ser::Error),
}

impl RelayConfig {
    /// Read a config file from disk.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let bytes = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&bytes)?)
    }

    /// Serialise + write the config to disk. Preserves the exact
    /// key ordering the type produces.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)?;
        std::fs::write(path, text)?;
        Ok(())
    }

    /// Ensure a workspace binding exists; returns `true` if it was
    /// added.
    pub fn add_workspace(&mut self, id: u64, pin: bool) -> bool {
        if self.workspaces.iter().any(|w| w.id == id) {
            return false;
        }
        self.workspaces.push(WorkspaceBinding { id, pin });
        true
    }

    /// Remove a workspace binding; returns `true` if it was
    /// present.
    pub fn remove_workspace(&mut self, id: u64) -> bool {
        let before = self.workspaces.len();
        self.workspaces.retain(|w| w.id != id);
        before != self.workspaces.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_usable() {
        let cfg = RelayConfig::default();
        assert_eq!(cfg.identity_root, PathBuf::from("/var/lib/qos"));
        assert_eq!(cfg.state_root, PathBuf::from("/var/lib/qos-relay"));
        assert_eq!(cfg.iroh_listen.port(), 0);
        assert!(cfg.workspaces.is_empty());
    }

    #[test]
    fn round_trip_via_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("relay.toml");
        let mut cfg = RelayConfig::default();
        cfg.add_workspace(7, true);
        cfg.add_workspace(9, false);
        cfg.save(&path).unwrap();
        let back = RelayConfig::load(&path).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn parses_minimal_toml() {
        let text = r#"
            identity_root = "/var/lib/qos"
            state_root = "/var/lib/qos-relay"
        "#;
        let cfg: RelayConfig = toml::from_str(text).unwrap();
        assert_eq!(cfg.workspaces.len(), 0);
    }

    #[test]
    fn add_workspace_idempotent() {
        let mut cfg = RelayConfig::default();
        assert!(cfg.add_workspace(1, true));
        assert!(!cfg.add_workspace(1, true));
        assert_eq!(cfg.workspaces.len(), 1);
    }

    #[test]
    fn remove_workspace() {
        let mut cfg = RelayConfig::default();
        cfg.add_workspace(1, true);
        assert!(cfg.remove_workspace(1));
        assert!(!cfg.remove_workspace(1));
    }

    #[test]
    fn unknown_field_rejected() {
        let text = r#"
            identity_root = "/var/lib/qos"
            state_root = "/var/lib/qos-relay"
            surprise = 3
        "#;
        assert!(toml::from_str::<RelayConfig>(text).is_err());
    }
}
