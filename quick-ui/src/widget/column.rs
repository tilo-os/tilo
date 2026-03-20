use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use winit::event::WindowEvent;
use taffy::{TaffyTree, NodeId, Style};
use taffy::style_helpers::{length, percent};
use crate::widget::Widget;
use crate::text_render::TextRenderer;

pub struct Column {
    children: Vec<Box<dyn Widget>>,
}

impl Column {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
        }
    }

    pub fn push(mut self, child: Box<dyn Widget>) -> Self {
        self.children.push(child);
        self
    }
}

impl Widget for Column {
    fn sync(&mut self, state: &AutoCommit, path: &ObjId) {
        for child in &mut self.children {
            child.sync(state, path);
        }
    }

    fn build_layout(&mut self, taffy: &mut TaffyTree) -> NodeId {
        let child_nodes: Vec<NodeId> = self.children
            .iter_mut()
            .map(|c| c.build_layout(taffy))
            .collect();
            
        taffy.new_with_children(
            Style {
                display: taffy::Display::Flex,
                flex_direction: taffy::FlexDirection::Column,
                align_items: Some(taffy::AlignItems::FlexStart),
                gap: taffy::Size {
                    width: length(0.0),
                    height: length(10.0),
                },
                padding: taffy::Rect {
                    left: length(20.0),
                    right: length(20.0),
                    top: length(20.0),
                    bottom: length(20.0),
                },
                size: taffy::Size {
                    width: percent(1.0),
                    height: percent(1.0),
                },
                ..Default::default()
            },
            &child_nodes
        ).unwrap()
    }

    fn apply_layout(&mut self, taffy: &TaffyTree, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        let new_origin = Point::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
        );
        
        let children = taffy.children(node).unwrap();
        for (child_widget, child_node) in self.children.iter_mut().zip(children.iter()) {
            child_widget.apply_layout(taffy, *child_node, new_origin);
        }
    }

    fn draw(&mut self, scene: &mut Scene, transform: Affine, text_renderer: &mut TextRenderer) {
        for child in &mut self.children {
            child.draw(scene, transform, text_renderer);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, state: &mut AutoCommit, path: &ObjId, handled: &mut bool) {
        for child in &mut self.children {
            child.handle_event(event, scale_factor, state, path, handled);
            if *handled {
                break;
            }
        }
    }
}
