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

/// The shim's own file name: the template `install` copies, never run itself.
pub const TEMPLATE_NAME: &str = "tilo-cli-shim";

/// What running the shim at `exe` without a readable sidecar says. Run under
/// its own name it is the template, which has no sidecar by design: say what
/// it is and what to run instead. A copy whose sidecar went missing names
/// the file it needs.
#[must_use]
pub fn missing_sidecar_message(exe: &Path, error: &str) -> String {
    if exe.file_name().is_some_and(|name| name == TEMPLATE_NAME) {
        return format!(
            "{TEMPLATE_NAME} is the program `tilo-install` copies for each command an \
             installed app declares; it does nothing run by itself.\n\
             Run that command by name, or `tilo` for this desktop's apps and actions \
             (`tilo actions`, `tilo do <action>`, `tilo open <app>`)."
        );
    }
    format!(
        "{TEMPLATE_NAME}: sidecar {} unreadable: {error}; reinstall the app that \
         provides this command",
        sidecar_path_for(exe).display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_template_run_by_itself_points_at_tilo() {
        let message = missing_sidecar_message(
            Path::new("/usr/libexec/tilo-cli-shim"),
            "No such file or directory",
        );
        assert!(message.contains("tilo-install"), "{message}");
        assert!(message.contains("`tilo actions`"), "{message}");
        assert!(!message.contains("sidecar"), "{message}");
    }

    #[test]
    fn a_copy_missing_its_sidecar_names_the_file() {
        let message = missing_sidecar_message(
            Path::new("/usr/lib/tilo-apps/bin/notes"),
            "No such file or directory",
        );
        assert!(
            message.contains("/usr/lib/tilo-apps/bin/notes.tilo-shim"),
            "{message}"
        );
        assert!(message.contains("reinstall"), "{message}");
    }
}
