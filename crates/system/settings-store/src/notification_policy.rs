//! Shared notification-delivery policy.
//!
//! The compositor enforces this record; Personal Settings only edits it. App
//! keys are verified package/application ids supplied by the launcher caller
//! map, never the sender-controlled freedesktop `app_name` string.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "model", derive(tilo_resource::Model))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub struct AppNotificationRule {
    /// `None` follows the global preference.
    #[serde(default)]
    pub banners: Option<bool>,
    #[serde(default)]
    pub sounds: Option<bool>,
    #[serde(default)]
    pub badges: Option<bool>,
    #[serde(default)]
    pub previews: Option<bool>,
    #[serde(default)]
    pub lock_screen: Option<bool>,
    /// `None` follows the global preference. Only notifications with a
    /// compositor-verified application identity are ever eligible.
    #[serde(default)]
    pub cross_device: Option<bool>,
    /// A user-approved priority app may pierce a scheduled focus period. This
    /// cannot be requested by the app itself.
    #[serde(default)]
    pub priority: bool,
    /// Permit this verified communications app to assert a sender endpoint
    /// which the shell checks against its Contacts-owned priority registry.
    /// The app cannot make an arbitrary contact priority and no unverified
    /// caller may use this path.
    #[serde(default)]
    pub priority_contacts: bool,
    /// Absolute Unix deadline for a temporary mute. `None` is not muted.
    #[serde(default)]
    pub muted_until: Option<i64>,
}

crate::read_through_json_value!(AppNotificationRule);

#[cfg_attr(feature = "model", derive(tilo_resource::Model))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub struct FocusSchedule {
    #[serde(default)]
    pub enabled: bool,
    /// Minutes after local midnight, clamped to a valid day on load.
    #[serde(default = "default_focus_start")]
    pub start_minute: u16,
    #[serde(default = "default_focus_end")]
    pub end_minute: u16,
    /// Monday is bit zero; zero means every day for backwards-compatible
    /// hand-written settings.
    #[serde(default)]
    pub weekdays: u8,
    #[serde(default = "default_true")]
    pub allow_priority: bool,
}

crate::read_through_json_value!(FocusSchedule);

const fn default_focus_start() -> u16 {
    22 * 60
}
const fn default_focus_end() -> u16 {
    7 * 60
}
const fn default_true() -> bool {
    true
}

impl Default for FocusSchedule {
    fn default() -> Self {
        Self {
            enabled: false,
            start_minute: default_focus_start(),
            end_minute: default_focus_end(),
            weekdays: 0,
            allow_priority: true,
        }
    }
}

impl FocusSchedule {
    /// The minute after local midnight the current focus period ends, or
    /// `None` when no bounded period is active (off, or all day).
    pub fn active_until(self, minute: u16, weekday_from_monday: u8) -> Option<u16> {
        let end = self.end_minute.min(1439);
        (self.active_at(minute, weekday_from_monday) && self.start_minute.min(1439) != end)
            .then_some(end)
    }

    pub fn active_at(self, minute: u16, weekday_from_monday: u8) -> bool {
        if !self.enabled {
            return false;
        }
        let day_enabled = self.weekdays == 0
            || (weekday_from_monday < 7 && self.weekdays & (1_u8 << weekday_from_monday) != 0);
        if !day_enabled {
            return false;
        }
        let minute = minute.min(1439);
        let start = self.start_minute.min(1439);
        let end = self.end_minute.min(1439);
        if start == end {
            return true;
        }
        if start < end {
            (start..end).contains(&minute)
        } else {
            minute >= start || minute < end
        }
    }
}

// Independent, user-facing notification toggles, not a state machine — and this
// document's JSON shape and field names are load-bearing across `shell`, so folding
// them into a bitflags type ripples well outside this crate.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent notification toggles serialised by field name; consumers in other \
              crates read and persist them directly"
)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationPolicy {
    #[serde(default)]
    pub do_not_disturb: bool,
    #[serde(default = "default_true")]
    pub show_banners: bool,
    #[serde(default = "default_true")]
    pub show_previews: bool,
    #[serde(default = "default_true")]
    pub play_sounds: bool,
    #[serde(default)]
    pub show_on_lock_screen: bool,
    #[serde(default = "default_true")]
    pub show_badges: bool,
    /// Project eligible notifications to this person's other certified Tilo
    /// devices. Per-app policy and sender sensitivity can only narrow this.
    #[serde(default = "default_true")]
    pub cross_device: bool,
    #[serde(default)]
    pub focus: FocusSchedule,
    #[serde(default)]
    pub per_app: BTreeMap<String, AppNotificationRule>,
}

impl Default for NotificationPolicy {
    fn default() -> Self {
        Self {
            do_not_disturb: false,
            show_banners: true,
            show_previews: true,
            play_sounds: true,
            show_on_lock_screen: false,
            show_badges: true,
            cross_device: true,
            focus: FocusSchedule::default(),
            per_app: BTreeMap::new(),
        }
    }
}

// Independent facets of "should this notification show right now", each derived by a
// different rule — not a state machine, and `shell` reads them by field to decide what to
// render.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent derived notification facets read by field in `shell`"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EffectiveNotificationPolicy {
    pub banners: bool,
    pub sounds: bool,
    pub badges: bool,
    pub previews: bool,
    pub lock_screen: bool,
    pub cross_device: bool,
    pub quiet: bool,
}

impl NotificationPolicy {
    /// The one persisted Do Not Disturb switch. The shell's palette action and
    /// Quick Settings tile write it here; Personal Settings edits the same key.
    ///
    /// # Errors
    ///
    /// Returns an error if no config directory is available or the settings
    /// document cannot be written.
    pub fn save_do_not_disturb(on: bool) -> std::io::Result<()> {
        crate::update_section("notifications", Self::do_not_disturb_values(on))
    }

    fn do_not_disturb_values(on: bool) -> serde_json::Map<String, serde_json::Value> {
        let mut values = serde_json::Map::new();
        values.insert("do_not_disturb".into(), serde_json::Value::Bool(on));
        values
    }

    pub fn load_at(path: &std::path::Path) -> Self {
        serde_json::from_value(serde_json::Value::Object(crate::load_section_at(
            path,
            "notifications",
        )))
        .unwrap_or_default()
    }

    pub fn load() -> Self {
        serde_json::from_value(serde_json::Value::Object(crate::load_section(
            "notifications",
        )))
        .unwrap_or_default()
    }

    pub fn effective_for(
        &self,
        verified_app_id: Option<&str>,
        unix_now: i64,
        local_minute: u16,
        weekday_from_monday: u8,
    ) -> EffectiveNotificationPolicy {
        self.effective_for_context(
            verified_app_id,
            false,
            unix_now,
            local_minute,
            weekday_from_monday,
        )
    }

    pub fn effective_for_context(
        &self,
        verified_app_id: Option<&str>,
        verified_priority_contact: bool,
        unix_now: i64,
        local_minute: u16,
        weekday_from_monday: u8,
    ) -> EffectiveNotificationPolicy {
        let rule = verified_app_id.and_then(|id| self.per_app.get(id));
        let temporarily_muted = rule
            .and_then(|r| r.muted_until)
            .is_some_and(|deadline| deadline > unix_now);
        let focus = self.focus.active_at(local_minute, weekday_from_monday);
        let pierces_focus = rule
            .is_some_and(|r| r.priority || (r.priority_contacts && verified_priority_contact))
            && self.focus.allow_priority;
        let quiet = self.do_not_disturb || temporarily_muted || (focus && !pierces_focus);
        EffectiveNotificationPolicy {
            banners: !quiet && rule.and_then(|r| r.banners).unwrap_or(self.show_banners),
            sounds: !quiet && rule.and_then(|r| r.sounds).unwrap_or(self.play_sounds),
            badges: rule.and_then(|r| r.badges).unwrap_or(self.show_badges),
            previews: rule.and_then(|r| r.previews).unwrap_or(self.show_previews),
            lock_screen: rule
                .and_then(|r| r.lock_screen)
                .unwrap_or(self.show_on_lock_screen),
            cross_device: verified_app_id.is_some()
                && rule
                    .and_then(|rule| rule.cross_device)
                    .unwrap_or(self.cross_device),
            quiet,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_priority_and_mutes_follow_user_policy() {
        // Phase `focus_reports_when_its_period_ends`.
        {
            let f = FocusSchedule {
                enabled: true,
                ..FocusSchedule::default()
            };
            assert_eq!(f.active_until(23 * 60, 0), Some(7 * 60));
            assert_eq!(f.active_until(12 * 60, 0), None);
            let all_day = FocusSchedule {
                enabled: true,
                start_minute: 0,
                end_minute: 0,
                ..FocusSchedule::default()
            };
            assert_eq!(all_day.active_until(12 * 60, 0), None);
        }
        // Phase `overnight_focus_crosses_midnight`.
        {
            let f = FocusSchedule {
                enabled: true,
                ..FocusSchedule::default()
            };
            assert!(f.active_at(23 * 60, 0));
            assert!(f.active_at(6 * 60, 0));
            assert!(!f.active_at(12 * 60, 0));
        }
        // Phase `app_priority_is_user_policy_not_sender_metadata`.
        {
            let mut p = NotificationPolicy {
                focus: FocusSchedule {
                    enabled: true,
                    start_minute: 0,
                    end_minute: 0,
                    ..FocusSchedule::default()
                },
                ..NotificationPolicy::default()
            };
            p.per_app.insert(
                "so.tilo.chat".into(),
                AppNotificationRule {
                    priority: true,
                    ..AppNotificationRule::default()
                },
            );
            assert!(!p.effective_for(Some("so.tilo.chat"), 0, 500, 0).quiet);
            assert!(p.effective_for(Some("spoofed name"), 0, 500, 0).quiet);
        }
        // Phase `temporary_mute_expires_without_a_cleanup_write`.
        {
            let mut p = NotificationPolicy::default();
            p.per_app.insert(
                "so.tilo.mail".into(),
                AppNotificationRule {
                    muted_until: Some(200),
                    ..AppNotificationRule::default()
                },
            );
            assert!(p.effective_for(Some("so.tilo.mail"), 199, 0, 0).quiet);
            assert!(!p.effective_for(Some("so.tilo.mail"), 200, 0, 0).quiet);
        }
    }

    #[test]
    fn breakthroughs_require_verified_identity_and_authorized_apps() {
        // Phase `contact_only_pierces_for_a_verified_match_and_authorized_app`.
        {
            let mut p = NotificationPolicy {
                focus: FocusSchedule {
                    enabled: true,
                    start_minute: 0,
                    end_minute: 0,
                    ..FocusSchedule::default()
                },
                ..NotificationPolicy::default()
            };
            p.per_app.insert(
                "so.tilo.mail".into(),
                AppNotificationRule {
                    priority_contacts: true,
                    ..AppNotificationRule::default()
                },
            );
            assert!(
                !p.effective_for_context(Some("so.tilo.mail"), true, 0, 500, 0)
                    .quiet
            );
            assert!(
                p.effective_for_context(Some("so.tilo.mail"), false, 0, 500, 0)
                    .quiet
            );
            assert!(
                p.effective_for_context(Some("so.tilo.other"), true, 0, 500, 0)
                    .quiet
            );
        }
        // Phase `cross_device_requires_verified_identity_and_honours_per_app_policy`.
        {
            let mut policy = NotificationPolicy::default();
            assert!(
                policy
                    .effective_for(Some("so.tilo.mail"), 0, 0, 0)
                    .cross_device
            );
            assert!(!policy.effective_for(None, 0, 0, 0).cross_device);

            policy.per_app.insert(
                "so.tilo.mail".into(),
                AppNotificationRule {
                    cross_device: Some(false),
                    ..AppNotificationRule::default()
                },
            );
            assert!(
                !policy
                    .effective_for(Some("so.tilo.mail"), 0, 0, 0)
                    .cross_device
            );
        }
    }
}
