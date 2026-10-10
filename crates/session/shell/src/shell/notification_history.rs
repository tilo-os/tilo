//! The private on-disk copy that lets the retained notification history
//! survive a compositor restart.

use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use tilo_desktop::notifications::Notification;

/// Private, bounded file holding the retained history between sessions.
#[derive(Clone, Debug)]
pub struct NotificationHistoryStore {
    path: PathBuf,
}

impl NotificationHistoryStore {
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// `$XDG_STATE_HOME/tilo/notification-history.json`, or `None` when the
    /// session has no user state directory (tests, offscreen renders).
    pub fn for_user() -> Option<Self> {
        tilo_paths::state_dir().map(|root| Self::at(root.join("tilo/notification-history.json")))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read the retained history, oldest first. A missing, unreadable or
    /// malformed file yields an empty history; the next save replaces it.
    pub fn load(&self) -> Vec<Notification> {
        self.load_at(SystemTime::now(), Instant::now())
    }

    fn load_at(&self, wall_now: SystemTime, now: Instant) -> Vec<Notification> {
        let Ok(bytes) = std::fs::read(&self.path) else {
            return Vec::new();
        };
        tilo_desktop::notifications::decode_history(&bytes, wall_now, now)
            .unwrap_or_else(|error| {
                tracing::warn!(path = %self.path.display(), %error, "notification history is unreadable; starting empty");
                Vec::new()
            })
    }

    /// Replace the file with `history` (oldest first, already bounded by the
    /// caller). The directory is `0700` and the file `0600`, written
    /// atomically so a crash mid-write never leaves a torn history.
    ///
    /// # Errors
    ///
    /// Returns the I/O error when the directory or file cannot be written.
    pub fn save(&self, history: &[Notification]) -> std::io::Result<()> {
        self.save_at(history, SystemTime::now(), Instant::now())
    }

    fn save_at(
        &self,
        history: &[Notification],
        wall_now: SystemTime,
        now: Instant,
    ) -> std::io::Result<()> {
        let bytes = tilo_desktop::notifications::encode_history(history, wall_now, now)
            .map_err(std::io::Error::other)?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
            }
        }
        tilo_files::native::atomic::write_with_mode(&self.path, &bytes, 0o600)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tilo_desktop::notifications::MAX_HISTORY;
    const MAX_PERSISTED_TEXT: usize = 2048;

    fn from_app(app: &str, title: &str) -> Notification {
        let mut notification = Notification::new(title, "");
        notification.app_id = Some(app.to_string());
        notification.app_name = Some(app.to_uppercase());
        notification
    }

    #[test]
    fn the_store_is_private_bounded_and_round_trips_age_and_provenance() {
        let dir = tempfile::tempdir().unwrap();
        let store = NotificationHistoryStore::at(dir.path().join("state/tilo/history.json"));
        let now = Instant::now();
        let wall = SystemTime::now();
        let mut history: Vec<Notification> = (0..MAX_HISTORY + 5)
            .map(|index| from_app("mail", &format!("n{index}")))
            .collect();
        let last = history.last_mut().unwrap();
        last.body = "x".repeat(MAX_PERSISTED_TEXT * 2);
        last.sender_id = Some(7);
        last.actions = vec![("default".into(), "Open".into())];
        last.created_at = now.checked_sub(Duration::from_secs(120)).unwrap();
        store.save_at(&history, wall, now).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(store.path())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "the history file is private");
        }

        let restored = store.load_at(wall, now);
        assert_eq!(restored.len(), MAX_HISTORY);
        assert_eq!(restored.first().unwrap().title, "n5");
        let newest = restored.last().unwrap();
        assert_eq!(newest.title, format!("n{}", MAX_HISTORY + 4));
        assert_eq!(newest.body.len(), MAX_PERSISTED_TEXT);
        assert_eq!(newest.app_id.as_deref(), Some("mail"));
        assert_eq!(
            newest.sender_id, None,
            "a D-Bus id never outlives its daemon"
        );
        assert!(newest.actions.is_empty());
        let age = now.saturating_duration_since(newest.created_at);
        assert!(
            (Duration::from_secs(119)..=Duration::from_secs(121)).contains(&age),
            "age survives the round trip, got {age:?}"
        );
    }

    #[test]
    fn a_malformed_store_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.json");
        std::fs::write(&path, b"{not json").unwrap();
        assert!(NotificationHistoryStore::at(path).load().is_empty());
    }
}
