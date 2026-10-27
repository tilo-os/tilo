//! What plays, painted the one way every player surface paints it: Music's
//! transport and its Now Playing page, the shell's Quick Settings card, the
//! lock screen and the desk.
//!
//! * [`SongFace`]: the song's cover and lines. When the song changes (the
//!   next one, a cover arriving over its placeholder) the old cover and
//!   lines fade out where they stand while the new ones fade in
//!   ([`CrossFade`]); a change a key or an action made lands at once.
//! * [`Scrub`]: a seek bar held by the pointer or a finger in a surface that
//!   paints its own controls. The knob follows the hand 1:1, the time under
//!   it shows on a chip above it ([`paint_held_readout`]), and letting go
//!   seeks once. The retained [`Slider`](crate::widget::Slider) does the same
//!   with [`Slider::reads_out_while_held`](crate::widget::Slider::reads_out_while_held).

use vello::kurbo::{Point, Rect};
use vello::peniko::{BlendMode, Fill};

use crate::motion::{CrossFade, recent_input, want_frames_until};
use crate::paint::PaintCtx;
use crate::text_render::TextStyle;
use crate::time::Instant;
use crate::tokens::{EffectiveMotion, MotionSource};
use crate::widget::media_card::{MediaArt, paint_art_tile};
use crate::widget::{SliderFace, TextStackFace, Tone, TransportArt};

pub use crate::widget::slider::paint_held_readout;

/// The song a player surface shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Song {
    pub title: String,
    /// Who it is by, or why it can't play ([`Self::error`]).
    pub subtitle: String,
    /// A third, quieter line: the app that plays it, where several can.
    pub detail: String,
    /// The subtitle says why the song can't play.
    pub error: bool,
    pub art: TransportArt,
    /// The name the placeholder cover takes its initials and colour from.
    pub art_name: String,
}

impl Song {
    fn is_nothing(&self) -> bool {
        *self == Self::default()
    }
}

/// A song on screen, cross-fading to the next: see the [module docs](self).
#[derive(Clone, Debug)]
pub struct SongFace {
    fade: CrossFade<Song>,
}

impl Default for SongFace {
    fn default() -> Self {
        Self::new()
    }
}

impl SongFace {
    /// Showing nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self {
            fade: CrossFade::new(Song::default()),
        }
    }

    /// Showing `song` at rest: for a surface that paints from scratch each
    /// time and keeps no motion of its own (the lock screen, the desk).
    #[must_use]
    pub fn showing(song: &Song) -> Self {
        Self {
            fade: CrossFade::new(song.clone()),
        }
    }

    /// Show `song` from the frame at `now`; whether it changed what is
    /// shown. The first song a surface shows is there at once; a later
    /// change cross-fades, unless a key or an action made it.
    pub fn show(&mut self, song: &Song, motion: EffectiveMotion, now: Instant) -> bool {
        if self.fade.shown() == song {
            return false;
        }
        if self.fade.shown().is_nothing() {
            self.fade = CrossFade::new(song.clone());
            return true;
        }
        // A song changing in place is the content swapping where it stands,
        // whoever changed it; only a key or an action asks for it at once.
        let source = recent_input(now)
            .filter(|source| source.lands_at_once())
            .unwrap_or(MotionSource::Pointer);
        self.fade.show(song.clone(), motion, source, now);
        if let Some(until) = self.fade.settles_at() {
            want_frames_until(until);
        }
        true
    }

    /// The song shown (the newest one, while the old still fades).
    #[must_use]
    pub fn song(&self) -> &Song {
        self.fade.shown()
    }

    #[must_use]
    pub fn is_moving(&self, now: Instant) -> bool {
        self.fade.is_moving(now)
    }

    #[must_use]
    pub fn settles_at(&self) -> Option<Instant> {
        self.fade.settles_at()
    }

    /// The outgoing song and the incoming one, each with its opacity.
    fn layers(&self, now: Instant) -> impl Iterator<Item = (&Song, f32)> {
        self.fade.outgoing(now).into_iter().chain(std::iter::once((
            self.fade.shown(),
            self.fade.incoming(now),
        )))
    }

    /// Paint the cover in `art` with corners of `radius`: the picture
    /// cover-fitted, or the placeholder seeded with the song's art name.
    pub fn paint_art(&self, ctx: &mut PaintCtx<'_>, art: Rect, radius: f64, now: Instant) {
        for (song, opacity) in self.layers(now) {
            faded(ctx, art, opacity, |ctx| paint_cover(ctx, art, song, radius));
        }
    }

    /// Paint the lines in `rect`, centred vertically and elided to its
    /// width, in the theme's song type or in `styles` (title, caption) for a
    /// surface that scales its type itself.
    pub fn paint_lines(
        &self,
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        styles: Option<(&TextStyle, &TextStyle)>,
        now: Instant,
    ) {
        for (song, opacity) in self.layers(now) {
            faded(ctx, rect, opacity, |ctx| {
                let mut face = TextStackFace::new(&song.title)
                    .secondary(&song.subtitle)
                    .tertiary(&song.detail)
                    .secondary_tone(if song.error {
                        Tone::Danger
                    } else {
                        Tone::Neutral
                    });
                if let Some((title, caption)) = styles {
                    face = face.styled(title, caption);
                }
                face.paint(ctx, rect);
            });
        }
    }

    /// What a screen reader says for the song shown.
    #[must_use]
    pub fn spoken(&self) -> String {
        let song = self.song();
        TextStackFace::new(&song.title)
            .secondary(&song.subtitle)
            .tertiary(&song.detail)
            .spoken()
    }
}

fn paint_cover(ctx: &mut PaintCtx<'_>, art: Rect, song: &Song, radius: f64) {
    let kind = match &song.art {
        TransportArt::None => return,
        TransportArt::Placeholder => MediaArt::Placeholder,
        TransportArt::Image(image) => MediaArt::Image(image),
    };
    paint_art_tile(ctx, art, kind, &song.art_name, radius);
}

/// Paint at `opacity` what `paint` draws inside `bounds`.
fn faded(
    ctx: &mut PaintCtx<'_>,
    bounds: Rect,
    opacity: f32,
    paint: impl FnOnce(&mut PaintCtx<'_>),
) {
    if opacity <= 0.0 {
        return;
    }
    if opacity >= 1.0 {
        paint(ctx);
        return;
    }
    // The layer clips: a cover's anti-aliased edge and a line's descenders
    // stay inside it.
    let bounds = bounds.inflate(2.0, 2.0);
    ctx.scene.push_layer(
        Fill::NonZero,
        BlendMode::default(),
        opacity,
        ctx.transform,
        &bounds,
    );
    paint(ctx);
    ctx.scene.pop_layer();
}

/// A seek bar held by the pointer or a finger, for a surface that paints
/// its own controls and routes its own input (the shell's cards). The host
/// hands in the face it paints, so the press, the drag and the paint read
/// one geometry.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Scrub {
    held: Option<f64>,
}

impl Scrub {
    /// A press on the bar at `point`: the knob jumps there and is held.
    pub fn press(&mut self, face: &SliderFace, point: Point) -> f64 {
        let fraction = face.value_at(point);
        self.held = Some(fraction);
        fraction
    }

    /// The held knob follows `point`; `None` while nothing is held.
    pub fn drag(&mut self, face: &SliderFace, point: Point) -> Option<f64> {
        let held = self.held.as_mut()?;
        *held = face.value_at(point);
        Some(*held)
    }

    /// Let go: the fraction to seek to, once.
    pub fn release(&mut self) -> Option<f64> {
        self.held.take()
    }

    /// Drop the hold without seeking (the surface closed under it).
    pub fn cancel(&mut self) {
        self.held = None;
    }

    #[must_use]
    pub fn held(&self) -> Option<f64> {
        self.held
    }

    /// Where the knob stands: under the hand while held, else at `playing`.
    #[must_use]
    pub fn shown(&self, playing: f64) -> f64 {
        self.held.unwrap_or(playing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    fn song(title: &str) -> Song {
        Song {
            title: title.into(),
            subtitle: "Artist".into(),
            art: TransportArt::Placeholder,
            art_name: "Album".into(),
            ..Song::default()
        }
    }

    #[test]
    fn the_first_song_lands_and_the_next_cross_fades() {
        let motion = Theme::dark().motion();
        let now = crate::time::now();
        let mut face = SongFace::new();
        assert!(face.show(&song("One"), motion, now));
        assert!(!face.is_moving(now), "the first song is there at once");
        crate::motion::note_source(MotionSource::Pointer);
        assert!(face.show(&song("Two"), motion, now));
        assert!(face.is_moving(now), "the next one fades in");
        let layers: Vec<_> = face.layers(now).map(|(s, _)| s.title.clone()).collect();
        assert_eq!(layers, ["One", "Two"], "the old song fades under the new");
        assert!(
            !face.show(&song("Two"), motion, now),
            "the same song changes nothing"
        );
        let settled = face.settles_at().expect("a fade settles");
        assert!(!face.is_moving(settled));
        assert_eq!(face.layers(settled).count(), 1);
    }

    #[test]
    fn a_song_a_key_changed_lands_at_once() {
        let motion = Theme::dark().motion();
        let now = crate::time::now();
        let mut face = SongFace::new();
        face.show(&song("One"), motion, now);
        crate::motion::note_source(MotionSource::Keyboard);
        face.show(&song("Two"), motion, now);
        assert!(!face.is_moving(now));
        assert_eq!(face.song().title, "Two");
    }

    #[test]
    fn a_held_scrub_follows_the_hand_and_seeks_once_on_release() {
        let face = SliderFace::new(Rect::new(0.0, 0.0, 200.0, 24.0), 0.25);
        let track = face.track_rect();
        let at = |fraction: f64| Point::new(track.x0 + track.width() * fraction, 12.0);
        let mut scrub = Scrub::default();
        assert_eq!(
            scrub.drag(&face, at(0.5)),
            None,
            "nothing held, nothing moves"
        );
        assert!((scrub.press(&face, at(0.5)) - 0.5).abs() < 1e-9);
        assert!((scrub.drag(&face, at(0.8)).expect("held") - 0.8).abs() < 1e-9);
        assert!(
            (scrub.shown(0.25) - 0.8).abs() < 1e-9,
            "the knob is under the hand"
        );
        assert!((scrub.release().expect("a seek") - 0.8).abs() < 1e-9);
        assert_eq!(scrub.release(), None, "one seek per hold");
        assert!(
            (scrub.shown(0.25) - 0.25).abs() < 1e-9,
            "back at the playhead"
        );
    }
}
