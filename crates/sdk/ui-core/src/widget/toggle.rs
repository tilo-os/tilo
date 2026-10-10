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
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect, Stroke};
use winit::event::{ElementState, MouseButton, WindowEvent};

pub(super) type BoolChangeCallback = Box<dyn FnMut(bool)>;

/// Flip a two-state control the way a click does: through its action when it
/// has one, else through its binding. `on_change` hears the new value only
/// when no action owns it.
pub(super) fn commit_flip(
    checked: &mut bool,
    action: &mut Option<ActionEmitter<bool>>,
    binding: &mut Option<BoxedBinding<bool>>,
    on_change: &mut Option<BoolChangeCallback>,
) {
    let flipped = !*checked;
    if let Some(action) = action {
        let _ = action.emit(flipped);
    } else if let Some(binding) = binding {
        binding.publish(&flipped);
    }
    *checked = flipped;
    if action.is_none()
        && let Some(callback) = on_change.as_mut()
    {
        callback(flipped);
    }
}

/// `is_hovered`, `is_checked`, `is_focused` and `enabled` are four
/// independent state axes (pointer, CRDT-synced value, keyboard focus,
/// build-time enablement), each set/read by name — not positional bool
/// arguments — so bundling them into a bitflags type would touch every
/// read/write site here for no behavioural change.
#[allow(
    clippy::struct_excessive_bools,
    reason = "four independent state axes, all set/read by name, not positional bool args"
)]
pub struct Toggle {
    semantic_id: String,
    rect: Rect,
    is_hovered: bool,
    is_checked: bool,
    is_focused: bool,
    presence_path: Vec<PathSegment>,
    permission_scope: Option<PermissionScope>,
    enabled: bool,
    id_override: Option<String>,
    /// Spoken name for a label-less toggle (e.g. an icon-only switch);
    /// unset falls back to a sibling label from the tree builder.
    accessible_name: Option<String>,
    /// Where the checked value lives: a document key or process-local state.
    binding: Option<BoxedBinding<bool>>,
    /// Observes user intent without owning model synchronization.
    on_change: Option<BoolChangeCallback>,
    action: Option<ActionEmitter<bool>>,
    interaction: crate::interaction::InteractionEnvironment,
}

impl Toggle {
    fn effectively_enabled(&self) -> bool {
        self.enabled && self.action.as_ref().is_none_or(ActionEmitter::is_available)
    }
    pub fn new(id: &str, on_toggle_path: &str) -> Self {
        Self {
            semantic_id: id.to_string(),
            rect: Rect::ZERO,
            is_hovered: false,
            is_checked: false,
            is_focused: false,
            presence_path: Vec::new(),
            permission_scope: None,
            enabled: true,
            id_override: None,
            accessible_name: Some(crate::widget::humanize_control_id(id)),
            binding: Some(crate::doc_source::bool_key(on_toggle_path)),
            on_change: None,
            action: None,
            interaction: crate::interaction::InteractionEnvironment::default(),
        }
    }

    /// Create a true two-way binding to typed process-local boolean state.
    ///
    /// Pointer, keyboard, and accessibility activation update `state`;
    /// mutations through any clone of `state` update this switch on the next
    /// synchronization pass. Use [`Self::on_change`] only to observe user
    /// changes, not to keep the model in sync.
    pub fn from_state(state: &crate::LocalState<bool>) -> Self {
        let (checked, binding) = super::binding::local(state);
        Self {
            is_checked: checked,
            binding: Some(binding),
            ..Self::new("toggle", "")
        }
    }

    /// Observe changes produced by user interaction.
    ///
    /// Model synchronization and [`Self::set_checked`] do not invoke this
    /// callback, so validation or action dispatch cannot mistake a model
    /// refresh for new user intent.
    #[must_use]
    pub fn on_change(mut self, callback: impl FnMut(bool) + 'static) -> Self {
        if self.action.is_none() {
            self.on_change = Some(Box::new(callback));
        }
        self
    }

    /// Route pointer, keyboard, and accessibility toggles through an action.
    #[must_use]
    pub fn on_action(mut self, binding: tilo_actions::ActionBinding<bool>) -> Self {
        self.action = Some(ActionEmitter::new(binding));
        self.on_change = None;
        self
    }

    /// Return the value currently presented by this switch.
    pub fn is_checked(&self) -> bool {
        self.is_checked
    }

    /// Replace the presented value and publish it to a bound local model.
    ///
    /// This is a programmatic model mutation and does not invoke
    /// [`Self::on_change`]. Document-bound controls should mutate their
    /// Automerge path instead and let [`Widget::sync`] adopt the result.
    pub fn set_checked(&mut self, checked: bool) {
        self.is_checked = checked;
        if self.action.is_none() {
            self.publish_local_state();
        }
    }

    fn publish_local_state(&mut self) {
        if let Some(binding) = &mut self.binding {
            binding.publish(&self.is_checked);
        }
    }

    fn commit_user_toggle(&mut self, _state: &AppModel, _root: &ModelPath) {
        commit_flip(
            &mut self.is_checked,
            &mut self.action,
            &mut self.binding,
            &mut self.on_change,
        );
    }

    #[must_use]
    pub fn with_presence(mut self, path: Vec<PathSegment>) -> Self {
        self.presence_path = path;
        self
    }

    #[must_use]
    pub fn with_permission_scope(mut self, scope: PermissionScope) -> Self {
        self.permission_scope = Some(scope);
        self
    }

    /// Default `test_id()` is `"toggle"`; override to disambiguate multiple
    /// toggles in one surface.
    #[must_use]
    pub fn with_id(mut self, id: &str) -> Self {
        self.id_override = Some(id.to_string());
        self
    }

    /// Set the enabled state at build time. A disabled toggle is skipped
    /// by focus, rejects input and renders dimmed. Default `true`.
    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    #[must_use]
    pub fn with_accessible_name(mut self, name: &str) -> Self {
        self.accessible_name = Some(name.to_string());
        self
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }
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
    /// The knob.
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
}

/// Single source of truth for both the retained [`Toggle`]'s draw and
/// [`Toggle::paint`], so every switch shares one palette per state.
///
/// On, the switch is an inverted control: the track is `intent.inverted` and
/// the knob its label colour (`intent.on_inverted`). Off, the track is a lift
/// of the ground (HIG §9.2, §9.5 "Rest edges"). On a light material the off
/// knob is a raised white object with a derived ring, never a dark disc; on a
/// dark one it is the secondary ink, and hover lifts it to the primary ink.
/// Focus is your colour round the track.
pub fn toggle_face_colors(theme: &Theme, on: bool, interact: ToggleInteract) -> ToggleColors {
    let colors = &theme.tokens.colors;
    let floor = crate::tokens::colors::NON_TEXT_MIN_CONTRAST;
    // A lift of the window ground: lighter on a dark material, darker on a
    // light one, so a white knob always stands out from it.
    let off_track = colors.intent.selection;
    let track = if on {
        colors.intent.inverted
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
        let knob = ink_across(&[track], colors.intent.on_inverted, floor);
        (if interact.hovered { harder(knob) } else { knob }, None)
    } else if light_material(colors) {
        let ring = ink_across(&[track], colors.text_tertiary, floor);
        (
            colors.surfaces.raised,
            Some(if interact.hovered { harder(ring) } else { ring }),
        )
    } else if interact.hovered {
        (ink_across(&[track], colors.text_primary, floor), None)
    } else {
        (ink_across(&[track], colors.text_dim, floor), None)
    };
    let (edge, edge_width) = if interact.focused {
        (colors.intent.you, 2.0)
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
    on: bool,
    interact: ToggleInteract,
}

impl ToggleFace {
    pub fn new(rect: Rect, on: bool) -> Self {
        Self {
            rect,
            on,
            interact: ToggleInteract::default(),
        }
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
        toggle_face_colors(theme, self.on, self.interact)
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
        .with_state(AccessState {
            focusable: enabled,
            focused,
            disabled: !enabled,
            checked: Some(on),
            ..Default::default()
        })
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
        let cx = if face.on {
            face.rect.x1 - kr - KNOB_EDGE_GAP
        } else {
            face.rect.x0 + kr + KNOB_EDGE_GAP
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

    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        if let Some(binding) = &mut self.binding {
            match binding.pull(state, root) {
                Pulled::Value(checked) => self.is_checked = checked,
                Pulled::Missing => self.is_checked = false,
                Pulled::Unchanged => {}
            }
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let (track_w, track_h) = ToggleFace::size(theme);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a switch is a few dozen logical pixels, far inside f32"
        )]
        let (track_w, track_h) = (track_w as f32, track_h as f32);
        let target = self.interaction.minimum_target();
        let style = Style {
            size: Size {
                width: Dimension::length(track_w.max(target)),
                height: Dimension::length(track_h.max(target)),
            },
            // A switch is a fixed target, like a button: in a row that also
            // holds a percent-width field it must not be the item that gives.
            // With the default shrink factor and no measured minimum, taffy
            // squeezed it to zero width beside a stretching text field.
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
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
        _presence: &crate::presence::PresenceCtx,
    ) {
        // One painter for the retained widget and immediate-mode hosts, so
        // the two cannot drift: the pill is centred in the layout target and
        // every state resolves through the same `ToggleFace`.
        Self::paint(
            scene,
            transform,
            theme,
            &ToggleFace::new(track_rect(self.rect, theme), self.is_checked)
                .hovered(self.is_hovered)
                .focused(self.is_focused)
                .disabled(!self.effectively_enabled()),
        );
    }

    /// A fixed size: only the first layout (and a theme change, which
    /// rebuilds the tree) needs one.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if !self.effectively_enabled() {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let logical_pos =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                // A hover change is a repaint, not consumed input: a parent
                // that stops at the first handled child must still deliver
                // this move to the control the pointer went to, or a switch
                // beside this one never learns it is under the pointer.
                self.is_hovered = self.rect.contains(logical_pos);
            }
            WindowEvent::MouseInput {
                state: element_state,
                button: MouseButton::Left,
                ..
            } if self.is_hovered && *element_state == ElementState::Pressed => {
                *ctx.handled = true;
                *ctx.focus_target = Some(ctx.node_id);
                self.commit_user_toggle(ctx.state.doc(), ctx.path);
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "Toggle"
    }

    fn test_id(&self) -> Option<&str> {
        Some(self.id_override.as_deref().unwrap_or("toggle"))
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

    /// AT/keyboard activation (Enter/Space route to `Activate`/`ToggleCheck`
    /// upstream) — same mutation as the mouse path.
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
        match action {
            Action::ToggleCheck | Action::Activate => {
                self.commit_user_toggle(state.doc(), path);
                true
            }
            _ => false,
        }
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        let mut node = toggle_accessibility(
            self.is_checked,
            self.is_focused(),
            self.is_enabled(),
            self.accessible_name.as_deref(),
        )
        .with_semantic_key(&self.semantic_id);
        if self.action.is_none() && self.on_change.is_none() {
            node = node.with_action_target(self.test_id().unwrap_or("toggle"));
        }
        Some(node)
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
        crate::widget::draw_gated_leaf(
            self,
            scene,
            transform,
            text_renderer,
            theme,
            presence,
            permissions,
        );
    }
}

impl crate::presence::capability::PublishesPresence for Toggle {
    fn presence_key(&self) -> Option<PresenceKey> {
        PresenceKey::widget(&self.presence_path)
    }
}

impl crate::permissions::capability::PermissionGated for Toggle {
    fn permission_scope(&self) -> Option<PermissionScope> {
        self.permission_scope.clone()
    }
}

impl crate::widget::controls::ActionControls for Toggle {
    crate::widget::controls::optional_emitter_controls!();
    fn control_action_bindings(&self) -> Vec<ControlBindingMetadata> {
        crate::widget::controls::value_control_bindings(
            self.test_id().unwrap_or("toggle"),
            self.action.as_ref(),
            self.binding.as_ref().and_then(|b| b.document_key()),
            self.on_change.is_some(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Dispatch;
    use crate::widget::Widget;
    use automerge::{AutoCommit, ReadDoc, transaction::Transactable};

    #[test]
    fn toggle_identity_enabled_state_and_accessibility_are_reported() {
        // Case `toggle_test_id_default_is_toggle`.
        {
            let t = Toggle::new("any", "path");
            assert_eq!(Widget::test_id(&t), Some("toggle"));
        }

        // Case `toggle_test_id_can_be_overridden`.
        {
            let t = Toggle::new("any", "path").with_id("airplane_mode");
            assert_eq!(Widget::test_id(&t), Some("airplane_mode"));
        }

        // Case `with_enabled_false_is_reported_by_is_enabled`.
        {
            let t = Toggle::new("any", "path").with_enabled(false);
            assert!(!Widget::is_enabled(&t));
            assert!(
                Widget::is_enabled(&Toggle::new("any", "path")),
                "default is enabled"
            );
        }

        // Case `accessibility_reports_switch_role_and_checked_state`.
        {
            use crate::a11y::Role;
            let mut t = Toggle::new("any", "path");
            let off = Widget::accessibility(&t).unwrap();
            assert_eq!(off.role, Role::Switch);
            assert_eq!(off.state.checked, Some(false));

            t.is_checked = true; // same module → set the synced on-state directly
            let on = Widget::accessibility(&t).unwrap();
            assert_eq!(on.state.checked, Some(true));
        }

        // Case `accessibility_uses_accessible_name_when_set`.
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
                toggle.is_checked = on;
                toggle.is_hovered = hovered;
                toggle.is_focused = focused;
                toggle.enabled = enabled;
                toggle.rect = target;
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

        // Case `toggle_face_colors_track_tracks_on_state`.
        {
            let theme = Theme::dark();
            let off = toggle_face_colors(&theme, false, ToggleInteract::default());
            let on = toggle_face_colors(&theme, true, ToggleInteract::default());
            assert_eq!(
                on.track, theme.tokens.colors.intent.inverted,
                "on-track is the inverted fill"
            );
            assert_eq!(
                off.track, theme.tokens.colors.intent.selection,
                "off-track is a lift off the ground, not a ramp slot"
            );
            assert_ne!(on.track, off.track, "on and off tracks must differ");
        }

        // Case `toggle_knob_is_inverted_on_and_a_raised_object_off_in_light`.
        // On, the switch is an inverted control and its knob is the inverted
        // label colour. Off, a light material shows a raised white knob with
        // a derived ring (never a dark disc); a dark one the secondary ink.
        // Legible on every material either way.
        {
            for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
                let colors = &theme.tokens.colors;
                let off = toggle_face_colors(&theme, false, ToggleInteract::default());
                let on = toggle_face_colors(&theme, true, ToggleInteract::default());
                assert_eq!(on.knob, colors.intent.on_inverted, "{}", theme.name);
                assert_eq!(on.knob_edge, None);
                if light_material(colors) {
                    assert_eq!(off.knob, colors.surfaces.raised, "{}", theme.name);
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

        // Case `toggle_face_colors_focus_swaps_border_and_weight`.
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
                focused.edge, theme.tokens.colors.intent.you,
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

        // Case `toggle_face_matches_shared_colors`.
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
        // Case `local_state_binding_is_two_way_across_toggles`.
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

        // Case `document_binding_resets_after_the_boolean_path_disappears`.
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

        // Case `disabled_toggle_does_not_toggle`.
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

        // Case `action_binding_owns_keyboard_and_accessibility_activation`.
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
        assert!(!t.is_hovered);
    }
}
