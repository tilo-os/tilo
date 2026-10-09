//! Headless click-driving harness for Tube.
//!
//! Runs the real five-pass frame (sync → `build_layout` → `compute_layout` →
//! `apply_layout`) against the app, then lets a test resolve a widget's laid-out
//! [`Rect`] by `test_id` (or tree path) and inject a real winit click / key at
//! those coordinates through `Widget::handle_event`. No GPU, no event loop —
//! this exercises the exact production interaction path (`ActionBus` drain +
//! `apply`), which is what makes the assertions meaningful.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]
#![allow(
    dead_code,
    reason = "this module is compiled into every integration-test binary and each one uses a \
              different subset of the harness"
)]

use std::collections::HashMap;
use ui::framework::HostDelivery as _;

use taffy::prelude::*;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::dpi::PhysicalPosition;
use winit::event::{
    DeviceId, ElementState, Modifiers, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent,
};

pub struct Harness<W: Widget> {
    pub app: W,
    pub state: ui::State,
    theme: Theme,
    text: TextRenderer,
    taffy: TaffyTree<()>,
    node_map: HashMap<NodeId, *mut dyn Widget>,
    child: NodeId,
    root: NodeId,
    viewport: (f32, f32),
    modifiers: Modifiers,
    /// The `focus_target` a widget requested during the last dispatch — exactly
    /// what `ui::App` reads to move keyboard focus. Lets a test prove a click
    /// targeted a specific input for focus, on the real event path.
    last_focus: Option<NodeId>,
}

impl<W: Widget> Harness<W> {
    /// Build a harness at the given logical viewport and run the first frame.
    pub fn new(app: W, viewport: (f32, f32)) -> Self {
        Self::with_theme(app, viewport, Theme::dark())
    }

    pub fn with_text_scale(app: W, viewport: (f32, f32), text_scale: f32) -> Self {
        Self::with_theme(app, viewport, Theme::dark().with_text_scale(text_scale))
    }

    /// Build a harness with the exact theme and text scale a real offscreen
    /// shot would use. Keeping the renderer's scale in lockstep with the theme
    /// is essential for rect assertions at 200% text.
    pub fn with_theme(app: W, viewport: (f32, f32), theme: Theme) -> Self {
        let mut text = TextRenderer::new();
        text.set_text_scale(theme.text_scale);
        let mut h = Harness {
            app,
            state: ui::State::new(),
            theme,
            text,
            taffy: TaffyTree::new(),
            node_map: HashMap::new(),
            child: NodeId::new(0),
            root: NodeId::new(0),
            viewport,
            modifiers: Modifiers::default(),
            last_focus: None,
        };
        h.frame();
        h
    }

    /// The renderer scale the frame measured and will paint with.
    pub fn text_scale(&self) -> f32 {
        self.text.text_scale()
    }

    /// Measure `s` with the frame's own renderer, exactly as a `Text` child
    /// would, so a test can compare a laid-out rect against the metric that
    /// produced it.
    pub fn measure_text(&mut self, s: &str, size: f32, max_w: Option<f32>) -> (f32, f32) {
        self.text.measure(s, size, max_w)
    }

    /// Every laid-out rect under the widget with `id`, depth-first, paired with
    /// its depth below that widget (the widget itself is depth 0). Lets a test
    /// prove composition geometry — "each text line ends before the next
    /// starts" — on the real measured tree rather than on a helper's estimate.
    pub fn subtree_rects(&self, id: &str) -> Vec<(usize, Option<String>, Rect)> {
        fn find<'a>(w: &'a dyn Widget, id: &str) -> Option<&'a dyn Widget> {
            if w.test_id() == Some(id) {
                return Some(w);
            }
            for c in w.children() {
                if let Some(f) = find(c, id) {
                    return Some(f);
                }
            }
            None
        }
        fn walk(w: &dyn Widget, depth: usize, out: &mut Vec<(usize, Option<String>, Rect)>) {
            out.push((depth, w.test_id().map(str::to_owned), w.get_rect()));
            for c in w.children() {
                walk(c, depth.saturating_add(1), out);
            }
        }
        let mut out = Vec::new();
        if let Some(w) = find(&self.app, id) {
            walk(w, 0, &mut out);
        }
        out
    }

    /// Run one full layout frame (sync + build + compute + apply). Called after
    /// every event that dirties the structure so rects reflect the new tree.
    #[allow(
        clippy::unwrap_used,
        reason = "test-harness plumbing (this module compiles only into the test \
              binary): a broken layout / missing test_id is a real test \
              failure and must fail loudly here, not be swallowed by a \
              production-safety Result the harness has no caller to check"
    )]
    pub fn frame(&mut self) {
        self.taffy = TaffyTree::new();
        self.node_map.clear();
        self.app.sync(&self.state.doc, &self.state.root);
        let child = self
            .app
            .build_layout(&mut self.taffy, &self.theme, &mut self.node_map);
        let (w, h) = self.viewport;
        let root = self
            .taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(w),
                        height: Dimension::length(h),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();
        {
            let measure_map = self.node_map.clone();
            let tr = &mut self.text;
            self.taffy
                .compute_layout_with_measure(
                    root,
                    taffy::Size {
                        width: AvailableSpace::Definite(w),
                        height: AvailableSpace::Definite(h),
                    },
                    |known, available, node_id, _, _| {
                        if let Some(&ptr) = measure_map.get(&node_id) {
                            unsafe { (*ptr).measure(known, available, tr) }
                        } else {
                            taffy::Size::ZERO
                        }
                    },
                )
                .unwrap();
        }
        self.app.apply_layout(&self.taffy, child, Point::ORIGIN);
        self.child = child;
        self.root = root;
    }

    /// Depth-first search for the first widget whose `test_id` equals `id`,
    /// returning its laid-out rect.
    pub fn rect_by_id(&self, id: &str) -> Option<Rect> {
        fn walk(w: &dyn Widget, id: &str) -> Option<Rect> {
            if w.test_id() == Some(id) {
                return Some(w.get_rect());
            }
            for c in w.children() {
                if let Some(r) = walk(c, id) {
                    return Some(r);
                }
            }
            None
        }
        walk(&self.app, id)
    }

    /// True once a widget with `id` exists in the current tree.
    pub fn has_id(&self, id: &str) -> bool {
        self.rect_by_id(id).is_some()
    }

    fn dispatch(&mut self, event: &WindowEvent) {
        let focus = ui::testing::Dispatch::new()
            .modifiers(self.modifiers)
            .doc(&mut self.state.doc)
            .path(&self.state.root)
            .layout(&self.taffy, self.child)
            .send(&mut self.app, event)
            .focus_target;
        if focus.is_some() {
            self.last_focus = focus;
        }
        self.dispatch_control_invocations();
    }

    /// Mirror the live host's action-bound control door after each input
    /// event. Stopping after the leaf emits an invocation makes a working
    /// effect control look inert in this integration harness.
    fn dispatch_control_invocations(&mut self) {
        while let Some(intent) = ui::widget::controls::take_control_invocation(&mut self.app) {
            let resources = ui::resources::resources(&self.app, &self.state.doc);
            let spec = ui::tilo_actions::door::resolve(&resources, &intent.address)
                .expect("action-bound test control must resolve in the current resource tree");
            assert!(
                matches!(
                    spec.contract.confirmation,
                    ui::tilo_actions::ConfirmationPolicy::None
                ),
                "the harness must not manufacture confirmation"
            );
            let encoded =
                ui::tilo_actions::door::encode_json(&resources, &intent.address, &intent.arguments)
                    .expect("bound control arguments must satisfy their contract");
            self.app
                .start_encoded(
                    spec.kind,
                    &encoded.type_id,
                    &encoded.payload,
                    &mut self.state.doc,
                    &self.state.root,
                )
                .expect("bound resource operation must not fail")
                .expect("the app must accept its bound resource operation");
        }
    }

    /// The taffy node of the first widget with `test_id == id` (via `node_map`).
    fn node_by_id(&self, id: &str) -> Option<NodeId> {
        for (node, ptr) in &self.node_map {
            if unsafe { (**ptr).test_id() } == Some(id) {
                return Some(*node);
            }
        }
        None
    }

    /// Click the centre of the widget addressed by `id` (move → press →
    /// release) **without** a relayout afterwards, and report whether the click
    /// asked `ui::App` to move keyboard focus to that same widget. This is the
    /// real hit-test + event-routing path: a `true` means a real click at the
    /// real coordinates focuses the real input.
    #[allow(
        clippy::panic,
        reason = "test-harness plumbing (this module compiles only into the test \
              binary): a broken layout / missing test_id is a real test \
              failure and must fail loudly here, not be swallowed by a \
              production-safety Result the harness has no caller to check"
    )]
    pub fn click_focuses(&mut self, id: &str) -> bool {
        let r = self
            .rect_by_id(id)
            .unwrap_or_else(|| panic!("no widget with test_id {id:?} in the laid-out tree"));
        let target = self.node_by_id(id);
        let p = r.center();
        let pos = PhysicalPosition::new(p.x, p.y);
        let dev = DeviceId::dummy();
        self.last_focus = None;
        self.dispatch(&WindowEvent::CursorMoved {
            device_id: dev,
            position: pos,
        });
        self.dispatch(&WindowEvent::MouseInput {
            device_id: dev,
            state: ElementState::Pressed,
            button: MouseButton::Left,
        });
        self.dispatch(&WindowEvent::MouseInput {
            device_id: dev,
            state: ElementState::Released,
            button: MouseButton::Left,
        });
        target.is_some() && self.last_focus == target
    }

    /// Scroll with a wheel notch over the point `at` (a negative `lines` scrolls
    /// content up, revealing what's below), then relayout — the real wheel path
    /// through `handle_event`. Moving the cursor first mirrors a real gesture.
    pub fn wheel_at(&mut self, at: Point, lines: f32) {
        let dev = DeviceId::dummy();
        self.dispatch(&WindowEvent::CursorMoved {
            device_id: dev,
            position: PhysicalPosition::new(at.x, at.y),
        });
        self.dispatch(&WindowEvent::MouseWheel {
            device_id: dev,
            delta: MouseScrollDelta::LineDelta(0.0, lines),
            phase: TouchPhase::Moved,
        });
        self.frame();
    }

    /// Inject a full left click (move → press → release) at a point, then relayout.
    pub fn click_at(&mut self, p: Point) {
        let pos = PhysicalPosition::new(p.x, p.y);
        let dev = DeviceId::dummy();
        self.dispatch(&WindowEvent::CursorMoved {
            device_id: dev,
            position: pos,
        });
        self.dispatch(&WindowEvent::MouseInput {
            device_id: dev,
            state: ElementState::Pressed,
            button: MouseButton::Left,
        });
        self.dispatch(&WindowEvent::MouseInput {
            device_id: dev,
            state: ElementState::Released,
            button: MouseButton::Left,
        });
        self.frame();
    }

    /// Click the centre of the widget addressed by `id`. Panics if not found —
    /// a missing id is a real test failure (the affordance isn't in the tree).
    #[allow(
        clippy::panic,
        reason = "test-harness plumbing (this module compiles only into the test \
              binary): a broken layout / missing test_id is a real test \
              failure and must fail loudly here, not be swallowed by a \
              production-safety Result the harness has no caller to check"
    )]
    pub fn click_id(&mut self, id: &str) {
        let r = self
            .rect_by_id(id)
            .unwrap_or_else(|| panic!("no widget with test_id {id:?} in the laid-out tree"));
        self.click_at(r.center());
    }

    /// Laid-out rects of the **direct children** of the widget with `test_id ==
    /// id` — e.g. the card rects inside the grid, for a "no right gutter" check.
    pub fn child_rects(&self, id: &str) -> Vec<Rect> {
        fn find<'a>(w: &'a dyn Widget, id: &str) -> Option<&'a dyn Widget> {
            if w.test_id() == Some(id) {
                return Some(w);
            }
            for c in w.children() {
                if let Some(f) = find(c, id) {
                    return Some(f);
                }
            }
            None
        }
        find(&self.app, id)
            .map(|w| w.children().iter().map(|c| c.get_rect()).collect())
            .unwrap_or_default()
    }

    /// Draw the current tree (with viewport culling, exactly as the live app
    /// composites it) and return the number of IMAGE draw commands emitted —
    /// proof that real decoded photos (thumbnails / avatars) reached the scene
    /// rather than the gradient/initials fallback.
    pub fn image_draw_count(&mut self) -> usize {
        use vello_encoding::DrawTag;
        // Sync the *built* tree first: lazy image widgets resolve their decoded
        // image in `sync`, and `Harness::new`/`frame` sync *before* build (when
        // the tree doesn't exist yet). In the live app the tree persists so a
        // later `sync` folds the image in; mirror that here by syncing the tree
        // that `frame` just built before we draw it.
        self.app.sync(&self.state.doc, &self.state.root);
        let (w, h) = self.viewport;
        let mut scene = Scene::new();
        let presence = PresenceCtx::default();
        let permissions = PermissionContext::permissive();
        let vp = presence.push_viewport(Rect::new(0.0, 0.0, f64::from(w), f64::from(h)));
        self.app.draw_with_presence(
            &mut scene,
            Affine::IDENTITY,
            &mut self.text,
            &self.theme,
            &presence,
            &permissions,
        );
        presence.pop_viewport(vp);
        self.app.mark_drawn();
        scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::IMAGE)
            .count()
    }

    /// Whether any widget in the laid-out tree still reports it needs a layout
    /// re-measure. After a fully laid-out + drawn frame this must be `false`: a
    /// widget that reports "dirty" forever (e.g. an avatar keyed off a draw-time
    /// cache while culled off-screen) forces the enclosing split to rebuild a
    /// cosmic-text `FontSystem` + re-solve the whole content pane every scroll
    /// frame — the scroll-jank regression this guards.
    pub fn layout_needs_measure(&self) -> bool {
        Widget::layout_needs_measure(&self.app)
    }
}

// Keyboard injection note: winit 0.30's `KeyEvent` has a `pub(crate)`
// `platform_specific` field, so a `KeyEvent` cannot be constructed outside the
// crate — headless tests can't synthesise key presses through `handle_event`.
// Every *pointer* interaction (tab clicks, card activation, the compact search
// button) goes through the real `handle_event` + computed-rect path here —
// which is the load-bearing interaction to prove.
