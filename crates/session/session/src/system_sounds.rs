//! The one place the session plays a system event sound.
//! Every caller names a [`SystemSoundEvent`]; this reads the person's "System sounds" preference
//! and the quiet state out of the settings document, applies
//! [`settings_store::system_sounds::should_play`], and hands a cue to the bounded
//! [`EventSoundPlayer`] worker (a `pw-play` of the resolved theme file). Nothing here blocks the
//! loop.

use media::event_sound::EventSoundPlayer;
use settings_store::system_sounds::{QuietState, SystemSoundEvent, SystemSoundPrefs, should_play};

/// What plays the session's event sounds.
pub(crate) struct SystemSounds {
    player: EventSoundPlayer,
}

impl SystemSounds {
    pub(crate) fn new() -> Self {
        Self {
            player: EventSoundPlayer::new(),
        }
    }

    /// Play `event` if the person's preference and the quiet state allow it. `delivery_allows` is
    /// the caller's own verdict for this instance (a notification's sender policy has already
    /// applied Do Not Disturb and focus time; a `suppress-sound` hint makes it `false` ). Returns
    /// whether a cue was queued.
    pub(crate) fn play(
        &self,
        event: SystemSoundEvent,
        quiet: QuietState,
        delivery_allows: bool,
    ) -> bool {
        let prefs = SystemSoundPrefs::from_document(&settings_store::load());
        should_play(event, &prefs, quiet, delivery_allows)
            && self
                .player
                .play(tilo_audio_service::sounds::cue(event, &prefs))
    }
}

#[cfg(test)]
mod tests {
    use settings_store::system_sounds::{DEFAULT_THEME, SystemSoundEvent, SystemSoundPrefs};

    #[test]
    fn cue_carries_the_events_ids_description_theme_and_clamped_volume() {
        let defaults = SystemSoundPrefs::default();
        let cue = tilo_audio_service::sounds::cue(SystemSoundEvent::Notification, &defaults);
        assert_eq!(cue.ids, SystemSoundEvent::Notification.sound_ids());
        assert_eq!(
            cue.description,
            SystemSoundEvent::Notification.description()
        );
        assert_eq!(cue.theme, DEFAULT_THEME);
        assert_eq!(cue.volume.to_bits(), 0.7_f32.to_bits());

        let low = SystemSoundPrefs {
            theme: Some("ocean".into()),
            volume: -0.5,
            ..SystemSoundPrefs::default()
        };
        let cue = tilo_audio_service::sounds::cue(SystemSoundEvent::Screenshot, &low);
        assert_eq!(cue.ids, SystemSoundEvent::Screenshot.sound_ids());
        assert_eq!(cue.description, SystemSoundEvent::Screenshot.description());
        assert_eq!(cue.theme, "ocean");
        assert_eq!(cue.volume.to_bits(), 0.0_f32.to_bits());

        let high = SystemSoundPrefs {
            volume: 1.75,
            ..SystemSoundPrefs::default()
        };
        let cue = tilo_audio_service::sounds::cue(SystemSoundEvent::Screenshot, &high);
        assert_eq!(cue.volume.to_bits(), 1.0_f32.to_bits());
    }
}
