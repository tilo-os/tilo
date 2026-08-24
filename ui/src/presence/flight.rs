//! **Presence for actions**: the object an action moved, shown crossing
//! between the two applications it moved it between.
//!
//! The rest of this module paints where a participant *is* — their pointer,
//! their caret, the mist at the edge they drifted past. This paints what a
//! participant *did*, and it exists because an agent's work was otherwise
//! invisible: it performed an action, a document changed, and a picture
//! appeared with nothing in between for anyone to watch. A viewer had to be
//! told what happened rather than shown it.
//!
//! # Why this is not a synthesised pointer
//!
//! The obvious way to make an agent's work visible is to have it drive a
//! cursor, so it looks like a person using the machine. That would be a
//! considerable regression, and it is worth being explicit about what would
//! be lost, because every one of these properties exists *because* an agent
//! acts through declared actions instead of pretending to be a person:
//!
//! * **Attribution.** The door takes the caller's actor bytes and commits
//!   the Automerge change under them, so history names who asked.
//!   Synthesised input arrives as *the seat's* pointer, and the edit would
//!   be attributed to whoever is sitting there. `undo_for_author` and the
//!   durable author mark both stop meaning anything.
//! * **The capability gate.** An operation the caller may not perform is
//!   absent from the survey, so its address resolves to nothing. Synthesised
//!   input has no address to check, so there is nothing to deny, and the
//!   permission model becomes advisory.
//! * **Working without a screen.** An action address is two app-minted ids
//!   and no coordinate, so a caller with no display — on a peer's machine,
//!   in a test, over a socket — performs the same operation. A pointer path
//!   needs a screen, and races the layout it is aiming at.
//! * **Replay.** An action is a value that replays to a byte-identical
//!   change on every replica. A cursor path is not a value; it produces one
//!   local effect that no peer can replay or dedupe.
//!
//! And it would be *less* truthful, not more. A synthesised gesture shows a
//! plausible drag whether or not anything happened at the end of it — an
//! action that was refused would still animate. What is drawn here is a
//! consequence of an action that already succeeded, so an operation that did
//! not happen shows nothing at all.
//!
//! # What proves two applications were involved
//!
//! Not a guess from timing, and not "two windows changed at once". The
//! action model already records the dependency, in `Collects::Content`: an
//! argument that is a content hash "nobody types and nothing can invent: it
//! exists only because some earlier operation produced the content it
//! names."
//!
//! So when an action's argument is a content address, and some other
//! application on this machine is the one that offered that content, the two
//! ends of a transfer are known facts rather than an inference. The word
//! processor's `place-image` cannot have been invoked with a hash unless
//! something produced the bytes behind it.
//!
//! That is also what makes the object *identifiable in flight*: the payload
//! is a content hash, the bytes are in the store, so what crosses the screen
//! is the photograph itself rather than a token standing in for it.

use std::time::{Duration, Instant};

use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::{BlendMode, Color, Fill, ImageData as PenikoImage};
use vello::Scene;

use crate::color_compat::ColorCompat;
use crate::permissions::UserId;
use crate::text_render::TextRenderer;
use crate::theme::Theme;

use super::render::{color_for, contrast_ink};
use super::PresenceCtx;

/// How long one crossing takes.
///
/// Sized to be followed rather than merely noticed. The strip's make-space
/// slide is 420 ms and a tab's close fade 180 ms, but both of those are the
/// interface acknowledging something the person just did and already knows
/// about. This is the opposite case: nobody watching knows an agent is about
/// to move a photograph between two applications, so the motion has to carry
/// the whole explanation, and it is the only thing on screen that will.
///
/// Long enough to read the object and both endpoints; short enough that it
/// is over before it becomes a wait.
pub const FLIGHT_DURATION: Duration = Duration::from_millis(720);

/// Longest edge of the object as it crosses, in logical pixels.
///
/// Big enough to recognise a photograph from across a room — this is filmed —
/// and small enough that it never hides the window it is arriving into.
pub const FLIGHT_LONG_EDGE: f64 = 132.0;

/// How far the arc bows away from the straight line between the two ends, as
/// a fraction of the distance travelled, and the most it may ever bow.
///
/// A straight line reads as a value being assigned. Something carried by
/// somebody arcs, and the arc is what makes this read as a hand moving an
/// object rather than a property animating between two numbers.
const ARC_RISE: f64 = 0.16;
const ARC_RISE_MAX: f64 = 104.0;

/// One object crossing from the application that produced it to the one an
/// action just placed it in.
///
/// Recorded **after** the action succeeded, never before: this is a report,
/// and an action that was refused leaves no record and therefore draws
/// nothing. Nothing here is on the action's own path, so noting a flight
/// cannot add latency to performing one.
#[derive(Clone, Debug, PartialEq)]
pub struct ActionFlight {
    /// Who performed the action. Drives the colour, so the crossing reads as
    /// a particular participant having done it rather than as an animation
    /// the application decided to play.
    pub actor: UserId,
    /// The content hash that travelled — the object's identity, and what the
    /// bytes are looked up by so the real thing can be drawn.
    pub payload: String,
    /// When it started. Progress is read off the clock rather than stepped,
    /// so a dropped frame costs smoothness and never desynchronises the
    /// object from the two ends it is travelling between.
    pub started: Instant,
}

/// Deliberately no endpoints.
///
/// Where the two applications are on screen is the compositor's to answer,
/// and it has to answer it *every frame* rather than once: the strip pans
/// while the object is in the air — often because revealing the destination
/// is part of the same gesture — so coordinates frozen when the action
/// landed would have the object drift away from the windows it is supposed
/// to be travelling between. Resolving per frame keeps it glued to them, and
/// is what lets the pan and the flight read as one movement instead of two.
impl ActionFlight {
    pub fn new(actor: UserId, payload: impl Into<String>) -> Self {
        Self {
            actor,
            payload: payload.into(),
            started: Instant::now(),
        }
    }

    /// Linear progress at `now`, or `None` once the crossing is over.
    ///
    /// `None` is what retires a flight: a caller drops the ones that answer
    /// it so the list cannot grow without bound.
    pub fn progress(&self, now: Instant) -> Option<f64> {
        let elapsed = now.saturating_duration_since(self.started);
        if elapsed >= FLIGHT_DURATION {
            return None;
        }
        Some(elapsed.as_secs_f64() / FLIGHT_DURATION.as_secs_f64())
    }
}

/// The object's state at one instant of its crossing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FlightPose {
    /// Centre of the object in scene coordinates.
    pub center: Point,
    /// Multiplier on its resting size. Peaks mid-flight, so it reads as
    /// lifted over the gap and set back down rather than slid across it.
    pub scale: f64,
    /// 0..=1. Fades in as it leaves and out as it merges into what the
    /// destination is now drawing for itself.
    pub alpha: f64,
}

/// Cubic ease-in-out.
///
/// Accelerating out of the source and decelerating into the destination is
/// what makes the two ends read as *places* — the object leaves one and
/// arrives at the other, rather than passing through both at constant speed
/// on its way somewhere else.
fn ease_in_out(t: f64) -> f64 {
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        let f = -2.0 * t + 2.0;
        1.0 - (f * f * f) / 2.0
    }
}

/// The pose at linear progress `t` for a crossing between `from` and `to`.
///
/// Split out as a free function of its inputs so the curve can be checked
/// without a clock, a GPU, or a compositor.
pub fn pose(from: Point, to: Point, t: f64) -> FlightPose {
    let t = t.clamp(0.0, 1.0);
    let e = ease_in_out(t);

    let travel = to - from;
    let distance = travel.hypot();

    // Bow the path away from the straight line. The offset is perpendicular
    // to the travel and always points *up* the screen, whichever way the
    // object is going: an object carried over a gap rises, and one that
    // sagged downward on a right-to-left trip would read as dropped.
    let rise = (distance * ARC_RISE).min(ARC_RISE_MAX);
    let perpendicular = if distance > f64::EPSILON {
        let n = Vec2::new(travel.y, -travel.x) / distance;
        if n.y > 0.0 {
            -n
        } else {
            n
        }
    } else {
        Vec2::new(0.0, -1.0)
    };
    // Peaks at 1.0 halfway along and vanishes at both ends, so the object
    // starts and finishes exactly on its endpoints.
    let bow = 4.0 * e * (1.0 - e);

    let center = from + travel * e + perpendicular * (rise * bow);

    FlightPose {
        center,
        // Largest where the arc is highest — the same "lifted" reading, said
        // twice so it survives being watched on a small screen.
        scale: 1.0 + 0.10 * (std::f64::consts::PI * e).sin(),
        alpha: alpha_at(t),
    }
}

/// Opacity at linear progress `t`.
///
/// The tail matters more than the head: the destination begins drawing the
/// object for itself the moment the action lands, so a flight that stayed
/// solid to the last frame would briefly show the same photograph twice.
fn alpha_at(t: f64) -> f64 {
    const IN: f64 = 0.08;
    const OUT: f64 = 0.14;
    if t < IN {
        t / IN
    } else if t > 1.0 - OUT {
        ((1.0 - t) / OUT).max(0.0)
    } else {
        1.0
    }
}

/// The object's resting rectangle for a source image of `w` by `h` pixels,
/// centred on `center` and scaled by `scale`.
///
/// Aspect is preserved from the content itself, so a wide crop crosses the
/// screen wide. A photograph that arrived as a square regardless of its
/// shape would be a token standing in for the object rather than the object.
pub fn flight_rect(center: Point, scale: f64, w: u32, h: u32) -> Rect {
    let (w, h) = if w == 0 || h == 0 { (1, 1) } else { (w, h) };
    let long = w.max(h) as f64;
    let unit = FLIGHT_LONG_EDGE * scale / long;
    let half_w = w as f64 * unit / 2.0;
    let half_h = h as f64 * unit / 2.0;
    Rect::new(
        center.x - half_w,
        center.y - half_h,
        center.x + half_w,
        center.y + half_h,
    )
}

/// Paint one crossing at linear progress `t`.
///
/// `from` and `to` are this frame's answer to where the two applications
/// are, in scene coordinates — see [`ActionFlight`] on why they are supplied
/// rather than stored. `content` is the payload's bytes decoded, when this
/// machine holds them.
///
/// Everything drawn is either the object itself or the participant's colour.
/// Nothing here is a fixed decoration the application chose: the colour
/// comes from the presence machinery that already colours that participant's
/// pointer and caret, so a viewer reads the same person doing all three.
#[allow(clippy::too_many_arguments)]
pub fn draw_action_flight(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    presence: &PresenceCtx,
    actor: &UserId,
    name: &str,
    content: Option<&PenikoImage>,
    from: Point,
    to: Point,
    t: f64,
) {
    let p = pose(from, to, t);
    if p.alpha <= 0.001 {
        return;
    }
    let a = p.alpha as f32;
    let peer = color_for(presence, actor, theme);
    let tokens = &theme.tokens;

    // Size from the content's own shape when it is here. Without the bytes
    // there is nothing to take a shape from, so it falls back to a square —
    // and the object is drawn as a plain card rather than pretending to be a
    // picture nobody on this machine can see.
    let (iw, ih) = content.map(|i| (i.width, i.height)).unwrap_or((1, 1));
    let rect = flight_rect(p.center, p.scale, iw, ih);
    let radius = tokens.radii.md as f64;

    // A shadow, offset down, so the object reads as held above the desktop
    // rather than pasted onto it. Layered rather than blurred: three
    // progressively larger, fainter rounds approximate a soft edge at a
    // fraction of the cost, which is the same trick the edge mist uses.
    for i in (1..=3).rev() {
        let spread = i as f64 * 3.0;
        let shade = RoundedRect::from_rect(
            Rect::new(
                rect.x0 - spread,
                rect.y0 - spread + 4.0,
                rect.x1 + spread,
                rect.y1 + spread + 4.0,
            ),
            radius + spread,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            Color::from_rgba8(0, 0, 0, 255).with_alpha_factor(0.05 * a),
            None,
            &shade,
        );
    }

    match content {
        // The photograph itself, which is the whole point: what crosses the
        // screen has to be recognisable as the thing that moved, or the
        // motion is decoration rather than evidence.
        Some(img) => {
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                a,
                transform,
                &RoundedRect::from_rect(rect, radius),
            );
            crate::image_store::paint_image_cover(scene, transform, rect, img, radius);
            scene.pop_layer();
        }
        // The action happened and this machine cannot draw what it carried.
        // A card in the participant's colour still says *somebody moved
        // something from here to there*, which is true; inventing a picture
        // would not be.
        None => {
            scene.fill(
                Fill::NonZero,
                transform,
                theme.bg_primary.with_alpha_factor(a),
                None,
                &RoundedRect::from_rect(rect, radius),
            );
        }
    }

    // The participant's colour around the edge. This is what makes the
    // object read as carried *by somebody* rather than as an animation the
    // destination application decided to play when it received something.
    scene.stroke(
        &Stroke::new(2.0),
        transform,
        peer.with_alpha_factor(a),
        None,
        &RoundedRect::from_rect(rect, radius),
    );

    // And a chip that says who, in the same vocabulary as the name tag on a
    // peer's pointer and on the mist at the screen edge.
    if !name.is_empty() {
        let label_size = tokens.typography.scale.xs;
        let (tw, _) = text_renderer.measure(name, label_size, None);
        let pad = 5.0_f64;
        let chip = Rect::new(
            rect.x0,
            rect.y0 - label_size as f64 - pad * 1.5,
            rect.x0 + tw as f64 + pad * 2.0,
            rect.y0 - 2.0,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            peer.with_alpha_factor(a),
            None,
            &RoundedRect::from_rect(chip, tokens.radii.sm as f64),
        );
        text_renderer.draw_text(
            scene,
            name,
            label_size,
            contrast_ink(peer).with_alpha_factor(a),
            transform * Affine::translate((chip.x0 + pad, chip.y0 + pad / 2.0)),
            None,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Point = Point::new(100.0, 400.0);
    const B: Point = Point::new(900.0, 400.0);

    #[test]
    fn a_crossing_starts_and_ends_exactly_on_its_endpoints() {
        // The arc must not leave the object hovering near the window it came
        // from or short of the one it landed in: the endpoints are the whole
        // claim being made.
        let start = pose(A, B, 0.0);
        let end = pose(A, B, 1.0);
        assert!((start.center - A).hypot() < 1e-9, "{:?}", start.center);
        assert!((end.center - B).hypot() < 1e-9, "{:?}", end.center);
    }

    #[test]
    fn the_path_bows_upward_whichever_way_it_travels() {
        // Rightward and leftward are the same carry. A trip that sagged
        // downward one way would read as the object being dropped.
        let right = pose(A, B, 0.5).center;
        let left = pose(B, A, 0.5).center;
        assert!(right.y < A.y, "rightward should rise: {right:?}");
        assert!(left.y < A.y, "leftward should rise too: {left:?}");
    }

    #[test]
    fn the_object_is_lifted_in_the_middle_and_resting_at_both_ends() {
        assert!((pose(A, B, 0.0).scale - 1.0).abs() < 1e-6);
        assert!((pose(A, B, 1.0).scale - 1.0).abs() < 1e-6);
        assert!(pose(A, B, 0.5).scale > 1.05, "should lift mid-flight");
    }

    #[test]
    fn it_is_solid_for_most_of_the_way_and_gone_at_the_end() {
        assert!(pose(A, B, 0.0).alpha < 0.01, "fades in rather than popping");
        assert!(pose(A, B, 0.5).alpha > 0.99, "solid while it is crossing");
        assert!(pose(A, B, 1.0).alpha < 0.01, "hands over to the destination");
    }

    #[test]
    fn progress_moves_forward_and_then_retires() {
        let f = ActionFlight::new(UserId("claude".into()), "abc");
        let quarter = f.started + FLIGHT_DURATION / 4;
        let half = f.started + FLIGHT_DURATION / 2;
        let p1 = f.progress(quarter).expect("still crossing at a quarter");
        let p2 = f.progress(half).expect("still crossing at half");
        assert!(p2 > p1, "{p1} then {p2}");
        assert!(
            f.progress(f.started + FLIGHT_DURATION).is_none(),
            "a finished crossing must retire, or the list grows for ever",
        );
    }

    #[test]
    fn a_zero_length_crossing_does_not_divide_by_its_own_distance() {
        // Two applications sharing one window can put both ends in the same
        // place. The arithmetic must not produce NaN and paint nothing.
        let p = pose(A, A, 0.5);
        assert!(p.center.x.is_finite() && p.center.y.is_finite(), "{p:?}");
    }

    #[test]
    fn the_object_keeps_the_shape_of_the_content_it_carries() {
        let wide = flight_rect(Point::ZERO, 1.0, 400, 100);
        assert!(
            wide.width() > wide.height() * 3.0,
            "a wide crop must cross wide: {wide:?}",
        );
        assert!((wide.width() - FLIGHT_LONG_EDGE).abs() < 1e-6);
        // Degenerate metadata must not produce an infinite rect.
        assert!(flight_rect(Point::ZERO, 1.0, 0, 0).width().is_finite());
    }
}
