use crate::permissions::PermissionScope;
use crate::presence::{PathSegment, PresenceKey};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::binding::{BoxedBinding, Pulled};
use crate::widget::{ActionEmitter, ControlBindingKind, ControlBindingMetadata};
use crate::widget::{AppModel, ModelPath};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect};
use winit::event::{ElementState, MouseButton, WindowEvent};

/// Horizontal padding between `rect` and the track on each side.
/// Both `draw` and `handle_event` use this to translate between
/// cursor x and a 0..1 value, so it has to live in one place.
const TRACK_PADDING: f64 = 5.0;
/// How many times slower the value follows the pointer while Shift is held
/// during a drag.
const FINE_DRAG: f64 = 10.0;
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
    value: f64, // 0.0 to 1.0
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
}

impl Slider {
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
            binding: Some(crate::doc_binding::number_key(on_change_path)),
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
        slider.value = value.clamp(0.0, 1.0);
        slider.binding = Some(binding);
        slider.publish_local_state();
        slider
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

    /// Return the normalized value currently presented by this slider.
    pub fn value(&self) -> f64 {
        self.value
    }

    /// Set the normalized value programmatically and update a bound local
    /// model without invoking [`Self::on_change`].
    pub fn set_value(&mut self, value: f64) {
        self.value = value.clamp(0.0, 1.0);
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
        let track_width = self.rect.width() - TRACK_PADDING * 2.0;
        ((cursor_x - (self.rect.x0 + TRACK_PADDING)) / track_width).clamp(0.0, 1.0)
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
    fn commit_value(&mut self, new_value: f64, state: &mut AppModel, root: &ModelPath) -> bool {
        if (new_value - self.value).abs() <= 0.001 {
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
                binding.push(state, root, &self.value);
            }
        } else if let Some(binding) = &mut self.binding {
            binding.push(state, root, &self.value);
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
        let unfilled = theme.tokens.colors.lift(1.0);
        let filled = theme.accent_primary;
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
        // One thumb colour that clears both halves of the track (HIG §9.5),
        // held across rest / hover / drag: the `*_white_*` veneers were a
        // grey blob on dark and vanished into the unfilled track on light.
        // Hover and drag read from the radius (above), a firmer edge, and
        // the halo — not from a second thumb colour.
        let fill_color = crate::widget::toggle::ink_across(
            &[filled, unfilled],
            theme.tokens.colors.text_primary,
            crate::tokens::colors::NON_TEXT_MIN_CONTRAST,
        );
        let border_color = if face.dragging || face.hovered {
            crate::surface::edge_on_with(fill_color, crate::chrome::PANEL_BORDER_ALPHA)
        } else {
            crate::surface::edge_on(fill_color)
        };
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

        // Tight focus ring around the knob, not the whole widget rect.
        if face.focused {
            scene.stroke(
                &vello::kurbo::Stroke::new(1.5),
                transform,
                theme.border_focus,
                None,
                &Circle::new(knob_center, knob_radius + 2.0),
            );
        }
        if face.disabled {
            scene.pop_layer();
        }
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

    fn sync(&mut self, state: &mut AppModel, root: &ModelPath) {
        if let Some(binding) = &mut self.binding
            && let Pulled::Value(value) = binding.pull(state, root)
        {
            self.value = value.clamp(0.0, 1.0);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(self.interaction.minimum_target()),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &crate::presence::PresenceCtx,
    ) {
        Slider::paint(
            scene,
            transform,
            theme,
            &SliderFace::new(self.rect, self.value)
                .hovered(self.is_hovered)
                .dragging(self.is_dragging)
                .focused(self.is_focused)
                .disabled(!self.effectively_enabled()),
        );
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
                let hover = self.rect.contains(p);
                if hover != self.is_hovered {
                    self.is_hovered = hover;
                    *ctx.handled = true;
                }

                if self.is_dragging {
                    let new_value = if ctx.modifiers.state().shift_key() {
                        let (x, value) = *self.fine_from.get_or_insert((p.x, self.value));
                        let track = (self.rect.width() - TRACK_PADDING * 2.0).max(1.0);
                        (value + (p.x - x) / track / FINE_DRAG).clamp(0.0, 1.0)
                    } else {
                        self.fine_from = None;
                        self.value_from_cursor_x(p.x)
                    };
                    if self.commit_value(new_value, ctx.state, ctx.path) {
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
                if *element_state == ElementState::Pressed && self.rect.contains(self.last_cursor) {
                    self.is_dragging = true;
                    self.drag_from = Some(self.value);
                    *ctx.focus_target = Some(ctx.node_id);
                    *ctx.handled = true;
                    let new_value = self.value_from_cursor_x(self.last_cursor.x);
                    self.commit_value(new_value, ctx.state, ctx.path);
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

    #[allow(
        clippy::cast_possible_truncation,
        reason = "the rounded percentage is bounded to the inclusive range 0..=100"
    )]
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        // The slider models a normalized 0..1 value; expose it as a percentage
        // so a screen reader announces something meaningful ("50 percent").
        let pct = (self.value.clamp(0.0, 1.0) * 100.0).round() as i64;
        let name = self
            .accessible_name
            .as_ref()
            .map(|n| crate::i18n!(n))
            .filter(|n| !n.is_empty());
        let mut node = AccessNode::new(Role::Slider)
            .with_semantic_key(&self.semantic_id)
            .with_optional_name(name)
            .with_value(format!("{pct}%"))
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
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        use crate::a11y::Action;
        const STEP: f64 = 0.05;
        if !self.effectively_enabled() {
            return false;
        }
        match action {
            Action::Increment => {
                self.commit_value((self.value + STEP).clamp(0.0, 1.0), state, path)
            }
            Action::Decrement => {
                self.commit_value((self.value - STEP).clamp(0.0, 1.0), state, path)
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
                // Accept either a 0..1 fraction or a 0..100 percentage.
                let frac = if parsed > 1.0 { parsed / 100.0 } else { parsed };
                self.commit_value(frac.clamp(0.0, 1.0), state, path)
            }
            _ => false,
        }
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
        permissions: &crate::permissions::PermissionContext,
    ) {
        if presence.should_cull(self.get_rect()) {
            return;
        }
        self.draw(scene, transform, text_renderer, theme, presence);

        if let Some(scope) = crate::permissions::capability::permission_scope(self)
            && !permissions.check(&scope)
        {
            let overlay = crate::tokens::colors::PERMISSION_DENIED_SCRIM;
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                overlay,
                None,
                &self.rect,
            );
        }

        if let Some(key) = crate::presence::capability::presence_key(self)
            && let Some(peers) = presence.here.get(&key)
            && let Some(user) = peers.first()
        {
            presence.draw_peer_focus(scene, theme, user, self.rect);
        }
    }
}

impl crate::presence::capability::PublishesPresence for Slider {
    fn presence_key(&self) -> Option<PresenceKey> {
        if self.presence_path.is_empty() {
            None
        } else {
            Some(PresenceKey::Widget {
                path: self.presence_path.clone(),
            })
        }
    }
}

impl crate::permissions::capability::PermissionGated for Slider {
    fn permission_scope(&self) -> Option<PermissionScope> {
        self.permission_scope.clone()
    }
}

impl crate::widget::controls::ActionControls for Slider {
    fn install_control_custody(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) {
        if let Some(action) = &mut self.action {
            let _ = action.install_custody(domain, attachment);
        }
    }
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.action.as_mut().and_then(ActionEmitter::take)
    }
    fn control_action_bindings(&self) -> Vec<ControlBindingMetadata> {
        let control_id = self.test_id().unwrap_or("slider");
        if let Some(action) = &self.action {
            vec![action.metadata(control_id)]
        } else if let Some(key) = self.binding.as_ref().and_then(|b| b.document_key()) {
            vec![ControlBindingMetadata {
                control_id: control_id.to_string(),
                kind: ControlBindingKind::CollaborativeDocument(key.to_owned()),
            }]
        } else if self.on_change.is_some() {
            vec![ControlBindingMetadata {
                control_id: control_id.to_string(),
                kind: ControlBindingKind::LegacyCallback,
            }]
        } else {
            vec![ControlBindingMetadata {
                control_id: control_id.to_string(),
                kind: ControlBindingKind::SemanticAction(control_id.to_string()),
            }]
        }
    }
    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        if let Some(action) = &mut self.action {
            action.refresh_availability(allowed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::{AutoCommit, ReadDoc};

    /// Build a slider with a known rect so we don't need a full taffy pass.
    /// Rect spans [0..200, 0..30] — track-padding 5 px, so the track itself
    /// is [5..195] (width 190).
    fn slider_at(x0: f64, y0: f64, x1: f64, y1: f64) -> Slider {
        let mut s = Slider::new("test", "value");
        s.rect = Rect::new(x0, y0, x1, y1);
        s
    }

    fn cursor_moved(x: f64, y: f64) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(x, y),
        }
    }

    fn mouse_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }

    fn mouse_release() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button: MouseButton::Left,
        }
    }

    /// Drive an event through the slider with default scaffolding.
    fn dispatch(s: &mut Slider, event: &WindowEvent) {
        dispatch_with(s, event, winit::event::Modifiers::default());
    }

    fn dispatch_with(s: &mut Slider, event: &WindowEvent, modifiers: winit::event::Modifiers) {
        let mut tree = TaffyTree::<()>::new();
        let node = tree.leaf_node(Style::default());
        let mut handled = false;
        let mut focus_target = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        s.handle_event(
            event,
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &modifiers,
                state: &mut doc,
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &tree,
            },
        );
    }

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
            // Rect [0..200, 0..30]. Track usable width 190 (padding 5 each side).
            let mut s = slider_at(0.0, 0.0, 200.0, 30.0);

            // Move cursor inside, near the left edge of the track.
            dispatch(&mut s, &cursor_moved(20.0, 15.0));
            // Press inside the rect — drag begins.
            dispatch(&mut s, &mouse_press());
            assert!(s.is_dragging, "press inside rect must start drag");

            // Cursor leaves the rect vertically (still within the track's
            // horizontal range). The drag must continue updating `value`.
            dispatch(&mut s, &cursor_moved(150.0, 200.0));
            let expected = ((150.0 - 5.0) / 190.0_f64).clamp(0.0, 1.0);
            assert!(
                (s.value - expected).abs() < 1e-6,
                "drag outside rect must keep tracking cursor x — got {}, expected {}",
                s.value,
                expected
            );

            // And another move further outside — value still tracks.
            dispatch(&mut s, &cursor_moved(95.0, -50.0));
            let expected2 = ((95.0 - 5.0) / 190.0_f64).clamp(0.0, 1.0);
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
            // Rect [0..200, 0..30], track [5..195] (width 190).
            // x at 0.8 of width → 5 + 0.8*190 = 157.0
            let mut s = slider_at(0.0, 0.0, 200.0, 30.0);

            // Move cursor to the press location *and* press without any
            // prior hover state being toggled by the same code path —
            // this is the "click on track to jump" affordance.
            let press_x = 5.0 + 0.8 * 190.0;
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
            Widget::sync(&mut first, &mut document, &automerge::ROOT);
            Widget::sync(&mut second, &mut document, &automerge::ROOT);
            assert_eq!(first.value(), 0.5);
            assert_eq!(second.value(), 0.5);
            assert!(changes.borrow().is_empty(), "model sync is not user intent");

            assert!(Widget::perform_action(
                &mut first,
                &Action::Increment,
                &mut document,
                &automerge::ROOT,
            ));
            assert_eq!(state.get(), 0.55);
            assert_eq!(&*changes.borrow(), &[0.55]);
            Widget::sync(&mut second, &mut document, &automerge::ROOT);
            assert_eq!(second.value(), 0.55);

            second.set_value(0.8);
            assert_eq!(state.get(), 0.8);
            Widget::sync(&mut first, &mut document, &automerge::ROOT);
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
                &mut doc,
                &root
            ));
            assert!((s.value - 0.55).abs() < 1e-9, "increment steps +5%");
            assert!(Widget::perform_action(
                &mut s,
                &Action::Decrement,
                &mut doc,
                &root
            ));
            assert!((s.value - 0.50).abs() < 1e-9, "decrement steps -5%");

            // SetValue accepts a 0..100 percentage.
            assert!(Widget::perform_action(
                &mut s,
                &Action::SetValue("80%".into()),
                &mut doc,
                &root
            ));
            assert!((s.value - 0.80).abs() < 1e-9);

            // SetValue also accepts a 0..1 fraction.
            assert!(Widget::perform_action(
                &mut s,
                &Action::SetValue("0.2".into()),
                &mut doc,
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
                &mut doc,
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
            let shift = winit::event::Modifiers::from(winit::keyboard::ModifiersState::SHIFT);
            dispatch(&mut slider, &cursor_moved(105.0, 15.0));
            dispatch(&mut slider, &mouse_press());
            let start = slider.value;
            dispatch_with(&mut slider, &cursor_moved(105.0, 15.0), shift);
            dispatch_with(&mut slider, &cursor_moved(205.0, 15.0), shift);
            assert!(
                (slider.value - start - 0.05).abs() < 1e-6,
                "{start} -> {}",
                slider.value
            );
            dispatch(&mut slider, &cursor_moved(205.0, 15.0));
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
                &mut doc,
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
}
