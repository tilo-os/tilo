//! The private on-disk copy that lets the retained notification history
//! survive a compositor restart.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use tilo_desktop::notifications::{MAX_HISTORY, Notification, NotificationSeverity};

/// Longest title/body/app label kept on disk. The live toast never shows
/// more than a few lines; a hostile sender must not be able to grow the
/// private store without bound through one enormous notification.
const MAX_PERSISTED_TEXT: usize = 2048;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum PersistedSeverity {
    Info,
    Warning,
    Error,
}

impl From<NotificationSeverity> for PersistedSeverity {
    fn from(severity: NotificationSeverity) -> Self {
        match severity {
            NotificationSeverity::Info => Self::Info,
            NotificationSeverity::Warning => Self::Warning,
            NotificationSeverity::Error => Self::Error,
        }
    }
}

impl From<PersistedSeverity> for NotificationSeverity {
    fn from(severity: PersistedSeverity) -> Self {
        match severity {
            PersistedSeverity::Info => Self::Info,
            PersistedSeverity::Warning => Self::Warning,
            PersistedSeverity::Error => Self::Error,
        }
    }
}

/// The durable shape of one retained notification. Process-scoped routes
/// (the D-Bus id, a mirrored notification's restart-scoped id) and action
/// keys are deliberately absent: after a restart nothing they address still
/// exists.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct PersistedNotification {
    title: String,
    body: String,
    severity: PersistedSeverity,
    app_name: Option<String>,
    app_id: Option<String>,
    source_workspace: Option<u64>,
    show_preview: bool,
    posted_unix_ms: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PersistedHistory {
    notifications: Vec<PersistedNotification>,
}

fn bounded(text: &str) -> String {
    if text.len() <= MAX_PERSISTED_TEXT {
        return text.to_string();
    }
    let mut end = MAX_PERSISTED_TEXT;
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.get(..end).unwrap_or_default().to_string()
}

fn unix_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}

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
        xdg_paths::state_dir().map(|root| Self::at(root.join("tilo/notification-history.json")))
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
        let Ok(stored) = serde_json::from_slice::<PersistedHistory>(&bytes) else {
            tracing::warn!(
                path = %self.path.display(),
                "notification history is unreadable; starting empty"
            );
            return Vec::new();
        };
        let wall_now_ms = unix_ms(wall_now);
        let skip = stored.notifications.len().saturating_sub(MAX_HISTORY);
        stored
            .notifications
            .into_iter()
            .skip(skip)
            .map(|entry| {
                let age = Duration::from_millis(wall_now_ms.saturating_sub(entry.posted_unix_ms));
                let mut notification =
                    Notification::new(bounded(&entry.title), bounded(&entry.body))
                        .with_severity(entry.severity.into())
                        .with_created_at(now.checked_sub(age).unwrap_or(now));
                notification.app_name = entry.app_name.as_deref().map(bounded);
                notification.app_id = entry.app_id.as_deref().map(bounded);
                notification.source_workspace = entry.source_workspace;
                notification.show_preview = entry.show_preview;
                notification.show_on_lock_screen = false;
                notification
            })
            .collect()
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
        let wall_now_ms = unix_ms(wall_now);
        let skip = history.len().saturating_sub(MAX_HISTORY);
        let stored = PersistedHistory {
            notifications: history
                .iter()
                .skip(skip)
                .map(|notification| {
                    let age = now.saturating_duration_since(notification.created_at);
                    PersistedNotification {
                        title: bounded(&notification.title),
                        body: bounded(&notification.body),
                        severity: notification.severity.into(),
                        app_name: notification.app_name.as_deref().map(bounded),
                        app_id: notification.app_id.as_deref().map(bounded),
                        source_workspace: notification.source_workspace,
                        show_preview: notification.show_preview,
                        posted_unix_ms: wall_now_ms
                            .saturating_sub(u64::try_from(age.as_millis()).unwrap_or(u64::MAX)),
                    }
                })
                .collect(),
        };
        let bytes = serde_json::to_vec(&stored).map_err(std::io::Error::other)?;
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
