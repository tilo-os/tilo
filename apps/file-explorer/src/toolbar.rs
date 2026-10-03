//! `FileToolbar` — the file manager's single, calm top bar.
//!
//! One row, no banding:
//!
//! ```text
//! [ ← → ↑ ]   [ Home › Projects › tilo ················· ]   [ List | Grid ]  ( Sort by )  [ ⋯ ]
//! [ ← ↑ ]     [ Home › Projects › tilo ················· ]                         [ ⋯ ]
//! ```
//!
//! * a nav cluster (back / forward / up) — the only icon-only controls, and
//!   universally understood arrows;
//! * the [`PathBar`] (breadcrumb that becomes an editable path field), which
//!   grows to fill the middle;
//! * a labelled `List | Grid` view switch;
//! * a labelled **Sort** menu button, shown only in grid mode (list mode sorts
//!   by clicking a column header);
//! * a **⋯** overflow that opens the labelled "View options" menu (new folder /
//!   file, show-hidden toggle, terminal).
//!
//! There is **no search field** (search is OS-owned, `crates/sdk/ui-core/HIG.md` §5), **no
//! reload button**, and **no unlabelled state-toggle icons** — the two cryptic
//! symbolic controls and the mystery show-hidden toggle are gone; show-hidden
//! now lives, clearly labelled with a check, inside the View menu.
//!
//! Because the [`PathBar`] holds live edit state (the in-progress path text +
//! completion popover) that must survive the file manager's rebuild-each-frame
//! composition, `FileToolbar` is held **persistently** by the app (like the
//! body shell), and its stateless button children are rebuilt each layout.

use std::collections::HashMap;
use ui::widget::{AppModel, ModelPath};

use taffy::{
    AlignItems, Dimension, Display, FlexDirection, LengthPercentage, NodeId, Rect as TaffyRect,
    Size, Style, TaffyTree,
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

/// Compact nav-button size.
const NAV_W: f32 = 36.0;
const NAV_H: f32 = 32.0;
/// Below this width, navigation and location remain visible while view/sort
/// choices move into the one labelled overflow. Keeping desktop controls in a
/// 390 px row made the path and segmented control overlap.
// Full mode needs room for three navigation controls, a readable location,
// the labelled view switch, optional sort control, overflow, gaps, and safe
// edge padding.  Switching at 520 px let those fixed controls consume the
// breadcrumb at tablet/narrow-desktop widths (notably 560 px).  The compact
// overflow preserves every operation while protecting the primary location
// and navigation affordances through this entire in-between range.
const COMPACT_BREAKPOINT_PX: f64 = 760.0;

/// Which history and hierarchy moves the current location allows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NavAvailability {
    pub back: bool,
    pub forward: bool,
    pub up: bool,
}

pub struct FileToolbar {
    sink: CommandSink,
    /// The editable path bar (breadcrumb ⇄ text field). Persistent so an
    /// in-progress path edit survives the app's per-frame rebuild.
    path_bar: PathBar,
    // Live context, refreshed each rebuild via `set_context`.
    nav: NavAvailability,
    view_mode: ViewMode,
    archive_read_only: bool,
    // Stateless children, rebuilt each `build_layout`.
    left: Vec<Box<dyn Widget>>,
    right: Vec<Box<dyn Widget>>,
    rect: Rect,
    /// Derived from the last laid-out width. A change marks the toolbar dirty
    /// so the next composition uses the appropriate child set.
    compact: bool,
    structure_dirty: bool,
}

impl FileToolbar {
    pub fn new(sink: CommandSink) -> Self {
        Self {
            path_bar: PathBar::new(sink.clone()),
            sink,
            nav: NavAvailability::default(),
            view_mode: ViewMode::List,
            archive_read_only: false,
            left: Vec::new(),
            right: Vec::new(),
            rect: Rect::ZERO,
            // Start compact: an initial layout does not yet know its viewport.
            // A wide window promotes the full controls on the next frame;
            // narrow windows are correct from their very first frame.
            compact: true,
            structure_dirty: false,
        }
    }

    /// Refresh the live context (called before each rebuild).
    pub fn set_context(
        &mut self,
        nav: NavAvailability,
        view_mode: ViewMode,
        archive_read_only: bool,
    ) {
        self.nav = nav;
        self.view_mode = view_mode;
        self.archive_read_only = archive_read_only;
    }

    /// Show `current` in the path bar, abbreviating under `home` and naming
    /// any of `workspace_roots` it lies in (called before each rebuild).
    pub fn set_path(
        &mut self,
        current: std::path::PathBuf,
        home: Option<std::path::PathBuf>,
        workspace_roots: Vec<std::path::PathBuf>,
    ) {
        self.path_bar.set_path(current, home, workspace_roots);
    }

    /// Put the path bar into editable mode (the Ctrl+L "edit location" path).
    pub fn begin_path_edit(&mut self) {
        self.path_bar.begin_edit();
    }

    /// Whether the path bar is currently being edited (so the app leaves the
    /// keyboard to it).
    pub fn is_path_editing(&self) -> bool {
        self.path_bar.is_editing()
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
        Box::new(
            Button::new(id, "", None)
                .with_icon(icon)
                .with_size(NAV_W, NAV_H)
                .with_enabled(enabled)
                // The glyph alone is ambiguous; the tooltip labels it on hover
                // and doubles as the button's accessible name.
                .tooltip(tip)
                .on_effect(move || push(&sink, cmd.clone())),
        )
    }

    fn build_left(&self) -> Vec<Box<dyn Widget>> {
        let mut controls = vec![self.nav_button(
            "files.back",
            Icon::ArrowLeft,
            FileCommand::Back,
            self.nav.back,
            &t("files-back"),
        )];
        if !self.compact {
            controls.push(self.nav_button(
                "files.forward",
                Icon::ArrowRight,
                FileCommand::Forward,
                self.nav.forward,
                &t("files-forward"),
            ));
        }
        controls.push(self.nav_button(
            "files.up",
            Icon::ArrowUp,
            FileCommand::Up,
            self.nav.up,
            &t("files-up"),
        ));
        controls
    }

    fn build_right(&self) -> Vec<Box<dyn Widget>> {
        let mut out: Vec<Box<dyn Widget>> = Vec::new();

        if !self.compact {
            // View switch — labelled, so the active view is obvious.
            let view_sink = self.sink.clone();
            let selected = match self.view_mode {
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

            // Grid mode has no clickable column header, so it gets a labelled Sort
            // menu button. List mode sorts by clicking a header, so it needs none.
            if self.view_mode == ViewMode::Grid {
                let sink = self.sink.clone();
                out.push(Box::new(
                    Button::new("files.sort_menu", &t("files-sort-by"), None)
                        .with_icon(Icon::ArrowUpDown)
                        .with_size(104.0, NAV_H)
                        .on_effect(move || push(&sink, FileCommand::OpenSortMenu)),
                ));
            }
        }

        // Archives browse like folders, but extraction is their primary
        // mutation. Keep it directly reachable rather than burying it in an
        // overflow; compact layouts retain the same 36px control with a
        // spoken/hover label so the location field still has room.
        if self.archive_read_only {
            let sink = self.sink.clone();
            let extract_label = t("files-archive-extract");
            let button = Button::new(
                "files.archive.extract",
                if self.compact { "" } else { &extract_label },
                None,
            )
            .with_icon(Icon::Download)
            .with_size(if self.compact { NAV_W } else { 104.0 }, NAV_H)
            .with_accessible_name(&extract_label)
            .tooltip(extract_label)
            .on_effect(move || push(&sink, FileCommand::ExtractCurrentArchive));
            out.push(Box::new(button));
        }

        // Archive browsing is read-only and already has a direct Extract
        // control. Its overflow would contain only a duplicate Extract row,
        // so omit the no-choice menu entirely.
        if !self.archive_read_only {
            // Overflow: the labelled View-options menu (new folder/file, show
            // hidden, terminal). Every action it reveals is spelled out.
            let sink = self.sink.clone();
            out.push(Box::new(
                Button::new("files.view_menu", "", None)
                    .with_icon(Icon::MoreHorizontal)
                    .with_size(NAV_W, NAV_H)
                    .with_accessible_name(&t("files-view-options"))
                    .tooltip(t("files-view-options"))
                    .on_effect(move || push(&sink, FileCommand::OpenViewMenu)),
            ));
        }
        out
    }

    /// Bottom-right anchor of the "View options" (⋯) button, so the app can
    /// open its menu right-aligned under the button (never off the right edge).
    /// The ⋯ is always the last control on the right.
    pub fn view_menu_anchor(&self) -> Point {
        self.view_menu_rect().map_or_else(
            || Point::new(self.rect.x1, self.rect.y1),
            |r| Point::new(r.x1, r.y1 + 2.0),
        )
    }

    /// Exact rectangle of the overflow trigger. The app uses its top edge to
    /// flip a tall menu above the trigger when a constrained viewport cannot
    /// fit the normal below-trigger placement.
    pub fn view_menu_rect(&self) -> Option<Rect> {
        (!self.archive_read_only)
            .then(|| self.right.last().map(|button| button.get_rect()))
            .flatten()
    }

    /// Bottom-right anchor of the "Sort by" button (present only in grid mode,
    /// where it is the second control on the right, before the ⋯).
    pub fn sort_menu_anchor(&self) -> Point {
        self.sort_menu_rect().map_or_else(
            || Point::new(self.rect.x1, self.rect.y1),
            |r| Point::new(r.x1, r.y1 + 2.0),
        )
    }

    /// Exact rectangle of the grid-mode Sort trigger, if it is visible.
    pub fn sort_menu_rect(&self) -> Option<Rect> {
        self.right
            .get(1)
            .or_else(|| self.right.last())
            .map(|button| button.get_rect())
    }

    /// The Up nav button's on-screen rect, so a drag can be dropped on it to
    /// move the carried items into the parent directory. It is the final left
    /// control in both full and compact layouts.
    pub fn up_button_rect(&self) -> Option<Rect> {
        self.left.last().map(|b| b.get_rect())
    }

    /// Whether compact navigation is active. Exposed for the Files responsive
    /// layout test; all live behavior is driven by [`Self::apply_layout`].
    pub fn is_compact(&self) -> bool {
        self.compact
    }

    /// Whether a compact overflow should offer Forward navigation.
    pub fn can_go_forward(&self) -> bool {
        self.nav.forward
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
        self.path_bar.sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let sp = &theme.tokens.spacing;
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
            padding: TaffyRect {
                left: LengthPercentage::length(sp.s3),
                right: LengthPercentage::length(sp.s3),
                top: LengthPercentage::length(sp.s2),
                bottom: LengthPercentage::length(sp.s2),
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
        let compact = self.rect.width() < COMPACT_BREAKPOINT_PX;
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
        std::mem::take(&mut self.structure_dirty) || self.path_bar.take_structure_dirty()
    }

    fn name(&self) -> &'static str {
        "FileToolbar"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}
