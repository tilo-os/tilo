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
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::{Point, Rect};
use winit::dpi::PhysicalPosition;
use winit::event::{DeviceId, ElementState, Modifiers, MouseButton, WindowEvent};

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
}

// Keyboard injection note: winit 0.30's `KeyEvent` has a `pub(crate)`
// `platform_specific` field, so a `KeyEvent` cannot be constructed outside the
// crate — headless tests can't synthesise key presses through `handle_event`.
// The keyboard-triggered *toggle* (Ctrl+K / Esc) is therefore exercised via
// `TubeApp`'s test hook, while every *pointer* interaction (tab clicks, palette
// result activation, scrim dismissal) goes through the real `handle_event` +
// computed-rect path here — which is the load-bearing interaction to prove.
