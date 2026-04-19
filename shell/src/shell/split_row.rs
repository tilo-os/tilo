//! Shell-level horizontal split layout.
//!
//! `quick_ui::Row` is opinionated toward wrapping, gap-spaced,
//! `AlignItems::Center` layouts suited to toolbars; the shell-root needs a
//! plain two-child row where the left child keeps its intrinsic width and
//! the right child stretches to fill the remaining horizontal and vertical
//! space. This widget provides exactly that — a bare taffy flex row with
//! `flex_grow: 1` on the right child and `stretch` cross-alignment so
//! children fill the available height.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, FlexDirection, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use quick_ui::presence::PresenceCtx;
use quick_ui::text_render::TextRenderer;
use quick_ui::theme::Theme;
use quick_ui::widget::Widget;

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
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        self.left.sync(state, path);
        self.right.sync(state, path);
    }

    fn update(&mut self, state: &mut AutoCommit, path: &ObjId) -> bool {
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

        // The right child should take all remaining horizontal space while
        // stretching vertically. Override whatever its `build_layout`
        // returned so a Container-with-percent(1)-height still gets its
        // flex-grow set to 1 — without this, taffy would size the right
        // child by its intrinsic content width.
        let mut right_style = taffy.style(right_node).unwrap().clone();
        right_style.flex_grow = 1.0;
        right_style.size = Size {
            width: Dimension::auto(),
            height: Dimension::percent(1.0),
        };
        taffy.set_style(right_node, right_style).unwrap();

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
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
        let children = taffy.children(node).unwrap();
        self.left.apply_layout(
            taffy,
            children[0],
            Point::new(self.rect.x0, self.rect.y0),
        );
        self.right.apply_layout(
            taffy,
            children[1],
            Point::new(self.rect.x0, self.rect.y0),
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.left.draw(scene, transform, text_renderer, theme, presence);
        self.right.draw(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        let children = taffy.children(node_id).unwrap();
        self.left.handle_event(
            event,
            scale_factor,
            modifiers,
            state,
            path,
            handled,
            focus_target,
            children[0],
            taffy,
        );
        if *handled {
            return;
        }
        self.right.handle_event(
            event,
            scale_factor,
            modifiers,
            state,
            path,
            handled,
            focus_target,
            children[1],
            taffy,
        );
    }

    fn name(&self) -> &str {
        "SplitRow"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}
