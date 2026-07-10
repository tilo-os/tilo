//! `AppShell` — a reusable responsive app-shell with an integrated sidebar.
//!
//! This is the promotion of the "sidebar + content + search + collapse"
//! chrome that a settings-style app needs, into one cohesive toolkit
//! component so every system app reads the same and no app re-implements the
//! breakpoint, the collapse behaviour, or the visual grammar.
//!
//! ## What it is
//!
//! A single continuous surface, **not** a scatter of outlined cards:
//!
//! * a **sidebar panel** flush to the window's left edge (no rounded card, no
//!   floating outline) whose top is an **integrated header** carrying the
//!   search field and the sidebar-collapse toggle, aligned to one another;
//! * a **content area** filling the rest, separated from the sidebar by a
//!   single hairline divider — one panel + one area, not two cards.
//!
//! ## The three states
//!
//! Resolved each frame from the allocated width (`>= breakpoint` ⇒ *wide*)
//! and two booleans the shell owns (`collapsed`, `overlay_open`):
//!
//! * **Wide, expanded** — the sidebar is docked beside the content and its
//!   header holds the search + collapse button.
//! * **Wide, collapsed** — the sidebar is hidden to reclaim its width; the
//!   collapse button **and** the search field float over the content in a
//!   small pill so the user can still search or re-open the sidebar.
//! * **Narrow (phone)** — the list and the content are separate screens: the
//!   content fills the screen with the floating controls over it, and the
//!   sidebar (the list) is summoned as a floating overlay over a dimming
//!   scrim (dismissed by the scrim, Esc, the toggle, or picking an item).
//!
//! ## Why the children live in the host taffy tree
//!
//! Unlike [`crate::widget::NavSplit`], which lays its children out in an
//! *isolated* inner tree (so a nested field can never take keyboard focus —
//! the `App`'s focus walk never visits that tree), `AppShell` builds its
//! search field, nav list, and content as **direct children of its own node
//! in the host tree**, positioned with taffy absolute insets. That keeps the
//! search field reachable by `focusable_order`, so a click focuses it and the
//! caret survives the per-keystroke filter rebuild.
//!
//! ## Theming
//!
//! Every colour is read from the passed-in [`Theme`] — the sidebar panel, the
//! divider, the floating pill, and the scrim all track the OS palette. The
//! host supplies the nav list, the content widget, and (optionally) a search
//! field; the shell owns the collapse/overlay state and the chrome.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::{auto, length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;

/// A boxed child widget, matching the toolkit's `Element` currency.
type Element = Box<dyn Widget>;

/// Default docked / overlay sidebar width (logical px).
pub const DEFAULT_SHELL_SIDEBAR_WIDTH: f64 = 264.0;
/// Default width breakpoint (logical px): at or above it the sidebar docks,
/// below it the shell switches to the narrow list-vs-content screens.
pub const DEFAULT_SHELL_BREAKPOINT: f64 = 720.0;

/// Height (logical px) of the integrated sidebar header band.
const HEADER_H: f64 = 56.0;
/// Horizontal inset (logical px) of the header content — `spacing.s3`.
const PAD: f64 = 12.0;
/// Gap (logical px) between the collapse toggle and the search field —
/// `spacing.s2`.
const GAP: f64 = 8.0;
/// Shared height (logical px) of the search field **and** the collapse
/// toggle. Both controls are pinned to this so they line up on one baseline
/// (the review called out that they were different heights).
const CTRL_H: f64 = 34.0;
/// Margin (logical px) of the floating control pill from the window edge.
const FLOAT_M: f64 = 12.0;
/// Inner padding (logical px) of the floating control pill.
const FLOAT_PAD: f64 = 6.0;
/// Width (logical px) of the search field inside the floating pill.
const FLOAT_SEARCH_W: f64 = 220.0;
/// Hamburger glyph used for the collapse/menu toggle. Reads as "toggle the
/// navigation" in every state.
const TOGGLE_GLYPH: &str = "\u{f035c}";
/// Toggle glyph size (logical px).
const TOGGLE_GLYPH_SIZE: f32 = 18.0;

/// How the shell resolves at the current width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellLayout {
    /// Wide: the sidebar docks beside the content.
    Wide,
    /// Narrow: the list and the content are separate screens.
    Narrow,
}

impl ShellLayout {
    /// Resolve purely from the allocated width. The boundary is inclusive at
    /// the breakpoint → [`ShellLayout::Wide`].
    pub fn from_width(width: f64, breakpoint: f64) -> Self {
        if width >= breakpoint { ShellLayout::Wide } else { ShellLayout::Narrow }
    }
}

/// Where the search + toggle controls sit this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Placement {
    /// Inside the sidebar header (wide-expanded, or the open narrow overlay).
    Header,
    /// In a small pill floating over the content (wide-collapsed, or the
    /// closed narrow content screen).
    Floating,
}

/// Which owned child a taffy slot maps to, so `apply_layout` / `handle_event`
/// / `children()` all agree on the order the nodes were built in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    Search,
    Nav,
    Content,
}

/// The reusable responsive app-shell. See the module docs.
pub struct AppShell {
    nav: Element,
    content: Element,
    search: Option<Element>,

    sidebar_width: f64,
    breakpoint: f64,

    /// The user hid the docked sidebar to reclaim its width (wide only).
    collapsed: bool,
    /// The narrow floating overlay (the list screen) is showing.
    overlay_open: bool,
    /// A width to resolve the layout from before the first real frame — the
    /// offscreen `--shots` renderer seeds this so a cold screenshot picks the
    /// right wide/narrow layout immediately.
    width_hint: Option<f64>,

    /// A state change (toggle, breakpoint cross, nav/content swap) needs the
    /// host to re-run `build_layout`. Drained by `take_structure_dirty`.
    pending_rebuild: bool,

    // ---- resolved geometry (from the last layout pass) ----
    rect: Rect,
    layout: ShellLayout,
    /// The layout the current taffy tree was *built* for, to detect a
    /// breakpoint cross that needs a structural rebuild.
    built_layout: ShellLayout,
    sidebar_panel_rect: Rect,
    content_area_rect: Rect,
    toggle_rect: Rect,
    pill_rect: Rect,
    scrim_rect: Rect,

    /// The order the child nodes were built in, mirrored by `children()`.
    slots: Vec<Slot>,

    // ---- chrome interaction state ----
    toggle_hover: bool,
    toggle_press: bool,
    last_cursor: Point,
}

impl AppShell {
    /// Build a shell around a nav list and a content widget. Add a search
    /// field with [`AppShell::with_search`].
    pub fn new(nav: Element, content: Element) -> Self {
        Self {
            nav,
            content,
            search: None,
            sidebar_width: DEFAULT_SHELL_SIDEBAR_WIDTH,
            breakpoint: DEFAULT_SHELL_BREAKPOINT,
            collapsed: false,
            overlay_open: false,
            width_hint: None,
            pending_rebuild: false,
            rect: Rect::ZERO,
            layout: ShellLayout::Wide,
            built_layout: ShellLayout::Wide,
            sidebar_panel_rect: Rect::ZERO,
            content_area_rect: Rect::ZERO,
            toggle_rect: Rect::ZERO,
            pill_rect: Rect::ZERO,
            scrim_rect: Rect::ZERO,
            slots: Vec::new(),
            toggle_hover: false,
            toggle_press: false,
            last_cursor: Point::ZERO,
        }
    }

    /// Attach the host's search field. It is placed in the sidebar header (or
    /// the floating pill) and, because it is a direct child in the host tree,
    /// stays keyboard-focusable across the filter rebuild.
    pub fn with_search(mut self, search: Element) -> Self {
        self.search = Some(search);
        self
    }

    /// Override the docked / overlay sidebar width (logical px).
    pub fn with_sidebar_width(mut self, px: f64) -> Self {
        self.sidebar_width = px;
        self
    }

    /// Override the wide/narrow width breakpoint (logical px).
    pub fn with_breakpoint(mut self, px: f64) -> Self {
        self.breakpoint = px;
        self
    }

    /// Seed the width used to resolve the layout before the first real frame
    /// (deterministic `--shots`).
    pub fn with_width_hint(mut self, px: f64) -> Self {
        self.width_hint = Some(px);
        self.layout = ShellLayout::from_width(px, self.breakpoint);
        self.built_layout = self.layout;
        self
    }

    /// Start with the docked sidebar collapsed (reclaiming its width).
    pub fn with_collapsed(mut self, collapsed: bool) -> Self {
        self.collapsed = collapsed;
        self
    }

    /// Start with the narrow overlay (the list screen) open.
    pub fn with_overlay_open(mut self, open: bool) -> Self {
        self.overlay_open = open;
        self
    }

    /// Replace the nav list (the host does this when the selection changes).
    /// Requests a structural rebuild so the new list reaches the screen.
    pub fn set_nav(&mut self, nav: Element) {
        self.nav = nav;
        self.pending_rebuild = true;
    }

    /// Replace the content widget (the host does this when the selection
    /// changes). Requests a structural rebuild.
    pub fn set_content(&mut self, content: Element) {
        self.content = content;
        self.pending_rebuild = true;
    }

    /// The host calls this after a nav item is chosen. On a narrow layout it
    /// dismisses the overlay so the content screen shows — the master→detail
    /// step. A no-op when wide.
    pub fn notify_selection(&mut self) {
        if self.layout == ShellLayout::Narrow && self.overlay_open {
            self.overlay_open = false;
            self.pending_rebuild = true;
        }
    }

    /// Whether the current frame resolves to the narrow layout.
    pub fn is_narrow(&self) -> bool {
        self.layout == ShellLayout::Narrow
    }

    /// Whether the docked sidebar is explicitly collapsed.
    pub fn collapsed(&self) -> bool {
        self.collapsed
    }

    /// Whether the sidebar / list is currently on screen — docked-and-expanded
    /// when wide, or the open overlay when narrow.
    pub fn sidebar_open(&self) -> bool {
        self.sidebar_shown()
    }

    /// The resolved layout.
    pub fn layout(&self) -> ShellLayout {
        self.layout
    }

    /// The on-screen rect of the collapse / menu toggle (after a layout pass).
    /// Exposed so hosts and interaction tests can hit-test the control the
    /// shell draws itself.
    pub fn toggle_rect(&self) -> Rect {
        self.toggle_rect
    }

    /// The resolved sidebar-panel rect (after a layout pass). Zero-width when
    /// the sidebar is hidden.
    pub fn sidebar_panel_rect(&self) -> Rect {
        self.sidebar_panel_rect
    }

    /// The resolved content-area rect (after a layout pass).
    pub fn content_area_rect(&self) -> Rect {
        self.content_area_rect
    }

    /// Resolve the layout from the width hint or the last laid-out rect.
    fn resolve_layout(&self) -> ShellLayout {
        let w = self.width_hint.unwrap_or_else(|| self.rect.width());
        if w <= 0.0 {
            // No width known yet (very first build): assume wide.
            return ShellLayout::Wide;
        }
        ShellLayout::from_width(w, self.breakpoint)
    }

    /// Whether the sidebar/list surface is shown this frame.
    fn sidebar_shown(&self) -> bool {
        match self.layout {
            ShellLayout::Wide => !self.collapsed,
            ShellLayout::Narrow => self.overlay_open,
        }
    }

    /// Where the search + toggle controls sit this frame.
    fn placement(&self) -> Placement {
        if self.sidebar_shown() { Placement::Header } else { Placement::Floating }
    }

    /// Toggle the sidebar: collapse/expand when wide, open/close the overlay
    /// when narrow. Requests a structural rebuild.
    fn on_toggle(&mut self) {
        match self.layout {
            ShellLayout::Wide => self.collapsed = !self.collapsed,
            ShellLayout::Narrow => self.overlay_open = !self.overlay_open,
        }
        self.pending_rebuild = true;
    }

    /// Close the narrow overlay (scrim / Esc). Returns whether anything moved.
    fn close_overlay(&mut self) -> bool {
        if self.layout == ShellLayout::Narrow && self.overlay_open {
            self.overlay_open = false;
            self.pending_rebuild = true;
            true
        } else {
            false
        }
    }

    /// Recompute the sidebar / content / toggle / pill / scrim rects from the
    /// outer rect and the resolved state. Kept geometry-only (no theme) so it
    /// runs in `apply_layout`.
    fn recompute_rects(&mut self) {
        let r = self.rect;
        let sw = self.sidebar_width.min(r.width());
        let shown = self.sidebar_shown();

        // The sidebar panel (header + nav) and the content area.
        if self.layout == ShellLayout::Wide && shown {
            self.sidebar_panel_rect = Rect::new(r.x0, r.y0, r.x0 + sw, r.y1);
            self.content_area_rect = Rect::new(r.x0 + sw, r.y0, r.x1, r.y1);
        } else {
            self.content_area_rect = r;
            // Narrow overlay panel (drawn over the scrim); zero-width when not
            // shown so hit-testing / draw skip it.
            self.sidebar_panel_rect = if shown {
                Rect::new(r.x0, r.y0, r.x0 + sw, r.y1)
            } else {
                Rect::ZERO
            };
        }

        // Scrim only under the open narrow overlay.
        self.scrim_rect = if self.layout == ShellLayout::Narrow && shown { r } else { Rect::ZERO };

        // The search + toggle placement.
        match self.placement() {
            Placement::Header => {
                // Toggle at the header's left, search filling to its right.
                let cy = r.y0 + (HEADER_H - CTRL_H) / 2.0;
                self.toggle_rect = Rect::new(r.x0 + PAD, cy, r.x0 + PAD + CTRL_H, cy + CTRL_H);
                self.pill_rect = Rect::ZERO;
            }
            Placement::Floating => {
                let pill_w = FLOAT_PAD + CTRL_H + GAP + FLOAT_SEARCH_W + FLOAT_PAD;
                let pill_h = FLOAT_PAD * 2.0 + CTRL_H;
                self.pill_rect = Rect::new(
                    r.x0 + FLOAT_M,
                    r.y0 + FLOAT_M,
                    r.x0 + FLOAT_M + pill_w,
                    r.y0 + FLOAT_M + pill_h,
                );
                self.toggle_rect = Rect::new(
                    self.pill_rect.x0 + FLOAT_PAD,
                    self.pill_rect.y0 + FLOAT_PAD,
                    self.pill_rect.x0 + FLOAT_PAD + CTRL_H,
                    self.pill_rect.y0 + FLOAT_PAD + CTRL_H,
                );
            }
        }
    }

    /// The absolute inset + size a child slot occupies. Width-independent
    /// (fixed sidebar width; the content uses left+right insets), so it is
    /// resolvable at `build_layout` time before the real width is known.
    fn slot_style(&self, slot: Slot, base: &Style) -> Style {
        let sw = self.sidebar_width;
        let mut s = base.clone();
        s.position = taffy::Position::Absolute;
        match slot {
            Slot::Content => {
                let left = if self.layout == ShellLayout::Wide && self.sidebar_shown() {
                    length(sw as f32)
                } else {
                    length(0.0)
                };
                s.inset = taffy::Rect { left, right: length(0.0), top: length(0.0), bottom: length(0.0) };
                s.size = taffy::Size { width: auto(), height: auto() };
            }
            Slot::Nav => {
                s.inset = taffy::Rect {
                    left: length(0.0),
                    right: auto(),
                    top: length(HEADER_H as f32),
                    bottom: length(0.0),
                };
                s.size = taffy::Size { width: length(sw as f32), height: auto() };
            }
            Slot::Search => {
                let (left, top, w) = match self.placement() {
                    Placement::Header => (
                        (PAD + CTRL_H + GAP) as f32,
                        ((HEADER_H - CTRL_H) / 2.0) as f32,
                        (sw - (PAD + CTRL_H + GAP) - PAD) as f32,
                    ),
                    Placement::Floating => (
                        (FLOAT_M + FLOAT_PAD + CTRL_H + GAP) as f32,
                        (FLOAT_M + FLOAT_PAD) as f32,
                        FLOAT_SEARCH_W as f32,
                    ),
                };
                s.inset = taffy::Rect { left: length(left), right: auto(), top: length(top), bottom: auto() };
                s.size = taffy::Size { width: length(w), height: length(CTRL_H as f32) };
            }
        }
        s
    }

    /// Draw the collapse/menu toggle: a subtle hover pill under a centred
    /// hamburger glyph.
    fn draw_toggle(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let radius = theme.tokens.radii.md as f64;
        if self.toggle_press {
            let pill = RoundedRect::from_rect(self.toggle_rect, radius);
            scene.fill(Fill::NonZero, transform, theme.bg_active, None, &pill);
        } else if self.toggle_hover {
            let pill = RoundedRect::from_rect(self.toggle_rect, radius);
            scene.fill(Fill::NonZero, transform, theme.bg_hover, None, &pill);
        }
        let (gw, _gh) = tr.measure(TOGGLE_GLYPH, TOGGLE_GLYPH_SIZE, None);
        let gx = self.toggle_rect.x0 + (self.toggle_rect.width() - gw as f64) / 2.0;
        let line_box = TOGGLE_GLYPH_SIZE as f64 * 1.25;
        let gy = self.toggle_rect.y0 + (self.toggle_rect.height() - line_box) / 2.0;
        tr.draw_text(
            scene,
            TOGGLE_GLYPH,
            TOGGLE_GLYPH_SIZE,
            theme.text_primary,
            transform * Affine::translate((gx, gy)),
            None,
        );
    }

    /// Paint the shell chrome (surfaces, divider, scrim, pill, toggle) and its
    /// children in z-order. Shared by `draw` and `draw_with_presence`; the
    /// `permissions` flag selects the presence-aware child draw path.
    fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        permissions: Option<&crate::permissions::PermissionContext>,
    ) {
        let sidebar_fill = theme.tokens.colors.neutral[0];
        let content_fill = theme.tokens.colors.surfaces.base;
        let divider = theme.tokens.colors.border_standard;

        // 1. Content-area background (kills the transparent "dead space" look).
        scene.fill(Fill::NonZero, transform, content_fill, None, &self.rect);

        // 2. Content widget.
        paint_child(self.content.as_mut(), scene, transform, tr, theme, presence, permissions);

        let shown = self.sidebar_shown();

        // 3. Docked sidebar panel (wide-expanded): one flat surface, no card.
        if self.layout == ShellLayout::Wide && shown {
            scene.fill(Fill::NonZero, transform, sidebar_fill, None, &self.sidebar_panel_rect);
            let x = self.sidebar_panel_rect.x1;
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                divider,
                None,
                &Line::new(Point::new(x, self.rect.y0), Point::new(x, self.rect.y1)),
            );
            paint_child(self.nav.as_mut(), scene, transform, tr, theme, presence, permissions);
        }

        // 4. Narrow overlay: scrim, then the list panel over it.
        if self.layout == ShellLayout::Narrow && shown {
            let scrim = Color::from_rgba8(0, 0, 0, 128);
            scene.fill(Fill::NonZero, transform, scrim, None, &self.scrim_rect);
            scene.fill(Fill::NonZero, transform, sidebar_fill, None, &self.sidebar_panel_rect);
            let x = self.sidebar_panel_rect.x1;
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                divider,
                None,
                &Line::new(Point::new(x, self.rect.y0), Point::new(x, self.rect.y1)),
            );
            paint_child(self.nav.as_mut(), scene, transform, tr, theme, presence, permissions);
        }

        // 5. Floating pill (collapsed / narrow content screen).
        if !shown {
            let pill = RoundedRect::from_rect(self.pill_rect, theme.tokens.radii.lg as f64);
            scene.fill(Fill::NonZero, transform, theme.tokens.colors.surfaces.raised, None, &pill);
            scene.stroke(&Stroke::new(1.0), transform, divider, None, &pill);
        }

        // 6. Toggle + search on top of their surface.
        self.draw_toggle(scene, transform, tr, theme);
        if let Some(search) = &mut self.search {
            paint_child(search.as_mut(), scene, transform, tr, theme, presence, permissions);
        }
    }
}

/// Draw one child through the presence-aware path when a permission context is
/// available, else the plain path. Free function (not a method) so it borrows
/// none of `AppShell`'s fields — the caller keeps a live `&mut` on the child.
#[allow(clippy::too_many_arguments)]
fn paint_child(
    w: &mut dyn Widget,
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    presence: &PresenceCtx,
    permissions: Option<&crate::permissions::PermissionContext>,
) {
    match permissions {
        Some(p) => w.draw_with_presence(scene, transform, tr, theme, presence, p),
        None => w.draw(scene, transform, tr, theme, presence),
    }
}

impl Widget for AppShell {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        if let Some(search) = &mut self.search {
            search.sync(state, path);
        }
        self.nav.sync(state, path);
        self.content.sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Resolve the layout for this build from the width hint / last rect.
        self.layout = self.resolve_layout();
        self.built_layout = self.layout;

        // Build the active children as ABSOLUTE-positioned direct nodes so
        // they stay in the host tree (and thus keyboard-focusable), then pin
        // each to its slot rect.
        self.slots.clear();
        let mut child_nodes: Vec<NodeId> = Vec::new();

        if let Some(search) = &mut self.search {
            let node = search.build_layout(taffy, theme, node_map);
            let base = taffy.style(node).unwrap().clone();
            let _ = taffy.set_style(node, self.slot_style(Slot::Search, &base));
            self.slots.push(Slot::Search);
            child_nodes.push(node);
        }
        if self.sidebar_shown() {
            let node = self.nav.build_layout(taffy, theme, node_map);
            let base = taffy.style(node).unwrap().clone();
            let _ = taffy.set_style(node, self.slot_style(Slot::Nav, &base));
            self.slots.push(Slot::Nav);
            child_nodes.push(node);
        }
        {
            let node = self.content.build_layout(taffy, theme, node_map);
            let base = taffy.style(node).unwrap().clone();
            let _ = taffy.set_style(node, self.slot_style(Slot::Content, &base));
            self.slots.push(Slot::Content);
            child_nodes.push(node);
        }

        let node = taffy
            .new_with_children(
                Style {
                    display: taffy::Display::Flex,
                    size: taffy::Size { width: percent(1.0), height: percent(1.0) },
                    ..Default::default()
                },
                &child_nodes,
            )
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

        // A breakpoint cross since this tree was built needs a rebuild so the
        // child set / insets match the new layout.
        let now = ShellLayout::from_width(self.rect.width(), self.breakpoint);
        if now != self.built_layout {
            self.pending_rebuild = true;
        }
        // Once we have a real rect, stop leaning on the seed hint.
        self.width_hint = None;
        self.layout = now;
        self.recompute_rects();

        // Lay out the children at their absolute slots.
        let child_nodes = taffy.children(node).unwrap_or_default();
        let child_origin = Point::new(self.rect.x0, self.rect.y0);
        for (slot, &child_node) in self.slots.iter().zip(child_nodes.iter()) {
            let w: &mut dyn Widget = match slot {
                Slot::Search => match &mut self.search {
                    Some(s) => s.as_mut(),
                    None => continue,
                },
                Slot::Nav => self.nav.as_mut(),
                Slot::Content => self.content.as_mut(),
            };
            w.apply_layout(taffy, child_node, child_origin);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.paint(scene, transform, text_renderer, theme, presence, None);
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        permissions: &crate::permissions::PermissionContext,
    ) {
        self.paint(scene, transform, text_renderer, theme, presence, Some(permissions));
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
                let hover = self.toggle_rect.contains(self.last_cursor);
                if hover != self.toggle_hover {
                    self.toggle_hover = hover;
                }
            }
            WindowEvent::KeyboardInput {
                event: KeyEvent { logical_key: Key::Named(NamedKey::Escape), state: ElementState::Pressed, .. },
                ..
            } => {
                if self.close_overlay() {
                    *handled = true;
                    return;
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                if self.toggle_rect.contains(self.last_cursor) {
                    self.toggle_press = true;
                    *handled = true;
                    return;
                }
                // A press on the scrim (outside the overlay panel) dismisses it.
                if self.scrim_rect.width() > 0.0
                    && self.scrim_rect.contains(self.last_cursor)
                    && !self.sidebar_panel_rect.contains(self.last_cursor)
                    && self.close_overlay()
                {
                    *handled = true;
                    return;
                }
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                if self.toggle_press {
                    let on_toggle = self.toggle_rect.contains(self.last_cursor);
                    self.toggle_press = false;
                    if on_toggle {
                        self.on_toggle();
                        *handled = true;
                        return;
                    }
                }
            }
            _ => {}
        }
        if *handled {
            return;
        }

        // Route to children, topmost first. In the narrow overlay the list
        // sits *over* the content, so the sidebar must win. Otherwise the
        // sidebar and content are side by side and a position-carrying event
        // (a click) self-selects by rect — but a position-less wheel does not,
        // so we route the content pane first there, matching the old split's
        // "docked ⇒ content scrolls" behaviour (sidebar row clicks still land,
        // since the content pane ignores a click outside its rect).
        let child_nodes = taffy.children(node_id).unwrap_or_default();
        let mut pairs: Vec<(Slot, NodeId)> = Vec::new();
        for (slot, &n) in self.slots.iter().zip(child_nodes.iter()) {
            pairs.push((*slot, n));
        }
        let overlay_on_top = self.layout == ShellLayout::Narrow && self.sidebar_shown();
        let order = if overlay_on_top {
            [Slot::Search, Slot::Nav, Slot::Content]
        } else {
            [Slot::Search, Slot::Content, Slot::Nav]
        };
        for want in order {
            if *handled {
                break;
            }
            if let Some(&(_, n)) = pairs.iter().find(|(s, _)| *s == want) {
                let w: &mut dyn Widget = match want {
                    Slot::Search => match &mut self.search {
                        Some(s) => s.as_mut(),
                        None => continue,
                    },
                    Slot::Nav => self.nav.as_mut(),
                    Slot::Content => self.content.as_mut(),
                };
                w.handle_event(event, scale_factor, modifiers, state, path, handled, focus_target, n, taffy);
            }
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut out: Vec<&dyn Widget> = Vec::new();
        for slot in &self.slots {
            match slot {
                Slot::Search => {
                    if let Some(s) = &self.search {
                        out.push(s.as_ref());
                    }
                }
                Slot::Nav => out.push(self.nav.as_ref()),
                Slot::Content => out.push(self.content.as_ref()),
            }
        }
        out
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        // Borrow each field at most once, in slot order.
        let mut search = self.search.as_mut();
        let mut nav = Some(&mut self.nav);
        let mut content = Some(&mut self.content);
        let mut out: Vec<&mut (dyn Widget + '_)> = Vec::new();
        for slot in &self.slots {
            match slot {
                Slot::Search => {
                    if let Some(s) = search.take() {
                        out.push(s.as_mut());
                    }
                }
                Slot::Nav => {
                    if let Some(n) = nav.take() {
                        out.push(n.as_mut());
                    }
                }
                Slot::Content => {
                    if let Some(c) = content.take() {
                        out.push(c.as_mut());
                    }
                }
            }
        }
        out
    }

    fn name(&self) -> &str {
        "AppShell"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.pending_rebuild);
        let search = self.search.as_mut().map(|s| s.take_structure_dirty()).unwrap_or(false);
        let nav = self.nav.take_structure_dirty();
        let content = self.content.take_structure_dirty();
        own || search || nav || content
    }

    fn needs_animation_frame(&self) -> bool {
        self.content.needs_animation_frame()
            || self.nav.needs_animation_frame()
            || self.search.as_ref().is_some_and(|s| s.needs_animation_frame())
    }

    fn needs_idle_sync(&self) -> bool {
        self.content.needs_idle_sync()
            || self.nav.needs_idle_sync()
            || self.search.as_ref().is_some_and(|s| s.needs_idle_sync())
    }

    fn layout_needs_measure(&self) -> bool {
        self.content.layout_needs_measure()
            || self.nav.layout_needs_measure()
            || self.search.as_ref().is_some_and(|s| s.layout_needs_measure())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::{Text, TextInput};
    use vello::kurbo::Affine;
    use vello_encoding::DrawTag;

    fn shell() -> AppShell {
        AppShell::new(Box::new(Text::new("nav")), Box::new(Text::new("content")))
            .with_search(Box::new(TextInput::new(Vec::new()).with_id("search")))
    }

    fn lay(s: &mut AppShell, w: f64, h: f64) {
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = s.build_layout(&mut taffy, &theme, &mut map);
        let mm = map.clone();
        let mut tr = TextRenderer::new();
        taffy
            .compute_layout_with_measure(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(w as f32),
                    height: taffy::AvailableSpace::Definite(h as f32),
                },
                |known, available, id, _, _| {
                    if let Some(&p) = mm.get(&id) {
                        unsafe { (*p).measure(known, available, &mut tr) }
                    } else {
                        taffy::Size::ZERO
                    }
                },
            )
            .unwrap();
        s.apply_layout(&taffy, node, Point::ZERO);
    }

    /// The search element's resolved rect (its slot, laid out).
    fn search_rect(s: &AppShell) -> Rect {
        s.search.as_ref().unwrap().get_rect()
    }

    #[test]
    fn breakpoint_boundary_is_inclusive_wide() {
        assert_eq!(ShellLayout::from_width(720.0, 720.0), ShellLayout::Wide);
        assert_eq!(ShellLayout::from_width(719.0, 720.0), ShellLayout::Narrow);
    }

    #[test]
    fn wide_docks_an_integrated_cohesive_sidebar() {
        let mut s = shell();
        lay(&mut s, 1000.0, 700.0);
        assert!(!s.is_narrow(), "1000px is wide");
        assert!(s.sidebar_open(), "wide sidebar is docked/open by default");
        // One panel flush to the window's left edge, full height — not a card
        // inset from the edge.
        assert_eq!(s.sidebar_panel_rect.x0, 0.0, "sidebar panel is flush left");
        assert!((s.sidebar_panel_rect.height() - 700.0).abs() < 0.5, "panel spans full height");
        assert!(
            (s.sidebar_panel_rect.width() - s.sidebar_width).abs() < 0.5,
            "panel is exactly the sidebar width"
        );
        // Content begins exactly where the sidebar ends — no gap, no overlap.
        assert!(
            (s.content_area_rect.x0 - s.sidebar_panel_rect.x1).abs() < 0.5,
            "content starts flush against the sidebar (no dead gutter)"
        );
    }

    #[test]
    fn header_holds_search_and_toggle_aligned_and_inside_the_sidebar() {
        let mut s = shell();
        lay(&mut s, 1000.0, 700.0);
        let sr = search_rect(&s);
        let tr = s.toggle_rect;
        // Both controls live inside the sidebar header band, not spilling into
        // the content pane.
        assert!(tr.x0 >= s.sidebar_panel_rect.x0 && tr.x1 <= s.sidebar_panel_rect.x1, "toggle in sidebar");
        assert!(sr.x1 <= s.sidebar_panel_rect.x1 + 0.5, "search stays over the sidebar column");
        assert!(tr.y1 <= s.rect.y0 + HEADER_H + 0.5, "toggle sits in the header band");
        // Aligned: identical height and vertical centre (the review bug).
        assert!((sr.height() - tr.height()).abs() < 0.5, "search + toggle share a height");
        let sr_mid = (sr.y0 + sr.y1) / 2.0;
        let tr_mid = (tr.y0 + tr.y1) / 2.0;
        assert!((sr_mid - tr_mid).abs() < 0.5, "search + toggle share a baseline");
        // The toggle sits left of the search (it opens/closes; search fills).
        assert!(tr.x1 <= sr.x0, "toggle precedes the search field in the header");
    }

    #[test]
    fn collapse_hides_sidebar_and_floats_controls_over_content() {
        let mut s = shell();
        lay(&mut s, 1000.0, 700.0);
        assert!(s.sidebar_open());
        // Collapse.
        s.on_toggle();
        assert!(s.collapsed(), "toggle collapses the docked sidebar");
        lay(&mut s, 1000.0, 700.0);
        assert!(!s.sidebar_open(), "collapsed sidebar is hidden");
        // Content reclaims the full width.
        assert!(
            (s.content_area_rect.width() - 1000.0).abs() < 0.5,
            "collapsed content spans the full width"
        );
        // The toggle + search now float in a small pill over the content, near
        // the top-left corner — still reachable.
        assert!(s.pill_rect.width() > 0.0, "a floating control pill is shown");
        assert!(s.toggle_rect.y0 < 100.0 && s.toggle_rect.x0 < 100.0, "controls float top-left");
        let sr = search_rect(&s);
        assert!((sr.height() - s.toggle_rect.height()).abs() < 0.5, "search + toggle still aligned when floating");
    }

    #[test]
    fn narrow_defaults_to_the_content_screen_with_floating_controls() {
        let mut s = shell();
        lay(&mut s, 460.0, 820.0);
        assert!(s.is_narrow(), "460px resolves to the narrow layout");
        assert!(!s.sidebar_open(), "narrow starts on the content screen (list hidden)");
        assert!((s.content_area_rect.width() - 460.0).abs() < 0.5, "content fills the narrow screen");
        assert!(s.pill_rect.width() > 0.0, "the floating menu/search pill is available");
        assert_eq!(s.scrim_rect, Rect::ZERO, "no scrim while on the content screen");
    }

    #[test]
    fn narrow_open_overlay_is_the_list_screen_over_a_scrim() {
        let mut s = shell().with_overlay_open(true);
        lay(&mut s, 460.0, 820.0);
        assert!(s.is_narrow());
        assert!(s.sidebar_open(), "the overlay shows the list");
        assert!(s.scrim_rect.width() > 0.0, "the open overlay dims the content behind a scrim");
        assert_eq!(s.sidebar_panel_rect.x0, 0.0, "the list slides in from the left edge");
        assert!(s.sidebar_panel_rect.width() > 0.0);
    }

    #[test]
    fn picking_an_item_returns_the_narrow_overlay_to_content() {
        let mut s = shell().with_overlay_open(true);
        lay(&mut s, 460.0, 820.0);
        assert!(s.sidebar_open());
        s.notify_selection();
        assert!(s.take_structure_dirty(), "a narrow selection requests a rebuild");
        lay(&mut s, 460.0, 820.0);
        assert!(!s.sidebar_open(), "after picking an item the overlay closes to the content screen");
    }

    #[test]
    fn clicking_the_toggle_collapses_then_expands() {
        let mut s = shell();
        lay(&mut s, 1000.0, 700.0);
        // Click the toggle: cursor onto it, press, release.
        let mid = Point::new((s.toggle_rect.x0 + s.toggle_rect.x1) / 2.0, (s.toggle_rect.y0 + s.toggle_rect.y1) / 2.0);
        click(&mut s, mid);
        assert!(s.collapsed(), "a real click on the toggle collapses");
        assert!(s.take_structure_dirty(), "the collapse requests a rebuild");
        lay(&mut s, 1000.0, 700.0);
        // Click the (now floating) toggle again → expands.
        let mid = Point::new((s.toggle_rect.x0 + s.toggle_rect.x1) / 2.0, (s.toggle_rect.y0 + s.toggle_rect.y1) / 2.0);
        click(&mut s, mid);
        assert!(!s.collapsed(), "clicking the floating toggle re-expands");
    }

    #[test]
    fn esc_closes_the_narrow_overlay() {
        let mut s = shell().with_overlay_open(true);
        lay(&mut s, 460.0, 820.0);
        assert!(s.sidebar_open());
        let mut handled = false;
        dispatch(&mut s, &esc_event(), &mut handled);
        assert!(handled, "Esc on an open overlay is consumed");
        assert!(!s.sidebar_open(), "Esc closes the overlay");
    }

    #[test]
    fn scrim_click_closes_the_narrow_overlay() {
        let mut s = shell().with_overlay_open(true);
        lay(&mut s, 460.0, 820.0);
        // Click well to the right of the overlay panel, on the scrim.
        let pt = Point::new(s.rect.x1 - 10.0, 400.0);
        let mut handled = false;
        dispatch(&mut s, &cursor_at(pt), &mut handled);
        dispatch(&mut s, &left_press(), &mut handled);
        assert!(handled, "scrim click is consumed");
        assert!(!s.sidebar_open(), "scrim click closes the overlay");
    }

    #[test]
    fn resize_across_the_breakpoint_requests_a_rebuild() {
        let mut s = shell();
        lay(&mut s, 1000.0, 700.0);
        assert!(!s.is_narrow());
        let _ = s.take_structure_dirty();
        // Shrink below the breakpoint: apply_layout should notice the cross.
        lay(&mut s, 500.0, 700.0);
        assert!(s.is_narrow(), "resize resolves to narrow");
        // The very-first narrow apply flags a rebuild so the child set updates.
        // (One frame later `take_structure_dirty` is true.)
        assert!(s.take_structure_dirty() || s.built_layout == ShellLayout::Narrow);
    }

    #[test]
    fn children_track_the_slot_set() {
        let mut s = shell();
        lay(&mut s, 1000.0, 700.0);
        // Wide-expanded: search + nav + content.
        assert_eq!(s.children().len(), 3);
        s.on_toggle(); // collapse → nav drops out
        lay(&mut s, 1000.0, 700.0);
        assert_eq!(s.children().len(), 2, "collapsed sidebar removes the nav child (search + content)");
    }

    #[test]
    fn draw_emits_content() {
        let mut s = shell();
        lay(&mut s, 1000.0, 700.0);
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let perms = crate::permissions::PermissionContext::permissive();
        let mut scene = Scene::new();
        s.draw_with_presence(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence, &perms);
        let fills = scene.encoding().draw_tags.iter().filter(|t| **t == DrawTag::COLOR).count();
        assert!(fills >= 2, "a laid-out shell paints its surfaces + content");
    }

    // --- event scaffolding ---
    use winit::event::DeviceId;
    fn synth_device() -> DeviceId {
        unsafe { std::mem::zeroed() }
    }
    fn cursor_at(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved { device_id: synth_device(), position: winit::dpi::PhysicalPosition::new(p.x, p.y) }
    }
    fn left_press() -> WindowEvent {
        WindowEvent::MouseInput { device_id: synth_device(), state: ElementState::Pressed, button: MouseButton::Left }
    }
    fn left_release() -> WindowEvent {
        WindowEvent::MouseInput { device_id: synth_device(), state: ElementState::Released, button: MouseButton::Left }
    }
    fn esc_event() -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(NamedKey::Escape);
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput { device_id: synth_device(), event: ev, is_synthetic: false }
    }
    fn dispatch(s: &mut AppShell, event: &WindowEvent, handled: &mut bool) {
        // A throwaway taffy tree with a matching child count so routing works.
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = s.build_layout(&mut taffy, &theme, &mut map);
        let mut focus = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        s.handle_event(event, 1.0, &winit::event::Modifiers::default(), &mut doc, &root, handled, &mut focus, node, &taffy);
    }
    fn click(s: &mut AppShell, at: Point) {
        let mut handled = false;
        dispatch(s, &cursor_at(at), &mut handled);
        let mut handled = false;
        dispatch(s, &left_press(), &mut handled);
        let mut handled = false;
        dispatch(s, &left_release(), &mut handled);
    }
}
