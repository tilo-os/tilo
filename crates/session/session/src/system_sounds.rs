//! The one place the session plays a system event sound.
//!
//! Every caller names a [`SystemSoundEvent`]; this reads the person's
//! "System sounds" preference and the quiet state out of the settings
//! document, applies [`settings_store::system_sounds::should_play`], and
//! hands a cue to the bounded [`EventSoundPlayer`] worker (a `pw-play` of
//! the resolved theme file). Nothing here blocks the loop.

use media::event_sound::{EventSoundPlayer, SoundCue};
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

    /// Play `event` if the person's preference and the quiet state allow
    /// it. `delivery_allows` is the caller's own verdict for this instance
    /// (a notification's sender policy has already applied Do Not Disturb
    /// and focus time; a `suppress-sound` hint makes it `false`). Returns
    /// whether a cue was queued.
    pub(crate) fn play(
        &self,
        event: SystemSoundEvent,
        quiet: QuietState,
        delivery_allows: bool,
    ) -> bool {
        let prefs = SystemSoundPrefs::from_document(&settings_store::load());
        should_play(event, &prefs, quiet, delivery_allows) && self.player.play(cue(event, &prefs))
    }
}

/// The cue for `event` as the person's preferences have it.
pub(crate) fn cue(event: SystemSoundEvent, prefs: &SystemSoundPrefs) -> SoundCue {
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
