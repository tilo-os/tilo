//! **Presence for actions, part two**: the thing an action is being performed
//! *on*, marked while it happens.
//!
//! [`super::flight`] paints an object crossing between two applications, which
//! is the case where the interesting fact is a relationship between two
//! windows. This paints the other and much commoner case: an operation
//! performed on one resource, inside one window, where the interesting fact is
//! *which part of the picture* the participant reached for.
//!
//! Without it an agent's work is provable only by reading the document
//! afterwards. On screen a window simply changes, and nothing on it says who
//! did that or what they touched — the same frame is produced whether the
//! person at the keyboard cropped the photograph or a participant on the other
//! side of the room did.
//!
//! # A resource does not know where it is, and must not
//!
//! An action is addressed as `<resource id>#<action id>`, both minted by the
//! app, and the app's own surface is emphatic that "no screen position appears
//! anywhere in this surface" — that is what lets a caller with no display
//! enumerate and invoke, and what keeps one address meaning one operation
//! across layouts, themes and resolutions.
//!
//! So the association runs the other way. A *view* of a resource knows both:
//! it is a widget, so it has a rect, and it is showing a published resource, so
//! it knows the id. [`Widget::resource_rects`](crate::widget::Widget::resource_rects)
//! is where it says so, and it is the same shape as
//! [`Widget::test_rects`](crate::widget::Widget::test_rects) for the same
//! reason: a rect that only the widget can compute has to be republished by the
//! widget or nothing outside it can aim at it.
//!
//! An address that resolves to no rect this frame simply draws nothing. That is
//! the honest answer for a window that is scrolled off the strip, on another
//! workspace, or belongs to a peer — and it is why nothing here can invent a
//! position for something it cannot see.
//!
//! # Why this is not a synthesised cursor
//!
//! The obvious way to show an agent working is to fly a pointer across the
//! screen and click things. [`super::flight`] sets out at length why that would
//! be a regression — attribution, the capability gate, working without a
//! screen, and replay all depend on an agent acting through *addresses* rather
//! than through coordinates — and every word of it applies here.
//!
//! One consequence is worth repeating because it is the difference between
//! evidence and decoration: a synthesised gesture plays whether or not anything
//! happened at the end of it. What is drawn here is a consequence of an
//! invocation that already went through the door, so an operation that was
//! refused, or never made, marks nothing at all.
//!
//! # What it has to say in a frame
//!
//! Three things, to a viewer who was not told in advance:
//!
//! * **That somebody is acting** — hence motion. Four brackets converge onto
//!   the resource rather than a static outline appearing on it, because a still
//!   outline reads as selection and selection is something the *person* did.
//! * **What is being acted on** — hence the mark hugging the resource's own
//!   rect, not the window's.
//! * **That it was not the person** — hence the participant's colour and their
//!   name, in the very pill that already carries their pointer, their caret
//!   and an object they moved
//!   ([`draw_peer_pill`](super::render::draw_peer_pill), extended here with a
//!   leading glyph rather than reimplemented); and, for a participant the room
//!   says is an agent, the same [`Icon::Bot`](crate::icons::Icon::Bot) glyph
//!   their avatar wears in the bar. An agent is already told apart from a
//!   person there ([`Avatar::draw_agent_at`](crate::widget::Avatar::draw_agent_at)),
//!   and a viewer who has learned that mark once should not have to learn a
//!   second one here.

use std::time::{Duration, Instant};

use vello::kurbo::{Affine, Line, Rect, RoundedRect, Stroke};
use vello::Scene;

use crate::color_compat::ColorCompat;
use crate::permissions::UserId;
use crate::text_render::TextRenderer;
use crate::theme::Theme;

use super::render::color_for;
use super::PresenceCtx;

/// How long one touch stays on screen.
///
/// Longer than a crossing (720 ms), because a crossing is a movement a viewer
/// follows and this is a label a viewer reads. Two words and a name at the
/// size a chip is set in take about a second of dwell before they have been
/// read rather than merely seen, and the mark is worth nothing if it is gone
/// by then.
///
/// Short enough that consecutive operations still read as separate events:
/// three rotates in a row should be three marks, not one continuous glow.
pub const TOUCH_DURATION: Duration = Duration::from_millis(1500);

/// Fraction of the span spent converging, and spent releasing.
///
/// The middle is held at full strength, which is the part that is legible in
/// a still frame — a curve that is easing for its whole life has no instant
/// that represents it.
const REACH: f64 = 0.16;
const RELEASE: f64 = 0.22;

/// How far outside the resource the brackets start, in logical pixels.
const REACH_DISTANCE: f64 = 26.0;

/// Gap between the resource and the pill naming who is touching it.
const PILL_GAP: f64 = 4.0;

/// Length of one bracket arm, as a fraction of the shorter side, and the
/// bounds it is clamped into. A bracket that is nearly the whole edge is a
/// rectangle; one that is a few pixels is a speck.
const ARM_FRACTION: f64 = 0.22;
const ARM_MIN: f64 = 10.0;
const ARM_MAX: f64 = 34.0;

/// One resource a participant is acting on, right now.
///
/// Deliberately no geometry — the same reason [`super::flight::ActionFlight`]
/// carries no endpoints. Where a resource is on screen changes every frame the
/// strip pans, the window resizes or the app relayouts, and it is often the
/// *action itself* that moves it: cropping a photograph changes the rect the
/// crop was performed on. Coordinates frozen at invocation time would drift off
/// the thing they are about, so the address is stored and resolved per frame.
#[derive(Clone, Debug, PartialEq)]
pub struct ActionTouch {
    /// Who performed it. Drives the colour, so a viewer reads the same
    /// participant here, on their pointer and on an object they moved.
    pub actor: UserId,
    /// The resource half of the address — what is being acted on. Resolved
    /// against the frame's resource rects; an address nothing is showing draws
    /// nothing.
    pub resource: String,
    /// The action's display label as the app published it (`Crop`,
    /// `Rotate right`). Shown verbatim: the app owns its own vocabulary, and a
    /// label the shell reworded would stop matching the operation a caller
    /// enumerated.
    pub action_label: String,
    /// Whether the room says this participant is an agent rather than a
    /// person.
    ///
    /// Not inferred from the name, and not decided here: it is the same
    /// `is_agent` the roster carries and the bar's avatar already reads, so one
    /// fact drives both marks. A participant the room has no answer for is
    /// drawn as a person, which is the safe way round — claiming somebody is a
    /// bot is a worse error than not saying.
    pub is_agent: bool,
    /// When it landed. Progress is read off the clock, so a dropped frame
    /// costs smoothness rather than desynchronising the mark from the deed.
    pub started: Instant,
}

impl ActionTouch {
    pub fn new(
        actor: UserId,
        resource: impl Into<String>,
        action_label: impl Into<String>,
        is_agent: bool,
    ) -> Self {
        Self {
            actor,
            resource: resource.into(),
            action_label: action_label.into(),
            is_agent,
            started: Instant::now(),
        }
    }

    /// Linear progress at `now`, or `None` once the mark is over.
    ///
    /// `None` is what retires a touch — a caller drops the ones that answer it
    /// so the list cannot grow without bound.
    pub fn progress(&self, now: Instant) -> Option<f64> {
        let elapsed = now.saturating_duration_since(self.started);
        if elapsed >= TOUCH_DURATION {
            return None;
        }
        Some(elapsed.as_secs_f64() / TOUCH_DURATION.as_secs_f64())
    }
}

/// The mark's state at one instant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TouchPose {
    /// 0..=1. Fades in as the brackets arrive and out as they let go.
    pub alpha: f64,
    /// How far the brackets still are from the resource, in logical pixels.
    /// Zero for the whole held middle, so a still frame taken there shows the
    /// mark exactly on the thing.
    pub offset: f64,
}

/// Cubic ease-out — fast to arrive, slow to settle, which is what a hand
/// reaching for something does and what a value interpolating does not.
fn ease_out(t: f64) -> f64 {
    let f = 1.0 - t;
    1.0 - f * f * f
}

/// The pose at linear progress `t`.
///
/// A free function of its input so the curve can be checked without a clock, a
/// GPU or a compositor.
pub fn pose(t: f64) -> TouchPose {
    let t = t.clamp(0.0, 1.0);
    if t < REACH {
        let k = ease_out(t / REACH);
        TouchPose {
            alpha: k,
            offset: REACH_DISTANCE * (1.0 - k),
        }
    } else if t > 1.0 - RELEASE {
        let k = (1.0 - t) / RELEASE;
        TouchPose {
            alpha: k,
            // Lets go outward rather than shrinking away, so the last thing
            // seen is the resource uncovered rather than the mark collapsing
            // into it.
            offset: REACH_DISTANCE * 0.35 * (1.0 - k),
        }
    } else {
        TouchPose {
            alpha: 1.0,
            offset: 0.0,
        }
    }
}

/// Paint one participant's touch on `rect`, at linear progress `t`.
///
/// `rect` is this frame's answer to where the resource is, in the same
/// coordinate space the caller's `transform` maps — see [`ActionTouch`] on why
/// it is supplied rather than stored.
///
/// Every colour here is either the participant's identity colour or a theme
/// token. Nothing is a decoration this module chose: the colour comes from the
/// same presence machinery that colours that participant's pointer and caret,
/// so a viewer reads one person doing all of it.
#[allow(clippy::too_many_arguments)]
pub fn draw_action_touch(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    presence: &PresenceCtx,
    actor: &UserId,
    name: &str,
    action_label: &str,
    is_agent: bool,
    rect: Rect,
    t: f64,
) {
    if rect.width() <= 1.0 || rect.height() <= 1.0 {
        return;
    }
    let p = pose(t);
    if p.alpha <= 0.001 {
        return;
    }
    let a = p.alpha as f32;
    let peer = color_for(presence, actor, theme);
    let tokens = &theme.tokens;
    let radius = tokens.radii.md as f64;

    // The ring, hugging the resource. Thin, because it has to sit on top of a
    // photograph without becoming part of it.
    let ring = RoundedRect::from_rect(rect.inflate(p.offset * 0.4, p.offset * 0.4), radius);
    scene.stroke(
        &Stroke::new(2.0),
        transform,
        peer.with_alpha_factor(a * 0.9),
        None,
        &ring,
    );
    // A second, wider, fainter pass standing in for a glow — the same layered
    // trick the crossing's shadow uses, at a fraction of a real blur's cost.
    scene.stroke(
        &Stroke::new(6.0),
        transform,
        peer.with_alpha_factor(a * 0.16),
        None,
        &ring,
    );

    // Four brackets converging on the corners. This is the part that reads as
    // somebody reaching for the object rather than the object being outlined:
    // at `t = 0` they are outside it and travelling in.
    let arm = (rect.width().min(rect.height()) * ARM_FRACTION).clamp(ARM_MIN, ARM_MAX);
    let o = p.offset;
    let b = Rect::new(
        rect.x0 - o,
        rect.y0 - o,
        rect.x1 + o,
        rect.y1 + o,
    );
    let stroke = Stroke::new(3.0);
    let ink = peer.with_alpha_factor(a);
    for (corner, dx, dy) in [
        ((b.x0, b.y0), 1.0, 1.0),
        ((b.x1, b.y0), -1.0, 1.0),
        ((b.x0, b.y1), 1.0, -1.0),
        ((b.x1, b.y1), -1.0, -1.0),
    ] {
        let (cx, cy) = corner;
        scene.stroke(
            &stroke,
            transform,
            ink,
            None,
            &Line::new((cx, cy), (cx + dx * arm, cy)),
        );
        scene.stroke(
            &stroke,
            transform,
            ink,
            None,
            &Line::new((cx, cy), (cx, cy + dy * arm)),
        );
    }

    // Who, and what they are doing — in the pill a peer's pointer and their
    // caret already wear, painted by the same function that paints those.
    //
    // Not a chip of its own. The pill has a measured size, a shadow, a
    // contrast-picked ink and a type step chosen by looking at frames
    // (`pill_label_size`: `scale.sm`, because `xs` put people's names on
    // screen at about eight pixels of glyph). A second implementation here
    // would start by getting all four of those wrong, and this one is on
    // camera for longer than any of them.
    if name.is_empty() && action_label.is_empty() {
        return;
    }
    let text = if name.is_empty() {
        action_label.to_string()
    } else if action_label.is_empty() {
        name.to_string()
    } else {
        // Name first, because *who* is the fact a viewer needs before the
        // sentence means anything. The middle dot rather than a colon: a
        // colon reads as the app labelling a field, and this is one
        // participant's name against one operation's name.
        format!("{name} · {action_label}")
    };
    // The bot glyph leads the pill for an agent, and a person's pill has
    // none — the absence is the distinction, exactly as it is in the avatar
    // cluster in the bar.
    let glyph = if is_agent {
        Some(crate::icons::Icon::Bot)
    } else {
        None
    };
    let (chip_w, chip_h) =
        super::render::peer_pill_size_with_icon(text_renderer, theme, &text, glyph);
    // Above the resource by preference, below it when the resource is against
    // the top of its window — a pill drawn off the top of a tile is clipped by
    // it, which is the one placement guaranteed to be unreadable.
    let above = b.y0 - chip_h - PILL_GAP;
    let (y0, y1) = if above > 2.0 {
        (above, b.y0 - PILL_GAP)
    } else {
        (b.y1 + PILL_GAP, b.y1 + PILL_GAP + chip_h)
    };
    // Anchored to the resource, not to the brackets: the brackets start
    // outside it, and a pill that started out there too would be half off the
    // window while it converged — the one moment it most needs to be readable.
    let chip = Rect::new(rect.x0, y0, rect.x0 + chip_w, y1);
    super::render::draw_peer_pill_with_icon(
        scene, transform, text_renderer, theme, chip, &text, peer, a, glyph,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_touch_retires_itself() {
        let touch = ActionTouch::new(UserId("claude".into()), "photos.image:/a.png", "Crop", true);
        let now = touch.started;
        assert_eq!(touch.progress(now), Some(0.0));
        assert!(touch.progress(now + TOUCH_DURATION / 2).is_some());
        assert_eq!(touch.progress(now + TOUCH_DURATION), None);
        assert_eq!(touch.progress(now + TOUCH_DURATION * 2), None);
    }

    /// The middle of the span is held at rest. A capture taken there has to
    /// show the mark on the resource, not mid-curve towards it — otherwise no
    /// single frame ever depicts what the feature looks like.
    #[test]
    fn the_middle_is_held_on_the_resource() {
        let p = pose(0.5);
        assert_eq!(p.offset, 0.0);
        assert_eq!(p.alpha, 1.0);
    }

    /// It arrives from outside and lets go outward. Both ends are off the
    /// resource, which is what makes the motion read as reaching rather than
    /// as an outline switching on.
    #[test]
    fn it_reaches_in_and_lets_go() {
        let start = pose(0.0);
        assert!(start.offset > 0.0, "starts away from the resource");
        assert!(start.alpha < 0.01, "and invisible, so it fades in");
        let end = pose(1.0);
        assert!(end.alpha < 0.01, "and is gone by the end");
    }

    /// Monotonic fade in and out — no flicker anywhere in the span.
    #[test]
    fn alpha_rises_then_falls_without_flicker() {
        let mut prev = 0.0;
        let mut peaked = false;
        for i in 0..=100 {
            let a = pose(i as f64 / 100.0).alpha;
            if a + 1e-9 < prev {
                peaked = true;
            } else if peaked {
                assert!(
                    a <= prev + 1e-9,
                    "alpha rose again after falling at t={}",
                    i as f64 / 100.0
                );
            }
            prev = a;
        }
        assert!(peaked, "alpha must come back down");
    }
}
