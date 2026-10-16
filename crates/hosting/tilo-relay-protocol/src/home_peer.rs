//! Versioned local registry records for enrolled always-on peers.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use tilo_user_identity::home_peer_registry_path;

/// Most devices [`remember_personal_device`] keeps in one list.
const PERSONAL_DEVICE_LIMIT: usize = 128;
/// Largest list file [`remember_personal_device`] reads.
const PERSONAL_DEVICE_FILE_LIMIT: usize = 64 * 1024;

/// This person's enrolled devices below a device identity root: a JSON array
/// of lowercase-hex device endpoint keys, beside the home-peer registry.
///
/// It is an authorization input. Remote actions may address only the devices
/// it lists, and certified secrets go only to them, so a device belongs in it
/// only once its certificate under this identity's root has been verified.
pub fn personal_device_peer_path(identity_root: &Path) -> PathBuf {
    home_peer_registry_path(identity_root).with_file_name("personal-device-peers.json")
}

/// Add `device_public_key` to the personal device list at `path`, keeping
/// every device already listed. Repeating it is a no-op, so an interrupted
/// enrollment can run it again.
///
/// # Errors
///
/// The existing list is unreadable, larger than 64 KiB, not a list of at most
/// 128 lowercase-hex 32-byte keys, or the atomic replacement fails. Nothing
/// is written then.
pub fn remember_personal_device(path: &Path, device_public_key: [u8; 32]) -> io::Result<()> {
    let id = hex::encode(device_public_key);
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error),
    };
    if bytes.len() > PERSONAL_DEVICE_FILE_LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "personal device list exceeds 64 KiB",
        ));
    }
    let mut ids: Vec<String> = if bytes.is_empty() {
        Vec::new()
    } else {
        serde_json::from_slice(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
    };
    if ids.len() > PERSONAL_DEVICE_LIMIT
        || ids.iter().any(|known| {
            known.len() != 64
                || hex::decode(known)
                    .ok()
                    .is_none_or(|decoded| decoded.len() != 32)
        })
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "personal device list is invalid",
        ));
    }
    if ids.iter().any(|known| known == &id) {
        return Ok(());
    }
    ids.push(id);
    let encoded = serde_json::to_vec_pretty(&ids)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Endpoint keys are public, and the session reads the list through the
    // device-trust group whatever umask the writer (a root-run join, the
    // identity host) runs under.
    #[cfg(unix)]
    {
        tilo_files::native::atomic::write_with_mode(
            path,
            &encoded,
            tilo_user_identity::persist::PUBLIC_FILE_MODE,
        )
    }
    #[cfg(not(unix))]
    {
        tilo_files::native::atomic::write(path, &encoded)
    }
}

/// Durable registry record written after enrollment acknowledgement completes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HomePeerRegistration {
    /// Registry schema version; readers accept only [`Self::CURRENT_SCHEMA`].
    pub schema_version: u16,
    /// Certified filesystem/device endpoint key, lowercase hex.
    pub device_node_id: String,
    /// Domain-separated relay service endpoint key, lowercase hex.
    pub relay_service_node_id: String,
    /// Last authenticated, network-addressable endpoint advertisement for
    /// the relay service. The service id alone is insufficient without an
    /// address lookup service; absent until the peer advertises one.
    #[serde(default)]
    pub relay_endpoint: Option<iroh_base::EndpointAddr>,
    /// User-visible device name.
    pub label: String,
    /// Root identity key, lowercase hex.
    pub user_public_key: String,
    /// Root-signed host certificate, retained so normal fleet revocation can
    /// manage a headless peer.
    #[serde(default)]
    pub device_cert: Option<tilo_user_identity::DeviceCert>,
    /// Relay room shards this peer was last configured to retain.
    pub relay_workspace_ids: Vec<u64>,
    /// What the peer last said it runs for environments: whether a worker
    /// hosts environment sessions, and whether one runs legacy apps. A
    /// device refreshes it from the peer's management status; a relay
    /// with no environment worker is not offered as a place to run one.
    #[serde(default)]
    pub environments: RelayEnvironments,
}

/// What a device last heard from its relays about environments, newer than
/// their registrations: `relay service node id (hex) → offer`. Kept per
/// user in their cache ([`RELAY_OFFERS_FILE`]), since the registrations
/// beside the identity keys are not the user's to rewrite.
pub type RelayOffers = std::collections::BTreeMap<String, RelayOffer>;

/// The file below the user's cache directory holding [`RelayOffers`].
pub const RELAY_OFFERS_FILE: &str = "tilo/relay-offers.json";

/// One relay's answer, and when it was asked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayOffer {
    /// What it said its workers run; kept from before when it did not
    /// answer.
    pub environments: RelayEnvironments,
    /// When it was last asked, Unix milliseconds.
    pub checked_at_ms: u64,
}

/// What a relay currently runs for environments, from its workers' live
/// advertisements.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayEnvironments {
    /// A worker hosts interactive environment sessions now.
    #[serde(default)]
    pub sessions: bool,
    /// A worker advertising `kvm` runs legacy apps now.
    #[serde(default)]
    pub legacy_apps: bool,
}

impl HomePeerRegistration {
    /// Schema version written by this build.
    pub const CURRENT_SCHEMA: u16 = 3;
}

/// Whether an operator-facing device label is safe to render as one line.
pub fn valid_home_peer_label(label: &str) -> bool {
    let label = label.trim();
    !label.is_empty()
        && label.len() <= 80
        && label.chars().all(|character| {
            !character.is_control()
                && !matches!(
                    character,
                    '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
                )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The list keeps every device once, survives a repeat and refuses a
    /// file it cannot vouch for without rewriting it.
    #[test]
    fn remembered_personal_devices_are_unique_bounded_and_validated() {
        let dir = tempfile::tempdir().unwrap();
        let path = personal_device_peer_path(dir.path());
        assert_eq!(path, dir.path().join("peers/personal-device-peers.json"));
        remember_personal_device(&path, [1; 32]).unwrap();
        remember_personal_device(&path, [2; 32]).unwrap();
        remember_personal_device(&path, [1; 32]).unwrap();
        let ids: Vec<String> = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(ids, vec![hex::encode([1; 32]), hex::encode([2; 32])]);

        std::fs::write(&path, br#"["not-a-key"]"#).unwrap();
        let error = remember_personal_device(&path, [3; 32]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(std::fs::read(&path).unwrap(), br#"["not-a-key"]"#);

        let full: Vec<String> = (0..=PERSONAL_DEVICE_LIMIT)
            .map(|index| hex::encode([u8::try_from(index % 256).unwrap(); 32]))
            .collect();
        std::fs::write(&path, serde_json::to_vec(&full).unwrap()).unwrap();
        assert_eq!(
            remember_personal_device(&path, [4; 32]).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}
