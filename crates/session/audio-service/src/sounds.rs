//! The system sounds the audio service plays: an output arriving, an
//! output leaving.
//!
//! The person's Sound settings decide which events may make a sound, how
//! loud and from which theme; Do Not Disturb and focus time hold back the
//! ones nobody asked for. [`should_play`] is that rule, shared with the rest
//! of the desktop, and the cue is played on the bounded worker in
//! [`media::event_sound`], which never blocks the service.
//!
//! Nothing is faked: a machine without a sound theme or `pw-play` reports no
//! backend and stays silent.

use media::event_sound::{EventSoundPlayer, SoundCue};
use settings_store::notification_policy::NotificationPolicy;
use settings_store::system_sounds::{QuietState, SystemSoundEvent, SystemSoundPrefs, should_play};

/// Plays the system sounds this machine can make.
#[derive(Clone)]
pub struct SystemSounds {
    player: EventSoundPlayer,
}

impl SystemSounds {
    #[must_use]
    pub fn new() -> Self {
        Self {
            player: EventSoundPlayer::new(),
        }
    }

    /// Play what `event` sounds: an output arriving or leaving.
    pub fn device(&self, event: crate::DeviceEvent) -> bool {
        self.play(match event {
            crate::DeviceEvent::Connected => SystemSoundEvent::DeviceConnected,
            crate::DeviceEvent::Disconnected => SystemSoundEvent::DeviceDisconnected,
        })
    }

    /// Play `event` if the person's settings allow it now. Returns whether a
    /// cue was queued.
    pub fn play(&self, event: SystemSoundEvent) -> bool {
        let document = settings_store::load();
        let prefs = SystemSoundPrefs::from_document(&document);
        let quiet = quiet_state(&document, chrono::Local::now());
        plays(event, &prefs, quiet) && self.player.play(cue(event, &prefs))
    }
}

impl Default for SystemSounds {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether `event` may sound now, given the settings and what holds
/// interruptions back. Nothing delivers these but this service, so delivery
/// never holds one back; a device sound is an interruption, not an answer to
/// something the person just did.
#[must_use]
pub fn plays(event: SystemSoundEvent, prefs: &SystemSoundPrefs, quiet: QuietState) -> bool {
    should_play(event, prefs, quiet, true)
}

/// The sound `event` makes, as the person's settings name it.
#[must_use]
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

/// What holds interruptions back right now, from the person's notification
/// policy: Do Not Disturb, and focus time at `now`.
fn quiet_state(
    document: &serde_json::Map<String, serde_json::Value>,
    now: chrono::DateTime<chrono::Local>,
) -> QuietState {
    use chrono::{Datelike as _, Timelike as _};
    let policy = NotificationPolicy::from_document(document);
    let minute =
        u16::try_from(now.hour().saturating_mul(60).saturating_add(now.minute())).unwrap_or(0);
    let weekday = u8::try_from(now.weekday().num_days_from_monday()).unwrap_or(0);
    QuietState {
        do_not_disturb: policy.do_not_disturb,
        focus: policy.focus.active_at(minute, weekday),
        presenting: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefs() -> SystemSoundPrefs {
        SystemSoundPrefs::default()
    }

    #[test]
    fn a_device_arriving_or_leaving_sounds_in_the_chosen_theme_and_volume() {
        let mut chosen = prefs();
        chosen.theme = Some("ocean".into());
        chosen.volume = 0.25;
        let connected = cue(SystemSoundEvent::DeviceConnected, &chosen);
        assert_eq!(connected.theme, "ocean");
        assert!((connected.volume - 0.25).abs() < f32::EPSILON);
        assert_eq!(connected.ids, SystemSoundEvent::DeviceConnected.sound_ids());
        assert_eq!(
            cue(SystemSoundEvent::DeviceDisconnected, &prefs()).theme,
            settings_store::system_sounds::DEFAULT_THEME
        );
    }

    #[test]
    fn do_not_disturb_and_focus_hold_a_device_sound_back() {
        let event = SystemSoundEvent::DeviceConnected;
        assert!(plays(event, &prefs(), QuietState::default()));
        for quiet in [
            QuietState {
                do_not_disturb: true,
                ..QuietState::default()
            },
            QuietState {
                focus: true,
                ..QuietState::default()
            },
            QuietState {
                presenting: true,
                ..QuietState::default()
            },
        ] {
            assert!(!plays(event, &prefs(), quiet), "{quiet:?}");
        }
    }

    #[test]
    fn the_persons_switches_and_the_volume_decide() {
        let quiet = QuietState::default();
        let mut off = prefs();
        off.enabled = false;
        assert!(!plays(SystemSoundEvent::DeviceConnected, &off, quiet));

        let mut silent = prefs();
        silent.volume = 0.0;
        assert!(!plays(SystemSoundEvent::DeviceConnected, &silent, quiet));

        let mut events = prefs();
        events
            .events
            .set(SystemSoundEvent::DeviceDisconnected, false);
        assert!(plays(SystemSoundEvent::DeviceConnected, &events, quiet));
        assert!(!plays(SystemSoundEvent::DeviceDisconnected, &events, quiet));
    }

    #[test]
    fn focus_time_is_read_from_the_persons_schedule() {
        use chrono::TimeZone as _;
        let now = chrono::Local
            .with_ymd_and_hms(2026, 11, 2, 10, 30, 0)
            .single()
            .expect("an unambiguous local time");
        let mut document = serde_json::Map::new();
        assert_eq!(quiet_state(&document, now), QuietState::default());
        document.insert(
            "notifications".into(),
            serde_json::json!({
                "do_not_disturb": true,
                "focus": {"enabled": true, "start_minute": 540, "end_minute": 720}
            }),
        );
        let quiet = quiet_state(&document, now);
        assert!(quiet.do_not_disturb);
        assert!(quiet.focus, "10:30 is inside the 09:00-12:00 focus period");
        assert!(
            !quiet.presenting,
            "the service is not a screen and says nothing about presenting"
        );
    }
}
