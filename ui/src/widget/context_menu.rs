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
//! - `Widget` impl that renders at the stored position when open using the
//!   `Theme::tokens` raised-surface + border tokens (no hard-coded styling).
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
use winit::event::WindowEvent;

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;

/// Boxed zero-argument callback invoked when an action item is clicked.
pub type MenuAction = Arc<dyn Fn() + Send + Sync>;

/// One entry in a [`ContextMenu`]. Actions carry an invocation callback;
/// separators are purely visual and ignored by [`ContextMenu::click_item`].
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
    /// Build a clickable action item with the given label and callback.
    pub fn action<F>(label: impl Into<String>, action: F) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        MenuItem::Action { label: label.into(), action: Arc::new(action) }
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
}

impl Default for ContextMenu {
    fn default() -> Self { Self::new() }
}

impl ContextMenu {
    pub fn new() -> Self {
        Self { items: Vec::new(), open: false, position: None, rect: Rect::ZERO }
    }

    /// Append a [`MenuItem`] to the end of the menu.
    pub fn push(&mut self, item: MenuItem) { self.items.push(item); }

    /// Borrow the menu's full item list (actions and separators) in order.
    pub fn items(&self) -> &[MenuItem] { &self.items }

    /// Open the menu anchored at logical position `[x, y]`.
    pub fn open_at(&mut self, position: [f32; 2]) {
        self.open = true;
        self.position = Some(position);
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

    /// Invoke the action for the item at `idx` (no-op for separators /
    /// out-of-range indices). Action items close the menu after firing.
    pub fn click_item(&mut self, idx: usize) {
        let Some(item) = self.items.get(idx) else { return };
        match item {
            MenuItem::Action { action, .. } => {
                let cb = action.clone();
                cb();
                self.open = false;
            }
            MenuItem::Separator => {}
        }
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
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
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
        for item in &self.items {
            match item {
                MenuItem::Action { label, .. } => {
                    let label_y = y + (ROW_HEIGHT - text_size as f64) / 2.0;
                    text_renderer.draw_text(
                        scene,
                        label,
                        text_size,
                        theme.tokens.colors.text_primary,
                        transform * Affine::translate((self.rect.x0 + 10.0, label_y)),
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
        _event: &WindowEvent,
        _scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        _handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        // Input routing (hit-test against rows, dismiss-on-click-outside) is
        // wired by the first shell consumer; the primitive currently exposes
        // `click_item` for programmatic / shell-driven dispatch.
    }

    fn name(&self) -> &str { "ContextMenu" }

    fn get_rect(&self) -> Rect { self.rect }
}
