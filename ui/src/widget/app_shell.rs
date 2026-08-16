//! `AppShell` — a reusable responsive app-shell with an integrated sidebar.
//!
//! This is the promotion of the "sidebar + content + search" chrome that a
//! settings-style app needs into one cohesive toolkit component, so every
//! system app reads the same and no app re-implements the breakpoint or the
//! visual grammar.
//!
//! ## What it is
//!
//! A single continuous surface, **not** a scatter of outlined cards. The shell
//! resolves into exactly one of two states, decided purely by whether the
//! sidebar fits beside the content at the current width:
//!
//! * **Wide** — the sidebar fits. It is **always** docked flush to the
//!   window's left edge: the search field pinned at the top of the sidebar,
//!   the nav list beneath it, and the content filling the area to its right,
//!   separated by a single hairline divider. There is no collapse control —
//!   when the sidebar fits it simply stays.
//! * **Narrow (phone)** — the sidebar does not fit, so the list and the content
//!   become **two separate full-screen screens**, never an overlay and never a
//!   scrim over the content:
//!   * the *list screen* fills the window with the search field + the nav list;
//!   * the *content screen* fills the window with the selected category's
//!     content and a back-arrow icon in the top-left. Tapping back returns to
//!     the list. Picking a category on the list navigates to its content
//!     screen. The selected category is preserved across back/forward.
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
//! divider, and the back arrow all track the OS palette. The host supplies the
//! nav list, the content widget, and (optionally) a search field; the shell
//! owns the responsive behaviour and the chrome.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::{auto, length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;

/// A boxed child widget, matching the toolkit's `Element` currency.
type Element = Box<dyn Widget>;

/// Default docked sidebar width (logical px).
pub const DEFAULT_SHELL_SIDEBAR_WIDTH: f64 = 264.0;
/// Default width breakpoint (logical px): at or above it the sidebar fits and
/// docks; below it the shell switches to the narrow list-vs-content screens.
/// Sized as roughly `sidebar_width + a minimum content column`.
pub const DEFAULT_SHELL_BREAKPOINT: f64 = 720.0;

/// Horizontal / vertical inset (logical px) of the search band content —
/// `spacing.s3`.
const PAD: f64 = 12.0;
/// Height (logical px) of the search field.
const CTRL_H: f64 = 34.0;
/// Height (logical px) of the search band at the top of the sidebar / list:
/// the search field with padding above and below.
const SEARCH_BAND_H: f64 = PAD + CTRL_H + PAD;
/// Side (logical px) of the narrow content screen's back-arrow button.
const BACK_BTN: f64 = 34.0;
/// Height (logical px) of the narrow content screen's top band that holds the
/// back arrow; the content starts below it.
const BACK_BAND_H: f64 = PAD + BACK_BTN + PAD;
/// Arrow-left glyph for the narrow content screen's back affordance.
const BACK_GLYPH: &str = "\u{f004d}";
/// Back-arrow glyph size (logical px).
const BACK_GLYPH_SIZE: f32 = 20.0;

/// How the shell resolves at the current width — decided purely by whether the
/// sidebar fits beside the content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellLayout {
    /// Wide: the sidebar fits and docks beside the content.
    Wide,
    /// Narrow: the sidebar does not fit; the list and content are separate
    /// full-screen screens.
    Narrow,
}

impl ShellLayout {
    /// Resolve purely from the allocated width. The boundary is inclusive at
    /// the breakpoint → [`ShellLayout::Wide`].
    pub fn from_width(width: f64, breakpoint: f64) -> Self {
        if width >= breakpoint { ShellLayout::Wide } else { ShellLayout::Narrow }
    }
}

/// Which of the two full-screen narrow screens is showing. Meaningful only in
/// the [`ShellLayout::Narrow`] state; ignored when wide (both are shown).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NarrowScreen {
    /// The category list (search + nav) fills the window.
    List,
    /// The selected category's content fills the window, with a back arrow.
    Content,
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

    /// Which narrow screen is showing (narrow only). Preserved across a
    /// wide↔narrow resize so the user returns to where they were.
    narrow_screen: NarrowScreen,
    /// The surface the user most recently engaged (a press landed in it, a
    /// category was picked, or Back was taken). A wide→narrow collapse seeds
    /// [`Self::narrow_screen`] from this, so the content the user was reading
    /// survives the resize instead of dumping them back on the category list.
    /// Defaults to [`NarrowScreen::Content`] — a collapse **never** defaults to
    /// the navigation list.
    last_active: NarrowScreen,
    /// A width to resolve the layout from before the first real frame — the
    /// offscreen `--shots` renderer seeds this so a cold screenshot picks the
    /// right wide/narrow layout immediately.
    width_hint: Option<f64>,

    /// A state change (screen navigation, breakpoint cross, nav/content swap)
    /// needs the host to re-run `build_layout`. Drained by
    /// `take_structure_dirty`.
    pending_rebuild: bool,

    // ---- resolved geometry (from the last layout pass) ----
    rect: Rect,
    layout: ShellLayout,
    /// The layout the current taffy tree was *built* for, to detect a
    /// breakpoint cross that needs a structural rebuild.
    built_layout: ShellLayout,
    sidebar_panel_rect: Rect,
    content_area_rect: Rect,
    /// The narrow content screen's back-arrow button (zero-sized otherwise).
    back_rect: Rect,

    /// The order the child nodes were built in, mirrored by `children()`.
    slots: Vec<Slot>,

    // ---- back-button interaction state ----
    back_hover: bool,
    back_press: bool,
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
            // Collapse to the CONTENT first: the primary content stays on
            // screen and the category list is reachable via the back arrow.
            // Never default a collapse to the navigation list.
            narrow_screen: NarrowScreen::Content,
            last_active: NarrowScreen::Content,
            width_hint: None,
            pending_rebuild: false,
            rect: Rect::ZERO,
            layout: ShellLayout::Wide,
            built_layout: ShellLayout::Wide,
            sidebar_panel_rect: Rect::ZERO,
            content_area_rect: Rect::ZERO,
            back_rect: Rect::ZERO,
            slots: Vec::new(),
            back_hover: false,
            back_press: false,
            last_cursor: Point::ZERO,
        }
    }

    /// Attach the host's search field. It is pinned at the top of the sidebar
    /// (or the narrow list screen) and, because it is a direct child in the
    /// host tree, stays keyboard-focusable across the filter rebuild.
    pub fn with_search(mut self, search: Element) -> Self {
        self.search = Some(search);
        self
    }

    /// Override the docked sidebar width (logical px).
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

    /// Seed which narrow screen the shell starts on — the "active surface"
    /// hint (a `--shots` state, or a host that tracks navigation in its own
    /// model). No visible effect when the layout resolves to wide. Also seeds
    /// the last-interacted surface, so a later collapse honours this choice.
    pub fn with_narrow_screen(mut self, screen: NarrowScreen) -> Self {
        self.narrow_screen = screen;
        self.last_active = screen;
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
    /// navigates from the list screen to the content screen (the master→detail
    /// step). A no-op when wide (both are already visible).
    pub fn notify_selection(&mut self) {
        // Picking a category is a move toward its content — remember that so a
        // later collapse keeps the content on screen.
        self.last_active = NarrowScreen::Content;
        if self.layout == ShellLayout::Narrow && self.narrow_screen == NarrowScreen::List {
            self.narrow_screen = NarrowScreen::Content;
            self.pending_rebuild = true;
        }
    }

    /// Navigate back from the narrow content screen to the list screen. The
    /// selected category is preserved. Returns whether anything moved (false
    /// when wide, or already on the list). The shell wires the back-arrow
    /// button and Esc to this; hosts may also call it directly.
    pub fn back(&mut self) -> bool {
        if self.layout == ShellLayout::Narrow && self.narrow_screen == NarrowScreen::Content {
            self.narrow_screen = NarrowScreen::List;
            // The user explicitly asked for the list — respect that on a later
            // collapse instead of forcing the content-first default.
            self.last_active = NarrowScreen::List;
            self.pending_rebuild = true;
            true
        } else {
            false
        }
    }

    /// Whether the current frame resolves to the narrow layout.
    pub fn is_narrow(&self) -> bool {
        self.layout == ShellLayout::Narrow
    }

    /// Which narrow screen is showing (meaningful only when [`is_narrow`]).
    ///
    /// [`is_narrow`]: AppShell::is_narrow
    pub fn narrow_screen(&self) -> NarrowScreen {
        self.narrow_screen
    }

    /// Whether the sidebar / list surface is on screen — always when wide, or
    /// the list screen when narrow.
    pub fn sidebar_open(&self) -> bool {
        self.sidebar_shown()
    }

    /// The resolved layout.
    pub fn layout(&self) -> ShellLayout {
        self.layout
    }

    /// The narrow content screen's back-arrow button rect (after a layout
    /// pass). Zero-sized when not on the narrow content screen. Exposed so
    /// hosts and interaction tests can hit-test the control the shell draws.
    pub fn back_rect(&self) -> Rect {
        self.back_rect
    }

    /// The resolved sidebar-panel rect (after a layout pass). Zero-width when
    /// the sidebar is not shown (the narrow content screen).
    pub fn sidebar_panel_rect(&self) -> Rect {
        self.sidebar_panel_rect
    }

    /// The resolved content-area rect (after a layout pass). Zero-width on the
    /// narrow list screen (no content behind it).
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

    /// Whether the sidebar / list surface (search + nav) is shown this frame:
    /// always when wide, only the list screen when narrow.
    fn sidebar_shown(&self) -> bool {
        match self.layout {
            ShellLayout::Wide => true,
            ShellLayout::Narrow => self.narrow_screen == NarrowScreen::List,
        }
    }

    /// Whether the content widget is shown this frame: always when wide, only
    /// the content screen when narrow.
    fn content_shown(&self) -> bool {
        match self.layout {
            ShellLayout::Wide => true,
            ShellLayout::Narrow => self.narrow_screen == NarrowScreen::Content,
        }
    }

    /// Recompute the sidebar / content / back-button rects from the outer rect
    /// and the resolved state. Kept geometry-only (no theme) so it runs in
    /// `apply_layout`.
    fn recompute_rects(&mut self) {
        let r = self.rect;
        let sw = self.sidebar_width.min(r.width());

        match self.layout {
            ShellLayout::Wide => {
                self.sidebar_panel_rect = Rect::new(r.x0, r.y0, r.x0 + sw, r.y1);
                self.content_area_rect = Rect::new(r.x0 + sw, r.y0, r.x1, r.y1);
                self.back_rect = Rect::ZERO;
            }
            ShellLayout::Narrow => match self.narrow_screen {
                NarrowScreen::List => {
                    // The list fills the whole screen; no content behind it.
                    self.sidebar_panel_rect = r;
                    self.content_area_rect = Rect::ZERO;
                    self.back_rect = Rect::ZERO;
                }
                NarrowScreen::Content => {
                    // The content fills the whole screen; the back arrow sits
                    // in the top-left band above it.
                    self.sidebar_panel_rect = Rect::ZERO;
                    self.content_area_rect = r;
                    self.back_rect = Rect::new(
                        r.x0 + PAD,
                        r.y0 + PAD,
                        r.x0 + PAD + BACK_BTN,
                        r.y0 + PAD + BACK_BTN,
                    );
                }
            },
        }
    }

    /// The absolute inset + size a child slot occupies. Width-independent where
    /// it can be (fixed sidebar width; content uses left+right insets), so it is
    /// resolvable at `build_layout` time before the real width is known.
    fn slot_style(&self, slot: Slot, base: &Style) -> Style {
        let sw = self.sidebar_width;
        let mut s = base.clone();
        s.position = taffy::Position::Absolute;
        match slot {
            Slot::Content => {
                // Wide: right of the fixed sidebar column. Narrow content
                // screen: full width, below the back band.
                let (left, top) = if self.layout == ShellLayout::Wide {
                    (length(sw as f32), length(0.0))
                } else {
                    (length(0.0), length(BACK_BAND_H as f32))
                };
                s.inset = taffy::Rect { left, right: length(0.0), top, bottom: length(0.0) };
                s.size = taffy::Size { width: auto(), height: auto() };
            }
            Slot::Nav => {
                // Below the search band. Wide: fixed sidebar width. Narrow list
                // screen: fill the full width.
                let right = if self.layout == ShellLayout::Wide { auto() } else { length(0.0) };
                let width = if self.layout == ShellLayout::Wide { length(sw as f32) } else { auto() };
                s.inset = taffy::Rect {
                    left: length(0.0),
                    right,
                    top: length(SEARCH_BAND_H as f32),
                    bottom: length(0.0),
                };
                s.size = taffy::Size { width, height: auto() };
            }
            Slot::Search => {
                // Pinned at the top. Wide: fixed sidebar-width column. Narrow
                // list screen: stretch across the full width.
                s.inset = taffy::Rect {
                    left: length(PAD as f32),
                    right: if self.layout == ShellLayout::Wide { auto() } else { length(PAD as f32) },
                    top: length(PAD as f32),
                    bottom: auto(),
                };
                let width = if self.layout == ShellLayout::Wide {
                    length((sw - 2.0 * PAD) as f32)
                } else {
                    auto()
                };
                s.size = taffy::Size { width, height: length(CTRL_H as f32) };
            }
        }
        s
    }

    /// Draw the narrow content screen's back-arrow button: a subtle hover pill
    /// under a centred arrow-left glyph.
    fn draw_back(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let radius = theme.tokens.radii.md as f64;
        if self.back_press {
            let pill = RoundedRect::from_rect(self.back_rect, radius);
            scene.fill(Fill::NonZero, transform, theme.bg_active, None, &pill);
        } else if self.back_hover {
            let pill = RoundedRect::from_rect(self.back_rect, radius);
            scene.fill(Fill::NonZero, transform, theme.bg_hover, None, &pill);
        }
        let (gw, _gh) = tr.measure(BACK_GLYPH, BACK_GLYPH_SIZE, None);
        let gx = self.back_rect.x0 + (self.back_rect.width() - gw as f64) / 2.0;
        let line_box = BACK_GLYPH_SIZE as f64 * 1.25;
        let gy = self.back_rect.y0 + (self.back_rect.height() - line_box) / 2.0;
        tr.draw_text(
            scene,
            BACK_GLYPH,
            BACK_GLYPH_SIZE,
            theme.text_primary,
            transform * Affine::translate((gx, gy)),
            None,
        );
    }

    /// Paint the shell chrome (surfaces, divider, back arrow) and its children
    /// in z-order. Shared by `draw` and `draw_with_presence`; the `permissions`
    /// flag selects the presence-aware child draw path.
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

        let narrow_list = self.layout == ShellLayout::Narrow && self.narrow_screen == NarrowScreen::List;

        // 1. Base background. The narrow list screen is one flat sidebar
        //    surface; otherwise the content surface fills behind everything.
        let base = if narrow_list { sidebar_fill } else { content_fill };
        scene.fill(Fill::NonZero, transform, base, None, &self.rect);

        // 2. Content widget (wide, or the narrow content screen).
        if self.content_shown() {
            paint_child(self.content.as_mut(), scene, transform, tr, theme, presence, permissions);
        }

        // 3. Wide docked sidebar panel: one flat surface + a hairline divider.
        if self.layout == ShellLayout::Wide {
            scene.fill(Fill::NonZero, transform, sidebar_fill, None, &self.sidebar_panel_rect);
            let x = self.sidebar_panel_rect.x1;
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                divider,
                None,
                &Line::new(Point::new(x, self.rect.y0), Point::new(x, self.rect.y1)),
            );
        }

        // 4. Nav list + search on top of their surface (any screen that shows
        //    the sidebar: wide, or the narrow list screen).
        if self.sidebar_shown() {
            paint_child(self.nav.as_mut(), scene, transform, tr, theme, presence, permissions);
            if let Some(search) = &mut self.search {
                paint_child(search.as_mut(), scene, transform, tr, theme, presence, permissions);
            }
        }

        // 5. Narrow content screen: a hairline under the top band + the back
        //    arrow that returns to the list.
        if self.layout == ShellLayout::Narrow && self.narrow_screen == NarrowScreen::Content {
            let y = self.rect.y0 + BACK_BAND_H;
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                divider,
                None,
                &Line::new(Point::new(self.rect.x0, y), Point::new(self.rect.x1, y)),
            );
            self.draw_back(scene, transform, tr, theme);
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

        if self.sidebar_shown() {
            if let Some(search) = &mut self.search {
                let node = search.build_layout(taffy, theme, node_map);
                let base = taffy.style(node).unwrap().clone();
                let _ = taffy.set_style(node, self.slot_style(Slot::Search, &base));
                self.slots.push(Slot::Search);
                child_nodes.push(node);
            }
            let node = self.nav.build_layout(taffy, theme, node_map);
            let base = taffy.style(node).unwrap().clone();
            let _ = taffy.set_style(node, self.slot_style(Slot::Nav, &base));
            self.slots.push(Slot::Nav);
            child_nodes.push(node);
        }
        if self.content_shown() {
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
            // A wide→narrow collapse can only show one surface. Seed it from
            // the last-interacted one (content by default) so the content the
            // user was reading survives the resize — never the category list.
            if now == ShellLayout::Narrow {
                self.narrow_screen = self.last_active;
            }
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
                let hover = self.back_rect.width() > 0.0 && self.back_rect.contains(self.last_cursor);
                if hover != self.back_hover {
                    self.back_hover = hover;
                }
            }
            WindowEvent::KeyboardInput {
                event: KeyEvent { logical_key: Key::Named(NamedKey::Escape), state: ElementState::Pressed, .. },
                ..
            } => {
                // Esc on the narrow content screen goes back to the list.
                if self.back() {
                    *handled = true;
                    return;
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                if self.back_rect.width() > 0.0 && self.back_rect.contains(self.last_cursor) {
                    self.back_press = true;
                    *handled = true;
                    return;
                }
                // Record which surface the user engaged so a later collapse
                // keeps them there. Not consumed — the press still routes to
                // the child under it.
                if self.content_area_rect.width() > 0.0
                    && self.content_area_rect.contains(self.last_cursor)
                {
                    self.last_active = NarrowScreen::Content;
                } else if self.sidebar_panel_rect.width() > 0.0
                    && self.sidebar_panel_rect.contains(self.last_cursor)
                {
                    self.last_active = NarrowScreen::List;
                }
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                if self.back_press {
                    let on_back = self.back_rect.contains(self.last_cursor);
                    self.back_press = false;
                    if on_back {
                        self.back();
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

        // Route to children. In every state the surfaces are side by side (or
        // only one is present), so a position-carrying event self-selects by
        // rect. We route the content pane before the sidebar so a position-less
        // wheel scrolls the content (matching a split's "content scrolls"
        // behaviour); a sidebar row click still lands because the content pane
        // ignores a click outside its rect.
        let child_nodes = taffy.children(node_id).unwrap_or_default();
        let mut pairs: Vec<(Slot, NodeId)> = Vec::new();
        for (slot, &n) in self.slots.iter().zip(child_nodes.iter()) {
            pairs.push((*slot, n));
        }
        for want in [Slot::Search, Slot::Content, Slot::Nav] {
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

    /// Wide: the sidebar fits, so it is ALWAYS docked — no collapse/expand
    /// toggle exists anywhere, the search is pinned to the top of the sidebar,
    /// and the content fills the area to its right.
    #[test]
    fn sidebar_always_visible_and_no_toggle_when_it_fits() {
        let mut s = shell();
        lay(&mut s, 1000.0, 700.0);
        assert!(!s.is_narrow(), "1000px is wide (the sidebar fits)");
        assert!(s.sidebar_open(), "the fitting sidebar is always shown");
        // No toggle button: the shell draws no collapse affordance in the wide
        // state (the back rect — the only shell-drawn button — is empty).
        assert_eq!(s.back_rect(), Rect::ZERO, "there is no toggle/back button when the sidebar fits");
        // Sidebar flush to the left edge, full height, exactly its width.
        assert_eq!(s.sidebar_panel_rect.x0, 0.0, "sidebar panel is flush left");
        assert!((s.sidebar_panel_rect.height() - 700.0).abs() < 0.5, "panel spans full height");
        assert!(
            (s.sidebar_panel_rect.width() - s.sidebar_width).abs() < 0.5,
            "panel is exactly the sidebar width"
        );
        // Search pinned at the TOP of the sidebar column.
        let sr = search_rect(&s);
        assert!(sr.x0 >= s.sidebar_panel_rect.x0 && sr.x1 <= s.sidebar_panel_rect.x1 + 0.5, "search over the sidebar column");
        assert!(sr.y0 <= s.rect.y0 + PAD + 0.5, "search is pinned to the top of the sidebar");
        // Content begins exactly where the sidebar ends — no gap, no overlap.
        assert!(
            (s.content_area_rect.x0 - s.sidebar_panel_rect.x1).abs() < 0.5,
            "content starts flush against the sidebar"
        );
        assert!(s.content_area_rect.x1 > s.sidebar_panel_rect.x1, "content fills the area to the right");
    }

    /// Narrow collapse keeps the CONTENT on screen, never the category list.
    /// The list is reachable via the back arrow — it is not the default screen.
    #[test]
    fn narrow_collapses_to_content_not_the_list() {
        // Seed the width so the cold frame resolves narrow immediately (as a
        // real narrow window boot does).
        let mut s = shell().with_width_hint(460.0);
        lay(&mut s, 460.0, 820.0);
        assert!(s.is_narrow(), "460px resolves to the narrow layout");
        assert_eq!(
            s.narrow_screen(),
            NarrowScreen::Content,
            "a collapse keeps the content — never defaults to the nav list",
        );
        assert!(!s.sidebar_open(), "the category list is NOT the shown surface");
        // The content fills the whole window.
        assert!((s.content_area_rect.width() - 460.0).abs() < 0.5, "content fills the screen width");
        assert!((s.content_area_rect.height() - 820.0).abs() < 0.5, "content fills the screen height");
        // The list is reachable: a back arrow sits in the top-left.
        assert!(s.back_rect().width() > 0.0, "the content screen offers a back arrow to reach the list");
        // Only the content is composed — the nav/search are off this screen.
        let names: Vec<&str> = s.children().iter().map(|c| c.name()).collect();
        assert_eq!(names, vec!["Text"], "content screen composes only the content");
    }

    /// When the user IS on the list screen (reached via Back / an explicit
    /// hint), it is its OWN full-screen screen — not an overlay, not a scrim
    /// over the content. The content is not even built behind it.
    #[test]
    fn narrow_list_screen_is_standalone_not_an_overlay() {
        let mut s = shell().with_width_hint(460.0).with_narrow_screen(NarrowScreen::List);
        lay(&mut s, 460.0, 820.0);
        assert!(s.is_narrow(), "460px resolves to the narrow layout");
        assert_eq!(s.narrow_screen(), NarrowScreen::List);
        assert!(s.sidebar_open(), "the list screen shows the sidebar");
        // The list fills the whole window.
        assert!((s.sidebar_panel_rect.width() - 460.0).abs() < 0.5, "the list fills the screen width");
        assert!((s.sidebar_panel_rect.height() - 820.0).abs() < 0.5, "the list fills the screen height");
        // No content behind it, no back button.
        assert_eq!(s.content_area_rect, Rect::ZERO, "no content is shown behind the list (no scrim)");
        assert_eq!(s.back_rect(), Rect::ZERO, "the list screen has no back button");
        // Only the search + nav are composed — the content widget is not built.
        let names: Vec<&str> = s.children().iter().map(|c| c.name()).collect();
        assert_eq!(names, vec!["TextInput", "Text"], "list screen composes only search + nav");
    }

    /// Narrow: picking a category navigates the list screen → the content
    /// screen, which shows the content full-screen with a back arrow.
    #[test]
    fn narrow_pick_navigates_to_content_with_back() {
        // Start on the list screen (the user navigated Back to it).
        let mut s = shell().with_narrow_screen(NarrowScreen::List);
        lay(&mut s, 460.0, 820.0);
        assert_eq!(s.narrow_screen(), NarrowScreen::List);
        // The host calls this when a row is picked.
        s.notify_selection();
        assert!(s.take_structure_dirty(), "a narrow selection requests a rebuild");
        lay(&mut s, 460.0, 820.0);
        assert_eq!(s.narrow_screen(), NarrowScreen::Content, "the pick navigates to the content screen");
        assert!(!s.sidebar_open(), "the list is no longer shown");
        assert!((s.content_area_rect.width() - 460.0).abs() < 0.5, "content fills the screen");
        // A back arrow appears in the top-left.
        assert!(s.back_rect().width() > 0.0, "the content screen has a back arrow");
        assert!(s.back_rect().x0 < 60.0 && s.back_rect().y0 < 60.0, "the back arrow is in the top-left");
        // Only the content is composed now.
        let names: Vec<&str> = s.children().iter().map(|c| c.name()).collect();
        assert_eq!(names, vec!["Text"], "content screen composes only the content");
    }

    /// Narrow: the back arrow is the way from the content screen back to the
    /// list, preserving the selection.
    #[test]
    fn back_from_content_returns_to_the_list() {
        let mut s = shell().with_narrow_screen(NarrowScreen::Content);
        lay(&mut s, 460.0, 820.0);
        assert_eq!(s.narrow_screen(), NarrowScreen::Content, "starts on the content screen");
        assert!(s.back_rect().width() > 0.0, "the back arrow is present");
        // A real click on the back arrow.
        let mid = Point::new(
            (s.back_rect().x0 + s.back_rect().x1) / 2.0,
            (s.back_rect().y0 + s.back_rect().y1) / 2.0,
        );
        click(&mut s, mid);
        assert_eq!(s.narrow_screen(), NarrowScreen::List, "back returns to the list screen");
        assert!(s.take_structure_dirty(), "the back navigation requests a rebuild");
        lay(&mut s, 460.0, 820.0);
        assert!(s.sidebar_open(), "the list screen is shown again");
        assert_eq!(s.content_area_rect, Rect::ZERO, "no content behind the list");
    }

    #[test]
    fn esc_from_content_returns_to_the_list() {
        let mut s = shell().with_narrow_screen(NarrowScreen::Content);
        lay(&mut s, 460.0, 820.0);
        assert_eq!(s.narrow_screen(), NarrowScreen::Content);
        let mut handled = false;
        dispatch(&mut s, &esc_event(), &mut handled);
        assert!(handled, "Esc on the content screen is consumed");
        assert_eq!(s.narrow_screen(), NarrowScreen::List, "Esc navigates back to the list");
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
        assert!(s.take_structure_dirty() || s.built_layout == ShellLayout::Narrow);
    }

    #[test]
    fn children_track_the_slot_set() {
        let mut s = shell();
        lay(&mut s, 1000.0, 700.0);
        // Wide: search + nav + content.
        assert_eq!(s.children().len(), 3);
        // Cross to narrow. The first frame after a breakpoint cross rebuilds
        // (the cross is only known once `apply_layout` sees the new width), so
        // settle with a second layout — exactly what the host's frame loop does.
        // The collapse keeps the CONTENT, so only the content is composed.
        lay(&mut s, 460.0, 820.0);
        lay(&mut s, 460.0, 820.0);
        assert_eq!(s.children().len(), 1, "narrow collapse composes only the content");
        // Navigate back to the list screen: search + nav, no content behind.
        assert!(s.back(), "back moves to the list screen");
        lay(&mut s, 460.0, 820.0);
        assert_eq!(s.children().len(), 2, "narrow list composes search + nav (no content behind)");
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
