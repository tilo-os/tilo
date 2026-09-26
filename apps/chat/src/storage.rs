//! Where an older release of Chat kept its workspace.
//!
//! The whole convergent document — workspaces, categories, channels,
//! messages, reactions, edit history, members, voice membership — was saved
//! as raw Automerge bytes to a single file:
//!
//! ```text
//! $XDG_DATA_HOME/tilo-chat/workspace.automerge   (≈ ~/.local/share/tilo-chat/…)
//! ```
//!
//! That file is where existing users' history lives, so this module still
//! resolves it: `main` hands the path to `ui::persist::open_migrating`, which
//! merges it into the shared store once and leaves the original alone.
//!
//! Nothing writes through here. The debounced background saver this module
//! used to own was a second live writer alongside `app-state`'s — two files,
//! both updated on every edit, on two independent 400 ms timers, free to
//! disagree after a crash. The saver is gone; `app-state` is the writer.

use std::io;
use std::path::{Path, PathBuf};

/// File name of the Automerge save inside the data directory.
pub const WORKSPACE_FILE: &str = "workspace.automerge";

/// Resolve the data directory: `$TILO_CHAT_DATA` when set (the OS points chat
/// at a per-sandbox store, and tests pin a temp dir here to stay hermetic),
/// else `$XDG_DATA_HOME/tilo-chat`, falling back to `$HOME/.local/share/tilo-chat`.
/// `None` only if none can be located (extreme minimal containers) — the binary
/// then runs in-memory only.
pub fn data_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("TILO_CHAT_DATA") {
        return Some(PathBuf::from(dir));
    }
    xdg_paths::data_dir()
        .or_else(|| xdg_paths::home().map(|h| h.join(".local/share")))
        .map(|d| d.join("tilo-chat"))
}

/// Path of the Automerge save inside `dir`.
pub fn workspace_path(dir: &Path) -> PathBuf {
    dir.join(WORKSPACE_FILE)
}

/// Read the persisted Automerge bytes, if a store exists. `Ok(None)` on
/// first launch (no file yet) — the caller seeds a fresh workspace.
///
/// # Errors
/// Returns the underlying I/O error for anything other than the file simply
/// not existing yet (e.g. a permissions problem reading the workspace file).
pub fn load_bytes(dir: &Path) -> io::Result<Option<Vec<u8>>> {
    match std::fs::read(workspace_path(dir)) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// The module's whole remaining job: point `open_migrating` at the file
    /// older releases wrote, read it, and never write it again.
    #[test]
    fn legacy_workspace_file_is_read_from_its_historic_path_and_never_written() {
        // Missing: no legacy document means `None`, not an error.
        let tmp = TempDir::new().unwrap();
        assert!(
            load_bytes(tmp.path()).unwrap().is_none(),
            "case missing: load returns None"
        );

        // Readable: hand `open_migrating` a path it can read an older
        // release's Automerge save from.
        let tmp = TempDir::new().unwrap();
        let bytes = b"automerge-save-bytes".to_vec();
        std::fs::write(workspace_path(tmp.path()), &bytes).unwrap();
        assert_eq!(
            load_bytes(tmp.path()).unwrap().unwrap(),
            bytes,
            "case readable: the legacy document is readable from its directory"
        );

        // File name: changing this silently orphans every existing user's history.
        let p = workspace_path(Path::new("/data/tilo-chat"));
        assert_eq!(
            p,
            Path::new("/data/tilo-chat/workspace.automerge"),
            "case file name: the one older releases wrote"
        );

        // No writes: a guard against the dual-write coming back. The needles
        // are assembled at runtime so this assertion does not match itself.
        // Only the production half — the tests legitimately write a legacy
        // file to have something for `open_migrating` to import.
        let src = include_str!("storage.rs");
        let production = src
            .split(["#[cfg", "(test)]"].concat().as_str())
            .next()
            .unwrap();
        for needle in [["fs::", "write("].concat(), ["fs::", "rename("].concat()] {
            assert!(
                !production.contains(&needle),
                "storage.rs regained `{needle}` — app-state owns persistence, \
                 and a second writer here is what made two files disagree"
            );
        }
    }
}
