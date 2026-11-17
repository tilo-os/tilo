//! A workspace's entry in the owner record: the layout this build reads and
//! writes, the workspace named once by its id.

use serde::Deserialize;
use tilo_files::native::migration::Format;

use super::WORKSPACE_VERSION;

/// A workspace's entry ([`super::StoredWorkspace`]).
pub(crate) const FORMAT: Format = Format {
    name: "owner workspace record",
    version: WORKSPACE_VERSION,
    oldest: 5,
    steps: &[],
};

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
