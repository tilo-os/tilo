//! `PlacesSidebar` — the modern Favourites / Places list down the left of the
//! file explorer. A single widget that renders both sections (user
//! favourites + System) and owns the drag-to-reorder interaction for the
//! favourites via the shared [`ui::widget::drag::DragController`] (live
//! floating ghost + reflow).
//!
//! Because the file explorer rebuilds its whole child tree each frame, the
//! reorder state can't live in the (throwaway) widget — it lives in a shared
//! [`FavStore`] the app owns and hands a clone of to each freshly-built
//! sidebar. The `DragController` (and the favourites `Vec`) therefore survive
//! the per-frame rebuild, so a drag started on one frame keeps reflowing on the
//! next. A plain click navigates; a drag past the threshold reorders on drop.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use ui::widget::{AppModel, ModelPath};

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::command::{CommandSink, FileCommand, push};
use crate::favorites::Favorite;
use crate::i18n::t;
use ui::chrome::{SelectionState, panel_shadow, selection_tier};
use ui::icons::{Icon, LUCIDE_DEFAULT_STROKE_PX, draw_icon};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::spacing::Spacing;
use ui::widget::Widget;
use ui::widget::drag::{DragController, ListMetrics};

/// Sidebar width in logical px (kept equal to the historical value so parity
/// tests + layout expectations hold).
pub const SIDEBAR_WIDTH: f64 = 200.0;

// One rhythm for the sidebar (HIG §7b): the panel gutter and every inset
// inside a row are `s3`; a section is separated from the previous one by
// `s3` as well; the icon and its label are `s2` apart; adjacent row pills
// keep an `s1` gap between them (half an `s1` off each row's top and bottom).
const PAD: f64 = Spacing::BASE.s3 as f64;
const TOP: f64 = Spacing::BASE.s3 as f64;
const HEADER_H: f64 = 26.0;
const ROW_H: f64 = 34.0;
const SECTION_GAP: f64 = Spacing::BASE.s3 as f64;
const ICON_PX: f64 = 17.0;
/// Inset from a row's left edge to its icon. The section captions share it,
/// so "FAVOURITES" / "SYSTEM" sit on the icons' left edge instead of 8px
/// outside it.
const ROW_CONTENT_INSET: f64 = Spacing::BASE.s3 as f64;
const ICON_LABEL_GAP: f64 = Spacing::BASE.s2 as f64;
const PILL_INSET_Y: f64 = Spacing::BASE.s1 as f64 / 2.0;

/// The laid-out geometry of the favourites band — written by the sidebar in
/// `apply_layout` so the app (and tests / the shots demo) can synthesise a
/// drag in the exact coordinate space the reflow uses.
#[derive(Clone, Copy, Debug)]
pub struct FavBand {
    pub origin_y: f64,
    pub row_h: f64,
    pub x0: f64,
    pub x1: f64,
    pub count: usize,
}

impl FavBand {
    pub fn metrics(&self) -> ListMetrics {
        ListMetrics::uniform(self.origin_y, self.row_h, 0.0, self.count)
    }
    pub fn center_x(&self) -> f64 {
        f64::midpoint(self.x0, self.x1)
    }
    /// Vertical centre of favourite row `i`.
    pub fn row_center_y(&self, i: usize) -> f64 {
        self.origin_y + i as f64 * self.row_h + self.row_h / 2.0
    }
}

/// Shared, persistent reorder + favourites state the app owns and every
/// per-frame [`PlacesSidebar`] borrows.
pub struct FavState {
    pub favorites: Vec<Favorite>,
    pub drag: DragController<usize>,
    /// The favourite index a press landed on (click-vs-drag disambiguation).
    pub pressed: Option<usize>,
    /// Latest laid-out favourites geometry.
    pub band: Option<FavBand>,
    /// Keyboard cursor and focus live with the persistent store because the
    /// sidebar widget itself is rebuilt when Files' structure changes.
    pub keyboard_index: Option<usize>,
    pub focused: bool,
}

impl FavState {
    pub fn new(favorites: Vec<Favorite>) -> Self {
        Self {
            favorites,
            drag: DragController::vertical(),
            pressed: None,
            band: None,
            keyboard_index: None,
            focused: false,
        }
    }
}

/// The variable part of a row's paint — bundled so [`PlacesSidebar::draw_row`]
/// stays under clippy's argument-count limit.
#[derive(Clone, Copy)]
struct RowVisual<'a> {
    icon: Icon,
    label: &'a str,
    active: bool,
    hovered: bool,
    ghost: bool,
}

pub type FavStore = Rc<RefCell<FavState>>;

/// Build a shared favourites store.
pub fn new_store(favorites: Vec<Favorite>) -> FavStore {
    Rc::new(RefCell::new(FavState::new(favorites)))
}

pub struct PlacesSidebar {
    favs: FavStore,
    active: PathBuf,
    system: Vec<(Icon, String, PathBuf)>,
    /// Status badges of system places (a remote account's health).
    badges: HashMap<PathBuf, String>,
    sink: CommandSink,
    rect: Rect,
    text_scale: f64,
    last_cursor: Point,
    hover_fav: Option<usize>,
    hover_sys: Option<usize>,
}

impl PlacesSidebar {
    pub fn new(favs: FavStore, active: PathBuf, sink: CommandSink) -> Self {
        // System places: the filesystem root, then the Trash (when the platform
        // exposes one). Deleting moves files here; clicking it opens the trash.
        // Recent leads the fixed places: what you used last, from any folder.
        let mut system = vec![
            (
                Icon::History,
                t("files-place-recent"),
                crate::recent::location(),
            ),
            (
                Icon::HardDrive,
                t("files-place-root"),
                crate::breadcrumbs::filesystem_root(&active),
            ),
        ];
        if let Some(trash) = crate::fileops::trash_dir() {
            system.push((Icon::Trash2, t("files-place-trash"), trash));
        }
        Self {
            favs,
            active,
            system,
            badges: HashMap::new(),
            sink,
            rect: Rect::ZERO,
            text_scale: 1.0,
            last_cursor: Point::ZERO,
            hover_fav: None,
            hover_sys: None,
        }
    }

    /// Surface the Tilo collaborative-workspace mounts as first-class System
    /// locations, above Root. Each root the FUSE driver mounts (e.g.
    /// `/var/workspace`) becomes a clickable place with a cloud emblem, so the
    /// user reaches their synced workspace without typing a path.
    /// Seed the pointer position the host last saw. Like the file view, this
    /// widget is rebuilt every frame and hit-tests presses against its own
    /// `last_cursor`; without seeding, a press arriving with no `CursorMoved`
    /// since the rebuild would test against `(0, 0)` and miss the row.
    #[must_use]
    pub fn with_cursor(mut self, cursor: Point) -> Self {
        self.last_cursor = cursor;
        self
    }

    #[must_use]
    pub fn with_workspaces(mut self, roots: Vec<PathBuf>) -> Self {
        let mut entries: Vec<(Icon, String, PathBuf)> = roots
            .into_iter()
            .map(|p| {
                // Shared with the breadcrumb, which names the same place.
                let label = crate::model::workspace_label(&p);
                (Icon::CloudDownload, label, p)
            })
            .collect();
        // Workspaces lead the System section; Root (and any future devices)
        // follow.
        entries.append(&mut self.system);
        self.system = entries;
        self
    }

    /// Surface mounted cloud accounts (`~/Remote/<label>`) as System places
    /// after the workspaces, each with its health badge when not up to date.
    #[must_use]
    pub fn with_remote_accounts(
        mut self,
        accounts: Vec<(String, PathBuf, Option<String>)>,
    ) -> Self {
        let at = self
            .system
            .iter()
            .take_while(|(icon, _, _)| *icon == Icon::CloudDownload)
            .count();
        for (offset, (label, path, badge)) in accounts.into_iter().enumerate() {
            if let Some(badge) = badge {
                self.badges.insert(path.clone(), badge);
            }
            let icon = if self.badges.get(&path).is_some_and(|b| b == "Offline") {
                Icon::WifiOff
            } else {
                Icon::CloudDownload
            };
            self.system.insert(
                at.saturating_add(offset).min(self.system.len()),
                (icon, label, path),
            );
        }
        self
    }

    fn fav_count(&self) -> usize {
        self.favs.borrow().favorites.len()
    }

    fn place_count(&self) -> usize {
        self.fav_count() + self.system.len()
    }

    fn active_index(&self) -> Option<usize> {
        self.favs
            .borrow()
            .favorites
            .iter()
            .position(|favorite| favorite.path == self.active)
            .or_else(|| {
                self.system
                    .iter()
                    .position(|(_, _, path)| *path == self.active)
                    .map(|index| self.fav_count() + index)
            })
    }

    fn keyboard_index(&self) -> Option<usize> {
        let count = self.place_count();
        if count == 0 {
            return None;
        }
        self.favs
            .borrow()
            .keyboard_index
            .filter(|index| *index < count)
            .or_else(|| self.active_index())
            .or(Some(0))
    }

    fn set_keyboard_index(&self, index: usize) {
        let count = self.place_count();
        self.favs.borrow_mut().keyboard_index = (count > 0).then_some(index.min(count - 1));
    }

    fn place_at_index(&self, index: usize) -> Option<(String, PathBuf)> {
        let favs = self.favs.borrow();
        if let Some(favorite) = favs.favorites.get(index) {
            return Some((favorite.label.clone(), favorite.path.clone()));
        }
        drop(favs);
        self.system
            .get(index.checked_sub(self.fav_count())?)
            .map(|(_, label, path)| (label.clone(), path.clone()))
    }

    fn activate_keyboard_place(&self) -> bool {
        let Some((_, path)) = self
            .keyboard_index()
            .and_then(|index| self.place_at_index(index))
        else {
            return false;
        };
        push(&self.sink, FileCommand::Navigate(path));
        true
    }

    fn fav_first_y(&self) -> f64 {
        self.rect.y0 + TOP + (HEADER_H * self.text_scale)
    }
    fn sys_header_y(&self) -> f64 {
        // With no favourites there is no Favourites header to reserve room
        // for, so the System section starts at the very top instead of
        // leaving an empty gap where the (unshown) header would be.
        if self.fav_count() == 0 {
            self.rect.y0 + TOP
        } else {
            self.fav_first_y() + self.fav_count() as f64 * (ROW_H * self.text_scale) + SECTION_GAP
        }
    }
    fn sys_first_y(&self) -> f64 {
        self.sys_header_y() + (HEADER_H * self.text_scale)
    }

    fn row_x0(&self) -> f64 {
        self.rect.x0 + PAD
    }
    fn row_x1(&self) -> f64 {
        self.rect.x1 - PAD
    }

    fn fav_row_rect(&self, i: usize) -> Rect {
        let y0 = self.fav_first_y() + i as f64 * (ROW_H * self.text_scale);
        Rect::new(
            self.row_x0(),
            y0,
            self.row_x1(),
            y0 + (ROW_H * self.text_scale),
        )
    }
    fn sys_row_rect(&self, i: usize) -> Rect {
        let y0 = self.sys_first_y() + i as f64 * (ROW_H * self.text_scale);
        Rect::new(
            self.row_x0(),
            y0,
            self.row_x1(),
            y0 + (ROW_H * self.text_scale),
        )
    }

    fn metrics(&self) -> ListMetrics {
        ListMetrics::uniform(
            self.fav_first_y(),
            ROW_H * self.text_scale,
            0.0,
            self.fav_count(),
        )
    }
    fn band(&self) -> FavBand {
        FavBand {
            origin_y: self.fav_first_y(),
            row_h: (ROW_H * self.text_scale),
            x0: self.row_x0(),
            x1: self.row_x1(),
            count: self.fav_count(),
        }
    }

    fn favorite_at(&self, pt: Point) -> Option<usize> {
        if pt.x < self.row_x0() || pt.x > self.row_x1() {
            return None;
        }
        let n = self.fav_count();
        if n == 0 {
            return None;
        }
        let rel = pt.y - self.fav_first_y();
        if rel < 0.0 {
            return None;
        }
        let i = (rel / (ROW_H * self.text_scale)) as usize;
        (i < n).then_some(i)
    }
    fn system_at(&self, pt: Point) -> Option<usize> {
        if pt.x < self.row_x0() || pt.x > self.row_x1() {
            return None;
        }
        let rel = pt.y - self.sys_first_y();
        if rel < 0.0 {
            return None;
        }
        let i = (rel / (ROW_H * self.text_scale)) as usize;
        (i < self.system.len()).then_some(i)
    }

    fn draw_row(
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        visual: RowVisual,
    ) {
        let RowVisual {
            icon,
            label,
            active,
            hovered,
            ghost,
        } = visual;
        let tokens = &theme.tokens;
        let radius = f64::from(tokens.radii.md);
        let pill = Rect::new(
            rect.x0,
            rect.y0 + PILL_INSET_Y,
            rect.x1,
            rect.y1 - PILL_INSET_Y,
        );
        if ghost {
            // The dragged favourite is genuinely off the surface (HIG §9.3):
            // it casts the Panel shadow and reads as a raised card with its
            // derived edge, not as an accent wash.
            panel_shadow(scene, transform, pill, radius, theme, 1.0);
            ui::surface::card(
                scene,
                transform,
                pill,
                radius,
                tokens.colors.surfaces.raised,
            );
        } else if active || hovered {
            // The chosen place is a lift, never an accent fill (HIG §9.3).
            let tier = selection_tier(theme, SelectionState::new(active, hovered, false));
            scene.fill(
                Fill::NonZero,
                transform,
                tier.fill,
                None,
                &RoundedRect::from_rect(pill, radius),
            );
            if let Some(border) = tier.border {
                scene.stroke(
                    &Stroke::new(1.0),
                    transform,
                    border,
                    None,
                    &ui::surface::edge_outline(pill, radius),
                );
            }
            if let Some(glow) = tier.inner_glow {
                scene.stroke(
                    &Stroke::new(1.0),
                    transform,
                    glow,
                    None,
                    &ui::surface::edge_outline(pill.inset(-1.0), radius - 1.0),
                );
            }
        }

        // A lifted face is still the theme's own surface, so the row keeps its
        // ordinary ink; the active place is told apart by its lift, not by
        // white-on-accent text.
        let icon_color = if active || ghost {
            tokens.colors.text_primary
        } else {
            tokens.colors.text_dim
        };
        let text_color = tokens.colors.text_primary;
        let icon_x = rect.x0 + ROW_CONTENT_INSET;
        let icon_y = rect.y0 + (rect.height() - ICON_PX) / 2.0;
        draw_icon(
            scene,
            transform,
            Rect::new(icon_x, icon_y, icon_x + ICON_PX, icon_y + ICON_PX),
            icon,
            icon_color,
            LUCIDE_DEFAULT_STROKE_PX,
        );
        let size = theme.tokens.typography.scale.base / tr.text_scale();
        // Optically center the label against the box-centered icon (shared with
        // the file list / drag ghost — see `crate::text_layout`).
        let (_, height) = tr.measure(label, size, None);
        let ty = rect.y0 + (rect.height() - f64::from(height)) / 2.0;
        tr.draw_text(
            scene,
            label,
            size,
            text_color,
            transform * Affine::translate((icon_x + ICON_PX + ICON_LABEL_GAP, ty)),
            None,
        );
    }

    fn draw_header(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        y: f64,
        label: &str,
    ) {
        let size = theme.tokens.typography.scale.xs / tr.text_scale();
        let (_, height) = tr.measure(label, size, None);
        tr.draw_text(
            scene,
            &label.to_uppercase(),
            size,
            theme.tokens.colors.text_dim,
            transform
                * Affine::translate((
                    self.row_x0() + ROW_CONTENT_INSET,
                    y + (HEADER_H * self.text_scale - f64::from(height)) / 2.0,
                )),
            None,
        );
    }
}

impl Widget for PlacesSidebar {
    fn is_layout_dirty(&self) -> bool {
        // This leaf fills its assigned pane on both axes. Favorite and hover
        // changes affect painted rows, never the pane's intrinsic size.
        false
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.text_scale = f64::from(theme.text_scale);
        // Fill the pane the responsive shell hands us: docked (wide) the shell
        // allocates the sidebar width, but as the full-screen master pane
        // (narrow) the list must span the whole width rather than leaving a
        // dead strip. So take `percent(1.0)` and let the shell own the width.
        // `new_leaf` only fails on tree-internal bookkeeping errors, never on
        // caller input; there is none to report here.
        #[allow(
            clippy::unwrap_used,
            reason = "new_leaf cannot fail for a freshly built style"
        )]
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        // `node` was created by `build_layout` in this same tree, so a
        // lookup miss would mean the frame cycle called these out of order.
        #[allow(
            clippy::unwrap_used,
            reason = "node was built in this tree by build_layout; a miss means the frame cycle called these out of order"
        )]
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        // Publish the favourites band so the app / demo can drive a drag in the
        // same coordinate space the reflow uses.
        let band = self.band();
        self.favs.borrow_mut().band = Some(band);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.draw_panel_bg(scene, transform, theme);
        self.draw_favourites_section(scene, transform, tr, theme);
        self.draw_system_section(scene, transform, tr, theme);
        self.draw_drag_ghost(scene, transform, tr, theme);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one event dispatcher keeps keyboard focus, pointer capture and favourite drag transitions in order"
    )] // Central event dispatcher keeps gesture state transitions ordered.
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        match event {
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed && self.favs.borrow().focused =>
            {
                use winit::keyboard::{Key, NamedKey};
                let count = self.place_count();
                let current = self.keyboard_index().unwrap_or(0);
                match &event.logical_key {
                    Key::Named(NamedKey::ArrowUp) => {
                        self.set_keyboard_index(current.saturating_sub(1));
                        *ctx.handled = true;
                    }
                    Key::Named(NamedKey::ArrowDown) => {
                        self.set_keyboard_index((current + 1).min(count.saturating_sub(1)));
                        *ctx.handled = true;
                    }
                    Key::Named(NamedKey::Home) => {
                        self.set_keyboard_index(0);
                        *ctx.handled = true;
                    }
                    Key::Named(NamedKey::End) => {
                        if count > 0 {
                            self.set_keyboard_index(count - 1);
                        }
                        *ctx.handled = true;
                    }
                    Key::Named(NamedKey::Enter) => {
                        *ctx.handled = self.activate_keyboard_place();
                    }
                    Key::Named(NamedKey::ContextMenu) => {
                        if current < self.fav_count()
                            && let Some((_, path)) = self.place_at_index(current)
                        {
                            push(&self.sink, FileCommand::OpenFavoriteMenu(path));
                            *ctx.handled = true;
                        }
                    }
                    _ => {}
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let pt = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.last_cursor = pt;
                self.hover_fav = self.favorite_at(pt);
                self.hover_sys = self.system_at(pt);
                let metrics = self.metrics();
                let mut favs = self.favs.borrow_mut();
                if favs.drag.is_pending() {
                    favs.drag.pointer_moved(pt, &metrics);
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let pt = self.last_cursor;
                if let Some(i) = self.favorite_at(pt) {
                    self.set_keyboard_index(i);
                    self.favs.borrow_mut().focused = true;
                    *ctx.focus_target = Some(ctx.node_id);
                    let metrics = self.metrics();
                    let grab = pt.y - metrics.slot_lead(i);
                    let mut favs = self.favs.borrow_mut();
                    favs.drag.begin(i, i, grab, pt);
                    favs.pressed = Some(i);
                    *ctx.handled = true;
                } else if let Some(i) = self.system_at(pt)
                    && let Some((_, _, path)) = self.system.get(i)
                {
                    self.set_keyboard_index(self.fav_count() + i);
                    self.favs.borrow_mut().focused = true;
                    *ctx.focus_target = Some(ctx.node_id);
                    push(&self.sink, FileCommand::Navigate(path.clone()));
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                // A right-press over a favourite asks for its own menu (Open +
                // Remove from Favourites). The app opens it at the cursor once it
                // drains this command. System rows have no menu.
                if let Some(i) = self.favorite_at(self.last_cursor) {
                    let path = self.favs.borrow().favorites.get(i).map(|f| f.path.clone());
                    if let Some(p) = path {
                        push(&self.sink, FileCommand::OpenFavoriteMenu(p));
                        *ctx.handled = true;
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                let mut favs = self.favs.borrow_mut();
                if !favs.drag.is_pending() {
                    return;
                }
                let was_active = favs.drag.is_active();
                let pressed = favs.pressed.take();
                let commit = favs.drag.commit();
                drop(favs);
                if let Some((from, to)) = commit {
                    push(&self.sink, FileCommand::ReorderFavorite { from, to });
                    *ctx.handled = true;
                } else {
                    if !was_active && let Some(i) = pressed {
                        let path = self.favs.borrow().favorites.get(i).map(|f| f.path.clone());
                        if let Some(p) = path {
                            push(&self.sink, FileCommand::Navigate(p));
                        }
                    }
                    *ctx.handled = true;
                }
            }
            _ => {}
        }
    }

    /// While a favourite drag is latched the sidebar wants every pointer event
    /// so the drag survives the cursor crossing into the content pane.
    fn wants_pointer_capture(&self) -> bool {
        self.favs.borrow().drag.is_pending()
    }

    fn name(&self) -> &'static str {
        "PlacesSidebar"
    }
    fn test_id(&self) -> Option<&str> {
        Some("places_sidebar")
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        let mut favs = self.favs.borrow_mut();
        favs.focused = focused;
        if focused && favs.keyboard_index.is_none() {
            drop(favs);
            if let Some(index) = self
                .active_index()
                .or_else(|| (self.place_count() > 0).then_some(0))
            {
                self.set_keyboard_index(index);
            }
        }
    }

    fn is_focused(&self) -> bool {
        self.favs.borrow().focused
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let count = self.place_count();
        let keyboard = self.keyboard_index();
        let focused = self.is_focused();
        let mut children = Vec::with_capacity(count);
        for index in 0..count {
            let Some((label, path)) = self.place_at_index(index) else {
                continue;
            };
            let section = if index < self.fav_count() {
                t("files-a11y-favourite")
            } else {
                t("files-a11y-system-location")
            };
            children.push(
                AccessNode::new(Role::ListItem)
                    .with_name(label)
                    .with_description(crate::i18n::t_args(
                        "files-a11y-place-description",
                        &[
                            crate::i18n::arg("section", section),
                            crate::i18n::arg("path", path.display().to_string()),
                        ],
                    ))
                    .with_position(index + 1, count)
                    .with_state(AccessState {
                        focusable: true,
                        focused: focused && keyboard == Some(index),
                        selected: path == self.active,
                        ..Default::default()
                    }),
            );
        }
        Some(
            AccessNode::new(Role::List)
                .with_name(t("files-a11y-places"))
                .with_optional_value(
                    keyboard.and_then(|index| self.place_at_index(index).map(|(label, _)| label)),
                )
                .with_state(AccessState {
                    focusable: true,
                    focused,
                    ..Default::default()
                })
                .with_children(children),
        )
    }

    fn perform_action(
        &mut self,
        action: &ui::a11y::Action,
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        match action {
            ui::a11y::Action::Activate => self.activate_keyboard_place(),
            ui::a11y::Action::ShowContextMenu => {
                let Some(index) = self
                    .keyboard_index()
                    .filter(|index| *index < self.fav_count())
                else {
                    return false;
                };
                let Some((_, path)) = self.place_at_index(index) else {
                    return false;
                };
                push(&self.sink, FileCommand::OpenFavoriteMenu(path));
                true
            }
            _ => false,
        }
    }
}

impl PlacesSidebar {
    /// Panel background (a hair lighter than the content) + a quiet divider.
    /// The sidebar sits on the app base with a derived hairline down its
    /// right edge (HIG §9.5). It stays on `surfaces.base` because its rows
    /// are painted with the shared selection tier, which lifts from `base`:
    /// a lifted panel would swallow the tier's hover step. The edge is
    /// measured against the panel's own fill, so it registers on dark, light
    /// and high-contrast alike.
    fn draw_panel_bg(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let fill = theme.tokens.colors.surfaces.base;
        scene.fill(Fill::NonZero, transform, fill, None, &self.rect);
        scene.fill(
            Fill::NonZero,
            transform,
            ui::surface::edge_on(fill),
            None,
            &Rect::new(self.rect.x1 - 1.0, self.rect.y0, self.rect.x1, self.rect.y1),
        );
    }

    /// Favourites header (only when there are favourites to head — an empty
    /// section header reads as a bug) plus the favourite rows, with live
    /// reflow while dragging.
    fn draw_favourites_section(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.fav_count() > 0 {
            self.draw_header(
                scene,
                transform,
                tr,
                theme,
                self.rect.y0 + TOP,
                &t("files-place-favourites"),
            );
        }

        // Compute the list geometry BEFORE borrowing the store (metrics reads
        // the store).
        let metrics = self.metrics();
        let (drag_active, from_idx) = {
            let mut favs = self.favs.borrow_mut();
            let active = favs.drag.is_active();
            if active {
                favs.drag.sync_insertion(&metrics);
            }
            (active, favs.drag.from_index())
        };
        let favs = self.favs.borrow();
        for (i, fav) in favs.favorites.iter().enumerate() {
            if drag_active && Some(i) == from_idx {
                continue; // painted as the floating ghost below
            }
            let shift = if drag_active {
                favs.drag.item_shift(i, &metrics)
            } else {
                0.0
            };
            let mut rect = self.fav_row_rect(i);
            rect = Rect::new(rect.x0, rect.y0 + shift, rect.x1, rect.y1 + shift);
            let active = !drag_active && fav.path == self.active;
            let hovered = self.hover_fav == Some(i) && !active;
            Self::draw_row(
                scene,
                transform,
                tr,
                theme,
                rect,
                RowVisual {
                    icon: fav.icon(),
                    label: &fav.label,
                    active,
                    hovered,
                    ghost: false,
                },
            );
        }
    }

    fn draw_system_section(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.draw_header(
            scene,
            transform,
            tr,
            theme,
            self.sys_header_y(),
            &t("files-place-system"),
        );
        for (i, (icon, label, path)) in self.system.iter().enumerate() {
            let rect = self.sys_row_rect(i);
            let active = *path == self.active;
            let hovered = self.hover_sys == Some(i) && !active;
            Self::draw_row(
                scene,
                transform,
                tr,
                theme,
                rect,
                RowVisual {
                    icon: *icon,
                    label,
                    active,
                    hovered,
                    ghost: false,
                },
            );
            if let Some(badge) = self.badges.get(path) {
                Self::draw_badge(scene, transform, tr, theme, rect, badge);
            }
        }
    }

    /// A status badge at the trailing edge of a row, in the dim ink the
    /// section captions use.
    fn draw_badge(
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        badge: &str,
    ) {
        let size = theme.tokens.typography.scale.xs / tr.text_scale();
        let (width, height) = tr.measure(badge, size, None);
        tr.draw_text(
            scene,
            badge,
            size,
            theme.tokens.colors.text_dim,
            transform
                * Affine::translate((
                    rect.x1 - ROW_CONTENT_INSET - f64::from(width),
                    rect.y0 + (rect.height() - f64::from(height)) / 2.0,
                )),
            None,
        );
    }

    /// Floating ghost of the dragged favourite, if a drag is active. Reads
    /// the drag state [`Self::draw_favourites_section`] already synced this
    /// frame, so call order matters: favourites first, then the ghost.
    fn draw_drag_ghost(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let favs = self.favs.borrow();
        if !favs.drag.is_active() {
            return;
        }
        let metrics = self.metrics();
        let from_idx = favs.drag.from_index();
        if let (Some(fi), Some(ghost)) = (
            from_idx,
            favs.drag.ghost_rect(self.row_x0(), self.row_x1(), &metrics),
        ) && let Some(fav) = favs.favorites.get(fi)
        {
            Self::draw_row(
                scene,
                transform,
                tr,
                theme,
                ghost,
                RowVisual {
                    icon: fav.icon(),
                    label: &fav.label,
                    active: false,
                    hovered: false,
                    ghost: true,
                },
            );
        }
    }
}

/// Test / demo helper: begin an active favourite drag from index `from` toward
/// `to`, using the published [`FavBand`] geometry so the reflow renders in the
/// right coordinate space. Returns `false` if no band has been laid out yet.
pub fn debug_begin_drag(store: &FavStore, from: usize, to: usize) -> bool {
    let band = { store.borrow().band };
    let Some(band) = band else { return false };
    if from >= band.count {
        return false;
    }
    let metrics = band.metrics();
    let grab = band.row_h / 2.0;
    let start = Point::new(band.center_x(), band.row_center_y(from));
    let target = Point::new(band.center_x(), band.row_center_y(to));
    let mut favs = store.borrow_mut();
    favs.drag.begin(from, from, grab, start);
    favs.pressed = Some(from);
    favs.drag.pointer_moved(target, &metrics);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::new_sink;
    use crate::favorites;
    use automerge::AutoCommit;
    use std::path::Path;

    fn lay_out(sb: &mut PlacesSidebar) -> (TaffyTree<()>, NodeId) {
        let mut tree = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let theme = Theme::dark();
        let node = sb.build_layout(&mut tree, &theme, &mut node_map);
        let root = tree
            .new_with_children(
                Style {
                    display: taffy::Display::Flex,
                    flex_direction: taffy::FlexDirection::Column,
                    size: Size {
                        width: Dimension::length(SIDEBAR_WIDTH as f32),
                        height: Dimension::length(700.0),
                    },
                    ..Default::default()
                },
                &[node],
            )
            .unwrap();
        tree.compute_layout(
            root,
            Size {
                width: taffy::AvailableSpace::Definite(SIDEBAR_WIDTH as f32),
                height: taffy::AvailableSpace::Definite(700.0),
            },
        )
        .unwrap();
        sb.apply_layout(&tree, node, Point::ZERO);
        (tree, node)
    }

    fn dispatch(
        sb: &mut PlacesSidebar,
        tree: &TaffyTree<()>,
        node: NodeId,
        ev: &WindowEvent,
    ) -> bool {
        let mut handled = false;
        let mut focus = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let mods = winit::event::Modifiers::default();
        sb.handle_event(
            ev,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &mods,
                state: &mut doc,
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: tree,
            },
        );
        handled
    }

    fn device() -> winit::event::DeviceId {
        unsafe { std::mem::zeroed() }
    }

    #[test]
    fn mounted_cloud_accounts_are_system_places_with_badges() {
        let sb = PlacesSidebar::new(new_store(Vec::new()), PathBuf::from("/"), new_sink())
            .with_workspaces(vec![PathBuf::from("/var/workspace")])
            .with_remote_accounts(vec![
                ("Cloud".into(), PathBuf::from("/home/a/Remote/Cloud"), None),
                (
                    "Work".into(),
                    PathBuf::from("/home/a/Remote/Work"),
                    Some("Offline".into()),
                ),
            ]);
        let labels: Vec<&str> = sb
            .system
            .iter()
            .map(|(_, label, _)| label.as_str())
            .collect();
        assert_eq!(&labels[..3], ["workspace", "Cloud", "Work"]);
        assert_eq!(sb.system[2].0, Icon::WifiOff);
        assert_eq!(
            sb.badges
                .get(&PathBuf::from("/home/a/Remote/Work"))
                .map(String::as_str),
            Some("Offline")
        );
        assert!(
            !sb.badges
                .contains_key(&PathBuf::from("/home/a/Remote/Cloud"))
        );
    }

    /// Large-text header and rows fit their glyphs and rows and sections keep token gaps.
    #[expect(
        clippy::float_cmp,
        reason = "the insets are defined as the exact spacing token"
    )]
    #[test]
    fn places_metrics_fit_glyphs_and_token_gaps() {
        // --- phase: large_text_places_header_and_rows_fit_their_painted_glyphs ---
        {
            let sink = new_sink();
            let mut sidebar = PlacesSidebar::new(new_store(vec![]), PathBuf::from("/"), sink);
            let theme = Theme::dark().with_text_scale(2.0);
            let mut tree = TaffyTree::new();
            sidebar.build_layout(&mut tree, &theme, &mut HashMap::new());
            sidebar.rect = Rect::new(0.0, 0.0, 240.0, 600.0);
            let mut tr = TextRenderer::new();
            tr.set_text_scale(2.0);
            let (_, header_h) = tr.measure("SYSTEM", theme.tokens.typography.scale.xs / 2.0, None);
            let (_, row_h) = tr.measure("Root", theme.tokens.typography.scale.base / 2.0, None);
            assert!(sidebar.sys_first_y() - sidebar.sys_header_y() >= f64::from(header_h));
            assert!(sidebar.sys_row_rect(0).height() >= f64::from(row_h));
            let mut scene = Scene::new();
            sidebar.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                &PresenceCtx::default(),
            );
            let runs = &scene.encoding().resources.glyph_runs;
            assert!(
                runs.iter()
                    .all(|run| run.font_size <= theme.tokens.typography.scale.base)
            );
            assert!(
                runs.iter()
                    .any(|run| (run.font_size - theme.tokens.typography.scale.base).abs() < 0.01)
            );
        }

        // --- phase: rows_and_sections_keep_token_gaps ---
        // HIG §7b, one rhythm for the sidebar: adjacent row pills keep an `s1`
        // gap, a section is an `s3` from the previous one, and every inset is a
        // token step — so no two neighbours touch at 1×.
        {
            let sink = new_sink();
            let store = new_store(favorites::defaults(
                Path::new("/home/u"),
                &favorites::UserDirs::default(),
            ));
            let mut sb = PlacesSidebar::new(store, PathBuf::from("/home/u"), sink);
            let _ = lay_out(&mut sb);
            let s1 = f64::from(Spacing::BASE.s1);
            let s3 = f64::from(Spacing::BASE.s3);
            let favs = sb.fav_count();
            assert!(favs >= 2, "defaults seed several favourites");
            for i in 0..favs - 1 {
                let a = sb.fav_row_rect(i);
                let b = sb.fav_row_rect(i + 1);
                let gap = (b.y0 + PILL_INSET_Y) - (a.y1 - PILL_INSET_Y);
                assert!((gap - s1).abs() < 0.01, "pills {i}/{} gap {gap}", i + 1);
            }
            let last = sb.fav_row_rect(favs - 1);
            assert!((sb.sys_header_y() - last.y1 - s3).abs() < 0.01);
            assert_eq!(PAD, s3);
            assert_eq!(ROW_CONTENT_INSET, s3);
            assert!(sb.row_x0() > sb.rect.x0 && sb.row_x1() < sb.rect.x1);
        }
    }

    /// Dragging a favourite reorders, a plain click navigates, right press opens its menu and a debug drag reflows.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn pointer_on_places_reorders_navigates_and_opens_menus() {
        // --- phase: dragging_a_favourite_down_pushes_a_reorder_command ---
        {
            let sink = new_sink();
            let store = new_store(favorites::defaults(
                Path::new("/home/u"),
                &favorites::UserDirs::default(),
            ));
            let mut sb = PlacesSidebar::new(store.clone(), PathBuf::from("/home/u"), sink.clone());
            let (tree, node) = lay_out(&mut sb);

            // Grab favourite 1 (Documents) and drag it down two rows.
            let band = store.borrow().band.unwrap();
            let start_y = band.row_center_y(1);
            let end_y = band.row_center_y(3);
            let x = band.center_x();
            dispatch(
                &mut sb,
                &tree,
                node,
                &WindowEvent::CursorMoved {
                    device_id: device(),
                    position: (x, start_y).into(),
                },
            );
            dispatch(
                &mut sb,
                &tree,
                node,
                &WindowEvent::MouseInput {
                    device_id: device(),
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                },
            );
            dispatch(
                &mut sb,
                &tree,
                node,
                &WindowEvent::CursorMoved {
                    device_id: device(),
                    position: (x, end_y).into(),
                },
            );
            assert!(
                store.borrow().drag.is_active(),
                "the pointer travel promoted a drag"
            );
            dispatch(
                &mut sb,
                &tree,
                node,
                &WindowEvent::MouseInput {
                    device_id: device(),
                    state: ElementState::Released,
                    button: MouseButton::Left,
                },
            );

            let cmds = sink.borrow();
            assert_eq!(cmds.len(), 1, "exactly one command emitted");
            match &cmds[0] {
                FileCommand::ReorderFavorite { from, to } => {
                    assert_eq!(*from, 1, "dragged favourite 1");
                    assert_eq!(*to, 3, "dropped at row 3");
                }
                other => panic!("expected a reorder, got {other:?}"),
            }
        }

        // --- phase: a_plain_click_navigates_instead_of_reordering ---
        {
            let sink = new_sink();
            let store = new_store(favorites::defaults(
                Path::new("/home/u"),
                &favorites::UserDirs::default(),
            ));
            let mut sb = PlacesSidebar::new(store.clone(), PathBuf::from("/home/u"), sink.clone());
            let (tree, node) = lay_out(&mut sb);
            let band = store.borrow().band.unwrap();
            let y = band.row_center_y(2); // Downloads
            let x = band.center_x();
            // Press + release with no travel between → a click, not a drag.
            dispatch(
                &mut sb,
                &tree,
                node,
                &WindowEvent::CursorMoved {
                    device_id: device(),
                    position: (x, y).into(),
                },
            );
            dispatch(
                &mut sb,
                &tree,
                node,
                &WindowEvent::MouseInput {
                    device_id: device(),
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                },
            );
            dispatch(
                &mut sb,
                &tree,
                node,
                &WindowEvent::MouseInput {
                    device_id: device(),
                    state: ElementState::Released,
                    button: MouseButton::Left,
                },
            );
            let cmds = sink.borrow();
            assert_eq!(cmds.len(), 1);
            assert!(
                matches!(&cmds[0], FileCommand::Navigate(p) if p.ends_with("Downloads")),
                "click navigates: {:?}",
                cmds[0]
            );
        }

        // --- phase: right_pressing_a_favourite_requests_its_menu ---
        {
            let sink = new_sink();
            let store = new_store(favorites::defaults(
                Path::new("/home/u"),
                &favorites::UserDirs::default(),
            ));
            let mut sb = PlacesSidebar::new(store.clone(), PathBuf::from("/home/u"), sink.clone());
            let (tree, node) = lay_out(&mut sb);
            let band = store.borrow().band.unwrap();
            // Right-press over favourite row 1 (Documents).
            let y = band.row_center_y(1);
            let x = band.center_x();
            dispatch(
                &mut sb,
                &tree,
                node,
                &WindowEvent::CursorMoved {
                    device_id: device(),
                    position: (x, y).into(),
                },
            );
            let handled = dispatch(
                &mut sb,
                &tree,
                node,
                &WindowEvent::MouseInput {
                    device_id: device(),
                    state: ElementState::Pressed,
                    button: MouseButton::Right,
                },
            );
            assert!(handled, "a right-press over a favourite is consumed");
            let cmds = sink.borrow();
            assert_eq!(cmds.len(), 1);
            assert!(
                matches!(&cmds[0], FileCommand::OpenFavoriteMenu(p) if p.ends_with("Documents")),
                "right-press asks for the favourite's menu: {:?}",
                cmds[0],
            );
        }

        // --- phase: debug_begin_drag_activates_a_reflow ---
        {
            let sink = new_sink();
            let store = new_store(favorites::defaults(
                Path::new("/home/u"),
                &favorites::UserDirs::default(),
            ));
            let mut sb = PlacesSidebar::new(store.clone(), PathBuf::from("/home/u"), sink);
            let _ = lay_out(&mut sb);
            assert!(
                debug_begin_drag(&store, 4, 1),
                "band was laid out → drag begins"
            );
            assert!(store.borrow().drag.is_active());
            assert_eq!(
                store.borrow().drag.insertion(),
                Some(1),
                "gap sits at the drop row"
            );
        }
    }

    /// Trash and workspace roots are system places, and accessibility exposes selection, keyboard cursor and activation.
    #[test]
    fn system_places_are_clickable_and_accessible() {
        // --- phase: trash_is_a_system_place_pointing_at_the_home_trash ---
        {
            // Deleting moves files to the Trash; the sidebar must offer a way back to
            // it. It sits in the System section (near Root) and points at the XDG
            // home-trash files dir.
            let sink = new_sink();
            let store = new_store(favorites::defaults(
                Path::new("/home/u"),
                &favorites::UserDirs::default(),
            ));
            let sb = PlacesSidebar::new(store, PathBuf::from("/home/u"), sink);
            if let Some(trash) = crate::fileops::trash_dir() {
                let entry = sb.system.iter().find(|(_, label, _)| label == "Trash");
                let (icon, _, path) = entry.expect("a Trash system place");
                assert_eq!(*icon, Icon::Trash2, "the trash uses a trash-can glyph");
                assert_eq!(*path, trash, "and points at the home-trash files dir");
            }
        }

        // --- phase: workspace_roots_are_clickable_system_places ---
        {
            let sink = new_sink();
            let store = new_store(favorites::defaults(
                Path::new("/home/u"),
                &favorites::UserDirs::default(),
            ));
            let ws = PathBuf::from("/var/workspace");
            let mut sb = PlacesSidebar::new(store, PathBuf::from("/home/u"), sink.clone())
                .with_workspaces(vec![ws.clone()]);
            // The workspace leads the System section with a cloud emblem.
            assert_eq!(
                sb.system[0].1, "workspace",
                "labelled by the mount's folder name"
            );
            assert_eq!(sb.system[0].2, ws);
            assert!(
                sb.system.iter().any(|(_, label, _)| label == "Root"),
                "Root still present, after the workspace",
            );

            let (tree, node) = lay_out(&mut sb);
            // Click the first System row (the workspace) and expect a navigation.
            let y = sb.sys_first_y() + ROW_H / 2.0;
            let x = f64::midpoint(sb.row_x0(), sb.row_x1());
            dispatch(
                &mut sb,
                &tree,
                node,
                &WindowEvent::CursorMoved {
                    device_id: device(),
                    position: (x, y).into(),
                },
            );
            dispatch(
                &mut sb,
                &tree,
                node,
                &WindowEvent::MouseInput {
                    device_id: device(),
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                },
            );
            let cmds = sink.borrow();
            assert!(
                matches!(&cmds[0], FileCommand::Navigate(p) if *p == ws),
                "clicking the workspace navigates into it: {cmds:?}",
            );
        }

        // --- phase: accessibility_exposes_places_selection_keyboard_cursor_and_activation ---
        {
            use ui::a11y::{Action, Role};

            let sink = new_sink();
            let store = new_store(favorites::defaults(
                Path::new("/home/u"),
                &favorites::UserDirs::default(),
            ));
            let mut sidebar = PlacesSidebar::new(store, PathBuf::from("/home/u"), sink.clone());
            sidebar.set_focus(true);
            sidebar.set_keyboard_index(1);

            let node = sidebar.accessibility().expect("places has semantics");
            assert_eq!(node.role, Role::List);
            assert_eq!(node.name.as_deref(), Some("Places"));
            assert!(node.state.focusable && node.state.focused);
            assert_eq!(node.children.len(), sidebar.place_count());
            assert!(node.children[0].state.selected, "Home is the active place");
            assert!(
                node.children[1].state.focused,
                "keyboard cursor is independent"
            );
            assert_eq!(node.children[1].pos_in_set, Some(2));
            assert_eq!(node.children[1].set_size, Some(sidebar.place_count()));

            let mut doc = AutoCommit::new();
            assert!(sidebar.perform_action(&Action::Activate, &mut doc, &automerge::ROOT));
            assert!(matches!(
                sink.borrow().last(),
                Some(FileCommand::Navigate(path)) if path.ends_with("Documents")
            ));
        }
    }
}
