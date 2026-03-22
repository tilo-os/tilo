use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Affine, Point, RoundedRect, Rect};
use winit::event::WindowEvent;
use taffy::{TaffyTree, NodeId, Style};
use taffy::style_helpers::{length, percent};
use crate::widget::Widget;
use crate::text_render::TextRenderer;
use crate::theme::Theme;

pub struct Container {
    child: Option<Box<dyn Widget>>,
    rect: Rect,
    has_background: bool,
}

impl Container {
    pub fn new() -> Self {
        Self {
            child: None,
            rect: Rect::ZERO,
            has_background: false,
        }
    }

    pub fn with_child(mut self, child: Box<dyn Widget>) -> Self {
        self.child = Some(child);
        self
    }

    pub fn with_background(mut self) -> Self {
        self.has_background = true;
        self
    }
}

impl Widget for Container {
    fn sync(&mut self, state: &AutoCommit, path: &ObjId) {
        if let Some(child) = &mut self.child {
            child.sync(state, path);
        }
    }

    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, theme: &Theme, node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let child_node = self.child.as_mut().map(|c| c.build_layout(taffy, theme, node_map));
        
        let style = Style {
            display: taffy::Display::Flex,
            flex_shrink: 0.0, // Prevent vertical squashing
            padding: taffy::Rect {
                left: length(theme.padding_standard),
                right: length(theme.padding_standard),
                top: length(theme.padding_standard),
                bottom: length(theme.padding_standard),
            },
            size: taffy::Size {
                width: percent(1.0),
                height: taffy::Dimension::auto(),
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
            child.apply_layout(taffy, child_node, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    fn draw(&mut self, scene: &mut Scene, transform: Affine, text_renderer: &mut TextRenderer, theme: &Theme) {
        if self.has_background {
            let shape = RoundedRect::from_rect(self.rect, theme.radius_standard);
            scene.fill(vello::peniko::Fill::NonZero, transform, theme.bg_primary, None, &shape);
            scene.stroke(&vello::kurbo::Stroke::new(1.0), transform, theme.border_standard, None, &shape);
        }

        if let Some(child) = &mut self.child {
            child.draw(scene, transform, text_renderer, theme);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, state: &mut AutoCommit, path: &ObjId, handled: &mut bool, focus_target: &mut Option<NodeId>, node_id: NodeId, taffy: &TaffyTree<()>) {
        if let Some(child) = &mut self.child {
            let child_node = taffy.children(node_id).unwrap()[0];
            child.handle_event(event, scale_factor, state, path, handled, focus_target, child_node, taffy);
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.child.as_ref().map(|c| c.as_ref() as &dyn Widget).into_iter().collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.child.as_mut().map(|c| c.as_mut() as &mut (dyn Widget + '_)).into_iter().collect()
    }

    fn get_rect(&self) -> vello::kurbo::Rect {
        self.rect
    }

    fn name(&self) -> &str { "Container" }

    fn is_focused(&self) -> bool { false }
    fn needs_commit(&self) -> bool { self.children().iter().any(|c| c.needs_commit()) }
    fn on_commit(&mut self) { for child in self.children_mut() { child.on_commit(); } }
}
