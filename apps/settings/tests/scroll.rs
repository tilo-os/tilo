//! Driven scrolling test for the settings content pane.
//!
//! A category whose section cards overflow a short window must scroll. This
//! builds the *real* composed `SettingsApp` tree, lays it out at a short
//! viewport, then drives the genuine winit wheel + keyboard path and asserts
//! the content pane's `Scrollable` actually moved its child. Geometry is read
//! only through the public `Widget` trait (`name`, `get_rect`, `children`) so
//! the assertions ride the production layout.

use std::collections::HashMap;

use settings::categories::Category;
use settings::settings_app::SettingsApp;
use taffy::prelude::*;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::Point;
use winit::event::{Modifiers, MouseScrollDelta, TouchPhase, WindowEvent};

struct Harness {
    app: SettingsApp,
    taffy: TaffyTree<()>,
    self_node: NodeId,
    state: ui::State,
}

impl Harness {
    fn new(category: Category, w: f32, h: f32) -> Self {
        let mut app = SettingsApp::with_selected(category).with_width_hint(w);
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let mut tr = TextRenderer::new();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);

        let self_node = app.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(w),
                        height: Dimension::length(h),
                    },
                    ..Default::default()
                },
                &[self_node],
            )
            .unwrap();
        let measure_map = node_map.clone();
        let trm = &mut tr;
        taffy
            .compute_layout_with_measure(
                root,
                Size {
                    width: AvailableSpace::Definite(w),
                    height: AvailableSpace::Definite(h),
                },
                |known, available, node_id, _, _| {
                    if let Some(&ptr) = measure_map.get(&node_id) {
                        unsafe { (*ptr).measure(known, available, trm) }
                    } else {
                        Size::ZERO
                    }
                },
            )
            .unwrap();
        app.apply_layout(&taffy, self_node, Point::ORIGIN);
        Self {
            app,
            taffy,
            self_node,
            state,
        }
    }

    fn relayout(&mut self) {
        self.app
            .apply_layout(&self.taffy, self.self_node, Point::ORIGIN);
    }

    fn dispatch(&mut self, event: &WindowEvent) -> bool {
        let mut handled = false;
        let mut focus_target = None;
        self.app.handle_event(
            event,
            1.0,
            &Modifiers::default(),
            &mut self.state.doc,
            &self.state.root,
            &mut handled,
            &mut focus_target,
            self.self_node,
            &self.taffy,
        );
        handled
    }

    fn wheel(&mut self, notches_down: f32) {
        let ev = WindowEvent::MouseWheel {
            device_id: unsafe { std::mem::zeroed() },
            delta: MouseScrollDelta::LineDelta(0.0, -notches_down),
            phase: TouchPhase::Moved,
        };
        self.dispatch(&ev);
    }
}

/// The content pane's `Scrollable` is the one furthest to the right (the
/// sidebar's own `Scrollable` hugs the left edge). Returns `(offset, viewport_h,
/// content_h)` where `offset` is how far it has shifted its child up.
fn content_scroll(app: &SettingsApp) -> (f64, f64, f64) {
    fn collect<'a>(w: &'a dyn Widget, out: &mut Vec<&'a dyn Widget>) {
        if w.name() == "Scrollable" {
            out.push(w);
        }
        for c in w.children() {
            collect(c, out);
        }
    }
    let mut scrolls = Vec::new();
    collect(app, &mut scrolls);
    let content = scrolls
        .into_iter()
        .max_by(|a, b| a.get_rect().x0.partial_cmp(&b.get_rect().x0).unwrap())
        .expect("a content Scrollable exists in the settings tree");
    let child = *content
        .children()
        .first()
        .expect("the Scrollable wraps its content column");
    let vp = content.get_rect();
    (
        vp.y0 - child.get_rect().y0,
        vp.height(),
        child.get_rect().height(),
    )
}

#[test]
fn content_pane_scrolls_with_the_mouse_wheel() {
    // A tall category on a short, wide (docked-sidebar) window. Power has a
    // fixed four-card structure (battery / profile / idle / behaviour) that
    // reliably overflows a short viewport by far more than a single wheel
    // step, so the staged wheel→fling→clamp assertions below don't ride on
    // one pane's exact pixel height (or on how many accounts a given box has).
    let mut h = Harness::new(Category::Power, 1100.0, 340.0);

    let (offset0, viewport_h, content_h) = content_scroll(&h.app);
    assert!(
        content_h > viewport_h + 40.0,
        "precondition: content ({content_h}) must overflow the viewport ({viewport_h})",
    );
    assert!(offset0.abs() < 0.5, "starts unscrolled, got {offset0}");

    // Wheel down: the content pane's Scrollable shifts its child up.
    h.wheel(3.0);
    h.relayout();
    let (one, _, _) = content_scroll(&h.app);
    assert!(
        one > 0.5,
        "a wheel-down scrolls the settings content; offset stayed {one}"
    );

    // Fling to the bottom: scrolls further, then clamps — a second identical
    // fling must NOT move it any further (it's parked at the content edge).
    h.wheel(1000.0);
    h.relayout();
    let (bottom, _, _) = content_scroll(&h.app);
    assert!(bottom > one, "flinging scrolls further ({bottom} vs {one})");

    h.wheel(1000.0);
    h.relayout();
    let (still, _, _) = content_scroll(&h.app);
    assert!(
        (still - bottom).abs() < 0.5,
        "scroll clamps at the content edge — a second fling doesn't move it ({still} vs {bottom})",
    );
}
