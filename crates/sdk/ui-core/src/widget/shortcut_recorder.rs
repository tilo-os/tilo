//! A compact, visual keyboard-shortcut editor.
//!
//! The displayed keycaps are the control: click them, or focus them and press
//! Enter/Space, then type the replacement chord. Escape cancels. A reset
//! affordance is present only while the value differs from its default.
//!
//! The chord shown is a [`crate::LocalState`]; a recorded chord is offered to
//! the action the recorder is bound to ([`ShortcutRecorder::on_action`]),
//! which decides whether it is taken — the state then shows what was stored.

use crate::widget::{AppModel, ModelPath};
use std::collections::HashMap;

use crate::widget::binding::{BoxedBinding, Pulled};
use crate::widget::{ActionEmitter, ControlBindingKind, ControlBindingMetadata};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::a11y::{AccessNode, AccessState, Role};
use crate::input::{InputBinding, KeyLegend, KeyboardBinding, KeyboardModifier};
use crate::interaction::InteractionEnvironment;
use crate::platform::PlatformConventions;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::{InputHint, LegendConventions, TaffyTreeExt, Widget};

pub struct ShortcutRecorder {
    id: String,
    /// The chord shown.
    stored: BoxedBinding<String>,
    /// Whether the chord shown was refused.
    rejected: Option<BoxedBinding<bool>>,
    /// Where a recorded chord goes.
    action: Option<ActionEmitter<String>>,
    default: String,
    value: String,
    invalid: bool,
    rect: Rect,
    pointer: Option<Point>,
    focused: bool,
    capturing: bool,
    platform: PlatformConventions,
    interaction: InteractionEnvironment,
    /// The width of the keycaps, or of the capture prompt while capturing,
    /// measured with the text renderer.
    keys_width: f64,
    /// The geometry of the latest theme: the control's height, the reset
    /// button's edge and the gap before it.
    height: f64,
    reset_edge: f64,
    gap: f64,
    /// The theme of the latest layout, for measuring the keycaps.
    layout_theme: Option<Theme>,
}

impl ShortcutRecorder {
    /// A recorder showing `state`'s chord, whose default is `default`.
    pub fn new(id: &str, state: &crate::LocalState<String>, default: &str) -> Self {
        let (value, stored) = super::binding::local(state);
        Self {
            id: id.into(),
            stored,
            rejected: None,
            action: None,
            default: default.into(),
            value,
            invalid: false,
            rect: Rect::ZERO,
            pointer: None,
            focused: false,
            capturing: false,
            platform: PlatformConventions::default(),
            interaction: InteractionEnvironment::default(),
            keys_width: 0.0,
            height: f64::from(crate::tokens::Metrics::BASE.min_pointer_target),
            reset_edge: f64::from(crate::tokens::Metrics::BASE.bar_control_height),
            gap: f64::from(crate::tokens::Spacing::BASE.s2),
            layout_theme: None,
        }
    }

    /// The keycaps start on the control's leading edge, as an
    /// [`InputHint`]'s do in the rows around it.
    fn chord_rect(&self) -> Rect {
        let x1 = (self.rect.x0 + self.keys_width).min(self.rect.x1);
        Rect::new(self.rect.x0, self.rect.y0, x1, self.rect.y1)
    }

    /// The reset button, after the keycaps, while the value differs from
    /// its default.
    /// Offer every recorded chord, and a reset to the default, to `binding`.
    #[must_use]
    pub fn on_action(mut self, binding: tilo_actions::ActionBinding<String>) -> Self {
        self.action = Some(ActionEmitter::new(binding));
        self
    }

    fn reset_rect(&self) -> Option<Rect> {
        (self.value != self.default).then(|| {
            let x0 = self.chord_rect().x1 + self.gap;
            let cy = self.rect.center().y;
            Rect::new(
                x0,
                cy - self.reset_edge / 2.0,
                x0 + self.reset_edge,
                cy + self.reset_edge / 2.0,
            )
        })
    }

    /// The control's width: the keycaps (or the capture prompt), and the
    /// reset button when it shows.
    fn content_width(&self) -> f64 {
        let reset = if self.value == self.default {
            0.0
        } else {
            self.gap + self.reset_edge
        };
        self.keys_width + reset
    }

    fn measure_keys(&mut self, text: &mut TextRenderer, theme: &Theme) {
        let keys = InputHint::measure_width(
            &InputBinding::Keyboard(self.binding()),
            LegendConventions::new(self.platform, &self.interaction),
            text,
            theme,
        );
        let prompt = f64::from(
            text.measure_style(CAPTURE_PROMPT, None, &theme.text(TextRole::Label))
                .0,
        );
        // The control is as wide as what it shows: its keycaps, or the
        // prompt while it captures. Room kept for the prompt beside short
        // chords would leave their caps short of the column's edge.
        self.keys_width = if self.capturing { prompt } else { keys }.ceil();
    }

    fn binding(&self) -> KeyboardBinding {
        binding_from_text(&self.value)
    }

    fn write(&mut self, _state: &AppModel, _root: &ModelPath, value: String) {
        if let Some(action) = &mut self.action {
            let _ = action.emit(value.clone());
        } else {
            self.stored.publish(&value);
        }
        self.value = value;
        self.capturing = false;
    }
}

/// Shown in place of the keycaps while a new chord is being recorded.
const CAPTURE_PROMPT: &str = "Type a shortcut";

impl Widget for ShortcutRecorder {
    crate::provides!(dyn crate::widget::controls::ActionControls);

    fn set_platform_conventions(&mut self, conventions: PlatformConventions) {
        self.platform = conventions;
        self.interaction.keyboard_family =
            if conventions.platform() == crate::platform::Platform::MacOs {
                crate::interaction::KeyboardFamily::Apple
            } else {
                crate::interaction::KeyboardFamily::Pc
            };
    }

    fn set_interaction_environment(&mut self, environment: InteractionEnvironment) {
        self.interaction = environment;
    }

    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        if let Pulled::Value(value) = self.stored.pull(state, root) {
            self.value = value;
        }
        if let Some(rejected) = &mut self.rejected
            && let Pulled::Value(invalid) = rejected.pull(state, root)
        {
            self.invalid = invalid;
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.height = crate::widget::ControlSize::Regular.height(theme);
        self.reset_edge = crate::widget::ControlSize::Small.height(theme);
        self.gap = f64::from(theme.tokens.spacing.s2);
        self.layout_theme = Some(theme.clone());
        let style = Style {
            size: Size::from_lengths(
                crate::num::f32_of_f64(self.content_width()),
                crate::num::f32_of_f64(self.height),
            ),
            // Fixed target: never the item that gives in an overflowing row.
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn adjust_layout_style(
        &mut self,
        tree: &mut TaffyTree<()>,
        node: NodeId,
        text: &mut TextRenderer,
    ) -> bool {
        let Some(theme) = self.layout_theme.take() else {
            return false;
        };
        self.measure_keys(text, &theme);
        self.layout_theme = Some(theme);
        let Ok(current) = tree.style(node) else {
            return false;
        };
        let mut style = current.clone();
        style.size.width = Dimension::length(crate::num::f32_of_f64(self.content_width()));
        if style == *current {
            return false;
        }
        tree.set_style(node, style).is_ok()
    }

    crate::rect_geometry!();

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.measure_keys(text, theme);
        let chord = self.chord_rect();
        let keycap = InputHint::keycap_height(theme);
        let top = chord.center().y - keycap / 2.0;
        // The keycaps sit where an InputHint's would; hover, focus and
        // capture draw their ring outward, round them.
        let ring = chord
            .with_origin(Point::new(chord.x0, top))
            .with_size((chord.width(), keycap))
            .inflate(
                f64::from(theme.tokens.spacing.s1),
                f64::from(theme.tokens.spacing.s1),
            );
        let hovered = self.pointer.is_some_and(|p| ring.contains(p));
        if hovered || self.focused || self.capturing || self.invalid {
            let ink = if self.invalid {
                theme.tokens.colors.semantic.danger
            } else if self.focused || self.capturing {
                theme.tokens.colors.intent.you
            } else {
                crate::surface::edge_on(theme.tokens.colors.surfaces.content)
            };
            scene.stroke(
                &Stroke::new(if self.capturing { 2.0 } else { 1.0 }),
                transform,
                ink,
                None,
                &RoundedRect::from_rect(ring, f64::from(theme.tokens.radii.item)),
            );
        }
        if self.capturing {
            let style = theme.text(TextRole::Label);
            let (_, h) = text.measure_style(CAPTURE_PROMPT, None, &style);
            text.draw_style(
                scene,
                CAPTURE_PROMPT,
                theme.tokens.colors.text_dim,
                transform.pre_translate((chord.x0, chord.center().y - f64::from(h) / 2.0).into()),
                None,
                &style,
            );
        } else {
            InputHint::paint_at(
                &mut crate::paint::PaintCtx::new(scene, transform, text, theme),
                Point::new(chord.x0, top),
                &InputBinding::Keyboard(self.binding()),
                LegendConventions::new(self.platform, &self.interaction),
            );
        }
        if let Some(reset) = self.reset_rect() {
            let hovered = self.pointer.is_some_and(|p| reset.contains(p));
            crate::widget::Button::paint(
                scene,
                transform,
                text,
                theme,
                &crate::widget::ButtonFace::icon_only(
                    reset,
                    crate::icons::Icon::RotateCcw,
                    "Reset to default",
                )
                .control_size(crate::widget::ControlSize::Small)
                .hovered(hovered),
            );
        }
        if self.invalid && !self.capturing {
            let edge = f64::from(theme.tokens.metrics.icon_sm);
            let x0 = self.reset_rect().map_or(chord.x1, |r| r.x1) + self.gap;
            crate::icons::draw_icon(
                scene,
                transform,
                Rect::new(
                    x0,
                    chord.center().y - edge / 2.0,
                    x0 + edge,
                    chord.center().y + edge / 2.0,
                ),
                crate::icons::Icon::TriangleAlert,
                theme.tokens.colors.semantic.danger,
                crate::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = Some(ctx.logical(position));
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let Some(point) = self.pointer else { return };
                if let Some(reset) = self.reset_rect().filter(|r| r.contains(point)) {
                    let _ = reset;
                    self.write(ctx.state.doc(), ctx.path, self.default.clone());
                    *ctx.focus_target = Some(ctx.node_id);
                    *ctx.handled = true;
                } else if self.chord_rect().contains(point) {
                    self.capturing = true;
                    *ctx.focus_target = Some(ctx.node_id);
                    *ctx.handled = true;
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed && self.focused =>
            {
                if self.capturing {
                    if matches!(event.logical_key, Key::Named(NamedKey::Escape)) {
                        self.capturing = false;
                    } else if let Some(value) = chord_from_event(&event.logical_key, *ctx.modifiers)
                    {
                        self.write(ctx.state.doc(), ctx.path, value);
                    }
                    *ctx.handled = true;
                } else if matches!(
                    event.logical_key,
                    Key::Named(NamedKey::Enter | NamedKey::Space)
                ) {
                    self.capturing = true;
                    *ctx.handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "ShortcutRecorder"
    }
    fn test_id(&self) -> Option<&str> {
        Some(&self.id)
    }
    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.rect
            .contains(point)
            .then_some(winit::window::CursorIcon::Pointer)
    }
    fn tooltip(&self) -> Option<&str> {
        self.invalid.then_some("Shortcut unavailable")
    }
    fn is_focusable(&self) -> bool {
        true
    }
    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
        if !focused {
            self.capturing = false;
        }
    }
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn accessibility(&self) -> Option<AccessNode> {
        let mut state = AccessState::new().focusable(true).focused(self.focused);
        state.invalid = self.invalid;
        let mut node = AccessNode::new(Role::Button)
            .with_name("Change shortcut")
            .with_value(self.binding().accessible_label(self.platform))
            .with_state(state);
        if self.invalid {
            node =
                node.with_description("That shortcut is unavailable. Activate to choose another.");
        }
        Some(node)
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        _key_path: &[u64],
        _state: &mut crate::widget::Edits<'_>,
        _root: &ModelPath,
    ) -> bool {
        if matches!(action, crate::a11y::Action::Activate) {
            self.capturing = true;
            true
        } else {
            false
        }
    }
}

fn binding_from_text(value: &str) -> KeyboardBinding {
    let mut modifiers = Vec::new();
    let mut key = KeyLegend::Character("—".into());
    for part in value
        .split('+')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        match part.to_ascii_lowercase().as_str() {
            "super" | "meta" | "cmd" | "command" => modifiers.push(KeyboardModifier::System),
            "ctrl" | "control" => modifiers.push(KeyboardModifier::Control),
            "alt" | "option" => modifiers.push(KeyboardModifier::Alt),
            "shift" => modifiers.push(KeyboardModifier::Shift),
            "fn" => modifiers.push(KeyboardModifier::Fn),
            _ => key = KeyLegend::from_name(part),
        }
    }
    KeyboardBinding::new(modifiers, key)
}

fn chord_from_event(key: &Key, modifiers: winit::event::Modifiers) -> Option<String> {
    let key = match key {
        Key::Character(value) => value.to_lowercase(),
        Key::Named(NamedKey::ArrowLeft) => "Left".into(),
        Key::Named(NamedKey::ArrowRight) => "Right".into(),
        Key::Named(NamedKey::ArrowUp) => "Up".into(),
        Key::Named(NamedKey::ArrowDown) => "Down".into(),
        Key::Named(NamedKey::Tab) => "Tab".into(),
        Key::Named(NamedKey::Enter) => "Enter".into(),
        Key::Named(NamedKey::Backspace) => "Backspace".into(),
        Key::Named(NamedKey::Delete) => "Delete".into(),
        Key::Named(NamedKey::Home) => "Home".into(),
        Key::Named(NamedKey::End) => "End".into(),
        Key::Named(NamedKey::PageUp) => "PageUp".into(),
        Key::Named(NamedKey::PageDown) => "PageDown".into(),
        Key::Named(NamedKey::F1) => "F1".into(),
        Key::Named(NamedKey::F2) => "F2".into(),
        Key::Named(NamedKey::F3) => "F3".into(),
        Key::Named(NamedKey::F4) => "F4".into(),
        Key::Named(NamedKey::F5) => "F5".into(),
        Key::Named(NamedKey::F6) => "F6".into(),
        Key::Named(NamedKey::F7) => "F7".into(),
        Key::Named(NamedKey::F8) => "F8".into(),
        Key::Named(NamedKey::F9) => "F9".into(),
        Key::Named(NamedKey::F10) => "F10".into(),
        Key::Named(NamedKey::F11) => "F11".into(),
        Key::Named(NamedKey::F12) => "F12".into(),
        _ => return None,
    };
    let state = modifiers.state();
    let mut parts = Vec::new();
    if state.super_key() {
        parts.push("Super".to_string());
    }
    if state.control_key() {
        parts.push("Ctrl".to_string());
    }
    if state.alt_key() {
        parts.push("Alt".to_string());
    }
    if state.shift_key() {
        parts.push("Shift".to_string());
    }
    if parts.is_empty() && key.chars().count() == 1 {
        return None;
    }
    parts.push(key);
    Some(parts.join("+"))
}

impl crate::widget::controls::ActionControls for ShortcutRecorder {
    crate::widget::controls::optional_emitter_controls!();
    fn control_action_bindings(&self) -> Vec<ControlBindingMetadata> {
        match &self.action {
            Some(action) => vec![action.metadata(&self.id)],
            None => vec![ControlBindingMetadata {
                control_id: self.id.clone(),
                kind: ControlBindingKind::SemanticAction(self.id.clone()),
            }],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recorder(value: &str, default: &str) -> ShortcutRecorder {
        ShortcutRecorder::new("x", &crate::LocalState::new(value.to_owned()), default)
    }

    #[test]
    fn binding_display_names_keys_and_offers_reset_only_when_changed() {
        // Case `reset_exists_only_for_a_changed_binding`.
        {
            let unchanged = recorder("Super+Left", "Super+Left");
            assert!(unchanged.reset_rect().is_none());
            let changed = recorder("Super+H", "Super+Left");
            assert!(changed.reset_rect().is_some());
        }

        // Case `visual_binding_uses_readable_named_keys`.
        {
            let recorder = recorder("Super+Down", "Super+Left");
            assert_eq!(
                recorder
                    .binding()
                    .keycap_labels(PlatformConventions::new(crate::platform::Platform::Linux)),
                ["Super", "Down"]
            );
            assert_eq!(
                recorder
                    .binding()
                    .keycap_labels(PlatformConventions::new(crate::platform::Platform::MacOs)),
                ["⌘", "↓"]
            );
        }
    }
}
