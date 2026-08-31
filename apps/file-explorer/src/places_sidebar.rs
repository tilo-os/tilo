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

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::command::{CommandSink, FileCommand, push};
use crate::favorites::Favorite;
use ui::ColorCompat;
use ui::icons::{Icon, LUCIDE_DEFAULT_STROKE_PX, draw_icon};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use ui::widget::drag::{DragController, ListMetrics};

/// Sidebar width in logical px (kept equal to the historical value so parity
/// tests + layout expectations hold).
pub const SIDEBAR_WIDTH: f64 = 200.0;

const PAD: f64 = 12.0;
const TOP: f64 = 10.0;
const HEADER_H: f64 = 26.0;
const ROW_H: f64 = 34.0;
const SECTION_GAP: f64 = 10.0;
const ICON_PX: f64 = 17.0;

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
}

impl FavState {
    pub fn new(favorites: Vec<Favorite>) -> Self {
        Self {
            favorites,
            drag: DragController::vertical(),
            pressed: None,
            band: None,
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
    sink: CommandSink,
    rect: Rect,
    last_cursor: Point,
    hover_fav: Option<usize>,
    hover_sys: Option<usize>,
}

impl PlacesSidebar {
    pub fn new(favs: FavStore, active: PathBuf, sink: CommandSink) -> Self {
        // System places: the filesystem root, then the Trash (when the platform
        // exposes one). Deleting moves files here; clicking it opens the trash.
        let mut system = vec![(Icon::HardDrive, "Root".to_string(), PathBuf::from("/"))];
        if let Some(trash) = crate::fileops::trash_dir() {
            system.push((Icon::Trash2, "Trash".to_string(), trash));
        }
        Self {
            favs,
            active,
            system,
            sink,
            rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            hover_fav: None,
            hover_sys: None,
        }
    }

    /// Surface the qOS collaborative-workspace mounts as first-class System
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

    fn fav_count(&self) -> usize {
        self.favs.borrow().favorites.len()
    }

    fn fav_first_y(&self) -> f64 {
        self.rect.y0 + TOP + HEADER_H
    }
    fn sys_header_y(&self) -> f64 {
        // With no favourites there is no Favourites header to reserve room
        // for, so the System section starts at the very top instead of
        // leaving an empty gap where the (unshown) header would be.
        if self.fav_count() == 0 {
            self.rect.y0 + TOP
        } else {
            self.fav_first_y() + self.fav_count() as f64 * ROW_H + SECTION_GAP
        }
    }
    fn sys_first_y(&self) -> f64 {
        self.sys_header_y() + HEADER_H
    }

    fn row_x0(&self) -> f64 {
        self.rect.x0 + PAD
    }
    fn row_x1(&self) -> f64 {
        self.rect.x1 - PAD
    }

    fn fav_row_rect(&self, i: usize) -> Rect {
        let y0 = self.fav_first_y() + i as f64 * ROW_H;
        Rect::new(self.row_x0(), y0, self.row_x1(), y0 + ROW_H)
    }
    fn sys_row_rect(&self, i: usize) -> Rect {
        let y0 = self.sys_first_y() + i as f64 * ROW_H;
        Rect::new(self.row_x0(), y0, self.row_x1(), y0 + ROW_H)
    }

    fn metrics(&self) -> ListMetrics {
        ListMetrics::uniform(self.fav_first_y(), ROW_H, 0.0, self.fav_count())
    }
    fn band(&self) -> FavBand {
        FavBand {
            origin_y: self.fav_first_y(),
            row_h: ROW_H,
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
        let i = (rel / ROW_H) as usize;
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
        let i = (rel / ROW_H) as usize;
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
        let accent = tokens.colors.accent.stops[ui::tokens::colors::ACCENT_PRIMARY_IDX];
        let pill = Rect::new(rect.x0, rect.y0 + 2.0, rect.x1, rect.y1 - 2.0);
        if ghost {
            let mut a = accent;
            a = Color::from_rgba8(a.r(), a.g(), a.b(), 0xe0);
            scene.fill(
                Fill::NonZero,
                transform,
                a,
                None,
                &RoundedRect::from_rect(pill, f64::from(tokens.radii.md)),
            );
        } else if active {
            scene.fill(
                Fill::NonZero,
                transform,
                accent,
                None,
                &RoundedRect::from_rect(pill, f64::from(tokens.radii.md)),
            );
        } else if hovered {
            scene.fill(
                Fill::NonZero,
                transform,
                tokens.colors.surface_white_10,
                None,
                &RoundedRect::from_rect(pill, f64::from(tokens.radii.md)),
            );
        }

        let selected = active || ghost;
        let icon_color = if selected {
            Color::WHITE
        } else {
            tokens.colors.text_dim
        };
        let text_color = if selected {
            Color::WHITE
        } else {
            tokens.colors.text_primary
        };
        let icon_x = rect.x0 + 10.0;
        let icon_y = rect.y0 + (ROW_H - ICON_PX) / 2.0;
        draw_icon(
            scene,
            transform,
            Rect::new(icon_x, icon_y, icon_x + ICON_PX, icon_y + ICON_PX),
            icon,
            icon_color,
            LUCIDE_DEFAULT_STROKE_PX,
        );
        let size = theme.tokens.typography.scale.base;
        // Optically center the label against the box-centered icon (shared with
        // the file list / drag ghost — see `crate::text_layout`).
        let ty = crate::text_layout::centered_text_y(rect.y0, ROW_H, size);
        tr.draw_text(
            scene,
            label,
            size,
            text_color,
            transform * Affine::translate((icon_x + ICON_PX + 10.0, ty)),
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
        let size = theme.tokens.typography.scale.xs;
        tr.draw_text(
            scene,
            &label.to_uppercase(),
            size,
            theme.tokens.colors.text_dim,
            transform
                * Affine::translate((self.row_x0() + 2.0, y + (HEADER_H - f64::from(size)) / 2.0)),
            None,
        );
        let _ = scene;
    }
}

impl Widget for PlacesSidebar {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
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

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        let _ = focus_target;
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let pt = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_cursor = pt;
                self.hover_fav = self.favorite_at(pt);
                self.hover_sys = self.system_at(pt);
                let metrics = self.metrics();
                let mut favs = self.favs.borrow_mut();
                if favs.drag.is_pending() {
                    favs.drag.pointer_moved(pt, &metrics);
                    *handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let pt = self.last_cursor;
                if let Some(i) = self.favorite_at(pt) {
                    let metrics = self.metrics();
                    let grab = pt.y - metrics.slot_lead(i);
                    let mut favs = self.favs.borrow_mut();
                    favs.drag.begin(i, i, grab, pt);
                    favs.pressed = Some(i);
                    *handled = true;
                } else if let Some(i) = self.system_at(pt)
                    && let Some((_, _, path)) = self.system.get(i)
                {
                    push(&self.sink, FileCommand::Navigate(path.clone()));
                    *handled = true;
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
                        *handled = true;
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
                    *handled = true;
                } else {
                    if !was_active && let Some(i) = pressed {
                        let path = self.favs.borrow().favorites.get(i).map(|f| f.path.clone());
                        if let Some(p) = path {
                            push(&self.sink, FileCommand::Navigate(p));
                        }
                    }
                    *handled = true;
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
}

impl PlacesSidebar {
    /// Panel background (a hair lighter than the content) + a quiet divider.
    fn draw_panel_bg(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let tokens = &theme.tokens;
        scene.fill(
            Fill::NonZero,
            transform,
            tokens.colors.surface_white_5,
            None,
            &self.rect,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            tokens.colors.border_white_10,
            None,
            &Line::new((self.rect.x1, self.rect.y0), (self.rect.x1, self.rect.y1)),
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
                "Favourites",
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
        self.draw_header(scene, transform, tr, theme, self.sys_header_y(), "System");
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
        }
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
            1.0,
            &mods,
            &mut doc,
            &root,
            &mut handled,
            &mut focus,
            node,
            tree,
        );
        handled
    }

    fn device() -> winit::event::DeviceId {
        unsafe { std::mem::zeroed() }
    }

    #[test]
    fn dragging_a_favourite_down_pushes_a_reorder_command() {
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

    #[test]
    fn a_plain_click_navigates_instead_of_reordering() {
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

    #[test]
    fn right_pressing_a_favourite_requests_its_menu() {
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

    #[test]
    fn trash_is_a_system_place_pointing_at_the_home_trash() {
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

    #[test]
    fn workspace_roots_are_clickable_system_places() {
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

    #[test]
    fn debug_begin_drag_activates_a_reflow() {
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
