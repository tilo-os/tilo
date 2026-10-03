//! Shell-level horizontal split layout: a bare taffy flex row where the left
//! child keeps its intrinsic width and the right stretches to fill the rest.
//! `ui::Row` doesn't serve — it's opinionated toward wrapping toolbar layouts.

use std::collections::HashMap;
use ui::widget::{AppModel, ModelPath};

use taffy::{Dimension, FlexDirection, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

pub struct SplitRow {
    left: Box<dyn Widget>,
    right: Box<dyn Widget>,
    rect: Rect,
}

impl SplitRow {
    pub fn new(left: Box<dyn Widget>, right: Box<dyn Widget>) -> Self {
        Self {
            left,
            right,
            rect: Rect::ZERO,
        }
    }
}

impl Widget for SplitRow {
    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        self.left.sync(state, path);
        self.right.sync(state, path);
    }

    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
        let a = self.left.update(state, path);
        let b = self.right.update(state, path);
        a || b
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let left_node = self.left.build_layout(taffy, theme, node_map);
        let right_node = self.right.build_layout(taffy, theme, node_map);

        // Force flex-grow 1 — without it taffy sizes the right child by
        // its intrinsic content width. A style lookup on a node this
        // function just inserted is not reachable to fail; if it
        // (impossibly) does, skip the override rather than panic.
        if let Ok(style) = taffy.style(right_node) {
            let mut right_style = style.clone();
            right_style.flex_grow = 1.0;
            right_style.size = Size {
                width: Dimension::auto(),
                height: Dimension::percent(1.0),
            };
            let _ = taffy.set_style(right_node, right_style);
        }

        let node = taffy
            .new_with_children(
                Style {
                    display: taffy::Display::Flex,
                    flex_direction: FlexDirection::Row,
                    align_items: Some(taffy::AlignItems::Stretch),
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::percent(1.0),
                    },
                    flex_grow: 1.0,
                    flex_shrink: 1.0,
                    ..Default::default()
                },
                &[left_node, right_node],
            )
            // Both children were just created in this same tree, so this
            // is not reachable to fail; falling back to the left child
            // alone still shows something instead of panicking.
            .unwrap_or(left_node);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(l) = taffy.layout(node) else {
            return;
        };
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        let Ok(children) = taffy.children(node) else {
            return;
        };
        if let Some(&left_child) = children.first() {
            self.left
                .apply_layout(taffy, left_child, Point::new(self.rect.x0, self.rect.y0));
        }
        if let Some(&right_child) = children.get(1) {
            self.right
                .apply_layout(taffy, right_child, Point::new(self.rect.x0, self.rect.y0));
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
        self.left
            .draw(scene, transform, text_renderer, theme, presence);
        self.right
            .draw(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let Ok(children) = ctx.taffy.children(ctx.node_id) else {
            return;
        };
        if let Some(&left_child) = children.first() {
            self.left
                .handle_event(event, &mut ctx.reborrow(ctx.path, left_child));
        }
        if *ctx.handled {
            return;
        }
        if let Some(&right_child) = children.get(1) {
            self.right
                .handle_event(event, &mut ctx.reborrow(ctx.path, right_child));
        }
    }

    fn name(&self) -> &'static str {
        "SplitRow"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}
