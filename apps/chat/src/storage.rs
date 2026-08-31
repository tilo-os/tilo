//! Where an older release of Chat kept its workspace.
//!
//! The whole convergent document — workspaces, categories, channels,
//! messages, reactions, edit history, members, voice membership — was saved
//! as raw Automerge bytes to a single file:
//!
//! ```text
//! $XDG_DATA_HOME/qos-chat/workspace.automerge   (≈ ~/.local/share/qos-chat/…)
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

/// Resolve the data directory: `$QOS_CHAT_DATA` when set (the OS points chat
/// at a per-sandbox store, and tests pin a temp dir here to stay hermetic),
/// else `$XDG_DATA_HOME/qos-chat`, falling back to `$HOME/.local/share/qos-chat`.
/// `None` only if none can be located (extreme minimal containers) — the binary
/// then runs in-memory only.
pub fn data_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("QOS_CHAT_DATA") {
        return Some(PathBuf::from(dir));
    }
    dirs::data_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".local/share")))
        .map(|d| d.join("qos-chat"))
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

    #[test]
    fn load_returns_none_when_missing() {
        let tmp = TempDir::new().unwrap();
        assert!(load_bytes(tmp.path()).unwrap().is_none());
    }

    #[test]
    fn the_legacy_document_is_readable_from_its_directory() {
        // This is the whole remaining job: hand `open_migrating` a path it
        // can read an older release's Automerge save from.
        let tmp = TempDir::new().unwrap();
        let bytes = b"automerge-save-bytes".to_vec();
        std::fs::write(workspace_path(tmp.path()), &bytes).unwrap();
        assert_eq!(load_bytes(tmp.path()).unwrap().unwrap(), bytes);
    }

    #[test]
    fn the_workspace_file_name_is_the_one_older_releases_wrote() {
        // Changing this silently orphans every existing user's history.
        let p = workspace_path(Path::new("/data/qos-chat"));
        assert_eq!(p, Path::new("/data/qos-chat/workspace.automerge"));
    }

    #[test]
    fn this_module_no_longer_writes() {
        // A guard against the dual-write coming back: the only reason this
        // module still exists is to point `open_migrating` at the legacy
        // file. The needles are assembled at runtime so this assertion does
        // not match itself.
        // Only the production half — the tests below legitimately write a
        // legacy file to have something for `open_migrating` to import.
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
