//! `AppShell` — reusable responsive app-shell: the "sidebar + content +
//! search" chrome a settings-style app needs, one continuous surface
//! (not a scatter of outlined cards).
//!
//! Resolves to exactly one of two states purely from whether the sidebar
//! fits beside the content at the current width — see [`ShellLayout`].
//!
//! Unlike [`crate::widget::NavSplit`]'s isolated inner tree, `AppShell`
//! builds search/nav/content as direct, absolute-positioned children of
//! its own node in the host tree, so the search field stays reachable by
//! `focusable_order` and its caret survives the per-keystroke filter
//! rebuild.

use crate::widget::{AppModel, ModelPath};
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use taffy::style_helpers::{auto, length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::{Metrics, Spacing};
use crate::widget::{AuthoredExtent, Button, TaffyTreeExt, Widget};
use crate::{LocalState, local_state::LocalBinding};

type Element = Box<dyn Widget>;

/// Default docked sidebar width (logical px). Predates
/// `Metrics::sidebar_width` (280) and stays at 264 until the apps built on
/// it are re-measured; a change here moves every `AppShell` app's sidebar.
pub const DEFAULT_SHELL_SIDEBAR_WIDTH: f64 = 264.0;
/// At or above this width (logical px) the sidebar docks; below it the shell
/// switches to the narrow list-vs-content screens. Roughly
/// `sidebar_width + a minimum content column` — the OS-wide
/// `Metrics::compact_breakpoint`.
#[allow(
    clippy::cast_lossless,
    reason = "const context: f64::from is not const, and an f32 -> f64 widening is exact"
)]
pub const DEFAULT_SHELL_BREAKPOINT: f64 = Metrics::BASE.compact_breakpoint as f64;

/// `spacing.s3` at 1×; the live value is read from the theme at build.
#[allow(
    clippy::cast_lossless,
    reason = "const context: f64::from is not const, and an f32 -> f64 widening is exact"
)]
const PAD: f64 = Spacing::BASE.s3 as f64;
/// Search-field slot height at 1×, grown with the body line by
/// `text_control_height` when the tree is built. Neither the 40 px
/// `bar_height` nor the 28 px `bar_control_height`; `NavShell`'s chip is 36.
/// Recorded, not yet unified (the F1 layout review proposes
/// `min_pointer_target` + `s3` inset for both).
const CTRL_H: f64 = 34.0;
/// Icon-only Back button side at 1×; follows `ui_scale`, not the text.
const BACK_BTN: f64 = 34.0;
const BACK_BAND_H: f64 = PAD * 2.0 + BACK_BTN;

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
        if width >= breakpoint {
            ShellLayout::Wide
        } else {
            ShellLayout::Narrow
        }
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

/// Which owned child a taffy slot maps to, so `apply_layout`/`handle_event`/
/// `children()` agree on build order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    Back,
    Search,
    Nav,
    Content,
}

pub struct AppShell {
    nav: Element,
    content: Element,
    search: Option<Element>,
    back: Button,
    back_requested: Rc<Cell<bool>>,
    test_id: Option<String>,
    back_accessible_name: String,
    narrow_screen_binding: Option<LocalBinding<NarrowScreen>>,

    /// The app's inputs as authored at 1× (HIG §9.6, [`AuthoredExtent`]);
    /// a `None` breakpoint is the token default
    /// (`theme.tokens.metrics.compact_breakpoint`).
    authored_sidebar_width: AuthoredExtent,
    authored_breakpoint: Option<AuthoredExtent>,
    /// Resolved against the theme in `build_layout`
    /// ([`Theme::responsive_scale`]); the authored values until then.
    sidebar_width: f64,
    breakpoint: f64,
    /// Chrome metrics resolved against the theme: the `s3` inset, the
    /// search slot grown with the body line, the icon-only Back button
    /// grown with `ui_scale`.
    pad: f64,
    ctrl_h: f64,
    back_btn: f64,

    /// Which narrow screen is showing (narrow only). Preserved across a
    /// wide↔narrow resize so the user returns to where they were.
    narrow_screen: NarrowScreen,
    /// The surface the user most recently engaged. A wide→narrow collapse
    /// seeds [`Self::narrow_screen`] from this — never defaults to the
    /// navigation list.
    last_active: NarrowScreen,
    /// Resolves the layout before the first real frame — the offscreen
    /// `--shots` renderer seeds this for a deterministic cold screenshot.
    width_hint: Option<f64>,

    /// Needs the host to re-run `build_layout`. Drained by `take_structure_dirty`.
    pending_rebuild: bool,

    rect: Rect,
    layout: ShellLayout,
    /// The layout the current taffy tree was *built* for, to detect a
    /// breakpoint cross that needs a structural rebuild.
    built_layout: ShellLayout,
    sidebar_panel_rect: Rect,
    content_area_rect: Rect,
    /// The narrow content screen's back-arrow button (zero-sized otherwise).
    back_rect: Rect,
    /// Narrow chrome grows with text scale so large page headings do not
    /// overlap the Back affordance or its divider.
    back_band_h: f64,
    /// Vertically centres the fixed-size Back target in that band.
    back_button_top: f64,

    /// The order the child nodes were built in, mirrored by `children()`.
    slots: Vec<Slot>,

    last_cursor: Point,
}

impl AppShell {
    /// Add a search field with [`AppShell::with_search`].
    pub fn new(nav: Element, content: Element) -> Self {
        let back_requested = Rc::new(Cell::new(false));
        let back = Self::back_button("app-shell-back", "Back", &back_requested, BACK_BTN);
        Self {
            nav,
            content,
            search: None,
            back,
            back_requested,
            test_id: None,
            back_accessible_name: String::from("Back"),
            narrow_screen_binding: None,
            authored_sidebar_width: AuthoredExtent::text(DEFAULT_SHELL_SIDEBAR_WIDTH),
            authored_breakpoint: None,
            sidebar_width: DEFAULT_SHELL_SIDEBAR_WIDTH,
            breakpoint: DEFAULT_SHELL_BREAKPOINT,
            pad: PAD,
            ctrl_h: CTRL_H,
            back_btn: BACK_BTN,
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
            back_band_h: BACK_BAND_H,
            back_button_top: PAD,
            slots: Vec::new(),
            last_cursor: Point::ZERO,
        }
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "a button side in on-screen pixels, far inside f32's exact range"
    )]
    fn back_button(id: &str, name: &str, requested: &Rc<Cell<bool>>, side: f64) -> Button {
        let requested = Rc::clone(requested);
        Button::new(id, "", None)
            .with_icon(crate::icons::Icon::ArrowLeft)
            .with_accessible_name(name)
            .with_size(side as f32, side as f32)
            .ghost()
            .on_view(move || requested.set(true))
    }

    fn rebuild_back_button(&mut self) {
        let id = self
            .test_id
            .as_deref()
            .map_or_else(|| String::from("app-shell-back"), |id| format!("{id}-back"));
        self.back = Self::back_button(
            &id,
            &self.back_accessible_name,
            &self.back_requested,
            self.back_btn,
        );
    }

    /// Resolve the authored inputs and chrome metrics against `theme`
    /// (HIG §9.6): the text-sized sidebar and breakpoint grow by
    /// [`Theme::responsive_scale`] (the token default is already
    /// `ui_scale`d and grows with `text_growth`), the search slot with the
    /// body line, the icon-only Back button with `ui_scale`. Identity on a
    /// stock theme, so 1× is the authored geometry.
    fn resolve_inputs(&mut self, theme: &Theme) {
        self.sidebar_width = self.authored_sidebar_width.resolve(theme);
        self.breakpoint = self.authored_breakpoint.map_or_else(
            || f64::from(theme.tokens.metrics.compact_breakpoint) * f64::from(theme.text_growth()),
            |extent| extent.resolve(theme),
        );
        self.pad = f64::from(theme.tokens.spacing.s3);
        self.ctrl_h = crate::widget::text_control_height(CTRL_H, theme);
        let back_btn = BACK_BTN * f64::from(theme.ui_scale);
        if back_btn.to_bits() != self.back_btn.to_bits() {
            self.back_btn = back_btn;
            self.rebuild_back_button();
        }
    }

    /// The breakpoint the shell currently resolves against — the authored
    /// (or token) value scaled by the theme it was last built with.
    pub fn breakpoint(&self) -> f64 {
        self.breakpoint
    }

    /// The docked sidebar width currently laid out, resolved like
    /// [`Self::breakpoint`].
    pub fn sidebar_width(&self) -> f64 {
        self.sidebar_width
    }

    /// Height of the band the search slot occupies at the top of the
    /// sidebar / narrow list: inset, control, inset.
    fn search_band_h(&self) -> f64 {
        self.pad + self.ctrl_h + self.pad
    }

    /// Height of the band the narrow Back button occupies.
    fn back_band_h(&self) -> f64 {
        (self.pad + self.back_btn + self.pad).max(self.back_band_h)
    }

    /// Pinned at the top of the sidebar (or the narrow list screen); stays
    /// keyboard-focusable across the filter rebuild.
    #[must_use]
    pub fn with_search(mut self, search: Element) -> Self {
        self.search = Some(search);
        self
    }

    /// Give the shell a stable address for inspection and semantic actions.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.test_id = Some(id.into());
        self.rebuild_back_button();
        self
    }

    /// Set the Fluent key (or literal fallback) announced for compact Back.
    #[must_use]
    pub fn with_back_accessible_name(mut self, name: impl Into<String>) -> Self {
        self.back_accessible_name = name.into();
        self.rebuild_back_button();
        self
    }

    /// Bind compact list/content navigation to typed local presentation state.
    ///
    /// A navigation row can set [`NarrowScreen::Content`] without retaining a
    /// handle to the shell; activating the shell's Back affordance publishes
    /// [`NarrowScreen::List`] through the same state. Wide layouts keep both
    /// panes visible while remembering which one a later collapse should show.
    #[must_use]
    pub fn with_narrow_screen_state(mut self, state: &LocalState<NarrowScreen>) -> Self {
        let (screen, binding) = LocalBinding::new(state);
        self.narrow_screen = screen;
        self.last_active = screen;
        self.narrow_screen_binding = Some(binding);
        self
    }

    /// Docked width of the (text-sized) navigation sidebar, in logical px
    /// **authored at 1×**. The shell multiplies it by
    /// [`Theme::responsive_scale`] when the tree is built, so the app never
    /// restates the text-scale rule. Read it back with
    /// [`Self::sidebar_width`].
    #[must_use]
    pub fn with_sidebar_width(mut self, px: f64) -> Self {
        self.authored_sidebar_width = AuthoredExtent::text(px);
        self.sidebar_width = px;
        self
    }

    /// The width at or above which the sidebar docks, authored at 1× and
    /// scaled like [`Self::with_sidebar_width`]. Default:
    /// `theme.tokens.metrics.compact_breakpoint`.
    #[must_use]
    pub fn with_breakpoint(mut self, px: f64) -> Self {
        self.authored_breakpoint = Some(AuthoredExtent::text(px));
        self.breakpoint = px;
        self
    }

    /// For a deterministic cold `--shots` frame.
    #[must_use]
    pub fn with_width_hint(mut self, px: f64) -> Self {
        self.width_hint = Some(px);
        self.layout = ShellLayout::from_width(px, self.breakpoint);
        self.built_layout = self.layout;
        self
    }

    /// No visible effect when wide. Also seeds the last-interacted surface,
    /// so a later collapse honours this choice.
    #[must_use]
    pub fn with_narrow_screen(mut self, screen: NarrowScreen) -> Self {
        self.set_narrow_screen(screen, true);
        self
    }

    fn set_narrow_screen(&mut self, screen: NarrowScreen, publish: bool) {
        let changed = self.narrow_screen != screen;
        self.narrow_screen = screen;
        self.last_active = screen;
        if changed && self.layout == ShellLayout::Narrow {
            self.pending_rebuild = true;
        }
        if publish && let Some(binding) = &mut self.narrow_screen_binding {
            binding.publish(&screen);
        }
    }

    pub fn set_nav(&mut self, nav: Element) {
        self.nav = nav;
        self.pending_rebuild = true;
    }

    pub fn set_content(&mut self, content: Element) {
        self.content = content;
        self.pending_rebuild = true;
    }

    /// The host calls this after a nav item is chosen. No-op when wide
    /// (both are already visible).
    pub fn notify_selection(&mut self) {
        self.set_narrow_screen(NarrowScreen::Content, true);
    }

    /// Returns whether anything moved (false when wide, or already on the
    /// list).
    pub fn back(&mut self) -> bool {
        if self.layout == ShellLayout::Narrow && self.narrow_screen == NarrowScreen::Content {
            self.set_narrow_screen(NarrowScreen::List, true);
            true
        } else {
            false
        }
    }

    pub fn is_narrow(&self) -> bool {
        self.layout == ShellLayout::Narrow
    }

    /// Meaningful only when [`is_narrow`](AppShell::is_narrow).
    pub fn narrow_screen(&self) -> NarrowScreen {
        self.narrow_screen
    }

    pub fn sidebar_open(&self) -> bool {
        self.sidebar_shown()
    }

    pub fn layout(&self) -> ShellLayout {
        self.layout
    }

    /// Zero-sized when not on the narrow content screen.
    pub fn back_rect(&self) -> Rect {
        self.back_rect
    }

    /// Zero-width when the sidebar is not shown (the narrow content screen).
    pub fn sidebar_panel_rect(&self) -> Rect {
        self.sidebar_panel_rect
    }

    /// Zero-width on the narrow list screen (no content behind it).
    pub fn content_area_rect(&self) -> Rect {
        self.content_area_rect
    }

    fn resolve_layout(&self) -> ShellLayout {
        let w = self.width_hint.unwrap_or_else(|| self.rect.width());
        if w <= 0.0 {
            // No width known yet (very first build): assume wide.
            return ShellLayout::Wide;
        }
        ShellLayout::from_width(w, self.breakpoint)
    }

    fn sidebar_shown(&self) -> bool {
        match self.layout {
            ShellLayout::Wide => true,
            ShellLayout::Narrow => self.narrow_screen == NarrowScreen::List,
        }
    }

    fn content_shown(&self) -> bool {
        match self.layout {
            ShellLayout::Wide => true,
            ShellLayout::Narrow => self.narrow_screen == NarrowScreen::Content,
        }
    }

    /// Kept geometry-only (no theme) so it runs in `apply_layout`.
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
                    self.sidebar_panel_rect = r;
                    self.content_area_rect = Rect::ZERO;
                    self.back_rect = Rect::ZERO;
                }
                NarrowScreen::Content => {
                    self.sidebar_panel_rect = Rect::ZERO;
                    self.content_area_rect = r;
                    self.back_rect = Rect::new(
                        r.x0 + self.pad,
                        r.y0 + self.back_button_top,
                        r.x0 + self.pad + self.back_btn,
                        r.y0 + self.back_button_top + self.back_btn,
                    );
                }
            },
        }
    }

    /// Width-independent where it can be (fixed sidebar width; content uses
    /// left+right insets), so it is resolvable at `build_layout` time before
    /// the real width is known.
    // Every `f64 -> f32` cast below narrows a sidebar/band/pad layout constant
    // or width in on-screen pixels — bounded well inside `f32`'s exact range.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "shell layout dimensions are small on-screen pixel values"
    )]
    fn slot_style(&self, slot: Slot, base: &Style) -> Style {
        let sw = self.sidebar_width;
        let pad = self.pad;
        let mut s = base.clone();
        s.position = taffy::Position::Absolute;
        match slot {
            Slot::Back => {
                s.inset = taffy::Rect {
                    left: length(pad as f32),
                    right: auto(),
                    top: length(self.back_button_top as f32),
                    bottom: auto(),
                };
                s.size = taffy::Size {
                    width: length(self.back_btn as f32),
                    height: length(self.back_btn as f32),
                };
            }
            Slot::Content => {
                let (left, top) = if self.layout == ShellLayout::Wide {
                    (length(sw as f32), length(0.0_f32))
                } else {
                    (length(0.0_f32), length(self.back_band_h() as f32))
                };
                s.inset = taffy::Rect {
                    left,
                    right: length(0.0_f32),
                    top,
                    bottom: length(0.0_f32),
                };
                s.size = taffy::Size {
                    width: auto(),
                    height: auto(),
                };
            }
            Slot::Nav => {
                let right = if self.layout == ShellLayout::Wide {
                    auto()
                } else {
                    length(0.0_f32)
                };
                let width = if self.layout == ShellLayout::Wide {
                    length(sw as f32)
                } else {
                    auto()
                };
                s.inset = taffy::Rect {
                    left: length(0.0_f32),
                    right,
                    top: length(self.search_band_h() as f32),
                    bottom: length(0.0_f32),
                };
                s.size = taffy::Size {
                    width,
                    height: auto(),
                };
            }
            Slot::Search => {
                s.inset = taffy::Rect {
                    left: length(pad as f32),
                    right: if self.layout == ShellLayout::Wide {
                        auto()
                    } else {
                        length(pad as f32)
                    },
                    top: length(pad as f32),
                    bottom: auto(),
                };
                let width = if self.layout == ShellLayout::Wide {
                    length((sw - 2.0 * pad) as f32)
                } else {
                    auto()
                };
                s.size = taffy::Size {
                    width,
                    height: length(self.ctrl_h as f32),
                };
            }
        }
        s
    }

    /// Shared by `draw` and `draw_with_presence`; `permissions` selects the
    /// presence-aware child draw path.
    fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        permissions: Option<&crate::permissions::PermissionContext>,
    ) {
        // Each region is its own surface tier (HIG §9.2): the navigation
        // column the sidebar surface, the rest the window ground. The
        // divider derives from the sidebar surface (HIG §9.5).
        let sidebar_fill = theme.tokens.colors.surfaces.sidebar;
        let content_fill = theme.tokens.colors.surfaces.content;
        let divider = crate::surface::edge_on(sidebar_fill);

        let narrow_list =
            self.layout == ShellLayout::Narrow && self.narrow_screen == NarrowScreen::List;

        let base = if narrow_list {
            sidebar_fill
        } else {
            content_fill
        };
        scene.fill(Fill::NonZero, transform, base, None, &self.rect);

        if self.content_shown() {
            paint_child(
                self.content.as_mut(),
                scene,
                transform,
                tr,
                theme,
                presence,
                permissions,
            );
        }

        if self.layout == ShellLayout::Wide {
            scene.fill(
                Fill::NonZero,
                transform,
                sidebar_fill,
                None,
                &self.sidebar_panel_rect,
            );
            let x = self.sidebar_panel_rect.x1 - 0.5;
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                divider,
                None,
                &Line::new(Point::new(x, self.rect.y0), Point::new(x, self.rect.y1)),
            );
        }

        if self.sidebar_shown() {
            paint_child(
                self.nav.as_mut(),
                scene,
                transform,
                tr,
                theme,
                presence,
                permissions,
            );
            if let Some(search) = &mut self.search {
                paint_child(
                    search.as_mut(),
                    scene,
                    transform,
                    tr,
                    theme,
                    presence,
                    permissions,
                );
            }
        }

        if self.layout == ShellLayout::Narrow && self.narrow_screen == NarrowScreen::Content {
            let y = self.rect.y0 + self.back_band_h() - 0.5;
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                crate::surface::edge_on(content_fill),
                None,
                &Line::new(Point::new(self.rect.x0, y), Point::new(self.rect.x1, y)),
            );
            paint_child(
                &mut self.back,
                scene,
                transform,
                tr,
                theme,
                presence,
                permissions,
            );
        }
    }
}

/// Free function (not a method) so it borrows none of `AppShell`'s fields —
/// the caller keeps a live `&mut` on the child.
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
    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        if self.back_requested.replace(false) {
            self.back();
        }
        let next_screen = self
            .narrow_screen_binding
            .as_mut()
            .and_then(LocalBinding::poll);
        if let Some(screen) = next_screen {
            self.set_narrow_screen(screen, false);
        }
        if let Some(search) = &mut self.search {
            search.sync(state, path);
        }
        self.back.sync(state, path);
        self.nav.sync(state, path);
        self.content.sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.resolve_inputs(theme);
        self.layout = self.resolve_layout();
        self.built_layout = self.layout;
        self.back_band_h = (BACK_BAND_H * f64::from(theme.text_scale)).max(BACK_BAND_H);
        self.back_button_top = (self.back_band_h - BACK_BTN).max(0.0) / 2.0;

        // ABSOLUTE-positioned direct nodes stay in the host tree (and thus
        // keyboard-focusable); each is then pinned to its slot rect.
        self.slots.clear();
        let mut child_nodes: Vec<NodeId> = Vec::new();

        if self.layout == ShellLayout::Narrow && self.narrow_screen == NarrowScreen::Content {
            let node = self.back.build_layout(taffy, theme, node_map);
            let base = taffy.style(node).cloned().unwrap_or_default();
            let _ = taffy.set_style(node, self.slot_style(Slot::Back, &base));
            self.slots.push(Slot::Back);
            child_nodes.push(node);
        }
        if self.sidebar_shown() {
            if let Some(search) = &mut self.search {
                let node = search.build_layout(taffy, theme, node_map);
                let base = taffy.style(node).cloned().unwrap_or_default();
                let _ = taffy.set_style(node, self.slot_style(Slot::Search, &base));
                self.slots.push(Slot::Search);
                child_nodes.push(node);
            }
            let node = self.nav.build_layout(taffy, theme, node_map);
            let base = taffy.style(node).cloned().unwrap_or_default();
            let _ = taffy.set_style(node, self.slot_style(Slot::Nav, &base));
            self.slots.push(Slot::Nav);
            child_nodes.push(node);
        }
        if self.content_shown() {
            let node = self.content.build_layout(taffy, theme, node_map);
            let base = taffy.style(node).cloned().unwrap_or_default();
            let _ = taffy.set_style(node, self.slot_style(Slot::Content, &base));
            self.slots.push(Slot::Content);
            child_nodes.push(node);
        }

        let node = taffy.parent_node(
            Style {
                display: taffy::Display::Flex,
                size: taffy::Size {
                    width: percent(1.0_f32),
                    height: percent(1.0_f32),
                },
                ..Default::default()
            },
            &child_nodes,
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );

        // A breakpoint cross since this tree was built needs a rebuild so the
        // child set / insets match the new layout.
        let now = ShellLayout::from_width(self.rect.width(), self.breakpoint);
        if now != self.built_layout {
            self.pending_rebuild = true;
            // A wide→narrow collapse can only show one surface — seed it from
            // the last-interacted one, never the category list by default.
            if now == ShellLayout::Narrow {
                self.narrow_screen = self.last_active;
            }
        }
        // Once we have a real rect, stop leaning on the seed hint.
        self.width_hint = None;
        self.layout = now;
        self.recompute_rects();

        let child_nodes = taffy.children(node).unwrap_or_default();
        let child_origin = Point::new(self.rect.x0, self.rect.y0);
        for (slot, &child_node) in self.slots.iter().zip(child_nodes.iter()) {
            let w: &mut dyn Widget = match slot {
                Slot::Back => &mut self.back,
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
        self.paint(
            scene,
            transform,
            text_renderer,
            theme,
            presence,
            Some(permissions),
        );
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Escape),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                // Esc on the narrow content screen goes back to the list.
                if self.back() {
                    *ctx.handled = true;
                    return;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                // Record which surface the user engaged so a later collapse
                // keeps them there. Not consumed — the press still routes to
                // the child under it.
                if self.content_area_rect.width() > 0.0
                    && self.content_area_rect.contains(self.last_cursor)
                {
                    self.set_narrow_screen(NarrowScreen::Content, true);
                } else if self.sidebar_panel_rect.width() > 0.0
                    && self.sidebar_panel_rect.contains(self.last_cursor)
                {
                    self.set_narrow_screen(NarrowScreen::List, true);
                }
            }
            _ => {}
        }
        if *ctx.handled {
            return;
        }

        // Mouse button/wheel events carry no position in winit; route those
        // through the surface selected by the latest CursorMoved instead of
        // offering them to an unrelated pane first. This matters for controls
        // that retain hover/press state: a content control must not consume a
        // release meant for a sidebar row, and a content wheel must reach its
        // Scrollable rather than the sidebar list.
        let child_nodes = ctx.taffy.children(ctx.node_id).unwrap_or_default();
        let mut pairs: Vec<(Slot, NodeId)> = Vec::new();
        for (slot, &n) in self.slots.iter().zip(child_nodes.iter()) {
            pairs.push((*slot, n));
        }
        let pointer_order = if self.sidebar_panel_rect.contains(self.last_cursor) {
            [Slot::Search, Slot::Nav, Slot::Back, Slot::Content]
        } else if self.content_area_rect.contains(self.last_cursor) {
            [Slot::Content, Slot::Back, Slot::Search, Slot::Nav]
        } else {
            [Slot::Back, Slot::Search, Slot::Content, Slot::Nav]
        };
        let order = match event {
            WindowEvent::MouseInput { .. }
            | WindowEvent::MouseWheel { .. }
            | WindowEvent::TouchpadPressure { .. }
            | WindowEvent::Touch(_) => pointer_order,
            _ => [Slot::Back, Slot::Search, Slot::Content, Slot::Nav],
        };
        for want in order {
            if *ctx.handled {
                break;
            }
            if let Some(&(_, n)) = pairs.iter().find(|(s, _)| *s == want) {
                let w: &mut dyn Widget = match want {
                    Slot::Back => &mut self.back,
                    Slot::Search => match &mut self.search {
                        Some(s) => s.as_mut(),
                        None => continue,
                    },
                    Slot::Nav => self.nav.as_mut(),
                    Slot::Content => self.content.as_mut(),
                };
                w.handle_event(event, &mut ctx.reborrow(ctx.path, n));
                if self.back_requested.replace(false) {
                    self.back();
                    *ctx.handled = true;
                }
            }
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut out: Vec<&dyn Widget> = Vec::new();
        for slot in &self.slots {
            match slot {
                Slot::Back => out.push(&self.back),
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

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        for slot in &self.slots {
            match slot {
                Slot::Back => visitor(&self.back),
                Slot::Search => {
                    if let Some(search) = &self.search {
                        visitor(search.as_ref());
                    }
                }
                Slot::Nav => visitor(self.nav.as_ref()),
                Slot::Content => visitor(self.content.as_ref()),
            }
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        // Borrow each field at most once, in slot order.
        let mut search = self.search.as_mut();
        let mut back = Some(&mut self.back);
        let mut nav = Some(&mut self.nav);
        let mut content = Some(&mut self.content);
        let mut out: Vec<&mut (dyn Widget + '_)> = Vec::new();
        for slot in &self.slots {
            match slot {
                Slot::Back => {
                    if let Some(b) = back.take() {
                        out.push(b);
                    }
                }
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

    fn name(&self) -> &'static str {
        "AppShell"
    }

    fn test_id(&self) -> Option<&str> {
        self.test_id.as_deref()
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn take_structure_dirty(&mut self) -> bool {
        if self.back_requested.replace(false) {
            self.back();
        }
        let own = std::mem::take(&mut self.pending_rebuild);
        let back = self.back.take_structure_dirty();
        let search = self
            .search
            .as_mut()
            .is_some_and(|s| s.take_structure_dirty());
        let nav = self.nav.take_structure_dirty();
        let content = self.content.take_structure_dirty();
        own || back || search || nav || content
    }

    fn needs_animation_frame(&self) -> bool {
        self.content.needs_animation_frame()
            || self.back.needs_animation_frame()
            || self.nav.needs_animation_frame()
            || self
                .search
                .as_ref()
                .is_some_and(|s| s.needs_animation_frame())
    }

    fn needs_idle_sync(&self) -> bool {
        self.content.needs_idle_sync()
            || self.back.needs_idle_sync()
            || self.nav.needs_idle_sync()
            || self.search.as_ref().is_some_and(|s| s.needs_idle_sync())
    }

    fn layout_needs_measure(&self) -> bool {
        self.content.layout_needs_measure()
            || self.back.layout_needs_measure()
            || self.nav.layout_needs_measure()
            || self
                .search
                .as_ref()
                .is_some_and(|s| s.layout_needs_measure())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::{Text, TextInput};
    use automerge::AutoCommit;
    use vello::kurbo::Affine;
    use vello_encoding::DrawTag;

    fn shell() -> AppShell {
        AppShell::new(Box::new(Text::new("nav")), Box::new(Text::new("content")))
            .with_search(Box::new(TextInput::new(Vec::new()).with_id("search")))
    }

    // Test fixture widths/heights are small literal px values, well inside
    // `f32`'s exact-integer range.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "test fixture dimensions are small literal px values"
    )]
    fn lay(s: &mut AppShell, w: f64, h: f64) {
        lay_with(s, &Theme::dark(), w, h);
    }

    fn lay_with(s: &mut AppShell, theme: &Theme, w: f64, h: f64) {
        let mut taffy = TaffyTree::<()>::new();
        let mut map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = s.build_layout(&mut taffy, theme, &mut map);
        let mm = map.clone();
        let mut tr = TextRenderer::new();
        tr.set_text_scale(theme.text_scale);
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
    fn wide_shell_breakpoint_scale_and_sidebar() {
        fn breakpoint_boundary_is_inclusive_wide() {
            assert_eq!(ShellLayout::from_width(720.0, 720.0), ShellLayout::Wide);
            assert_eq!(ShellLayout::from_width(719.0, 720.0), ShellLayout::Narrow);
        }

        /// HIG §9.6: the sidebar width and breakpoint are authored at 1× and
        /// resolved against the theme (`× ui_scale × text_scale.max(1.0)`); the
        /// default breakpoint is the `ui_scale`d `compact_breakpoint` token; the
        /// search slot grows with the body line and the icon-only Back button
        /// with `ui_scale` alone. 1× is exactly the authored geometry.
        fn shell_inputs_and_chrome_follow_the_responsive_scale() {
            let authored = || {
                AppShell::new(Box::new(Text::new("nav")), Box::new(Text::new("content")))
                    .with_search(Box::new(Text::new("search")))
                    .with_sidebar_width(264.0)
                    .with_breakpoint(720.0)
            };
            let mut one = authored();
            lay(&mut one, 1000.0, 600.0);
            assert_eq!((one.breakpoint(), one.sidebar_width()), (720.0, 264.0));
            assert_eq!((one.pad, one.ctrl_h, one.back_btn), (12.0, 34.0, 34.0));
            assert_eq!(one.search_band_h(), 58.0);
            assert_eq!(search_rect(&one).height(), 34.0);

            let two = Theme::dark().with_text_scale(2.0);
            let mut large = authored();
            lay_with(&mut large, &two, 1000.0, 600.0);
            assert_eq!((large.breakpoint(), large.sidebar_width()), (1440.0, 528.0));
            assert_eq!(
                large.layout,
                ShellLayout::Narrow,
                "1000 px at 2× is a 500 px window at 1×"
            );
            let grown = crate::widget::text_control_height(CTRL_H, &two);
            assert!(grown > CTRL_H && grown < 2.0 * CTRL_H);
            assert_eq!(
                large.ctrl_h, grown,
                "the search slot grows with the body line"
            );
            assert_eq!(
                large.back_btn, 34.0,
                "the icon-only Back button is not text-sized"
            );
            assert!(
                (search_rect(&large).height() - grown).abs() <= 0.5,
                "the slot style carries the grown height (taffy rounds to whole pixels): {} vs {grown}",
                search_rect(&large).height()
            );

            let mut defaults =
                AppShell::new(Box::new(Text::new("nav")), Box::new(Text::new("content")));
            lay_with(
                &mut defaults,
                &Theme::dark().with_ui_scale(1.5),
                1400.0,
                600.0,
            );
            assert!(
                (defaults.breakpoint() - 1080.0).abs() < 1e-6,
                "compact_breakpoint at ui 1.5"
            );
            assert!(
                (defaults.sidebar_width() - 396.0).abs() < 1e-6,
                "264 authored × ui 1.5"
            );
            assert!(
                (defaults.back_btn - 51.0).abs() < 1e-6,
                "the Back button follows ui_scale"
            );
            assert!((defaults.pad - 18.0).abs() < 1e-6, "s3 at ui 1.5");
        }

        /// Wide: the sidebar is ALWAYS docked — no collapse/expand toggle exists
        /// anywhere.
        fn sidebar_always_visible_and_no_toggle_when_it_fits() {
            let mut s = shell();
            lay(&mut s, 1000.0, 700.0);
            assert!(!s.is_narrow(), "1000px is wide (the sidebar fits)");
            assert!(s.sidebar_open(), "the fitting sidebar is always shown");
            assert_eq!(
                s.back_rect(),
                Rect::ZERO,
                "there is no toggle/back button when the sidebar fits"
            );
            assert_eq!(s.sidebar_panel_rect.x0, 0.0, "sidebar panel is flush left");
            assert!(
                (s.sidebar_panel_rect.height() - 700.0).abs() < 0.5,
                "panel spans full height"
            );
            assert!(
                (s.sidebar_panel_rect.width() - s.sidebar_width).abs() < 0.5,
                "panel is exactly the sidebar width"
            );
            let sr = search_rect(&s);
            assert!(
                sr.x0 >= s.sidebar_panel_rect.x0 && sr.x1 <= s.sidebar_panel_rect.x1 + 0.5,
                "search over the sidebar column"
            );
            assert!(
                sr.y0 <= s.rect.y0 + PAD + 0.5,
                "search is pinned to the top of the sidebar"
            );
            assert!(
                (s.content_area_rect.x0 - s.sidebar_panel_rect.x1).abs() < 0.5,
                "content starts flush against the sidebar"
            );
            assert!(
                s.content_area_rect.x1 > s.sidebar_panel_rect.x1,
                "content fills the area to the right"
            );
        }

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

        fn children_track_the_slot_set() {
            let mut s = shell();
            lay(&mut s, 1000.0, 700.0);
            // Wide: search + nav + content.
            assert_eq!(s.children().len(), 3);
            // The cross is only known once `apply_layout` sees the new width, so
            // the first frame after it still rebuilds — settle with a second
            // layout, exactly what the host's frame loop does.
            lay(&mut s, 460.0, 820.0);
            lay(&mut s, 460.0, 820.0);
            assert_eq!(
                s.children().len(),
                2,
                "narrow collapse composes Back + content"
            );
            assert!(s.back(), "back moves to the list screen");
            lay(&mut s, 460.0, 820.0);
            assert_eq!(
                s.children().len(),
                2,
                "narrow list composes search + nav (no content behind)"
            );
        }

        fn draw_emits_content() {
            let mut s = shell();
            lay(&mut s, 1000.0, 700.0);
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let presence = PresenceCtx::default();
            let perms = crate::permissions::PermissionContext::permissive();
            let mut scene = Scene::new();
            s.draw_with_presence(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                &presence,
                &perms,
            );
            let fills = scene
                .encoding()
                .draw_tags
                .iter()
                .filter(|t| **t == DrawTag::COLOR)
                .count();
            assert!(fills >= 2, "a laid-out shell paints its surfaces + content");
        }

        breakpoint_boundary_is_inclusive_wide();
        shell_inputs_and_chrome_follow_the_responsive_scale();
        sidebar_always_visible_and_no_toggle_when_it_fits();
        resize_across_the_breakpoint_requests_a_rebuild();
        children_track_the_slot_set();
        draw_emits_content();
    }

    #[test]
    fn narrow_shell_navigates_between_list_and_content() {
        /// Narrow collapse keeps the CONTENT on screen, never the category list.
        /// The list is reachable via the back arrow — it is not the default screen.
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
            assert!(
                !s.sidebar_open(),
                "the category list is NOT the shown surface"
            );
            assert!(
                (s.content_area_rect.width() - 460.0).abs() < 0.5,
                "content fills the screen width"
            );
            assert!(
                (s.content_area_rect.height() - 820.0).abs() < 0.5,
                "content fills the screen height"
            );
            assert!(
                s.back_rect().width() > 0.0,
                "the content screen offers a back arrow to reach the list"
            );
            let names: Vec<&str> = s.children().iter().map(|c| c.name()).collect();
            assert_eq!(
                names,
                vec!["Button", "Text"],
                "content screen composes the standard Back control before content"
            );
        }

        fn narrow_back_band_grows_for_large_text() {
            let mut s = shell().with_width_hint(460.0);
            let theme = Theme::dark().with_text_scale(2.0);
            let mut taffy = TaffyTree::<()>::new();
            let mut map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let _ = s.build_layout(&mut taffy, &theme, &mut map);

            assert_eq!(s.layout, ShellLayout::Narrow);
            assert_eq!(s.back_band_h, BACK_BAND_H * 2.0);
            assert_eq!(
                s.back_button_top,
                (s.back_band_h - BACK_BTN) / 2.0,
                "Back remains centred in the expanded accessibility band"
            );
        }

        /// The list screen is its OWN full-screen screen — not an overlay or
        /// scrim; the content is not even built behind it.
        fn narrow_list_screen_is_standalone_not_an_overlay() {
            let mut s = shell()
                .with_width_hint(460.0)
                .with_narrow_screen(NarrowScreen::List);
            lay(&mut s, 460.0, 820.0);
            assert!(s.is_narrow(), "460px resolves to the narrow layout");
            assert_eq!(s.narrow_screen(), NarrowScreen::List);
            assert!(s.sidebar_open(), "the list screen shows the sidebar");
            assert!(
                (s.sidebar_panel_rect.width() - 460.0).abs() < 0.5,
                "the list fills the screen width"
            );
            assert!(
                (s.sidebar_panel_rect.height() - 820.0).abs() < 0.5,
                "the list fills the screen height"
            );
            assert_eq!(
                s.content_area_rect,
                Rect::ZERO,
                "no content is shown behind the list (no scrim)"
            );
            assert_eq!(
                s.back_rect(),
                Rect::ZERO,
                "the list screen has no back button"
            );
            let names: Vec<&str> = s.children().iter().map(|c| c.name()).collect();
            assert_eq!(
                names,
                vec!["TextInput", "Text"],
                "list screen composes only search + nav"
            );
        }

        /// Narrow: picking a category navigates the list screen → the content
        /// screen, which shows the content full-screen with a back arrow.
        fn narrow_pick_navigates_to_content_with_back() {
            // Start on the list screen (the user navigated Back to it).
            let mut s = shell().with_narrow_screen(NarrowScreen::List);
            lay(&mut s, 460.0, 820.0);
            assert_eq!(s.narrow_screen(), NarrowScreen::List);
            s.notify_selection(); // the host calls this when a row is picked
            assert!(
                s.take_structure_dirty(),
                "a narrow selection requests a rebuild"
            );
            lay(&mut s, 460.0, 820.0);
            assert_eq!(
                s.narrow_screen(),
                NarrowScreen::Content,
                "the pick navigates to the content screen"
            );
            assert!(!s.sidebar_open(), "the list is no longer shown");
            assert!(
                (s.content_area_rect.width() - 460.0).abs() < 0.5,
                "content fills the screen"
            );
            assert!(
                s.back_rect().width() > 0.0,
                "the content screen has a back arrow"
            );
            assert!(
                s.back_rect().x0 < 60.0 && s.back_rect().y0 < 60.0,
                "the back arrow is in the top-left"
            );
            let names: Vec<&str> = s.children().iter().map(|c| c.name()).collect();
            assert_eq!(
                names,
                vec!["Button", "Text"],
                "content screen composes the standard Back control before content"
            );
        }

        fn bound_compact_navigation_and_accessible_back_share_one_state() {
            let screen = LocalState::new(NarrowScreen::List);
            let mut shell = shell()
                .with_id("settings-shell")
                .with_back_accessible_name("Go back")
                .with_narrow_screen_state(&screen)
                .with_width_hint(460.0);
            lay(&mut shell, 460.0, 820.0);
            assert_eq!(shell.narrow_screen(), NarrowScreen::List);

            screen.set(NarrowScreen::Content);
            let mut document = AutoCommit::new();
            shell.sync(&mut document, &automerge::ROOT);
            assert!(shell.take_structure_dirty());
            lay(&mut shell, 460.0, 820.0);

            assert_eq!(shell.back.test_id(), Some("settings-shell-back"));
            let back = shell.back.accessibility().expect("retained Back button");
            assert_eq!(back.role, crate::a11y::Role::Button);
            assert_eq!(back.name.as_deref(), Some("Go back"));
            assert!(shell.back.perform_action(
                &crate::a11y::Action::Activate,
                &mut document,
                &automerge::ROOT,
            ));
            dispatch_view_actions(&mut shell, &mut document);
            assert!(shell.take_structure_dirty());
            assert_eq!(screen.with(Clone::clone), NarrowScreen::List);
        }

        fn headless_cold_compact_shell_is_structurally_settled() {
            let app = shell().with_id("cold-shell");
            let mut driver =
                crate::headless::Driver::new(crate::State::new(), Box::new(app), (460, 820));

            driver
                .expect(
                    &crate::headless::SemanticExpectation::named("cold-shell-back")
                        .role(crate::a11y::Role::Button)
                        .name("Back"),
                )
                .expect("the first observable compact frame includes retained Back");
            assert!(
                driver
                    .focus_access_nodes()
                    .iter()
                    .any(|node| node.name.as_deref() == Some("Back")),
                "the settled Back control is in keyboard focus order"
            );
        }

        /// Narrow: the back arrow is the way from the content screen back to the
        /// list, preserving the selection.
        fn back_from_content_returns_to_the_list() {
            let mut s = shell()
                .with_narrow_screen(NarrowScreen::Content)
                .with_width_hint(460.0);
            lay(&mut s, 460.0, 820.0);
            assert_eq!(
                s.narrow_screen(),
                NarrowScreen::Content,
                "starts on the content screen"
            );
            assert!(s.back_rect().width() > 0.0, "the back arrow is present");
            // A real click on the back arrow.
            let mid = Point::new(
                f64::midpoint(s.back_rect().x0, s.back_rect().x1),
                f64::midpoint(s.back_rect().y0, s.back_rect().y1),
            );
            click(&mut s, mid);
            assert!(
                s.take_structure_dirty(),
                "the retained Back activation requests a rebuild"
            );
            assert_eq!(
                s.narrow_screen(),
                NarrowScreen::List,
                "back returns to the list screen"
            );
            lay(&mut s, 460.0, 820.0);
            assert!(s.sidebar_open(), "the list screen is shown again");
            assert_eq!(
                s.content_area_rect,
                Rect::ZERO,
                "no content behind the list"
            );
        }

        fn esc_from_content_returns_to_the_list() {
            let mut s = shell().with_narrow_screen(NarrowScreen::Content);
            lay(&mut s, 460.0, 820.0);
            assert_eq!(s.narrow_screen(), NarrowScreen::Content);
            let mut handled = false;
            dispatch(&mut s, &esc_event(), &mut handled);
            assert!(handled, "Esc on the content screen is consumed");
            assert_eq!(
                s.narrow_screen(),
                NarrowScreen::List,
                "Esc navigates back to the list"
            );
        }

        narrow_collapses_to_content_not_the_list();
        narrow_back_band_grows_for_large_text();
        narrow_list_screen_is_standalone_not_an_overlay();
        narrow_pick_navigates_to_content_with_back();
        bound_compact_navigation_and_accessible_back_share_one_state();
        headless_cold_compact_shell_is_structurally_settled();
        back_from_content_returns_to_the_list();
        esc_from_content_returns_to_the_list();
    }

    use winit::event::DeviceId;
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
    fn left_release() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: synth_device(),
            state: ElementState::Released,
            button: MouseButton::Left,
        }
    }
    fn esc_event() -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(NamedKey::Escape);
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
    fn dispatch(s: &mut AppShell, event: &WindowEvent, handled: &mut bool) {
        // A throwaway taffy tree with a matching child count so routing works.
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = s.build_layout(&mut taffy, &theme, &mut map);
        let mut focus = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        s.handle_event(
            event,
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state: &mut doc,
                path: &root,
                handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
    }
    fn click(s: &mut AppShell, at: Point) {
        let mut handled = false;
        dispatch(s, &cursor_at(at), &mut handled);
        let mut handled = false;
        dispatch(s, &left_press(), &mut handled);
        let mut handled = false;
        dispatch(s, &left_release(), &mut handled);
        dispatch_view_actions(s, &mut AutoCommit::new());
    }

    fn dispatch_view_actions(s: &mut AppShell, doc: &mut AutoCommit) {
        while let Some(intent) = crate::widget::controls::take_control_invocation(s) {
            let mut resources = Vec::new();
            crate::widget::collect_widget_resources(s, doc, &mut resources);
            let encoded =
                tilo_actions::door::encode_json(&resources, &intent.address, &intent.arguments)
                    .expect("AppShell Back exports the action it emits");
            assert!(
                s.start_operation(
                    &crate::widget::ResourceOperation::local(
                        tilo_actions::InvocationId::new(),
                        tilo_actions::OperationKind::View,
                        &encoded.type_id,
                        &encoded.payload
                    ),
                    doc,
                    &automerge::ROOT
                )
                .expect("Back view action succeeds")
                .is_some()
            );
        }
    }
}
