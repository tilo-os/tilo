//! Opaque canvas; the domain renderer never enters the live Widget/action tree.
use super::{HistoryPreview, PreviewNavigation};
use crate::{
    presence::PresenceCtx,
    text_render::TextRenderer,
    theme::Theme,
    widget::{TaffyTreeExt, Widget},
};
use automerge::{AutoCommit, ObjId};
use std::collections::HashMap;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::{
    Scene,
    kurbo::{Affine, Point, Rect},
};
use winit::event::{MouseScrollDelta, WindowEvent};

pub(super) struct PreviewCanvas {
    pub(super) preview: Option<Box<dyn HistoryPreview>>,
    rect: Rect,
    cursor: Point,
    error: Option<String>,
}
impl PreviewCanvas {
    pub(super) fn new() -> Self {
        Self {
            preview: None,
            rect: Rect::ZERO,
            cursor: Point::new(-1.0, -1.0),
            error: None,
        }
    }
}
impl Widget for PreviewCanvas {
    fn name(&self) -> &str {
        "Historical content"
    }
    fn test_id(&self) -> Option<&str> {
        Some("history-preview")
    }
    fn sync(&mut self, _: &mut AutoCommit, _: &ObjId) {}
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        _: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = tree.leaf_node(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::auto(),
            },
            min_size: Size {
                width: Dimension::length(0.0),
                height: Dimension::length(0.0),
            },
            flex_grow: 1.0,
            ..Default::default()
        });
        nodes.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = tree.layout(node).copied().unwrap_or_default();
        self.rect = Rect::from_origin_size(
            (
                origin.x + f64::from(layout.location.x),
                origin.y + f64::from(layout.location.y),
            ),
            (f64::from(layout.size.width), f64::from(layout.size.height)),
        );
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _: &PresenceCtx,
    ) {
        if let Some(preview) = &mut self.preview {
            match preview.layout(self.rect, text, theme) {
                Ok(()) => {
                    self.error = None;
                    preview.draw(scene, transform, text, theme);
                }
                Err(error) => self.error = Some(error),
            }
        }
    }
    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        _: &winit::event::Modifiers,
        _: &mut AutoCommit,
        _: &ObjId,
        handled: &mut bool,
        _: &mut Option<NodeId>,
        _: NodeId,
        _: &TaffyTree<()>,
    ) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = Point::new(position.x / scale, position.y / scale);
        }
        if *handled {
            return;
        }
        if let WindowEvent::MouseWheel { delta, .. } = event
            && self.rect.contains(self.cursor)
        {
            let (dx, dy) = match delta {
                MouseScrollDelta::LineDelta(x, y) => (f64::from(*x) * 32.0, f64::from(*y) * 32.0),
                MouseScrollDelta::PixelDelta(point) => (point.x, point.y),
            };
            if let Some(preview) = &mut self.preview {
                *handled = preview.navigate(PreviewNavigation::Scroll { dx, dy });
            }
        }
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        Some(
            crate::a11y::AccessNode::new(crate::a11y::Role::Document).with_name(
                self.error.clone().unwrap_or_else(|| {
                    self.preview.as_ref().map_or_else(
                        || "Historical content unavailable".into(),
                        |preview| preview.accessibility().description,
                    )
                }),
            ),
        )
    }
}
