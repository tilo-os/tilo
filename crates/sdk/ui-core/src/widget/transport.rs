//! `TransportFace`: the previous, play/pause and next controls of a media
//! player, for a painted host that owns its hit-testing (a music bar, a
//! video's controls).
//!
//! Play/pause is the transport's one prominent control: a filled circle in
//! the primary action's inverted colours with a solid glyph, a size up from
//! its neighbours. It is the one round button: a circle by meaning, the
//! way a play control is everywhere. Previous and next are plain: solid
//! glyphs in the ground's ink, a round lift under the pointer. The glyphs
//! are solid media marks (a play triangle, pause bars, a skip triangle
//! against its stop bar), never outlines. With nothing to play the
//! controls keep their look under the disabled alpha.
//!
//! ```ignore
//! let side = TransportFace::side(TransportControl::PlayPause { playing }, theme);
//! let face = TransportFace::new(rect, TransportControl::PlayPause { playing })
//!     .hovered(face_hit.contains(mouse));
//! TransportFace::paint(scene, transform, theme, &face);
//! ```

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Point, Rect, RoundedRect, Shape, Stroke};
use vello::peniko::Fill;

use crate::icons::Icon;
use crate::theme::Theme;
use crate::widget::{ButtonInteract, ButtonRole, ControlSize, face_colors};

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
        // A transport with nothing to play keeps its look under the disabled
        // alpha, so play still reads as the primary control.
        let disabled = face.interact.disabled;
        let enabled = ButtonInteract {
            disabled: false,
            hovered: face.interact.hovered && !disabled,
            pressed: face.interact.pressed && !disabled,
            ..face.interact
        };
        let (fill, _, ink) = face_colors(theme, role, enabled);
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
        let glyph = (side * if face.control.is_play() { 0.42 } else { 0.5 }).round();
        let glyph_rect = Rect::from_center_size(centre, (glyph, glyph));
        for shape in glyph_shapes(face.control, glyph_rect) {
            scene.fill(Fill::NonZero, transform, ink, None, &shape);
        }
        if disabled {
            scene.pop_layer();
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

/// The solid media glyphs of `control` in the square `r`: a play triangle
/// (its mass, not its box, centred), two pause bars, or a skip triangle
/// against its stop bar. Corners are softened the way the icon set's are.
fn glyph_shapes(control: TransportControl, r: Rect) -> Vec<BezPath> {
    let g = r.width();
    let at = |x: f64, y: f64| Point::new(r.x0 + x * g, r.y0 + y * g);
    let corner = g * 0.08;
    let bar = |x0: f64, x1: f64| {
        RoundedRect::from_rect(
            Rect::from_points(at(x0, 0.1), at(x1, 0.9)),
            (g * 0.06).max(1.0),
        )
        .to_path(0.1)
    };
    match control {
        TransportControl::PlayPause { playing: true } => vec![bar(0.14, 0.4), bar(0.6, 0.86)],
        TransportControl::PlayPause { playing: false } => {
            vec![rounded_triangle(
                [at(0.18, 0.06), at(0.18, 0.94), at(0.96, 0.5)],
                corner,
            )]
        }
        TransportControl::Next => vec![
            rounded_triangle([at(0.08, 0.1), at(0.08, 0.9), at(0.74, 0.5)], corner),
            bar(0.76, 0.92),
        ],
        TransportControl::Previous => vec![
            rounded_triangle([at(0.92, 0.1), at(0.92, 0.9), at(0.26, 0.5)], corner),
            bar(0.08, 0.24),
        ],
    }
}

/// A triangle through `points` with each corner rounded by `radius`.
fn rounded_triangle(points: [Point; 3], radius: f64) -> BezPath {
    let [a, b, c] = points;
    let mut path = BezPath::new();
    for (i, (prev, here, next)) in [(c, a, b), (a, b, c), (b, c, a)].into_iter().enumerate() {
        let toward = |to: Point| {
            let d = to - here;
            let len = d.hypot();
            if len <= 0.0 {
                here
            } else {
                here + d * (radius.min(len / 2.0) / len)
            }
        };
        let (enter, leave) = (toward(prev), toward(next));
        if i == 0 {
            path.move_to(enter);
        } else {
            path.line_to(enter);
        }
        path.quad_to(here, leave);
    }
    path.close_path();
    path
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
