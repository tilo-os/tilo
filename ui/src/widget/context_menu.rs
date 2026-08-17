//! `ContextMenu` primitive — a right-click / long-press invoked list of
//! actions with optional separators.
//!
//! Spec: `docs/superpowers/specs/2026-04-18-nirios-shell-design.md` — Phase A
//! step 15 (`ContextMenu`, `Tooltip`, `Toast` primitives landed with their
//! first shell consumer). Backed visually by `overlay::Popover` so the menu
//! participates in the overlay stack, scrim dismissal and focus contract.
//!
//! This landing (plan task **T7**) provides:
//! - `MenuItem::Action { label, action }` and `MenuItem::Separator` variants.
//! - `ContextMenu` with `open_at([x, y])`, `close()`, `is_open`, `position`,
//!   `click_item(idx)` — action items close the menu after invoking the
//!   callback, separators are no-ops.
//! - `Widget` impl that renders at the stored position when open on the
//!   `Theme::tokens` overlay surface — the solid, near-black "menu tier" — with
//!   a hairline border and a subtle hover veneer (no hard-coded styling).
//!
//! Callers (shell chrome / file-explorer) push `MenuItem::action(label, fn)`
//! and optional `MenuItem::separator()` entries, then call `open_at` on
//! right-click. Input integration (click routing, keyboard nav) lands
//! alongside the first shell consumer — this task only has to satisfy the
//! primitive's construction + invocation contract exercised by the test.

use automerge::{AutoCommit, ObjId};
use std::sync::Arc;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::icons::{Icon, LUCIDE_DEFAULT_STROKE_PX};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;

/// Boxed zero-argument callback invoked when an action item is clicked.
pub type MenuAction = Arc<dyn Fn() + Send + Sync>;

/// One entry in a [`ContextMenu`]. Actions carry an invocation callback and an
/// optional leading [`Icon`]; separators are purely visual and ignored by
/// [`ContextMenu::click_item`].
#[derive(Clone)]
pub enum MenuItem {
    Action { label: String, icon: Option<Icon>, action: MenuAction },
    Separator,
}

impl std::fmt::Debug for MenuItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MenuItem::Action { label, icon, .. } => f
                .debug_struct("MenuItem::Action")
                .field("label", label)
                .field("icon", icon)
                .field("action", &"<fn>")
                .finish(),
            MenuItem::Separator => f.write_str("MenuItem::Separator"),
        }
    }
}

impl MenuItem {
    /// Build a clickable action item with the given label and callback (no
    /// icon). Use [`MenuItem::action_with_icon`] for a leading glyph.
    pub fn action<F>(label: impl Into<String>, action: F) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        MenuItem::Action { label: label.into(), icon: None, action: Arc::new(action) }
    }

    /// Build a clickable action item with a leading [`Icon`], a label and a
    /// callback. The glyph is tinted from the theme's primary text colour and
    /// drawn to the left of the label.
    pub fn action_with_icon<F>(icon: Icon, label: impl Into<String>, action: F) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        MenuItem::Action { label: label.into(), icon: Some(icon), action: Arc::new(action) }
    }

    /// Build a non-interactive separator (visual divider).
    pub fn separator() -> Self { MenuItem::Separator }

    /// Human-readable label for action items. `None` for separators.
    pub fn label(&self) -> Option<&str> {
        match self {
            MenuItem::Action { label, .. } => Some(label.as_str()),
            MenuItem::Separator => None,
        }
    }

    /// The leading icon for an action item, if any. `None` for separators and
    /// icon-less actions.
    pub fn icon(&self) -> Option<Icon> {
        match self {
            MenuItem::Action { icon, .. } => *icon,
            MenuItem::Separator => None,
        }
    }
}

/// Popover-style list of [`MenuItem`]s anchored at a caller-provided
/// position. Backed by `overlay::Popover` once wired into the shell's
/// overlay stack; the primitive itself only owns the open/closed state,
/// the anchor position and the item list.
pub struct ContextMenu {
    items: Vec<MenuItem>,
    open: bool,
    position: Option<[f32; 2]>,
    rect: Rect,
    /// Index of the action row the cursor is currently over (for hover
    /// highlight + keyboard navigation). `None` when the cursor is off the
    /// menu or on a separator.
    hovered: Option<usize>,
    /// Last cursor position in logical coords — `MouseInput` carries none,
    /// so we track it from `CursorMoved` to map a press to a row.
    last_cursor: Point,
    /// Set by `handle_event` to the index of the action row the user just
    /// chose, drained by [`ContextMenu::take_selection`]. This is the
    /// `&mut self` + CRDT-friendly action channel: instead of forcing every
    /// menu action through an `Arc<dyn Fn()>` (which can't splice into an
    /// Automerge doc the host owns), the host polls `take_selection()` after
    /// dispatching the event and performs the mutation itself. The legacy
    /// `Arc` callback still fires too, so existing callers are unaffected.
    selection: Option<usize>,
}

impl Default for ContextMenu {
    fn default() -> Self { Self::new() }
}

impl ContextMenu {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            open: false,
            position: None,
            rect: Rect::ZERO,
            hovered: None,
            last_cursor: Point::ZERO,
            selection: None,
        }
    }

    /// Append a [`MenuItem`] to the end of the menu.
    pub fn push(&mut self, item: MenuItem) { self.items.push(item); }

    /// Borrow the menu's full item list (actions and separators) in order.
    pub fn items(&self) -> &[MenuItem] { &self.items }

    /// Open the menu anchored at logical position `[x, y]`. Also computes the
    /// menu's `rect` immediately (via [`layout_at_position`](Self::layout_at_position))
    /// so [`item_at`](Self::item_at) / [`contains`](Self::contains) are usable
    /// without first running a `draw`/`apply_layout` pass — needed by consumers
    /// (like `TextInput`) that route a click to a row in the same input batch
    /// the menu opened in.
    pub fn open_at(&mut self, position: [f32; 2]) {
        self.open = true;
        self.position = Some(position);
        self.layout_at_position();
    }

    /// Recompute `self.rect` from the stored anchor position and the menu's
    /// item heights, mirroring the `build_layout` + `apply_layout` geometry but
    /// without a taffy round-trip. A no-op when the menu has never been
    /// positioned. Lets a standalone consumer (one that owns a `ContextMenu` as
    /// a field rather than as a tree node) hit-test and draw the menu directly.
    pub fn layout_at_position(&mut self) {
        let Some([px, py]) = self.position else { return };
        let height =
            PADDING * 2.0 + self.items.iter().map(item_height).sum::<f64>().max(ROW_HEIGHT);
        let x0 = px as f64;
        let y0 = py as f64;
        self.rect = Rect::new(x0, y0, x0 + MENU_WIDTH, y0 + height);
    }

    /// Close the menu. Safe to call when already closed.
    pub fn close(&mut self) {
        self.open = false;
    }

    /// `true` while the menu is visible.
    pub fn is_open(&self) -> bool { self.open }

    /// The anchor position passed to the most recent [`Self::open_at`],
    /// preserved across `close()` so callers can re-open at the same spot
    /// if desired. Returns `None` before the menu has ever been opened.
    pub fn position(&self) -> Option<[f32; 2]> { self.position }

    /// The item index under a logical `point`, if the menu is open and the
    /// point lands on an action row. Lets a consumer route a left-click to
    /// [`click_item`](Self::click_item) without re-deriving the row layout.
    pub fn item_at(&self, point: [f64; 2]) -> Option<usize> {
        if !self.open {
            return None;
        }
        let [px, py] = point;
        if px < self.rect.x0 || px > self.rect.x1 || py < self.rect.y0 || py > self.rect.y1 {
            return None;
        }
        let mut y = self.rect.y0 + PADDING;
        for (idx, item) in self.items.iter().enumerate() {
            let h = item_height(item);
            if matches!(item, MenuItem::Action { .. }) && py >= y && py < y + h {
                return Some(idx);
            }
            y += h;
        }
        None
    }

    /// Whether `point` is inside the open menu's rect (for dismiss-on-
    /// click-outside routing).
    pub fn contains(&self, point: [f64; 2]) -> bool {
        self.open && self.rect.contains(vello::kurbo::Point::new(point[0], point[1]))
    }

    /// Invoke the action for the item at `idx` (no-op for separators /
    /// out-of-range indices). Action items close the menu after firing and
    /// record the index in [`Self::selection`] so a `&mut self` / CRDT host
    /// can react via [`Self::take_selection`].
    pub fn click_item(&mut self, idx: usize) {
        let Some(item) = self.items.get(idx) else { return };
        match item {
            MenuItem::Action { action, .. } => {
                let cb = action.clone();
                cb();
                self.selection = Some(idx);
                self.open = false;
            }
            MenuItem::Separator => {}
        }
    }

    /// Drain the index of the action row the user most recently chose (via
    /// click or Enter). Returns `Some(idx)` exactly once per selection, then
    /// `None`. This is the CRDT-friendly action channel — poll it after
    /// dispatching an event to perform a `&mut self` mutation that the
    /// `Arc<dyn Fn()>` callback model can't express.
    pub fn take_selection(&mut self) -> Option<usize> {
        self.selection.take()
    }

    /// The action row currently under the cursor (for hover highlight), or
    /// `None`.
    pub fn hovered(&self) -> Option<usize> {
        self.hovered
    }

    /// Move the keyboard hover to the next/previous action row, skipping
    /// separators. `dir > 0` moves down, `dir < 0` up.
    fn move_hover(&mut self, dir: i32) {
        let n = self.items.len();
        if n == 0 {
            return;
        }
        let start = self.hovered.map(|i| i as i32).unwrap_or(if dir > 0 { -1 } else { n as i32 });
        let mut i = start;
        for _ in 0..n {
            i += dir.signum();
            if i < 0 {
                i = n as i32 - 1;
            } else if i >= n as i32 {
                i = 0;
            }
            if matches!(self.items[i as usize], MenuItem::Action { .. }) {
                self.hovered = Some(i as usize);
                return;
            }
        }
    }
}

const ROW_HEIGHT: f64 = 28.0;
const SEPARATOR_HEIGHT: f64 = 9.0;
const MENU_WIDTH: f64 = 220.0;
const PADDING: f64 = 6.0;
/// Edge length of a leading action glyph.
const ICON_PX: f64 = 16.0;

fn item_height(item: &MenuItem) -> f64 {
    match item {
        MenuItem::Action { .. } => ROW_HEIGHT,
        MenuItem::Separator => SEPARATOR_HEIGHT,
    }
}

impl Widget for ContextMenu {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let height: f64 = if self.open {
            PADDING * 2.0 + self.items.iter().map(item_height).sum::<f64>().max(ROW_HEIGHT)
        } else {
            0.0
        };
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::length(MENU_WIDTH as f32),
                    height: Dimension::length(height as f32),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        // Prefer the caller-provided anchor when the menu is open; otherwise
        // follow the taffy-assigned position so offscreen/closed menus still
        // have a sensible rect.
        let l = taffy.layout(node).unwrap();
        let (x0, y0) = if let (true, Some([px, py])) = (self.open, self.position) {
            (px as f64, py as f64)
        } else {
            (origin.x + l.location.x as f64, origin.y + l.location.y as f64)
        };
        self.rect = Rect::new(x0, y0, x0 + l.size.width as f64, y0 + l.size.height as f64);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        if !self.open { return; }

        let shape = RoundedRect::from_rect(self.rect, theme.tokens.radii.md as f64);
        // The solid, near-black overlay tier — the "menu surface" — so the menu
        // reads as an opaque raised sheet, never bleeding the content beneath.
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.overlay,
            None,
            &shape,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.tokens.colors.border_standard,
            None,
            &shape,
        );

        let text_size = theme.tokens.typography.scale.sm;
        let mut y = self.rect.y0 + PADDING;
        for (idx, item) in self.items.iter().enumerate() {
            match item {
                MenuItem::Action { label, icon, .. } => {
                    // Hover highlight behind the active row.
                    if self.hovered == Some(idx) {
                        let hl = Rect::new(
                            self.rect.x0 + 4.0,
                            y + 1.0,
                            self.rect.x1 - 4.0,
                            y + ROW_HEIGHT - 1.0,
                        );
                        let hl_shape = RoundedRect::from_rect(hl, theme.tokens.radii.sm as f64);
                        // A subtle light veneer for the hovered row — visible on
                        // the near-black menu surface (matching the dropdown /
                        // autocomplete row hover).
                        scene.fill(
                            vello::peniko::Fill::NonZero,
                            transform,
                            theme.tokens.colors.surface_white_10,
                            None,
                            &hl_shape,
                        );
                    }
                    // Optional leading glyph, then the label indented past it so
                    // labelled rows align whether or not they carry an icon.
                    let label_x = if let Some(icon) = icon {
                        let iy = y + (ROW_HEIGHT - ICON_PX) / 2.0;
                        let icon_rect =
                            Rect::new(self.rect.x0 + 10.0, iy, self.rect.x0 + 10.0 + ICON_PX, iy + ICON_PX);
                        icon.draw_with_stroke(
                            scene,
                            transform,
                            icon_rect,
                            theme.tokens.colors.text_primary,
                            LUCIDE_DEFAULT_STROKE_PX,
                        );
                        self.rect.x0 + 10.0 + ICON_PX + 10.0
                    } else {
                        self.rect.x0 + 10.0
                    };
                    let label_y = y + (ROW_HEIGHT - text_size as f64) / 2.0;
                    text_renderer.draw_text(
                        scene,
                        label,
                        text_size,
                        theme.tokens.colors.text_primary,
                        transform * Affine::translate((label_x, label_y)),
                        None,
                    );
                    y += ROW_HEIGHT;
                }
                MenuItem::Separator => {
                    let sep_y = y + SEPARATOR_HEIGHT / 2.0;
                    let line = Rect::new(
                        self.rect.x0 + 8.0,
                        sep_y - 0.5,
                        self.rect.x1 - 8.0,
                        sep_y + 0.5,
                    );
                    scene.fill(
                        vello::peniko::Fill::NonZero,
                        transform,
                        theme.tokens.colors.border_standard,
                        None,
                        &line,
                    );
                    y += SEPARATOR_HEIGHT;
                }
            }
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
        if !self.open {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor =
                    Point::new(position.x / scale_factor, position.y / scale_factor);
                self.hovered = self.item_at([self.last_cursor.x, self.last_cursor.y]);
                // An open menu is a transient overlay; while it's up it owns
                // the pointer over its own rect. We don't mark hover handled
                // (so cursor presence still flows), only presses.
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => match key {
                Key::Named(NamedKey::Escape) => {
                    self.close();
                    *handled = true;
                }
                Key::Named(NamedKey::ArrowDown) => {
                    self.move_hover(1);
                    *handled = true;
                }
                Key::Named(NamedKey::ArrowUp) => {
                    self.move_hover(-1);
                    *handled = true;
                }
                Key::Named(NamedKey::Enter) => {
                    if let Some(idx) = self.hovered {
                        self.click_item(idx);
                    }
                    *handled = true;
                }
                _ => {}
            },
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(idx) = self.item_at([self.last_cursor.x, self.last_cursor.y]) {
                    // Row click — fire the action + record the selection,
                    // then close. Consume so it doesn't reach widgets behind.
                    self.click_item(idx);
                    *handled = true;
                } else {
                    // Click outside the menu (or on a separator) dismisses.
                    // An open menu is modal-ish: swallow the dismiss click so
                    // the press that closed the menu doesn't also activate
                    // whatever sits underneath.
                    self.close();
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str { "ContextMenu" }

    /// An open context menu traps input over its rect (and dismisses on
    /// outside-click via `handle_event`), so it participates in the App's
    /// modal-capture contract — suppressing focus shortcuts while up.
    fn modal_capture(&self) -> Option<Rect> {
        if self.open { Some(self.rect) } else { None }
    }

    fn get_rect(&self) -> Rect { self.rect }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        // The menu items are drawn rows (not child widgets), so the container
        // carries the semantics. `expanded` mirrors the open state and a
        // closed menu is `hidden` so it collapses out of the focus order.
        Some(
            AccessNode::new(Role::Menu).with_state(AccessState {
                expanded: Some(self.open),
                hidden: !self.open,
                ..Default::default()
            }),
        )
    }
}

#[cfg(test)]
mod a11y_tests {
    use super::*;
    use crate::widget::Widget;

    #[test]
    fn accessibility_reports_menu_role_and_collapsed_when_closed() {
        use crate::a11y::Role;
        let mut menu = ContextMenu::new();
        menu.push(MenuItem::action("Open", || {}));
        let node = Widget::accessibility(&menu).expect("menu is not presentational");
        assert_eq!(node.role, Role::Menu);
        assert_eq!(node.state.expanded, Some(false), "closed menu is not expanded");
        assert!(node.state.hidden, "closed menu is hidden from the tree");
    }

    #[test]
    fn accessibility_open_menu_is_expanded_and_visible() {
        let mut menu = ContextMenu::new();
        menu.push(MenuItem::action("Open", || {}));
        menu.open_at([10.0, 20.0]);
        let node = Widget::accessibility(&menu).unwrap();
        assert_eq!(node.state.expanded, Some(true), "open menu is expanded");
        assert!(!node.state.hidden, "open menu is visible");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use winit::event::DeviceId;

    /// Open a 3-action menu (with a separator) at (100, 100) and run the
    /// layout pass so `rect` + row geometry are populated. The callbacks
    /// must be `Send + Sync` (the `MenuAction` bound), so the counter is an
    /// `Arc<AtomicU32>`.
    fn open_menu() -> (ContextMenu, Arc<AtomicU32>) {
        let fired = Arc::new(AtomicU32::new(0));
        let f1 = fired.clone();
        let f2 = fired.clone();
        let mut m = ContextMenu::new();
        m.push(MenuItem::action("Cut", move || {
            f1.fetch_add(1, Ordering::SeqCst);
        }));
        m.push(MenuItem::separator());
        m.push(MenuItem::action("Copy", move || {
            f2.fetch_add(100, Ordering::SeqCst);
        }));
        m.open_at([100.0, 100.0]);
        lay_out(&mut m);
        (m, fired)
    }

    fn lay_out(m: &mut ContextMenu) {
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = m.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(800.0),
                    height: taffy::AvailableSpace::Definite(600.0),
                },
            )
            .unwrap();
        m.apply_layout(&taffy, node, Point::ZERO);
    }

    fn synth_device() -> DeviceId {
        unsafe { std::mem::zeroed() }
    }
    fn cursor_at(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: synth_device(),
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        }
    }
    fn left_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: synth_device(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }
    fn key(named: NamedKey) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(named);
        ev.physical_key =
            winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput {
            device_id: synth_device(),
            event: ev,
            is_synthetic: false,
        }
    }
    fn dispatch(m: &mut ContextMenu, event: &WindowEvent) -> bool {
        let mut taffy = TaffyTree::<()>::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        let mut handled = false;
        let mut focus_target = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        m.handle_event(
            event,
            1.0,
            &winit::event::Modifiers::default(),
            &mut doc,
            &root,
            &mut handled,
            &mut focus_target,
            node,
            &taffy,
        );
        handled
    }

    #[test]
    fn click_on_row_fires_action_and_records_selection() {
        let (mut m, fired) = open_menu();
        // First action row centre: y0 + PADDING + ROW_HEIGHT/2.
        let r = m.get_rect();
        let row0_y = r.y0 + PADDING + ROW_HEIGHT / 2.0;
        let p = Point::new(r.x0 + 20.0, row0_y);
        dispatch(&mut m, &cursor_at(p));
        let handled = dispatch(&mut m, &left_press());
        assert!(handled, "a row click is consumed");
        assert_eq!(fired.load(Ordering::SeqCst), 1, "the first action's callback fired");
        assert_eq!(m.take_selection(), Some(0), "selection index is exposed");
        assert!(!m.is_open(), "the menu closes after a selection");
        assert_eq!(m.take_selection(), None, "selection drains exactly once");
    }

    #[test]
    fn click_outside_dismisses_and_is_swallowed() {
        let (mut m, fired) = open_menu();
        let r = m.get_rect();
        let p = Point::new(r.x1 + 50.0, r.y1 + 50.0);
        dispatch(&mut m, &cursor_at(p));
        let handled = dispatch(&mut m, &left_press());
        assert!(handled, "outside click is swallowed (not passed through)");
        assert!(!m.is_open(), "outside click dismisses the menu");
        assert_eq!(fired.load(Ordering::SeqCst), 0, "no action fires on an outside click");
    }

    #[test]
    fn esc_dismisses() {
        let (mut m, _f) = open_menu();
        let handled = dispatch(&mut m, &key(NamedKey::Escape));
        assert!(handled);
        assert!(!m.is_open(), "Esc closes the menu");
    }

    #[test]
    fn keyboard_nav_skips_separator_and_enter_selects() {
        let (mut m, fired) = open_menu();
        // Down → first action (idx 0).
        dispatch(&mut m, &key(NamedKey::ArrowDown));
        assert_eq!(m.hovered(), Some(0));
        // Down again → must skip the separator (idx 1) and land on idx 2.
        dispatch(&mut m, &key(NamedKey::ArrowDown));
        assert_eq!(m.hovered(), Some(2), "ArrowDown skips the separator");
        // Enter selects the hovered row.
        dispatch(&mut m, &key(NamedKey::Enter));
        assert_eq!(fired.load(Ordering::SeqCst), 100, "Enter fired the second action");
        assert_eq!(m.take_selection(), Some(2));
    }

    #[test]
    fn hover_tracks_cursor_row() {
        let (mut m, _f) = open_menu();
        let r = m.get_rect();
        let row0_y = r.y0 + PADDING + ROW_HEIGHT / 2.0;
        dispatch(&mut m, &cursor_at(Point::new(r.x0 + 10.0, row0_y)));
        assert_eq!(m.hovered(), Some(0), "cursor over row 0 highlights it");
        // Move onto the separator → no hover.
        let sep_y = r.y0 + PADDING + ROW_HEIGHT + SEPARATOR_HEIGHT / 2.0;
        dispatch(&mut m, &cursor_at(Point::new(r.x0 + 10.0, sep_y)));
        assert_eq!(m.hovered(), None, "cursor over a separator clears hover");
    }

    #[test]
    fn modal_capture_when_open() {
        let (m, _f) = open_menu();
        assert!(Widget::modal_capture(&m).is_some(), "open menu reports capture");
        let closed = ContextMenu::new();
        assert!(Widget::modal_capture(&closed).is_none());
    }

    #[test]
    fn action_with_icon_reports_its_glyph() {
        use crate::icons::Icon;
        let item = MenuItem::action_with_icon(Icon::Copy, "Copy link", || {});
        assert_eq!(item.icon(), Some(Icon::Copy), "an icon item exposes its glyph");
        assert_eq!(item.label(), Some("Copy link"));
        // A plain action carries no icon.
        assert_eq!(MenuItem::action("Plain", || {}).icon(), None);
    }

    #[test]
    fn icon_row_paints_the_glyph() {
        use crate::icons::Icon;
        use vello::Scene;
        use vello_encoding::DrawTag;

        // Draw an icon-bearing menu and a label-only menu of the same shape;
        // the icon variant must emit strictly more draw work (the glyph
        // strokes) — proving the icon reaches the scene.
        fn draw_menu(with_icon: bool) -> usize {
            let mut m = ContextMenu::new();
            if with_icon {
                m.push(MenuItem::action_with_icon(Icon::Star, "Favourite", || {}));
            } else {
                m.push(MenuItem::action("Favourite", || {}));
            }
            m.open_at([40.0, 40.0]);
            lay_out(&mut m);
            let theme = Theme::dark();
            let mut tr = crate::text_render::TextRenderer::new();
            let presence = crate::presence::PresenceCtx::default();
            let mut scene = Scene::new();
            m.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
            scene
                .encoding()
                .draw_tags
                .iter()
                .filter(|t| **t == DrawTag::COLOR)
                .count()
        }
        assert!(
            draw_menu(true) > draw_menu(false),
            "the icon variant paints extra glyph strokes"
        );
    }
}
