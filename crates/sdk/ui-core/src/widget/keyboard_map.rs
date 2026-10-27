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
use tilo_lock::MutexRecover as _;
use tilo_sort::CompactSort as _;
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
            let mut state = self.state.lock_recover();
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
            let mut state = self.state.lock_recover();
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
    single_level: bool,
    live_layout: bool,
    inspected_key: Option<String>,
    source: String,
    model: String,
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
            single_level: false,
            live_layout: false,
            inspected_key: None,
            source: String::new(),
            model: String::new(),
            rect: Rect::ZERO,
            focused: false,
            input: KeyboardMapInput::default(),
            pointer: None,
            dirty: true,
        }
    }

    /// Name the exact configuration compiled for these painted positions.
    #[must_use]
    pub fn with_configuration(mut self, source: String, model: String) -> Self {
        self.source = source;
        self.model = model;
        self
    }

    /// Read the mounted map's current paint state, never a keystroke history.
    /// Live symbols require actual foreground native configuration; preview
    /// legends remain explicitly identified as compiled preview data.
    pub fn inspection_snapshot(&self) -> Result<serde_json::Value, String> {
        let state = self.input.state.lock_recover();
        let mounted = self.rect.is_finite() && self.rect.width() > 0.0 && self.rect.height() > 0.0;
        let native = state.foreground.then_some(state.layout.as_ref()).flatten();
        let selected = self
            .inspected_key
            .as_ref()
            .and_then(|name| self.keys.iter().find(|key| &key.name == name));
        let native_key = selected.and_then(|key| {
            native?
                .keys
                .iter()
                .find(|entry| entry.physical == key.physical)
        });
        let available =
            mounted && selected.is_some() && (!self.live_layout || native_key.is_some());
        let symbols = selected
            .filter(|_| available)
            .map(|key| self.legend_values_at(key, &state))
            .unwrap_or_default();
        let mut pressed = self
            .keys
            .iter()
            .filter(|key| mounted && state.foreground && state.keys.contains(&key.physical))
            .map(|key| key.name.clone())
            .collect::<Vec<_>>();
        pressed.compact_sort();
        if self.keys.len() > 256
            || self.source.len() > 128
            || self.model.len() > 32
            || self
                .inspected_key
                .as_ref()
                .is_some_and(|name| name.len() > 32)
            || pressed.iter().any(|name| name.len() > 32)
            || pressed.len() > 256
            || symbols.len() > 32
            || symbols.iter().any(|symbol| symbol.len() > 1024)
        {
            return Err("The keyboard inspection exceeds its bound.".into());
        }
        Ok(serde_json::json!({
            "available": available,
            "page_visible": mounted,
            "foreground": state.foreground,
            "mode": if self.live_layout { "live" } else { "preview" },
            "source": (!self.live_layout).then_some(&self.source),
            "model": self.model,
            "inspected_key": self.inspected_key,
            "selected_symbols": symbols,
            "native_layout_available": native.is_some(),
            "native_group": native.map(|layout| layout.group),
            "native_modifiers": native.map(|layout| [layout.depressed, layout.latched, layout.locked]),
            "pressed_physical_keys": pressed,
        }))
    }

    /// Keep the owning page's private press state across retained recomposition.
    #[must_use]
    pub fn with_input(mut self, input: KeyboardMapInput) -> Self {
        self.input = input;
        self
    }

    /// Paint only the legend of `level` (base, Shift, AltGr, ...) on each
    /// key, at the size of a single keycap legend, instead of every level
    /// packed into the key's corners.
    #[must_use]
    pub fn with_single_level(mut self, level: usize) -> Self {
        self.preview_level = level;
        self.single_level = true;
        self
    }

    /// Whether the focused seat types what this map shows: `None` without
    /// focused-seat evidence, otherwise whether every letter key's base
    /// legend matches the seat's. A page that configures a layout uses it to
    /// say that the configuration has not reached the keyboard.
    pub fn live_agrees(&self) -> Option<bool> {
        let state = self.input.state.lock_recover();
        let layout = state.layout.as_ref()?;
        let caps = crate::keycaps::active();
        let mut compared = 0_usize;
        for key in &self.keys {
            let Some(shown) = key.levels.first().filter(|legend| {
                let mut chars = legend.chars();
                chars.next().is_some_and(char::is_alphabetic) && chars.next().is_none()
            }) else {
                continue;
            };
            let Some(entry) = layout
                .keys
                .iter()
                .find(|entry| entry.physical == key.physical)
            else {
                continue;
            };
            let Some(symbol) = entry.symbols.first() else {
                continue;
            };
            compared += 1;
            if caps.native_symbol(&symbol.name, &symbol.text) != *shown {
                return Some(false);
            }
        }
        (compared > 0).then_some(true)
    }

    /// Show the actual focused seat's effective symbols. Explicit layout
    /// previews leave this disabled so they never masquerade as the live seat.
    #[must_use]
    pub fn with_live_layout(mut self, enabled: bool) -> Self {
        self.live_layout = enabled;
        self
    }

    /// Select a physical position for a readable inspector without recording
    /// whichever key the person happens to press. Unknown positions are rejected.
    #[must_use]
    pub fn with_inspected_key(mut self, name: &str) -> Self {
        self.inspected_key = self
            .keys
            .iter()
            .find(|key| key.name == name)
            .map(|key| key.name.clone());
        self
    }

    /// Add pointer/touch selection through the same resource action as an
    /// inspector's shared selector. The visual map remains independently cloneable.
    pub fn selectable(self) -> KeyboardMapSelection {
        KeyboardMapSelection {
            map: self,
            action: None,
            enabled: true,
            pointer: None,
            pressed: None,
        }
    }

    fn key_at(&self, point: Point) -> Option<&KeyboardMapKey> {
        let scale = self.rect.width() / self.model_size.width.max(1.0);
        let local = Point::new(
            (point.x - self.rect.x0) / scale,
            (point.y - self.rect.y0) / scale,
        );
        self.keys
            .iter()
            .find(|key| key.legend_rect().contains(local))
    }

    /// Stable physical positions and their compiled first-level legends, for a
    /// shared searchable selector. These are configuration, never entered text.
    pub fn inspection_keys(&self) -> Vec<(String, String)> {
        self.keys
            .iter()
            .map(|key| {
                (
                    key.name.clone(),
                    format!(
                        "{}: {}",
                        key.name,
                        key.levels.first().map_or("", String::as_str)
                    ),
                )
            })
            .collect()
    }

    /// Full-size inspection values for an explicitly chosen physical position.
    /// Live mode reads the same immutable native snapshot as paint; unavailable
    /// native state never falls back to preview or preference symbols.
    pub fn inspection_values(&self, name: &str) -> Option<Vec<String>> {
        self.keys
            .iter()
            .find(|key| key.name == name)
            .map(|key| self.legend_values(key))
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
        self.input.state.lock_recover().foreground = !matches!(event, WindowEvent::Focused(false));
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
            let mut pressed = self.input.state.lock_recover();
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
        self.input.state.lock_recover().keys.contains(&key)
    }

    /// Effective focused-seat symbols for this position, independent of the
    /// logical key event and preview layout. None means no native observation.
    pub fn active_symbols(
        &self,
        key: PhysicalKey,
    ) -> Option<Vec<winit::keyboard::KeyboardLayoutSymbol>> {
        self.input
            .state
            .lock_recover()
            .layout
            .as_ref()?
            .keys
            .iter()
            .find(|entry| entry.physical == key)
            .map(|entry| entry.symbols.clone())
    }

    fn legend_values(&self, key: &KeyboardMapKey) -> Vec<String> {
        let state = self.input.state.lock_recover();
        self.legend_values_at(key, &state)
    }

    fn legend_values_at(&self, key: &KeyboardMapKey, state: &PressState) -> Vec<String> {
        if self.live_layout {
            // Missing focused-seat evidence must never fall back to preferences.
            let symbols = state
                .layout
                .as_ref()
                .and_then(|layout| {
                    layout
                        .keys
                        .iter()
                        .find(|entry| entry.physical == key.physical)
                })
                .map(|entry| entry.symbols.clone())
                .unwrap_or_default();
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
            colours.intent.value_fill
        } else {
            colours.surfaces.raised
        };
        scene.fill(Fill::NonZero, transform, fill, None, &path);
        let rect = key.legend_rect();
        let scale = self.rect.width() / self.model_size.width.max(1.0);
        // A screen-pixel outline remains visible independently of surface fill
        // contrast, including high contrast themes and narrow previews.
        scene.stroke(
            &Stroke::new(
                if self.inspected_key.as_deref() == Some(key.name.as_str()) {
                    2.0 / scale
                } else {
                    1.0 / scale
                },
            ),
            transform,
            if self.inspected_key.as_deref() == Some(key.name.as_str()) {
                colours.border_focus
            } else {
                crate::surface::edge_on(fill)
            },
            None,
            &path,
        );
        let style = theme.text(if self.live_layout || self.single_level {
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
        let (skip, take) = if self.live_layout {
            (0, 1)
        } else if self.single_level {
            (self.preview_level, 1)
        } else {
            (
                self.preview_level
                    .max(if self.extra_levels { 4 } else { 0 }),
                4,
            )
        };
        for legend in values.iter().skip(skip).take(take) {
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
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::auto(),
            },
            aspect_ratio: Some(crate::num::f32_of_f64(
                self.model_size.width / self.model_size.height.max(1.0),
            )),
            ..Style::default()
        };
        taffy.widget_leaf(style, nodes, self)
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
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
            self.pointer = Some(ctx.logical(position));
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
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn set_focus(&mut self, focused: bool) {
        self.dirty |= self.focused != focused;
        self.focused = focused;
        let changed = {
            let mut pressed = self.input.state.lock_recover();
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
    crate::redraw_flag!();
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
                .with_state(AccessState::new().focused(self.focused).read_only(true))
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

/// Action-bound inspection over the shared physical map. Keyboard presses
/// never select an inspector or publish entered text.
pub struct KeyboardMapSelection {
    map: KeyboardMap,
    action: Option<crate::widget::ActionEmitter<String>>,
    enabled: bool,
    pointer: Option<Point>,
    pressed: Option<String>,
}

impl KeyboardMapSelection {
    /// Bind inspection to the host-admitted Settings resource effect.
    #[must_use]
    pub fn on_action(mut self, binding: tilo_actions::ActionBinding<String>) -> Self {
        self.action = Some(crate::widget::ActionEmitter::new(binding));
        self
    }

    /// Disable local inspection without disabling private key feedback.
    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    fn key_target(name: &str) -> String {
        format!("input.keyboard_map.key.{name}")
    }

    fn inspection_rect(&self, key: &KeyboardMapKey) -> Rect {
        let scale = self.map.rect.width() / self.map.model_size.width.max(1.0);
        let rect = key.legend_rect();
        Rect::new(
            self.map.rect.x0 + rect.x0 * scale,
            self.map.rect.y0 + rect.y0 * scale,
            self.map.rect.x0 + rect.x1 * scale,
            self.map.rect.y0 + rect.y1 * scale,
        )
    }

    fn inspection_enabled(&self) -> bool {
        self.enabled
            && self
                .action
                .as_ref()
                .is_some_and(|action| action.is_available())
    }

    fn press(&mut self, point: Point) {
        self.pressed = self.map.key_at(point).map(|key| key.name.clone());
    }

    fn release(&mut self, point: Point) -> bool {
        let Some(name) = self.pressed.take() else {
            return false;
        };
        if self.map.key_at(point).is_none_or(|key| key.name != name) {
            return false;
        }
        self.action
            .as_mut()
            .is_some_and(|action| action.emit(name).is_ok())
    }
}

impl crate::widget::Decorator for KeyboardMapSelection {
    type Inner = KeyboardMap;
    fn inner(&self) -> &KeyboardMap {
        &self.map
    }
    fn inner_mut(&mut self) -> &mut KeyboardMap {
        &mut self.map
    }
    crate::provides!(dyn crate::widget::controls::ActionControls);

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(&self.map)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(&mut self.map)
    }

    fn accessibility(&self) -> Option<AccessNode> {
        let mut root = self.map.accessibility()?;
        // The map is a group; each painted physical position is the actual
        // inspection control. These expose the same admitted effect as touch.
        root.children = self
            .map
            .keys
            .iter()
            .map(|key| {
                AccessNode::new(Role::Button)
                    .with_name(format!(
                        "{}: {}",
                        key.name,
                        self.map.legend_values(key).join(" / ")
                    ))
                    .with_action_target(Self::key_target(&key.name))
                    .with_bounds(self.inspection_rect(key))
                    .with_state(
                        AccessState::new()
                            .selected(self.map.inspected_key.as_ref() == Some(&key.name))
                            .disabled(!self.inspection_enabled()),
                    )
            })
            .collect();
        Some(root)
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        match path {
            [index] => self
                .map
                .keys
                .get(*index)
                .map(|key| self.inspection_rect(key)),
            _ => None,
        }
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        _state: &mut crate::widget::Edits<'_>,
        _path: &crate::widget::ModelPath,
    ) -> bool {
        if !self.inspection_enabled() || !matches!(action, crate::a11y::Action::Activate) {
            return false;
        }
        let Some(key) = self
            .map
            .keys
            .iter()
            .find(|key| Self::key_target(&key.name) == target)
        else {
            return false;
        };
        self.action
            .as_mut()
            .is_some_and(|action| action.emit(key.name.clone()).is_ok())
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        self.map.handle_event(event, ctx);
        if !self.enabled
            || self
                .action
                .as_ref()
                .is_none_or(|action| !action.is_available())
        {
            self.pressed = None;
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = Some(ctx.logical(position));
            }
            WindowEvent::MouseInput {
                state,
                button: winit::event::MouseButton::Left,
                ..
            } => {
                if let Some(point) = self.pointer {
                    if *state == ElementState::Pressed {
                        self.press(point);
                    } else if self.release(point) {
                        *ctx.handled = true;
                    }
                }
            }
            WindowEvent::Touch(touch) => {
                let point = ctx.logical(touch.location);
                match touch.phase {
                    winit::event::TouchPhase::Started => self.press(point),
                    winit::event::TouchPhase::Ended => {
                        if self.release(point) {
                            *ctx.handled = true;
                        }
                    }
                    winit::event::TouchPhase::Cancelled => self.pressed = None,
                    winit::event::TouchPhase::Moved => {}
                }
            }
            WindowEvent::Focused(false) => self.pressed = None,
            _ => {}
        }
    }
}

impl crate::widget::controls::ActionControls for KeyboardMapSelection {
    crate::widget::controls::optional_emitter_controls!();
    fn control_action_bindings(&self) -> Vec<crate::widget::ControlBindingMetadata> {
        self.action
            .as_ref()
            .map(|action| {
                self.map
                    .keys
                    .iter()
                    .map(|key| action.metadata(Self::key_target(&key.name)))
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod selection_tests {
    use super::*;
    use crate::widget::controls::ActionControls as _;

    #[test]
    fn touch_and_pointer_inspection_emit_the_same_admitted_physical_key_action() {
        let key = KeyboardMapKey {
            name: "AD06".into(),
            physical: PhysicalKey::Code(winit::keyboard::KeyCode::KeyY),
            outline: vec![
                Point::new(0.0, 0.0),
                Point::new(20.0, 0.0),
                Point::new(20.0, 20.0),
                Point::new(0.0, 20.0),
            ],
            levels: vec!["y".into(), "Y".into()],
        };
        let mut map = KeyboardMap::new(
            vec![key],
            Size {
                width: 400.0,
                height: 100.0,
            },
            String::new(),
        )
        .selectable()
        .on_action(crate::widget::test_string_action_binding());
        map.map.rect = Rect::new(0.0, 0.0, 600.0, 150.0);
        let mut doc = automerge::AutoCommit::new();
        let mut taffy = TaffyTree::<()>::new();
        let node = taffy.leaf_node(Style::default());
        let modifiers = winit::event::Modifiers::default();
        let mut handled = false;
        let mut focus = None;
        let mut ctx = EventCtx {
            scale_factor: 1.0,
            modifiers: &modifiers,
            state: crate::widget::Edits::lend(&mut doc),
            path: &automerge::ROOT,
            handled: &mut handled,
            focus_target: &mut focus,
            node_id: node,
            taffy: &taffy,
        };
        for phase in [
            winit::event::TouchPhase::Started,
            winit::event::TouchPhase::Ended,
        ] {
            map.handle_event(
                &WindowEvent::Touch(winit::event::Touch {
                    device_id: winit::event::DeviceId::dummy(),
                    phase,
                    location: winit::dpi::PhysicalPosition::new(15.0, 15.0),
                    force: None,
                    id: 1,
                }),
                &mut ctx,
            );
        }
        let touch = map.take_control_invocation();
        assert!(
            touch.is_some(),
            "the actual shared touch handler must emit inspection"
        );
        map.handle_event(&crate::synthetic_input::cursor_moved(15.0, 15.0), &mut ctx);
        for state in [ElementState::Pressed, ElementState::Released] {
            map.handle_event(
                &crate::synthetic_input::mouse_input(state, winit::event::MouseButton::Left),
                &mut ctx,
            );
        }
        let pointer = map.take_control_invocation();
        assert_eq!(touch, pointer);
        assert_eq!(
            pointer
                .as_ref()
                .and_then(|intent| intent.arguments.get("value"))
                .and_then(serde_json::Value::as_str),
            Some("AD06")
        );
        let semantics = map.accessibility().unwrap();
        assert_eq!(semantics.role, Role::Group);
        assert_eq!(semantics.children.len(), 1);
        let control = &semantics.children[0];
        assert_eq!(control.role, Role::Button);
        assert!(!control.state.selected);
        let target = control.action_target.as_deref().unwrap();
        assert_eq!(map.control_action_bindings()[0].control_id, target);
        assert!(map.perform_semantic_action(
            target,
            &crate::a11y::Action::Activate,
            &mut ctx.state,
            &automerge::ROOT
        ));
        assert_eq!(map.take_control_invocation(), pointer);
        map.map.inspected_key = Some("AD06".into());
        assert!(map.accessibility().unwrap().children[0].state.selected);
        assert!(!map.perform_semantic_action(
            "input.keyboard_map.key.unknown",
            &crate::a11y::Action::Activate,
            &mut ctx.state,
            &automerge::ROOT
        ));
        map.refresh_control_action_availability(&HashSet::new());
        assert!(map.accessibility().unwrap().children[0].state.disabled);
        assert!(!map.perform_semantic_action(
            target,
            &crate::a11y::Action::Activate,
            &mut ctx.state,
            &automerge::ROOT
        ));
        for phase in [
            winit::event::TouchPhase::Started,
            winit::event::TouchPhase::Ended,
        ] {
            map.handle_event(
                &WindowEvent::Touch(winit::event::Touch {
                    device_id: winit::event::DeviceId::dummy(),
                    phase,
                    location: winit::dpi::PhysicalPosition::new(15.0, 15.0),
                    force: None,
                    id: 1,
                }),
                &mut ctx,
            );
        }
        assert!(
            map.take_control_invocation().is_none(),
            "revoked action cannot inspect"
        );
    }
}
