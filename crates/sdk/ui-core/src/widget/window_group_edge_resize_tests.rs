//! Resizing a window by its edge: the width follows the drag live, is
//! committed once, and an interruption puts it back.

use super::tests::{g_normal_width, make_group};
use super::*;
use crate::testing::Dispatch;
use winit::event::MouseButton;

struct EdgeResizeJourney {
    row: crate::widget::GroupRow,
    tree: TaffyTree<()>,
    node: NodeId,
    pointer: Point,
}

impl EdgeResizeJourney {
    fn new() -> Self {
        use taffy::prelude::TaffyMaxContent;

        let mut row = crate::widget::GroupRow::new(vec![make_group()]);
        let mut tree = TaffyTree::new();
        let node = row.build_layout(
            &mut tree,
            &Theme::dark().with_reduce_motion(true),
            &mut Default::default(),
        );
        let root = tree.parent_node(
            Style {
                size: Size {
                    width: Dimension::length(1600.0),
                    height: Dimension::length(900.0),
                },
                ..Default::default()
            },
            &[node],
        );
        tree.compute_layout(root, Size::MAX_CONTENT).unwrap();
        row.apply_layout(&tree, node, Point::ZERO);
        let rect = row.groups[0].get_rect();
        assert!(rect.height() > 0.0);
        Self {
            row,
            tree,
            node,
            pointer: Point::new(rect.x1 - 3.0, rect.center().y),
        }
    }

    fn send(&mut self, event: &WindowEvent) -> bool {
        Dispatch::new()
            .layout(&self.tree, self.node)
            .send(&mut self.row, event)
            .handled
    }

    fn move_pointer(&mut self, dx: f64) {
        self.send(&WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(self.pointer.x + dx, self.pointer.y),
        });
    }

    fn mouse(&mut self, state: ElementState) {
        self.send(&WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state,
            button: MouseButton::Left,
        });
    }

    fn begin(&mut self) {
        self.move_pointer(0.0);
        self.mouse(ElementState::Pressed);
        assert!(self.row.wants_pointer_capture());
        self.move_pointer(100.0);
        assert_ne!(
            self.row.groups[0].layout_width_px(),
            self.row.groups[0].resolved_width_px(),
            "the drag sets the width live"
        );
        assert!(self.row.groups[0].take_pending_width_change().is_none());
    }

    fn assert_cancelled_and_restart(&mut self) {
        assert!(
            !self.row.wants_pointer_capture(),
            "interruption must release capture"
        );
        assert_eq!(self.row.groups[0].width(), WindowWidth::Normal);
        assert_eq!(
            self.row.groups[0].layout_width_px(),
            self.row.groups[0].resolved_width_px(),
            "and the window is back at its width"
        );
        self.move_pointer(250.0);
        self.mouse(ElementState::Released);
        assert_eq!(self.row.groups[0].width(), WindowWidth::Normal);
        assert!(self.row.groups[0].take_pending_width_change().is_none());
        self.begin();
        self.mouse(ElementState::Released);
        assert!(!self.row.wants_pointer_capture());
        assert!(self.row.groups[0].take_pending_width_change().is_some());
        assert!(self.row.groups[0].take_pending_width_change().is_none());
    }
}

#[test]
fn window_edge_resize_focus_loss_cancels_preview_and_allows_a_fresh_drag() {
    let mut journey = EdgeResizeJourney::new();
    journey.begin();
    journey.send(&WindowEvent::Focused(false));
    journey.assert_cancelled_and_restart();
}

#[test]
fn window_edge_resize_escape_cancels_preview_before_a_late_release() {
    let mut journey = EdgeResizeJourney::new();
    journey.begin();
    let escape = WindowEvent::KeyboardInput {
        device_id: winit::event::DeviceId::dummy(),
        event: crate::synthetic_input::key_event(
            Key::Named(winit::keyboard::NamedKey::Escape),
            None,
            ElementState::Pressed,
        ),
        is_synthetic: true,
    };
    assert!(journey.send(&escape));
    journey.send(&escape);
    journey.assert_cancelled_and_restart();
}

#[test]
fn an_edge_drag_sets_the_width_live_and_commits_it_once() {
    use std::collections::HashMap;
    use taffy::Style;
    use taffy::prelude::TaffyMaxContent;

    // (Re)build the taffy tree from the group's current state and run a
    // full layout pass, returning the group's laid-out width.
    fn layout_width(g: &mut WindowGroup, theme: &Theme) -> f64 {
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = g.build_layout(&mut taffy, theme, &mut node_map);
        let root = taffy.parent_node(Style::default(), &[node]);
        taffy
            .compute_layout(root, taffy::Size::MAX_CONTENT)
            .unwrap();
        g.apply_layout(&taffy, node, Point::ZERO);
        g.get_rect().width()
    }

    {
        let theme = Theme::dark();
        let mut g = make_group();
        // 1800 px strip: one per-mille is 1.8 px, so a share alone could
        // not put the edge where the pointer is.
        g.set_strip_usable_px(1800.0);
        let _ = g.take_structure_dirty();
        assert_eq!(layout_width(&mut g, &theme), 1201.0);
        g.begin_edge_drag(WindowEdge::Trailing);
        assert!(g.edge_resize_in_flight());
        assert_eq!(g.edge_drag_origin_px(), Some(1201.0));
        assert_eq!(g.edge_highlight(), Some(WindowEdge::Trailing));
        g.set_live_width(1700.4);
        assert!(
            g.take_structure_dirty(),
            "a live width relays the window out"
        );
        assert_eq!(layout_width(&mut g, &theme), 1700.0, "whole pixels");
        assert_eq!(
            g.take_pending_width_change(),
            None,
            "mid-drag nothing is handed to the shell to persist",
        );
        let settle = g.end_edge_drag(true);
        assert_eq!(
            g.width(),
            WindowWidth::Fraction(944),
            "the drag is stored as the share it landed on (1700/1800)",
        );
        assert_eq!(
            g.take_pending_width_change(),
            Some(WindowWidth::Fraction(944))
        );
        assert_eq!(g.take_pending_width_change(), None, "drained once");
        assert_eq!(
            settle,
            g.resolved_width_px() - 1700.0,
            "the commit reports what the share's own grid moves the width by",
        );
        assert!(!g.edge_resize_in_flight());
        assert_eq!(g.edge_highlight(), None);
    }

    {
        let mut g = make_group();
        g.set_strip_usable_px(1200.0);
        g.begin_edge_drag(WindowEdge::Leading);
        g.set_live_width(300.0);
        assert_eq!(g.layout_width_px(), 300.0);
        let _ = g.end_edge_drag(false);
        assert_eq!(g.width(), WindowWidth::Normal);
        assert_eq!(g.layout_width_px(), 800.0);
        assert_eq!(g.take_pending_width_change(), None);
    }

    {
        let mut g = make_group();
        g.set_strip_usable_px(1200.0);
        let limits = g.width_limits();
        assert_eq!(limits.min, min_group_px(1200.0));
        assert_eq!(limits.max, max_group_px(1200.0));
    }

    {
        let theme = Theme::dark();
        let mut g = make_group();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = g.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy.parent_node(
            Style {
                size: Size {
                    width: Dimension::length(1600.0),
                    height: Dimension::length(900.0),
                },
                ..Default::default()
            },
            &[node],
        );
        taffy
            .compute_layout(root, taffy::Size::MAX_CONTENT)
            .unwrap();
        g.apply_layout(&taffy, node, Point::ZERO);
        let rect = g.get_rect();
        let cy = rect.center().y;
        for x in [rect.x1 - 3.0, rect.x0 + 3.0] {
            assert_eq!(
                Widget::cursor_shape(&g, Point::new(x, cy)),
                Some(winit::window::CursorIcon::EwResize),
                "an edge band requests an east-west resize cursor (x = {x})",
            );
        }
        assert_eq!(
            Widget::cursor_shape(&g, Point::new(rect.center().x, cy)),
            None,
            "pointer over the group body (not the edge band) defers",
        );
        // An in-flight drag holds the cursor even off the band.
        g.begin_edge_drag(WindowEdge::Trailing);
        assert_eq!(
            Widget::cursor_shape(&g, Point::new(rect.center().x, cy)),
            Some(winit::window::CursorIcon::EwResize),
        );
    }

    {
        // A width a person set is a share, kept on every display (the
        // default width stops growing past what reads well instead).
        let mut wide = make_group();
        wide.set_width(WindowWidth::Fraction(667));
        wide.set_strip_usable_px(3840.0);
        assert_eq!(
            wide.resolved_width_px(),
            2561.0,
            "667/1000 of a 3840 px strip — the same share a 1200 px strip \
         gives at 800 px, to the 1 px that per-mille quantisation costs \
         at this size",
        );
        let mut default = make_group();
        default.set_strip_usable_px(3840.0);
        assert_eq!(default.width(), g_normal_width());
        assert_eq!(default.resolved_width_px(), 1280.0);
    }
}
