//! A workspace's entry in the owner record: the layout this build reads and
//! writes, the workspace named once by its id.

use serde::Deserialize;
use tilo_files::native::migration::Format;

use super::WORKSPACE_VERSION;

/// A workspace's entry ([`super::StoredWorkspace`]).
pub(crate) const FORMAT: Format = Format {
    name: "owner workspace document",
    version: WORKSPACE_VERSION,
    oldest: 10,
    steps: &[without_ownership],
};

/// Version 10 to 11: the entry no longer keeps the outgoing ownership a build before the one path
/// kept. No build since version 10 changed it (a write of this device's is made durable and sent
/// on a link), so it held only writes the workspace already has or will never take, and it goes.
/// The journal beside a version 10 entry is a delta of the fields that build split, ownership
/// with them ([`crate::owner::journal::replay`]).
#[expect(
    clippy::needless_pass_by_value,
    reason = "a step takes the bytes it brings up, as every step of a format does"
)]
fn without_ownership(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    let mut entry: serde_json::Map<String, serde_json::Value> =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    entry.shift_remove("publication");
    entry.insert("version".into(), 11.into());
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
