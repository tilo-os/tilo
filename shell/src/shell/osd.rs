//! The transient on-screen display for volume, brightness and transport.
//!
//! A hardware key that changes something invisible needs an answer on
//! screen. Volume has no visible surface at all; brightness has one you
//! cannot read *because* it is what changed. Every desktop solves this
//! the same way — a slab that appears near the bottom of the screen,
//! shows the new level, and gets out of the way — and its two properties
//! are both about not being in the way: it never takes focus, and it
//! never survives the moment it was about.
//!
//! Deliberately not a notification. A notification is a message that
//! persists until acknowledged and stacks with its peers; an OSD is a
//! readout of a value that is only interesting while it is being changed.
//! Pressing volume-up eight times must show one slab counting up, not
//! eight banners.

use std::time::{Duration, Instant};

/// How long the slab stays up after the last press. Long enough to read
/// after a single tap, short enough that a burst of presses does not
/// leave it hanging around afterwards.
pub const OSD_HOLD: Duration = Duration::from_millis(1_400);

/// The fade-out tail, inside [`OSD_HOLD`]'s trailing edge.
pub const OSD_FADE: Duration = Duration::from_millis(280);

/// What the slab is reporting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OsdKind {
    /// Audio output level. `muted` swaps the glyph and dims the bar, but
    /// does not move it: a muted sink at 60% is still set to 60%, and
    /// emptying the bar made the slab claim a level of zero that the very
    /// next unmute contradicted. Silence is carried by the crossed-out
    /// glyph and the caption; the bar carries the level, which is true
    /// either way.
    Volume { muted: bool },
    /// Display backlight level.
    Brightness,
    /// Microphone capture mute. A toggle, not a level — the bar is
    /// omitted and the caption carries the state.
    MicMute { muted: bool },
    /// Media transport: the action taken, not a level.
    Transport(Transport),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    Play,
    Pause,
    Next,
    Previous,
}

impl Transport {
    pub fn caption(self) -> &'static str {
        match self {
            Transport::Play => "Play",
            Transport::Pause => "Pause",
            Transport::Next => "Next track",
            Transport::Previous => "Previous track",
        }
    }
}

/// One on-screen readout in flight.
#[derive(Clone, Copy, Debug)]
pub struct Osd {
    pub kind: OsdKind,
    /// `0.0..=1.0`. Ignored by the kinds that have no level.
    pub level: f32,
    /// When the value last changed. Re-raising an OSD of the same kind
    /// refreshes this rather than stacking a second slab.
    pub raised_at: Instant,
}

impl Osd {
    pub fn volume(level: f32, muted: bool) -> Self {
        Self {
            kind: OsdKind::Volume { muted },
            level: level.clamp(0.0, 1.0),
            raised_at: Instant::now(),
        }
    }

    pub fn brightness(level: f32) -> Self {
        Self {
            kind: OsdKind::Brightness,
            level: level.clamp(0.0, 1.0),
            raised_at: Instant::now(),
        }
    }

    pub fn mic_mute(muted: bool) -> Self {
        Self {
            kind: OsdKind::MicMute { muted },
            level: 0.0,
            raised_at: Instant::now(),
        }
    }

    pub fn transport(t: Transport) -> Self {
        Self {
            kind: OsdKind::Transport(t),
            level: 0.0,
            raised_at: Instant::now(),
        }
    }

    /// Whether this kind draws a level bar. Toggles and transport
    /// actions do not have a level, and a bar pinned at zero next to
    /// "Next track" would read as a broken meter.
    pub fn has_level(&self) -> bool {
        matches!(self.kind, OsdKind::Volume { .. } | OsdKind::Brightness)
    }

    /// The one-line caption under the bar.
    pub fn caption(&self) -> String {
        match self.kind {
            OsdKind::Volume { muted: true } => "Muted".to_string(),
            OsdKind::Volume { muted: false } => {
                format!("Volume {}%", (self.level * 100.0).round() as u32)
            }
            OsdKind::Brightness => format!("Brightness {}%", (self.level * 100.0).round() as u32),
            OsdKind::MicMute { muted: true } => "Microphone muted".to_string(),
            OsdKind::MicMute { muted: false } => "Microphone on".to_string(),
            OsdKind::Transport(t) => t.caption().to_string(),
        }
    }

    /// Opacity at `now`: fully opaque while held, easing to nothing over
    /// the trailing [`OSD_FADE`], and `0.0` once expired.
    pub fn alpha_at(&self, now: Instant) -> f32 {
        let elapsed = now.saturating_duration_since(self.raised_at);
        if elapsed >= OSD_HOLD {
            return 0.0;
        }
        let remaining = OSD_HOLD - elapsed;
        if remaining >= OSD_FADE {
            return 1.0;
        }
        let t = remaining.as_secs_f32() / OSD_FADE.as_secs_f32();
        // Ease-out so the last frames are the ones that thin out, rather
        // than a linear ramp that reads as a flicker at the end.
        t * t
    }

    /// Whether the slab should still be painted at `now`.
    pub fn is_live(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.raised_at) < OSD_HOLD
    }
}

/// Merge a newly-raised OSD into whatever is already on screen.
///
/// A second press of the same key restarts the timer on the existing
/// slab; a press of a *different* key replaces it outright. Both are the
/// same operation — the new reading wins — but stating it here keeps the
/// "never stack" property in one place rather than in each caller.
pub fn raise(current: &mut Option<Osd>, next: Osd) {
    *current = Some(next);
}

/// Drop an expired slab. Returns `true` when something was cleared, so
/// the caller knows to bump its repaint revision.
pub fn expire(current: &mut Option<Osd>, now: Instant) -> bool {
    if current.is_some_and(|o| !o.is_live(now)) {
        *current = None;
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_muted_volume_reads_as_muted_not_as_a_percentage() {
        let o = Osd::volume(0.6, true);
        assert_eq!(o.caption(), "Muted");
        assert!(
            o.has_level(),
            "the bar is still drawn, so the slab does not change shape on mute"
        );
    }

    /// Muting does not move the level, so the bar has something true to
    /// draw. It used to be painted empty while `level` stayed at the real
    /// value, which is how the slab could read 0% and then jump straight
    /// back to 65% the moment the sink was unmuted.
    #[test]
    fn muting_does_not_discard_the_level_it_will_return_to() {
        let muted = Osd::volume(0.65, true);
        assert_eq!(muted.level, 0.65);
        assert_eq!(
            muted.level,
            Osd::volume(0.65, false).level,
            "the level a muted sink reports is the level it is set to",
        );
    }

    #[test]
    fn levels_are_reported_as_whole_percent() {
        assert_eq!(Osd::volume(0.375, false).caption(), "Volume 38%");
        assert_eq!(Osd::brightness(0.0).caption(), "Brightness 0%");
        assert_eq!(Osd::brightness(1.0).caption(), "Brightness 100%");
    }

    #[test]
    fn transport_and_mic_have_no_level_bar() {
        assert!(!Osd::transport(Transport::Next).has_level());
        assert!(!Osd::mic_mute(true).has_level());
        assert_eq!(Osd::transport(Transport::Next).caption(), "Next track");
        assert_eq!(Osd::mic_mute(true).caption(), "Microphone muted");
    }

    #[test]
    fn a_level_out_of_range_is_clamped_rather_than_drawn_past_the_bar() {
        assert_eq!(Osd::volume(1.7, false).level, 1.0);
        assert_eq!(Osd::brightness(-0.2).level, 0.0);
    }

    #[test]
    fn a_fresh_slab_is_opaque_and_an_old_one_is_gone() {
        let o = Osd::volume(0.5, false);
        let t0 = o.raised_at;
        assert_eq!(o.alpha_at(t0), 1.0);
        assert!(o.is_live(t0));
        let past = t0 + OSD_HOLD;
        assert_eq!(o.alpha_at(past), 0.0);
        assert!(!o.is_live(past));
    }

    #[test]
    fn the_slab_thins_out_over_the_tail_rather_than_vanishing() {
        let o = Osd::brightness(0.5);
        let t0 = o.raised_at;
        // Just before the fade begins: still fully opaque.
        let before_fade = t0 + OSD_HOLD - OSD_FADE - Duration::from_millis(1);
        assert_eq!(o.alpha_at(before_fade), 1.0);
        // Halfway through the fade: partly transparent, not yet gone.
        let mid = t0 + OSD_HOLD - OSD_FADE / 2;
        let a = o.alpha_at(mid);
        assert!(a > 0.0 && a < 1.0, "expected a partial alpha, got {a}");
    }

    #[test]
    fn a_second_press_restarts_the_slab_instead_of_stacking_one() {
        let mut cur = None;
        raise(&mut cur, Osd::volume(0.3, false));
        let first_raise = cur.unwrap().raised_at;
        std::thread::sleep(Duration::from_millis(2));
        raise(&mut cur, Osd::volume(0.4, false));
        let second = cur.unwrap();
        assert!(
            second.raised_at > first_raise,
            "the timer restarts on every press, so a burst shows one slab"
        );
        assert_eq!(second.level, 0.4, "and it shows the newest value");
    }

    #[test]
    fn a_different_key_replaces_rather_than_queues() {
        let mut cur = None;
        raise(&mut cur, Osd::volume(0.3, false));
        raise(&mut cur, Osd::brightness(0.9));
        assert_eq!(cur.unwrap().kind, OsdKind::Brightness);
    }

    #[test]
    fn expiry_reports_whether_it_changed_anything() {
        let mut cur = Some(Osd::volume(0.3, false));
        let t0 = cur.unwrap().raised_at;
        assert!(!expire(&mut cur, t0), "a live slab is not expired");
        assert!(expire(&mut cur, t0 + OSD_HOLD), "an old slab is dropped");
        assert!(cur.is_none());
        assert!(
            !expire(&mut cur, t0 + OSD_HOLD),
            "expiring nothing must not claim a repaint"
        );
    }
}
