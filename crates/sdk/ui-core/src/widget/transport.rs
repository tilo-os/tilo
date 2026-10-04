//! `TransportFace`: the previous, play/pause and next controls of a media
//! player, for a painted host that owns its hit-testing (a music bar, a
//! video's controls).
//!
//! Play/pause is the transport's one prominent control: a filled circle in
//! the primary action's inverted colours with a solid glyph, a size up from
//! its neighbours. It is the one round button: a circle by meaning, the
//! way a play control is everywhere. Previous and next are plain: solid
//! glyphs in the ground's ink, a round lift under the pointer. The glyphs
//! are drawn solid ([`Icon::draw_solid`]), never as outlines.
//!
//! ```ignore
//! let side = TransportFace::side(TransportControl::PlayPause { playing }, theme);
//! let face = TransportFace::new(rect, TransportControl::PlayPause { playing })
//!     .hovered(face_hit.contains(mouse));
//! TransportFace::paint(scene, transform, theme, &face);
//! ```

use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, Stroke};
use vello::peniko::Fill;

use crate::icons::Icon;
use crate::theme::Theme;
use crate::widget::{ButtonInteract, ButtonRole, ControlSize, face_colors};

/// One control of a media transport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportControl {
    Previous,
    /// Play while paused, pause while playing.
    PlayPause {
        playing: bool,
    },
    Next,
}

impl TransportControl {
    /// The glyph the control shows now.
    #[must_use]
    pub const fn icon(self) -> Icon {
        match self {
            Self::Previous => Icon::SkipBack,
            Self::PlayPause { playing: true } => Icon::Pause,
            Self::PlayPause { playing: false } => Icon::Play,
            Self::Next => Icon::SkipForward,
        }
    }

    /// What the control does now: its accessible name and tooltip.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Previous => "Previous track",
            Self::PlayPause { playing: true } => "Pause",
            Self::PlayPause { playing: false } => "Play",
            Self::Next => "Next track",
        }
    }

    const fn is_play(self) -> bool {
        matches!(self, Self::PlayPause { .. })
    }
}

/// The face of one transport control at `rect` (a square: the circle is
/// inscribed in it).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransportFace {
    rect: Rect,
    control: TransportControl,
    interact: ButtonInteract,
}

impl TransportFace {
    #[must_use]
    pub fn new(rect: Rect, control: TransportControl) -> Self {
        Self {
            rect,
            control,
            interact: ButtonInteract::default(),
        }
    }

    /// The side of `control`'s square: play/pause is a large control,
    /// previous and next regular ones.
    #[must_use]
    pub fn side(control: TransportControl, theme: &Theme) -> f64 {
        if control.is_play() {
            ControlSize::Large.height(theme)
        } else {
            ControlSize::Regular.height(theme)
        }
    }

    #[must_use]
    pub fn hovered(mut self, v: bool) -> Self {
        self.interact.hovered = v;
        self
    }

    #[must_use]
    pub fn pressed(mut self, v: bool) -> Self {
        self.interact.pressed = v;
        self
    }

    #[must_use]
    pub fn focused(mut self, v: bool) -> Self {
        self.interact.focused = v;
        self
    }

    #[must_use]
    pub fn disabled(mut self, v: bool) -> Self {
        self.interact.disabled = v;
        self
    }

    #[must_use]
    pub fn control(&self) -> TransportControl {
        self.control
    }

    #[must_use]
    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// Where a pointer activates the control: its square, grown about its
    /// centre to the minimum pointer target.
    #[must_use]
    pub fn hit_rect(&self, theme: &Theme) -> Rect {
        let target = f64::from(theme.tokens.metrics.min_pointer_target);
        let grow_x = ((target - self.rect.width()) / 2.0).max(0.0);
        let grow_y = ((target - self.rect.height()) / 2.0).max(0.0);
        self.rect.inflate(grow_x, grow_y)
    }

    /// Paint the control.
    pub fn paint(scene: &mut Scene, transform: Affine, theme: &Theme, face: &Self) {
        let rect = face.rect;
        let side = rect.width().min(rect.height());
        if side <= 0.0 {
            return;
        }
        let centre = rect.center();
        let radius = side / 2.0;
        let role = if face.control.is_play() {
            ButtonRole::Primary
        } else {
            ButtonRole::Plain
        };
        let (fill, edge, ink) = face_colors(theme, role, face.interact);
        let disabled = face.interact.disabled;
        let circle = Circle::new(centre, radius);
        if disabled {
            scene.push_layer(
                Fill::NonZero,
                vello::peniko::BlendMode::default(),
                crate::widget::DISABLED_ALPHA,
                transform,
                &circle,
            );
        }
        if fill.components[3] > 0.0 {
            scene.fill(Fill::NonZero, transform, fill, None, &circle);
        }
        // The glyph takes a little under half the circle; play sits a touch
        // right of centre so the triangle's mass, not its box, is centred.
        let glyph = (side * if face.control.is_play() { 0.46 } else { 0.56 }).round();
        let nudge = if face.control == (TransportControl::PlayPause { playing: false }) {
            glyph / 12.0
        } else {
            0.0
        };
        let glyph_rect =
            Rect::from_center_size(Point::new(centre.x + nudge, centre.y), (glyph, glyph));
        face.control
            .icon()
            .draw_solid(scene, transform, glyph_rect, ink);
        if disabled {
            scene.pop_layer();
        }
        // A resting play button (nothing to play) keeps its round edge, so
        // its bounds still show on a light ground.
        if let Some(edge) = edge
            && (!face.interact.focused || disabled)
        {
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                edge,
                None,
                &Circle::new(centre, radius - 0.5),
            );
        }
        if face.interact.focused && !disabled {
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                theme.border_focus,
                None,
                &Circle::new(centre, radius + 1.0),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn play_is_the_prominent_control() {
        let theme = Theme::dark();
        let play = TransportFace::side(TransportControl::PlayPause { playing: false }, &theme);
        let skip = TransportFace::side(TransportControl::Next, &theme);
        assert!(play > skip, "play {play} stands a size up from skip {skip}");
    }

    #[test]
    fn the_control_names_what_it_does_now() {
        assert_eq!(
            TransportControl::PlayPause { playing: true }.name(),
            "Pause"
        );
        assert_eq!(
            TransportControl::PlayPause { playing: false }.icon(),
            Icon::Play
        );
        assert_eq!(TransportControl::Previous.icon(), Icon::SkipBack);
    }

    #[test]
    fn a_small_face_still_meets_the_pointer_target() {
        let theme = Theme::dark();
        let face = TransportFace::new(Rect::new(0.0, 0.0, 20.0, 20.0), TransportControl::Next);
        let hit = face.hit_rect(&theme);
        assert!(hit.width() >= f64::from(theme.tokens.metrics.min_pointer_target) - 0.01);
    }
}
