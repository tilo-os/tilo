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

use crate::mesh::{DeviceRole, ReplicationIntent, ServiceCapabilities};
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

    /// Atomic readiness proof produced by `fs replica`.  Content-replication
    /// capacity is advertised only while this says the complete current
    /// namespace (manifests, chunks, and thumbnails) is resident.
    #[serde(default = "default_full_replica_status")]
    pub full_replica_status: PathBuf,

    /// Iroh endpoint bind hint. `0.0.0.0:0` = OS-chosen port.
    #[serde(default = "default_iroh_listen")]
    pub iroh_listen: SocketAddr,

    /// Which peers this relay accepts. Defaults to the matching-
    /// user policy — only peers whose cert chains to the same user
    /// identity as this relay's cert.
    #[serde(default)]
    pub trust_policy: TrustPolicy,

    /// How this certified device participates in the personal mesh. A relay
    /// daemon defaults to an always-on server; its service claims are still
    /// intersected with the signed device certificate at runtime.
    #[serde(default = "default_device_role")]
    pub device_role: DeviceRole,

    /// Services requested by the operator. Runtime advertisement is clamped
    /// to the planes started by the composite home peer and its live residency
    /// proof.
    #[serde(default = "default_services")]
    pub services: ServiceCapabilities,

    /// Workspaces the relay pins and forwards for. Populated by
    /// `qos-relay-admin bind-workspace`; hand-editable.
    #[serde(default, rename = "workspace")]
    pub workspaces: Vec<WorkspaceBinding>,

    /// Other relays this one federates with. Each entry becomes an
    /// outbound dial on startup; the pair mutually auths using the
    /// existing device-cert challenge/response, and thereafter looks
    /// exactly like any other peer session — same fan-out, same
    /// store-and-forward, one filter line for echo suppression. An
    /// empty list keeps the relay standalone (current behaviour).
    #[serde(default, rename = "peer_relays")]
    pub peer_relays: Vec<PeerRelayConfig>,
}

/// A configured federated peer relay. Two relays that list each
/// other here converge on their shared workspaces via the same CRDT
/// merge that any two devices already do — no new transport, no new
/// fan-out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerRelayConfig {
    /// Iroh endpoint id (Ed25519 public key) of the peer relay,
    /// hex-encoded (64 chars).
    pub iroh_node_id: String,
    /// Human-readable label — appears in status output / logs.
    #[serde(default)]
    pub label: String,
    /// Preference weight for peer-selection strategies; larger =
    /// preferred. Currently only surfaced in status; a client will
    /// use it once the wheel-disc chooses a nearest host.
    #[serde(default = "default_priority")]
    pub priority: i32,
}

fn default_priority() -> i32 {
    0
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

    /// Which existing collaboration/blob streams this device retains. The
    /// standalone relay defaults to workspace records only because it does
    /// not run the filesystem/blob host plane.
    #[serde(default = "default_server_replication")]
    pub replication: ReplicationIntent,
}

fn true_default() -> bool {
    true
}

fn default_device_role() -> DeviceRole {
    DeviceRole::AlwaysOnServer
}

fn default_services() -> ServiceCapabilities {
    ServiceCapabilities::relay_only()
}

fn default_server_replication() -> ReplicationIntent {
    // CRDT persistence is the only replication plane present in qos-relay.
    // FullWorkspace needs the separate filesystem/blob host, so an operator
    // must opt into it only after deploying that service.
    ReplicationIntent::WorkspaceOnly
}

fn default_identity_root() -> PathBuf {
    PathBuf::from("/var/lib/qos")
}

fn default_state_root() -> PathBuf {
    PathBuf::from("/var/lib/qos-relay")
}

fn default_full_replica_status() -> PathBuf {
    PathBuf::from("/var/lib/qos/full-replica/status.json")
}

fn default_iroh_listen() -> SocketAddr {
    // Constructed directly rather than parsed — `0.0.0.0:0` is
    // infallible either way, but this has no panic path to document.
    SocketAddr::from(([0, 0, 0, 0], 0))
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            identity_root: default_identity_root(),
            state_root: default_state_root(),
            full_replica_status: default_full_replica_status(),
            iroh_listen: default_iroh_listen(),
            trust_policy: TrustPolicy::default(),
            device_role: default_device_role(),
            services: default_services(),
            workspaces: Vec::new(),
            peer_relays: Vec::new(),
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
    ///
    /// # Errors
    ///
    /// Returns an error if `path` can't be read, or its contents
    /// don't parse as valid TOML for this schema.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let bytes = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&bytes)?)
    }

    /// Serialise + write the config to disk. Preserves the exact
    /// key ordering the type produces.
    ///
    /// # Errors
    ///
    /// Returns an error if `path`'s parent directory can't be
    /// created, `self` fails to serialise to TOML (shouldn't happen
    /// for this shape), or the write fails.
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
        self.workspaces.push(WorkspaceBinding {
            id,
            pin,
            replication: default_server_replication(),
        });
        true
    }

    /// Remove a workspace binding; returns `true` if it was
    /// present.
    pub fn remove_workspace(&mut self, id: u64) -> bool {
        let before = self.workspaces.len();
        self.workspaces.retain(|w| w.id != id);
        before != self.workspaces.len()
    }

    /// Ensure a peer-relay entry exists; returns `true` if it was
    /// added. Uniqueness is by iroh node id (case-insensitive hex).
    pub fn add_peer_relay(&mut self, iroh_node_id: &str, label: &str, priority: i32) -> bool {
        let norm = iroh_node_id.to_ascii_lowercase();
        if self
            .peer_relays
            .iter()
            .any(|r| r.iroh_node_id.eq_ignore_ascii_case(&norm))
        {
            return false;
        }
        self.peer_relays.push(PeerRelayConfig {
            iroh_node_id: norm,
            label: label.to_string(),
            priority,
        });
        true
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
        assert_eq!(cfg.device_role, DeviceRole::AlwaysOnServer);
        assert!(cfg.services.workspace_replication);
        assert!(cfg.services.inbound_relay);
        assert!(!cfg.services.full_content_replication);
        assert!(!cfg.services.remote_control_relay);
        assert!(!cfg.services.app_serving);
        assert!(!cfg.services.compute);
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
        assert_eq!(
            cfg.workspaces[0].replication,
            ReplicationIntent::WorkspaceOnly
        );
    }

    #[test]
    fn remove_workspace() {
        let mut cfg = RelayConfig::default();
        cfg.add_workspace(1, true);
        assert!(cfg.remove_workspace(1));
        assert!(!cfg.remove_workspace(1));
    }

    #[test]
    fn parses_peer_relays_toml() {
        let text = r#"
            identity_root = "/var/lib/qos"
            state_root = "/var/lib/qos-relay"

            [[peer_relays]]
            iroh_node_id = "aa"
            label = "west"
            priority = 100

            [[peer_relays]]
            iroh_node_id = "bb"
            label = "east"
        "#;
        let cfg: RelayConfig = toml::from_str(text).unwrap();
        assert_eq!(cfg.peer_relays.len(), 2);
        assert_eq!(cfg.peer_relays[0].label, "west");
        assert_eq!(cfg.peer_relays[0].priority, 100);
        // Default priority for the second entry.
        assert_eq!(cfg.peer_relays[1].priority, 0);
    }

    #[test]
    fn add_peer_relay_dedups_by_node_id() {
        let mut cfg = RelayConfig::default();
        assert!(cfg.add_peer_relay("ABCD", "a", 1));
        assert!(!cfg.add_peer_relay("abcd", "again", 2));
        assert_eq!(cfg.peer_relays.len(), 1);
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
