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
        xdg_paths::state_dir().map(|root| Self::at(root.join("tilo/emoji-recents.json")))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Replace the file: directory `0700`, file `0600`, written atomically.
    fn write(&self, recents: &Recents) -> std::io::Result<()> {
        let bytes = serde_json::to_vec(recents).map_err(std::io::Error::other)?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        }
        tilo_files::native::atomic::write_with_mode(&self.path, &bytes, 0o600)
    }
}

impl tilo_desktop::emoji_picker::RecentsStore for FileRecentsStore {
    /// A missing or malformed file reads as none.
    fn load(&self) -> Recents {
        std::fs::read(&self.path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save(&self, recents: &Recents) -> Result<(), String> {
        self.write(recents)
            .map_err(|error| format!("{}: {error}", self.path.display()))
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
}
