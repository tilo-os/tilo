//! Which system events make a sound, how loud, from which theme — and the one
//! rule that decides whether a given event plays right now.
//!
//! Settings → Sound owns the preference; the compositor reads it through
//! [`SystemSoundPrefs::from_document`] and asks [`should_play`] before it
//! queues a cue. Keeping the table and the rule here gives the two binaries a
//! single definition to agree on.
//!
//! ```json
//! "audio": {
//!   "system_sounds": {
//!     "enabled": true, "volume": 0.7, "theme": "freedesktop",
//!     "events": { "notification": true, "message": true, "volume_change": true,
//!                 "screenshot": true, "device_connected": true,
//!                 "device_disconnected": true, "low_battery": true, "error": true }
//!   }
//! }
//! ```

use serde::{Deserialize, Serialize};

/// Settings section and leaf that hold [`SystemSoundPrefs`].
pub const SECTION: &str = "audio";
pub const LEAF: &str = "system_sounds";

/// The sound theme used when the person has not chosen one: the
/// freedesktop.org reference theme every other theme inherits from.
pub const DEFAULT_THEME: &str = "freedesktop";

/// A system event that can make a sound.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SystemSoundEvent {
    Notification,
    Message,
    VolumeChange,
    Screenshot,
    DeviceConnected,
    DeviceDisconnected,
    LowBattery,
    Error,
}

/// How an event relates to Do Not Disturb and focus time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoundKind {
    /// Confirms something the person just did (a volume key, a screenshot).
    /// Quiet modes hold back interruptions, not answers to your own action.
    Feedback,
    /// Something happened that the person did not ask for. Silenced by Do
    /// Not Disturb, focus time and while presenting or sharing the screen.
    Interruption,
    /// The machine needs attention soon or it stops working. Plays through
    /// Do Not Disturb and focus time; only its own toggle silences it.
    Critical,
}

impl SystemSoundEvent {
    pub const ALL: [Self; 8] = [
        Self::Notification,
        Self::Message,
        Self::VolumeChange,
        Self::Screenshot,
        Self::DeviceConnected,
        Self::DeviceDisconnected,
        Self::LowBattery,
        Self::Error,
    ];

    /// Stable key in `system_sounds.events`.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Notification => "notification",
            Self::Message => "message",
            Self::VolumeChange => "volume_change",
            Self::Screenshot => "screenshot",
            Self::DeviceConnected => "device_connected",
            Self::DeviceDisconnected => "device_disconnected",
            Self::LowBattery => "low_battery",
            Self::Error => "error",
        }
    }

    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|event| event.key() == key)
    }

    /// Row label in Settings → Sound.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Notification => "Notifications",
            Self::Message => "New messages",
            Self::VolumeChange => "Volume change feedback",
            Self::Screenshot => "Screenshot shutter",
            Self::DeviceConnected => "Device connected",
            Self::DeviceDisconnected => "Device disconnected",
            Self::LowBattery => "Low battery",
            Self::Error => "Errors",
        }
    }

    /// freedesktop.org sound-naming-spec ids, most specific first. A theme
    /// that lacks the first falls back to the next.
    #[must_use]
    pub const fn sound_ids(self) -> &'static [&'static str] {
        match self {
            Self::Notification => &["message", "dialog-information"],
            Self::Message => &["message-new-instant", "message"],
            Self::VolumeChange => &["audio-volume-change"],
            Self::Screenshot => &["screen-capture", "camera-shutter"],
            Self::DeviceConnected => &["device-added"],
            Self::DeviceDisconnected => &["device-removed"],
            Self::LowBattery => &["battery-low", "power-unplug", "dialog-warning"],
            Self::Error => &["dialog-error"],
        }
    }

    /// Accessible description passed along with the cue.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Notification => "Notification",
            Self::Message => "New message",
            Self::VolumeChange => "Volume changed",
            Self::Screenshot => "Screenshot taken",
            Self::DeviceConnected => "Device connected",
            Self::DeviceDisconnected => "Device disconnected",
            Self::LowBattery => "Battery low",
            Self::Error => "Error",
        }
    }

    #[must_use]
    pub const fn kind(self) -> SoundKind {
        match self {
            Self::VolumeChange | Self::Screenshot => SoundKind::Feedback,
            Self::LowBattery => SoundKind::Critical,
            Self::Notification
            | Self::Message
            | Self::DeviceConnected
            | Self::DeviceDisconnected
            | Self::Error => SoundKind::Interruption,
        }
    }

    /// The event a desktop notification stands for, from its freedesktop
    /// `category` hint (or the portal's `category`).
    #[must_use]
    pub fn for_notification_category(category: Option<&str>) -> Self {
        match category.unwrap_or_default() {
            "im" | "im.received" | "email.arrived" => Self::Message,
            "device.added" => Self::DeviceConnected,
            "device.removed" => Self::DeviceDisconnected,
            // Never `LowBattery`: that event plays through Do Not Disturb, so
            // only the session's own power warning may claim it.
            _ => Self::Notification,
        }
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "one independent, serialised on/off choice per event"
)]
#[cfg_attr(
    feature = "model",
    derive(tilo_resource::Model, tilo_resource::JsonSchema)
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self", default)]
pub struct SystemSoundEvents {
    pub notification: bool,
    pub message: bool,
    pub volume_change: bool,
    pub screenshot: bool,
    pub device_connected: bool,
    pub device_disconnected: bool,
    pub low_battery: bool,
    pub error: bool,
}

crate::read_through_json_value!(SystemSoundEvents);

impl Default for SystemSoundEvents {
    fn default() -> Self {
        Self {
            notification: true,
            message: true,
            volume_change: true,
            screenshot: true,
            device_connected: true,
            device_disconnected: true,
            low_battery: true,
            error: true,
        }
    }
}

impl SystemSoundEvents {
    #[must_use]
    pub const fn get(&self, event: SystemSoundEvent) -> bool {
        match event {
            SystemSoundEvent::Notification => self.notification,
            SystemSoundEvent::Message => self.message,
            SystemSoundEvent::VolumeChange => self.volume_change,
            SystemSoundEvent::Screenshot => self.screenshot,
            SystemSoundEvent::DeviceConnected => self.device_connected,
            SystemSoundEvent::DeviceDisconnected => self.device_disconnected,
            SystemSoundEvent::LowBattery => self.low_battery,
            SystemSoundEvent::Error => self.error,
        }
    }

    pub fn set(&mut self, event: SystemSoundEvent, on: bool) {
        let slot = match event {
            SystemSoundEvent::Notification => &mut self.notification,
            SystemSoundEvent::Message => &mut self.message,
            SystemSoundEvent::VolumeChange => &mut self.volume_change,
            SystemSoundEvent::Screenshot => &mut self.screenshot,
            SystemSoundEvent::DeviceConnected => &mut self.device_connected,
            SystemSoundEvent::DeviceDisconnected => &mut self.device_disconnected,
            SystemSoundEvent::LowBattery => &mut self.low_battery,
            SystemSoundEvent::Error => &mut self.error,
        };
        *slot = on;
    }
}

/// The "System sounds" preference.
#[cfg_attr(
    feature = "model",
    derive(tilo_resource::Model, tilo_resource::JsonSchema)
)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self", default)]
pub struct SystemSoundPrefs {
    /// Master switch.
    pub enabled: bool,
    /// Linear `0..=1`, relative to the output volume.
    pub volume: f64,
    /// freedesktop sound theme name; `None` follows [`DEFAULT_THEME`].
    pub theme: Option<String>,
    pub events: SystemSoundEvents,
}

crate::read_through_json_value!(SystemSoundPrefs);

impl Default for SystemSoundPrefs {
    fn default() -> Self {
        Self {
            enabled: true,
            volume: DEFAULT_VOLUME,
            theme: None,
            events: SystemSoundEvents::default(),
        }
    }
}

pub const DEFAULT_VOLUME: f64 = 0.7;

impl SystemSoundPrefs {
    /// Read from a parsed settings document; anything missing or malformed
    /// falls back to the defaults.
    #[must_use]
    pub fn from_document(doc: &serde_json::Map<String, serde_json::Value>) -> Self {
        doc.get(SECTION)
            .and_then(|section| section.get(LEAF))
            .and_then(|value| serde_json::from_value::<Self>(value.clone()).ok())
            .map(Self::sanitized)
            .unwrap_or_default()
    }

    #[must_use]
    pub fn sanitized(mut self) -> Self {
        self.volume = if self.volume.is_finite() {
            self.volume.clamp(0.0, 1.0)
        } else {
            DEFAULT_VOLUME
        };
        self.theme = self
            .theme
            .filter(|theme| is_valid_theme_name(theme))
            .filter(|theme| theme != DEFAULT_THEME);
        self
    }

    /// The theme to look sounds up in.
    #[must_use]
    pub fn theme_name(&self) -> &str {
        self.theme.as_deref().unwrap_or(DEFAULT_THEME)
    }
}

/// A theme name is one path component: no separators, no `.`/`..`, nothing
/// that could make a lookup leave the sound-theme directories.
#[must_use]
pub fn is_valid_theme_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', '\0'])
        && !name.chars().any(char::is_control)
}

/// What is holding interruptions back right now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QuietState {
    pub do_not_disturb: bool,
    /// A focus schedule is active.
    pub focus: bool,
    /// Presenting or sharing the screen.
    pub presenting: bool,
}

impl QuietState {
    #[must_use]
    pub const fn any(self) -> bool {
        self.do_not_disturb || self.focus || self.presenting
    }
}

/// Whether `event` should make a sound now.
///
/// `delivery_allows` is the caller's own verdict for this instance: for a
/// notification it is the notification policy's per-app sound decision
/// (which already applies Do Not Disturb, focus time and the sender's
/// priority), and a `suppress-sound` hint makes it `false`. Other callers
/// pass `true`.
#[must_use]
pub fn should_play(
    event: SystemSoundEvent,
    prefs: &SystemSoundPrefs,
    quiet: QuietState,
    delivery_allows: bool,
) -> bool {
    if !prefs.enabled || prefs.volume <= 0.0 || !prefs.events.get(event) || !delivery_allows {
        return false;
    }
    match event.kind() {
        SoundKind::Feedback | SoundKind::Critical => true,
        // Notifications and messages arrive with the notification policy's
        // verdict, which lets a priority sender through focus time; only the
        // automatic hold while presenting is added here.
        SoundKind::Interruption
            if matches!(
                event,
                SystemSoundEvent::Notification | SystemSoundEvent::Message
            ) =>
        {
            !quiet.presenting
        }
        SoundKind::Interruption => !quiet.any(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn prefs() -> SystemSoundPrefs {
        SystemSoundPrefs::default()
    }

    #[test]
    fn keys_round_trip_and_every_event_has_a_sound_id() {
        for event in SystemSoundEvent::ALL {
            assert_eq!(SystemSoundEvent::from_key(event.key()), Some(event));
            assert!(!event.sound_ids().is_empty());
        }
        assert_eq!(SystemSoundEvent::from_key("startup"), None);
    }

    /// Every event × Do Not Disturb × its own toggle.
    #[test]
    fn policy_table() {
        use SystemSoundEvent as E;
        // Feedback and critical events play through Do Not Disturb.
        // Notifications and messages carry their own verdict, which is
        // already "no" under DND; other interruptions are held back here.
        let expected_under_dnd =
            |event: E| matches!(event, E::VolumeChange | E::Screenshot | E::LowBattery);
        for event in E::ALL {
            for dnd in [false, true] {
                for toggle in [false, true] {
                    let mut prefs = prefs();
                    prefs.events.set(event, toggle);
                    let quiet = QuietState {
                        do_not_disturb: dnd,
                        ..QuietState::default()
                    };
                    let notification_verdict =
                        !(dnd && matches!(event, E::Notification | E::Message));
                    let expected = toggle && (!dnd || expected_under_dnd(event));
                    assert_eq!(
                        should_play(event, &prefs, quiet, notification_verdict),
                        expected,
                        "{event:?} dnd={dnd} toggle={toggle}"
                    );
                }
            }
        }
    }

    #[test]
    fn focus_time_and_presenting_hold_interruptions_only() {
        let focus = QuietState {
            focus: true,
            ..QuietState::default()
        };
        let presenting = QuietState {
            presenting: true,
            ..QuietState::default()
        };
        let p = prefs();
        assert!(!should_play(
            SystemSoundEvent::DeviceConnected,
            &p,
            focus,
            true
        ));
        assert!(!should_play(SystemSoundEvent::Error, &p, presenting, true));
        // A priority sender pierced focus time in the notification policy.
        assert!(should_play(SystemSoundEvent::Message, &p, focus, true));
        assert!(!should_play(
            SystemSoundEvent::Message,
            &p,
            presenting,
            true
        ));
        assert!(should_play(
            SystemSoundEvent::VolumeChange,
            &p,
            presenting,
            true
        ));
        assert!(should_play(SystemSoundEvent::LowBattery, &p, focus, true));
    }

    #[test]
    fn the_master_switch_volume_and_suppression_silence_everything() {
        let mut off = prefs();
        off.enabled = false;
        let mut silent = prefs();
        silent.volume = 0.0;
        for event in SystemSoundEvent::ALL {
            assert!(!should_play(event, &off, QuietState::default(), true));
            assert!(!should_play(event, &silent, QuietState::default(), true));
            assert!(!should_play(event, &prefs(), QuietState::default(), false));
            assert!(should_play(event, &prefs(), QuietState::default(), true));
        }
    }

    #[test]
    fn notification_categories_pick_the_event() {
        use SystemSoundEvent as E;
        assert_eq!(E::for_notification_category(None), E::Notification);
        assert_eq!(
            E::for_notification_category(Some("im.received")),
            E::Message
        );
        assert_eq!(
            E::for_notification_category(Some("device.added")),
            E::DeviceConnected
        );
        assert_eq!(
            E::for_notification_category(Some("device.removed")),
            E::DeviceDisconnected
        );
        assert_eq!(
            E::for_notification_category(Some("battery.low")),
            E::Notification
        );
        assert_eq!(
            E::for_notification_category(Some("transfer.complete")),
            E::Notification
        );
    }

    #[test]
    fn preferences_round_trip_and_malformed_values_fall_back() {
        let mut chosen = prefs();
        chosen.volume = 0.25;
        chosen.theme = Some("ocean".into());
        chosen.events.set(SystemSoundEvent::VolumeChange, false);
        let mut doc = serde_json::Map::new();
        doc.insert(
            SECTION.into(),
            json!({ LEAF: serde_json::to_value(&chosen).unwrap() }),
        );
        assert_eq!(SystemSoundPrefs::from_document(&doc), chosen);

        doc.insert(
            SECTION.into(),
            json!({ LEAF: { "volume": 7.0, "theme": "../../etc", "events": { "error": false } } }),
        );
        let parsed = SystemSoundPrefs::from_document(&doc);
        assert!((parsed.volume - 1.0).abs() < f64::EPSILON);
        assert_eq!(parsed.theme_name(), DEFAULT_THEME);
        assert!(!parsed.events.error);
        assert!(parsed.events.message, "unspecified events stay on");

        doc.insert(SECTION.into(), json!({ LEAF: "loud" }));
        assert_eq!(SystemSoundPrefs::from_document(&doc), prefs());
        assert_eq!(
            SystemSoundPrefs::from_document(&serde_json::Map::new()),
            prefs()
        );
    }
}
