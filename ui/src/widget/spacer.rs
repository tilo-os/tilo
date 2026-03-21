use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;
use taffy::{TaffyTree, NodeId, Style};
use crate::widget::Widget;
use crate::text_render::TextRenderer;
use crate::theme::Theme;

pub struct Spacer {
    rect: Rect,
}

impl Spacer {
    pub fn new() -> Self {
        Self {
            rect: Rect::ZERO,
        }
    }
}

impl Widget for Spacer {
    fn sync(&mut self, _state: &AutoCommit, _path: &ObjId) {}

    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, _theme: &Theme, node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let node = taffy.new_leaf(Style {
            flex_grow: 1.0,
            flex_shrink: 1.0,
            ..Default::default()
        }).unwrap();
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
    }

    fn draw(&mut self, _scene: &mut Scene, _transform: Affine, _text_renderer: &mut TextRenderer, _theme: &Theme) {}

    fn handle_event(&mut self, _event: &WindowEvent, _scale_factor: f64, _state: &mut AutoCommit, _path: &ObjId, _handled: &mut bool) {}

    fn name(&self) -> &str { "Spacer" }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}
