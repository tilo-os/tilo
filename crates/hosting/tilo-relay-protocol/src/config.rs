//! On-disk config schema for `tilo-relay`.
//!
//! The daemon reads a TOML file — path is passed via `--config` and
//! defaults to `/etc/tilo/relay.toml`. The file contains
//! filesystem paths + a listen address + policy + a list of
//! workspace bindings. Any secrets stay on disk under those paths
//! and are never inlined here.
//!
//! Example:
//!
//! ```toml
//! # /etc/tilo/relay.toml
//!
//! # Where the user root public key + this device's keypair + cert live.
//! # Matches `tilo_user_identity::IdentityPaths::default_system()`.
//! identity_root = "/var/lib/tilo"
//!
//! # Where relay state (workspace pins, pending deltas) lives.
//! state_root = "/var/lib/tilo-relay"
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
    /// [`tilo_user_identity::IdentityPaths::under`].
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

    /// The filesystem home-peer bundle. When present, its workspace tickets
    /// are imported as durable full-workspace relay bindings at startup.
    #[serde(default = "default_home_peer_bundle")]
    pub home_peer_bundle: PathBuf,

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
    /// `tilo-relay-admin bind-workspace`; hand-editable.
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

/// Local recovery contract for an admitted checkpoint publisher. This is
/// intentionally absent from persisted configuration and wire protocols.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CheckpointPrefixRecovery {
    /// Generic SDK/browser/relay publishers have no automatic resend promise.
    #[default]
    Disabled,
    /// The native session owner republishes its complete durable baseline on
    /// every new Ready session after an uncertain send.
    NativeOwnerDurableBaseline,
}

/// A single workspace this relay is bound to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceBinding {
    /// Workspace id (matches
    /// [`tilo_user_identity::WorkspaceId`]).
    #[serde(with = "workspace_id_serde")]
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
    /// Signed filesystem-authority users permitted to use this relay room.
    /// Home-peer import maintains this from the bundle; it is intentionally
    /// room-scoped rather than a relay-wide collaborator union.
    #[serde(default)]
    pub admitted_users: Vec<String>,
    /// Local shell governor pin supplied by the native owner. Neither workspace
    /// membership nor a same-user relay identity implies this authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell_authority: Option<crate::retention::RetentionAuthority>,
    /// Programmatic host capability. Persisted/admin configuration cannot opt
    /// into checkpoint-prefix recovery semantics.
    #[serde(skip)]
    pub checkpoint_prefix_recovery: CheckpointPrefixRecovery,
}

mod workspace_id_serde {
    use serde::{Deserialize, Deserializer, Serializer};

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Encoded {
        Decimal(String),
        Legacy(u64),
    }

    #[allow(
        clippy::trivially_copy_pass_by_ref,
        reason = "serde's with-module serialization callback requires a reference to the field"
    )]
    pub fn serialize<S>(id: &u64, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&id.to_string())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<u64, D::Error>
    where
        D: Deserializer<'de>,
    {
        match Encoded::deserialize(deserializer)? {
            Encoded::Decimal(value) => value.parse().map_err(serde::de::Error::custom),
            Encoded::Legacy(value) => Ok(value),
        }
    }
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
    // CRDT persistence is the only replication plane present in tilo-relay.
    // FullWorkspace needs the separate filesystem/blob host, so an operator
    // must opt into it only after deploying that service.
    ReplicationIntent::WorkspaceOnly
}

fn default_identity_root() -> PathBuf {
    PathBuf::from("/var/lib/tilo")
}

fn default_state_root() -> PathBuf {
    PathBuf::from("/var/lib/tilo-relay")
}

fn default_full_replica_status() -> PathBuf {
    PathBuf::from("/var/lib/tilo/full-replica/status.json")
}

fn default_home_peer_bundle() -> PathBuf {
    PathBuf::from("/etc/tilo/home-peer.toml")
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
            home_peer_bundle: default_home_peer_bundle(),
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
            admitted_users: Vec::new(),
            shell_authority: None,
            checkpoint_prefix_recovery: CheckpointPrefixRecovery::Disabled,
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

    /// The two paths every hand-written config starts with.
    const PRELUDE: &str =
        "identity_root = \"/var/lib/tilo\"\nstate_root = \"/var/lib/tilo-relay\"\n";

    #[test]
    fn relay_config_defaults_and_toml_decoding() {
        // Defaults are usable as-is.
        let cfg = RelayConfig::default();
        assert_eq!(cfg.identity_root, PathBuf::from("/var/lib/tilo"));
        assert_eq!(cfg.state_root, PathBuf::from("/var/lib/tilo-relay"));
        assert_eq!(cfg.iroh_listen.port(), 0);
        assert!(cfg.workspaces.is_empty());
        assert_eq!(cfg.device_role, DeviceRole::AlwaysOnServer);
        assert!(cfg.services.workspace_replication);
        assert!(cfg.services.inbound_relay);
        assert!(!cfg.services.full_content_replication);
        assert!(cfg.services.remote_control_relay);
        assert!(!cfg.services.app_serving);
        assert!(!cfg.services.compute);

        // A minimal file names only the two roots.
        let cfg: RelayConfig = toml::from_str(PRELUDE).unwrap();
        assert_eq!(cfg.workspaces.len(), 0);

        // Peer relays parse, with priority defaulting to 0.
        let cfg: RelayConfig = toml::from_str(&format!(
            "{PRELUDE}\n[[peer_relays]]\niroh_node_id = \"aa\"\nlabel = \"west\"\npriority = 100\n\n\
             [[peer_relays]]\niroh_node_id = \"bb\"\nlabel = \"east\"\n"
        ))
        .unwrap();
        assert_eq!(cfg.peer_relays.len(), 2);
        assert_eq!(cfg.peer_relays[0].label, "west");
        assert_eq!(cfg.peer_relays[0].priority, 100);
        assert_eq!(cfg.peer_relays[1].priority, 0);

        // An unknown field is rejected rather than ignored.
        assert!(toml::from_str::<RelayConfig>(&format!("{PRELUDE}surprise = 3\n")).is_err());

        // Workspace ids above TOML's integer range round-trip as strings, and
        // legacy integer ids still parse.
        let high = (i64::MAX as u64) + 42;
        let binding = WorkspaceBinding {
            id: high,
            pin: true,
            replication: ReplicationIntent::FullWorkspace,
            admitted_users: Vec::new(),
            shell_authority: None,
            checkpoint_prefix_recovery: CheckpointPrefixRecovery::Disabled,
        };
        let text = toml::to_string(&binding).unwrap();
        assert!(text.contains(&format!("id = \"{high}\"")));
        assert_eq!(toml::from_str::<WorkspaceBinding>(&text).unwrap().id, high);
        let legacy: WorkspaceBinding = toml::from_str("id = 42").unwrap();
        assert_eq!(legacy.id, 42);
    }

    #[test]
    fn shell_authority_config_accepts_exact_local_binding() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../tilo-relay/tests/fixtures/shell-proof/failure.json"
        ))
        .unwrap();
        let binding = serde_json::json!({"id": "42", "shell_authority": {"room": fixture["room"], "governor": fixture["authority"]}});
        let binding: WorkspaceBinding = serde_json::from_value(binding).unwrap();
        assert_eq!(
            toml::from_str::<WorkspaceBinding>(&toml::to_string(&binding).unwrap()).unwrap(),
            binding
        );
        assert_eq!(
            binding.checkpoint_prefix_recovery,
            CheckpointPrefixRecovery::Disabled
        );
        let encoded = toml::to_string(&binding).unwrap();
        assert!(!encoded.contains("checkpoint_prefix_recovery"));
        assert!(
            toml::from_str::<WorkspaceBinding>(
                "id = 42\ncheckpoint_prefix_recovery = \"NativeOwnerDurableBaseline\""
            )
            .is_err(),
            "persisted configuration cannot opt into a process recovery contract"
        );
        assert!(
            serde_json::from_value::<WorkspaceBinding>(serde_json::json!({"id": "42"}))
                .unwrap()
                .shell_authority
                .is_none()
        );
    }

    #[test]
    fn checkpoint_prefix_recovery_is_runtime_only_when_serializing_toml_and_json() {
        let mut binding: WorkspaceBinding =
            serde_json::from_value(serde_json::json!({"id": "42"})).unwrap();
        binding.checkpoint_prefix_recovery = CheckpointPrefixRecovery::NativeOwnerDurableBaseline;

        let toml_text = toml::to_string(&binding).unwrap();
        assert!(!toml_text.contains("checkpoint_prefix_recovery"));
        assert_eq!(
            toml::from_str::<WorkspaceBinding>(&toml_text)
                .unwrap()
                .checkpoint_prefix_recovery,
            CheckpointPrefixRecovery::Disabled
        );

        let json_value = serde_json::to_value(&binding).unwrap();
        assert!(json_value.get("checkpoint_prefix_recovery").is_none());
        assert_eq!(
            serde_json::from_value::<WorkspaceBinding>(json_value)
                .unwrap()
                .checkpoint_prefix_recovery,
            CheckpointPrefixRecovery::Disabled
        );
    }

    #[test]
    fn relay_config_edits_are_idempotent_and_persist() {
        let mut cfg = RelayConfig::default();
        assert!(cfg.add_workspace(1, true));
        assert!(!cfg.add_workspace(1, true), "adding twice is a no-op");
        assert_eq!(cfg.workspaces.len(), 1);
        assert_eq!(
            cfg.workspaces[0].replication,
            ReplicationIntent::WorkspaceOnly
        );
        assert!(cfg.remove_workspace(1));
        assert!(!cfg.remove_workspace(1), "removing twice is a no-op");

        // Peer relays dedup by node id, case-insensitively.
        assert!(cfg.add_peer_relay("ABCD", "a", 1));
        assert!(!cfg.add_peer_relay("abcd", "again", 2));
        assert_eq!(cfg.peer_relays.len(), 1);

        // Save + load is lossless.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("relay.toml");
        let mut cfg = RelayConfig::default();
        cfg.add_workspace(7, true);
        cfg.add_workspace(9, false);
        cfg.save(&path).unwrap();
        assert_eq!(cfg, RelayConfig::load(&path).unwrap());
    }
}
