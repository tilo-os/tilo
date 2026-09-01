//! `ContextMenu` primitive — a right-click / long-press invoked list of
//! actions with optional separators, backed visually by `overlay::Popover`
//! so it participates in the overlay stack, scrim dismissal, and focus
//! contract.
//!
//! Callers push `MenuItem::action(label, fn)` and optional
//! `MenuItem::separator()` entries, then call `open_at` on right-click.

use automerge::{AutoCommit, ObjId};
use std::sync::Arc;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke, Vec2};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

pub type MenuAction = Arc<dyn Fn() + Send + Sync>;

/// Separators are purely visual and ignored by [`ContextMenu::click_item`].
#[derive(Clone)]
pub enum MenuItem {
    Action { label: String, action: MenuAction },
    Separator,
}

impl std::fmt::Debug for MenuItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MenuItem::Action { label, .. } => f
                .debug_struct("MenuItem::Action")
                .field("label", label)
                .field("action", &"<fn>")
                .finish(),
            MenuItem::Separator => f.write_str("MenuItem::Separator"),
        }
    }
}

impl MenuItem {
    pub fn action<F>(label: impl Into<String>, action: F) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        MenuItem::Action {
            label: label.into(),
            action: Arc::new(action),
        }
    }

    pub fn separator() -> Self {
        MenuItem::Separator
    }

    pub fn label(&self) -> Option<&str> {
        match self {
            MenuItem::Action { label, .. } => Some(label.as_str()),
            MenuItem::Separator => None,
        }
    }
}

pub struct ContextMenu {
    items: Vec<MenuItem>,
    open: bool,
    position: Option<[f32; 2]>,
    rect: Rect,
    /// `None` when the cursor is off the menu or on a separator.
    hovered: Option<usize>,
    /// `MouseInput` carries no position, so this is tracked from
    /// `CursorMoved` to map a press to a row.
    last_cursor: Point,
    /// Set by `handle_event`, drained by [`ContextMenu::take_selection`].
    /// The `&mut self` + CRDT-friendly action channel: instead of forcing
    /// every menu action through an `Arc<dyn Fn()>` (which can't splice
    /// into an Automerge doc the host owns), the host polls
    /// `take_selection()` after dispatching the event and mutates itself.
    /// The legacy `Arc` callback still fires too.
    selection: Option<usize>,
}

impl Default for ContextMenu {
    fn default() -> Self {
        Self::new()
    }
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

    pub fn push(&mut self, item: MenuItem) {
        self.items.push(item);
    }

    pub fn items(&self) -> &[MenuItem] {
        &self.items
    }

    /// Also computes `rect` immediately (via
    /// [`layout_at_position`](Self::layout_at_position)) so
    /// [`item_at`](Self::item_at) / [`contains`](Self::contains) are usable
    /// without first running a `draw`/`apply_layout` pass — needed by
    /// consumers (like `TextInput`) that route a click to a row in the same
    /// input batch the menu opened in.
    pub fn open_at(&mut self, position: [f32; 2]) {
        self.open = true;
        self.position = Some(position);
        self.layout_at_position();
    }

    /// Mirrors the `build_layout` + `apply_layout` geometry without a taffy
    /// round-trip. A no-op when the menu has never been positioned. Lets a
    /// standalone consumer (one that owns a `ContextMenu` as a field rather
    /// than a tree node) hit-test and draw the menu directly.
    pub fn layout_at_position(&mut self) {
        let Some([px, py]) = self.position else {
            return;
        };
        let height = PADDING * 2.0
            + self
                .items
                .iter()
                .map(item_height)
                .sum::<f64>()
                .max(ROW_HEIGHT);
        let x0 = f64::from(px);
        let y0 = f64::from(py);
        self.rect = Rect::new(x0, y0, x0 + MENU_WIDTH, y0 + height);
    }

    /// Safe to call when already closed.
    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Preserved across `close()` so callers can re-open at the same spot.
    /// `None` before the menu has ever been opened.
    pub fn position(&self) -> Option<[f32; 2]> {
        self.position
    }

    /// Lets a consumer route a left-click to
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

    /// For dismiss-on-click-outside routing.
    pub fn contains(&self, point: [f64; 2]) -> bool {
        self.open
            && self
                .rect
                .contains(vello::kurbo::Point::new(point[0], point[1]))
    }

    /// No-op for separators / out-of-range indices. Records the index in
    /// [`Self::selection`] so a `&mut self` / CRDT host can react via
    /// [`Self::take_selection`].
    pub fn click_item(&mut self, idx: usize) {
        let Some(item) = self.items.get(idx) else {
            return;
        };
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

    /// Returns `Some(idx)` exactly once per selection, then `None`. The
    /// CRDT-friendly action channel — poll it after dispatching an event to
    /// perform a `&mut self` mutation the `Arc<dyn Fn()>` model can't express.
    pub fn take_selection(&mut self) -> Option<usize> {
        self.selection.take()
    }

    pub fn hovered(&self) -> Option<usize> {
        self.hovered
    }

    /// Skips separators. `dir > 0` moves down, `dir < 0` up.
    fn move_hover(&mut self, dir: i32) {
        let n = self.items.len();
        if n == 0 {
            return;
        }
        let down = dir > 0;
        // With nothing hovered, entering from above starts at the first row
        // and entering from below at the last; otherwise step off the row we
        // are on. Either way the walk then visits at most `n` candidates.
        let mut idx = match self.hovered {
            Some(i) => wrapping_step(i, n, down),
            None if down => 0,
            None => n.saturating_sub(1),
        };
        for _ in 0..n {
            if matches!(self.items.get(idx), Some(MenuItem::Action { .. })) {
                self.hovered = Some(idx);
                return;
            }
            idx = wrapping_step(idx, n, down);
        }
    }
}

/// One step through `n` items, wrapping past either end. Kept in `usize`
/// so a menu index can never go negative or wrap through a cast.
fn wrapping_step(i: usize, n: usize, down: bool) -> usize {
    if down {
        let next = i.saturating_add(1);
        if next >= n { 0 } else { next }
    } else {
        i.checked_sub(1).unwrap_or_else(|| n.saturating_sub(1))
    }
}

const ROW_HEIGHT: f64 = 28.0;
const SEPARATOR_HEIGHT: f64 = 9.0;
const MENU_WIDTH: f64 = 220.0;
const PADDING: f64 = 6.0;

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
            PADDING * 2.0
                + self
                    .items
                    .iter()
                    .map(item_height)
                    .sum::<f64>()
                    .max(ROW_HEIGHT)
        } else {
            0.0
        };
        // Both are small logical-pixel menu dimensions: MENU_WIDTH is a
        // compile-time constant and `height` is a row-count multiple of
        // ROW_HEIGHT, so neither comes close to f32's range.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "bounded logical-pixel menu dimensions; f32 precision is sufficient"
        )]
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::length(MENU_WIDTH as f32),
                height: Dimension::length(height as f32),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        // The caller-provided anchor wins when open; otherwise the
        // taffy-assigned position keeps offscreen/closed menus sensible.
        let l = taffy.layout(node).copied().unwrap_or_default();
        let (x0, y0) = if let (true, Some([px, py])) = (self.open, self.position) {
            (f64::from(px), f64::from(py))
        } else {
            (
                origin.x + f64::from(l.location.x),
                origin.y + f64::from(l.location.y),
            )
        };
        self.rect = Rect::new(
            x0,
            y0,
            x0 + f64::from(l.size.width),
            y0 + f64::from(l.size.height),
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        if !self.open {
            return;
        }

        let shape = RoundedRect::from_rect(self.rect, f64::from(theme.tokens.radii.md));
        // The near-black overlay tier, so the menu reads as an opaque
        // raised sheet, never bleeding the content beneath.
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
                MenuItem::Action { label, .. } => {
                    if self.hovered == Some(idx) {
                        let hl = Rect::new(
                            self.rect.x0 + 4.0,
                            y + 1.0,
                            self.rect.x1 - 4.0,
                            y + ROW_HEIGHT - 1.0,
                        );
                        let hl_shape = RoundedRect::from_rect(hl, f64::from(theme.tokens.radii.sm));
                        // Matches the dropdown / autocomplete row hover.
                        scene.fill(
                            vello::peniko::Fill::NonZero,
                            transform,
                            theme.tokens.colors.surface_white_10,
                            None,
                            &hl_shape,
                        );
                    }
                    let label_y = y + (ROW_HEIGHT - f64::from(text_size)) / 2.0;
                    text_renderer.draw_text(
                        scene,
                        label,
                        text_size,
                        theme.tokens.colors.text_primary,
                        transform.pre_translate(Vec2::new(self.rect.x0 + 10.0, label_y)),
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
                self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.hovered = self.item_at([self.last_cursor.x, self.last_cursor.y]);
                // Not marked handled (so cursor presence still flows) —
                // only presses are.
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
                    self.click_item(idx);
                    *handled = true;
                } else {
                    // An open menu is modal-ish: swallow the dismiss click
                    // so it doesn't also activate whatever sits underneath.
                    self.close();
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "ContextMenu"
    }

    /// Participates in the App's modal-capture contract — suppressing focus
    /// shortcuts while up.
    fn modal_capture(&self) -> Option<Rect> {
        if self.open { Some(self.rect) } else { None }
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        // Menu items are painted rows rather than child widgets, so publish
        // virtual children. A container-only node made the menu visible to an
        // assistive technology but hid every action inside it.
        let action_count = self
            .items
            .iter()
            .filter(|item| matches!(item, MenuItem::Action { .. }))
            .count();
        let mut action_position: usize = 0;
        let children = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                let MenuItem::Action { label, .. } = item else {
                    return None;
                };
                action_position = action_position.saturating_add(1);
                Some(
                    AccessNode::new(Role::MenuItem)
                        .with_name(label.clone())
                        .with_position(action_position, action_count)
                        .with_state(AccessState {
                            focusable: true,
                            focused: self.open && self.hovered == Some(index),
                            hidden: !self.open,
                            ..Default::default()
                        }),
                )
            })
            .collect();
        Some(
            AccessNode::new(Role::Menu)
                .with_state(AccessState {
                    expanded: Some(self.open),
                    hidden: !self.open,
                    ..Default::default()
                })
                .with_children(children),
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
        assert_eq!(
            node.state.expanded,
            Some(false),
            "closed menu is not expanded"
        );
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

    #[test]
    fn accessibility_exposes_named_action_rows_and_skips_separators() {
        let mut menu = ContextMenu::new();
        menu.push(MenuItem::action("Keep Offline", || {}));
        menu.push(MenuItem::separator());
        menu.push(MenuItem::action("Sharing & Access…", || {}));
        menu.open_at([10.0, 20.0]);

        let node = Widget::accessibility(&menu).unwrap();
        assert_eq!(node.children.len(), 2);
        assert_eq!(node.children[0].role, crate::a11y::Role::MenuItem);
        assert_eq!(node.children[0].name.as_deref(), Some("Keep Offline"));
        assert_eq!(node.children[0].pos_in_set, Some(1));
        assert_eq!(node.children[0].set_size, Some(2));
        assert_eq!(node.children[1].name.as_deref(), Some("Sharing & Access…"));
        assert!(node.children.iter().all(|child| child.state.focusable));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use winit::event::DeviceId;

    /// Callbacks must be `Send + Sync` (the `MenuAction` bound), so the
    /// counter is an `Arc<AtomicU32>`.
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
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
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
        let node = taffy.leaf_node(Style::default());
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
        assert_eq!(
            fired.load(Ordering::SeqCst),
            1,
            "the first action's callback fired"
        );
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
        assert_eq!(
            fired.load(Ordering::SeqCst),
            0,
            "no action fires on an outside click"
        );
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
        assert_eq!(
            fired.load(Ordering::SeqCst),
            100,
            "Enter fired the second action"
        );
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
        assert!(
            Widget::modal_capture(&m).is_some(),
            "open menu reports capture"
        );
        let closed = ContextMenu::new();
        assert!(Widget::modal_capture(&closed).is_none());
    }
}
