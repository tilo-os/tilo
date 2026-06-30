//! [`OskWidget`] — the on-screen keyboard widget.
//!
//! Self-contained: it draws its own toolbar + keys (geometry from the pure
//! [`crate::layout::key_rects`] / [`crate::toolbar::toolbar_rects`] passes)
//! and hit-tests pointer taps itself, rather than composing dozens of
//! `Button`/`Row` widgets — the key set changes with the active layer and
//! shift state, so a custom draw + hit-test keeps the geometry honest and
//! the taffy tree a single node.
//!
//! ## Output channels (two ways, mirroring `FilePicker`)
//! Compositor text injection (W7, `input-method-v2`) is **not** wired here.
//! Until it is, what the user "types" is observable two ways:
//! * [`OskWidget::on_key`] — an imperative callback fired per activation.
//! * [`OskWidget::pending_text`] / [`OskWidget::take_pending_text`] — a
//!   buffer of the resulting text (peek + drain, like
//!   `FilePicker::outcome` / `FilePicker::take_outcome`).
//!
//! ## Seams for later workstreams
//! * **W7 (IM injection):** a host will drain [`OskWidget::take_pending_text`]
//!   (and observe [`OskWidget::on_key`]) and forward commit-strings through a
//!   `zwp_input_method_v2` client to the focused surface. Nothing in this
//!   widget assumes a particular sink.
//! * **W9 (FUTO swipe):** the same [`crate::layout::key_rects`] geometry is
//!   the source of truth a swipe decoder normalises into 0..1 space; a future
//!   `on_swipe(points)` entry point feeds candidates into the toolbar's
//!   candidate ribbon. No swipe code lives here yet.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use ui::icons::{self, Icon};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

use crate::layout::{Key, KeyAction, KeyId, KeyboardLayout, ShiftState, key_rects};
use crate::toolbar::{OskToolbarAction, OskToolbarItem, toolbar_rects};

/// Default target height of a single key row, in logical px. Responsive
/// sizing keeps the *width* filling the surface and uses this for height —
/// the same code serves a phone (small surface) and a desktop (wide
/// surface); only the surface size differs. ≥ 48 px honours the design's
/// minimum touch target.
pub const DEFAULT_KEY_HEIGHT: f32 = 56.0;
/// Default toolbar row height.
pub const DEFAULT_TOOLBAR_HEIGHT: f32 = 48.0;

/// The on-screen keyboard widget.
pub struct OskWidget {
    /// Registered layouts by name. Always contains the symbols layer plus
    /// at least one letter layout. Adding AZERTY/QWERTZ/… is just inserting
    /// data here (see [`OskWidget::with_layout`]).
    layouts: HashMap<String, KeyboardLayout>,
    /// Name of the active *letter* layout (e.g. `"qwerty"`).
    active_letter: String,
    /// Whether the symbols layer is currently displayed instead of letters.
    showing_symbols: bool,
    /// Current shift / caps state.
    shift: ShiftState,
    /// Toolbar descriptors (data-driven, replaceable).
    toolbar: Vec<OskToolbarItem>,
    /// Configurable target row height (responsive sizing).
    target_key_height: f32,
    /// Toolbar row height.
    toolbar_height: f32,

    // --- frame caches (populated in apply_layout) ---
    rect: Rect,
    key_geom: Vec<(KeyId, Rect)>,
    toolbar_geom: Vec<(usize, Rect)>,

    // --- interaction state ---
    hovered_key: Option<KeyId>,
    pressed_key: Option<KeyId>,
    hovered_tool: Option<usize>,
    pressed_tool: Option<usize>,

    // --- output ---
    pending_text: String,
    key_log: Vec<KeyAction>,
    toolbar_log: Vec<OskToolbarAction>,
    on_key: Option<Box<dyn FnMut(KeyAction)>>,
    on_toolbar: Option<Box<dyn FnMut(OskToolbarAction)>>,

    // --- dirty tracking ---
    /// Set when the displayed layer/layout changes so the host rebuilds the
    /// taffy node (its intrinsic height tracks the row count). Cleared by
    /// `take_structure_dirty`.
    structure_dirty: bool,
}

impl Default for OskWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl OskWidget {
    /// A keyboard pre-loaded with QWERTY + AZERTY letter layouts and the
    /// symbols layer, the default toolbar, and default sizing. QWERTY is
    /// active.
    pub fn new() -> Self {
        let mut layouts = HashMap::new();
        for l in [
            KeyboardLayout::qwerty(),
            KeyboardLayout::azerty(),
            KeyboardLayout::symbols(),
        ] {
            layouts.insert(l.name.clone(), l);
        }
        Self {
            layouts,
            active_letter: "qwerty".to_string(),
            showing_symbols: false,
            shift: ShiftState::Off,
            toolbar: OskToolbarItem::default_set(),
            target_key_height: DEFAULT_KEY_HEIGHT,
            toolbar_height: DEFAULT_TOOLBAR_HEIGHT,
            rect: Rect::ZERO,
            key_geom: Vec::new(),
            toolbar_geom: Vec::new(),
            hovered_key: None,
            pressed_key: None,
            hovered_tool: None,
            pressed_tool: None,
            pending_text: String::new(),
            key_log: Vec::new(),
            toolbar_log: Vec::new(),
            on_key: None,
            on_toolbar: None,
            structure_dirty: false,
        }
    }

    // --- builders -------------------------------------------------------

    /// Register an additional layout (e.g. a community AZERTY/Dvorak/emoji
    /// file parsed into a [`KeyboardLayout`]). Letter layouts become
    /// selectable via [`KeyAction::Layout`].
    pub fn with_layout(mut self, layout: KeyboardLayout) -> Self {
        self.layouts.insert(layout.name.clone(), layout);
        self
    }

    /// Choose the active letter layout by name (must be registered).
    pub fn with_active_layout(mut self, name: &str) -> Self {
        if self.layouts.contains_key(name) {
            self.active_letter = name.to_string();
        }
        self
    }

    /// Replace the toolbar with a custom data-driven set.
    pub fn with_toolbar(mut self, items: Vec<OskToolbarItem>) -> Self {
        self.toolbar = items;
        self
    }

    /// Set the responsive target key-row height (logical px).
    pub fn with_target_key_height(mut self, h: f32) -> Self {
        self.target_key_height = h.max(1.0);
        self
    }

    /// Imperative per-activation callback. Receives the *emitted* action
    /// (already shift-cased for letters).
    pub fn on_key<F: FnMut(KeyAction) + 'static>(mut self, f: F) -> Self {
        self.on_key = Some(Box::new(f));
        self
    }

    /// Imperative toolbar-tap callback.
    pub fn on_toolbar<F: FnMut(OskToolbarAction) + 'static>(mut self, f: F) -> Self {
        self.on_toolbar = Some(Box::new(f));
        self
    }

    // --- output accessors ----------------------------------------------

    /// Peek at the accumulated typed text (mirror of `FilePicker::outcome`).
    pub fn pending_text(&self) -> &str {
        &self.pending_text
    }

    /// Drain the accumulated typed text (mirror of
    /// `FilePicker::take_outcome`). The host (W7) calls this and injects the
    /// returned string into the focused surface.
    pub fn take_pending_text(&mut self) -> String {
        std::mem::take(&mut self.pending_text)
    }

    /// Drain the log of emitted key actions (for hosts/tests that want the
    /// raw [`KeyAction`] stream rather than the flattened text).
    pub fn take_key_actions(&mut self) -> Vec<KeyAction> {
        std::mem::take(&mut self.key_log)
    }

    /// Drain the log of toolbar actions.
    pub fn take_toolbar_actions(&mut self) -> Vec<OskToolbarAction> {
        std::mem::take(&mut self.toolbar_log)
    }

    // --- state accessors -----------------------------------------------

    pub fn shift_state(&self) -> ShiftState {
        self.shift
    }

    pub fn showing_symbols(&self) -> bool {
        self.showing_symbols
    }

    /// The name of the currently *displayed* layout (`"symbols"` when the
    /// symbol layer is up, otherwise the active letter layout).
    pub fn active_layout_name(&self) -> &str {
        if self.showing_symbols {
            "symbols"
        } else {
            &self.active_letter
        }
    }

    /// Frame key geometry (id → rect), for hosts/tests/swipe decoding.
    pub fn key_geometry(&self) -> &[(KeyId, Rect)] {
        &self.key_geom
    }

    /// Frame toolbar geometry (index → rect).
    pub fn toolbar_geometry(&self) -> &[(usize, Rect)] {
        &self.toolbar_geom
    }

    // --- programmatic activation (test/host helpers) -------------------

    /// Activate a key by id as if tapped. No-op for an out-of-range id.
    pub fn activate_key(&mut self, id: KeyId) {
        let action = match self.current_layout().key(id) {
            Some(k) => k.emit_action(self.shift),
            None => return,
        };
        self.apply_action(action);
    }

    /// Activate the first key whose *base* label matches `label`
    /// (e.g. `"a"`, `"⇧"`, `"?123"`), returning whether one was found.
    pub fn activate_label(&mut self, label: &str) -> bool {
        let id = {
            let layout = self.current_layout();
            let mut found = None;
            'outer: for (r, row) in layout.rows.iter().enumerate() {
                for (c, key) in row.iter().enumerate() {
                    if key.label == label {
                        found = Some(KeyId::new(r, c));
                        break 'outer;
                    }
                }
            }
            found
        };
        match id {
            Some(id) => {
                self.activate_key(id);
                true
            }
            None => false,
        }
    }

    /// Activate a toolbar item by index as if tapped.
    pub fn activate_tool(&mut self, index: usize) {
        let action = match self.toolbar.get(index) {
            Some(it) => it.action.clone(),
            None => return,
        };
        self.toolbar_log.push(action.clone());
        if let Some(cb) = &mut self.on_toolbar {
            cb(action);
        }
    }

    // --- internals ------------------------------------------------------

    /// The layout currently on screen.
    fn current_layout(&self) -> &KeyboardLayout {
        let name = self.active_layout_name();
        self.layouts
            .get(name)
            // Symbols/active layout are always registered, but fall back to
            // the active letter layout defensively rather than panic.
            .or_else(|| self.layouts.get(&self.active_letter))
            .expect("active layout registered")
    }

    /// Apply an emitted [`KeyAction`]: update text/state, log it, fire the
    /// callback.
    fn apply_action(&mut self, action: KeyAction) {
        match &action {
            KeyAction::Char(s) => {
                self.pending_text.push_str(s);
                // One-shot shift resets after a character; caps is sticky.
                if self.shift == ShiftState::Shift {
                    self.shift = ShiftState::Off;
                }
            }
            KeyAction::Space => self.pending_text.push(' '),
            KeyAction::Enter => self.pending_text.push('\n'),
            KeyAction::Backspace => {
                self.pending_text.pop();
            }
            KeyAction::Shift => {
                self.shift = self.shift.cycle();
            }
            KeyAction::SymbolsToggle => {
                self.showing_symbols = !self.showing_symbols;
                self.structure_dirty = true;
            }
            KeyAction::Layout(name) => {
                if self.layouts.contains_key(name) {
                    self.active_letter = name.clone();
                    self.showing_symbols = false;
                    self.structure_dirty = true;
                }
            }
        }
        self.key_log.push(action.clone());
        if let Some(cb) = &mut self.on_key {
            cb(action);
        }
    }

    fn key_at(&self, p: Point) -> Option<KeyId> {
        self.key_geom
            .iter()
            .find(|(_, r)| r.contains(p))
            .map(|(id, _)| *id)
    }

    fn tool_at(&self, p: Point) -> Option<usize> {
        self.toolbar_geom
            .iter()
            .find(|(_, r)| r.contains(p))
            .map(|(i, _)| *i)
    }
}

/// Draw a rounded "button" cell (used for both keys and toolbar items).
#[allow(clippy::too_many_arguments)]
fn draw_cell(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    icon: Option<Icon>,
    label: &str,
    hovered: bool,
    pressed: bool,
    accent: bool,
    font_scale: f64,
) {
    let t = &theme.tokens;
    // Inset so adjacent cells have a visible gutter.
    let pad = (rect.height() * 0.06).clamp(2.0, 6.0);
    let cell = Rect::new(
        rect.x0 + pad,
        rect.y0 + pad,
        rect.x1 - pad,
        rect.y1 - pad,
    );
    let bg = if pressed {
        theme.bg_active
    } else if hovered {
        theme.bg_hover
    } else if accent {
        t.colors.surfaces.overlay
    } else {
        t.colors.surfaces.raised
    };
    let shape = RoundedRect::from_rect(cell, t.radii.md as f64);
    scene.fill(vello::peniko::Fill::NonZero, transform, bg, None, &shape);
    let border = if accent {
        theme.border_focus
    } else {
        t.colors.border_standard
    };
    scene.stroke(
        &vello::kurbo::Stroke::new(1.0),
        transform,
        border,
        None,
        &shape,
    );

    let fg = if accent {
        theme.accent_primary
    } else {
        t.colors.text_primary
    };

    if let Some(icon) = icon {
        let s = (cell.height() * 0.5).clamp(14.0, 26.0);
        let ix = cell.center().x - s / 2.0;
        let iy = cell.center().y - s / 2.0;
        icons::draw_icon(
            scene,
            transform,
            Rect::new(ix, iy, ix + s, iy + s),
            icon,
            fg,
            icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        return;
    }

    if label.is_empty() {
        return;
    }
    let font = (cell.height() * font_scale).clamp(11.0, 30.0) as f32;
    let (tw, _th) = text_renderer.measure(label, font, None);
    let tx = cell.center().x - tw as f64 / 2.0;
    let ty = cell.y0 + (cell.height() - font as f64) / 2.0;
    text_renderer.draw_text(
        scene,
        label,
        font,
        fg,
        transform * Affine::translate((tx, ty)),
        None,
    );
}

impl Widget for OskWidget {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let rows = self.current_layout().row_count();
        let total_h = self.toolbar_height + rows as f32 * self.target_key_height;
        let node = taffy
            .new_leaf(Style {
                flex_shrink: 0.0,
                size: Size {
                    // Fill the available width (phone → desktop); intrinsic
                    // height from the row count × target key height.
                    width: Dimension::percent(1.0),
                    height: Dimension::length(total_h),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );

        // Toolbar occupies the top band; keys fill the rest.
        let toolbar_h = (self.toolbar_height as f64).min(self.rect.height());
        let toolbar_area = Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x1,
            self.rect.y0 + toolbar_h,
        );
        let key_area = Rect::new(
            self.rect.x0,
            self.rect.y0 + toolbar_h,
            self.rect.x1,
            self.rect.y1,
        );

        self.toolbar_geom = toolbar_rects(&self.toolbar, toolbar_area);
        let geom = {
            let layout = self.current_layout();
            key_rects(layout, key_area, self.shift)
        };
        self.key_geom = geom;
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        // Surface background.
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &self.rect,
        );

        // Toolbar.
        for (i, r) in &self.toolbar_geom {
            let item = &self.toolbar[*i];
            let hovered = self.hovered_tool == Some(*i);
            let pressed = self.pressed_tool == Some(*i);
            draw_cell(
                scene,
                transform,
                text_renderer,
                theme,
                *r,
                item.icon,
                &item.label,
                hovered,
                pressed,
                false,
                0.34,
            );
        }

        // Keys.
        let shift = self.shift;
        let layout = self.current_layout();
        for (id, rect) in &self.key_geom {
            let Some(key) = layout.key(*id) else { continue };
            let label = key.display_label(shift);
            let is_action = !matches!(&key.action, KeyAction::Char(_));
            let shift_active = matches!(&key.action, KeyAction::Shift) && shift.is_upper();
            let hovered = self.hovered_key == Some(*id);
            let pressed = self.pressed_key == Some(*id);
            // Letters get a larger glyph; multi-char action labels smaller.
            let font_scale = if Key::is_letter(key) { 0.42 } else { 0.30 };
            draw_cell(
                scene,
                transform,
                text_renderer,
                theme,
                *rect,
                None,
                &label,
                hovered,
                pressed,
                is_action || shift_active,
                font_scale,
            );
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                let k = self.key_at(p);
                let t = if k.is_some() { None } else { self.tool_at(p) };
                if k != self.hovered_key || t != self.hovered_tool {
                    self.hovered_key = k;
                    self.hovered_tool = t;
                    *handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                // Mirror `Button`: press latches onto whatever the last
                // CursorMoved hovered.
                if let Some(id) = self.hovered_key {
                    self.pressed_key = Some(id);
                    *handled = true;
                } else if let Some(i) = self.hovered_tool {
                    self.pressed_tool = Some(i);
                    *handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(id) = self.pressed_key.take() {
                    // Only fire if released over the same key.
                    if self.hovered_key == Some(id) {
                        self.activate_key(id);
                    }
                    *handled = true;
                } else if let Some(i) = self.pressed_tool.take() {
                    if self.hovered_tool == Some(i) {
                        self.activate_tool(i);
                    }
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "OnScreenKeyboard"
    }

    fn test_id(&self) -> Option<&str> {
        Some("on_screen_keyboard")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// A layer/layout switch changes the displayed row set (and, for custom
    /// layouts, the row count → the intrinsic height), so the host must
    /// rebuild the taffy node. Rising-edge: reading clears the flag.
    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.structure_dirty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn lay_out(osk: &mut OskWidget, w: f32, h: f32) {
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let child = osk.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(w),
                        height: Dimension::length(h),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();
        taffy
            .compute_layout(
                root,
                Size {
                    width: taffy::AvailableSpace::Definite(w),
                    height: taffy::AvailableSpace::Definite(h),
                },
            )
            .unwrap();
        osk.apply_layout(&taffy, child, Point::ORIGIN);
    }

    #[test]
    fn tapping_letter_appends_to_pending_text() {
        let mut osk = OskWidget::new();
        lay_out(&mut osk, 800.0, 360.0);
        assert!(osk.activate_label("h"));
        assert!(osk.activate_label("i"));
        assert_eq!(osk.pending_text(), "hi");
    }

    #[test]
    fn tapping_letter_emits_char_action_and_fires_callback() {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let mut osk = {
            let seen = seen.clone();
            OskWidget::new().on_key(move |a| seen.borrow_mut().push(a))
        };
        lay_out(&mut osk, 800.0, 360.0);
        osk.activate_label("a");
        let log = osk.take_key_actions();
        assert_eq!(log, vec![KeyAction::Char("a".into())]);
        assert_eq!(*seen.borrow(), vec![KeyAction::Char("a".into())]);
    }

    #[test]
    fn shift_uppercases_next_letter_then_resets() {
        let mut osk = OskWidget::new();
        lay_out(&mut osk, 800.0, 360.0);
        assert_eq!(osk.shift_state(), ShiftState::Off);
        osk.activate_label("⇧");
        assert_eq!(osk.shift_state(), ShiftState::Shift);
        osk.activate_label("a"); // -> "A", one-shot shift resets
        osk.activate_label("b"); // -> "b"
        assert_eq!(osk.pending_text(), "Ab");
        assert_eq!(osk.shift_state(), ShiftState::Off);
    }

    #[test]
    fn caps_lock_is_sticky() {
        let mut osk = OskWidget::new();
        lay_out(&mut osk, 800.0, 360.0);
        osk.activate_label("⇧"); // Off -> Shift
        osk.activate_label("⇧"); // Shift -> CapsLock
        assert_eq!(osk.shift_state(), ShiftState::CapsLock);
        osk.activate_label("a");
        osk.activate_label("b");
        assert_eq!(osk.pending_text(), "AB");
        assert_eq!(osk.shift_state(), ShiftState::CapsLock, "caps stays on");
    }

    #[test]
    fn symbols_toggle_switches_layer() {
        let mut osk = OskWidget::new();
        lay_out(&mut osk, 800.0, 360.0);
        assert_eq!(osk.active_layout_name(), "qwerty");
        assert!(!osk.showing_symbols());
        osk.activate_label("?123");
        assert!(osk.showing_symbols());
        assert_eq!(osk.active_layout_name(), "symbols");
        // Re-layout (host would after structure_dirty) and type a digit.
        lay_out(&mut osk, 800.0, 360.0);
        assert!(osk.activate_label("1"));
        assert_eq!(osk.pending_text(), "1");
        // The structure-dirty flag was raised by the toggle.
    }

    #[test]
    fn toggle_raises_structure_dirty() {
        let mut osk = OskWidget::new();
        lay_out(&mut osk, 800.0, 360.0);
        osk.activate_label("?123");
        assert!(osk.take_structure_dirty());
        assert!(!osk.take_structure_dirty(), "flag is rising-edge");
    }

    #[test]
    fn backspace_removes_last_char() {
        let mut osk = OskWidget::new();
        lay_out(&mut osk, 800.0, 360.0);
        osk.activate_label("h");
        osk.activate_label("i");
        osk.activate_label("⌫");
        assert_eq!(osk.pending_text(), "h");
    }

    #[test]
    fn space_and_take_pending_text_drains() {
        let mut osk = OskWidget::new();
        lay_out(&mut osk, 800.0, 360.0);
        osk.activate_label("h");
        osk.activate_label("i");
        osk.activate_label("space");
        assert_eq!(osk.take_pending_text(), "hi ");
        assert_eq!(osk.pending_text(), "", "drained");
    }

    #[test]
    fn pointer_tap_routes_to_key_under_cursor() {
        use winit::event::{DeviceId, Modifiers};
        let mut osk = OskWidget::new();
        lay_out(&mut osk, 800.0, 360.0);

        // Pick the rect of the 'q' key (row 0, col 0) and aim at its centre.
        let target = KeyId::new(0, 0);
        let rect = osk
            .key_geometry()
            .iter()
            .find(|(id, _)| *id == target)
            .map(|(_, r)| *r)
            .expect("q key laid out");
        let c = rect.center();

        let device: DeviceId = unsafe { std::mem::zeroed() };
        let mods = Modifiers::default();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let mut dispatch = |osk: &mut OskWidget, ev: WindowEvent| {
            let mut handled = false;
            let mut focus = None;
            osk.handle_event(
                &ev,
                1.0,
                &mods,
                &mut doc,
                &root,
                &mut handled,
                &mut focus,
                NodeId::new(0),
                &TaffyTree::new(),
            );
        };

        dispatch(
            &mut osk,
            WindowEvent::CursorMoved {
                device_id: device,
                position: (c.x, c.y).into(),
            },
        );
        dispatch(
            &mut osk,
            WindowEvent::MouseInput {
                device_id: device,
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
        );
        dispatch(
            &mut osk,
            WindowEvent::MouseInput {
                device_id: device,
                state: ElementState::Released,
                button: MouseButton::Left,
            },
        );

        assert_eq!(osk.pending_text(), "q", "tap over 'q' typed q");
    }

    #[test]
    fn toolbar_tap_emits_action() {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let mut osk = {
            let seen = seen.clone();
            OskWidget::new().on_toolbar(move |a| seen.borrow_mut().push(a))
        };
        lay_out(&mut osk, 800.0, 360.0);
        osk.activate_tool(0); // default[0] == Paste
        assert_eq!(osk.take_toolbar_actions(), vec![OskToolbarAction::Paste]);
        assert_eq!(*seen.borrow(), vec![OskToolbarAction::Paste]);
    }

    #[test]
    fn layout_action_switches_active_letter_layout() {
        let mut osk = OskWidget::new();
        lay_out(&mut osk, 800.0, 360.0);
        osk.apply_action(KeyAction::Layout("azerty".into()));
        assert_eq!(osk.active_layout_name(), "azerty");
        lay_out(&mut osk, 800.0, 360.0);
        // AZERTY row 0 starts with 'a'.
        assert!(osk.activate_label("a"));
        assert_eq!(osk.pending_text(), "a");
    }
}
