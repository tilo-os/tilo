//! A compact, visual keyboard-shortcut editor. The displayed keycaps are the control: click
//! them, or focus them and press Enter/Space, then type the replacement chord. Escape cancels;
//! Backspace or Delete puts back the default.

use tilo_ui_base::{widget::AppModel, widget::ModelPath};
use std::collections::HashMap;

use tilo_ui_base::{widget::binding::BoxedBinding, widget::binding::Pulled};
use tilo_ui_base::{widget::ActionEmitter, widget::ControlBindingKind, widget::ControlBindingMetadata};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::kurbo::{Point, Rect, RoundedRect, Stroke};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use tilo_ui_base::{a11y::AccessNode, a11y::AccessState, a11y::Role};
use tilo_ui_base::{input::InputBinding, input::KeyLegend, input::KeyboardBinding, input::KeyboardModifier};
use tilo_ui_base::interaction::InteractionEnvironment;
use tilo_ui_base::platform::PlatformConventions;

use tilo_ui_base::text_render::TextRenderer;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::TextRole;
#[path = "shortcut_recorder_component.rs"]
mod component;
use crate::{widget::InputHint, widget::LegendConventions};
use tilo_ui_base::{widget::TaffyTreeExt, widget::Widget};
pub use component::ShortcutRecorderProps;

pub struct ShortcutRecorder {
    id: String,
    stored: BoxedBinding<String>,
    /// Whether the chord shown was refused.
    rejected: Option<BoxedBinding<bool>>,
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
    keys_width: f64,
    /// The geometry of the latest theme: the control's height, the reset
    /// button's edge and the gap before it.
    height: f64,
    reset_edge: f64,
    gap: f64,
    layout_theme: Option<Theme>,
    /// What its width was last fitted to: whether it is capturing, and the chord it shows.
    /// While they stand, it asks for no layout.
    fitted: Option<(bool, String)>,
    edits: Option<tilo_ui_base::component::Bound<String>>,

    /// Input or a new document value changed what this widget paints: it
    /// repaints once, and rests after the frame draws it.
    dirty: bool,
}

impl ShortcutRecorder {
    /// A recorder showing `state`'s chord, whose default is `default`.
    pub fn new(id: &str, state: &tilo_ui_base::LocalState<String>, default: &str) -> Self {
        let (value, stored) = tilo_ui_base::widget::binding::local(state);
        Self {
            dirty: true,
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
            fitted: None,
            height: f64::from(tilo_ui_base::tokens::Metrics::BASE.min_pointer_target),
            reset_edge: f64::from(tilo_ui_base::tokens::Metrics::BASE.bar_control_height),
            gap: f64::from(tilo_ui_base::tokens::Spacing::BASE.s2),
            layout_theme: None,
            edits: None,
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

    /// Report each recorded chord to the component binding `edits`, as a described component
    /// does: the person's recording and reset, never a chord the view gave.
    #[must_use]
    pub fn on_edit(mut self, edits: tilo_ui_base::component::Bound<String>) -> Self {
        self.edits = Some(edits);
        self
    }

    #[must_use]
    pub fn chord(&self) -> &str {
        &self.value
    }

    #[must_use]
    pub fn default_chord(&self) -> &str {
        &self.default
    }

    /// Show `chord` in place, as one set elsewhere.
    pub fn set_chord(&mut self, chord: &str) {
        self.dirty = true;
        if self.value != chord {
            chord.clone_into(&mut self.value);
            self.capturing = false;
        }
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
        let words = |text: &mut TextRenderer, key: &str| {
            let said = tilo_ui_base::i18n::widget_format(key, &[]);
            f64::from(
                text.measure_style(&said, None, &theme.text(TextRole::Label))
                    .0,
            )
        };
        self.keys_width = if self.capturing {
            words(text, "widget-shortcut-prompt")
        } else if self.value.is_empty() {
            words(text, "widget-shortcut-none")
        } else {
            keys
        }
        .ceil();
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
        if let Some(edits) = &self.edits {
            edits.edited(tilo_ui_base::component::Value::Text(value.clone()));
        }
        self.value = value;
        self.capturing = false;
    }
}

impl Widget for ShortcutRecorder {
    tilo_ui_base::described!();

    tilo_ui_base::provides!(dyn tilo_ui_base::widget::controls::ActionControls);

    fn set_platform_conventions(&mut self, conventions: PlatformConventions) {
        self.platform = conventions;
        self.interaction.keyboard_family =
            if conventions.platform() == tilo_ui_base::platform::Platform::MacOs {
                tilo_ui_base::interaction::KeyboardFamily::Apple
            } else {
                tilo_ui_base::interaction::KeyboardFamily::Pc
            };
    }

    fn set_interaction_environment(&mut self, environment: InteractionEnvironment) {
        self.interaction = environment;
    }

    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        if let Pulled::Value(value) = self.stored.pull(state, root)
            && self.value != value
        {
            self.value = value;
            self.dirty = true;
        }
        if let Some(rejected) = &mut self.rejected
            && let Pulled::Value(invalid) = rejected.pull(state, root)
            && self.invalid != invalid
        {
            self.invalid = invalid;
            self.dirty = true;
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.height = crate::widget::ControlSize::Regular
            .height(theme)
            .max(f64::from(self.interaction.minimum_target()));
        self.reset_edge = crate::widget::ControlSize::Small.height(theme);
        self.gap = f64::from(theme.tokens.spacing.s2);
        self.layout_theme = Some(theme.clone());
        let style = Style {
            size: Size::from_lengths(
                tilo_ui_base::num::f32_of_f64(self.content_width()),
                tilo_ui_base::num::f32_of_f64(self.height),
            ),
            // Fixed target: never the item that gives in an overflowing row.
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    /// Its width is the chord it shows, measured before the first solve.
    fn size_before_solve(
        &mut self,
        tree: &mut TaffyTree<()>,
        node: NodeId,
        text: &mut TextRenderer,
    ) {
        let Some(theme) = self.layout_theme.take() else {
            return;
        };
        self.measure_keys(text, &theme);
        self.layout_theme = Some(theme);
        self.fitted = Some((self.capturing, self.value.clone()));
        let Ok(current) = tree.style(node) else {
            return;
        };
        let mut style = current.clone();
        style.size.width = Dimension::length(tilo_ui_base::num::f32_of_f64(self.content_width()));
        if style != *current {
            let _ = tree.set_style(node, style);
        }
    }

    fn is_layout_dirty(&self) -> bool {
        self.fitted
            .as_ref()
            .is_none_or(|(capturing, value)| *capturing != self.capturing || *value != self.value)
    }

    tilo_ui_base::rect_geometry!();

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        self.measure_keys(text, theme);
        let chord = self.chord_rect();
        let keycap = InputHint::keycap_height(theme);
        let top = chord.center().y - keycap / 2.0;
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
                theme.tokens.colors.intent.accent_ink
            } else {
                tilo_ui_base::surface::edge_on(theme.tokens.colors.surfaces.content)
            };
            scene.stroke(
                &Stroke::new(if self.capturing { 2.0 } else { 1.0 }),
                transform,
                ink,
                None,
                &RoundedRect::from_rect(ring, f64::from(theme.tokens.radii.item)),
            );
        }
        // While it listens it asks for the keys; with none set it says so, quietly.
        let words = if self.capturing {
            Some("widget-shortcut-prompt")
        } else if self.value.is_empty() {
            Some("widget-shortcut-none")
        } else {
            None
        };
        if let Some(key) = words {
            let said = tilo_ui_base::i18n::widget_format(key, &[]);
            let style = theme.text(TextRole::Label);
            let (_, h) = text.measure_style(&said, None, &style);
            text.draw_style(
                scene,
                &said,
                theme.tokens.colors.text_dim,
                transform.pre_translate((chord.x0, chord.center().y - f64::from(h) / 2.0).into()),
                None,
                &style,
            );
        } else {
            InputHint::paint_at(
                &mut tilo_ui_base::paint::PaintCtx::new(scene, transform, text, theme),
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
                    tilo_ui_base::icons::Icon::RotateCcw,
                    &tilo_ui_base::i18n::widget_format("widget-shortcut-reset", &[]),
                )
                .control_size(crate::widget::ControlSize::Small)
                .hovered(hovered),
            );
        }
        if self.invalid && !self.capturing {
            let edge = f64::from(theme.tokens.metrics.icon_sm);
            let x0 = self.reset_rect().map_or(chord.x1, |r| r.x1) + self.gap;
            tilo_ui_base::icons::draw_icon(
                scene,
                transform,
                Rect::new(
                    x0,
                    chord.center().y - edge / 2.0,
                    x0 + edge,
                    chord.center().y + edge / 2.0,
                ),
                tilo_ui_base::icons::Icon::TriangleAlert,
                theme.tokens.colors.semantic.danger,
                tilo_ui_base::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
        self.dirty = true;
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
                } else if matches!(
                    event.logical_key,
                    Key::Named(NamedKey::Backspace | NamedKey::Delete)
                ) && self.value != self.default
                {
                    // What the reset button does, from the keyboard.
                    self.write(ctx.state.doc(), ctx.path, self.default.clone());
                    *ctx.handled = true;
                }
            }
            _ => {}
        }
    }

    fn needs_redraw(&self) -> bool {
        tilo_ui_base::widget::redraw_or_children(self.dirty, self)
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
        tilo_ui_base::widget::Widget::mark_children_drawn(self);
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
    fn tooltip(&self) -> Option<std::borrow::Cow<'_, str>> {
        self.invalid
            .then_some(std::borrow::Cow::Borrowed("Shortcut unavailable"))
    }
    fn is_focusable(&self) -> bool {
        true
    }
    fn set_focus(&mut self, focused: bool) {
        self.dirty = true;
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
            .with_bounds(self.interaction.minimum_target_rect(self.rect))
            .with_name(tilo_ui_base::i18n::widget_format("widget-shortcut-change", &[]))
            .with_value(self.binding().accessible_label(self.platform))
            .with_state(state);
        if self.invalid {
            node = node.with_description(tilo_ui_base::i18n::widget_format(
                "widget-shortcut-unavailable",
                &[],
            ));
        }
        Some(node)
    }

    fn perform_action(
        &mut self,
        action: &tilo_ui_base::a11y::Action,
        _key_path: &[u64],
        _state: &mut tilo_ui_base::widget::Edits<'_>,
        _root: &ModelPath,
    ) -> bool {
        if matches!(action, tilo_ui_base::a11y::Action::Activate) {
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

impl tilo_ui_base::widget::controls::ActionControls for ShortcutRecorder {
    tilo_ui_base::widget::controls::optional_emitter_controls!();
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
        ShortcutRecorder::new("x", &tilo_ui_base::LocalState::new(value.to_owned()), default)
    }

    #[test]
    fn binding_display_names_keys_and_offers_reset_only_when_changed() {
        {
            let unchanged = recorder("Super+Left", "Super+Left");
            assert!(unchanged.reset_rect().is_none());
            let changed = recorder("Super+H", "Super+Left");
            assert!(changed.reset_rect().is_some());
        }

        {
            let recorder = recorder("Super+Down", "Super+Left");
            assert_eq!(
                recorder
                    .binding()
                    .keycap_labels(PlatformConventions::new(tilo_ui_base::platform::Platform::Linux)),
                ["Super", "Down"]
            );
            assert_eq!(
                recorder
                    .binding()
                    .keycap_labels(PlatformConventions::new(tilo_ui_base::platform::Platform::MacOs)),
                ["⌘", "↓"]
            );
        }
    }

    #[test]
    fn no_shortcut_reads_as_none_rather_than_a_keycap() {
        let theme = tilo_ui_base::theme::Theme::light();
        let mut text = TextRenderer::new();
        let mut empty = recorder("", "Ctrl+K");
        empty.measure_keys(&mut text, &theme);
        let none = tilo_ui_base::i18n::widget_format("widget-shortcut-none", &[]);
        let (width, _) = text.measure_style(&none, None, &theme.text(TextRole::Label));
        assert_eq!(empty.keys_width, f64::from(width).ceil());
        assert!(empty.reset_rect().is_some(), "the default is a press away");
    }

    #[test]
    fn backspace_on_a_changed_shortcut_puts_back_the_default() {
        let state = tilo_ui_base::LocalState::new("Ctrl+J".to_owned());
        let mut host = crate::app::driver::Driver::for_root(
            crate::State::new(),
            ShortcutRecorder::new("x", &state, "Ctrl+K"),
            (400, 200),
            tilo_ui_base::theme::Theme::light(),
        );
        let at = host.root::<ShortcutRecorder>().chord_rect().center();
        host.click_at(at.x, at.y);
        host.key("Escape");
        assert_eq!(
            state.get(),
            "Ctrl+J",
            "Escape leaves the shortcut as it was"
        );
        host.key("Backspace");
        assert_eq!(state.get(), "Ctrl+K");
    }
}
