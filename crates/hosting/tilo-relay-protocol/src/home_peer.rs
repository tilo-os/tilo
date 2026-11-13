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
    /// Relay workspace shards this peer was last configured to retain.
    pub relay_workspace_ids: Vec<u64>,
}

impl HomePeerRegistration {
    /// Schema version written by this build.
    pub const CURRENT_SCHEMA: u16 = 3;
}

/// Largest home-peer registry document read.
const HOME_PEER_FILE_LIMIT: usize = 1024 * 1024;
/// Most peers one identity's registry holds.
const HOME_PEER_LIMIT: usize = 128;

/// The home-peer registry document at `path`, which
/// [`home_peer_registry_path`] names: every entry checked against the schema
/// this build writes. What admits a peer, renames it, re-addresses it or
/// removes it reads and writes it here, so one set of bounds holds.
///
/// # Errors
///
/// The file is larger than 1 MiB, is not a list of at most [`HOME_PEER_LIMIT`]
/// registrations this build writes, or an entry's certificate does not verify.
pub fn read_home_peers(path: &Path) -> io::Result<Vec<HomePeerRegistration>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) if bytes.len() <= HOME_PEER_FILE_LIMIT => bytes,
        Ok(_) => return Err(invalid("home-peer registry exceeds 1 MiB")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    checked_home_peers(&bytes)
}

/// `bytes` as a home-peer registry this build accepts: the check a staged
/// document gets before it is published as the registry.
///
/// # Errors
///
/// As [`read_home_peers`], without the file.
pub fn checked_home_peers(bytes: &[u8]) -> io::Result<Vec<HomePeerRegistration>> {
    let peers: Vec<HomePeerRegistration> = serde_json::from_slice(bytes)
        .map_err(|error| invalid(&format!("home-peer registry is not one: {error}")))?;
    if peers.len() > HOME_PEER_LIMIT || peers.iter().any(|entry| !writable(entry)) {
        return Err(invalid(
            "home-peer registry is not the schema this build writes",
        ));
    }
    Ok(peers)
}

/// Replace the registry document at `path` with `peers`, ordered by device key.
///
/// # Errors
///
/// More than [`HOME_PEER_LIMIT`] peers, an entry this build does not write, or
/// the atomic replacement fails. Nothing is written then.
pub fn write_home_peers(path: &Path, peers: &[HomePeerRegistration]) -> io::Result<()> {
    if peers.len() > HOME_PEER_LIMIT || peers.iter().any(|entry| !writable(entry)) {
        return Err(invalid(
            "home-peer registry is not the schema this build writes",
        ));
    }
    let mut peers = peers.to_vec();
    peers.sort_by(|left, right| left.device_node_id.cmp(&right.device_node_id));
    let encoded = serde_json::to_vec_pretty(&peers).map_err(|error| invalid(&error.to_string()))?;
    tilo_files::native::atomic::write_creating_parent_with_mode(path, &encoded, 0o644)
}

/// Whether `entry` is a registration this build writes: a schema this fits,
/// hex keys, a label one line long, an address of its own service, and a
/// certificate that verifies as this device's under this person.
fn writable(entry: &HomePeerRegistration) -> bool {
    let key = |text: &str| {
        hex::decode(text)
            .ok()
            .is_some_and(|bytes| bytes.len() == 32)
    };
    entry.schema_version == HomePeerRegistration::CURRENT_SCHEMA
        && entry.relay_workspace_ids.len() <= 4096
        && valid_home_peer_label(&entry.label)
        && key(&entry.device_node_id)
        && key(&entry.relay_service_node_id)
        && key(&entry.user_public_key)
        && entry.relay_endpoint.as_ref().is_none_or(|endpoint| {
            !endpoint.is_empty()
                && hex::decode(&entry.relay_service_node_id)
                    .ok()
                    .is_some_and(|service| service.as_slice() == endpoint.id.as_bytes())
        })
        && entry.device_cert.as_ref().is_some_and(|cert| {
            tilo_user_identity::verify_device_cert(cert).is_ok()
                && hex::encode(cert.device_public_key) == entry.device_node_id
                && hex::encode(cert.user_public_key) == entry.user_public_key
        })
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_owned())
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
