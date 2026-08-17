//! Headless click-driving harness for Tube.
//!
//! Runs the real five-pass frame (sync → build_layout → compute_layout →
//! apply_layout) against the app, then lets a test resolve a widget's laid-out
//! [`Rect`] by `test_id` (or tree path) and inject a real winit click / key at
//! those coordinates through `Widget::handle_event`. No GPU, no event loop —
//! this exercises the exact production interaction path (ActionBus drain +
//! `apply`), which is what makes the assertions meaningful.

use std::collections::HashMap;

use taffy::prelude::*;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::{Affine, Point, Rect};
use vello::Scene;
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
        let mut h = Harness {
            app,
            state: ui::State::new(),
            theme: Theme::dark(),
            text: TextRenderer::new(),
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

    /// Run one full layout frame (sync + build + compute + apply). Called after
    /// every event that dirties the structure so rects reflect the new tree.
    pub fn frame(&mut self) {
        self.taffy = TaffyTree::new();
        self.node_map.clear();
        self.app.sync(&mut self.state.doc, &self.state.root);
        let child = self.app.build_layout(&mut self.taffy, &self.theme, &mut self.node_map);
        let (w, h) = self.viewport;
        let root = self
            .taffy
            .new_with_children(
                Style {
                    size: Size { width: Dimension::length(w), height: Dimension::length(h) },
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

    fn dispatch(&mut self, event: WindowEvent) {
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        self.app.handle_event(
            &event,
            1.0,
            &self.modifiers,
            &mut self.state.doc,
            &self.state.root,
            &mut handled,
            &mut focus,
            self.child,
            &self.taffy,
        );
        if focus.is_some() {
            self.last_focus = focus;
        }
    }

    /// The taffy node of the first widget with `test_id == id` (via `node_map`).
    fn node_by_id(&self, id: &str) -> Option<NodeId> {
        for (node, ptr) in self.node_map.iter() {
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
    pub fn click_focuses(&mut self, id: &str) -> bool {
        let r = self
            .rect_by_id(id)
            .unwrap_or_else(|| panic!("no widget with test_id {id:?} in the laid-out tree"));
        let target = self.node_by_id(id);
        let p = r.center();
        let pos = PhysicalPosition::new(p.x, p.y);
        let dev = DeviceId::dummy();
        self.last_focus = None;
        self.dispatch(WindowEvent::CursorMoved { device_id: dev, position: pos });
        self.dispatch(WindowEvent::MouseInput {
            device_id: dev,
            state: ElementState::Pressed,
            button: MouseButton::Left,
        });
        self.dispatch(WindowEvent::MouseInput {
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
        self.dispatch(WindowEvent::CursorMoved {
            device_id: dev,
            position: PhysicalPosition::new(at.x, at.y),
        });
        self.dispatch(WindowEvent::MouseWheel {
            device_id: dev,
            delta: MouseScrollDelta::LineDelta(0.0, lines),
            phase: TouchPhase::Moved,
        });
        // Smooth scrolling eases the rendered offset toward its target over
        // several frames (via `sync`); pump frames until the laid-out tree
        // stops moving so callers observe the settled scroll position rather
        // than a mid-glide frame. The overlay fade doesn't move rects, so this
        // returns as soon as the offset has converged.
        self.settle_scroll();
    }

    /// Pump layout frames until the smooth-scroll ease settles (the tree's
    /// laid-out rects stop moving). Bounded; runs at least one frame.
    fn settle_scroll(&mut self) {
        let mut prev = 0u64;
        for i in 0..240 {
            self.frame();
            let fp = self.layout_fingerprint();
            if i > 0 && fp == prev {
                break;
            }
            prev = fp;
        }
    }

    /// A cheap fingerprint of every widget's laid-out rect (pixel-rounded), so
    /// `settle_scroll` can tell when the scroll animation has come to rest.
    fn layout_fingerprint(&self) -> u64 {
        fn walk(w: &dyn Widget, acc: &mut u64) {
            let r = w.get_rect();
            for v in [r.x0, r.y0, r.x1, r.y1] {
                *acc = acc
                    .wrapping_mul(1099511628211)
                    .wrapping_add((v.round() as i64) as u64);
            }
            for c in w.children() {
                walk(c, acc);
            }
        }
        let mut acc = 1469598103934665603u64;
        walk(&self.app, &mut acc);
        acc
    }

    /// Inject a full left click (move → press → release) at a point, then relayout.
    pub fn click_at(&mut self, p: Point) {
        let pos = PhysicalPosition::new(p.x, p.y);
        let dev = DeviceId::dummy();
        self.dispatch(WindowEvent::CursorMoved { device_id: dev, position: pos });
        self.dispatch(WindowEvent::MouseInput {
            device_id: dev,
            state: ElementState::Pressed,
            button: MouseButton::Left,
        });
        self.dispatch(WindowEvent::MouseInput {
            device_id: dev,
            state: ElementState::Released,
            button: MouseButton::Left,
        });
        self.frame();
    }

    /// Click the centre of the widget addressed by `id`. Panics if not found —
    /// a missing id is a real test failure (the affordance isn't in the tree).
    pub fn click_id(&mut self, id: &str) {
        let r = self
            .rect_by_id(id)
            .unwrap_or_else(|| panic!("no widget with test_id {id:?} in the laid-out tree"));
        self.click_at(r.center());
    }

    /// Inject a press → drag → release (move to `from`, press, move to `to`,
    /// release), then relayout. Drives real pointer-capture drag handling
    /// through `handle_event` — used to exercise the scrubber's drag-to-seek.
    pub fn drag(&mut self, from: Point, to: Point) {
        let dev = DeviceId::dummy();
        self.dispatch(WindowEvent::CursorMoved {
            device_id: dev,
            position: PhysicalPosition::new(from.x, from.y),
        });
        self.dispatch(WindowEvent::MouseInput {
            device_id: dev,
            state: ElementState::Pressed,
            button: MouseButton::Left,
        });
        self.dispatch(WindowEvent::CursorMoved {
            device_id: dev,
            position: PhysicalPosition::new(to.x, to.y),
        });
        self.dispatch(WindowEvent::MouseInput {
            device_id: dev,
            state: ElementState::Released,
            button: MouseButton::Left,
        });
        self.frame();
    }

    /// Drag the horizontal scrubber addressed by `id` to `frac` of its width.
    /// Returns the target x used, for callers that want to reason about it.
    pub fn drag_scrubber_to(&mut self, id: &str, frac: f64) {
        let r = self
            .rect_by_id(id)
            .unwrap_or_else(|| panic!("no widget with test_id {id:?} in the laid-out tree"));
        let from = r.center();
        let to = Point::new(r.x0 + r.width() * frac.clamp(0.0, 1.0), r.center().y);
        self.drag(from, to);
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
        self.app.sync(&mut self.state.doc, &self.state.root);
        let (w, h) = self.viewport;
        let mut scene = Scene::new();
        let presence = PresenceCtx::default();
        let permissions = PermissionContext::permissive();
        let vp = presence.push_viewport(Rect::new(0.0, 0.0, w as f64, h as f64));
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
// The keyboard-triggered *toggle* (Ctrl+K / Esc) is therefore exercised via
// `TubeApp`'s test hook, while every *pointer* interaction (tab clicks, palette
// result activation, scrim dismissal) goes through the real `handle_event` +
// computed-rect path here — which is the load-bearing interaction to prove.
