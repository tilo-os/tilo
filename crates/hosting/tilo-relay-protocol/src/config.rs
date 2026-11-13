//! On-disk config schema for `tilo-relay`.
//!
//! The daemon reads a TOML file — path is passed via `--config` and
//! defaults to `/etc/tilo/relay.toml`. The file contains
//! filesystem paths + a listen address + policy. Which workspaces a relay
//! holds is decided by the workspaces themselves (their governance), never by
//! this file. Any secrets stay on disk under those paths and are never
//! inlined here.
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
//! ```

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::mesh::ServiceCapabilities;
use crate::policy::TrustPolicy;
use crate::work::WorkSetting;

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

    /// The filesystem home-peer bundle. When present, this relay serves
    /// full-content replication and discovery for the workspaces it names.
    #[serde(default = "default_home_peer_bundle")]
    pub home_peer_bundle: PathBuf,

    /// Iroh endpoint bind hint. `0.0.0.0:0` = OS-chosen port.
    #[serde(default = "default_iroh_listen")]
    pub iroh_listen: SocketAddr,

    /// Where this host runs its own iroh packet relay. A page has no UDP
    /// socket, so it reaches a host only through a relay: the host advertises
    /// this one in the address it signs, and its own transport uses it too.
    /// `None` leaves the host on the public relays.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub packet_relay: Option<SocketAddr>,

    /// The origin pages dial the packet relay at when something terminates TLS
    /// in front of it (a deployment's ingress publishing `wss://`): the URL the
    /// host advertises. Without it that is the bind address's own `http://`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub packet_relay_url: Option<String>,

    /// Which peers this relay accepts. Defaults to the matching-
    /// user policy — only peers whose cert chains to the same user
    /// identity as this relay's cert.
    #[serde(default)]
    pub trust_policy: TrustPolicy,

    /// The work this device takes from other devices. A host serves a workspace placed on it only
    /// when the setting takes the placing person's work.
    #[serde(default = "default_work")]
    pub work: WorkSetting,

    /// Services requested by the operator. Runtime advertisement is clamped
    /// to the planes started by the composite home peer and its live residency
    /// proof.
    #[serde(default = "default_services")]
    pub services: ServiceCapabilities,

    /// Web entry base (ending in `/`) of the operator's versioned browser bundle,
    /// announced in catalogue locators so links are minted under it.
    /// This is a navigation destination, not an admission or content-access grant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_entry: Option<String>,
    /// Per-person change and content budgets, for a host whose workspaces admit
    /// the public. `None` limits nobody.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member_limits: Option<MemberLimitsConfig>,
    /// Pre-admissions a governed workspace keeps, spent ones included. A
    /// public workspace that pre-admits every visitor raises it. `None` keeps
    /// 1024. It keeps the key `max_room_grants` in `relay.toml`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "max_room_grants"
    )]
    pub max_workspace_grants: Option<usize>,
}

fn default_work() -> WorkSetting {
    WorkSetting::own_person()
}

fn default_services() -> ServiceCapabilities {
    ServiceCapabilities::relay_only()
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
            packet_relay: None,
            packet_relay_url: None,
            trust_policy: TrustPolicy::default(),
            work: default_work(),
            services: default_services(),
            web_entry: None,
            member_limits: None,
            max_workspace_grants: None,
        }
    }
}

/// Per-person limits a public host applies to members' traffic.
///
/// A guest (a person acting in a workspace that is not theirs, such as a
/// public lounge) spends the guest budget (`changes_per_window`, `burst`). A
/// person acting in a workspace of their own, or through their server key
/// anywhere, spends the owner budget (`owner_changes_per_window`,
/// `owner_burst`) instead: a backend making a workspace per customer runs at
/// full speed while guests stay paced.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberLimitsConfig {
    /// A guest's changes refilled per window.
    pub changes_per_window: u32,
    /// The window, in seconds, of both change budgets.
    pub window_secs: u32,
    /// Changes a guest may send at once.
    pub burst: u32,
    /// An owner's changes refilled per window.
    #[serde(default = "default_owner_changes_per_window")]
    pub owner_changes_per_window: u32,
    /// Changes an owner may send at once.
    #[serde(default = "default_owner_burst")]
    pub owner_burst: u32,
    /// Record content bytes a person may send per hour.
    pub content_bytes_per_hour: u64,
}

const fn default_owner_changes_per_window() -> u32 {
    1000
}

const fn default_owner_burst() -> u32 {
    1000
}

impl Default for MemberLimitsConfig {
    fn default() -> Self {
        Self {
            changes_per_window: 20,
            window_secs: 10,
            burst: 40,
            owner_changes_per_window: default_owner_changes_per_window(),
            owner_burst: default_owner_burst(),
            content_bytes_per_hour: 8 * 1024 * 1024,
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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two paths every hand-written config starts with.
    const PRELUDE: &str =
        "identity_root = \"/var/lib/tilo\"\nstate_root = \"/var/lib/tilo-relay\"\n";

    #[test]
    fn relay_config_defaults_and_toml_decoding() {
        let cfg = RelayConfig::default();
        assert_eq!(cfg.identity_root, PathBuf::from("/var/lib/tilo"));
        assert_eq!(cfg.state_root, PathBuf::from("/var/lib/tilo-relay"));
        assert_eq!(cfg.iroh_listen.port(), 0);
        assert_eq!(cfg.work, WorkSetting::own_person());
        assert!(cfg.services.workspace_replication);
        assert!(cfg.services.inbound_relay);
        assert!(!cfg.services.full_content_replication);
        assert!(cfg.services.remote_control_relay);
        assert!(!cfg.services.app_serving);
        assert!(!cfg.services.compute);

        // A minimal file names only the two roots.
        let _: RelayConfig = toml::from_str(PRELUDE).unwrap();

        // An unknown field is rejected rather than ignored.
        assert!(toml::from_str::<RelayConfig>(&format!("{PRELUDE}surprise = 3\n")).is_err());
    }

    #[test]
    fn relay_config_edits_are_idempotent_and_persist() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("relay.toml");
        let cfg = RelayConfig::default();
        cfg.save(&path).unwrap();
        assert_eq!(cfg, RelayConfig::load(&path).unwrap());
    }
}
