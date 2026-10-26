//! The little looping pictures of the gestures hints teach, drawn with
//! vector shapes in the card's own ink, so they stay crisp at any scale and
//! follow the card's colour in every theme.
//!
//! Each is drawn in a 64 × 40 design box fitted into the card's picture
//! tile, at a phase of its loop (`0..1`, [`crate::motion::Beckon`]).

use super::Gesture;
use crate::motion::Beckon;
use vello::Scene;
use vello::kurbo::{
    Affine, BezPath, Circle, Line, Point, Rect, RoundedRect, RoundedRectRadii, Stroke, Vec2,
};
use vello::peniko::{BlendMode, Color, Fill};

/// The design box every picture is drawn in.
const ART_W: f64 = 64.0;
const ART_H: f64 = 40.0;
/// A mini window's corner and the height of its bar.
const WINDOW_RADIUS: f64 = 3.0;
const BAR_H: f64 = 5.5;
/// The pointer's size against a full-size one.
const POINTER_SCALE: f64 = 0.62;

/// The colours a picture is drawn in.
#[derive(Clone, Copy, Debug)]
pub(super) struct Ink {
    /// Lines, the pointer, a window's bar.
    pub line: Color,
    /// The card under the picture: the pointer's outline.
    pub ground: Color,
}

impl Ink {
    fn soft(self, alpha: f32) -> Color {
        self.line.multiply_alpha(alpha)
    }
}

/// Paint `gesture` at `phase` of its loop into `tile`, whole at `alpha`.
pub(super) fn paint(
    scene: &mut Scene,
    transform: Affine,
    tile: Rect,
    gesture: Gesture,
    phase: f64,
    ink: Ink,
    alpha: f32,
) {
    if alpha <= 0.0 || tile.is_zero_area() {
        return;
    }
    let scale = (tile.width() / ART_W).min(tile.height() / ART_H);
    let origin = Vec2::new(
        tile.x0 + (tile.width() - ART_W * scale) / 2.0,
        tile.y0 + (tile.height() - ART_H * scale) / 2.0,
    );
    let t = transform * Affine::translate(origin) * Affine::scale(scale);
    // A picture that does not come back to where it started on its own
    // fades out and in again round its loop.
    let loop_alpha = match gesture {
        Gesture::OpenFromSearch => {
            Beckon::stroke(phase, 0.0, 0.04) * (1.0 - Beckon::stroke(phase, 0.86, 0.96))
        }
        Gesture::MoveWindow | Gesture::ResizeEdge | Gesture::PanView => 1.0,
    };
    #[allow(
        clippy::cast_possible_truncation,
        reason = "an opacity is drawn at f32 precision"
    )]
    let layer_alpha = alpha * loop_alpha as f32;
    let clip = RoundedRect::from_rect(tile, 8.0);
    scene.push_layer(
        Fill::NonZero,
        BlendMode::default(),
        layer_alpha,
        transform,
        &clip,
    );
    match gesture {
        Gesture::OpenFromSearch => open_from_search(scene, t, phase, ink),
        Gesture::MoveWindow => move_window(scene, t, phase, ink),
        Gesture::ResizeEdge => resize_edge(scene, t, phase, ink),
        Gesture::PanView => pan_view(scene, t, phase, ink),
    }
    scene.pop_layer();
}

/// A mini window: a rounded frame with its bar, `pressed` brightening the
/// bar as a hand holds it.
fn window(scene: &mut Scene, t: Affine, rect: Rect, ink: Ink, pressed: f64) {
    let frame = RoundedRect::from_rect(rect, WINDOW_RADIUS);
    scene.fill(Fill::NonZero, t, ink.soft(0.16), None, &frame);
    let bar = RoundedRect::from_rect(
        Rect::new(rect.x0, rect.y0, rect.x1, rect.y0 + BAR_H),
        RoundedRectRadii::new(WINDOW_RADIUS, WINDOW_RADIUS, 0.0, 0.0),
    );
    #[allow(
        clippy::cast_possible_truncation,
        reason = "an opacity is drawn at f32 precision"
    )]
    let bar_alpha = (0.55 + 0.4 * pressed.clamp(0.0, 1.0)) as f32;
    scene.fill(Fill::NonZero, t, ink.soft(bar_alpha), None, &bar);
    scene.stroke(&Stroke::new(1.6), t, ink.line, None, &frame);
}

/// The pointer, its tip at `tip`, a little smaller while `pressed`.
fn pointer(scene: &mut Scene, t: Affine, tip: Point, pressed: f64, ink: Ink) {
    let size = POINTER_SCALE * (1.0 - 0.1 * pressed.clamp(0.0, 1.0));
    let at = t * Affine::translate(tip.to_vec2()) * Affine::scale(size);
    let arrow = crate::presence::render::arrow_path();
    scene.stroke(
        &Stroke::new(2.4 / size).with_join(vello::kurbo::Join::Round),
        at,
        ink.ground,
        None,
        &arrow,
    );
    scene.fill(Fill::NonZero, at, ink.line, None, &arrow);
}

/// The sideways resize pointer, centred on `at`.
fn resize_pointer(scene: &mut Scene, t: Affine, at: Point, ink: Ink) {
    let mut path = BezPath::new();
    let (x, y) = (at.x, at.y);
    path.move_to((x - 7.0, y));
    path.line_to((x - 3.6, y - 3.6));
    path.line_to((x - 3.6, y - 1.3));
    path.line_to((x + 3.6, y - 1.3));
    path.line_to((x + 3.6, y - 3.6));
    path.line_to((x + 7.0, y));
    path.line_to((x + 3.6, y + 3.6));
    path.line_to((x + 3.6, y + 1.3));
    path.line_to((x - 3.6, y + 1.3));
    path.line_to((x - 3.6, y + 3.6));
    path.close_path();
    scene.stroke(
        &Stroke::new(2.2).with_join(vello::kurbo::Join::Round),
        t,
        ink.ground,
        None,
        &path,
    );
    scene.fill(Fill::NonZero, t, ink.line, None, &path);
}

/// Along the straight line from `a` to `b`, `at` of the way.
fn between(a: Point, b: Point, at: f64) -> Point {
    a.lerp(b, at.clamp(0.0, 1.0))
}

/// How hard the hand holds between `down` and `up` (`0..=1`).
fn held(phase: f64, down: f64, up: f64) -> f64 {
    Beckon::stroke(phase, down - 0.04, down) - Beckon::stroke(phase, up, up + 0.04)
}

/// A key chord brings up search; something typed; a window comes up.
fn open_from_search(scene: &mut Scene, t: Affine, phase: f64, ink: Ink) {
    let shown = Beckon::stroke(phase, 0.04, 0.18);
    let pill = Rect::new(12.0, 5.0, 52.0, 15.0);
    let grown = Affine::scale_about(0.8 + 0.2 * shown, pill.center());
    #[allow(
        clippy::cast_possible_truncation,
        reason = "an opacity is drawn at f32 precision"
    )]
    let pill_alpha = shown as f32;
    let shape = RoundedRect::from_rect(pill, pill.height() / 2.0);
    scene.fill(
        Fill::NonZero,
        t * grown,
        ink.soft(0.2 * pill_alpha),
        None,
        &shape,
    );
    scene.stroke(
        &Stroke::new(1.6),
        t * grown,
        ink.line.multiply_alpha(pill_alpha),
        None,
        &shape,
    );
    let glass = Circle::new((18.0, 9.6), 2.3);
    scene.stroke(
        &Stroke::new(1.4),
        t * grown,
        ink.line.multiply_alpha(pill_alpha),
        None,
        &glass,
    );
    scene.stroke(
        &Stroke::new(1.4).with_caps(vello::kurbo::Cap::Round),
        t * grown,
        ink.line.multiply_alpha(pill_alpha),
        None,
        &Line::new((19.7, 11.3), (21.2, 12.8)),
    );
    // What is typed: a word's worth of ink growing along the field.
    let typed = Beckon::stroke(phase, 0.2, 0.44);
    if typed > 0.0 {
        let word = RoundedRect::from_rect(Rect::new(24.0, 8.8, 24.0 + 18.0 * typed, 11.2), 1.2);
        scene.fill(Fill::NonZero, t, ink.soft(0.85), None, &word);
    }
    // The window it opens, coming up under the field.
    let open = Beckon::stroke(phase, 0.48, 0.64);
    if open > 0.0 {
        let rect = Rect::new(17.0, 19.0, 47.0, 37.0);
        let rise = Affine::translate((0.0, 4.0 * (1.0 - open)))
            * Affine::scale_about(0.7 + 0.3 * open, Point::new(rect.center().x, rect.y0));
        #[allow(
            clippy::cast_possible_truncation,
            reason = "an opacity is drawn at f32 precision"
        )]
        let alpha = open as f32;
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            alpha,
            t,
            &Rect::new(0.0, 0.0, ART_W, ART_H),
        );
        window(scene, t * rise, rect, ink, 0.0);
        scene.pop_layer();
    }
}

/// A hand takes a window by its bar and carries it across, and back.
fn move_window(scene: &mut Scene, t: Affine, phase: f64, ink: Ink) {
    let home = Rect::new(9.0, 13.0, 35.0, 33.0);
    let travel = 18.0 * (Beckon::stroke(phase, 0.24, 0.52) - Beckon::stroke(phase, 0.62, 0.88));
    let pressed = held(phase, 0.2, 0.9);
    if travel > 0.5 {
        // Where it came from, faintly.
        let slot = RoundedRect::from_rect(home, WINDOW_RADIUS);
        scene.stroke(&Stroke::new(1.0), t, ink.soft(0.3), None, &slot);
    }
    let moved = home + Vec2::new(travel, 0.0);
    window(scene, t, moved, ink, pressed);
    let grip = Point::new(moved.x0 + 14.0, moved.y0 + BAR_H / 2.0);
    let rest = Point::new(48.0, 38.0);
    let tip = if phase < 0.2 {
        between(rest, grip, Beckon::stroke(phase, 0.0, 0.18))
    } else if phase < 0.92 {
        grip
    } else {
        between(grip, rest, Beckon::stroke(phase, 0.92, 1.0))
    };
    pointer(scene, t, tip, pressed, ink);
}

/// A hand draws a window's edge out, and back.
fn resize_edge(scene: &mut Scene, t: Affine, phase: f64, ink: Ink) {
    let pressed = held(phase, 0.2, 0.9);
    let reach = 16.0 * (Beckon::stroke(phase, 0.24, 0.52) - Beckon::stroke(phase, 0.62, 0.88));
    let rect = Rect::new(8.0, 10.0, 32.0 + reach, 32.0);
    window(scene, t, rect, ink, 0.0);
    // The edge lit along its length, as the desktop lights it.
    let on_edge = Beckon::stroke(phase, 0.12, 0.2) - Beckon::stroke(phase, 0.92, 0.98);
    if on_edge > 0.0 {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "an opacity is drawn at f32 precision"
        )]
        let alpha = (on_edge * (0.6 + 0.4 * pressed)) as f32;
        scene.stroke(
            &Stroke::new(2.6).with_caps(vello::kurbo::Cap::Round),
            t,
            ink.line.multiply_alpha(alpha),
            None,
            &Line::new((rect.x1, rect.y0 + 2.5), (rect.x1, rect.y1 - 2.5)),
        );
    }
    let grip = Point::new(rect.x1, (rect.y0 + rect.y1) / 2.0 + 1.0);
    let rest = Point::new(54.0, 37.0);
    let at = if phase < 0.18 {
        between(rest, grip, Beckon::stroke(phase, 0.0, 0.16))
    } else if phase < 0.92 {
        grip
    } else {
        between(grip, rest, Beckon::stroke(phase, 0.92, 1.0))
    };
    if on_edge > 0.5 {
        resize_pointer(scene, t, at, ink);
    } else {
        pointer(scene, t, at, 0.0, ink);
    }
}

/// A hand drags the empty canvas; the windows slide with it, and back.
fn pan_view(scene: &mut Scene, t: Affine, phase: f64, ink: Ink) {
    let pressed = held(phase, 0.2, 0.9);
    let slide = -26.0 * (Beckon::stroke(phase, 0.24, 0.54) - Beckon::stroke(phase, 0.62, 0.88));
    for x in [-16.0, 10.0, 36.0, 62.0] {
        window(
            scene,
            t,
            Rect::new(x + slide, 7.0, x + slide + 22.0, 27.0),
            ink,
            0.0,
        );
    }
    let grip = Point::new(34.0 + slide, 31.5);
    let rest = Point::new(50.0, 39.0);
    let tip = if phase < 0.2 {
        between(rest, grip, Beckon::stroke(phase, 0.0, 0.18))
    } else if phase < 0.92 {
        grip
    } else {
        between(grip, rest, Beckon::stroke(phase, 0.92, 1.0))
    };
    if pressed > 0.0 {
        // The press on empty canvas, as a small ring under the hand.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "an opacity is drawn at f32 precision"
        )]
        let alpha = (0.5 * pressed) as f32;
        scene.stroke(
            &Stroke::new(1.0),
            t,
            ink.line.multiply_alpha(alpha),
            None,
            &Circle::new(grip, 3.0 + 1.5 * pressed),
        );
    }
    pointer(scene, t, tip, pressed, ink);
}
