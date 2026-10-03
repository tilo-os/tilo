//! `FileToolbar` — the actions of the file manager's content header.
//!
//! The window shell draws one header row across the top and hands the content
//! column's actions to this widget:
//!
//! ```text
//! [ ← → ↑ ]   [ Home > Projects > tilo ················· ]   [ List | Grid ]  [ Sort by ]  [ ⋯ ]
//! [ ← ↑ ]     [ Home > Projects > tilo ················· ]                                [ ⋯ ]
//! ```
//!
//! * a nav cluster (back / forward / up), the only icon-only controls besides
//!   the overflow, and universally understood arrows;
//! * the [`PathBar`] (breadcrumb that becomes an editable path field), which
//!   grows to fill the middle;
//! * a labelled `List | Grid` view switch;
//! * a labelled **Sort by** menu button, shown only in grid mode (list mode
//!   sorts by clicking a column header);
//! * a **⋯** overflow that opens the labelled "View options" menu (new folder /
//!   file, show-hidden toggle, terminal).
//!
//! There is **no search field** (search belongs to the platform) and no
//! unlabelled state-toggle icons: show-hidden lives, clearly labelled with a
//! check, inside the View menu.
//!
//! The shell owns the widget once it is handed over, so the live context (what
//! the app knows: history, view mode, path) goes in through a
//! [`ToolbarHandle`], and what the app needs back (the controls' rects, whether
//! the path is being edited) comes out of it. The [`PathBar`] holds live edit
//! state (the in-progress path text and its completion popover) and lives
//! inside the widget for as long as the window does; its stateless button
//! children are rebuilt each layout.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use ui::widget::{AppModel, ModelPath};

use taffy::{
    AlignItems, Dimension, Display, FlexDirection, LengthPercentage, NodeId, Size, Style, TaffyTree,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::toggle_group::Segment;
use ui::widget::toggle_group::ToggleGroup;
use ui::widget::{Button, Widget};

use crate::command::{CommandSink, FileCommand, push};
use crate::i18n::t;
use crate::model::ViewMode;
use crate::path_bar::PathBar;

/// Which history and hierarchy moves the current location allows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NavAvailability {
    pub back: bool,
    pub forward: bool,
    pub up: bool,
}

/// What the app tells the toolbar before each layout.
#[derive(Clone, Debug)]
struct Context {
    nav: NavAvailability,
    view_mode: ViewMode,
    archive_read_only: bool,
    current: std::path::PathBuf,
    home: Option<std::path::PathBuf>,
    workspace_roots: Vec<std::path::PathBuf>,
    /// Set by [`ToolbarHandle::begin_path_edit`]; the toolbar acts on it at
    /// its next sync.
    edit_requested: bool,
    /// Room the shell keeps at the band's leading edge for its own controls
    /// (the sidebar toggle or Back, when they sit in this column's header).
    leading_inset: f32,
}

/// What the toolbar publishes for the app: its controls' rects after layout.
#[derive(Default)]
struct Published {
    up: Cell<Option<Rect>>,
    view_menu: Cell<Option<Rect>>,
    sort_menu: Cell<Option<Rect>>,
    area: Cell<Rect>,
    editing: Cell<bool>,
    compact: Cell<bool>,
    /// The inset changed: the toolbar is rebuilt with it at the next frame.
    inset_changed: Cell<bool>,
}

struct Shared {
    context: RefCell<Context>,
    published: Published,
}

/// The app's side of the toolbar the shell owns.
#[derive(Clone)]
pub struct ToolbarHandle(Rc<Shared>);

impl ToolbarHandle {
    /// Refresh the live context (called before each rebuild).
    pub fn set_context(&self, nav: NavAvailability, view_mode: ViewMode, archive_read_only: bool) {
        let mut context = self.0.context.borrow_mut();
        context.nav = nav;
        context.view_mode = view_mode;
        context.archive_read_only = archive_read_only;
    }

    /// Show `current` in the path bar, abbreviating under `home` and naming
    /// any of `workspace_roots` it lies in (called before each rebuild).
    pub fn set_path(
        &self,
        current: std::path::PathBuf,
        home: Option<std::path::PathBuf>,
        workspace_roots: Vec<std::path::PathBuf>,
    ) {
        let mut context = self.0.context.borrow_mut();
        context.current = current;
        context.home = home;
        context.workspace_roots = workspace_roots;
    }

    /// Put the path bar into editable mode (the Ctrl+L "edit location" path).
    pub fn begin_path_edit(&self) {
        self.0.context.borrow_mut().edit_requested = true;
        self.0.published.editing.set(true);
    }

    /// Whether the path bar is currently being edited (so the app leaves the
    /// keyboard to it).
    pub fn is_path_editing(&self) -> bool {
        self.0.published.editing.get()
    }

    /// Bottom-right anchor of the "View options" (⋯) button, so the app can
    /// open its menu right-aligned under the button (never off the right edge).
    pub fn view_menu_anchor(&self) -> Point {
        self.view_menu_rect().map_or_else(
            || {
                let area = self.0.published.area.get();
                Point::new(area.x1, area.y1)
            },
            |r| Point::new(r.x1, r.y1 + 2.0),
        )
    }

    /// Exact rectangle of the overflow trigger. The app uses its top edge to
    /// flip a tall menu above the trigger when a constrained viewport cannot
    /// fit the normal below-trigger placement.
    pub fn view_menu_rect(&self) -> Option<Rect> {
        self.0.published.view_menu.get()
    }

    /// Bottom-right anchor of the "Sort by" button (present only in grid mode,
    /// where it sits before the ⋯).
    pub fn sort_menu_anchor(&self) -> Point {
        self.sort_menu_rect()
            .or_else(|| self.view_menu_rect())
            .map_or_else(
                || {
                    let area = self.0.published.area.get();
                    Point::new(area.x1, area.y1)
                },
                |r| Point::new(r.x1, r.y1 + 2.0),
            )
    }

    /// Exact rectangle of the grid-mode Sort trigger, if it is visible.
    pub fn sort_menu_rect(&self) -> Option<Rect> {
        self.0.published.sort_menu.get()
    }

    /// The Up nav button's on-screen rect, so a drag can be dropped on it to
    /// move the carried items into the parent directory.
    pub fn up_button_rect(&self) -> Option<Rect> {
        self.0.published.up.get()
    }

    /// Whether compact navigation is active. Exposed for the Files responsive
    /// layout test; all live behavior is driven by the toolbar's layout.
    pub fn is_compact(&self) -> bool {
        self.0.published.compact.get()
    }

    /// Keep `inset` clear at the toolbar's leading edge, for the shell's
    /// toggle or Back button when it sits in this column's header.
    pub fn set_leading_inset(&self, inset: f64) {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "an inset is a few dozen logical pixels; f32 holds it exactly enough"
        )]
        let inset = inset.max(0.0) as f32;
        let mut context = self.0.context.borrow_mut();
        if (context.leading_inset - inset).abs() > 0.5 {
            context.leading_inset = inset;
            self.0.published.inset_changed.set(true);
        }
    }

    /// Whether a compact overflow should offer Forward navigation.
    pub fn can_go_forward(&self) -> bool {
        self.0.context.borrow().nav.forward
    }
}

pub struct FileToolbar {
    sink: CommandSink,
    shared: Rc<Shared>,
    /// The editable path bar (breadcrumb ⇄ text field). Persistent so an
    /// in-progress path edit survives the app's per-frame rebuild.
    path_bar: PathBar,
    // Stateless children, rebuilt each `build_layout`.
    left: Vec<Box<dyn Widget>>,
    right: Vec<Box<dyn Widget>>,
    rect: Rect,
    /// Derived from the last laid-out width. A change marks the toolbar dirty
    /// so the next composition uses the appropriate child set.
    compact: bool,
    /// The width below which view and sort choices move into the overflow.
    compact_below: f64,
    structure_dirty: bool,
}

impl FileToolbar {
    /// The toolbar for the shell to own, and the handle the app keeps.
    pub fn new(sink: CommandSink) -> (Self, ToolbarHandle) {
        let shared = Rc::new(Shared {
            context: RefCell::new(Context {
                nav: NavAvailability::default(),
                view_mode: ViewMode::List,
                archive_read_only: false,
                current: std::path::PathBuf::new(),
                home: None,
                workspace_roots: Vec::new(),
                edit_requested: false,
                leading_inset: 0.0,
            }),
            published: Published::default(),
        });
        // Start compact: an initial layout does not yet know its viewport. A
        // wide window promotes the full controls on the next frame; narrow
        // windows are correct from their very first frame.
        shared.published.compact.set(true);
        let toolbar = Self {
            path_bar: PathBar::new(sink.clone()),
            sink,
            shared: Rc::clone(&shared),
            left: Vec::new(),
            right: Vec::new(),
            rect: Rect::ZERO,
            compact: true,
            compact_below: 0.0,
            structure_dirty: false,
        };
        (toolbar, ToolbarHandle(shared))
    }

    fn nav_button(
        &self,
        id: &str,
        icon: Icon,
        cmd: FileCommand,
        enabled: bool,
        tip: &str,
    ) -> Box<dyn Widget> {
        let sink = self.sink.clone();
        // The glyph alone is ambiguous; the tooltip labels it on hover and
        // doubles as the button's accessible name.
        Box::new(
            Button::icon_only(id, icon, tip)
                .with_enabled(enabled)
                .on_effect(move || push(&sink, cmd.clone())),
        )
    }

    fn build_left(&self) -> Vec<Box<dyn Widget>> {
        let nav = self.shared.context.borrow().nav;
        let mut controls = vec![self.nav_button(
            "files.back",
            Icon::ArrowLeft,
            FileCommand::Back,
            nav.back,
            &t("files-back"),
        )];
        if !self.compact {
            controls.push(self.nav_button(
                "files.forward",
                Icon::ArrowRight,
                FileCommand::Forward,
                nav.forward,
                &t("files-forward"),
            ));
        }
        controls.push(self.nav_button(
            "files.up",
            Icon::ArrowUp,
            FileCommand::Up,
            nav.up,
            &t("files-up"),
        ));
        controls
    }

    fn build_right(&self) -> Vec<Box<dyn Widget>> {
        let (view_mode, archive_read_only) = {
            let context = self.shared.context.borrow();
            (context.view_mode, context.archive_read_only)
        };
        let mut out: Vec<Box<dyn Widget>> = Vec::new();

        if !self.compact {
            // View switch — labelled, so the active view is obvious.
            let view_sink = self.sink.clone();
            let selected = match view_mode {
                ViewMode::List => 0,
                ViewMode::Grid => 1,
            };
            out.push(Box::new(
                ToggleGroup::new(
                    vec![
                        Segment::new(t("files-view-list")),
                        Segment::new(t("files-view-grid")),
                    ],
                    selected,
                )
                .on_view("files.view-mode", move |i| {
                    let mode = if i == 0 {
                        ViewMode::List
                    } else {
                        ViewMode::Grid
                    };
                    push(&view_sink, FileCommand::SetView(mode));
                }),
            ));

            // Grid mode has no clickable column header, so it gets a labelled
            // Sort menu button. List mode sorts by clicking a header.
            if view_mode == ViewMode::Grid {
                let sink = self.sink.clone();
                out.push(Box::new(
                    Button::new("files.sort_menu", &t("files-sort-by"), None)
                        .with_icon(Icon::ArrowUpDown)
                        .on_effect(move || push(&sink, FileCommand::OpenSortMenu)),
                ));
            }
        }

        // Archives browse like folders, but extraction is their primary
        // mutation. Keep it directly reachable rather than burying it in an
        // overflow; compact layouts retain the same control as an icon with a
        // spoken/hover label so the location field still has room.
        if archive_read_only {
            let sink = self.sink.clone();
            let extract_label = t("files-archive-extract");
            let button = if self.compact {
                Button::icon_only("files.archive.extract", Icon::Download, &extract_label)
            } else {
                Button::new("files.archive.extract", &extract_label, None).with_icon(Icon::Download)
            };
            out.push(Box::new(button.on_effect(move || {
                push(&sink, FileCommand::ExtractCurrentArchive);
            })));
        }

        // Archive browsing is read-only and already has a direct Extract
        // control. Its overflow would contain only a duplicate Extract row,
        // so omit the no-choice menu entirely.
        if !archive_read_only {
            // Overflow: the labelled View-options menu (new folder/file, show
            // hidden, terminal). Every action it reveals is spelled out.
            let sink = self.sink.clone();
            out.push(Box::new(
                Button::icon_only(
                    "files.view_menu",
                    Icon::MoreHorizontal,
                    &t("files-view-options"),
                )
                .on_effect(move || push(&sink, FileCommand::OpenViewMenu)),
            ));
        }
        out
    }

    /// Publish where the controls ended up, for the app's anchored menus and
    /// drop targets.
    fn publish(&self) {
        let published = &self.shared.published;
        published.area.set(self.rect);
        published
            .up
            .set(self.left.last().map(|button| button.get_rect()));
        let archive_read_only = self.shared.context.borrow().archive_read_only;
        published.view_menu.set(
            (!archive_read_only)
                .then(|| self.right.last().map(|button| button.get_rect()))
                .flatten(),
        );
        let grid = self.shared.context.borrow().view_mode == ViewMode::Grid;
        published.sort_menu.set(
            (!self.compact && grid)
                .then(|| self.right.get(1).map(|button| button.get_rect()))
                .flatten(),
        );
        published.editing.set(self.path_bar.is_editing());
        published.compact.set(self.compact);
    }

    /// Group node ranges within the toolbar row's children. `None` if
    /// `children` doesn't match the shape `build_layout` produced (left
    /// controls, the path bar, right controls).
    fn child_groups<'a>(
        &self,
        children: &'a [NodeId],
    ) -> Option<(&'a [NodeId], NodeId, &'a [NodeId])> {
        let nleft = self.left.len();
        let (left, rest) = children.split_at_checked(nleft)?;
        let (&path_n, right) = rest.split_first()?;
        Some((left, path_n, right))
    }
}

impl Widget for FileToolbar {
    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        {
            let mut context = self.shared.context.borrow_mut();
            self.path_bar.set_path(
                context.current.clone(),
                context.home.clone(),
                context.workspace_roots.clone(),
            );
            if std::mem::take(&mut context.edit_requested) {
                self.path_bar.begin_edit();
            }
        }
        self.path_bar.sync(state, path);
        self.shared
            .published
            .editing
            .set(self.path_bar.is_editing());
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let sp = &theme.tokens.spacing;
        self.compact_below = f64::from(theme.tokens.metrics.content_min_width);
        self.left = self.build_left();
        self.right = self.build_right();

        let mut child_nodes: Vec<NodeId> = Vec::new();
        for c in &mut self.left {
            child_nodes.push(c.build_layout(taffy, theme, node_map));
        }
        child_nodes.push(self.path_bar.build_layout(taffy, theme, node_map));
        for c in &mut self.right {
            child_nodes.push(c.build_layout(taffy, theme, node_map));
        }

        let row_style = Style {
            display: Display::Flex,
            flex_direction: FlexDirection::Row,
            align_items: Some(AlignItems::Center),
            gap: Size {
                width: LengthPercentage::length(sp.s2),
                height: LengthPercentage::length(0.0),
            },
            padding: taffy::Rect {
                left: LengthPercentage::length(self.shared.context.borrow().leading_inset),
                right: LengthPercentage::length(0.0),
                top: LengthPercentage::length(0.0),
                bottom: LengthPercentage::length(0.0),
            },
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::auto(),
            },
            flex_shrink: 0.0,
            ..Default::default()
        };
        // `new_with_children` only fails when a child belongs to a different
        // tree; every entry in `child_nodes` was just built in this one above.
        #[allow(
            clippy::unwrap_used,
            reason = "every child node was just created in this same tree"
        )]
        let node = taffy.new_with_children(row_style, &child_nodes).unwrap();
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
        let compact = self.rect.width() < self.compact_below;
        if compact != self.compact {
            self.compact = compact;
            self.structure_dirty = true;
        }
        let base = Point::new(self.rect.x0, self.rect.y0);
        if let Ok(children) = taffy.children(node) {
            let nleft = self.left.len();
            for (c, &n) in self.left.iter_mut().zip(children.iter()) {
                c.apply_layout(taffy, n, base);
            }
            if let Some(&pn) = children.get(nleft) {
                self.path_bar.apply_layout(taffy, pn, base);
            }
            for (c, &n) in self.right.iter_mut().zip(children.iter().skip(nleft + 1)) {
                c.apply_layout(taffy, n, base);
            }
        }
        self.publish();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        for c in &mut self.left {
            c.draw(scene, transform, tr, theme, presence);
        }
        self.path_bar.draw(scene, transform, tr, theme, presence);
        for c in &mut self.right {
            c.draw(scene, transform, tr, theme, presence);
        }
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        permissions: &ui::permissions::PermissionContext,
    ) {
        for c in &mut self.left {
            c.draw_with_presence(scene, transform, tr, theme, presence, permissions);
        }
        self.path_bar
            .draw_with_presence(scene, transform, tr, theme, presence, permissions);
        for c in &mut self.right {
            c.draw_with_presence(scene, transform, tr, theme, presence, permissions);
        }
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // The path bar's completion popover is the only overlay here.
        self.path_bar
            .draw_overlay(scene, transform, tr, theme, presence);
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        let Ok(children) = ctx.taffy.children(ctx.node_id) else {
            return;
        };
        let Some((left_ns, path_n, right_ns)) = self.child_groups(&children) else {
            return;
        };
        // Path bar first, so it captures the keyboard while editing.
        self.path_bar
            .handle_event(event, &mut ctx.reborrow(ctx.path, path_n));
        self.shared
            .published
            .editing
            .set(self.path_bar.is_editing());
        for (c, &n) in self.left.iter_mut().zip(left_ns.iter()) {
            if *ctx.handled {
                break;
            }
            c.handle_event(event, &mut ctx.reborrow(ctx.path, n));
        }
        for (c, &n) in self.right.iter_mut().zip(right_ns.iter()) {
            if *ctx.handled {
                break;
            }
            c.handle_event(event, &mut ctx.reborrow(ctx.path, n));
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut v: Vec<&dyn Widget> = self.left.iter().map(std::convert::AsRef::as_ref).collect();
        v.push(&self.path_bar);
        v.extend(self.right.iter().map(std::convert::AsRef::as_ref));
        v
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut v: Vec<&mut (dyn Widget + '_)> = Vec::new();
        for c in &mut self.left {
            v.push(c.as_mut());
        }
        v.push(&mut self.path_bar);
        for c in &mut self.right {
            v.push(c.as_mut());
        }
        v
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.path_bar.cursor_shape(point)
    }

    fn take_structure_dirty(&mut self) -> bool {
        let inset = self.shared.published.inset_changed.take();
        std::mem::take(&mut self.structure_dirty) || inset || self.path_bar.take_structure_dirty()
    }

    fn name(&self) -> &'static str {
        "FileToolbar"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}
