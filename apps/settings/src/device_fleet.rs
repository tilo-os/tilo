//! Small Settings-facing view of the root-signed personal-device fleet.
//!
//! The shell owns enrollment transport, while this module reads and updates
//! the same versioned fleet file for the explicit decommission flow. Secret
//! key material never enters widget/Automerge state.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tilo_user_identity::{
    DeviceCert, RevocationEntry, RevocationReason, revocation_signing_digest, verify_device_cert,
    verify_revocation_entry,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredFleet {
    schema_version: u32,
    identity_id: String,
    certificates: Vec<DeviceCert>,
    revocations: Vec<RevocationEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DeviceView {
    pub label: String,
    pub key: [u8; 32],
    pub fingerprint: String,
    pub current: bool,
    pub revoked: bool,
    pub can_host: bool,
}

fn active_identity() -> io::Result<(PathBuf, runtime_identity::IdentitySummary)> {
    let dir = runtime_identity::identity_dir_from_env()?;
    let summary = runtime_identity::list_identities(&dir)?
        .into_iter()
        .find(|identity| identity.active)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no active identity"))?;
    Ok((dir, summary))
}

fn fleet_path(dir: &Path, identity_id: &str) -> PathBuf {
    dir.join("fleets").join(format!("{identity_id}.json"))
}

fn load_stored(dir: &Path, identity_id: &str) -> io::Result<StoredFleet> {
    let fleet: StoredFleet = serde_json::from_slice(&std::fs::read(fleet_path(dir, identity_id))?)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if fleet.schema_version != 1 || fleet.identity_id != identity_id {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "device fleet identity/schema mismatch",
        ));
    }
    for certificate in &fleet.certificates {
        verify_device_cert(certificate)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    }
    for revocation in &fleet.revocations {
        verify_revocation_entry(revocation)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    }
    Ok(fleet)
}

fn current_device_key() -> io::Result<[u8; 32]> {
    let paths =
        tilo_user_identity::IdentityPaths::under(super::panes::device_mesh::identity_root());
    tilo_user_identity::persist::read_device_key(&paths.device_privkey)
        .map_err(|error| io::Error::other(error.to_string()))
        .map(|key| key.verifying_key().to_bytes())
}

fn fingerprint(key: &[u8; 32]) -> String {
    key[..6]
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn list() -> io::Result<Vec<DeviceView>> {
    let (dir, identity) = active_identity()?;
    let fleet = load_stored(&dir, &identity.id)?;
    if fleet
        .certificates
        .iter()
        .any(|certificate| certificate.user_public_key != identity.public_key)
        || fleet
            .revocations
            .iter()
            .any(|revocation| revocation.user_public_key != identity.public_key)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "device fleet contains another identity",
        ));
    }
    let current = current_device_key()?;
    Ok(fleet
        .certificates
        .iter()
        .map(|certificate| DeviceView {
            label: certificate.device_label.clone(),
            key: certificate.device_public_key,
            fingerprint: fingerprint(&certificate.device_public_key),
            current: current == certificate.device_public_key,
            revoked: fleet
                .revocations
                .iter()
                .any(|entry| entry.revoked_device_public_key == certificate.device_public_key),
            can_host: certificate.capabilities.can_act_as_host,
        })
        .collect())
}

pub(crate) fn revoke(device: [u8; 32], reason: RevocationReason) -> io::Result<()> {
    let (dir, identity) = active_identity()?;
    if current_device_key()? == device {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "use another trusted device to revoke the device you are using",
        ));
    }
    let mut fleet = load_stored(&dir, &identity.id)?;
    if !fleet
        .certificates
        .iter()
        .any(|certificate| certificate.device_public_key == device)
    {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "device is not enrolled",
        ));
    }
    if fleet
        .revocations
        .iter()
        .any(|entry| entry.revoked_device_public_key == device)
    {
        return Ok(());
    }
    let record = runtime_identity::resolve_active_identity(&dir)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "active identity is unavailable"))?;
    let signer = runtime_identity::load_from_path(record)
        .map_err(|error| io::Error::other(error.to_string()))?;
    if signer.group_public_key() != identity.public_key {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "active signing identity does not match its device fleet",
        ));
    }
    let revoked_at_ns = ui::time::unix_now_ns();
    let mut entry = RevocationEntry {
        user_public_key: identity.public_key,
        revoked_device_public_key: device,
        revoked_at_ns,
        reason,
        signature: [0; 64],
    };
    let digest =
        revocation_signing_digest(&entry).map_err(|error| io::Error::other(error.to_string()))?;
    entry.signature = signer
        .sign(&digest)
        .map_err(|error| io::Error::other(error.to_string()))?;
    verify_revocation_entry(&entry).map_err(|error| io::Error::other(error.to_string()))?;
    fleet.revocations.push(entry);
    let bytes = serde_json::to_vec_pretty(&fleet)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    tilo_files::native::atomic::write_with_mode(&fleet_path(&dir, &identity.id), &bytes, 0o600)
}
