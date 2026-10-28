use super::bool_control::{
    BoolControl, bool_control_api, bool_control_capabilities, bool_control_widget_methods,
};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect, Stroke};
use winit::event::{ElementState, MouseButton, WindowEvent};

/// A switch: a `BoolControl` drawn as a pill whose knob slides to the
/// checked side.
///
/// A click or a tap slides the knob and turns the track (a second click
/// mid-flight reverses from where it is drawn); a drag holds the knob under
/// the hand and lets it settle on the nearer side, carrying the release's
/// velocity; Space, Enter and an assistive action land at once
/// ([`SwitchMotion`]). Settled, it asks for no frames and repaints only
/// when what it paints changes.
pub struct Toggle {
    semantic_id: String,
    control: BoolControl,
    /// Where the knob is drawn and how far the track has turned on.
    switch: SwitchMotion,
    /// A pointer or a finger holding the switch.
    hold: Option<KnobHold>,
    /// The last pointer x, where a press takes hold.
    pointer_x: f64,
    /// The pill as last drawn, from the switch's origin, for a hand
    /// dragging its knob.
    drawn_track: Rect,
    /// The motion the person allows, as last drawn.
    motion: Option<crate::tokens::EffectiveMotion>,
    /// How far a held knob moves before the hold is a drag, the theme's
    /// click slop as last laid out: below it, a press is a click.
    slop: f64,
    /// What the last presented frame painted, and what the frame being drawn
    /// paints: the switch needs a redraw only when they differ.
    painted: Option<Painted>,
    drawing: Option<Painted>,
    /// A view that builds the switch anew on each change: the one built in
    /// its place goes on from its knob (`motion::keep`).
    hand_on: crate::motion::HandOn<KeptSwitch>,
}

/// What a switch leaves for the one a rebuild puts in its place: its knob,
/// and the value it held, so a successor showing another switch's value
/// (another page, a row that moved) does not slide.
struct KeptSwitch {
    switch: SwitchMotion,
    on: bool,
}

/// What a switch's paint depends on besides its place and the theme:
/// checked, hovered, focused, enabled, and the drawn knob position and
/// tint (as bits, so a settled switch compares equal).
type Painted = (bool, bool, bool, bool, u64, u32);

/// A hand holding a switch: the pointer, or a finger dragging across it
/// (which the touch emulator presents as the pointer).
#[derive(Clone, Copy, Debug)]
struct KnobHold {
    /// Where the hold started.
    start_x: f64,
    /// The knob's drawn position when the hold started.
    from: f64,
    dragged: bool,
    /// The last two knob positions the hand set, for its velocity.
    last: Option<(crate::time::Instant, f64)>,
    previous: Option<(crate::time::Instant, f64)>,
}

/// A hand's last movement older than this is no velocity at release: it
/// stopped before letting go.
const STILL_AFTER: crate::time::Duration = crate::time::Duration::from_millis(80);

impl KnobHold {
    fn new(x: f64, from: f64) -> Self {
        Self {
            start_x: x,
            from,
            dragged: false,
            last: None,
            previous: None,
        }
    }

    fn sample(&mut self, at: crate::time::Instant, position: f64) {
        self.previous = self.last.replace((at, position));
    }

    /// The knob's velocity (travel per second) when let go at `now`.
    fn velocity(&self, now: crate::time::Instant) -> f64 {
        match (self.previous, self.last) {
            (Some((t0, p0)), Some((t1, p1)))
                if now.saturating_duration_since(t1) <= STILL_AFTER =>
            {
                let dt = t1.saturating_duration_since(t0).as_secs_f64();
                if dt > 0.0 { (p1 - p0) / dt } else { 0.0 }
            }
            _ => 0.0,
        }
    }
}

impl Toggle {
    pub fn new(id: &str, on_toggle_path: &str) -> Self {
        Self {
            semantic_id: id.to_string(),
            control: BoolControl::new(on_toggle_path, Some(crate::widget::humanize_control_id(id))),
            switch: SwitchMotion::default(),
            hold: None,
            pointer_x: 0.0,
            drawn_track: Rect::ZERO,
            motion: None,
            slop: f64::from(crate::tokens::Metrics::BASE.click_slop),
            painted: None,
            drawing: None,
            hand_on: crate::motion::HandOn::default(),
        }
    }

    /// What names the switch across rebuilds: an id it was given, else
    /// nothing (it is known by its place).
    fn identity(&self) -> Option<&str> {
        self.control
            .id_override
            .as_deref()
            .or((self.semantic_id != "toggle").then_some(self.semantic_id.as_str()))
    }

    /// Go on from the switch a rebuild dropped: its knob, on its way or at
    /// rest. Showing what it showed, a change since moves from there, as
    /// the input that made it asks; showing something else, it rests.
    fn take_up(&mut self, kept: KeptSwitch) {
        self.switch = kept.switch;
        if kept.on != self.control.is_checked {
            self.switch.snap(self.control.is_checked);
        }
    }

    fn painted_now(&self) -> Painted {
        let (knob, tint) = self.switch.drawn(crate::motion::frame_now());
        (
            self.control.is_checked,
            self.control.is_hovered,
            self.control.is_focused,
            self.control.effectively_enabled(),
            knob.to_bits(),
            tint.to_bits(),
        )
    }

    fn effective_motion(&self) -> crate::tokens::EffectiveMotion {
        self.motion.unwrap_or_else(|| {
            crate::tokens::EffectiveMotion::new(crate::tokens::Motion::default(), false)
        })
    }

    /// How far the knob travels across the drawn pill, in px.
    fn knob_travel(&self) -> f64 {
        (self.drawn_track.width() - self.drawn_track.height()).max(1.0)
    }

    /// Whether `point` is on the drawn knob, grown to a finger's reach.
    fn on_knob(&self, point: Point) -> bool {
        if self.drawn_track.area() <= 0.0 {
            return false;
        }
        // Where the pill is now: a scroll moves the switch without always
        // drawing it again.
        let track = self.drawn_track + self.control.rect.origin().to_vec2();
        let knob = self.switch.knob(crate::motion::frame_now());
        let centre = Point::new(
            track.x0 + track.height() / 2.0 + knob * self.knob_travel(),
            track.center().y,
        );
        centre.distance(point) <= track.height()
    }

    /// The held knob follows the hand to `x`, once it has moved past the
    /// slop: 1:1 from where it was drawn when the hand took it, so it stays
    /// under the hand however far the slop let the hand go first (a
    /// finger's drag begins only past the touch slop).
    fn drag_to(&mut self, x: f64) {
        let travel = self.knob_travel();
        let Some(hold) = self.hold.as_mut() else {
            return;
        };
        if !hold.dragged {
            if (x - hold.start_x).abs() < self.slop {
                return;
            }
            hold.dragged = true;
        }
        let position = (hold.from + (x - hold.start_x) / travel).clamp(0.0, 1.0);
        hold.sample(crate::time::now(), position);
        self.switch.hold(position);
    }

    /// The hand lets go, and the value changes now, once, if at all. A
    /// press that did not drag is a click: it flips the switch when let go
    /// over it. A drag settles on the side its release heads for, with its
    /// velocity, and asks for that side only if it is not the value: a
    /// drag back to where it began changes nothing.
    fn let_go(&mut self) {
        let Some(hold) = self.hold.take() else {
            return;
        };
        if !hold.dragged {
            if self.control.is_hovered {
                self.control.flip();
            }
            return;
        }
        let motion = self.effective_motion();
        let now = crate::motion::frame_now();
        let velocity = hold.velocity(crate::time::now());
        let position = self.switch.knob(now);
        let side = crate::motion::Snap::rest_for(position, velocity, motion) >= 0.5;
        if side != self.control.is_checked {
            self.control.choose(side);
        }
        self.switch.release(side, velocity, motion, now);
    }

    /// Create a true two-way binding to typed process-local boolean state.
    ///
    /// Pointer, keyboard, and accessibility activation update `state`;
    /// mutations through any clone of `state` update this switch on the next
    /// synchronization pass. Use [`Self::on_change`] only to observe user
    /// changes, not to keep the model in sync.
    pub fn from_state(state: &crate::LocalState<bool>) -> Self {
        let mut toggle = Self::new("toggle", "");
        toggle.control =
            BoolControl::local(state, Some(crate::widget::humanize_control_id("toggle")));
        toggle
    }

    bool_control_api!();
}

/// Interaction state for the immediate-mode toggle face; the retained
/// [`Toggle`] tracks these itself. A canvas app doing its own hit-testing
/// usually only knows `hovered`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ToggleInteract {
    pub hovered: bool,
    pub focused: bool,
    pub disabled: bool,
}

/// The switch's visual footprint: a 44×24 pill centred in whatever target
/// rect layout assigns (32 px tall at a fine pointer, 48 at a coarse one), so
/// the pill itself never stretches. Off the spacing scale on purpose: 24 is
/// the 20 px knob plus [`KNOB_EDGE_GAP`] either side, and 44 gives the knob
/// exactly one diameter of travel.
const TRACK_WIDTH: f32 = 44.0;
const TRACK_HEIGHT: f32 = 24.0;
/// Gap between knob and track edge. The knob radius is derived from the
/// track height and this gap, so a host painting a taller face keeps the
/// same proportions.
const KNOB_EDGE_GAP: f64 = 2.0;

/// The pill's rect inside a layout target: centred, never stretched, at
/// the theme's size.
fn track_rect(target: Rect, theme: &Theme) -> Rect {
    Rect::from_center_size(target.center(), ToggleFace::size(theme))
}

/// The resolved colours of one switch face.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ToggleColors {
    /// The track: the inverted fill when on, a lift of the ground when off.
    pub track: vello::peniko::Color,
    pub knob: vello::peniko::Color,
    /// A ring round the knob where the knob is a raised object rather than
    /// a glyph of its track (off, on a light material).
    pub knob_edge: Option<vello::peniko::Color>,
    /// The track's rest edge, or your colour when focused.
    pub edge: vello::peniko::Color,
    pub edge_width: f64,
}

impl ToggleColors {
    /// What outlines the knob against the track: its ring where it has one,
    /// otherwise its own fill.
    pub fn knob_outline(&self) -> vello::peniko::Color {
        self.knob_edge.unwrap_or(self.knob)
    }

    /// The face `tint` of the way from `off` to `on` (a switch turning):
    /// exactly one or the other at either end.
    #[must_use]
    pub fn between(off: Self, on: Self, tint: f32) -> Self {
        if tint <= 0.0 {
            return off;
        }
        if tint >= 1.0 {
            return on;
        }
        let mix = |a, b| crate::tokens::colors::mix8(a, b, tint);
        Self {
            track: mix(off.track, on.track),
            knob: mix(off.knob, on.knob),
            knob_edge: match (off.knob_edge, on.knob_edge) {
                (None, None) => None,
                (from, to) => Some(mix(from.unwrap_or(off.knob), to.unwrap_or(on.knob))),
            },
            edge: mix(off.edge, on.edge),
            edge_width: off.edge_width + (on.edge_width - off.edge_width) * f64::from(tint),
        }
    }
}

/// How a switch is drawn this frame: where its knob is along its travel
/// (`0` off, `1` on) and how far its track has turned on (`0..=1`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SwitchPose {
    pub knob: f64,
    pub tint: f32,
}

impl SwitchPose {
    /// A switch at rest, on or off.
    #[must_use]
    pub fn at_rest(on: bool) -> Self {
        Self {
            knob: if on { 1.0 } else { 0.0 },
            tint: if on { 1.0 } else { 0.0 },
        }
    }
}

/// The motion of one switch: the retained [`Toggle`] keeps one, and so does
/// any host that paints switches itself ([`ToggleFace::pose`]), one per
/// switch it shows.
///
/// A change a click or a finger caused slides the knob and turns the track
/// (`motion::ControlChange`, `fast`, critically damped), and a second one
/// mid-flight reverses from where it is drawn. A key, an assistive action or
/// a value nobody just caused (a sync) lands at once. Reduced motion lands
/// the knob and still cross-fades the tint. The first value shown rests.
/// Settled, it asks for no frames.
#[derive(Clone, Copy, Debug, Default)]
pub struct SwitchMotion {
    knob: crate::motion::Thumb,
    tint: crate::motion::Mark,
    shown: Option<bool>,
}

impl SwitchMotion {
    /// Show `on`, moving as `source` asks. The first value shown rests.
    #[track_caller]
    pub fn follow(
        &mut self,
        on: bool,
        motion: crate::tokens::EffectiveMotion,
        source: crate::tokens::MotionSource,
        now: crate::time::Instant,
    ) {
        match self.shown {
            Some(shown) if shown == on => {}
            None => self.snap(on),
            Some(_) => {
                self.shown = Some(on);
                self.knob.go(knob_end(on), motion, source, now);
                self.tint.set(on, motion, source, now);
            }
        }
    }

    /// Show `on` as the input the person just made caused it
    /// (`motion::recent_input`): a click slides, a key lands, and a change
    /// nobody just caused (a sync, another device) lands.
    #[track_caller]
    pub fn follow_input(
        &mut self,
        on: bool,
        motion: crate::tokens::EffectiveMotion,
        now: crate::time::Instant,
    ) {
        let source =
            crate::motion::recent_input(now).unwrap_or(crate::tokens::MotionSource::Action);
        self.follow(on, motion, source, now);
    }

    /// Rest on `on` at once.
    pub fn snap(&mut self, on: bool) {
        self.shown = Some(on);
        self.knob.snap(knob_end(on));
        self.tint.snap(on);
    }

    /// The hand holds the knob at `knob` (`0..=1` of its travel): drawn
    /// exactly there.
    pub fn hold(&mut self, knob: f64) {
        self.knob.track(knob);
    }

    /// The hand let go: settle on `on`, carrying its `velocity` (travel per
    /// second).
    #[track_caller]
    pub fn release(
        &mut self,
        on: bool,
        velocity: f64,
        motion: crate::tokens::EffectiveMotion,
        now: crate::time::Instant,
    ) {
        self.shown = Some(on);
        self.knob.release(knob_end(on), velocity, motion, now);
        self.tint
            .set(on, motion, crate::tokens::MotionSource::Gesture, now);
    }

    /// Where the knob is drawn at `now`, `0..=1` of its travel.
    #[track_caller]
    pub fn knob(&self, now: crate::time::Instant) -> f64 {
        self.knob.get(now)
    }

    /// The knob and the cross-faded tint at `now`, whatever the motion.
    #[track_caller]
    fn drawn(&self, now: crate::time::Instant) -> (f64, f32) {
        (self.knob.get(now), self.tint.level(now))
    }

    /// How to draw the switch at `now`: the tint follows the knob, or
    /// cross-fades on its own where reduced motion lands the knob.
    #[track_caller]
    #[must_use]
    pub fn pose(
        &self,
        motion: crate::tokens::EffectiveMotion,
        now: crate::time::Instant,
    ) -> SwitchPose {
        let knob = self.knob.get(now);
        let tint = if motion.reduced() {
            self.tint.level(now)
        } else {
            crate::motion::unit_opacity(knob)
        };
        SwitchPose { knob, tint }
    }

    /// Whether the switch is still on its way at `now`.
    #[track_caller]
    pub fn is_moving(&self, now: crate::time::Instant) -> bool {
        self.knob.is_moving(now) || self.tint.is_moving(now)
    }
}

fn knob_end(on: bool) -> f64 {
    if on { 1.0 } else { 0.0 }
}

/// Single source of truth for both the retained [`Toggle`]'s draw and
/// [`Toggle::paint`], so every switch shares one palette per state.
///
/// On, the switch is an inverted control: the track is `intent.accent` and
/// the knob its label colour (`intent.on_accent`). Off, the track is a lift
/// of the ground (HIG §9.2, §9.5 "Rest edges"). On a light material the off
/// knob is a raised white object with a derived ring, never a dark disc; on a
/// dark one it is the secondary ink, and hover lifts it to the primary ink.
/// Focus is your colour round the track.
pub fn toggle_face_colors(theme: &Theme, on: bool, interact: ToggleInteract) -> ToggleColors {
    let colors = &theme.tokens.colors;
    let floor = crate::tokens::colors::NON_TEXT_MIN_CONTRAST;
    // A lift of the window ground: lighter on a dark material, darker on a
    // light one, so a white knob always stands out from it.
    let off_track = colors.lift_on(colors.surfaces.content, 1.0);
    // On is green, the colour every person reads as "on": an inverted
    // (white) track on a black-and-white surface didn't read as on at all.
    let track = if on {
        colors.intent.on_state
    } else {
        off_track
    };
    // Hover is the same knob pressed harder: one contrast step (1:1)
    // further from the track, never across it.
    let harder = |tone: vello::peniko::Color| {
        let reached = worst_contrast(tone, &[track]);
        ink_across(&[track], tone, reached + 1.0)
    };
    let (knob, knob_edge) = if on {
        // The knob is white; at rest it holds back a touch so the pointer
        // can make it fully white, never darker: a darker knob on the green
        // track read as off.
        let mark = colors.intent.on_state_mark;
        let knob = if interact.hovered {
            mark
        } else {
            crate::tokens::colors::mix8(track, mark, 0.9)
        };
        (ink_across(&[track], knob, floor), None)
    } else if light_material(colors) {
        let ring = ink_across(&[track], colors.text_tertiary, floor);
        (
            colors.surfaces.overlay,
            Some(if interact.hovered { harder(ring) } else { ring }),
        )
    } else if interact.hovered {
        (ink_across(&[track], colors.text_primary, floor), None)
    } else {
        (ink_across(&[track], colors.text_dim, floor), None)
    };
    let (edge, edge_width) = if interact.focused {
        (colors.intent.accent_ink, 2.0)
    } else {
        (crate::surface::edge_on(track), 1.0)
    };
    ToggleColors {
        track,
        knob,
        knob_edge,
        edge,
        edge_width,
    }
}

/// Whether `colors` is a light material (dark ink on a light window
/// ground), where a knob is a raised white object rather than an ink glyph.
pub(crate) fn light_material(colors: &crate::tokens::colors::Colors) -> bool {
    crate::a11y::relative_luminance(colors.surfaces.content) > 0.5
}

/// The lowest contrast `ink` reaches over any of `grounds`.
pub(crate) fn worst_contrast(ink: vello::peniko::Color, grounds: &[vello::peniko::Color]) -> f64 {
    grounds
        .iter()
        .map(|g| crate::a11y::contrast_ratio(ink, *g))
        .fold(f64::INFINITY, f64::min)
}

/// One `tone` made legible on **every** ground it rides across — a knob
/// over an off and an on track, a thumb over the filled and unfilled halves
/// of a slider. `tone` comes back untouched when it already clears
/// `min_ratio` everywhere; otherwise it is stepped toward the extreme whose
/// *worst-case* contrast over all grounds is highest.
///
/// That choice is made over the whole set on purpose. A pair can straddle
/// the light/dark line — a white off-track under a mid-blue on-track on the
/// light preset — and [`crate::tokens::colors::Colors::ink_on_any`] picks
/// its polarity from the hardest ground alone, which there is the blue, so
/// it walks the knob toward white and straight into the other track. The
/// extreme itself is theme-free: [`Colors::hairline_on`] at full alpha is
/// white or black, whichever moves that ground more.
pub(crate) fn ink_across(
    grounds: &[vello::peniko::Color],
    tone: vello::peniko::Color,
    min_ratio: f64,
) -> vello::peniko::Color {
    use crate::tokens::colors::{Colors, mix8};
    let Some(first) = grounds.first() else {
        return tone;
    };
    if worst_contrast(tone, grounds) >= min_ratio {
        return tone;
    }
    // Both extremes, derived rather than spelled: `hairline_on` at full
    // alpha is white or black for one ground, and the same call on that
    // result is the other one.
    let one = Colors::hairline_on(*first, u8::MAX);
    let other = Colors::hairline_on(one, u8::MAX);
    let target = if worst_contrast(one, grounds) >= worst_contrast(other, grounds) {
        one
    } else {
        other
    };
    for step in 1..=16 {
        #[allow(
            clippy::cast_precision_loss,
            reason = "step ranges 1..=16, far below where f32 loses integer precision"
        )]
        let candidate = mix8(tone, target, step as f32 / 16.0);
        if worst_contrast(candidate, grounds) >= min_ratio {
            return candidate;
        }
    }
    target
}

pub struct ToggleFace {
    rect: Rect,
    interact: ToggleInteract,
    pose: SwitchPose,
}

impl ToggleFace {
    pub fn new(rect: Rect, on: bool) -> Self {
        Self {
            rect,
            interact: ToggleInteract::default(),
            pose: SwitchPose::at_rest(on),
        }
    }
    /// Draw the switch where its [`SwitchMotion`] is this frame rather than
    /// at rest on its value.
    #[must_use]
    pub fn pose(mut self, pose: SwitchPose) -> Self {
        self.pose = pose;
        self
    }
    #[must_use]
    pub fn hovered(mut self, v: bool) -> Self {
        self.interact.hovered = v;
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
    pub fn colors(&self, theme: &Theme) -> ToggleColors {
        let tint = self.pose.tint;
        if tint >= 1.0 {
            return toggle_face_colors(theme, true, self.interact);
        }
        if tint <= 0.0 {
            return toggle_face_colors(theme, false, self.interact);
        }
        ToggleColors::between(
            toggle_face_colors(theme, false, self.interact),
            toggle_face_colors(theme, true, self.interact),
            tint,
        )
    }
    /// The switch's `(width, height)` in `theme`: size a painted host's
    /// rect with it, so a painted switch matches the retained one.
    #[must_use]
    pub fn size(theme: &Theme) -> (f64, f64) {
        let scale = f64::from(theme.ui_scale);
        (
            f64::from(TRACK_WIDTH) * scale,
            f64::from(TRACK_HEIGHT) * scale,
        )
    }
}

/// Build the accessibility node shared by retained toggles and
/// context-specific toggle controls that own a different mutation channel.
pub fn toggle_accessibility(
    on: bool,
    focused: bool,
    enabled: bool,
    accessible_name: Option<&str>,
) -> crate::a11y::AccessNode {
    use crate::a11y::{AccessNode, AccessState, Role};
    AccessNode::new(Role::Switch)
        .with_optional_name(
            accessible_name
                .map(|name| crate::i18n!(name))
                .filter(|name| !name.is_empty()),
        )
        .with_state(
            AccessState::new()
                .focusable(enabled)
                .focused(focused)
                .disabled(!enabled)
                .checked(on),
        )
}

impl Toggle {
    /// Paints a themed switch inside `face.rect`, matching the retained
    /// [`Toggle`]'s look. Callers keep their own hit-testing; only the
    /// palette ([`toggle_face_colors`]) is shared.
    pub fn paint(scene: &mut Scene, transform: Affine, theme: &Theme, face: &ToggleFace) {
        let colors = face.colors(theme);
        let radius = face.rect.height() / 2.0;
        let track_shape = RoundedRect::from_rect(face.rect, radius);
        let dim = face.interact.disabled;
        if dim {
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                crate::widget::DISABLED_ALPHA,
                transform,
                &track_shape,
            );
        }
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            colors.track,
            None,
            &track_shape,
        );
        // Knob radius follows the track: 10 px inside the 24 px default.
        let kr = (radius - KNOB_EDGE_GAP).max(1.0);
        let cy = face.rect.y0 + face.rect.height() / 2.0;
        let (off_x, on_x) = (
            face.rect.x0 + kr + KNOB_EDGE_GAP,
            face.rect.x1 - kr - KNOB_EDGE_GAP,
        );
        let cx = match face.pose.knob {
            knob if knob >= 1.0 => on_x,
            knob if knob <= 0.0 => off_x,
            knob => off_x + (on_x - off_x) * knob,
        };
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            colors.knob,
            None,
            &Circle::new(Point::new(cx, cy), kr),
        );
        if let Some(ring) = colors.knob_edge {
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                ring,
                None,
                &Circle::new(Point::new(cx, cy), kr - 0.5),
            );
        }
        if dim {
            scene.pop_layer();
        }
        // Rest edge and focus ring share one half-pixel-inset outline, drawn
        // outside the disabled layer so a disabled switch keeps its bounds
        // at full strength (HIG §9.5).
        scene.stroke(
            &Stroke::new(colors.edge_width),
            transform,
            colors.edge,
            None,
            &crate::surface::edge_outline(face.rect, radius),
        );
    }
}

impl Widget for Toggle {
    bool_control_widget_methods!("toggle");

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.slop = f64::from(theme.tokens.metrics.click_slop);
        let (track_w, track_h) = ToggleFace::size(theme);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a switch is a few dozen logical pixels, far inside f32"
        )]
        let (track_w, track_h) = (track_w as f32, track_h as f32);
        let target = self.control.interaction.minimum_target();
        let style = Style {
            size: Size::from_lengths(track_w.max(target), track_h.max(target)),
            // A switch is a fixed target, like a button: in a row that also
            // holds a percent-width field it must not be the item that gives.
            // With the default shrink factor and no measured minimum, taffy
            // squeezed it to zero width beside a stretching text field.
            flex_shrink: 0.0,
            ..Default::default()
        };
        let identity = self.identity().map(str::to_owned);
        if let Some(kept) = self.hand_on.build(identity.as_deref()) {
            self.take_up(kept);
        }
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.control.rect = crate::widget::layout_rect(taffy, node, origin);
        if let Some(kept) = self.hand_on.place(self.control.rect) {
            self.take_up(kept);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        let now = crate::motion::frame_now();
        let motion = theme.motion();
        self.motion = Some(motion);
        // A knob under a dragging hand stays there; otherwise the switch
        // goes to its value as the input that changed it asks.
        if !self.hold.is_some_and(|hold| hold.dragged) {
            self.switch
                .follow_input(self.control.is_checked, motion, now);
        }
        self.drawing = Some(self.painted_now());
        // One painter for the retained widget and immediate-mode hosts, so
        // the two cannot drift: the pill is centred in the layout target and
        // every state resolves through the same `ToggleFace`.
        let c = &self.control;
        let track = track_rect(c.rect, theme);
        self.drawn_track = track - c.rect.origin().to_vec2();
        Self::paint(
            scene,
            transform,
            theme,
            &ToggleFace::new(track, c.is_checked)
                .pose(self.switch.pose(motion, now))
                .hovered(c.is_hovered)
                .focused(c.focus_visible())
                .disabled(!c.effectively_enabled()),
        );
        crate::widget::draw_gate_and_peer_focus(self, scene, transform, theme, presence);
    }

    /// A fixed size: only the first layout (and a theme change, which
    /// rebuilds the tree) needs one.
    fn is_layout_dirty(&self) -> bool {
        self.control.rect == Rect::ZERO
    }

    fn name(&self) -> &'static str {
        "Toggle"
    }

    /// A switch repaints when what it paints changed since the frame on
    /// screen, not on every frame: a settings page of them does not damage
    /// every switch for a scroll or a keystroke elsewhere. A knob on its way
    /// changes what it paints each frame; a settled one does not.
    fn needs_redraw(&self) -> bool {
        self.painted != Some(self.painted_now())
    }

    /// Frames only while the knob or the tint is on its way.
    fn needs_animation_frame(&self) -> bool {
        self.switch.is_moving(crate::motion::frame_now())
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if !self.control.effectively_enabled() {
            // A knob a hand left part-way goes back to the value.
            if self.hold.take().is_some_and(|hold| hold.dragged) {
                self.switch.snap(self.control.is_checked);
            }
            return;
        }
        let pointer_held = self.hold.is_some();
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let point = ctx.logical(position);
                self.pointer_x = point.x;
                if pointer_held {
                    self.control.is_hovered = self.control.rect.contains(point);
                    self.drag_to(point.x);
                    *ctx.handled = true;
                } else {
                    self.control.handle_event(event, ctx);
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.control.is_hovered => {
                // The press takes hold and changes nothing yet: let go, it
                // is a click; moved, the hand takes the knob.
                *ctx.handled = true;
                self.control.pressed_into_focus |= !self.control.is_focused;
                *ctx.focus_target = Some(ctx.node_id);
                let drawn = self.switch.knob(crate::motion::frame_now());
                self.hold = Some(KnobHold::new(self.pointer_x, drawn));
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if pointer_held => {
                *ctx.handled = true;
                self.let_go();
            }
            _ => self.control.handle_event(event, ctx),
        }
    }

    /// A finger on the knob drags it once it heads across; heading up or
    /// down it scrolls the page, as anywhere else on the switch, where a
    /// tap is a click.
    fn touch_claim(&self, point: Point) -> crate::interaction::TouchClaim {
        if self.control.effectively_enabled() && self.on_knob(point) {
            crate::interaction::TouchClaim::DragAcross
        } else {
            crate::interaction::TouchClaim::Pointer
        }
    }

    /// A held knob keeps the hand that holds it, wherever it goes.
    fn wants_pointer_capture(&self) -> bool {
        self.hold.is_some()
    }

    fn mark_drawn(&mut self) {
        if let Some(drawing) = self.drawing.take() {
            self.painted = Some(drawing);
        }
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        let mut node = toggle_accessibility(
            self.control.is_checked,
            self.is_focused(),
            self.is_enabled(),
            self.control.accessible_name.as_deref(),
        )
        .with_semantic_key(&self.semantic_id);
        if self.control.is_bare() {
            node = node.with_action_target(self.control.test_id("toggle"));
        }
        Some(node)
    }
}

bool_control_capabilities!(Toggle, "toggle");

/// A switch a rebuild drops leaves its knob for the one put in its place.
impl Drop for Toggle {
    fn drop(&mut self) {
        let switch = std::mem::take(&mut self.switch);
        let on = self.control.is_checked;
        self.hand_on.leave(KeptSwitch { switch, on });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Dispatch;
    use crate::widget::Widget;
    use automerge::{AutoCommit, ReadDoc, transaction::Transactable};
    use winit::event::{ElementState, MouseButton, WindowEvent};

    #[test]
    fn toggle_identity_enabled_state_and_accessibility_are_reported() {
        {
            let t = Toggle::new("any", "path");
            assert_eq!(Widget::test_id(&t), Some("toggle"));
        }

        {
            let t = Toggle::new("any", "path").with_id("airplane_mode");
            assert_eq!(Widget::test_id(&t), Some("airplane_mode"));
        }

        {
            let t = Toggle::new("any", "path").with_enabled(false);
            assert!(!Widget::is_enabled(&t));
            assert!(
                Widget::is_enabled(&Toggle::new("any", "path")),
                "default is enabled"
            );
        }

        {
            use crate::a11y::Role;
            let mut t = Toggle::new("any", "path");
            let off = Widget::accessibility(&t).unwrap();
            assert_eq!(off.role, Role::Switch);
            assert_eq!(off.state.checked, Some(false));

            t.control.is_checked = true; // same module → set the synced on-state directly
            let on = Widget::accessibility(&t).unwrap();
            assert_eq!(on.state.checked, Some(true));
        }

        {
            let plain = Toggle::new("any", "path");
            assert_eq!(
                Widget::accessibility(&plain).unwrap().name.as_deref(),
                Some("Any")
            );

            let named = Toggle::new("any", "path").with_accessible_name("Airplane mode");
            assert_eq!(
                Widget::accessibility(&named).unwrap().name.as_deref(),
                Some("Airplane mode"),
            );
        }
    }

    #[test]
    fn toggle_face_paints_documented_geometry_and_state_colours() {
        // Case `retained_draw_paints_the_documented_pill_geometry`.
        // The retained widget paints exactly the documented switch — a 44×24
        // pill centred in its layout target (never stretched to the 32/48 px
        // target), a 10 px knob 2 px inside the pill's edge, the shared palette,
        // and the disabled layer over the pill — spelled out here in literals so
        // a change to the painter *or* its constants fails against this, not
        // against itself.
        {
            let theme = Theme::dark();
            let target = Rect::new(20.0, 30.0, 64.0, 62.0);
            let pill = Rect::new(20.0, 34.0, 64.0, 58.0);
            assert_eq!(
                track_rect(target, &theme),
                pill,
                "44×24 pill centred in the 44×32 target"
            );
            let pill_shape = RoundedRect::from_rect(pill, 12.0);

            for (on, hovered, focused, enabled) in [
                (true, false, false, true),
                (false, true, false, true),
                (true, false, true, true),
                (false, false, false, false),
            ] {
                let mut toggle = Toggle::new("wifi", "wifi");
                toggle.control.is_checked = on;
                toggle.control.is_hovered = hovered;
                toggle.control.is_focused = focused;
                toggle.control.enabled = enabled;
                toggle.control.rect = target;
                let mut actual = Scene::new();
                let mut text = TextRenderer::new();
                toggle.draw(
                    &mut actual,
                    Affine::IDENTITY,
                    &mut text,
                    &theme,
                    &crate::presence::PresenceCtx::default(),
                );

                let interact = ToggleInteract {
                    hovered,
                    focused,
                    disabled: !enabled,
                };
                let colors = toggle_face_colors(&theme, on, interact);
                let mut expected = Scene::new();
                if !enabled {
                    expected.push_layer(
                        vello::peniko::Fill::NonZero,
                        vello::peniko::BlendMode::default(),
                        crate::widget::DISABLED_ALPHA,
                        Affine::IDENTITY,
                        &pill_shape,
                    );
                }
                expected.fill(
                    vello::peniko::Fill::NonZero,
                    Affine::IDENTITY,
                    colors.track,
                    None,
                    &pill_shape,
                );
                let knob_x = if on { pill.x1 - 12.0 } else { pill.x0 + 12.0 };
                expected.fill(
                    vello::peniko::Fill::NonZero,
                    Affine::IDENTITY,
                    colors.knob,
                    None,
                    &Circle::new(Point::new(knob_x, 46.0), 10.0),
                );
                if let Some(ring) = colors.knob_edge {
                    expected.stroke(
                        &Stroke::new(1.0),
                        Affine::IDENTITY,
                        ring,
                        None,
                        &Circle::new(Point::new(knob_x, 46.0), 9.5),
                    );
                }
                if !enabled {
                    expected.pop_layer();
                }
                // The edge (and the focus ring) ride the shared half-pixel-inset
                // outline, so a 1 px hairline covers the pill's outermost pixel
                // row instead of bleeding half onto the ground. It sits
                // outside the disabled layer, at full strength.
                expected.stroke(
                    &Stroke::new(colors.edge_width),
                    Affine::IDENTITY,
                    colors.edge,
                    None,
                    &crate::surface::edge_outline(pill, 12.0),
                );

                let state =
                    format!("on={on} hovered={hovered} focused={focused} enabled={enabled}");
                assert_eq!(
                    actual.encoding().path_data,
                    expected.encoding().path_data,
                    "pill/knob geometry differs for {state}"
                );
                assert_eq!(
                    actual.encoding().draw_data,
                    expected.encoding().draw_data,
                    "paint order or colours differ for {state}"
                );
            }
        }

        {
            let theme = Theme::dark();
            let off = toggle_face_colors(&theme, false, ToggleInteract::default());
            let on = toggle_face_colors(&theme, true, ToggleInteract::default());
            assert_eq!(
                on.track, theme.tokens.colors.intent.on_state,
                "on-track is the on colour"
            );
            assert_eq!(
                off.track,
                theme
                    .tokens
                    .colors
                    .lift_on(theme.tokens.colors.surfaces.content, 1.0),
                "off-track is the neutral lift off the window ground, not a ramp slot"
            );
            assert_ne!(on.track, off.track, "on and off tracks must differ");
        }

        // Case `toggle_knob_is_white_on_and_a_raised_object_off_in_light`.
        // On, the switch is the on colour with a white knob. Off, a light material shows a raised white knob with
        // a derived ring (never a dark disc); a dark one the secondary ink.
        // Legible on every material either way.
        {
            for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
                let colors = &theme.tokens.colors;
                let off = toggle_face_colors(&theme, false, ToggleInteract::default());
                let on = toggle_face_colors(&theme, true, ToggleInteract::default());
                let white = toggle_face_colors(
                    &theme,
                    true,
                    ToggleInteract {
                        hovered: true,
                        ..ToggleInteract::default()
                    },
                );
                assert_eq!(white.knob, colors.intent.on_state_mark, "{}", theme.name);
                assert!(
                    crate::a11y::contrast_ratio(on.knob, on.track)
                        >= crate::tokens::colors::NON_TEXT_MIN_CONTRAST,
                    "{}",
                    theme.name
                );
                assert_eq!(on.knob_edge, None);
                if light_material(colors) {
                    assert_eq!(off.knob, colors.surfaces.overlay, "{}", theme.name);
                    assert!(
                        crate::a11y::relative_luminance(off.knob)
                            > crate::a11y::relative_luminance(off.track),
                        "{}: the off knob is lighter than its track",
                        theme.name
                    );
                } else {
                    assert_eq!(off.knob, colors.text_dim, "{}", theme.name);
                    assert_eq!(off.knob_edge, None);
                }
                for (state, face) in [("off", off), ("on", on)] {
                    let ratio = crate::a11y::contrast_ratio(face.knob_outline(), face.track);
                    assert!(
                        ratio >= crate::tokens::colors::NON_TEXT_MIN_CONTRAST,
                        "{} {state}: knob outline on its track is only {ratio:.2}:1",
                        theme.name
                    );
                }
                assert_eq!(
                    off.edge,
                    crate::surface::edge_on(off.track),
                    "{}: rest edge derives from the track",
                    theme.name
                );
            }
        }

        {
            let theme = Theme::high_contrast();
            let rest = toggle_face_colors(&theme, true, ToggleInteract::default());
            let focused = toggle_face_colors(
                &theme,
                true,
                ToggleInteract {
                    focused: true,
                    ..Default::default()
                },
            );
            assert_eq!(
                focused.edge, theme.tokens.colors.intent.accent_ink,
                "focus is your colour"
            );
            assert_eq!(focused.edge_width, 2.0, "focused border is 2px");
            assert_eq!(
                rest.edge,
                crate::surface::edge_on(rest.track),
                "rest edge is derived from the track it sits on"
            );
            assert_eq!(rest.edge_width, 1.0);
        }

        {
            let theme = Theme::dark();
            let face = ToggleFace::new(Rect::new(0.0, 0.0, 44.0, 24.0), true).hovered(true);
            assert_eq!(
                face.colors(&theme),
                toggle_face_colors(
                    &theme,
                    true,
                    ToggleInteract {
                        hovered: true,
                        ..Default::default()
                    }
                ),
            );
        }
    }

    #[test]
    fn toggle_bindings_route_activation_unless_disabled() {
        {
            use crate::a11y::Action;

            let state = crate::LocalState::new(false);
            let changes = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let captured = std::rc::Rc::clone(&changes);
            let mut first = Toggle::from_state(&state)
                .on_change(move |checked| captured.borrow_mut().push(checked));
            let mut second = Toggle::from_state(&state);
            let mut document = AutoCommit::new();

            state.set(true);
            Widget::sync(&mut first, &document, &automerge::ROOT);
            Widget::sync(&mut second, &document, &automerge::ROOT);
            assert!(first.is_checked());
            assert!(second.is_checked());
            assert!(changes.borrow().is_empty(), "model sync is not user intent");

            assert!(Widget::perform_action(
                &mut first,
                &Action::Activate,
                &[],
                &mut crate::widget::Edits::lend(&mut document),
                &automerge::ROOT,
            ));
            assert!(!state.get(), "user activation publishes typed state");
            assert_eq!(&*changes.borrow(), &[false]);

            Widget::sync(&mut second, &document, &automerge::ROOT);
            assert!(!second.is_checked(), "sibling follows the shared model");

            second.set_checked(true);
            assert!(state.get(), "programmatic mutation publishes typed state");
            Widget::sync(&mut first, &document, &automerge::ROOT);
            assert!(first.is_checked());
            assert_eq!(
                &*changes.borrow(),
                &[false],
                "programmatic mutation does not invoke the user callback"
            );
        }

        {
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let mut toggle = Toggle::new("any", "enabled");
            doc.put(&root, "enabled", true).unwrap();
            Widget::sync(&mut toggle, &doc, &root);
            assert!(toggle.is_checked());

            doc.delete(&root, "enabled").unwrap();
            Widget::sync(&mut toggle, &doc, &root);
            assert!(!toggle.is_checked(), "a removed path means off");

            doc.put(&root, "enabled", "not-a-boolean").unwrap();
            Widget::sync(&mut toggle, &doc, &root);
            assert!(!toggle.is_checked(), "a mistyped path cannot stay stale");
        }

        {
            let mut t = Toggle::new("id", "on").with_enabled(false);

            let mut tree = TaffyTree::<()>::new();
            let mut node_map = std::collections::HashMap::new();
            let theme = Theme::dark();
            let node = t.build_layout(&mut tree, &theme, &mut node_map);
            tree.compute_layout(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(60.0),
                    height: taffy::AvailableSpace::Definite(40.0),
                },
            )
            .unwrap();
            t.apply_layout(&tree, node, Point::ZERO);

            let device = winit::event::DeviceId::dummy();
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let mut sender = Dispatch::new().doc(&mut doc).layout(&tree, node);
            let moved = sender.send(
                &mut t,
                &WindowEvent::CursorMoved {
                    device_id: device,
                    position: (10.0, 10.0).into(),
                },
            );
            let pressed = sender.send(
                &mut t,
                &WindowEvent::MouseInput {
                    device_id: device,
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                },
            );
            let focus = pressed.focus_target.or(moved.focus_target);
            let handled = moved.handled || pressed.handled;

            assert!(focus.is_none(), "disabled toggle must not request focus");
            assert!(!handled, "disabled toggle must not swallow input");
            assert!(
                doc.get(&root, "on").unwrap().is_none(),
                "disabled toggle must not write the CRDT path"
            );
        }

        {
            let mut toggle = Toggle::new("native", "legacy")
                .on_change(|_| panic!("legacy callback must be suppressed"))
                .on_action(crate::widget::test_bool_action_binding());
            let mut doc = AutoCommit::new();
            crate::widget::controls::refresh_control_action_availability(
                &mut toggle,
                &std::collections::HashSet::new(),
            );
            assert!(!Widget::is_enabled(&toggle));
            crate::widget::controls::refresh_control_action_availability(
                &mut toggle,
                &std::collections::HashSet::from([tilo_actions::ActionAddress::new(
                    "test:control",
                    "set",
                )]),
            );
            assert!(Widget::is_enabled(&toggle));
            assert!(Widget::perform_action(
                &mut toggle,
                &crate::a11y::Action::ToggleCheck,
                &[],
                &mut crate::widget::Edits::lend(&mut doc),
                &automerge::ROOT
            ));
            assert!(doc.get(&automerge::ROOT, "legacy").unwrap().is_none());
            assert_eq!(
                crate::widget::controls::take_control_invocation(&mut toggle)
                    .unwrap()
                    .arguments["value"],
                true
            );
        }
    }
    #[test]
    fn the_switch_size_follows_the_ui_scale_and_layout_settles() {
        let (w, h) = ToggleFace::size(&Theme::dark());
        assert_eq!((w, h), (44.0, 24.0));
        let (w2, _) = ToggleFace::size(&Theme::dark().with_ui_scale(1.5));
        assert!((w2 - 66.0).abs() < 1e-9);
        let mut toggle = Toggle::new("t", "t");
        assert!(Widget::is_layout_dirty(&toggle));
        let mut tree = TaffyTree::new();
        let node = toggle.build_layout(
            &mut tree,
            &Theme::dark(),
            &mut std::collections::HashMap::new(),
        );
        tree.compute_layout(node, taffy::prelude::TaffyMaxContent::MAX_CONTENT)
            .unwrap();
        toggle.apply_layout(&tree, node, Point::new(1.0, 1.0));
        assert!(!Widget::is_layout_dirty(&toggle));
    }

    /// A hover change repaints but consumes nothing: leaving one switch for
    /// its neighbour must still reach the neighbour.
    #[test]
    fn a_hover_change_leaves_the_move_for_the_next_control() {
        let mut t = Toggle::new("id", "on");
        let mut tree = TaffyTree::<()>::new();
        let mut node_map = std::collections::HashMap::new();
        let node = t.build_layout(&mut tree, &Theme::dark(), &mut node_map);
        tree.compute_layout(node, taffy::prelude::TaffyMaxContent::MAX_CONTENT)
            .unwrap();
        t.apply_layout(&tree, node, Point::ZERO);
        let device = winit::event::DeviceId::dummy();
        let mut sender = Dispatch::new().layout(&tree, node);
        for position in [(10.0, 10.0), (500.0, 10.0)] {
            let handled = sender
                .send(
                    &mut t,
                    &WindowEvent::CursorMoved {
                        device_id: device,
                        position: position.into(),
                    },
                )
                .handled;
            assert!(!handled, "a move to {position:?} is not consumed");
        }
        assert!(!t.control.is_hovered);
    }

    /// A switch is dirty only until the frame showing what it paints is
    /// drawn, and again once its state changes: an idle switch adds no
    /// damage to a frame.
    #[test]
    fn a_switch_repaints_only_when_what_it_paints_changes() {
        let theme = Theme::dark();
        let mut t = Toggle::new("wifi", "wifi");
        let frame = |t: &mut Toggle| {
            t.draw(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &theme,
                &crate::presence::PresenceCtx::empty(),
            );
            t.mark_drawn();
        };
        assert!(t.needs_redraw(), "never painted");
        frame(&mut t);
        assert!(!t.needs_redraw(), "painted and unchanged");
        let mut damage = Vec::new();
        t.collect_damage(&mut damage);
        assert!(damage.is_empty(), "an idle switch adds no damage");
        for change in [
            |t: &mut Toggle| t.control.is_hovered = true,
            |t: &mut Toggle| t.control.is_checked = true,
            |t: &mut Toggle| t.control.is_focused = true,
            |t: &mut Toggle| t.control.enabled = false,
        ] {
            change(&mut t);
            assert!(t.needs_redraw(), "a changed switch repaints");
            frame(&mut t);
            assert!(!t.needs_redraw(), "and rests once drawn");
        }
        // Drawn but not presented (a cache above it reused), nothing is
        // known to be on screen: the next frame still repaints it.
        t.control.is_hovered = false;
        t.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &theme,
            &crate::presence::PresenceCtx::empty(),
        );
        assert!(t.needs_redraw());
    }

    /// A switch laid out at the origin: a 44×24 pill in a 44×32 target,
    /// its knob 12 px in from either end (20 px of travel).
    fn switch_at_origin(t: &mut Toggle) {
        let mut tree = TaffyTree::<()>::new();
        let node = t.build_layout(
            &mut tree,
            &Theme::dark(),
            &mut std::collections::HashMap::new(),
        );
        tree.compute_layout(node, taffy::prelude::TaffyMaxContent::MAX_CONTENT)
            .unwrap();
        t.apply_layout(&tree, node, Point::ZERO);
    }

    fn present(t: &mut Toggle, theme: &Theme) {
        t.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            theme,
            &crate::presence::PresenceCtx::empty(),
        );
        t.mark_drawn();
    }

    fn ms(t0: crate::time::Instant, ms: u64) -> crate::time::Instant {
        t0 + crate::time::Duration::from_millis(ms)
    }

    #[test]
    fn a_click_slides_the_knob_and_a_second_click_reverses_it_mid_flight() {
        use crate::synthetic_input::{cursor_moved, mouse_press, mouse_release};
        use crate::testing::dispatch;
        use crate::time::testing::with_monotonic_time;
        use crate::tokens::MotionSource;
        let theme = Theme::dark();
        let state = crate::LocalState::new(false);
        let mut t = Toggle::from_state(&state);
        switch_at_origin(&mut t);
        let t0 = crate::time::now();
        with_monotonic_time(t0, || present(&mut t, &theme));
        assert!(!t.needs_animation_frame(), "the first paint rests");
        assert_eq!(t.switch.knob(t0), 0.0);
        with_monotonic_time(t0, || {
            crate::motion::note_source(MotionSource::Pointer);
            dispatch(&mut t, &cursor_moved(30.0, 16.0));
            dispatch(&mut t, &mouse_press());
            dispatch(&mut t, &mouse_release());
            assert!(state.get(), "the value changes once the click lets go");
            present(&mut t, &theme);
            assert!(t.needs_animation_frame(), "the knob slides");
        });
        let drawn = with_monotonic_time(ms(t0, 40), || {
            assert!(t.needs_redraw(), "a sliding knob repaints each frame");
            present(&mut t, &theme);
            let pose = t.switch.pose(theme.motion(), ms(t0, 40));
            assert!(pose.knob > 0.0 && pose.knob < 1.0, "on its way: {pose:?}");
            assert!(
                pose.tint > 0.0 && pose.tint < 1.0,
                "the track turns with it"
            );
            pose.knob
        });
        // A second click mid-flight reverses from where the knob is drawn.
        with_monotonic_time(ms(t0, 40), || {
            crate::motion::note_source(MotionSource::Pointer);
            dispatch(&mut t, &mouse_press());
            dispatch(&mut t, &mouse_release());
            assert!(!state.get());
            present(&mut t, &theme);
            let now = ms(t0, 40);
            assert!((t.switch.knob(now) - drawn).abs() < 1e-9, "no jump");
        });
        with_monotonic_time(ms(t0, 600), || {
            assert_eq!(t.switch.knob(ms(t0, 600)), 0.0, "back off");
            assert!(!t.needs_animation_frame(), "settled: no frames");
            present(&mut t, &theme);
            assert!(!t.needs_redraw(), "settled: no repaint");
        });
    }

    #[test]
    fn a_key_an_assistive_action_or_a_sync_lands_the_switch_at_once() {
        use crate::time::testing::with_monotonic_time;
        use crate::tokens::MotionSource;
        let theme = Theme::dark();
        let state = crate::LocalState::new(false);
        let mut t = Toggle::from_state(&state);
        switch_at_origin(&mut t);
        let t0 = crate::time::now();
        with_monotonic_time(t0, || present(&mut t, &theme));
        let mut doc = AutoCommit::new();
        for (source, on) in [
            (MotionSource::Keyboard, true),
            (MotionSource::Action, false),
        ] {
            with_monotonic_time(t0, || {
                crate::motion::note_source(source);
                assert!(Widget::perform_action(
                    &mut t,
                    &crate::a11y::Action::Activate,
                    &[],
                    &mut crate::widget::Edits::lend(&mut doc),
                    &automerge::ROOT,
                ));
                present(&mut t, &theme);
                assert_eq!(t.switch.knob(t0), if on { 1.0 } else { 0.0 }, "{source:?}");
                assert!(!t.needs_animation_frame(), "{source:?} lands");
            });
        }
        // A value nobody just caused (another device, a sync) lands too.
        with_monotonic_time(ms(t0, 2_000), || {
            state.set(true);
            Widget::sync(&mut t, &doc, &automerge::ROOT);
            present(&mut t, &theme);
            assert_eq!(t.switch.knob(ms(t0, 2_000)), 1.0);
            assert!(!t.needs_animation_frame());
        });
    }

    #[test]
    fn reduced_motion_lands_the_knob_and_cross_fades_the_tint() {
        use crate::synthetic_input::{cursor_moved, mouse_press, mouse_release};
        use crate::testing::dispatch;
        use crate::time::testing::with_monotonic_time;
        let theme = Theme::dark().with_reduce_motion(true);
        let state = crate::LocalState::new(false);
        let mut t = Toggle::from_state(&state);
        switch_at_origin(&mut t);
        let t0 = crate::time::now();
        with_monotonic_time(t0, || {
            present(&mut t, &theme);
            crate::motion::note_source(crate::tokens::MotionSource::Pointer);
            dispatch(&mut t, &cursor_moved(30.0, 16.0));
            dispatch(&mut t, &mouse_press());
            dispatch(&mut t, &mouse_release());
            present(&mut t, &theme);
        });
        let pose = t.switch.pose(theme.motion(), ms(t0, 40));
        assert_eq!(pose.knob, 1.0, "the knob lands");
        assert!(
            pose.tint > 0.0 && pose.tint < 1.0,
            "the tint fades: {pose:?}"
        );
        assert!(!t.switch.is_moving(ms(t0, 400)));
    }

    /// A switch counting the changes it makes, bound to `state`.
    fn counted(state: &crate::LocalState<bool>) -> (Toggle, std::rc::Rc<std::cell::Cell<u32>>) {
        let changes = std::rc::Rc::new(std::cell::Cell::new(0));
        let count = std::rc::Rc::clone(&changes);
        let mut t = Toggle::from_state(state).on_change(move |_| count.set(count.get() + 1));
        switch_at_origin(&mut t);
        (t, changes)
    }

    #[test]
    fn a_dragged_knob_follows_the_hand_one_to_one_and_settles_on_the_nearer_side() {
        use crate::synthetic_input::{cursor_moved, mouse_press, mouse_release};
        use crate::testing::dispatch;
        use crate::time::testing::with_monotonic_time;
        let theme = Theme::dark();
        let state = crate::LocalState::new(false);
        let (mut t, changes) = counted(&state);
        let t0 = crate::time::now();
        with_monotonic_time(t0, || {
            present(&mut t, &theme);
            crate::motion::note_source(crate::tokens::MotionSource::Pointer);
            dispatch(&mut t, &cursor_moved(12.0, 16.0));
            dispatch(&mut t, &mouse_press());
        });
        assert!(!state.get(), "a press changes nothing yet");
        assert!(Widget::wants_pointer_capture(&t), "the hand holds the knob");
        // Within the slop it is still a click; past it the knob is the
        // hand's, 1:1 over its 20 px of travel from where the hand took
        // it, wherever the pointer goes.
        with_monotonic_time(ms(t0, 8), || dispatch(&mut t, &cursor_moved(14.0, 16.0)));
        assert!(!t.hold.unwrap().dragged);
        with_monotonic_time(ms(t0, 16), || dispatch(&mut t, &cursor_moved(20.0, 16.0)));
        for (i, (x, knob)) in [(26.0, 0.7), (30.0, 0.9), (18.0, 0.3), (13.0, 0.05)]
            .into_iter()
            .enumerate()
        {
            let at = ms(t0, 24 + 8 * u64::try_from(i).unwrap());
            with_monotonic_time(at, || {
                dispatch(&mut t, &cursor_moved(x, 60.0));
                present(&mut t, &theme);
            });
            assert!(
                (t.switch.knob(at) - knob).abs() < 1e-9,
                "at x {x}: {}",
                t.switch.knob(at)
            );
            assert!(!t.switch.is_moving(at), "nothing runs under the hand");
        }
        assert_eq!(changes.get(), 0, "the knob moves; the value does not");
        // Let go heading left, near the left: it settles off, where it
        // began, and nothing changed.
        with_monotonic_time(ms(t0, 60), || dispatch(&mut t, &mouse_release()));
        assert!(!state.get(), "left on the off side");
        assert_eq!(changes.get(), 0, "a drag back to the start changes nothing");
        assert!(!Widget::wants_pointer_capture(&t));
        let rest = t.switch.knob.settles_at().map_or(ms(t0, 60), |at| at);
        assert_eq!(t.switch.knob(rest), 0.0);

        // Dragged past the middle and held still, it settles on: one change.
        let state = crate::LocalState::new(false);
        let (mut t, changes) = counted(&state);
        with_monotonic_time(t0, || {
            present(&mut t, &theme);
            dispatch(&mut t, &cursor_moved(12.0, 16.0));
            dispatch(&mut t, &mouse_press());
            dispatch(&mut t, &cursor_moved(16.0, 16.0));
        });
        with_monotonic_time(ms(t0, 10), || dispatch(&mut t, &cursor_moved(30.0, 16.0)));
        assert_eq!(changes.get(), 0);
        with_monotonic_time(ms(t0, 300), || dispatch(&mut t, &mouse_release()));
        assert!(state.get(), "left on the on side");
        assert_eq!(changes.get(), 1, "a drag across changes the value once");
        assert_eq!(t.switch.knob(ms(t0, 2_000)), 1.0);

        // A press let go off the switch, without a drag, is no click.
        with_monotonic_time(ms(t0, 3_000), || {
            dispatch(&mut t, &cursor_moved(30.0, 16.0));
            dispatch(&mut t, &mouse_press());
            dispatch(&mut t, &cursor_moved(31.0, 90.0));
            dispatch(&mut t, &mouse_release());
        });
        assert!(state.get());
        assert_eq!(changes.get(), 1);
    }

    /// Touch on a page of switches: a finger on a knob drags it once it
    /// heads across, a vertical swipe from the same knob scrolls the page,
    /// and a tap flips the switch.
    #[test]
    fn a_vertical_swipe_on_a_knob_scrolls_and_a_horizontal_one_drags_it() {
        use crate::widget::{Column, Scrollable};
        use winit::event::TouchPhase::{Ended, Moved, Started};
        let theme = Theme::dark();
        let states: Vec<_> = (0..30).map(|_| crate::LocalState::new(false)).collect();
        let mut column = Column::new();
        for state in &states {
            column = column.push(Box::new(Toggle::from_state(state)));
        }
        let mut frame = crate::headless::FrameHarness::for_root(
            crate::State::new(),
            Scrollable::new_vertical()
                .fill_parent_height()
                .with_child(Box::new(column)),
            (360, 300),
            theme.clone(),
        );
        let offset = |frame: &crate::headless::FrameHarness| frame.root::<Scrollable>().offset_y();
        // The knob of switch `index`, at rest off, where it is drawn now.
        let knob = |frame: &crate::headless::FrameHarness, index: usize| {
            let column = frame.root::<Scrollable>().children()[0];
            let track = track_rect(column.children()[index].get_rect(), &theme);
            Point::new(track.x0 + track.height() / 2.0, track.center().y)
        };
        let at = knob(&frame, 1);
        assert_eq!(
            Widget::touch_claim(frame.root::<Scrollable>().children()[0].children()[1], at),
            crate::interaction::TouchClaim::DragAcross
        );

        // A finger reports every 8 ms (120 Hz) however long the test takes
        // to handle each report: a swipe's throw is read from those times,
        // and a loaded host must not turn it into a slow drag.
        let finger = |frame: &mut crate::headless::FrameHarness,
                      start: crate::time::Instant,
                      reports: &[(u64, winit::event::TouchPhase, f64, f64)]| {
            let mut at = start;
            for &(id, phase, x, y) in reports {
                crate::time::testing::with_monotonic_time(at, || frame.touch(id, phase, x, y));
                at += crate::time::Duration::from_millis(8);
            }
            at
        };

        // Up from the knob: the page scrolls and the switch stays off.
        let mut swipe = vec![(1, Started, at.x, at.y)];
        for step in 1..=6 {
            swipe.push((1, Moved, at.x + 2.0, at.y - 10.0 * f64::from(step)));
        }
        swipe.push((1, Ended, at.x + 2.0, at.y - 60.0));
        let _ = finger(&mut frame, crate::time::now(), &swipe);
        assert!(offset(&frame) > 0.0, "the page scrolled");
        assert!(!states[1].get(), "the switch is untouched");
        assert!(states.iter().all(|state| !state.get()));

        // Across from a knob still on screen, once the swipe's glide has
        // come to rest: it is dragged to on, and the page stays.
        assert!(frame.settle_for_capture(crate::time::Duration::from_secs(5)));
        let scrolled = offset(&frame);
        let at = knob(&frame, 6);
        assert!(at.y > 0.0 && at.y < 300.0, "on screen: {at:?}");
        let mut drag = vec![(2, Started, at.x, at.y)];
        for step in 1..=5 {
            drag.push((2, Moved, at.x + 5.0 * f64::from(step), at.y + 1.0));
        }
        drag.push((2, Ended, at.x + 25.0, at.y + 1.0));
        let next = finger(&mut frame, crate::time::now(), &drag);
        assert!(states[6].get(), "dragged on");
        assert_eq!(states.iter().filter(|state| state.get()).count(), 1);
        assert_eq!(offset(&frame), scrolled, "a drag across does not scroll");

        // A tap on the knob flips it back.
        let at = Point::new(at.x + 20.0, at.y);
        let _ = finger(
            &mut frame,
            next,
            &[(3, Started, at.x, at.y), (3, Ended, at.x, at.y)],
        );
        assert!(!states[6].get(), "a tap flips");
        assert_eq!(offset(&frame), scrolled);
    }

    #[test]
    fn a_painted_switch_follows_its_motion_and_matches_the_widget_at_rest() {
        let theme = Theme::dark();
        let rect = Rect::new(0.0, 0.0, 44.0, 24.0);
        let on = ToggleFace::new(rect, true).colors(&theme);
        let off = ToggleFace::new(rect, false).colors(&theme);
        let half = ToggleFace::new(rect, true)
            .pose(SwitchPose {
                knob: 0.5,
                tint: 0.5,
            })
            .colors(&theme);
        assert_ne!(half.track, on.track);
        assert_ne!(half.track, off.track);
        assert_eq!(
            ToggleFace::new(rect, false)
                .pose(SwitchPose::at_rest(true))
                .colors(&theme),
            on,
            "a pose at rest on is exactly the on face"
        );
        // A host's motion: the first value rests, a click slides.
        let now = crate::time::now();
        let mut motion = SwitchMotion::default();
        motion.follow(
            false,
            theme.motion(),
            crate::tokens::MotionSource::Pointer,
            now,
        );
        assert!(!motion.is_moving(now));
        motion.follow(
            true,
            theme.motion(),
            crate::tokens::MotionSource::Pointer,
            now,
        );
        assert!(motion.is_moving(now));
        let later = now + crate::time::Duration::from_secs(1);
        assert_eq!(
            motion.pose(theme.motion(), later),
            SwitchPose::at_rest(true)
        );
    }
}
