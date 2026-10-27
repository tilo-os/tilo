//! Bounded retained-notification bytes, using the native desktop's format.
//! Hosts provide storage; restart restores provenance and age while
//! sender actions and remote routes remain scoped to their original session.
//! Host-scheduled occurrence targets are durable and retain their exact room.
use super::{MAX_HISTORY, Notification, NotificationSeverity};
use serde::{Deserialize, Serialize};
use ui::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
/// exists. A host-scheduled target names a durable occurrence instead.
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
    #[serde(default)]
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    scheduled_target: Option<super::ScheduledNotificationTarget>,
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

/// Encode retained notifications, oldest first, for a host's private store.
pub fn encode_history(
    history: &[Notification],
    wall_now: SystemTime,
    now: Instant,
) -> Result<Vec<u8>, serde_json::Error> {
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
                    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
                    scheduled_target: notification
                        .scheduled_target
                        .clone()
                        .filter(super::ScheduledNotificationTarget::valid),
                    posted_unix_ms: wall_now_ms
                        .saturating_sub(u64::try_from(age.as_millis()).unwrap_or(u64::MAX)),
                }
            })
            .collect(),
    };
    serde_json::to_vec(&stored)
}

/// Restore bounded history with its original age and verified app attribution.
pub fn decode_history(
    bytes: &[u8],
    wall_now: SystemTime,
    now: Instant,
) -> Result<Vec<Notification>, serde_json::Error> {
    let stored: PersistedHistory = serde_json::from_slice(bytes)?;
    let wall_now_ms = unix_ms(wall_now);
    let skip = stored.notifications.len().saturating_sub(MAX_HISTORY);
    Ok(stored
        .notifications
        .into_iter()
        .skip(skip)
        .map(|entry| {
            let age = Duration::from_millis(wall_now_ms.saturating_sub(entry.posted_unix_ms));
            let mut notification = Notification::new(bounded(&entry.title), bounded(&entry.body))
                .with_severity(entry.severity.into())
                .with_created_at(now.checked_sub(age).unwrap_or(now));
            notification.app_name = entry.app_name.as_deref().map(bounded);
            notification.app_id = entry.app_id.as_deref().map(bounded);
            notification.source_workspace = entry.source_workspace;
            #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
            {
                notification.scheduled_target = entry
                    .scheduled_target
                    .filter(super::ScheduledNotificationTarget::valid);
            }
            notification.show_preview = entry.show_preview;
            notification.show_on_lock_screen = false;
            notification
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_retained_history_survives_reload_without_stale_actions() {
        let now = ui::time::now();
        let wall = UNIX_EPOCH + Duration::from_secs(1_000);
        let mut notification = Notification::new("Timer finished", "Tea is ready");
        notification.app_id = Some("so.tilo.clocks".into());
        notification.app_name = Some("Clocks".into());
        notification.source_workspace = Some(7);
        notification.actions = vec![("default".into(), "Open".into())];
        let bytes = encode_history(&[notification], wall, now).unwrap();
        let restored = decode_history(&bytes, wall + Duration::from_secs(30), now).unwrap();
        assert_eq!(restored.len(), 1);
        let notice = &restored[0];
        assert_eq!(notice.title, "Timer finished");
        assert_eq!(notice.body, "Tea is ready");
        assert_eq!(notice.app_id.as_deref(), Some("so.tilo.clocks"));
        assert_eq!(notice.source_workspace, Some(7));
        assert_eq!(
            now.duration_since(notice.created_at),
            Duration::from_secs(30)
        );
        assert!(notice.actions.is_empty());
        assert!(notice.remote_route.is_none());
    }

    #[test]
    fn stored_history_keeps_existing_count_and_utf8_text_bounds() {
        let history: Vec<_> = (0..MAX_HISTORY + 3)
            .map(|i| Notification::new(i.to_string(), "ä".repeat(MAX_PERSISTED_TEXT)))
            .collect();
        let now = ui::time::now();
        let bytes = encode_history(&history, UNIX_EPOCH, now).unwrap();
        let restored = decode_history(&bytes, UNIX_EPOCH, now).unwrap();
        assert_eq!(restored.len(), MAX_HISTORY);
        assert_eq!(restored[0].title, "3");
        assert_eq!(restored[0].body.len(), MAX_PERSISTED_TEXT);
        assert!(decode_history(b"broken", UNIX_EPOCH, now).is_err());
    }
}
