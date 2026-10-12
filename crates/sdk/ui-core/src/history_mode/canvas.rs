//! Opaque canvas; the domain renderer never enters the live Widget/action tree.
use super::{HistoryPreview, PreviewNavigation};
use crate::{
    presence::PresenceCtx,
    text_render::TextRenderer,
    theme::Theme,
    widget::{TaffyTreeExt, Widget},
};
use std::collections::HashMap;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::{
    Scene,
    kurbo::{Affine, Point, Rect},
};
use winit::event::{MouseScrollDelta, WindowEvent};

pub(crate) struct PreviewCanvas {
    pub(crate) preview: Option<Box<dyn HistoryPreview>>,
    rect: Rect,
    cursor: Point,
    error: Option<String>,
    pub(super) status: Option<String>,
    dirty: bool,
}
impl PreviewCanvas {
    pub(crate) fn install(&mut self, result: Result<Box<dyn HistoryPreview>, String>) {
        self.status = None;
        match result {
            Ok(preview) => {
                self.preview = Some(preview);
                self.error = None;
            }
            Err(error) => {
                self.preview = None;
                self.error = Some(error);
            }
        }
        self.dirty = true;
    }
    pub(crate) fn clear(&mut self) {
        self.preview = None;
        self.error = None;
        self.status = None;
        self.dirty = true;
    }
    pub(crate) fn set_status(&mut self, status: Option<String>) {
        if self.status != status {
            self.status = status;
            self.dirty = true;
        }
    }
    pub(crate) fn navigate(&mut self, command: PreviewNavigation) -> bool {
        let handled = self
            .preview
            .as_mut()
            .is_some_and(|preview| preview.navigate(command));
        self.dirty |= handled;
        handled
    }
    pub(crate) fn new() -> Self {
        Self {
            preview: None,
            rect: Rect::ZERO,
            cursor: Point::new(-1.0, -1.0),
            error: None,
            status: None,
            dirty: true,
        }
    }
}
impl Widget for PreviewCanvas {
    fn name(&self) -> &'static str {
        "Historical content"
    }
    fn test_id(&self) -> Option<&str> {
        Some("history-preview")
    }
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
        let rect = crate::widget::layout_rect(tree, node, origin);
        if self.rect != rect {
            self.rect = rect;
            self.dirty = true;
        }
    }
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "floating point display geometry is non-authoritative and does not overflow integers"
    )]
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
        if let Some(message) = self.error.as_ref().or(self.status.as_ref()) {
            text.draw_text(
                scene,
                message,
                16.0,
                theme.text_primary,
                transform * Affine::translate((self.rect.x0 + 16.0, self.rect.y0 + 20.0)),
                None,
            );
        }
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        if *ctx.handled {
            return;
        }
        if let WindowEvent::MouseWheel { delta, .. } = event
            && self.rect.contains(self.cursor)
        {
            let (dx, dy) = match delta {
                MouseScrollDelta::LineDelta(x, y) => (f64::from(*x) * 32.0, f64::from(*y) * 32.0),
                MouseScrollDelta::PixelDelta(point) => (point.x, point.y),
            };
            *ctx.handled = self.navigate(PreviewNavigation::Scroll { dx, dy });
        }
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        Some(
            crate::a11y::AccessNode::new(crate::a11y::Role::Document).with_name(
                self.error
                    .clone()
                    .or_else(|| self.status.clone())
                    .unwrap_or_else(|| {
                        self.preview.as_ref().map_or_else(
                            || {
                                crate::i18n::I18nManager::lookup_plain(
                                    "history-mode-content-unavailable",
                                )
                            },
                            |preview| preview.accessibility().description,
                        )
                    }),
            ),
        )
    }
    fn needs_redraw(&self) -> bool {
        self.dirty
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
    }
}
