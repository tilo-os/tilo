//! A physical keyboard map. Geometry and XKB legends are provided by the
//! device's native compiler; focused pressed keys are private paint state.
use crate::{
    a11y::{AccessNode, AccessState, Role},
    presence::PresenceCtx,
    text_render::TextRenderer,
    theme::Theme,
    tokens::TextRole,
    widget::{EventCtx, TaffyTreeExt, Widget},
};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::{
    Scene,
    kurbo::{Affine, BezPath, Point, Rect},
    peniko::Fill,
};
use winit::{
    event::{ElementState, WindowEvent},
    keyboard::PhysicalKey,
};

/// A key's physical outline and ordered XKB levels, in model coordinates.
#[derive(Clone, Debug)]
pub struct KeyboardMapKey {
    /// Native XKB physical key name (such as AD06), not typed input.
    pub name: String,
    /// The same native physical identifier delivered by the window backend.
    pub physical: PhysicalKey,
    /// Physical outline in the selected model's coordinate units.
    pub outline: Vec<Point>,
    /// Compiled legends in native XKB level order.
    pub levels: Vec<String>,
}

#[derive(Default)]
struct PressState {
    foreground: bool,
    keys: HashSet<PhysicalKey>,
}

/// Private, process-local physical press state shared across page recomposition.
/// It contains no logical keys or text and is never a resource or document value.
#[derive(Clone)]
pub struct KeyboardMapInput {
    state: Arc<Mutex<PressState>>,
    wake: crate::WakeHandle,
}

impl Default for KeyboardMapInput {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(PressState::default())),
            wake: crate::WakeHandle::new(),
        }
    }
}

impl KeyboardMapInput {
    /// Forget held positions when the owning foreground page disappears.
    pub fn clear(&self) {
        let changed = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.foreground = false;
            let changed = !state.keys.is_empty();
            state.keys.clear();
            changed
        };
        if changed {
            self.wake.wake();
        }
    }
}

/// Theme-aware map of an explicitly selected physical model. It observes
/// only ordinary focused window events, never global devices or document edits.
#[derive(Clone)]
pub struct KeyboardMap {
    keys: Vec<KeyboardMapKey>,
    model_size: Size<f64>,
    label: String,
    extra_levels: bool,
    rect: Rect,
    focused: bool,
    input: KeyboardMapInput,
    pointer: Option<Point>,
    dirty: bool,
}

impl KeyboardMap {
    /// Mount compiled native legends over their explicitly selected model.
    pub fn new(keys: Vec<KeyboardMapKey>, model_size: Size<f64>, label: String) -> Self {
        Self {
            keys,
            model_size,
            label,
            extra_levels: false,
            rect: Rect::ZERO,
            focused: false,
            input: KeyboardMapInput::default(),
            pointer: None,
            dirty: true,
        }
    }

    /// Keep the owning page's private press state across retained recomposition.
    pub fn with_input(mut self, input: KeyboardMapInput) -> Self {
        self.input = input;
        self
    }

    /// Show levels 5–8 when the selected XKB layout supplies them.
    /// Layouts with fewer levels retain their first bank.
    pub fn with_extra_levels(mut self, extra: bool) -> Self {
        self.extra_levels = extra && self.level_count() > 4;
        self
    }

    /// Number of native XKB levels present in the selected map.
    pub fn level_count(&self) -> usize {
        self.keys
            .iter()
            .map(|key| key.levels.len())
            .max()
            .unwrap_or_default()
    }

    /// Observe the active page's ordinary window events without taking focus
    /// or consuming input. Only physical positions are retained; editing stays
    /// with the focused control. The owning app must stop observing on page exit.
    pub fn observe_foreground_event(&mut self, event: &WindowEvent) {
        self.input
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .foreground = !matches!(event, WindowEvent::Focused(false));
        self.observe_window_event(event);
    }

    /// Ordinary window event route, shared with the retained widget handler.
    /// No logical key or typed text is retained.
    pub fn observe_window_event(&mut self, event: &WindowEvent) {
        if let Some(input) = crate::widget::PhysicalInput::from_window_event(event) {
            self.observe_physical_input(input);
        }
    }

    /// Content-free foreground feedback, including framework-owned shortcuts.
    pub fn observe_physical_input(&mut self, input: crate::widget::PhysicalInput) {
        let changed = {
            let mut pressed = self
                .input
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match input {
                crate::widget::PhysicalInput::Focused(false) => {
                    self.focused = false;
                    pressed.foreground = false;
                    let changed = !pressed.keys.is_empty();
                    pressed.keys.clear();
                    changed
                }
                crate::widget::PhysicalInput::Focused(true) => {
                    pressed.foreground = true;
                    false
                }
                crate::widget::PhysicalInput::Key { key, state }
                    if self.keys.iter().any(|entry| entry.physical == key) =>
                {
                    pressed.foreground = true;
                    match state {
                        ElementState::Pressed => pressed.keys.insert(key),
                        ElementState::Released => pressed.keys.remove(&key),
                    }
                }
                _ => false,
            }
        };
        self.dirty |= changed;
        if changed {
            self.input.wake.wake();
        }
    }

    /// Whether the foreground preview paints this physical position.
    pub fn is_pressed(&self, key: PhysicalKey) -> bool {
        self.input
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .keys
            .contains(&key)
    }

    fn paint_key(
        &self,
        key: &KeyboardMapKey,
        scene: &mut Scene,
        transform: Affine,
        renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let mut path = BezPath::new();
        for (index, point) in key.outline.iter().enumerate() {
            if index == 0 {
                path.move_to(*point);
            } else {
                path.line_to(*point);
            }
        }
        path.close_path();
        let pressed = self.is_pressed(key.physical);
        let colours = &theme.tokens.colors;
        let fill = if pressed {
            colours.intent.inverted
        } else {
            colours.surfaces.raised
        };
        scene.fill(Fill::NonZero, transform, fill, None, &path);
        let rect = key.outline.iter().fold(Rect::ZERO, |rect, point| {
            if rect.is_zero_area() {
                Rect::new(point.x, point.y, point.x + 0.01, point.y + 0.01)
            } else {
                rect.union_pt(*point)
            }
        });
        // Four corners retain base, Shift, AltGr and Shift+AltGr independently.
        // Additional XKB levels remain in the accessible map's value.
        let style = theme.text(TextRole::Caption);
        let ink = if pressed {
            colours.surfaces.content
        } else {
            colours.text_primary
        };
        let scale = self.rect.width() / self.model_size.width.max(1.0);
        for (index, legend) in key
            .levels
            .iter()
            .skip(if self.extra_levels { 4 } else { 0 })
            .take(4)
            .enumerate()
        {
            if legend.is_empty() {
                continue;
            }
            let (width, height) = renderer.measure_style(legend, None, &style);
            let left = index < 2;
            let bottom = index % 2 == 0;
            let x = if key.levels.len() == 1 {
                (rect.x0 + rect.x1 - f64::from(width) / scale) / 2.0
            } else if left {
                rect.x0 + 2.0
            } else {
                rect.x1 - f64::from(width) / scale - 2.0
            };
            let y = if key.levels.len() == 1 {
                (rect.y0 + rect.y1 - f64::from(height) / scale) / 2.0
            } else if bottom {
                rect.y1 - f64::from(height) / scale - 1.0
            } else {
                rect.y0 + 1.0
            };
            renderer.draw_style(
                scene,
                legend,
                ink,
                transform * Affine::translate((x, y)) * Affine::scale(1.0 / scale),
                Some(crate::num::f32_of_f64(
                    (rect.width() * scale - 4.0).max(1.0),
                )),
                &style,
            );
        }
    }
}

impl Widget for KeyboardMap {
    fn observe_physical_input(&mut self, input: crate::widget::PhysicalInput) {
        KeyboardMap::observe_physical_input(self, input);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::auto(),
            },
            aspect_ratio: Some(crate::num::f32_of_f64(
                self.model_size.width / self.model_size.height.max(1.0),
            )),
            ..Style::default()
        });
        nodes.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::from_origin_size(
            (
                origin.x + f64::from(layout.location.x),
                origin.y + f64::from(layout.location.y),
            ),
            (f64::from(layout.size.width), f64::from(layout.size.height)),
        );
        self.dirty = true;
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let map = transform
            * Affine::translate((self.rect.x0, self.rect.y0))
            * Affine::scale(self.rect.width() / self.model_size.width.max(1.0));
        for key in &self.keys {
            self.paint_key(key, scene, map, renderer, theme);
        }
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.pointer = Some(Point::new(
                position.x / ctx.scale_factor,
                position.y / ctx.scale_factor,
            ));
        }
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: winit::event::MouseButton::Left,
                ..
            }
        ) && self.pointer.is_some_and(|point| self.rect.contains(point))
        {
            *ctx.focus_target = Some(ctx.node_id);
            self.set_focus(true);
            *ctx.handled = true;
        }
        self.observe_window_event(event);
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
    }
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn set_focus(&mut self, focused: bool) {
        self.dirty |= self.focused != focused;
        self.focused = focused;
        let changed = {
            let mut pressed = self
                .input
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !focused && !pressed.foreground {
                let changed = !pressed.keys.is_empty();
                pressed.keys.clear();
                changed
            } else {
                false
            }
        };
        self.dirty |= changed;
        if changed {
            self.input.wake.wake();
        }
    }
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    fn is_focusable(&self) -> bool {
        true
    }
    fn is_layout_dirty(&self) -> bool {
        false
    }
    fn needs_redraw(&self) -> bool {
        self.dirty
    }
    fn name(&self) -> &'static str {
        "KeyboardMap"
    }
    fn test_id(&self) -> Option<&str> {
        Some("input.keyboard_map")
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn accessibility(&self) -> Option<AccessNode> {
        Some(
            AccessNode::new(Role::Group)
                .with_state(AccessState {
                    focused: self.focused,
                    read_only: true,
                    ..Default::default()
                })
                .with_name(self.label.clone())
                .with_value(
                    self.keys
                        .iter()
                        .map(|key| format!("{}: {}", key.name, key.levels.join(" / ")))
                        .collect::<Vec<_>>()
                        .join("; "),
                ),
        )
    }
}
