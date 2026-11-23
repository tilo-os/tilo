//! A workspace's entry in the owner record: the layout this build reads and
//! writes, the workspace named once by its id.

use serde::Deserialize;
use tilo_files::native::migration::Format;

use super::WORKSPACE_VERSION;

/// A workspace's entry ([`super::StoredWorkspace`]).
pub(crate) const FORMAT: Format = Format {
    name: "owner workspace document",
    version: WORKSPACE_VERSION,
    oldest: 6,
    steps: &[without_join_log, journal_apart, record_types_moved],
};

/// Version 6 to 7: the entry no longer keeps the device's join log, which the workspace's own
/// service keeps. Everything else the device holds of the workspace reads as it was, so a device
/// that updates across this keeps its workspaces.
#[expect(
    clippy::needless_pass_by_value,
    reason = "a step takes the bytes it brings up, as every step of a format does"
)]
fn without_join_log(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    let mut entry: serde_json::Map<String, serde_json::Value> =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    entry.remove("join_ledger");
    entry.insert("version".into(), 7.into());
    serde_json::to_vec(&entry).map_err(|error| error.to_string())
}

/// Version 7 to 8: the journal beside the entry carries ownership's lists of writes apart from the
/// workspace's fields. The entry itself reads as it was; a journal an earlier build wrote beside
/// it is refused, as this build reads only the one it appends.
#[expect(
    clippy::needless_pass_by_value,
    reason = "a step takes the bytes it brings up, as every step of a format does"
)]
fn journal_apart(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    let mut entry: serde_json::Map<String, serde_json::Value> =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    entry.insert("version".into(), 8.into());
    serde_json::to_vec(&entry).map_err(|error| error.to_string())
}

/// Version 8 to 9: the entry's layout did not change. The write binding and target a journal entry
/// stores moved from `owner_commit` to `owner::publication` under new names, and the layout recorded
/// for this format names the file and the types, so it is pinned under a new version rather than
/// rewriting what version 8 recorded. No stored byte differs, so an entry and the journal beside it
/// read as they were, and this step rewrites only the version they name.
#[expect(
    clippy::needless_pass_by_value,
    reason = "a step takes the bytes it brings up, as every step of a format does"
)]
fn record_types_moved(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    let mut entry: serde_json::Map<String, serde_json::Value> =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    entry.insert("version".into(), 9.into());
    serde_json::to_vec(&entry).map_err(|error| error.to_string())
}

/// The version an entry's bytes name.
pub(crate) fn stored_version(bytes: &[u8]) -> Result<u32, String> {
    #[derive(Deserialize)]
    struct Versioned {
        version: u32,
    }
    serde_json::from_slice::<Versioned>(bytes)
        .map(|entry| entry.version)
        .map_err(|error| format!("{}: {error}", FORMAT.name))
}

#[cfg(test)]
#[path = "format_tests.rs"]
mod tests;
