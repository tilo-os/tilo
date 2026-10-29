//! What the notification banners ask of the chrome beyond opening, closing
//! and choosing on a banner: answering or declining a call, an inline reply,
//! and reading a banner in full in the notification centre.

use super::ChromeState;
use crate::notifications::CloseReason;
use crate::shell::notification::remote_action_command;

impl ChromeState {
    /// Run sender action `key` of live banner `id` and close the banner:
    /// back on the device it was mirrored from, or to the local sender.
    fn invoke_sender_action(&mut self, id: u64, key: &str) -> bool {
        let Some(removed) = self.notification_log.take_active(id) else {
            return false;
        };
        if removed.remote_route.is_some() {
            if let Some(command) = remote_action_command(&removed, key) {
                self.queue_remote_notification_command(command);
            }
        } else if let Some(dbus_id) = removed.sender_id {
            self.send_notification_action(dbus_id, key.to_owned(), true);
        }
        self.record_history(removed);
        self.bump();
        true
    }

    /// Answer (or decline) the call live banner `id` rings for: the
    /// caller's own answer or decline when it offers one, else opening the
    /// call (its default action) or closing the banner.
    pub fn answer_call(&mut self, id: u64, answer: bool) -> bool {
        let keys: &[&str] = if answer {
            &["accept", "answer"]
        } else {
            &["decline", "reject", "hangup"]
        };
        let Some(key) = self
            .notification_log
            .active()
            .iter()
            .find(|notice| notice.id == id)
            .map(|notice| {
                notice
                    .actions
                    .iter()
                    .map(|(key, _)| key.clone())
                    .find(|key| keys.contains(&key.as_str()))
            })
        else {
            return false;
        };
        match key {
            Some(key) => self.invoke_sender_action(id, &key),
            None if answer => self.activate_notification(id),
            None => {
                self.dismiss_notification(id);
                true
            }
        }
    }

    /// Send `text` to the sender of live banner `id` as its inline reply
    /// (`NotificationReplied`), and close the banner. Only a local sender
    /// that offered the `inline-reply` action takes one.
    pub fn reply_to_notification(&mut self, id: u64, text: &str) -> bool {
        let takes_reply = self
            .notification_log
            .active()
            .iter()
            .find(|notice| notice.id == id)
            .is_some_and(|notice| {
                notice.sender_id.is_some()
                    && notice.remote_route.is_none()
                    && notice
                        .actions
                        .iter()
                        .any(|(key, _)| key == tilo_notification_banners::INLINE_REPLY)
            });
        if !takes_reply {
            return false;
        }
        let Some(removed) = self.notification_log.take_active(id) else {
            return false;
        };
        if let (Some(bridge), Some(dbus_id)) =
            (self.notification_bridge.as_ref(), removed.sender_id)
        {
            bridge.note_reply(dbus_id, text);
            bridge.note_closed(dbus_id, CloseReason::Dismissed);
        }
        self.record_history(removed);
        self.bump();
        true
    }

    /// Read live banner `id` in full: it moves to the notification centre,
    /// still unanswered, and the centre opens.
    pub fn read_notification(&mut self, id: u64) -> bool {
        let Some(removed) = self.notification_log.take_active(id) else {
            return false;
        };
        self.record_history(removed);
        self.open_quick_settings_level(tilo_quick_settings::QsLevel::Notifications);
        self.bump();
        true
    }
}
