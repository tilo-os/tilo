//! Desktop backdrop widget — paints the nirios desktop background behind
//! all shell content.
//!
//! Matches `nirios-concept/src/components/DesktopView.tsx:210-217`:
//!   1. Linear gradient top-left → bottom-right, slate-900 (#0f172a) to
//!      slate-950 (#020617).
//!   2. 24×24 px grid of 1px lines at `rgba(128, 128, 128, 0.07)`.
//!   3. (Noise SVG overlay skipped for now — requires an image resource.)
//!
//! Structurally, this widget is a thin `Container`-style wrapper around an
//! optional single child: it forwards sync/layout/events/presence/commit
//! exactly like [`quick_ui::widget::Container`], but with no padding and
//! no rounded-rect chrome — just the flat gradient + grid beneath the
//! child.

use automerge::{AutoCommit, ObjId};
use quick_ui::presence::{PresenceCtx, PresenceKey};
use quick_ui::permissions::{PermissionContext, PermissionScope};
use quick_ui::text_render::TextRenderer;
use quick_ui::theme::Theme;
use quick_ui::widget::Widget;
use taffy::style_helpers::percent;
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, Stroke};
use vello::peniko::{Brush, Color, ColorStop, ColorStops, Extend, Fill, Gradient, GradientKind};
use winit::event::{Modifiers, WindowEvent};

/// Size (in logical pixels) of one grid cell.
const GRID_STEP: f64 = 24.0;

/// Widget that paints the nirios desktop backdrop (gradient + grid) behind
/// its single child.
pub struct DesktopBackdrop {
    child: Option<Box<dyn Widget>>,
    rect: Rect,
}

impl DesktopBackdrop {
    pub fn new() -> Self {
        Self {
            child: None,
            rect: Rect::ZERO,
        }
    }

    pub fn with_child(mut self, child: Box<dyn Widget>) -> Self {
        self.child = Some(child);
        self
    }

    fn paint_backdrop(&self, scene: &mut Scene, transform: Affine) {
        // 1. Diagonal gradient: top-left slate-900 → bottom-right slate-950.
        let gradient = Gradient {
            kind: GradientKind::Linear {
                start: Point::new(self.rect.x0, self.rect.y0),
                end: Point::new(self.rect.x1, self.rect.y1),
            },
            extend: Extend::Pad,
            stops: ColorStops::from_iter([
                ColorStop {
                    offset: 0.0,
                    color: Color::rgb8(0x0f, 0x17, 0x2a),
                },
                ColorStop {
                    offset: 1.0,
                    color: Color::rgb8(0x02, 0x06, 0x17),
                },
            ]),
        };
        scene.fill(
            Fill::NonZero,
            transform,
            &Brush::Gradient(gradient),
            None,
            &self.rect,
        );

        // 2. Grid — 1px lines at rgba(128, 128, 128, 0.07) every 24px.
        //    Clip to self.rect so lines don't bleed outside the backdrop.
        let line_color = Color::rgba8(128, 128, 128, 18); // 0.07 * 255 ≈ 18
        let stroke = Stroke::new(1.0);

        scene.push_layer(
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &self.rect,
        );

        // Vertical lines.
        let mut x = (self.rect.x0 / GRID_STEP).ceil() * GRID_STEP;
        while x < self.rect.x1 {
            let line = Line::new((x, self.rect.y0), (x, self.rect.y1));
            scene.stroke(&stroke, transform, line_color, None, &line);
            x += GRID_STEP;
        }

        // Horizontal lines.
        let mut y = (self.rect.y0 / GRID_STEP).ceil() * GRID_STEP;
        while y < self.rect.y1 {
            let line = Line::new((self.rect.x0, y), (self.rect.x1, y));
            scene.stroke(&stroke, transform, line_color, None, &line);
            y += GRID_STEP;
        }

        scene.pop_layer();
    }
}

impl Default for DesktopBackdrop {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for DesktopBackdrop {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        if let Some(child) = &mut self.child {
            child.sync(state, path);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let child_node = self
            .child
            .as_mut()
            .map(|c| c.build_layout(taffy, theme, node_map));

        let style = Style {
            display: taffy::Display::Flex,
            flex_shrink: 0.0,
            size: taffy::Size {
                width: percent(1.0),
                height: percent(1.0),
            },
            ..Default::default()
        };

        let node = if let Some(child) = child_node {
            taffy.new_with_children(style, &[child]).unwrap()
        } else {
            taffy.new_leaf(style).unwrap()
        };
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );

        if let Some(child) = &mut self.child {
            let child_node = taffy.children(node).unwrap()[0];
            child.apply_layout(
                taffy,
                child_node,
                Point::new(self.rect.x0, self.rect.y0),
            );
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
        self.paint_backdrop(scene, transform);
        if let Some(child) = &mut self.child {
            child.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        permissions: &PermissionContext,
    ) {
        self.paint_backdrop(scene, transform);
        if let Some(child) = &mut self.child {
            child.draw_with_presence(scene, transform, text_renderer, theme, presence, permissions);
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if let Some(child) = &mut self.child {
            let child_node = taffy.children(node_id).unwrap()[0];
            child.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                child_node,
                taffy,
            );
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.child
            .as_ref()
            .map(|c| c.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.child
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn name(&self) -> &str {
        "DesktopBackdrop"
    }

    fn is_focused(&self) -> bool {
        false
    }

    fn needs_commit(&self) -> bool {
        self.children().iter().any(|c| c.needs_commit())
    }

    fn on_commit(&mut self) {
        for child in self.children_mut() {
            child.on_commit();
        }
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.child
            .as_mut()
            .map(|c| c.take_structure_dirty())
            .unwrap_or(false)
    }

    fn presence_key(&self) -> Option<PresenceKey> {
        None
    }

    fn permission_scope(&self) -> Option<PermissionScope> {
        None
    }
}
