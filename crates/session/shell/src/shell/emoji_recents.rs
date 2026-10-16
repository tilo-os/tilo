//! The emoji picker's recents as a private file in the user's state
//! directory.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use emoji::Recents;

/// Private, per-user file holding the recents between sessions.
#[derive(Clone, Debug)]
pub struct FileRecentsStore {
    path: PathBuf,
}

impl FileRecentsStore {
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// `$XDG_STATE_HOME/tilo/emoji-recents.json`, or `None` without a user
    /// state directory.
    pub fn for_user() -> Option<Self> {
        tilo_paths::state_dir().map(|root| Self::at(root.join("tilo/emoji-recents.json")))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Replace the file: directory `0700`, file `0600`, written atomically.
fn write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }
    tilo_files::native::atomic::write_with_mode(path, bytes, 0o600)
}

impl tilo_desktop::emoji_picker::RecentsStore for FileRecentsStore {
    /// A missing or malformed file reads as none.
    fn load(&self) -> Recents {
        std::fs::read(&self.path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// A pick runs on the compositor's loop, so the write goes to the state
    /// writer: its fsyncs can take seconds under I/O pressure. A newer save
    /// that has not started replaces this one; a failed write is logged.
    fn save(&self, recents: &Recents) -> Result<(), String> {
        let bytes = serde_json::to_vec(recents)
            .map_err(|error| format!("{}: {error}", self.path.display()))?;
        let path = self.path.clone();
        crate::shell::durable_writer::global().submit(path.clone(), move || {
            if let Err(error) = write(&path, &bytes) {
                tracing::warn!(%error, path = %path.display(), "could not save emoji recents");
            }
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_desktop::emoji_picker::RecentsStore as _;

    #[test]
    fn recents_persist_privately() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = FileRecentsStore::at(dir.path().join("state/tilo/emoji-recents.json"));
        let mut recents = Recents::default();
        recents.record("👍");
        recents.record("🎉");
        store.save(&recents).expect("saved");
        crate::shell::durable_writer::global().flush();
        let mode = std::fs::metadata(store.path())
            .expect("saved")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(
            FileRecentsStore::at(store.path()).load().items(),
            ["🎉".to_string(), "👍".to_string()]
        );
    }

    /// Picking an emoji runs on the compositor's loop; saving the recents
    /// must not wait for the disk there.
    #[cfg(target_os = "linux")]
    #[test]
    fn saving_a_pick_never_waits_for_the_disk() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = FileRecentsStore::at(dir.path().join("state/tilo/emoji-recents.json"));
        let saving = store.clone();
        crate::shell::disk_sync_guard::on_a_thread_that_cannot_sync(move || {
            let mut recents = Recents::default();
            recents.record("🦀");
            saving.save(&recents).expect("queued");
        });
        crate::shell::durable_writer::global().flush();
        assert_eq!(store.load().items(), ["🦀".to_string()]);
    }
}
