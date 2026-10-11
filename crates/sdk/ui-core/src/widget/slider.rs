use crate::permissions::PermissionScope;
use crate::presence::{PathSegment, PresenceKey};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::binding::{BoxedBinding, Pulled};
use crate::widget::{ActionEmitter, ControlBindingMetadata};
use crate::widget::{AppModel, ModelPath};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

/// The track runs flush with the slider's rect; the knob overhangs its ends
/// by its radius, so the track lines up with the content edges beside it.
const TRACK_PADDING: f64 = 0.0;
/// Page Up / Page Down move this share of the range.
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
/// Thickness of the slider track.
const TRACK_HEIGHT: f64 = 6.0;

/// Knob radius at rest. Hover and press scale this — see the
/// `_SCALE` constants below. Pulled out so tests can pin the
/// idle-vs-hover-vs-press contract without hard-coding pixels.
const KNOB_RADIUS: f64 = 10.0;
const KNOB_RADIUS_HOVER_SCALE: f64 = 1.08;
const KNOB_RADIUS_PRESS_SCALE: f64 = 1.15;

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent pointer, focus, drag, and enabled presentation states"
)]
pub struct Slider {
    semantic_id: String,
    rect: Rect,
    is_hovered: bool,
    is_dragging: bool,
    is_focused: bool,
    /// The value, in `range` units.
    value: f64,
    /// The values the slider spans; `0.0..=1.0` unless set.
    range: (f64, f64),
    /// Values snap to `range.0 + k * step`.
    step: Option<f64>,
    /// How the value is spoken; a percentage of a `0..1` range otherwise.
    value_text: Option<ValueText>,
    /// Where the value lives: a document key or process-local state.
    binding: Option<BoxedBinding<f64>>,
    on_change: Option<Box<dyn FnMut(f64)>>,
    /// Invoke the bound action once per drag, when it is let go.
    settles: bool,
    /// The value when the current drag began.
    drag_from: Option<f64>,
    /// Where the pointer and the value were when Shift was pressed during
    /// a drag: from there the value moves a tenth as fast, to place it
    /// finely.
    fine_from: Option<(f64, f64)>,
    action: Option<ActionEmitter<f64>>,
    presence_path: Vec<PathSegment>,
    permission_scope: Option<PermissionScope>,
    /// When `false` the slider is disabled: skipped by focus, rejects
    /// input (no drag, no value change, no hover feedback) and renders
    /// dimmed. Default `true`.
    enabled: bool,
    /// Last observed cursor position in logical-pixel space. Updated on
    /// every `CursorMoved`, used by `MouseInput::Pressed` so a press can
    /// (a) decide whether it's inside the slider's rect without relying
    /// on a prior hover edge transition and (b) jump the value to the
    /// click point immediately — the standard "click on track to jump"
    /// affordance.
    last_cursor: Point,
    /// Optional override for the `Widget::test_id()` value. When `None`
    /// the slider returns the role-style default (`"slider"`); when set
    /// callers can disambiguate multiple sliders in the same surface
    /// (e.g. `"volume"`, `"brightness"`).
    id_override: Option<String>,
    /// Spoken name for a slider with no adjacent text label, so a screen
    /// reader announces what is being adjusted ("Volume, 50 percent")
    /// instead of just the value. When unset a sibling label is resolved
    /// by the tree builder.
    accessible_name: Option<String>,
    interaction: crate::interaction::InteractionEnvironment,
    /// The target height encoded in the current layout style.
    layout_target: Option<f32>,
}

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
            is_focused: false,
            value: 0.5,
            range: (0.0, 1.0),
            step: None,
            value_text: None,
            binding: Some(crate::doc_source::number_key(on_change_path)),
            on_change: None,
            settles: false,
            drag_from: None,
            fine_from: None,
            action: None,
            presence_path: Vec::new(),
            permission_scope: None,
            enabled: true,
            last_cursor: Point::ZERO,
            id_override: None,
            accessible_name: None,
            interaction: crate::interaction::InteractionEnvironment::default(),
            layout_target: None,
        }
    }

    /// Create a normalized slider bound to typed process-local state.
    ///
    /// Pointer drags and accessibility actions update `state`; mutations
    /// through any clone update this slider on the next synchronization pass.
    /// Values are clamped to `0.0..=1.0` at the control boundary.
    pub fn from_state(state: &crate::LocalState<f64>) -> Self {
        let (value, binding) = super::binding::local(state);
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

    /// Say the value in the slider's own terms ("1.5 s", "Medium") to
    /// assistive technology.
    #[must_use]
    pub fn value_text(mut self, format: impl Fn(f64) -> String + 'static) -> Self {
        self.value_text = Some(Box::new(format));
        self
    }

    /// Clamp `value` into the range and onto the step grid.
    fn constrain(&self, value: f64) -> f64 {
        let (min, max) = self.range;
        if !value.is_finite() {
            return min;
        }
        let value = value.clamp(min, max);
        match self.step {
            Some(step) => (min + ((value - min) / step).round() * step).clamp(min, max),
            None => value,
        }
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

    /// The value at a share of the range.
    fn at_fraction(&self, fraction: f64) -> f64 {
        let (min, max) = self.range;
        self.constrain(min + fraction.clamp(0.0, 1.0) * (max - min))
    }

    /// One arrow-key step.
    fn arrow_step(&self) -> f64 {
        self.step
            .unwrap_or((self.range.1 - self.range.0) * ARROW_FRACTION)
    }

    /// One page: a tenth of the range, at least one step.
    fn page_step(&self) -> f64 {
        ((self.range.1 - self.range.0) * PAGE_FRACTION).max(self.arrow_step())
    }

    /// What assistive technology says for the value.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the rounded percentage is bounded to the inclusive range 0..=100"
    )]
    fn spoken_value(&self) -> String {
        if let Some(format) = &self.value_text {
            return format(self.value);
        }
        if self.range == (0.0, 1.0) {
            format!("{}%", (self.fraction() * 100.0).round() as i64)
        } else {
            plain_number(self.value)
        }
    }

    /// Observe values produced by user interaction.
    ///
    /// Model synchronization and [`Self::set_value`] do not invoke this
    /// callback.
    #[must_use]
    pub fn on_change(mut self, callback: impl FnMut(f64) + 'static) -> Self {
        if self.action.is_none() {
            self.on_change = Some(Box::new(callback));
        }
        self
    }

    /// Invoke the bound action once per drag, with the value it is let go
    /// at, rather than at every step of it; a key or an assistive action
    /// still invokes it at once. For an action that is an undoable edit, so
    /// one drag is one step.
    #[must_use]
    pub fn commits_on_release(mut self) -> Self {
        self.settles = true;
        self
    }

    /// Route all pointer and accessibility value changes through an action.
    #[must_use]
    pub fn on_action(mut self, binding: tilo_actions::ActionBinding<f64>) -> Self {
        self.action = Some(ActionEmitter::new(binding));
        self.on_change = None;
        self
    }

    /// The value currently presented, in the range's units.
    pub fn value(&self) -> f64 {
        self.value
    }

    /// Set the value programmatically (in the range's units) and update a
    /// bound local model without invoking [`Self::on_change`].
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

    /// Opt this widget into presence tracking by binding it to a CRDT-style path.
    #[must_use]
    pub fn with_presence(mut self, path: Vec<PathSegment>) -> Self {
        self.presence_path = path;
        self
    }

    /// Override the value returned from `Widget::test_id()`. Without a
    /// custom id the slider reports `"slider"` (the role-style default);
    /// callers with more than one slider in the same frame can disambiguate
    /// them with `with_id("volume")` etc.
    #[must_use]
    pub fn with_id(mut self, id: &str) -> Self {
        self.id_override = Some(id.to_string());
        self
    }

    /// Gate this widget behind a permission scope.
    #[must_use]
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
    /// what it adjusts alongside the value.
    #[must_use]
    pub fn with_accessible_name(mut self, name: &str) -> Self {
        self.accessible_name = Some(name.to_string());
        self
    }

    /// Imperatively flip the enabled state.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Map a cursor x in widget-local logical pixels to the slider's
    /// 0..1 value, clamped at both ends.
    fn value_from_cursor_x(&self, cursor_x: f64) -> f64 {
        let fraction = SliderFace::new(self.rect, 0.0).value_at(Point::new(cursor_x, 0.0));
        self.at_fraction(fraction)
    }

    /// Arrow, Page and Home/End keys while focused.
    fn handle_key(&mut self, key: &KeyEvent, state: &AppModel, root: &ModelPath) -> bool {
        if !self.is_focused || key.state != ElementState::Pressed {
            return false;
        }
        let target = match &key.logical_key {
            Key::Named(NamedKey::ArrowRight | NamedKey::ArrowUp) => self.value + self.arrow_step(),
            Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowDown) => self.value - self.arrow_step(),
            Key::Named(NamedKey::PageUp) => self.value + self.page_step(),
            Key::Named(NamedKey::PageDown) => self.value - self.page_step(),
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

    /// Commit a new value to the CRDT iff it differs from the cached
    /// `value` by more than a frame-of-noise epsilon. Returns `true` if
    /// the commit happened so callers can flag the event as handled.
    fn commit_value(&mut self, new_value: f64, _state: &AppModel, _root: &ModelPath) -> bool {
        let span = (self.range.1 - self.range.0).abs().max(f64::EPSILON);
        if (new_value - self.value).abs() <= 0.001 * span {
            return false;
        }
        self.value = new_value;
        if let Some(action) = &mut self.action {
            if !(self.settles && self.is_dragging) {
                let _ = action.emit(self.value);
            } else if let Some(binding) = self
                .binding
                .as_mut()
                .filter(|binding| binding.document_key().is_none())
            {
                // The value a drag will settle on, shown to whatever reads
                // the control's local state (a label, a live preview) before
                // the action is invoked at release.
                binding.publish(&self.value);
            }
        } else if let Some(binding) = &mut self.binding {
            binding.publish(&self.value);
        }
        if self.action.is_none()
            && let Some(callback) = self.on_change.as_mut()
        {
            callback(self.value);
        }
        true
    }
}

/// An immediate-mode slider face: everything [`Slider::paint`] needs to draw
/// one, with no widget instance and no CRDT binding.
///
/// A canvas app that owns its own hit-testing (a timeline inspector, a mixer
/// strip) cannot mount a retained [`Slider`] per control — it has no Taffy
/// node for them, and it needs a whole drag to land as one undo boundary. It
/// can still share the *look*, which is what this is for. Mirrors
/// [`Button::paint`](crate::widget::Button::paint) and
/// [`ToggleGroup::paint`](crate::widget::ToggleGroup::paint).
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
    /// a host keeps this much room either side of a slider's rect.
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
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                crate::widget::DISABLED_ALPHA,
                transform,
                &face.rect,
            );
        }
        let track_rect = face.track_rect();
        // Capsule ends: the radius is the track's own half-height, so it
        // follows a taller track instead of a literal that once matched one.
        let track_radius = track_rect.height() / 2.0;
        // HIG §9.5: the unfilled half is a lift off the ground (not a ramp
        // slot) with an edge derived from itself; the filled half is accent.
        let unfilled = theme.tokens.colors.intent.selection;
        let filled = theme.tokens.colors.intent.inverted;
        let track_shape = RoundedRect::from_rect(track_rect, track_radius);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            unfilled,
            None,
            &track_shape,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            crate::surface::edge_on(unfilled),
            None,
            &crate::surface::edge_outline(track_rect, track_radius),
        );

        // Filled portion of the track.
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

        // Knob — radius scales with state so hover/press are distinct even
        // before you read the colours.
        let knob_radius = face.knob_radius();
        let knob_center = Point::new(active.x1, face.rect.center().y);
        let knob_shape = Circle::new(knob_center, knob_radius);
        // One thumb colour held across rest / hover / drag; hover and drag
        // read from the radius (above) and the halo. On a light material the
        // thumb is a raised white object ringed in a tone that clears both
        // halves of the track, never a dark disc; on a dark one it is the
        // primary ink (HIG §9.5), ringed in the ground so it stays a disc of
        // its own where it sits on the filled half, which is the same ink.
        let (fill_color, border_color) = thumb_colors(theme, filled, unfilled);
        let border_width = 1.5;
        // Pressed halo: nested hairlines outside the knob, derived against
        // the track they ring, drawn first so the knob sits cleanly on top.
        if face.dragging {
            for (r, col) in [
                (
                    knob_radius + 4.0,
                    crate::tokens::colors::Colors::hairline_on(
                        unfilled,
                        crate::chrome::HAIRLINE_ALPHA,
                    ),
                ),
                (
                    knob_radius + 2.0,
                    crate::tokens::colors::Colors::hairline_on(unfilled, crate::chrome::GLOW_ALPHA),
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

        // Tight focus ring, in your colour, around the knob, not the whole
        // widget rect.
        if face.focused {
            scene.stroke(
                &vello::kurbo::Stroke::new(1.5),
                transform,
                theme.tokens.colors.intent.you,
                None,
                &Circle::new(knob_center, knob_radius + 2.0),
            );
        }
        if face.disabled {
            scene.pop_layer();
        }
    }
}

/// The thumb's fill and ring over a track whose halves are `filled` and
/// `unfilled`.
fn thumb_colors(
    theme: &Theme,
    filled: vello::peniko::Color,
    unfilled: vello::peniko::Color,
) -> (vello::peniko::Color, vello::peniko::Color) {
    let colors = &theme.tokens.colors;
    let floor = crate::tokens::colors::NON_TEXT_MIN_CONTRAST;
    if crate::widget::toggle::light_material(colors) {
        let ring =
            crate::widget::toggle::ink_across(&[filled, unfilled], colors.text_tertiary, floor);
        (colors.surfaces.raised, ring)
    } else {
        let ring = crate::widget::toggle::ink_across(&[filled], colors.surfaces.content, floor);
        (colors.text_primary, ring)
    }
}

impl Widget for Slider {
    crate::provides!(
        dyn crate::widget::controls::ActionControls,
        dyn crate::permissions::capability::PermissionGated,
        dyn crate::presence::capability::PublishesPresence
    );

    fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
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
        taffy.widget_leaf(style, node_map, self)
    }

    /// Value, hover and drag change the paint, not this leaf's intrinsic size.
    /// Theme and viewport changes rebuild the host layout; target changes
    /// explicitly invalidate the authored style in `take_structure_dirty`.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        Slider::paint(
            scene,
            transform,
            theme,
            &SliderFace::new(self.rect, self.fraction())
                .hovered(self.is_hovered)
                .dragging(self.is_dragging)
                .focused(self.is_focused)
                .disabled(!self.effectively_enabled()),
        );
        crate::widget::draw_gate_and_peer_focus(self, scene, transform, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        // Disabled: reject all input — no drag, no value change, no hover.
        if !self.effectively_enabled() {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.last_cursor = p;
                let hover = self.rect.inflate(KNOB_RADIUS, 0.0).contains(p);
                if hover != self.is_hovered {
                    self.is_hovered = hover;
                    *ctx.handled = true;
                }

                if self.is_dragging {
                    let new_value = if ctx.modifiers.state().shift_key() {
                        let (x, value) = *self.fine_from.get_or_insert((p.x, self.value));
                        let track = (self.rect.width() - TRACK_PADDING * 2.0).max(1.0);
                        (value + (p.x - x) / track * Self::FINE_DRAG).clamp(0.0, 1.0)
                    } else {
                        self.fine_from = None;
                        self.value_from_cursor_x(p.x)
                    };
                    if self.commit_value(new_value, ctx.state.doc(), ctx.path) {
                        *ctx.handled = true;
                    }
                }
            }
            WindowEvent::MouseInput {
                state: element_state,
                button: MouseButton::Left,
                ..
            } => {
                // Press uses `rect.contains(last_cursor)` rather than
                // `is_hovered` so the press latches without depending on
                // an earlier hover-edge transition (e.g. focus delivered
                // a press without a preceding `CursorMoved`). On entry
                // we also jump `value` to the click x — the standard
                // "press on track to jump" affordance.
                if *element_state == ElementState::Pressed
                    && self
                        .rect
                        .inflate(KNOB_RADIUS, 0.0)
                        .contains(self.last_cursor)
                {
                    self.is_dragging = true;
                    self.drag_from = Some(self.value);
                    *ctx.focus_target = Some(ctx.node_id);
                    *ctx.handled = true;
                    let new_value = self.value_from_cursor_x(self.last_cursor.x);
                    self.commit_value(new_value, ctx.state.doc(), ctx.path);
                } else if *element_state == ElementState::Released && self.is_dragging {
                    self.is_dragging = false;
                    self.fine_from = None;
                    *ctx.handled = true;
                    let moved = self
                        .drag_from
                        .take()
                        .is_some_and(|from| (from - self.value).abs() > 0.001);
                    if self.settles
                        && moved
                        && let Some(action) = &mut self.action
                    {
                        let _ = action.emit(self.value);
                    }
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

    /// While dragging, the slider must keep receiving pointer events even
    /// when the cursor leaves its `rect` — otherwise releasing outside
    /// would silently strand the drag in the engaged state.
    fn wants_pointer_capture(&self) -> bool {
        self.is_dragging
    }

    fn name(&self) -> &'static str {
        "Slider"
    }

    fn test_id(&self) -> Option<&str> {
        Some(self.id_override.as_deref().unwrap_or("slider"))
    }

    fn get_rect(&self) -> Rect {
        self.rect
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

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        // A `0..1` slider speaks a percentage ("50 percent"); a ranged one
        // its value, or the app's own words for it (`value_text`).
        let name = self
            .accessible_name
            .as_ref()
            .map(|n| crate::i18n!(n))
            .filter(|n| !n.is_empty());
        let mut node = AccessNode::new(Role::Slider)
            .with_semantic_key(&self.semantic_id)
            .with_optional_name(name)
            .with_value(self.spoken_value())
            .with_state(AccessState {
                focusable: self.is_focusable(),
                focused: self.is_focused(),
                disabled: !self.is_enabled(),
                ..Default::default()
            });
        if self.action.is_none() && self.on_change.is_none() {
            node = node.with_action_target(self.test_id().unwrap_or("slider"));
        }
        Some(node)
    }

    /// Accessibility: an AT can step the slider with
    /// [`Action::Increment`]/[`Action::Decrement`] (5% per step) or jump it to
    /// an absolute value with [`Action::SetValue`] (accepting either a `0..1`
    /// fraction or a `0..100` percentage). All mutations go through the same
    /// `commit_value` path real drag input uses, so the CRDT write is
    /// identical. Disabled sliders reject every action.
    ///
    /// [`Action::Increment`]: crate::a11y::Action::Increment
    /// [`Action::Decrement`]: crate::a11y::Action::Decrement
    /// [`Action::SetValue`]: crate::a11y::Action::SetValue
    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        use crate::a11y::Action;
        if !self.effectively_enabled() {
            return false;
        }
        // An assistive step is five arrow steps (5% of a stepless range).
        let step = self.arrow_step() * 5.0;
        match action {
            Action::Increment => {
                let target = self.constrain(self.value + step);
                self.commit_value(target, state.doc(), path)
            }
            Action::Decrement => {
                let target = self.constrain(self.value - step);
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

impl crate::presence::capability::PublishesPresence for Slider {
    fn presence_key(&self) -> Option<PresenceKey> {
        PresenceKey::widget(&self.presence_path)
    }
}

impl crate::permissions::capability::PermissionGated for Slider {
    fn permission_scope(&self) -> Option<PermissionScope> {
        self.permission_scope.clone()
    }
}

impl crate::widget::controls::ActionControls for Slider {
    crate::widget::controls::optional_emitter_controls!();
    fn control_action_bindings(&self) -> Vec<ControlBindingMetadata> {
        crate::widget::controls::value_control_bindings(
            self.test_id().unwrap_or("slider"),
            self.action.as_ref(),
            self.binding.as_ref().and_then(|b| b.document_key()),
            self.on_change.is_some(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Dispatch, dispatch};
    use automerge::{AutoCommit, ReadDoc};

    #[test]
    fn retained_slider_local_value_and_hover_do_not_remeasure_but_targets_and_viewport_do() {
        let value = crate::LocalState::new(0.5);
        let mut host = crate::headless::FrameHarness::new(
            crate::State::new(),
            Box::new(Slider::from_state(&value)),
            (400, 80),
        );
        host.profile_frame();
        assert!(!host.last_relayout().measured);
        let rect = host
            .targets()
            .get("slider")
            .copied()
            .expect("the actual slider is mounted");
        host.click_at(rect.x0 + rect.width() * 0.75, rect.center().y);
        assert!(!host.last_relayout().measured, "value changes paint only");
        host.verify_retained_geometry().unwrap();
        assert!(
            value.get() > 0.7 && value.get() < 0.8,
            "the ordinary pointer changes the authoritative local value"
        );
        assert!(
            host.state()
                .doc
                .get(automerge::ROOT, "volume")
                .unwrap()
                .is_none(),
            "a value control never writes its document read source"
        );
        host.set_interaction_environment(crate::InteractionEnvironment::TOUCH);
        assert!(host.last_relayout().rebuilt);
        assert!(
            host.targets()
                .get("slider")
                .copied()
                .expect("the actual slider is mounted")
                .height()
                > rect.height()
        );
        host.verify_retained_geometry().unwrap();
        host.resize((640, 120)).unwrap();
        assert!(host.last_relayout().rebuilt);
        assert!(
            host.targets()
                .get("slider")
                .copied()
                .expect("the actual slider is mounted")
                .width()
                > rect.width()
        );
        host.verify_retained_geometry().unwrap();
        host.set_theme(crate::Theme::light());
        assert!(host.last_relayout().rebuilt);
        host.verify_retained_geometry().unwrap();
    }

    /// In a crowded row a slider keeps a usable track instead of shrinking
    /// to its knob.
    #[test]
    fn a_slider_in_a_crowded_row_keeps_a_track() {
        let harness = crate::headless::FrameHarness::new(
            crate::State::new(),
            Box::new(
                crate::widget::Row::new()
                    .no_wrap()
                    .push(Box::new(crate::widget::Text::new(
                        "A label long enough to take the whole row by itself",
                    )))
                    .push(Box::new(Slider::new("volume", "volume"))),
            ),
            (200, 80),
        );
        let snapshot = harness.inspection();
        let slider = snapshot
            .root
            .children
            .iter()
            .find(|child| child.widget == "Slider")
            .expect("slider in the row");
        assert!(slider.rect[2] >= 120.0, "{:?}", slider.rect);
    }

    /// On a light material the thumb is a raised white object with a ring
    /// that clears both halves of the track, never a dark disc.
    #[test]
    fn the_light_thumb_is_raised_and_ringed() {
        let theme = Theme::light();
        let mut scene = Scene::new();
        Slider::paint(
            &mut scene,
            Affine::IDENTITY,
            &theme,
            &SliderFace::new(Rect::new(0.0, 0.0, 200.0, 32.0), 0.5),
        );
        let colors = &theme.tokens.colors;
        let raised = colors.surfaces.raised;
        let ring = crate::widget::toggle::ink_across(
            &[colors.intent.inverted, colors.intent.selection],
            colors.text_tertiary,
            crate::tokens::colors::NON_TEXT_MIN_CONTRAST,
        );
        for ground in [colors.intent.inverted, colors.intent.selection] {
            assert!(
                crate::a11y::contrast_ratio(ring, ground)
                    >= crate::tokens::colors::NON_TEXT_MIN_CONTRAST
            );
        }
        assert!(crate::a11y::relative_luminance(raised) > 0.9);
    }

    /// On a dark material the thumb is the primary ink, ringed so it reads
    /// as a disc of its own over the filled half (the same ink) and over
    /// the unfilled one.
    #[test]
    fn the_dark_thumb_is_primary_ink_and_ringed() {
        let theme = Theme::dark();
        let colors = &theme.tokens.colors;
        let (filled, unfilled) = (colors.intent.inverted, colors.intent.selection);
        let (fill, ring) = thumb_colors(&theme, filled, unfilled);
        assert_eq!(fill, colors.text_primary);
        let floor = crate::tokens::colors::NON_TEXT_MIN_CONTRAST;
        assert!(crate::a11y::contrast_ratio(ring, filled) >= floor);
        assert!(crate::a11y::contrast_ratio(fill, unfilled) >= floor);
    }

    /// Build a slider with a known rect so we don't need a full taffy pass.
    /// Rect spans [0..200, 0..30]; the track runs flush with it.
    fn slider_at(x0: f64, y0: f64, x1: f64, y1: f64) -> Slider {
        let mut s = Slider::new("test", "value");
        s.rect = Rect::new(x0, y0, x1, y1);
        s
    }

    use crate::synthetic_input::cursor_moved;

    use crate::synthetic_input::mouse_press;

    use crate::synthetic_input::mouse_release;

    #[test]
    fn slider_pointer_input_hovers_drags_and_jumps() {
        // Case `knob_radius_grows_on_hover_and_drag`.
        {
            // Idle < hover < pressed. Visual contract for state contrast —
            // if this test fails the slider has stopped scaling the knob
            // and the polish has regressed.
            let mut s = Slider::new("test", "v");
            let r0 = s.knob_radius_for_state();

            s.set_hovered_for_test(true);
            let r1 = s.knob_radius_for_state();

            s.set_dragging_for_test(true);
            let r2 = s.knob_radius_for_state();

            assert!(r0 < r1, "hover must grow knob: idle={r0}, hover={r1}");
            assert!(
                r1 < r2,
                "press must grow knob further: hover={r1}, press={r2}",
            );
        }

        // Case `drag_continues_after_cursor_leaves_rect`.
        {
            // Rect [0..200, 0..30]. The track runs flush: 200 wide.
            let mut s = slider_at(0.0, 0.0, 200.0, 30.0);

            // Move cursor inside, near the left edge of the track.
            dispatch(&mut s, &cursor_moved(20.0, 15.0));
            // Press inside the rect — drag begins.
            dispatch(&mut s, &mouse_press());
            assert!(s.is_dragging, "press inside rect must start drag");

            // Cursor leaves the rect vertically (still within the track's
            // horizontal range). The drag must continue updating `value`.
            dispatch(&mut s, &cursor_moved(150.0, 200.0));
            let expected = (150.0 / 200.0_f64).clamp(0.0, 1.0);
            assert!(
                (s.value - expected).abs() < 1e-6,
                "drag outside rect must keep tracking cursor x — got {}, expected {}",
                s.value,
                expected
            );

            // And another move further outside — value still tracks.
            dispatch(&mut s, &cursor_moved(95.0, -50.0));
            let expected2 = (95.0 / 200.0_f64).clamp(0.0, 1.0);
            assert!(
                (s.value - expected2).abs() < 1e-6,
                "drag continues to track even far outside rect — got {}, expected {}",
                s.value,
                expected2
            );

            // Release ends the drag.
            dispatch(&mut s, &mouse_release());
            assert!(!s.is_dragging, "release must end drag");
        }

        // Case `press_on_track_jumps_value`.
        {
            // Rect [0..200, 0..30]; the track runs flush, so x at 0.8 of
            // the width is 160.
            let mut s = slider_at(0.0, 0.0, 200.0, 30.0);

            // Move cursor to the press location *and* press without any
            // prior hover state being toggled by the same code path —
            // this is the "click on track to jump" affordance.
            let press_x = 0.8 * 200.0;
            dispatch(&mut s, &cursor_moved(press_x, 15.0));
            dispatch(&mut s, &mouse_press());

            assert!(s.is_dragging, "press inside rect must start drag");
            assert!(
                (s.value - 0.8).abs() < 1e-3,
                "press on track at x=0.8 of width must jump value to ≈0.8 — got {}",
                s.value
            );
        }

        // Case `disabled_slider_rejects_input`.
        {
            // Rect [0..200, 0..30]. A disabled slider must ignore press/drag.
            let mut s = slider_at(0.0, 0.0, 200.0, 30.0);
            s.set_enabled(false);
            let before = s.value;

            dispatch(&mut s, &cursor_moved(20.0, 15.0));
            dispatch(&mut s, &mouse_press());
            dispatch(&mut s, &cursor_moved(150.0, 15.0));

            assert!(!s.is_dragging, "disabled slider must not start a drag");
            assert_eq!(s.value, before, "disabled slider must not change its value");
        }
    }

    #[test]
    fn slider_identity_binding_and_enabled_state() {
        // Case `slider_test_id_default_is_slider`.
        {
            let s = Slider::new("any", "value");
            assert_eq!(Widget::test_id(&s), Some("slider"));
        }

        // Case `slider_test_id_can_be_overridden`.
        {
            let s = Slider::new("any", "value").with_id("volume");
            assert_eq!(Widget::test_id(&s), Some("volume"));
        }

        // Case `local_state_binding_is_two_way_across_sliders`.
        {
            use crate::a11y::Action;

            let state = crate::LocalState::new(0.25);
            let changes = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let captured = std::rc::Rc::clone(&changes);
            let mut first = Slider::from_state(&state)
                .on_change(move |value| captured.borrow_mut().push(value));
            let mut second = Slider::from_state(&state);
            let mut document = AutoCommit::new();

            state.set(0.5);
            Widget::sync(&mut first, &document, &automerge::ROOT);
            Widget::sync(&mut second, &document, &automerge::ROOT);
            assert_eq!(first.value(), 0.5);
            assert_eq!(second.value(), 0.5);
            assert!(changes.borrow().is_empty(), "model sync is not user intent");

            assert!(Widget::perform_action(
                &mut first,
                &Action::Increment,
                &mut crate::widget::Edits::lend(&mut document),
                &automerge::ROOT,
            ));
            assert_eq!(state.get(), 0.55);
            assert_eq!(&*changes.borrow(), &[0.55]);
            Widget::sync(&mut second, &document, &automerge::ROOT);
            assert_eq!(second.value(), 0.55);

            second.set_value(0.8);
            assert_eq!(state.get(), 0.8);
            Widget::sync(&mut first, &document, &automerge::ROOT);
            assert_eq!(first.value(), 0.8);
            assert_eq!(&*changes.borrow(), &[0.55]);
        }

        // Case `with_enabled_false_is_reported_by_is_enabled`.
        {
            let s = Slider::new("any", "value").with_enabled(false);
            assert!(!Widget::is_enabled(&s));
            assert!(
                Widget::is_enabled(&Slider::new("any", "value")),
                "default is enabled"
            );
        }
    }

    #[test]
    fn slider_accessibility_reports_and_routes_value_actions() {
        // Case `accessibility_reports_slider_role_and_percent_value`.
        {
            use crate::a11y::Role;
            // Default value is 0.5 → "50%".
            let s = Slider::new("any", "value");
            let node = Widget::accessibility(&s).unwrap();
            assert_eq!(node.role, Role::Slider);
            assert_eq!(node.value.as_deref(), Some("50%"));
            assert!(node.state.focusable);
            assert!(!node.state.disabled);
            assert_eq!(node.state.checked, None, "a slider is not checkable");
        }

        // Case `accessibility_value_tracks_position_and_disabled`.
        {
            let mut s = Slider::new("any", "value").with_enabled(false);
            s.value = 0.25;
            let node = Widget::accessibility(&s).unwrap();
            assert_eq!(node.value.as_deref(), Some("25%"));
            assert!(node.state.disabled);
        }

        // Case `accessibility_uses_accessible_name_when_set`.
        {
            // No name by default; `with_accessible_name` names what the slider
            // adjusts so the reader pairs it with the value.
            let plain = Slider::new("any", "value");
            assert_eq!(Widget::accessibility(&plain).unwrap().name, None);

            let named = Slider::new("any", "value").with_accessible_name("Volume");
            let node = Widget::accessibility(&named).unwrap();
            assert_eq!(node.name.as_deref(), Some("Volume"));
            assert_eq!(node.value.as_deref(), Some("50%"));
        }

        // Case `perform_action_increments_decrements_and_sets_value`.
        {
            use crate::a11y::Action;
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let mut s = Slider::new("any", "value"); // default 0.5

            assert!(Widget::perform_action(
                &mut s,
                &Action::Increment,
                &mut crate::widget::Edits::lend(&mut doc),
                &root
            ));
            assert!((s.value - 0.55).abs() < 1e-9, "increment steps +5%");
            assert!(Widget::perform_action(
                &mut s,
                &Action::Decrement,
                &mut crate::widget::Edits::lend(&mut doc),
                &root
            ));
            assert!((s.value - 0.50).abs() < 1e-9, "decrement steps -5%");

            // SetValue accepts a 0..100 percentage.
            assert!(Widget::perform_action(
                &mut s,
                &Action::SetValue("80%".into()),
                &mut crate::widget::Edits::lend(&mut doc),
                &root
            ));
            assert!((s.value - 0.80).abs() < 1e-9);

            // SetValue also accepts a 0..1 fraction.
            assert!(Widget::perform_action(
                &mut s,
                &Action::SetValue("0.2".into()),
                &mut crate::widget::Edits::lend(&mut doc),
                &root
            ));
            assert!((s.value - 0.20).abs() < 1e-9);
        }

        // Case `disabled_slider_rejects_perform_action`.
        {
            use crate::a11y::Action;
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let mut s = Slider::new("any", "value").with_enabled(false);
            let before = s.value;
            assert!(!Widget::perform_action(
                &mut s,
                &Action::Increment,
                &mut crate::widget::Edits::lend(&mut doc),
                &root
            ));
            assert_eq!(s.value, before, "a disabled slider must not change value");
        }

        // Case `a_settling_slider_invokes_its_action_once_per_drag`.
        {
            let mut slider = slider_at(0.0, 0.0, 200.0, 30.0)
                .on_action(crate::widget::test_number_action_binding())
                .commits_on_release();
            crate::widget::controls::refresh_control_action_availability(
                &mut slider,
                &std::collections::HashSet::from([tilo_actions::ActionAddress::new(
                    "test:control",
                    "set",
                )]),
            );
            dispatch(&mut slider, &cursor_moved(20.0, 15.0));
            dispatch(&mut slider, &mouse_press());
            dispatch(&mut slider, &cursor_moved(100.0, 15.0));
            dispatch(&mut slider, &cursor_moved(150.0, 15.0));
            assert!(
                crate::widget::controls::take_control_invocation(&mut slider).is_none(),
                "nothing is invoked while the drag goes on"
            );
            dispatch(&mut slider, &mouse_release());
            let invoked = crate::widget::controls::take_control_invocation(&mut slider)
                .expect("letting go invokes the action");
            let value = invoked.arguments["value"].as_f64().unwrap();
            assert!((value - slider.value).abs() < 1e-9);
            assert!(crate::widget::controls::take_control_invocation(&mut slider).is_none());
        }

        // Case `shift_drags_the_value_a_tenth_as_fast`.
        {
            let mut slider = slider_at(0.0, 0.0, 210.0, 30.0);
            let mut shift = Dispatch::new().modifiers(winit::keyboard::ModifiersState::SHIFT);
            dispatch(&mut slider, &cursor_moved(105.0, 15.0));
            dispatch(&mut slider, &mouse_press());
            let start = slider.value;
            shift.send(&mut slider, &cursor_moved(105.0, 15.0));
            let half_track = SliderFace::new(slider.rect, 0.0).track_rect().width() / 2.0;
            shift.send(&mut slider, &cursor_moved(105.0 + half_track, 15.0));
            assert!(
                (slider.value - start - 0.05).abs() < 1e-6,
                "{start} -> {}",
                slider.value
            );
            dispatch(&mut slider, &cursor_moved(105.0 + half_track, 15.0));
            assert!(
                (slider.value - 1.0).abs() < 1e-6,
                "without Shift it follows again"
            );
        }

        // Case `a_settling_slider_shows_its_drag_in_its_local_state`.
        {
            let state = crate::LocalState::new(0.0);
            let mut slider = Slider::from_state(&state)
                .on_action(crate::widget::test_number_action_binding())
                .commits_on_release();
            slider.rect = Rect::new(0.0, 0.0, 200.0, 30.0);
            crate::widget::controls::refresh_control_action_availability(
                &mut slider,
                &std::collections::HashSet::from([tilo_actions::ActionAddress::new(
                    "test:control",
                    "set",
                )]),
            );
            dispatch(&mut slider, &cursor_moved(20.0, 15.0));
            dispatch(&mut slider, &mouse_press());
            dispatch(&mut slider, &cursor_moved(150.0, 15.0));
            assert!(
                (state.with(|value| *value) - slider.value).abs() < 1e-9,
                "the local state follows the drag"
            );
            assert!(crate::widget::controls::take_control_invocation(&mut slider).is_none());
            dispatch(&mut slider, &mouse_release());
            assert!(crate::widget::controls::take_control_invocation(&mut slider).is_some());
        }

        // Case `action_binding_owns_accessibility_value_changes`.
        {
            let mut slider = Slider::new("native", "legacy")
                .on_change(|_| panic!("legacy callback must be suppressed"))
                .on_action(crate::widget::test_number_action_binding());
            let mut doc = AutoCommit::new();
            crate::widget::controls::refresh_control_action_availability(
                &mut slider,
                &std::collections::HashSet::new(),
            );
            assert!(!Widget::is_enabled(&slider));
            crate::widget::controls::refresh_control_action_availability(
                &mut slider,
                &std::collections::HashSet::from([tilo_actions::ActionAddress::new(
                    "test:control",
                    "set",
                )]),
            );
            assert!(Widget::is_enabled(&slider));
            assert!(Widget::perform_action(
                &mut slider,
                &crate::a11y::Action::SetValue("80%".into()),
                &mut crate::widget::Edits::lend(&mut doc),
                &automerge::ROOT
            ));
            assert!(doc.get(&automerge::ROOT, "legacy").unwrap().is_none());
            assert_eq!(
                crate::widget::controls::take_control_invocation(&mut slider)
                    .unwrap()
                    .arguments["value"],
                0.8
            );
        }
    }
    #[test]
    fn ranged_stepped_keyboard_slider_with_a_flush_track() {
        let mut slider = Slider::new("delay", "")
            .range(0.0, 10.0)
            .step(0.5)
            .value_text(|seconds| format!("{seconds} s"));
        slider.set_value(3.3);
        assert!((slider.value() - 3.5).abs() < 1e-9, "snaps to the step");
        assert_eq!(
            Widget::accessibility(&slider)
                .and_then(|n| n.value)
                .as_deref(),
            Some("3.5 s")
        );
        slider.set_focus(true);
        let doc = AutoCommit::new();
        let key = |slider: &mut Slider, named: winit::keyboard::NamedKey| {
            let ev =
                crate::synthetic_input::key_event(Key::Named(named), None, ElementState::Pressed);
            slider.handle_key(&ev, &doc, &automerge::ROOT)
        };
        assert!(key(&mut slider, NamedKey::ArrowRight));
        assert!((slider.value() - 4.0).abs() < 1e-9);
        assert!(key(&mut slider, NamedKey::PageUp));
        assert!((slider.value() - 5.0).abs() < 1e-9);
        assert!(key(&mut slider, NamedKey::End));
        assert!((slider.value() - 10.0).abs() < 1e-9);
        assert!(key(&mut slider, NamedKey::Home));
        assert!(slider.value().abs() < 1e-9);

        let theme = Theme::dark();
        let face = SliderFace::new(Rect::new(20.0, 0.0, 220.0, 24.0), 0.0);
        assert_eq!(face.track_rect().x0, 20.0, "the track runs flush");
        assert!((face.value_at(Point::new(120.0, 12.0)) - 0.5).abs() < 1e-9);
        assert_eq!(face.value_at(Point::new(0.0, 12.0)), 0.0);
        let hit = face.hit_rect(&theme);
        assert!(
            hit.x0 < 20.0 && hit.height() >= 32.0,
            "the knob overhangs: {hit:?}"
        );
    }
}
