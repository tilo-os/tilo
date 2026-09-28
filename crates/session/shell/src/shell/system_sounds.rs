//! The one place the session plays a system event sound.
//!
//! Every caller names a [`SystemSoundEvent`]; this reads the person's
//! "System sounds" preference and the quiet state (Do Not Disturb, focus
//! time, presenting) from the shared settings document, applies
//! [`settings_store::system_sounds::should_play`], and hands a cue to the
//! bounded [`EventSoundPlayer`] worker (`pw-play` of the resolved theme
//! file). Nothing here blocks the render thread.

use chrono::{Datelike, Timelike};
use media::event_sound::{EventSoundPlayer, SoundCue};
use settings_store::notification_policy::NotificationPolicy;
use settings_store::system_sounds::{QuietState, SystemSoundEvent, SystemSoundPrefs, should_play};

#[derive(Clone, Debug)]
pub struct SystemSounds {
    player: EventSoundPlayer,
}

impl SystemSounds {
    pub fn new() -> Self {
        Self {
            player: EventSoundPlayer::new(),
        }
    }

    pub fn available(&self) -> bool {
        self.player.available()
    }

    /// Play `event` if the preference and the quiet state allow it.
    ///
    /// `presenting` is the compositor's automatic hold while presenting or
    /// sharing the screen. `delivery_allows` is the caller's verdict for
    /// this instance (see [`should_play`]). Returns whether a cue was queued.
    pub fn play(&self, event: SystemSoundEvent, presenting: bool, delivery_allows: bool) -> bool {
        let doc = settings_store::load();
        let prefs = SystemSoundPrefs::from_document(&doc);
        let quiet = quiet_state(&doc, presenting, chrono::Local::now());
        should_play(event, &prefs, quiet, delivery_allows) && self.player.play(cue(event, &prefs))
    }
}

impl Default for SystemSounds {
    fn default() -> Self {
        Self::new()
    }
}

fn quiet_state(
    doc: &serde_json::Map<String, serde_json::Value>,
    presenting: bool,
    now: chrono::DateTime<chrono::Local>,
) -> QuietState {
    let policy: NotificationPolicy = doc
        .get("notifications")
        .cloned()
        .and_then(|section| serde_json::from_value(section).ok())
        .unwrap_or_default();
    let minute =
        u16::try_from(now.hour().saturating_mul(60).saturating_add(now.minute())).unwrap_or(0);
    let weekday = u8::try_from(now.weekday().num_days_from_monday()).unwrap_or(0);
    QuietState {
        do_not_disturb: policy.do_not_disturb,
        focus: policy.focus.active_at(minute, weekday),
        presenting,
    }
}

pub fn cue(event: SystemSoundEvent, prefs: &SystemSoundPrefs) -> SoundCue {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the preference is clamped to 0..=1; f32 holds it exactly enough for a gain"
    )]
    let volume = prefs.volume.clamp(0.0, 1.0) as f32;
    SoundCue {
        ids: event.sound_ids(),
        description: event.description(),
        theme: prefs.theme_name().to_string(),
        volume,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    #[test]
    fn cues_carry_the_chosen_theme_and_volume() {
        let prefs = SystemSoundPrefs {
            theme: Some("ocean".into()),
            volume: 0.25,
            ..SystemSoundPrefs::default()
        };
        let cue = cue(SystemSoundEvent::Screenshot, &prefs);
        assert_eq!(cue.theme, "ocean");
        assert!((cue.volume - 0.25).abs() < f32::EPSILON);
        assert_eq!(cue.ids.first(), Some(&"screen-capture"));
        assert_eq!(
            super::cue(SystemSoundEvent::Error, &SystemSoundPrefs::default()).theme,
            "freedesktop"
        );
    }

    #[test]
    fn quiet_state_reads_do_not_disturb_and_the_focus_schedule() {
        let now = chrono::Local
            .with_ymd_and_hms(2026, 9, 28, 10, 30, 0)
            .single()
            .expect("an unambiguous local time");
        let mut doc = serde_json::Map::new();
        assert_eq!(quiet_state(&doc, false, now), QuietState::default());
        doc.insert(
            "notifications".into(),
            json!({
                "do_not_disturb": true,
                "focus": {"enabled": true, "start_minute": 540, "end_minute": 720}
            }),
        );
        let quiet = quiet_state(&doc, true, now);
        assert!(quiet.do_not_disturb);
        assert!(quiet.presenting);
        assert!(quiet.focus, "10:30 is inside the 09:00–12:00 focus period");
        let evening = chrono::Local
            .with_ymd_and_hms(2026, 9, 28, 19, 0, 0)
            .single()
            .expect("an unambiguous local time");
        assert!(!quiet_state(&doc, false, evening).focus);
    }
}
