//! The sidecar contract: `install` writes one beside every PATH shim it
//! places, and the `tilo-cli-shim` binary reads it back. Both sides use these
//! definitions, so the file layout and TOML shape cannot drift apart.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The suffix appended to the shim executable's path to find its sidecar.
pub const SIDECAR_SUFFIX: &str = ".tilo-shim";

/// Wire form of the sidecar TOML.
#[derive(Debug, Serialize, Deserialize)]
pub struct Sidecar {
    /// Reverse-DNS package id (`so.tilo.notes-cli`).
    pub package_id: String,
    /// Command name as declared in the manifest's `cli`.commands.
    /// Duplicated here (argv`0` also carries it) so renaming the shim
    /// doesn't silently rebind to a different command.
    pub command: String,
    /// Optional override for the launcher binary path, for out-of-tree
    /// testing; production installs leave it unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launcher: Option<PathBuf>,
}

/// The sidecar path for a shim executable at `exe`.
#[must_use]
pub fn sidecar_path_for(exe: &Path) -> PathBuf {
    let mut p = exe.to_path_buf();
    let name = p.file_name().map_or_else(
        || "tilo-cli-shim".to_string(),
        |s| s.to_string_lossy().into_owned(),
    );
    p.set_file_name(format!("{name}{SIDECAR_SUFFIX}"));
    p
}
