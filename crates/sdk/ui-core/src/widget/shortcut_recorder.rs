//! A compact, visual keyboard-shortcut editor.
//!
//! The displayed keycaps are the control: click them, or focus them and press
//! Enter/Space, then type the replacement chord. Escape cancels. A reset
//! affordance is present only while the value differs from its default.

use crate::widget::{AppModel, ModelPath};
use std::collections::HashMap;

use crate::widget::binding::{BoxedBinding, Pulled};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::a11y::{AccessNode, AccessState, Role};
use crate::input::{InputBinding, KeyLegend, KeyboardBinding, KeyboardModifier};
use crate::interaction::InteractionEnvironment;
use crate::platform::PlatformConventions;
use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::{InputHint, LegendConventions, TaffyTreeExt, Widget};

const HEIGHT: f64 = 34.0;
const RESET_SIZE: f64 = 26.0;

pub struct ShortcutRecorder {
    id: String,
    /// The chord text, as a document key.
    stored: BoxedBinding<String>,
    /// Whether the model rejected the chord (`<key>.error`).
    rejected: BoxedBinding<bool>,
    default: String,
    value: String,
    invalid: bool,
    rect: Rect,
    pointer: Option<Point>,
    focused: bool,
    capturing: bool,
    platform: PlatformConventions,
    interaction: InteractionEnvironment,
}

impl ShortcutRecorder {
    pub fn new(id: &str, path: &str, value: &str, default: &str) -> Self {
        Self {
            id: id.into(),
            stored: crate::doc_binding::str_key(path),
            rejected: crate::doc_binding::bool_key(&format!("{path}.error")),
            default: default.into(),
            value: value.into(),
            invalid: false,
            rect: Rect::ZERO,
            pointer: None,
            focused: false,
            capturing: false,
            platform: PlatformConventions::default(),
            interaction: InteractionEnvironment::default(),
        }
    }

    fn reset_rect(&self) -> Option<Rect> {
        (self.value != self.default).then(|| {
            Rect::new(
                self.rect.x1 - RESET_SIZE,
                self.rect.center().y - RESET_SIZE / 2.0,
                self.rect.x1,
                self.rect.center().y + RESET_SIZE / 2.0,
            )
        })
    }

    fn chord_rect(&self) -> Rect {
        let right = self.reset_rect().map_or(self.rect.x1, |r| r.x0 - 6.0);
        Rect::new(self.rect.x0, self.rect.y0, right, self.rect.y1)
    }

    fn binding(&self) -> KeyboardBinding {
        binding_from_text(&self.value)
    }

    fn write(&mut self, state: &mut AppModel, root: &ModelPath, value: String) {
        self.stored.push(state, root, &value);
        self.value = value;
        self.capturing = false;
    }
}

impl Widget for ShortcutRecorder {
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

    fn sync(&mut self, state: &mut AppModel, root: &ModelPath) {
        if let Pulled::Value(value) = self.stored.pull(state, root) {
            self.value = value;
        }
        self.invalid = matches!(self.rejected.pull(state, root), Pulled::Value(true));
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::length(230.0),
                height: Dimension::length(crate::num::f32_of_f64(HEIGHT)),
            },
            // Fixed target: never the item that gives in an overflowing row.
            flex_shrink: 0.0,
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
            origin.x + f64::from(layout.location.x + layout.size.width),
            origin.y + f64::from(layout.location.y + layout.size.height),
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let chord = self.chord_rect();
        let hovered = self.pointer.is_some_and(|p| chord.contains(p));
        if hovered || self.focused || self.capturing || self.invalid {
            scene.stroke(
                &Stroke::new(if self.capturing { 2.0 } else { 1.0 }),
                transform,
                if self.invalid {
                    theme.tokens.colors.semantic.danger
                } else {
                    theme.tokens.colors.intent.inverted
                },
                None,
                &RoundedRect::from_rect(chord, 7.0),
            );
        }
        if self.capturing {
            let label = "…";
            let (w, h) = text.measure_style(label, None, &theme.text(TextRole::Caption));
            text.draw_style(
                scene,
                label,
                theme.tokens.colors.text_primary,
                transform.pre_translate(
                    (
                        chord.center().x - f64::from(w) / 2.0,
                        chord.center().y - f64::from(h) / 2.0,
                    )
                        .into(),
                ),
                None,
                &theme
                    .text(TextRole::Label)
                    .with_weight(TextWeight::Semibold),
            );
        } else {
            InputHint::paint_at(
                &mut crate::paint::PaintCtx::new(scene, transform, text, theme),
                Point::new(
                    chord.x0 + 5.0,
                    chord.center().y - crate::widget::input_hint::HEIGHT / 2.0,
                ),
                &InputBinding::Keyboard(self.binding()),
                LegendConventions::new(self.platform, &self.interaction),
            );
            if self.invalid {
                text.draw_style(
                    scene,
                    "!",
                    theme.tokens.colors.semantic.danger,
                    transform.pre_translate((chord.x1 - 14.0, chord.y0 + 7.0).into()),
                    None,
                    &theme
                        .text(TextRole::Label)
                        .with_weight(TextWeight::Semibold),
                );
            }
        }
        if let Some(reset) = self.reset_rect() {
            let hovered = self.pointer.is_some_and(|p| reset.contains(p));
            if hovered {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    theme.tokens.colors.intent.selection_hover,
                    None,
                    &RoundedRect::from_rect(reset, 6.0),
                );
            }
            let (w, h) = text.measure_style("↶", None, &theme.text(TextRole::Body));
            text.draw_style(
                scene,
                "↶",
                theme.tokens.colors.text_primary,
                transform.pre_translate(
                    (
                        reset.center().x - f64::from(w) / 2.0,
                        reset.center().y - f64::from(h) / 2.0,
                    )
                        .into(),
                ),
                None,
                &theme.text(TextRole::Body),
            );
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = Some(Point::new(
                    position.x / ctx.scale_factor,
                    position.y / ctx.scale_factor,
                ));
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let Some(point) = self.pointer else { return };
                if let Some(reset) = self.reset_rect().filter(|r| r.contains(point)) {
                    let _ = reset;
                    self.write(ctx.state, ctx.path, self.default.clone());
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
                        self.write(ctx.state, ctx.path, value);
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
    fn get_rect(&self) -> Rect {
        self.rect
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
        _state: &mut AppModel,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_display_names_keys_and_offers_reset_only_when_changed() {
        // Case `reset_exists_only_for_a_changed_binding`.
        {
            let unchanged = ShortcutRecorder::new("x", "x", "Super+Left", "Super+Left");
            assert!(unchanged.reset_rect().is_none());
            let changed = ShortcutRecorder::new("x", "x", "Super+H", "Super+Left");
            assert!(changed.reset_rect().is_some());
        }

        // Case `visual_binding_uses_readable_named_keys`.
        {
            let recorder = ShortcutRecorder::new("x", "x", "Super+Down", "Super+Left");
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
