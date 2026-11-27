#[cfg(feature = "model")]
use tilo_permissions::PermissionScope;
use tilo_ui_base::text_render::TextRenderer;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::{presence::PathSegment, presence::PresenceKey};
#[path = "slider_component.rs"]
mod component;
pub use component::SliderProps;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use tilo_ui_base::widget::TaffyTreeExt;
use tilo_ui_base::widget::Widget;
use tilo_ui_base::{widget::ActionEmitter, widget::ControlBindingMetadata};
use tilo_ui_base::{widget::AppModel, widget::ModelPath};
use tilo_ui_base::{widget::binding::BoxedBinding, widget::binding::Pulled};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

/// The track runs flush with the slider's rect; the knob overhangs its ends
/// by its radius, so the track lines up with the content edges beside it.
pub const TRACK_PADDING: f64 = 0.0;
const PAGE_FRACTION: f64 = 0.1;
/// Arrow keys and assistive steps move this share of a range with no step.
const ARROW_FRACTION: f64 = 0.01;

/// Formats a slider's value for assistive technology ("40 %", "1.5 s").
type ValueText = Box<dyn Fn(f64) -> String>;

/// A value as plain digits: at most two decimals, no trailing zeros.
fn plain_number(value: f64) -> String {
    let text = format!("{value:.2}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text == "-0" {
        "0".to_owned()
    } else {
        text.to_owned()
    }
}
const TRACK_HEIGHT: f64 = 6.0;

/// Knob radius at rest. Hover and press scale this — see the
/// `_SCALE` constants below. Pulled out so tests can pin the
/// idle-vs-hover-vs-press contract without hard-coding pixels.
const KNOB_RADIUS: f64 = 10.0;
const KNOB_RADIUS_HOVER_SCALE: f64 = 1.08;
const KNOB_RADIUS_PRESS_SCALE: f64 = 1.15;
/// The knob's border, drawn outside its radius.
const KNOB_BORDER: f64 = 1.5;

/// One finger on a slider, read as the pointer held down: the value jumps to where the finger
/// lands, follows it while it moves (on or off the track) and settles where it lifts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SliderTouch {
    contact: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SliderTouchStep {
    /// A finger came down on the slider: press, and jump the value there.
    Press(Point),
    /// The holding finger moved: the value follows its x.
    Move(Point),
    /// The holding finger lifted, or the system took the contact: release
    /// where the value is.
    Release,
    /// Not this slider's contact.
    Ignored,
}

impl SliderTouch {
    /// Read one contact update. `scale_factor` turns its physical location
    /// logical; `hits` says whether a finger landing there is on the
    /// slider, and is asked only when a contact begins.
    pub fn step(
        &mut self,
        touch: &winit::event::Touch,
        scale_factor: f64,
        hits: impl FnOnce(Point) -> bool,
    ) -> SliderTouchStep {
        use winit::event::TouchPhase;
        let scale = if scale_factor > 0.0 {
            scale_factor
        } else {
            1.0
        };
        let point = Point::new(touch.location.x / scale, touch.location.y / scale);
        match touch.phase {
            TouchPhase::Started if self.contact.is_none() && hits(point) => {
                self.contact = Some(touch.id);
                SliderTouchStep::Press(point)
            }
            TouchPhase::Moved if self.contact == Some(touch.id) => SliderTouchStep::Move(point),
            TouchPhase::Ended | TouchPhase::Cancelled if self.contact == Some(touch.id) => {
                self.contact = None;
                SliderTouchStep::Release
            }
            _ => SliderTouchStep::Ignored,
        }
    }

    pub fn is_active(&self) -> bool {
        self.contact.is_some()
    }

    pub fn cancel(&mut self) {
        self.contact = None;
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent pointer, focus, drag, and enabled presentation states"
)]
pub struct Slider {
    semantic_id: String,
    rect: Rect,
    is_hovered: bool,
    is_dragging: bool,
    /// The drag under way has invoked the action at least once.
    gesture: bool,
    is_focused: bool,
    /// The value, in `range` units.
    value: f64,
    /// The values the slider spans; `0.0..=1.0` unless set.
    range: (f64, f64),
    /// Values snap to `range.0 + k * step`.
    step: Option<f64>,
    /// The values the slider stops at, in order, marked along the track: a value snaps to the
    /// nearest, and a key moves to the next. Empty for a slider with no stops of its own.
    stops: Vec<f64>,
    /// How the value is spoken; a percentage of a `0..1` range otherwise.
    value_text: Option<ValueText>,
    binding: Option<BoxedBinding<f64>>,
    /// What hears each value the person moves it to.
    on_change: Option<Box<dyn FnMut(f64)>>,
    settles: bool,
    drag_from: Option<f64>,
    /// Where the pointer and the value were when Shift was pressed during
    /// a drag: from there the value moves a tenth as fast, to place it
    /// finely.
    fine_from: Option<(f64, f64)>,
    action: Option<ActionEmitter<f64>>,
    refused_press: tilo_ui_base::widget::action_emitter::RefusedPress,
    presence_path: Vec<PathSegment>,
    #[cfg(feature = "model")]
    permission_scope: Option<PermissionScope>,
    /// When `false` the slider is disabled: skipped by focus, rejects
    /// input (no drag, no value change, no hover feedback) and renders
    /// dimmed. Default `true`.
    enabled: bool,
    /// Last observed cursor position in logical-pixel space.
    last_cursor: Point,
    /// Optional override for the `Widget::test_id()` value.
    id_override: Option<String>,
    /// Spoken name for a slider with no adjacent text label, so a screen reader announces what
    /// is being adjusted ("Volume, 50 percent") instead of just the value.
    accessible_name: Option<String>,
    /// `accessible_name` is already resolved text, spoken as given.
    name_resolved: bool,
    interaction: tilo_ui_base::interaction::InteractionEnvironment,
    layout_target: Option<f32>,
    /// Show the value in words beside the track ([`Self::shows_value`]).
    shows_value: bool,
    /// Show the value in words over the knob while it is held
    /// ([`Self::reads_out_while_held`]).
    reads_out: bool,
    readout_drawn: bool,
    /// How far above the knob the readout reaches and how far past its chip
    /// it paints, in the theme as last laid out: everything it may cover.
    readout_reach: (f64, vello::kurbo::Insets),
    /// The width the shown value takes at the trailing edge, measured when
    /// drawn; the track is the rest.
    value_area: f64,
    touch: SliderTouch,
    /// Where the thumb is drawn, as a share of the range: on the value,
    /// except while it glides to where the track was pressed.
    thumb: tilo_ui_base::motion::Thumb,
    thumb_placed: bool,
    /// Where the press that sent the thumb gliding was, and what pressed
    /// (the pointer or a finger): the glide gives way to the hand once it
    /// moves past its slop from there.
    glide_from: Option<(Point, tilo_ui_base::tokens::MotionSource)>,
    /// A view that builds the slider anew on each change: the one built in
    /// its place goes on from its thumb (`motion::keep`).
    hand_on: tilo_ui_base::motion::HandOn<KeptThumb>,
    motion: Option<tilo_ui_base::tokens::EffectiveMotion>,
    /// How far the hand moves from a press before the drag is the hand's,
    /// the theme's click slop as last laid out: a thumb gliding to the
    /// press gives way to it there.
    slop: f64,
    /// What the last presented frame painted, and what the frame being drawn
    /// paints: the slider needs a redraw only when they differ.
    painted: Option<Painted>,
    drawing: Option<Painted>,
}

/// What a slider's paint depends on besides its place and the theme: the
/// value and the drawn thumb (as bits), hovered, dragging, focused and
/// enabled.
type Painted = (u64, u64, bool, bool, bool, bool);

impl Slider {
    /// How fast the value follows the pointer while Shift is held during a
    /// drag, as a share of the usual pace.
    const FINE_DRAG: f64 = 0.1;

    fn effectively_enabled(&self) -> bool {
        self.enabled && self.action.as_ref().is_none_or(ActionEmitter::is_available)
    }
    pub fn new(id: &str, on_change_path: &str) -> Self {
        Self {
            semantic_id: id.to_string(),
            rect: Rect::ZERO,
            is_hovered: false,
            is_dragging: false,
            gesture: false,
            is_focused: false,
            value: 0.5,
            range: (0.0, 1.0),
            step: None,
            stops: Vec::new(),
            value_text: None,
            binding: Some(tilo_ui_base::doc_source::number_key(on_change_path)),
            on_change: None,
            settles: false,
            drag_from: None,
            fine_from: None,
            action: None,
            refused_press: tilo_ui_base::widget::action_emitter::RefusedPress::default(),
            presence_path: Vec::new(),
            #[cfg(feature = "model")]
            permission_scope: None,
            enabled: true,
            last_cursor: Point::ZERO,
            id_override: None,
            accessible_name: None,
            name_resolved: false,
            interaction: tilo_ui_base::interaction::InteractionEnvironment::default(),
            layout_target: None,
            shows_value: false,
            value_area: 0.0,
            touch: SliderTouch::default(),
            reads_out: false,
            readout_drawn: false,
            readout_reach: (0.0, vello::kurbo::Insets::ZERO),
            thumb: tilo_ui_base::motion::Thumb::default(),
            thumb_placed: false,
            glide_from: None,
            hand_on: tilo_ui_base::motion::HandOn::default(),
            motion: None,
            slop: f64::from(tilo_ui_base::tokens::Metrics::BASE.click_slop),
            painted: None,
            drawing: None,
        }
    }

    fn identity(&self) -> Option<&str> {
        self.id_override
            .as_deref()
            .or((self.semantic_id != "slider").then_some(self.semantic_id.as_str()))
    }

    /// Go on from the slider a rebuild dropped: its thumb, gliding to a press or at rest, and
    /// the hand holding it, if one is.
    fn take_up(&mut self, kept: KeptThumb) {
        self.thumb = kept.thumb;
        self.thumb_placed = kept.placed;
        self.glide_from = kept.glide_from;
        if let Some(held) = kept.held {
            self.is_dragging = true;
            self.touch = held.touch;
            self.drag_from = held.drag_from;
            self.fine_from = held.fine_from;
            self.gesture = held.gesture;
            self.last_cursor = held.last_cursor;
        }
    }

    fn painted_now(&self) -> Painted {
        (
            self.fraction().to_bits(),
            self.thumb.get(tilo_ui_base::motion::frame_now()).to_bits(),
            self.is_hovered,
            self.is_dragging,
            self.is_focused,
            self.effectively_enabled(),
        )
    }

    fn effective_motion(&self) -> tilo_ui_base::tokens::EffectiveMotion {
        self.motion.unwrap_or_else(|| {
            tilo_ui_base::tokens::EffectiveMotion::new(
                tilo_ui_base::tokens::Motion::default(),
                false,
            )
        })
    }

    /// Put the thumb on the value: a press away from it glides there
    /// (`source` is the pointer or a finger), a press on it or a drag
    /// holds it under the hand.
    fn place_thumb(&mut self, glide: Option<tilo_ui_base::tokens::MotionSource>) {
        let to = self.fraction();
        if !self.thumb_placed {
            return;
        }
        match glide {
            Some(source) => {
                let motion = self.effective_motion();
                self.thumb
                    .go(to, motion, source, tilo_ui_base::motion::frame_now());
            }
            None => self.thumb.track(to),
        }
    }

    /// Create a normalized slider bound to typed process-local state.
    pub fn from_state(state: &tilo_ui_base::LocalState<f64>) -> Self {
        let (value, binding) = tilo_ui_base::widget::binding::local(state);
        let mut slider = Self::new("slider", "");
        slider.value = value;
        slider.binding = Some(binding);
        slider
    }

    /// The values the slider spans (`0.0..=1.0` by default). The value, the
    /// binding and every callback are in these units.
    #[must_use]
    pub fn range(mut self, min: f64, max: f64) -> Self {
        let (min, max) = if min <= max { (min, max) } else { (max, min) };
        self.range = (min, max);
        self.value = self.constrain(self.value);
        self
    }

    /// Snap values to multiples of `step` from the range's start; the arrow
    /// keys move one step.
    #[must_use]
    pub fn step(mut self, step: f64) -> Self {
        self.step = (step.is_finite() && step > 0.0).then_some(step);
        self.value = self.constrain(self.value);
        self
    }

    /// The values the slider stops at, each marked along the track (versions along a history,
    /// chapters along a recording): a drag lands on the nearest, and the arrow keys go from one
    /// to the next. Values outside the range are left out.
    #[must_use]
    pub fn stops(mut self, stops: impl IntoIterator<Item = f64>) -> Self {
        let (min, max) = self.range;
        let mut stops: Vec<f64> = stops
            .into_iter()
            .filter(|stop| stop.is_finite() && (min..=max).contains(stop))
            .collect();
        stops.sort_by(f64::total_cmp);
        stops.dedup();
        self.stops = stops;
        self.value = self.constrain(self.value);
        self
    }

    /// The values it stops at, as [`Self::stops`] took them.
    #[must_use]
    pub fn stop_values(&self) -> &[f64] {
        &self.stops
    }

    #[must_use]
    pub fn bounds(&self) -> (f64, f64) {
        self.range
    }

    #[must_use]
    pub fn step_size(&self) -> Option<f64> {
        self.step
    }

    /// Say the value in the slider's own terms ("1.5 s", "Medium") to
    /// assistive technology.
    #[must_use]
    pub fn value_text(mut self, format: impl Fn(f64) -> String + 'static) -> Self {
        self.value_text = Some(Box::new(format));
        self
    }

    /// Show the value beside the track, in the words it is spoken in
    /// ([`Self::value_text`], a percentage of a `0..1` range otherwise), so
    /// "72%" or "1.5 s" is read without moving the knob.
    #[must_use]
    pub fn shows_value(mut self) -> Self {
        self.shows_value = true;
        self
    }

    /// While the knob is held, by the pointer or a finger, show the value
    /// it would land on in words ([`Self::value_text`]) on a chip above it:
    /// a scrubber says the time under the finger that covers the knob.
    #[must_use]
    pub fn reads_out_while_held(mut self) -> Self {
        self.reads_out = true;
        self
    }

    pub fn track_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0,
            self.rect.y0,
            (self.rect.x1 - self.value_area).max(self.rect.x0),
            self.rect.y1,
        )
    }

    /// Clamp `value` into the range and onto the step grid.
    fn constrain(&self, value: f64) -> f64 {
        let (min, max) = self.range;
        if !value.is_finite() {
            return min;
        }
        let value = value.clamp(min, max);
        if let Some(nearest) = self.nearest_stop(value) {
            return nearest;
        }
        match self.step {
            Some(step) => tilo_ui_base::num::snap_decimal(
                min + ((value - min) / step).round() * step,
                &[min, step],
            )
            .clamp(min, max),
            None => value,
        }
    }

    /// The stop nearest `value`, when the slider has stops.
    fn nearest_stop(&self, value: f64) -> Option<f64> {
        self.stops
            .iter()
            .copied()
            .min_by(|a, b| (a - value).abs().total_cmp(&(b - value).abs()))
    }

    /// Where moving the value by `by` takes it: on a slider with stops, at least to the next
    /// stop that way, so a key never stays where it was.
    fn moved_by(&self, by: f64) -> f64 {
        let target = self.constrain(self.value + by);
        if self.stops.is_empty() || target.to_bits() != self.value.to_bits() {
            return target;
        }
        let next = if by > 0.0 {
            self.stops.iter().copied().find(|stop| *stop > self.value)
        } else {
            self.stops
                .iter()
                .rev()
                .copied()
                .find(|stop| *stop < self.value)
        };
        next.unwrap_or(target)
    }

    /// The value as a share of the range, `0..=1`.
    fn fraction(&self) -> f64 {
        let (min, max) = self.range;
        if max > min {
            ((self.value - min) / (max - min)).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    fn at_fraction(&self, fraction: f64) -> f64 {
        let (min, max) = self.range;
        self.constrain(min + fraction.clamp(0.0, 1.0) * (max - min))
    }

    fn arrow_step(&self) -> f64 {
        self.step
            .unwrap_or((self.range.1 - self.range.0) * ARROW_FRACTION)
    }

    /// One page: a tenth of the range, at least one step.
    fn page_step(&self) -> f64 {
        ((self.range.1 - self.range.0) * PAGE_FRACTION).max(self.arrow_step())
    }

    /// What assistive technology says for the value.
    fn spoken_value(&self) -> String {
        self.spoken_value_of(self.value)
    }

    /// What assistive technology says for `value`.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the rounded percentage is bounded to the inclusive range 0..=100"
    )]
    fn spoken_value_of(&self, value: f64) -> String {
        if let Some(format) = &self.value_text {
            return format(value);
        }
        if self.range == (0.0, 1.0) {
            format!("{}%", (value.clamp(0.0, 1.0) * 100.0).round() as i64)
        } else {
            plain_number(value)
        }
    }

    /// Run `callback` with each value the person moves the slider to: a drag, a key and a
    /// screen reader's step alike. Model synchronization and [`Self::set_value`] move it without.
    #[must_use]
    pub fn on_change(mut self, callback: impl FnMut(f64) + 'static) -> Self {
        if self.action.is_none() {
            self.on_change = Some(Box::new(callback));
        }
        self
    }

    /// Invoke the bound action once per drag, with the value it is let go at, rather than at
    /// every step of it; a key or an assistive action still invokes it at once.
    #[must_use]
    pub fn commits_on_release(mut self) -> Self {
        self.settles = true;
        self
    }

    #[must_use]
    pub fn on_action(mut self, binding: tilo_actions::ActionBinding<f64>) -> Self {
        self.action = Some(ActionEmitter::new(crate::Bind::bound(binding)));
        self.on_change = None;
        self
    }

    /// The value currently presented, in the range's units.
    pub fn value(&self) -> f64 {
        self.value
    }

    /// Set the value programmatically (in the range's units) and update a
    /// bound local model without invoking [`Self::on_change`]'s callback.
    pub fn set_value(&mut self, value: f64) {
        self.value = self.constrain(value);
        if self.action.is_none() {
            self.publish_local_state();
        }
    }

    fn publish_local_state(&mut self) {
        if let Some(binding) = &mut self.binding {
            binding.publish(&self.value);
        }
    }

    #[must_use]
    pub fn with_presence(mut self, path: Vec<PathSegment>) -> Self {
        self.presence_path = path;
        self
    }

    /// Override the value returned from `Widget::test_id()`.
    #[must_use]
    pub fn with_id(mut self, id: &str) -> Self {
        self.id_override = Some(id.to_string());
        self
    }

    #[must_use]
    #[cfg(feature = "model")]
    pub fn with_permission_scope(mut self, scope: PermissionScope) -> Self {
        self.permission_scope = Some(scope);
        self
    }

    /// Set the enabled state at build time. A disabled slider is skipped
    /// by focus, rejects input and renders dimmed. Default `true`.
    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Give a label-less slider a spoken name so a screen reader announces
    /// what it adjusts alongside the value: `name` is a message id, resolved
    /// in the language this thread reads.
    #[must_use]
    pub fn with_accessible_name(mut self, name: &str) -> Self {
        self.accessible_name = Some(name.to_string());
        self.name_resolved = false;
        self
    }

    /// [`Self::with_accessible_name`] with text that is already resolved (the
    /// row's caption): spoken as given, never looked up as a message id.
    #[must_use]
    pub fn with_resolved_accessible_name(mut self, name: impl Into<String>) -> Self {
        self.accessible_name = Some(name.into());
        self.name_resolved = true;
        self
    }

    /// Whether the person holds the knob (a pointer drag or a finger on
    /// it): a host that moves the value itself (a song's position) leaves
    /// it to them until they let go.
    #[must_use]
    pub fn is_held(&self) -> bool {
        self.is_dragging || self.touch.is_active()
    }

    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Map a cursor x in widget-local logical pixels to the slider's
    /// 0..1 value, clamped at both ends.
    pub fn value_from_cursor_x(&self, cursor_x: f64) -> f64 {
        let fraction = SliderFace::new(self.track_rect(), 0.0).value_at(Point::new(cursor_x, 0.0));
        self.at_fraction(fraction)
    }

    /// Where a press or a finger takes hold of the slider: the track, with
    /// the knob's overhang at either end.
    fn hit_band(&self) -> Rect {
        self.track_rect().inflate(KNOB_RADIUS, 0.0)
    }

    /// Take hold at `point` and jump the value there. The value is set at
    /// once; the thumb glides there when the press was away from it, from
    /// `source` (the pointer or a finger).
    fn press_at(
        &mut self,
        point: Point,
        source: tilo_ui_base::tokens::MotionSource,
        ctx: &mut tilo_ui_base::widget::EventCtx<'_>,
    ) {
        self.last_cursor = point;
        self.is_dragging = true;
        self.gesture = false;
        self.drag_from = Some(self.value);
        *ctx.focus_target = Some(ctx.node_id);
        *ctx.handled = true;
        let new_value = self.value_from_cursor_x(point.x);
        self.commit_value(new_value, ctx.state.doc(), ctx.path);
        let track = self.track_rect();
        let thumb_x = track.x0
            + TRACK_PADDING
            + self.thumb.get(tilo_ui_base::motion::frame_now())
                * (track.width() - TRACK_PADDING * 2.0).max(0.0);
        let away = (point.x - thumb_x).abs() > KNOB_RADIUS;
        self.glide_from = away.then_some((point, source));
        self.place_thumb(away.then_some(source));
    }

    /// The held slider follows `point`; `fine` moves it a tenth as fast
    /// from where fine placement began.
    fn drag_to(&mut self, point: Point, fine: bool, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
        if !self.is_dragging {
            return;
        }
        let new_value = if fine {
            let (x, value) = *self.fine_from.get_or_insert((point.x, self.value));
            let track = (self.track_rect().width() - TRACK_PADDING * 2.0).max(1.0);
            let span = self.range.1 - self.range.0;
            self.constrain(value + (point.x - x) / track * Self::FINE_DRAG * span)
        } else {
            self.fine_from = None;
            self.value_from_cursor_x(point.x)
        };
        if self.commit_value(new_value, ctx.state.doc(), ctx.path) {
            *ctx.handled = true;
        }
        // A thumb gliding to the press follows the value there until the
        // hand moves off the press; from then it is welded to the hand.
        if self
            .glide_from
            .is_some_and(|(from, _)| (point.x - from.x).abs() > self.slop)
        {
            self.glide_from = None;
        }
        self.place_thumb(self.glide_from.map(|(_, source)| source));
    }

    fn release(&mut self, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
        self.is_dragging = false;
        self.fine_from = None;
        self.glide_from = None;
        *ctx.handled = true;
        let moved = self
            .drag_from
            .take()
            .is_some_and(|from| from.total_cmp(&self.value).is_ne());
        if self.settles && moved {
            if let Some(action) = &mut self.action {
                let _ = action.emit(self.value);
            } else if let Some(on_change) = &mut self.on_change {
                on_change(self.value);
            }
        }
    }

    fn handle_key(&mut self, key: &KeyEvent, state: &AppModel, root: &ModelPath) -> bool {
        if !self.is_focused || key.state != ElementState::Pressed {
            return false;
        }
        let target = match &key.logical_key {
            Key::Named(NamedKey::ArrowRight | NamedKey::ArrowUp) => {
                self.moved_by(self.arrow_step())
            }
            Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowDown) => {
                self.moved_by(-self.arrow_step())
            }
            Key::Named(NamedKey::PageUp) => self.moved_by(self.page_step()),
            Key::Named(NamedKey::PageDown) => self.moved_by(-self.page_step()),
            Key::Named(NamedKey::Home) => self.range.0,
            Key::Named(NamedKey::End) => self.range.1,
            _ => return false,
        };
        let target = self.constrain(target);
        self.commit_value(target, state, root);
        true
    }

    /// Knob radius for the current visual state. Pressed > hover > idle.
    /// Public so the size contract is testable without poking the scene.
    pub fn knob_radius_for_state(&self) -> f64 {
        if self.is_dragging {
            KNOB_RADIUS * KNOB_RADIUS_PRESS_SCALE
        } else if self.is_hovered {
            KNOB_RADIUS * KNOB_RADIUS_HOVER_SCALE
        } else {
            KNOB_RADIUS
        }
    }

    #[cfg(test)]
    pub(crate) fn set_hovered_for_test(&mut self, hovered: bool) {
        self.is_hovered = hovered;
    }

    #[cfg(test)]
    pub(crate) fn set_dragging_for_test(&mut self, dragging: bool) {
        self.is_dragging = dragging;
    }

    /// Commit each distinct value, including fine steps and exact endpoints.
    /// Returns `true` if the commit happened so callers can flag the event.
    fn commit_value(&mut self, new_value: f64, _state: &AppModel, _root: &ModelPath) -> bool {
        if new_value.total_cmp(&self.value).is_eq() {
            return false;
        }
        self.value = new_value;
        if let Some(action) = &mut self.action {
            if !(self.settles && self.is_dragging) {
                let input = if !self.is_dragging {
                    tilo_ui_base::widget::input_provenance::Input::Discrete
                } else if std::mem::replace(&mut self.gesture, true) {
                    tilo_ui_base::widget::input_provenance::Input::Continuing
                } else {
                    tilo_ui_base::widget::input_provenance::Input::Starting
                };
                let value = self.value;
                let _ = tilo_ui_base::widget::input_provenance::with_input(input, || {
                    action.emit(value)
                });
            } else if let Some(binding) = self
                .binding
                .as_mut()
                .filter(|binding| binding.document_key().is_none())
            {
                binding.publish(&self.value);
            }
        } else {
            if let Some(binding) = &mut self.binding {
                binding.publish(&self.value);
            }
            if let Some(on_change) = &mut self.on_change
                && !(self.settles && self.is_dragging)
            {
                on_change(self.value);
            }
        }
        true
    }
}

/// An immediate-mode slider face: everything [`Slider::paint`] needs to draw one, with no
/// widget instance and no CRDT binding.
#[derive(Clone, Copy, Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "immediate-mode face records independent visual states"
)]
pub struct SliderFace {
    rect: Rect,
    value: f64,
    hovered: bool,
    dragging: bool,
    focused: bool,
    disabled: bool,
}

impl SliderFace {
    /// A face over `rect` showing `value` (clamped to 0..=1).
    pub fn new(rect: Rect, value: f64) -> Self {
        Self {
            rect,
            value: value.clamp(0.0, 1.0),
            hovered: false,
            dragging: false,
            focused: false,
            disabled: false,
        }
    }
    #[must_use]
    pub fn hovered(mut self, v: bool) -> Self {
        self.hovered = v;
        self
    }
    #[must_use]
    pub fn dragging(mut self, v: bool) -> Self {
        self.dragging = v;
        self
    }
    #[must_use]
    pub fn focused(mut self, v: bool) -> Self {
        self.focused = v;
        self
    }
    #[must_use]
    pub fn disabled(mut self, v: bool) -> Self {
        self.disabled = v;
        self
    }

    /// Where a pointer takes this slider: the track and the knob overhanging
    /// its ends, grown to the minimum pointer target.
    pub fn hit_rect(&self, theme: &Theme) -> Rect {
        let target = f64::from(theme.tokens.metrics.min_pointer_target);
        let grow_y = ((target - self.rect.height()) / 2.0).max(0.0);
        self.rect.inflate(KNOB_RADIUS, grow_y)
    }

    /// How far the knob reaches past the track's ends (its pressed radius):
    /// a host keeps this much margin either side of a slider's rect.
    #[must_use]
    pub fn overhang() -> f64 {
        KNOB_RADIUS * KNOB_RADIUS_PRESS_SCALE
    }

    /// The share of the range (`0..=1`) under `point`, clamped at both ends.
    pub fn value_at(&self, point: Point) -> f64 {
        let track = self.track_rect();
        if track.width() <= 0.0 {
            return 0.0;
        }
        ((point.x - track.x0) / track.width()).clamp(0.0, 1.0)
    }

    #[must_use]
    pub fn knob_centre(&self) -> Point {
        let track = self.track_rect();
        Point::new(track.x0 + self.value * track.width(), self.rect.center().y)
    }

    /// The track rect this face paints, so a caller hit-tests the geometry it
    /// actually draws.
    pub fn track_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0 + TRACK_PADDING,
            self.rect.y0 + (self.rect.height() - TRACK_HEIGHT) / 2.0,
            self.rect.x1 - TRACK_PADDING,
            self.rect.y0 + f64::midpoint(self.rect.height(), TRACK_HEIGHT),
        )
    }

    /// Everything this face paints: the rect plus the knob, its border and its pressed halo
    /// overhanging the ends.
    fn paint_bounds(&self) -> Rect {
        // The pressed halo's outer ring (4) and the knob's border (1.5), as
        // `Slider::paint` draws them.
        let bleed = Self::overhang() + 4.0 + KNOB_BORDER;
        self.rect.inflate(bleed, bleed)
    }

    fn knob_radius(&self) -> f64 {
        if self.dragging {
            KNOB_RADIUS * KNOB_RADIUS_PRESS_SCALE
        } else if self.hovered {
            KNOB_RADIUS * KNOB_RADIUS_HOVER_SCALE
        } else {
            KNOB_RADIUS
        }
    }
}

impl Slider {
    /// Paint a themed slider face. The single painter for both the retained
    /// widget and any immediate-mode caller, so the two cannot drift apart.
    pub fn paint(scene: &mut Scene, transform: Affine, theme: &Theme, face: &SliderFace) {
        if face.disabled {
            crate::widget::paint_disabled(scene, transform, &face.paint_bounds());
        }
        let track_rect = face.track_rect();
        let track_radius = track_rect.height() / 2.0;
        let unfilled = theme.tokens.colors.surfaces.raised;
        let filled = theme.tokens.colors.intent.value_fill;
        let track_shape = RoundedRect::from_rect(track_rect, track_radius);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            unfilled,
            None,
            &track_shape,
        );

        let active = Rect::new(
            track_rect.x0,
            track_rect.y0,
            track_rect.x0 + face.value * track_rect.width(),
            track_rect.y1,
        );
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            filled,
            None,
            &RoundedRect::from_rect(active, track_radius),
        );

        let knob_radius = face.knob_radius();
        let knob_center = Point::new(active.x1, face.rect.center().y);
        let knob_shape = Circle::new(knob_center, knob_radius);
        // One thumb colour held across rest / hover / drag; hover and drag read from the radius
        // (above) and the halo.
        let (fill_color, border_color) = thumb_colors(theme, filled, unfilled);
        let border_width = KNOB_BORDER;
        // Pressed halo: nested hairlines outside the knob, derived against
        // the track they ring, drawn first so the knob sits cleanly on top.
        if face.dragging {
            for (r, col) in [
                (
                    knob_radius + 4.0,
                    tilo_ui_base::tokens::colors::Colors::hairline_on(
                        unfilled,
                        tilo_ui_base::chrome::HAIRLINE_ALPHA,
                    ),
                ),
                (
                    knob_radius + 2.0,
                    tilo_ui_base::tokens::colors::Colors::hairline_on(
                        unfilled,
                        tilo_ui_base::chrome::GLOW_ALPHA,
                    ),
                ),
            ] {
                scene.stroke(
                    &vello::kurbo::Stroke::new(1.0),
                    transform,
                    col,
                    None,
                    &Circle::new(knob_center, r),
                );
            }
        }
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            fill_color,
            None,
            &knob_shape,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(border_width),
            transform,
            border_color,
            None,
            &knob_shape,
        );

        if face.focused {
            scene.stroke(
                &vello::kurbo::Stroke::new(1.5),
                transform,
                theme.tokens.colors.intent.accent_ink,
                None,
                &Circle::new(knob_center, knob_radius + 2.0),
            );
        }
        if face.disabled {
            scene.pop_layer();
        }
    }
}

/// A slider's stops (`fractions` of its range, `0..=1`), as dots along the track: those the
/// knob has passed cut out of the fill, the rest dim on the bare track, so a history reads as
/// moments along it.
pub fn paint_stops(
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    face: &SliderFace,
    fractions: &[f64],
) {
    let track = face.track_rect();
    let passed = theme.tokens.colors.surfaces.raised;
    let ahead = theme.tokens.colors.text_tertiary;
    let y = track.center().y;
    let radius = track.height() / 4.0;
    // Stops closer than a dot apart read as one: a long history stays a row of dots, not a bar.
    let mut last: Option<f64> = None;
    for fraction in fractions {
        let fraction = fraction.clamp(0.0, 1.0);
        let x = track.x0 + fraction * track.width();
        if last.is_some_and(|last| x - last < radius * 4.0) {
            continue;
        }
        last = Some(x);
        let colour = if fraction <= face.value {
            passed
        } else {
            ahead
        };
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            colour,
            None,
            &Circle::new(Point::new(x, y), radius),
        );
    }
}

/// The value a held knob would land on, `label`, on a chip above the knob, kept over the
/// slider's own span so it never leaves the control it reads.
pub fn paint_held_readout(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    face: &SliderFace,
    label: &str,
) {
    let reach = SliderFace::overhang();
    let knob = Rect::from_center_size(face.knob_centre(), (2.0 * reach, 2.0 * reach));
    let span = Rect::new(
        face.rect.x0 - reach,
        knob.y0 - readout_height(theme),
        face.rect.x1 + reach,
        knob.y1,
    );
    crate::widget::tooltip::draw_hover_tooltip(scene, transform, text, theme, knob, label, span);
}

/// The width a shown value takes from the track: the widest value, a gap, and the knob's reach
/// past the track's end, so a knob at the top of the range never stands on the value.
fn value_area(widest: f64, theme: &Theme) -> f64 {
    widest + f64::from(theme.gap_standard) + SliderFace::overhang() + KNOB_BORDER
}

/// How far above its knob a held slider's readout reaches: the tooltip
/// chip above it in `theme`.
fn readout_height(theme: &Theme) -> f64 {
    crate::widget::tooltip::chip_reach_above(theme)
}

/// Where a held readout may stand over a slider laid out in `rect`,
/// `spare` being [`readout_height`] in the slider's theme.
fn readout_span(rect: Rect, spare: f64) -> Rect {
    let reach = SliderFace::overhang();
    Rect::new(rect.x0 - reach, rect.y0 - spare, rect.x1 + reach, rect.y1)
}

/// How far past its rect a readout chip paints in `theme`: its edge and
/// its floating shadow.
fn readout_bleed(theme: &Theme) -> vello::kurbo::Insets {
    let chip = Rect::new(0.0, 0.0, 1.0, 1.0);
    let painted = crate::widget::tooltip::chip_paint_bounds(chip, theme);
    painted - chip
}

/// The thumb's fill and ring over a track whose halves are `filled` and
/// `unfilled`.
fn thumb_colors(
    theme: &Theme,
    filled: vello::peniko::Color,
    unfilled: vello::peniko::Color,
) -> (vello::peniko::Color, vello::peniko::Color) {
    let colors = &theme.tokens.colors;
    let floor = tilo_ui_base::tokens::colors::NON_TEXT_MIN_CONTRAST;
    if crate::widget::toggle::light_material(colors) {
        let ring = crate::widget::toggle::ink_across(&[unfilled], colors.text_tertiary, floor);
        (colors.surfaces.overlay, ring)
    } else {
        let ring = crate::widget::toggle::ink_across(&[filled], colors.surfaces.content, floor);
        (colors.text_primary, ring)
    }
}

impl Widget for Slider {
    tilo_ui_base::described!();

    tilo_ui_base::provides!(
        dyn tilo_ui_base::widget::controls::ActionControls,
        #[cfg(feature = "model")]
        dyn tilo_ui_base::widget::permission::PermissionGuarded,
        dyn tilo_ui_base::presence::capability::PublishesPresence
    );

    /// A slider repaints when what it paints changed since the frame on screen: its value, its
    /// thumb on its way, its state.
    fn needs_redraw(&self) -> bool {
        self.painted != Some(self.painted_now())
    }

    /// A slider the frame left out (scrolled out of its view) shows nothing that could be stale:
    /// it rests as it stands, and is drawn whole once it comes back into view.
    fn mark_drawn(&mut self) {
        if let Some(drawing) = self.drawing.take() {
            self.painted = Some(drawing);
        } else {
            self.painted = Some(self.painted_now());
        }
    }

    fn needs_animation_frame(&self) -> bool {
        self.thumb.is_moving(tilo_ui_base::motion::frame_now())
    }

    fn set_interaction_environment(
        &mut self,
        environment: tilo_ui_base::interaction::InteractionEnvironment,
    ) {
        self.interaction = environment;
        self.is_hovered &= environment.supports_hover();
    }

    fn take_structure_dirty(&mut self) -> bool {
        if self
            .layout_target
            .is_some_and(|target| target.to_bits() != self.interaction.minimum_target().to_bits())
        {
            self.layout_target = None;
            true
        } else {
            false
        }
    }

    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        if let Some(binding) = &mut self.binding
            && let Pulled::Value(value) = binding.pull(state, root)
        {
            self.value = self.constrain(value);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.layout_target = Some(self.interaction.minimum_target());
        self.slop = f64::from(theme.tokens.metrics.click_slop);
        self.readout_reach = (readout_height(theme), readout_bleed(theme));
        // A slider fills its row but never collapses to a knob: the track
        // keeps six knob widths of travel.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a knob radius in logical pixels, far inside f32's range"
        )]
        let min_width = (KNOB_RADIUS * 12.0) as f32 * theme.ui_scale;
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(self.interaction.minimum_target()),
            },
            min_size: Size {
                width: Dimension::length(min_width),
                height: Dimension::auto(),
            },
            flex_shrink: 0.0,
            ..Default::default()
        };
        let identity = self.identity().map(str::to_owned);
        if let Some((kept, _)) = self.hand_on.build(identity.as_deref()) {
            self.take_up(kept);
        }
        taffy.widget_leaf(style, node_map, self)
    }

    /// Value, hover and drag change the paint, not this leaf's intrinsic size.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
        if let Some((kept, _)) = self.hand_on.place(self.rect) {
            self.take_up(kept);
        }
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        if self.shows_value {
            let style = theme.text(tilo_ui_base::tokens::TextRole::Body).numeric();
            // The area fits the widest value the slider shows, so the track
            // does not shift as the value changes.
            let widest = [
                self.range.0,
                self.range.1,
                f64::midpoint(self.range.0, self.range.1),
            ]
            .into_iter()
            .map(|value| {
                let text = self.spoken_value_of(value);
                f64::from(text_renderer.measure_style(&text, None, &style).0)
            })
            .fold(0.0_f64, f64::max);
            self.value_area = value_area(widest, theme).min(self.rect.width() / 2.0);
            let text = self.spoken_value();
            let (width, height) = text_renderer.measure_style(&text, None, &style);
            let ink = if self.effectively_enabled() {
                theme.text_dim
            } else {
                theme.text_tertiary
            };
            text_renderer.draw_style(
                scene,
                &text,
                ink,
                transform.pre_translate(
                    (
                        // Leading in its area, beside the knob's end of travel: the value reads
                        // with the track rather than off at the row's far edge.
                        self.rect.x1 - widest.max(f64::from(width)),
                        self.rect.y0 + (self.rect.height() - f64::from(height)) / 2.0,
                    )
                        .into(),
                ),
                None,
                &style,
            );
        } else {
            self.value_area = 0.0;
        }
        self.motion = Some(theme.motion());
        // The thumb is on the value, unless it glides there from a press: a
        // key, an assistive action or a sync puts it there at once.
        let value = self.fraction();
        if !self.thumb_placed || self.thumb.target().to_bits() != value.to_bits() {
            self.thumb.snap(value);
            self.thumb_placed = true;
        }
        let now = tilo_ui_base::motion::frame_now();
        self.drawing = Some(self.painted_now());
        let shown = crate::widget::shown::interaction(crate::widget::Interaction {
            hovered: self.is_hovered,
            pressed: self.is_dragging,
            focused: self.is_focused,
            disabled: !self.effectively_enabled(),
        });
        let face = SliderFace::new(self.track_rect(), self.thumb.get(now))
            .hovered(shown.hovered)
            .dragging(shown.pressed)
            .focused(shown.focused)
            .disabled(shown.disabled);
        Slider::paint(scene, transform, theme, &face);
        if !self.stops.is_empty() {
            let (min, max) = self.range;
            let span = (max - min).max(f64::EPSILON);
            let fractions: Vec<f64> = self.stops.iter().map(|stop| (stop - min) / span).collect();
            paint_stops(scene, transform, theme, &face, &fractions);
        }
        tilo_ui_base::widget::draw_over(
            self,
            &mut tilo_ui_base::draw::DrawCtx::new(
                scene,
                transform,
                text_renderer,
                theme,
                env.clone(),
            ),
        );
    }

    fn draw_overlay(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        self.readout_drawn = self.reads_out && self.is_held() && self.effectively_enabled();
        if self.readout_drawn {
            let face = SliderFace::new(self.track_rect(), self.fraction());
            let label = self.spoken_value();
            paint_held_readout(scene, transform, text_renderer, theme, &face, &label);
        }
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        if self.needs_redraw() || self.needs_animation_frame() {
            regions.push(self.rect);
        }
        if self.readout_drawn || (self.reads_out && self.is_held()) {
            let (spare, bleed) = self.readout_reach;
            regions.push(readout_span(self.rect, spare) + bleed);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
        if !self.effectively_enabled() {
            self.touch.cancel();
            if self.enabled
                && let Some(at) = self.refused_press.pressed(event, ctx, self.hit_band())
            {
                let tried = self.value_from_cursor_x(at.x);
                if tried.total_cmp(&self.value).is_ne()
                    && let Some(action) = &mut self.action
                    && let Err(error) = action.press(tried)
                {
                    tracing::warn!(%error, "a refused slider press could not be sent");
                }
            }
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = ctx.logical(position);
                self.last_cursor = p;
                let hover = self.hit_band().contains(p);
                if hover != self.is_hovered {
                    self.is_hovered = hover;
                    *ctx.handled = true;
                }
                if self.is_dragging && !self.touch.is_active() {
                    let fine = ctx.modifiers.state().shift_key();
                    self.drag_to(p, fine, ctx);
                }
            }
            WindowEvent::MouseInput {
                state: element_state,
                button: MouseButton::Left,
                ..
            } => {
                if *element_state == ElementState::Pressed
                    && self.hit_band().contains(self.last_cursor)
                {
                    self.press_at(
                        self.last_cursor,
                        tilo_ui_base::tokens::MotionSource::Pointer,
                        ctx,
                    );
                } else if *element_state == ElementState::Released
                    && self.is_dragging
                    && !self.touch.is_active()
                {
                    self.release(ctx);
                }
            }
            WindowEvent::Touch(touch) => {
                let band = self.hit_band();
                match self
                    .touch
                    .step(touch, ctx.scale_factor, |point| band.contains(point))
                {
                    SliderTouchStep::Press(point) => {
                        self.press_at(point, tilo_ui_base::tokens::MotionSource::Gesture, ctx);
                    }
                    SliderTouchStep::Move(point) => {
                        self.drag_to(point, false, ctx);
                        // The finger is the slider's even where the value
                        // does not move: it never scrolls what is beneath.
                        *ctx.handled = true;
                    }
                    SliderTouchStep::Release => {
                        if self.is_dragging {
                            self.release(ctx);
                        }
                        *ctx.handled = true;
                    }
                    SliderTouchStep::Ignored => {}
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if self.handle_key(event, ctx.state.doc(), ctx.path) =>
            {
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn claims_raw_touch(&self, point: Point) -> bool {
        self.effectively_enabled() && self.hit_band().contains(point)
    }

    /// While dragging, the slider must keep receiving pointer events even
    /// when the cursor leaves its `rect` — otherwise releasing outside
    /// would silently strand the drag in the engaged state.
    fn wants_pointer_capture(&self) -> bool {
        self.is_dragging || self.touch.is_active()
    }

    fn name(&self) -> &'static str {
        "Slider"
    }

    fn test_id(&self) -> Option<&str> {
        Some(self.id_override.as_deref().unwrap_or("slider"))
    }

    fn is_focusable(&self) -> bool {
        true
    }
    fn set_focus(&mut self, focused: bool) {
        self.is_focused = focused;
    }
    fn is_focused(&self) -> bool {
        self.is_focused
    }
    fn is_enabled(&self) -> bool {
        self.effectively_enabled()
    }

    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        use tilo_ui_base::{a11y::AccessNode, a11y::AccessState, a11y::Role};
        // A `0..1` slider speaks a percentage ("50 percent"); a ranged one
        // its value, or the app's own words for it (`value_text`).
        let name = self
            .accessible_name
            .as_deref()
            .and_then(|name| tilo_ui_base::i18n::spoken_name(name, self.name_resolved));
        let mut node = AccessNode::new(Role::Slider)
            .with_semantic_key(self.identity().unwrap_or("slider"))
            .with_optional_name(name)
            .with_value(self.spoken_value())
            .with_state(
                AccessState::new()
                    .focusable(self.is_focusable())
                    .focused(self.is_focused())
                    .disabled(!self.is_enabled()),
            );
        if self.action.is_none() {
            node = node.with_action_target(self.test_id().unwrap_or("slider"));
        }
        Some(node)
    }

    /// Accessibility: an AT can step the slider with
    /// [`Action::Increment`]/[`Action::Decrement`] (5% per step) or jump it to an absolute
    /// value with [`Action::SetValue`] (accepting either a `0..1` fraction or a `0..100`
    /// percentage).
    fn perform_action(
        &mut self,
        action: &tilo_ui_base::a11y::Action,
        _key_path: &[u64],
        state: &mut tilo_ui_base::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        use tilo_ui_base::a11y::Action;
        if !self.effectively_enabled() {
            return false;
        }
        // An assistive step is five arrow steps (5% of a stepless range).
        let step = self.arrow_step() * 5.0;
        match action {
            Action::Increment => {
                let target = self.moved_by(step);
                self.commit_value(target, state.doc(), path)
            }
            Action::Decrement => {
                let target = self.moved_by(-step);
                self.commit_value(target, state.doc(), path)
            }
            Action::SetValue(v) => {
                // `f64::from_str` accepts "nan"/"inf"/"-inf" as valid text,
                // which would otherwise slip a non-finite value into the
                // CRDT and any serialiser downstream that (rightly) refuses
                // non-finite floats.
                let Ok(parsed) = v.trim().trim_end_matches('%').parse::<f64>() else {
                    return false;
                };
                if !parsed.is_finite() {
                    return false;
                }
                // A 0..1 slider takes a fraction or a percentage; a ranged
                // one its own units.
                let target = if self.range == (0.0, 1.0) && (parsed > 1.0 || v.contains('%')) {
                    parsed / 100.0
                } else {
                    parsed
                };
                let target = self.constrain(target);
                self.commit_value(target, state.doc(), path)
            }
            _ => false,
        }
    }
}

impl tilo_ui_base::presence::capability::PublishesPresence for Slider {
    fn presence_key(&self) -> Option<PresenceKey> {
        PresenceKey::widget(&self.presence_path)
    }
}

#[cfg(feature = "model")]
impl tilo_ui_base::widget::permission::PermissionGuarded for Slider {
    fn permission_scope(&self) -> Option<PermissionScope> {
        self.permission_scope.clone()
    }
}

impl tilo_ui_base::widget::controls::ActionControls for Slider {
    tilo_ui_base::optional_emitter_controls!();
    fn control_action_bindings(&self) -> Vec<ControlBindingMetadata> {
        let id = self.test_id().unwrap_or("slider");
        tilo_ui_base::widget::controls::value_control_bindings(
            id,
            self.action.as_ref(),
            self.binding.as_ref().and_then(|b| b.document_key()),
        )
    }
}

#[derive(Default)]
struct KeptThumb {
    thumb: tilo_ui_base::motion::Thumb,
    placed: bool,
    glide_from: Option<(Point, tilo_ui_base::tokens::MotionSource)>,
    /// The hand holding the slider, when the view rebuilt it mid-drag (it
    /// does on every value a drag passes): the successor goes on with it.
    held: Option<HeldDrag>,
}

/// A drag under way: the finger or the pointer holding the slider, where
/// the drag began and what it has done so far.
#[derive(Clone, Copy)]
struct HeldDrag {
    touch: SliderTouch,
    drag_from: Option<f64>,
    fine_from: Option<(f64, f64)>,
    gesture: bool,
    last_cursor: Point,
}

impl Drop for Slider {
    fn drop(&mut self) {
        let kept = KeptThumb {
            thumb: std::mem::take(&mut self.thumb),
            placed: self.thumb_placed,
            glide_from: self.glide_from,
            held: self.is_dragging.then_some(HeldDrag {
                touch: self.touch,
                drag_from: self.drag_from,
                fine_from: self.fine_from,
                gesture: self.gesture,
                last_cursor: self.last_cursor,
            }),
        };
        self.hand_on.leave(kept);
    }
}

#[cfg(test)]
#[path = "slider_tests.rs"]
mod tests;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support {
    #![allow(
        dead_code,
        unused_imports,
        clippy::arithmetic_side_effects,
        clippy::disallowed_methods,
        clippy::expect_used,
        clippy::float_cmp,
        clippy::implicit_hasher,
        clippy::indexing_slicing,
        clippy::must_use_candidate,
        clippy::needless_pass_by_value,
        clippy::panic,
        clippy::unused_self,
        clippy::unwrap_used,
        clippy::wildcard_imports,
        reason = "test-support helpers are compiled for every test build whether or not a test uses this one"
    )]
    use super::*;
    use std::cell::*;
    use std::collections::*;
    use std::path::*;
    use std::rc::*;
    use std::sync::atomic::*;
    use std::sync::*;
    use std::time::*;
    use taffy::{AvailableSpace, Size};
    use tilo_ui::kurbo::*;
    use tilo_ui::testing::Dispatch;
    use tilo_ui::vello::*;
    use tilo_ui::widget::*;
    use tilo_ui::*;
    use tilo_ui_base::a11y::*;
    use tilo_ui_base::component::Event;
    use tilo_ui_base::component::*;
    use tilo_ui_base::icons::*;
    use tilo_ui_base::interaction::*;
    use tilo_ui_base::paint::*;
    use tilo_ui_base::presence::*;
    use tilo_ui_base::tokens::*;
    use tilo_ui_base::widget::*;
    use tilo_ui_base::*;
    use winit::event::*;
    use winit::keyboard::*;
    /// Build a slider with a known rect so we don't need a full taffy pass.
    /// Rect spans [0..200, 0..30]; the track runs flush with it.
    pub fn slider_at(x0: f64, y0: f64, x1: f64, y1: f64) -> Slider {
        let mut s = Slider::new("test", "value");
        s.rect = Rect::new(x0, y0, x1, y1);
        s
    }
    pub fn finger(phase: winit::event::TouchPhase, x: f64, y: f64) -> WindowEvent {
        WindowEvent::Touch(winit::event::Touch {
            device_id: winit::event::DeviceId::dummy(),
            phase,
            location: winit::dpi::PhysicalPosition::new(x, y),
            force: None,
            id: 3,
        })
    }
    pub fn present(s: &mut Slider, theme: &Theme) {
        s.draw(&mut tilo_ui_base::draw::DrawCtx::new(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            theme,
            tilo_ui_base::presence::PresenceCtx::empty().draw_env(),
        ));
        s.mark_drawn();
    }
}

/// What a slider's tests read of it from outside the module.
#[cfg(feature = "test-support")]
impl Slider {
    /// The knob the person drags.
    #[must_use]
    pub fn test_thumb(&self) -> &tilo_ui_base::motion::Thumb {
        &self.thumb
    }

    /// Where the value stands in its range, from 0 to 1.
    #[must_use]
    pub fn test_fraction(&self) -> f64 {
        self.fraction()
    }
}
