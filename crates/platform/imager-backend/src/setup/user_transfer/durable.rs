//! Private restart contract. Completion JSON is never proof of a copied tree;
//! recovery replays the canonical byte/fidelity receipts with renewed grants.
use super::{TransferRequest, flatpak};
use crate::setup::{
    destination_security, migration::PinnedDirectory, migration_durability as durability,
    migration_store::NativePath,
};
use std::{io, path::Path, sync::Arc};
const INTENT: &str = "user-transfer-intent.json";
const MUTATION: &str = "user-transfer-applications-started.json";
#[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Consent {
    schema: u32,
    source: NativePath,
    destination: NativePath,
    journals: NativePath,
    source_identity: durability::Identity,
    target_parent_identity: durability::Identity,
    copy_home: bool,
    copy_flatpak: bool,
    system_refs: Vec<flatpak::InstalledRef>,
}
fn failure() -> io::Error {
    io::Error::other("Transfer restart metadata is missing, changed or unsupported")
}
fn directory(request: &TransferRequest) -> io::Result<Arc<PinnedDirectory>> {
    let pin = PinnedDirectory::open_absolute(&request.private_journals)?;
    pin.validate_destination()?;
    destination_security::check_private_file(&pin.file)?;
    Ok(pin)
}
fn consent(request: &TransferRequest) -> io::Result<Consent> {
    let source = PinnedDirectory::open_absolute(&request.source_home)?;
    let parent = PinnedDirectory::open_absolute(request.target_home.parent().ok_or_else(failure)?)?;
    source.validate()?;
    parent.validate_destination()?;
    Ok(Consent {
        schema: 1,
        source: NativePath::encode(&request.source_home),
        destination: NativePath::encode(&request.target_home),
        journals: NativePath::encode(&request.private_journals),
        source_identity: durability::identity(&durability::readable_directory(&source.file)?)?,
        target_parent_identity: durability::identity(&durability::readable_directory(
            &parent.file,
        )?)?,
        copy_home: request.copy_home,
        copy_flatpak: request.copy_flatpak,
        system_refs: request.system_refs.clone(),
    })
}
pub use crate::user_transfer::{
    publish_private_artifact as publish, read_private_artifact as read,
};
pub(super) fn pending(request: &TransferRequest) -> io::Result<bool> {
    let pin = directory(request)?;
    match crate::setup::nofollow::metadata_nofollow(&pin.file, Path::new(INTENT)) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}
pub(super) fn save(request: &TransferRequest) -> io::Result<()> {
    let pin = directory(request)?;
    let bytes = serde_json::to_vec(&consent(request)?)?;
    publish(&pin, INTENT, &bytes)
}
pub(super) fn validate_recovery(request: &TransferRequest) -> Result<bool, String> {
    let pin = directory(request).map_err(|error| error.to_string())?;
    let bytes = read(&pin, INTENT).map_err(|error| error.to_string())?;
    let stored: Consent = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if stored != consent(request).map_err(|error| error.to_string())? {
        return Err(
            "Transfer source, destination or selection differs from the retained consent".into(),
        );
    }
    let started = match crate::setup::nofollow::metadata_nofollow(&pin.file, Path::new(MUTATION)) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.to_string()),
        Ok(_) => {
            if read(&pin, MUTATION).map_err(|error| error.to_string())? != bytes {
                return Err("Application mutation consent changed".into());
            }
            true
        }
    };
    Ok(started)
}
pub(super) fn applications_started(request: &TransferRequest) -> io::Result<()> {
    let pin = directory(request)?;
    let bytes = serde_json::to_vec(&consent(request)?)?;
    publish(&pin, MUTATION, &bytes)
}

pub(super) fn write_artifact(
    request: &TransferRequest,
    name: &str,
    value: &serde_json::Value,
) -> Result<(), String> {
    publish(
        &directory(request).map_err(|error| error.to_string())?,
        name,
        &serde_json::to_vec(value).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}
pub(super) fn read_artifact(
    request: &TransferRequest,
    name: &str,
) -> Result<Option<serde_json::Value>, String> {
    let pin = directory(request).map_err(|error| error.to_string())?;
    match read(&pin, name) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| error.to_string()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}
