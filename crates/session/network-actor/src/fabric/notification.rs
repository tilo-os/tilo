//! A notification mirrored from another of the person's devices.

use serde::{Deserialize, Serialize};
pub use tilo_session_core::desktop::notifications::ContinuityNotificationId;

/// Longest a mirrored notification may stay up on another device.
pub const MAX_CONTINUITY_NOTIFICATION_TTL_MS: u64 = 15 * 60 * 1_000;
const MAX_CONTINUITY_ACTIONS: usize = 16;

/// Whether `id` is a well-formed notification app id.
///
/// This is a wire check that both the sending device and the receiving device
/// run on ids chosen by host applications, so it keeps the wire rule
/// (non-empty, at most 160 bytes, ASCII letters, digits, `.`, `-`, `_`) and
/// must never be stricter than what a device on an earlier build sends.
#[must_use]
pub fn valid_app_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 160
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContinuityNotificationProjection {
    pub id: ContinuityNotificationId,
    pub app_id: String,
    pub app_name: String,
    pub title: String,
    pub body: String,
    pub body_redacted: bool,
    pub actions: Vec<(String, String)>,
    pub urgency: u8,
    pub expires_unix_ms: u64,
}

impl ContinuityNotificationProjection {
    #[must_use]
    pub fn validate_at(&self, now_unix_ms: u64) -> bool {
        self.id.epoch != [0; 16]
            && self.id.token != [0; 16]
            && valid_app_id(&self.app_id)
            && !self.app_name.is_empty()
            && self.app_name.len() <= 160
            && self.title.len() <= 512
            && self.body.len() <= 4_096
            && self.urgency <= 2
            && self.actions.len() <= MAX_CONTINUITY_ACTIONS
            && self.expires_unix_ms > now_unix_ms
            && self.expires_unix_ms.saturating_sub(now_unix_ms)
                <= MAX_CONTINUITY_NOTIFICATION_TTL_MS
            && [&self.app_name, &self.title]
                .into_iter()
                .all(|value| !value.chars().any(char::is_control))
            && self
                .body
                .chars()
                .all(|character| !character.is_control() || matches!(character, '\n' | '\t'))
            && (!self.body_redacted || self.body.is_empty())
            && self.actions.iter().all(|(key, label)| {
                !key.is_empty()
                    && key.len() <= 128
                    && label.len() <= 256
                    && !key.chars().any(char::is_control)
                    && !label.chars().any(char::is_control)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_app_ids_match_wire_rule_up_to_160_bytes() {
        let longest = "a".repeat(160);
        let too_long = "a".repeat(161);
        let too_long_dotted = format!("a.{}", "b".repeat(159));
        for (id, expected) in [
            ("so.tilo.mail", true),
            ("org.example.My_App-2", true),
            ("firefox", true),
            ("kde4-konsole", true),
            ("-foo", true),
            ("-org.example.Mail", true),
            (".foo", true),
            ("foo.", true),
            ("a..b", true),
            ("..", true),
            (".", true),
            (longest.as_str(), true),
            (too_long.as_str(), false),
            (too_long_dotted.as_str(), false),
            ("", false),
            ("org/example", false),
            ("org.example mail", false),
            ("so.tilo.n\u{f6}tes", false),
        ] {
            assert_eq!(valid_app_id(id), expected, "{id:?}");
            let projection = ContinuityNotificationProjection {
                id: ContinuityNotificationId {
                    epoch: [1; 16],
                    token: [2; 16],
                },
                app_id: id.to_owned(),
                app_name: "App".into(),
                title: "Notice".into(),
                body: "Body".into(),
                body_redacted: false,
                actions: Vec::new(),
                urgency: 1,
                expires_unix_ms: 20_000,
            };
            assert_eq!(projection.validate_at(10_000), expected, "{id:?}");
        }
    }
}
