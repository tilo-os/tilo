//! [`Disclosure`]: one band's [`Disclose`] in and out, as a find bar or an
//! in-window banner keeps it.

use super::named::{CROSS_FADE, Disclose, Reveal, unit_opacity};
use super::presence::Presence;
use crate::tokens::{EffectiveMotion, MotionSource};

/// The coming and going of one band (a find bar, a banner) on the standard
/// expand/collapse motion ([`Disclose`]): its clip height opens from the
/// top and what follows it moves with the band's edge, by transform.
///
/// The owner keeps its own shown state, which input acts on at once; this
/// only says how much of the band is drawn ([`Self::reveal`]). A change a
/// key or an action asked for lands on the next frame; one a pointer asked
/// for discloses. Reduced motion: the height lands and an arriving band's
/// content fades in at `fast`.
///
/// Every motion it starts asks the frame pump for frames until it settles
/// ([`super::want_frames_until`]); at rest it asks for none and computes
/// nothing beyond its resting reveal.
#[derive(Clone, Copy, Debug, Default)]
pub struct Disclosure {
    presence: Presence,
    motion: EffectiveMotion,
    /// Reduced motion: the band stands at its full height and only its
    /// content fades in.
    fade_only: bool,
}

impl Disclosure {
    /// Not shown, at rest.
    #[must_use]
    pub fn closed() -> Self {
        Self::default()
    }

    /// Shown, at rest: a band that is there from its first paint (first
    /// paint shows the resting state; motion explains changes).
    #[must_use]
    pub fn opened() -> Self {
        Self {
            presence: Presence::opened(),
            ..Self::default()
        }
    }

    /// Show or hide it as the last input asked ([`super::input_source`]):
    /// Ctrl+F or Escape lands, a menu or a close button moves.
    pub fn show(&mut self, open: bool, motion: EffectiveMotion) {
        self.show_from(open, motion, super::source::input_source());
    }

    /// Show or hide it for a change the person may not have caused (a
    /// banner saying the window went offline): it discloses unless a key or
    /// an action just asked for it, which lands. A jump at the top of what
    /// the person looks at reads as the layout breaking, so an unprompted
    /// band moves too.
    pub fn show_change(&mut self, open: bool, motion: EffectiveMotion) {
        let source =
            super::source::recent_input(super::frame_now()).unwrap_or(MotionSource::Pointer);
        self.show_from(open, motion, source);
    }

    /// Let a band that is going for good (its view stopped building it)
    /// leave as the last input asks: a key's result lands; otherwise it
    /// collapses, and under reduced motion its height lands and its paint
    /// fades out at `fast`.
    pub fn leave(&mut self, motion: EffectiveMotion) {
        let source =
            super::source::recent_input(super::frame_now()).unwrap_or(MotionSource::Pointer);
        if motion.reduced() && !source.lands_at_once() && self.presence.is_open() {
            self.motion = motion;
            self.fade_only = true;
            self.presence
                .show(false, CROSS_FADE, motion, source, super::frame_now());
            if let Some(until) = self.presence.settles_at() {
                super::clock::want_frames_until(until);
            }
            return;
        }
        self.show_from(false, motion, source);
    }

    /// Show or hide it for a change `source` asked for. Asking for the
    /// state it is in or going to changes nothing; the other one mid-flight
    /// reverses from where it is drawn.
    pub fn show_from(&mut self, open: bool, motion: EffectiveMotion, source: MotionSource) {
        self.motion = motion;
        if open == self.presence.is_open() {
            return;
        }
        let now = super::frame_now();
        if motion.reduced() && !source.lands_at_once() {
            // The height lands; an arriving band's content fades in.
            self.presence.snap(false);
            self.fade_only = open;
            if open {
                self.presence.show(true, CROSS_FADE, motion, source, now);
            }
        } else {
            self.fade_only = false;
            self.presence
                .show(open, Disclose::SPEC, motion, source, now);
        }
        if let Some(until) = self.presence.settles_at() {
            super::clock::want_frames_until(until);
        }
    }

    /// Jump to `open`, at rest.
    pub fn snap(&mut self, open: bool) {
        self.fade_only = false;
        self.presence.snap(open);
    }

    /// The state it is going to.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.presence.is_open()
    }

    /// Whether anything of it is drawn now: open, opening or still leaving.
    #[must_use]
    pub fn is_drawn(&self) -> bool {
        self.presence.is_visible(super::frame_now())
    }

    /// Whether it is still moving now (its owner asks for frames).
    #[must_use]
    pub fn is_moving(&self) -> bool {
        self.presence.is_moving(super::frame_now())
    }

    /// Whether its height is on the way now (not reduced motion's fade):
    /// while it is, the band stands out of its container's layout, which
    /// keeps what follows where it is at the larger extent and moves it by
    /// transform; the layout takes the band in once, at rest.
    #[must_use]
    pub fn is_sliding(&self) -> bool {
        !self.fade_only && self.is_moving()
    }

    /// When it rests; `None` at rest.
    #[must_use]
    pub fn settles_at(&self) -> Option<crate::time::Instant> {
        self.presence.settles_at()
    }

    /// How much of a band `full` tall is drawn now: its clip height (what
    /// follows the band is drawn this far below the band's top) and its
    /// content's opacity.
    #[must_use]
    pub fn reveal(&self, full: f64) -> Reveal {
        let progress = self.presence.progress(super::frame_now());
        if self.fade_only {
            return Reveal {
                height: full,
                opacity: unit_opacity(progress),
                turn: 1.0,
            };
        }
        Disclose::reveal(progress, full)
    }
}
