//! What another device projected into this session's notification area: the
//! toasts and history rows it becomes, and how one is retired again.

use tilo_desktop::notifications::{ContinuityNotificationId, Notification};

use crate::notifications::NotificationBridge;

use super::ChromeState;

impl ChromeState {
    /// The projections expire and appear by `now_unix_ms`, the Unix
    /// milliseconds to judge them by: a live session reads the wall clock, a
    /// test sets the time instead of pinning the clock.
    pub(crate) fn pump_remote_continuity_notifications_at(
        &mut self,
        bridge: &NotificationBridge,
        now_unix_ms: u64,
    ) {
        use chrono::{Datelike, Timelike};

        bridge.expire_remote_continuity(now_unix_ms);
        for event in bridge.drain_remote_continuity_events() {
            match event {
                crate::notifications::RemoteContinuityEvent::Upsert(remote) => {
                    let app_id = remote.projection.app_id.clone();
                    let now = chrono::Local::now();
                    let effective = self.notification_policy.effective_for(
                        Some(&app_id),
                        now.timestamp(),
                        u16::try_from(now.hour().saturating_mul(60).saturating_add(now.minute()))
                            .unwrap_or(u16::MAX),
                        u8::try_from(now.weekday().num_days_from_monday()).unwrap_or(u8::MAX),
                    );
                    if !effective.cross_device {
                        continue;
                    }
                    let Some(mut notification) =
                        crate::shell::notification::from_remote_continuity(remote, now_unix_ms)
                    else {
                        continue;
                    };
                    if !effective.previews {
                        notification.body.clear();
                        notification.show_preview = false;
                    }
                    if self.replace_remote_notification(&notification) {
                        continue;
                    }
                    if effective.banners {
                        self.push_notification_full(notification);
                    } else {
                        self.record_history(notification);
                        self.bump();
                    }
                }
                crate::notifications::RemoteContinuityEvent::Retire { source_device, id } => {
                    self.retire_remote_notification(&source_device, id);
                }
            }
        }
    }

    fn replace_remote_notification(&mut self, replacement: &Notification) -> bool {
        let replaced = self.notification_log.replace_remote(replacement);
        if replaced {
            self.bump();
        }
        replaced
    }

    fn retire_remote_notification(&mut self, source_device: &str, id: ContinuityNotificationId) {
        if self.notification_log.retire_remote(source_device, id) {
            self.bump();
        }
    }
}
