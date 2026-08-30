//! The project file-tree sidebar: a collapsible, icon-led tree of the open
//! project. Drawn directly into the scene for a dense explorer look. Emits
//! [`TreeEvent`]s the app drains each frame (open a file, toggle a folder).

use automerge::{AutoCommit, ObjId};
use std::collections::HashMap;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};

use crate::model::{NodeId as TreeId, NodeKind, Project};

const ROW_HEIGHT: f64 = 24.0;
const HEADER_HEIGHT: f64 = 38.0;
const INDENT: f64 = 14.0;
const ICON_PAD: f64 = 10.0;

/// What the user did in the tree, for the app to apply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeEvent {
    OpenFile(TreeId),
    ToggleFolder(TreeId),
}

pub struct FileTree {
    rect: Rect,
    /// Snapshot of the visible rows (node ids) computed in `set_project`.
    rows: Vec<TreeId>,
    /// Cached node metadata for the rows: (id, name, depth, `is_folder`,
    /// expanded). Decoupled from `Project` so `draw` doesn't borrow it.
    row_meta: Vec<RowMeta>,
    project_name: String,
    /// Which file row is the active/open buffer (highlighted).
    active: Option<TreeId>,
    hovered: Option<usize>,
    scroll_y: f64,
    pending: Vec<TreeEvent>,
    last_cursor: Point,
    dirty: bool,
}

#[derive(Clone, Debug)]
struct RowMeta {
    id: TreeId,
    name: String,
    depth: usize,
    is_folder: bool,
    expanded: bool,
}

impl FileTree {
    pub fn new() -> Self {
        FileTree {
            rect: Rect::ZERO,
            rows: Vec::new(),
            row_meta: Vec::new(),
            project_name: String::new(),
            active: None,
            hovered: None,
            scroll_y: 0.0,
            pending: Vec::new(),
            last_cursor: Point::ZERO,
            dirty: true,
        }
    }

    /// Rebuild the cached row list from the project's current expansion state.
    pub fn set_project(&mut self, project: &Project) {
        self.project_name = project.name.clone();
        self.rows = project.visible_rows();
        self.row_meta = self
            .rows
            .iter()
            .filter_map(|id| project.get(*id))
            .map(|n| {
                let (is_folder, expanded) = match &n.kind {
                    NodeKind::Folder { expanded, .. } => (true, *expanded),
                    NodeKind::File { .. } => (false, false),
                };
                RowMeta {
                    id: n.id,
                    name: n.name.clone(),
                    depth: n.depth.saturating_sub(1), // root children at depth 0
                    is_folder,
                    expanded,
                }
            })
            .collect();
        self.dirty = true;
    }

    pub fn set_active(&mut self, id: Option<TreeId>) {
        if self.active != id {
            self.active = id;
            self.dirty = true;
        }
    }

    /// Override the tree's rect directly, bypassing the taffy-derived one.
    ///
    /// In the narrow/overlay responsive layout the sidebar escapes normal
    /// flow — it slides in over the content rather than occupying a column.
    /// The app computes that off-flow rect each frame from the flap's eased
    /// reveal and hands it here so both `draw` and hit-testing operate on the
    /// overlay position (the tree draws and hit-tests purely from `self.rect`).
    pub fn place_at(&mut self, rect: Rect) {
        self.rect = rect;
    }

    pub fn take_events(&mut self) -> Vec<TreeEvent> {
        std::mem::take(&mut self.pending)
    }

    fn row_at(&self, p: Point) -> Option<usize> {
        if !self.rect.contains(p) {
            return None;
        }
        let y = p.y - self.rect.y0 - HEADER_HEIGHT + self.scroll_y;
        if y < 0.0 {
            return None;
        }
        let idx = (y / ROW_HEIGHT).floor() as usize;
        if idx < self.row_meta.len() {
            Some(idx)
        } else {
            None
        }
    }
}

impl Widget for FileTree {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
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
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
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
        let c = &theme.tokens.colors;
        // Sidebar background.
        scene.fill(
            Fill::NonZero,
            transform,
            c.surfaces.raised,
            None,
            &self.rect,
        );

        // Header: "EXPLORER" + project name.
        let header_rect = Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x1,
            self.rect.y0 + HEADER_HEIGHT,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            c.surfaces.base,
            None,
            &header_rect,
        );
        text_renderer.draw_text_weighted(
            scene,
            &self.project_name.to_uppercase(),
            12.0,
            c.text_dim,
            transform * Affine::translate((self.rect.x0 + 14.0, self.rect.y0 + 12.0)),
            Some(self.rect.width() as f32),
            TextWeight::Bold,
        );

        // Rows.
        let first = (self.scroll_y / ROW_HEIGHT).floor() as usize;
        for (i, meta) in self.row_meta.iter().enumerate().skip(first) {
            let y = self.rect.y0 + HEADER_HEIGHT + i as f64 * ROW_HEIGHT - self.scroll_y;
            if y > self.rect.y1 {
                break;
            }
            if y + ROW_HEIGHT < self.rect.y0 + HEADER_HEIGHT {
                continue;
            }
            let row_rect = Rect::new(self.rect.x0, y, self.rect.x1, y + ROW_HEIGHT);

            // Active / hover background.
            if self.active == Some(meta.id) {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    c.surface_white_10,
                    None,
                    &row_rect,
                );
                // Accent rail on the left of the active row.
                let rail = Rect::new(self.rect.x0, y, self.rect.x0 + 2.0, y + ROW_HEIGHT);
                scene.fill(Fill::NonZero, transform, c.accent[5], None, &rail);
            } else if self.hovered == Some(i) {
                scene.fill(Fill::NonZero, transform, c.surface_white_5, None, &row_rect);
            }

            let base_x = self.rect.x0 + 10.0 + meta.depth as f64 * INDENT;
            let icon_cx = base_x + 6.0;
            let icon_cy = y + ROW_HEIGHT / 2.0;

            if meta.is_folder {
                // Disclosure chevron.
                draw_chevron(
                    scene,
                    transform,
                    Point::new(icon_cx, icon_cy),
                    meta.expanded,
                    c.text_dim,
                );
                // Folder glyph.
                draw_folder_icon(
                    scene,
                    transform,
                    Point::new(icon_cx + 14.0, icon_cy),
                    if meta.expanded {
                        c.accent[6]
                    } else {
                        c.text_dim
                    },
                );
            } else {
                draw_file_icon(
                    scene,
                    transform,
                    Point::new(icon_cx + 14.0, icon_cy),
                    c.text_dim,
                );
            }

            let label_x = base_x + 28.0 + ICON_PAD;
            let label_color = if self.active == Some(meta.id) {
                c.text_primary
            } else {
                c.neutral[8]
            };
            text_renderer.draw_text(
                scene,
                &meta.name,
                13.0,
                label_color,
                transform * Affine::translate((label_x, y + 4.0)),
                Some((self.rect.x1 - label_x) as f32),
            );
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
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
                let new_hover = self.row_at(self.last_cursor);
                if new_hover != self.hovered {
                    self.hovered = new_hover;
                    self.dirty = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(idx) = self.row_at(self.last_cursor) {
                    let meta = &self.row_meta[idx];
                    if meta.is_folder {
                        self.pending.push(TreeEvent::ToggleFolder(meta.id));
                    } else {
                        self.pending.push(TreeEvent::OpenFile(meta.id));
                    }
                    self.dirty = true;
                    *handled = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. } if self.rect.contains(self.last_cursor) => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(*y) * ROW_HEIGHT,
                    MouseScrollDelta::PixelDelta(p) => p.y,
                };
                let content = self.row_meta.len() as f64 * ROW_HEIGHT;
                let view = (self.rect.height() - HEADER_HEIGHT).max(0.0);
                let max = (content - view).max(0.0);
                self.scroll_y = (self.scroll_y - dy).clamp(0.0, max);
                self.dirty = true;
                *handled = true;
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "FileTree"
    }

    fn test_id(&self) -> Option<&str> {
        Some("file_tree")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }
}

/// Draw a small disclosure chevron, pointing down when `expanded`, right
/// otherwise.
fn draw_chevron(
    scene: &mut Scene,
    transform: Affine,
    center: Point,
    expanded: bool,
    color: vello::peniko::Color,
) {
    let s = 3.5;
    let mut path = BezPath::new();
    if expanded {
        path.move_to((center.x - s, center.y - s * 0.6));
        path.line_to((center.x + s, center.y - s * 0.6));
        path.line_to((center.x, center.y + s * 0.8));
    } else {
        path.move_to((center.x - s * 0.6, center.y - s));
        path.line_to((center.x + s * 0.8, center.y));
        path.line_to((center.x - s * 0.6, center.y + s));
    }
    path.close_path();
    scene.fill(Fill::NonZero, transform, color, None, &path);
}

/// A simple folder glyph (a rounded rectangle with a tab).
fn draw_folder_icon(
    scene: &mut Scene,
    transform: Affine,
    center: Point,
    color: vello::peniko::Color,
) {
    let w = 11.0;
    let h = 8.0;
    let x0 = center.x - w / 2.0;
    let y0 = center.y - h / 2.0;
    // Tab.
    let tab = Rect::new(x0, y0 - 1.5, x0 + w * 0.45, y0 + 1.5);
    scene.fill(Fill::NonZero, transform, color, None, &tab);
    let body = Rect::new(x0, y0, x0 + w, y0 + h);
    scene.fill(Fill::NonZero, transform, color, None, &body);
}

/// A simple document glyph (a page with a folded corner).
fn draw_file_icon(
    scene: &mut Scene,
    transform: Affine,
    center: Point,
    color: vello::peniko::Color,
) {
    let w = 9.0;
    let h = 11.0;
    let x0 = center.x - w / 2.0;
    let y0 = center.y - h / 2.0;
    let mut path = BezPath::new();
    let fold = 3.0;
    path.move_to((x0, y0));
    path.line_to((x0 + w - fold, y0));
    path.line_to((x0 + w, y0 + fold));
    path.line_to((x0 + w, y0 + h));
    path.line_to((x0, y0 + h));
    path.close_path();
    scene.fill(Fill::NonZero, transform, color, None, &path);
}
