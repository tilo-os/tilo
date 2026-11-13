//! The content header: the back and forward controls, the path bar, and the
//! view, sort and overflow menus, in the window's bar or in the window.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use ui::widget::TaffyTreeExt;
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
use ui::widget::{Button, Widget, layout_rect};

use crate::command::{CommandSink, FileCommand, push};
use crate::i18n::t;
use crate::model::ViewMode;
use crate::path_bar::PathBar;
use ui::widget::DrawChild;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NavAvailability {
    pub back: bool,
    pub forward: bool,
    pub up: bool,
}

#[derive(Clone, Debug)]
struct Context {
    nav: NavAvailability,
    view_mode: ViewMode,
    archive_read_only: bool,
    current: std::path::PathBuf,
    home: Option<std::path::PathBuf>,
    workspace_roots: Vec<std::path::PathBuf>,
    edit_requested: bool,
}

#[derive(Default)]
struct Published {
    up: Cell<Option<Rect>>,
    crumbs: RefCell<Vec<(Rect, std::path::PathBuf)>>,
    view_menu: Cell<Option<Rect>>,
    sort_menu: Cell<Option<Rect>>,
    area: Cell<Rect>,
    editing: Cell<bool>,
    compact: Cell<bool>,
    compact_below: Cell<f64>,
    structure_dirty: Cell<bool>,
}

struct Shared {
    context: RefCell<Context>,
    published: Published,
}

#[derive(Clone)]
pub struct ToolbarHandle(Rc<Shared>);

impl ToolbarHandle {
    pub fn set_context(&self, nav: NavAvailability, view_mode: ViewMode, archive_read_only: bool) {
        let mut context = self.0.context.borrow_mut();
        context.nav = nav;
        context.view_mode = view_mode;
        context.archive_read_only = archive_read_only;
    }

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

    pub fn set_bar_anchor(&self, area: Rect) {
        self.0.published.area.set(area);
        self.0.published.view_menu.set(None);
        self.0.published.sort_menu.set(None);
        self.0.published.up.set(None);
        self.0.published.crumbs.borrow_mut().clear();
        self.0.published.editing.set(false);
    }

    pub fn begin_path_edit(&self) {
        self.0.context.borrow_mut().edit_requested = true;
        self.0.published.editing.set(true);
    }

    pub fn is_path_editing(&self) -> bool {
        self.0.published.editing.get()
    }

    pub fn view_menu_anchor(&self) -> Point {
        self.view_menu_rect().map_or_else(
            || {
                let area = self.0.published.area.get();
                Point::new(area.x1, area.y1)
            },
            |r| Point::new(r.x1, r.y1 + 2.0),
        )
    }

    pub fn view_menu_rect(&self) -> Option<Rect> {
        self.0.published.view_menu.get()
    }

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

    pub fn sort_menu_rect(&self) -> Option<Rect> {
        self.0.published.sort_menu.get()
    }

    pub fn crumb_at(&self, point: vello::kurbo::Point) -> Option<(std::path::PathBuf, Rect)> {
        self.0
            .published
            .crumbs
            .borrow()
            .iter()
            .find(|(rect, _)| rect.contains(point))
            .map(|(rect, path)| (path.clone(), *rect))
    }

    pub fn up_button_rect(&self) -> Option<Rect> {
        self.0.published.up.get()
    }

    pub fn is_compact(&self) -> bool {
        self.0.published.compact.get()
    }

    pub fn set_content_width(&self, width: f64) {
        let compact = width < self.0.published.compact_below.get();
        if self.0.published.compact.replace(compact) != compact {
            self.0.published.structure_dirty.set(true);
        }
    }

    pub fn can_go_forward(&self) -> bool {
        self.0.context.borrow().nav.forward
    }
}

struct Controls {
    sink: CommandSink,
    shared: Rc<Shared>,
}

impl Controls {
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
        Button::icon_only(id, icon, tip)
            .with_enabled(enabled)
            .on_effect(move || push(&sink, cmd.clone()))
            .into_widget()
    }

    fn build_left(&self, compact: bool) -> Vec<Box<dyn Widget>> {
        let nav = self.shared.context.borrow().nav;
        let mut controls = vec![self.nav_button(
            "files.back",
            Icon::ArrowLeft,
            FileCommand::Back,
            nav.back,
            &t("files-back"),
        )];
        if !compact {
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

    fn build_right(&self, compact: bool) -> Vec<Box<dyn Widget>> {
        let (view_mode, archive_read_only) = {
            let context = self.shared.context.borrow();
            (context.view_mode, context.archive_read_only)
        };
        let mut out: Vec<Box<dyn Widget>> = Vec::new();

        if !compact {
            let view_sink = self.sink.clone();
            let selected = match view_mode {
                ViewMode::List => 0,
                ViewMode::Grid => 1,
            };
            out.push(
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
                })
                .into_widget(),
            );

            // Grid mode has no clickable column header, so it gets a labelled
            // Sort menu button. List mode sorts by clicking a header.
            if view_mode == ViewMode::Grid {
                let sink = self.sink.clone();
                out.push(
                    Button::new("files.sort_menu", &t("files-sort-by"))
                        .with_icon(Icon::ArrowUpDown)
                        .on_effect(move || push(&sink, FileCommand::OpenSortMenu))
                        .into_widget(),
                );
            }
        }

        // Archives browse like folders, but extraction is their primary
        // mutation. Keep it directly reachable rather than burying it in an
        // overflow; compact layouts retain the same control as an icon with a
        // spoken/hover label so the location field still has room.
        if archive_read_only {
            let sink = self.sink.clone();
            let extract_label = t("files-archive-extract");
            let button = if compact {
                Button::icon_only("files.archive.extract", Icon::Download, &extract_label)
            } else {
                Button::new("files.archive.extract", &extract_label).with_icon(Icon::Download)
            };
            out.push(
                button
                    .on_effect(move || {
                        push(&sink, FileCommand::ExtractCurrentArchive);
                    })
                    .into_widget(),
            );
        }

        if !archive_read_only {
            let sink = self.sink.clone();
            out.push(
                Button::icon_only(
                    "files.view_menu",
                    Icon::MoreHorizontal,
                    &t("files-view-options"),
                )
                .on_effect(move || push(&sink, FileCommand::OpenViewMenu))
                .into_widget(),
            );
        }
        out
    }
}

fn row_style(sp: &ui::tokens::spacing::Spacing, grows: bool) -> Style {
    Style {
        display: Display::Flex,
        flex_direction: FlexDirection::Row,
        align_items: Some(AlignItems::Center),
        gap: Size {
            width: LengthPercentage::length(sp.s2),
            height: LengthPercentage::length(0.0),
        },
        min_size: Size {
            width: Dimension::length(0.0),
            height: Dimension::auto(),
        },
        flex_grow: if grows { 1.0 } else { 0.0 },
        flex_shrink: if grows { 1.0 } else { 0.0 },
        ..Default::default()
    }
}

pub struct FileToolbar {
    controls: Controls,
    path_bar: PathBar,
    left: Vec<Box<dyn Widget>>,
    rect: Rect,
}

pub struct ToolbarActions {
    controls: Controls,
    right: Vec<Box<dyn Widget>>,
    rect: Rect,
}

impl FileToolbar {
    pub fn new(sink: CommandSink) -> (Self, ToolbarActions, ToolbarHandle) {
        let shared = Rc::new(Shared {
            context: RefCell::new(Context {
                nav: NavAvailability::default(),
                view_mode: ViewMode::List,
                archive_read_only: false,
                current: std::path::PathBuf::new(),
                home: None,
                workspace_roots: Vec::new(),
                edit_requested: false,
            }),
            published: Published::default(),
        });
        // Start compact: the first layout does not yet know how wide it is.
        shared.published.compact.set(true);
        let leading = Self {
            path_bar: PathBar::new(sink.clone()),
            controls: Controls {
                sink: sink.clone(),
                shared: Rc::clone(&shared),
            },
            left: Vec::new(),
            rect: Rect::ZERO,
        };
        let actions = ToolbarActions {
            controls: Controls {
                sink,
                shared: Rc::clone(&shared),
            },
            right: Vec::new(),
            rect: Rect::ZERO,
        };
        (leading, actions, ToolbarHandle(shared))
    }
}

impl Widget for FileToolbar {
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        {
            let shared = &self.controls.shared;
            let mut context = shared.context.borrow_mut();
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
        self.controls
            .shared
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
        self.controls
            .shared
            .published
            .compact_below
            .set(f64::from(theme.tokens.metrics.content_min_width));
        let compact = self.controls.shared.published.compact.get();
        self.left = self.controls.build_left(compact);
        let mut child_nodes: Vec<NodeId> = Vec::new();
        for c in &mut self.left {
            child_nodes.push(c.build_layout(taffy, theme, node_map));
        }
        child_nodes.push(self.path_bar.build_layout(taffy, theme, node_map));
        taffy.widget_node(
            row_style(&theme.tokens.spacing, true),
            &child_nodes,
            node_map,
            self,
        )
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = layout_rect(taffy, node, origin);
        let base = Point::new(self.rect.x0, self.rect.y0);
        if let Ok(children) = taffy.children(node) {
            let nleft = self.left.len();
            for (c, &n) in self.left.iter_mut().zip(children.iter()) {
                c.apply_layout(taffy, n, base);
            }
            if let Some(&pn) = children.get(nleft) {
                self.path_bar.apply_layout(taffy, pn, base);
            }
        }
        let published = &self.controls.shared.published;
        published
            .up
            .set(self.left.last().map(|button| button.get_rect()));
        published.editing.set(self.path_bar.is_editing());
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
            c.draw_child(scene, transform, tr, theme, presence);
        }
        self.path_bar
            .draw_child(scene, transform, tr, theme, presence);
        self.controls
            .shared
            .published
            .crumbs
            .replace(self.path_bar.crumb_targets());
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
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
        let nleft = self.left.len();
        let Some((left_ns, rest)) = children.split_at_checked(nleft) else {
            return;
        };
        let Some(&path_n) = rest.first() else {
            return;
        };
        self.path_bar
            .handle_event(event, &mut ctx.reborrow(ctx.path, path_n));
        self.controls
            .shared
            .published
            .editing
            .set(self.path_bar.is_editing());
        for (c, &n) in self.left.iter_mut().zip(left_ns.iter()) {
            if *ctx.handled {
                break;
            }
            c.handle_event(event, &mut ctx.reborrow(ctx.path, n));
        }
    }

    fn collect_damage(&self, regions: &mut Vec<vello::kurbo::Rect>) {
        for child in &self.left {
            child.collect_damage(regions);
        }
        self.path_bar.collect_damage(regions);
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut v: Vec<&dyn Widget> = self.left.iter().map(std::convert::AsRef::as_ref).collect();
        v.push(&self.path_bar);
        v
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut v: Vec<&mut (dyn Widget + '_)> = Vec::new();
        for c in &mut self.left {
            v.push(c.as_mut());
        }
        v.push(&mut self.path_bar);
        v
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.path_bar.cursor_shape(point)
    }

    fn take_structure_dirty(&mut self) -> bool {
        let compact = self.controls.shared.published.structure_dirty.take();
        compact || self.path_bar.take_structure_dirty()
    }

    fn name(&self) -> &'static str {
        "FileToolbar"
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl Widget for ToolbarActions {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let compact = self.controls.shared.published.compact.get();
        self.right = self.controls.build_right(compact);
        let child_nodes: Vec<NodeId> = self
            .right
            .iter_mut()
            .map(|c| c.build_layout(taffy, theme, node_map))
            .collect();
        taffy.widget_node(
            row_style(&theme.tokens.spacing, false),
            &child_nodes,
            node_map,
            self,
        )
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = layout_rect(taffy, node, origin);
        let base = Point::new(self.rect.x0, self.rect.y0);
        if let Ok(children) = taffy.children(node) {
            for (c, &n) in self.right.iter_mut().zip(children.iter()) {
                c.apply_layout(taffy, n, base);
            }
        }
        let published = &self.controls.shared.published;
        published.area.set(self.rect);
        let context = self.controls.shared.context.borrow();
        published.view_menu.set(
            (!context.archive_read_only)
                .then(|| self.right.last().map(|button| button.get_rect()))
                .flatten(),
        );
        let compact = published.compact.get();
        published.sort_menu.set(
            (!compact && context.view_mode == ViewMode::Grid)
                .then(|| self.right.get(1).map(|button| button.get_rect()))
                .flatten(),
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        for c in &mut self.right {
            c.draw_child(scene, transform, tr, theme, presence);
        }
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        let Ok(children) = ctx.taffy.children(ctx.node_id) else {
            return;
        };
        for (c, &n) in self.right.iter_mut().zip(children.iter()) {
            if *ctx.handled {
                break;
            }
            c.handle_event(event, &mut ctx.reborrow(ctx.path, n));
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.right.iter().map(std::convert::AsRef::as_ref).collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut v: Vec<&mut (dyn Widget + '_)> = Vec::new();
        for c in &mut self.right {
            v.push(c.as_mut());
        }
        v
    }

    fn name(&self) -> &'static str {
        "ToolbarActions"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}
