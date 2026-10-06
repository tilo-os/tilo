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
    kurbo::{Affine, BezPath, Point, Rect, Stroke},
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

impl KeyboardMapKey {
    fn legend_rect(&self) -> Rect {
        let mut rect = self.outline.iter().fold(Rect::ZERO, |rect, point| {
            if rect.is_zero_area() {
                Rect::new(point.x, point.y, point.x + 0.01, point.y + 0.01)
            } else {
                rect.union_pt(*point)
            }
        });
        // Orthogonal physical models include concave Enter keys. Their
        // bounding box contains a neighbour's space; the common horizontal
        // core keeps every fitted legend inside the actual physical outline.
        let middle = (rect.x0 + rect.x1) / 2.0;
        for point in &self.outline {
            if point.x < middle {
                rect.x0 = rect.x0.max(point.x);
            } else if point.x > middle {
                rect.x1 = rect.x1.min(point.x);
            }
        }
        rect
    }
}

#[derive(Default)]
struct PressState {
    foreground: bool,
    keys: HashSet<PhysicalKey>,
    layout: Option<Arc<winit::keyboard::KeyboardLayout>>,
}

/// Private, process-local physical press state shared across page recomposition.
/// It contains no entered text or logical input history and never edits a document.
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
    /// Cache only native configuration while the page is absent. Off-page
    /// key presses are never retained; actual window blur revokes the cache.
    pub fn observe_configuration(&self, input: &crate::widget::PhysicalInput) {
        let changed = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match input {
                crate::widget::PhysicalInput::Layout(layout) => {
                    let changed = state.layout != *layout;
                    state.layout.clone_from(layout);
                    changed
                }
                crate::widget::PhysicalInput::Focused(false) => {
                    let changed = state.layout.is_some() || !state.keys.is_empty();
                    state.layout = None;
                    state.keys.clear();
                    state.foreground = false;
                    changed
                }
                _ => false,
            }
        };
        if changed {
            self.wake.wake();
        }
    }

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
    preview_level: usize,
    live_layout: bool,
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
            preview_level: 0,
            live_layout: false,
            rect: Rect::ZERO,
            focused: false,
            input: KeyboardMapInput::default(),
            pointer: None,
            dirty: true,
        }
    }

    /// Keep the owning page's private press state across retained recomposition.
    #[must_use]
    pub fn with_input(mut self, input: KeyboardMapInput) -> Self {
        self.input = input;
        self
    }

    /// Show the actual focused seat's effective symbols. Explicit layout
    /// previews leave this disabled so they never masquerade as the live seat.
    #[must_use]
    pub fn with_live_layout(mut self, enabled: bool) -> Self {
        self.live_layout = enabled;
        self
    }

    /// Show levels 5–8 when the selected XKB layout supplies them.
    /// Layouts with fewer levels retain their first bank.
    #[must_use]
    pub fn with_extra_levels(mut self, extra: bool) -> Self {
        self.extra_levels = extra && self.level_count() > 4;
        self
    }

    /// Inspect any ordered native level while preserving all levels in the
    /// shared tooltip/accessibility surface. Does not change the live seat.
    #[must_use]
    pub fn with_preview_level(mut self, level: usize) -> Self {
        self.preview_level = level.min(31);
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
                    let changed = !pressed.keys.is_empty() || pressed.layout.is_some();
                    pressed.keys.clear();
                    pressed.layout = None;
                    changed
                }
                crate::widget::PhysicalInput::Focused(true) => {
                    pressed.foreground = true;
                    false
                }
                crate::widget::PhysicalInput::Layout(layout) => {
                    let changed = pressed.layout != layout;
                    pressed.layout = layout;
                    changed
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

    /// Effective focused-seat symbols for this position, independent of the
    /// logical key event and preview layout. None means no native observation.
    pub fn active_symbols(
        &self,
        key: PhysicalKey,
    ) -> Option<Vec<winit::keyboard::KeyboardLayoutSymbol>> {
        self.input
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .layout
            .as_ref()?
            .keys
            .iter()
            .find(|entry| entry.physical == key)
            .map(|entry| entry.symbols.clone())
    }

    fn legend_values(&self, key: &KeyboardMapKey) -> Vec<String> {
        if self.live_layout {
            // Missing focused-seat evidence must never fall back to preferences.
            let symbols = self.active_symbols(key.physical).unwrap_or_default();
            let legend = symbols
                .into_iter()
                .map(|symbol| crate::keycaps::active().native_symbol(&symbol.name, &symbol.text))
                .collect::<Vec<_>>()
                .join(" ");
            vec![legend]
        } else {
            key.levels.clone()
        }
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
        let rect = key.legend_rect();
        let scale = self.rect.width() / self.model_size.width.max(1.0);
        // A screen-pixel outline remains visible independently of surface fill
        // contrast, including high contrast themes and narrow previews.
        scene.stroke(
            &Stroke::new(1.0 / scale),
            transform,
            colours.border_standard,
            None,
            &path,
        );
        let style = theme.text(if self.live_layout {
            TextRole::Body
        } else {
            TextRole::Caption
        });
        let ink = if pressed {
            colours.surfaces.content
        } else {
            colours.text_primary
        };
        // Preserve all native levels in metadata, but paint equivalent legends
        // once. Repeated function/control levels must not crowd the keycap.
        let values = self.legend_values(key);
        let mut legends: Vec<&str> = Vec::new();
        for legend in values
            .iter()
            .skip(if self.live_layout {
                0
            } else {
                self.preview_level
                    .max(if self.extra_levels { 4 } else { 0 })
            })
            .take(if self.live_layout { 1 } else { 4 })
        {
            if !legend.is_empty()
                && !legends.contains(&legend.as_str())
                && (legends.is_empty()
                    || self.rect.width() / self.model_size.width * key.legend_rect().width()
                        >= f64::from(theme.text(TextRole::Body).size) * 3.0)
            {
                legends.push(legend);
            }
        }
        let padding = f64::from(theme.tokens.spacing.s1) / scale;
        let inner = rect.inset(-padding.min(rect.width().min(rect.height()) / 8.0));
        for (index, legend) in legends.iter().enumerate() {
            let cell = if legends.len() == 1 {
                inner
            } else {
                let middle_x = (inner.x0 + inner.x1) / 2.0;
                let middle_y = (inner.y0 + inner.y1) / 2.0;
                Rect::new(
                    if index < 2 { inner.x0 } else { middle_x },
                    if index % 2 == 0 { middle_y } else { inner.y0 },
                    if index < 2 { middle_x } else { inner.x1 },
                    if index % 2 == 0 { inner.y1 } else { middle_y },
                )
            };
            let (width, height) = renderer.measure_style(legend, None, &style);
            let prominence = if index == 0 {
                1.0
            } else {
                f64::from(style.size / theme.text(TextRole::Body).size)
            };
            let fit = prominence
                .min(cell.width() * scale / f64::from(width.max(1.0)))
                .min(cell.height() * scale / f64::from(height.max(1.0)));
            let x = (cell.x0 + cell.x1 - f64::from(width) * fit / scale) / 2.0;
            let y = (cell.y0 + cell.y1 - f64::from(height) * fit / scale) / 2.0;
            renderer.draw_style(
                scene,
                legend,
                ink,
                transform * Affine::translate((x, y)) * Affine::scale(fit / scale),
                None,
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
    fn tooltip_target_at(&self, point: Point) -> Option<(Rect, String)> {
        let scale = self.rect.width() / self.model_size.width.max(1.0);
        let local = Point::new(
            (point.x - self.rect.x0) / scale,
            (point.y - self.rect.y0) / scale,
        );
        self.keys
            .iter()
            .find(|key| key.legend_rect().contains(local))
            .map(|key| {
                let rect = key.legend_rect();
                (
                    Rect::new(
                        self.rect.x0 + rect.x0 * scale,
                        self.rect.y0 + rect.y0 * scale,
                        self.rect.x0 + rect.x1 * scale,
                        self.rect.y0 + rect.y1 * scale,
                    ),
                    format!("{}: {}", key.name, self.legend_values(key).join(" / ")),
                )
            })
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
                        .map(|key| format!("{}: {}", key.name, self.legend_values(key).join(" / ")))
                        .collect::<Vec<_>>()
                        .join("; "),
                ),
        )
    }
}
