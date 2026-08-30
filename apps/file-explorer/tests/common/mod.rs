//! Shared harness for the file-explorer integration tests: it drives the real
//! `FileExplorerWidget` through the host's `sync → build_layout → compute →
//! apply_layout` cycle and dispatches real winit events, so tests exercise the
//! production input + layout paths rather than a mock.

#![allow(dead_code)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::HashMap;
use std::path::Path;

use file_explorer::FileExplorerWidget;
use taffy::prelude::*;
use ui::text_render::TextRenderer;
use ui::widget::Widget;
use vello::kurbo::Point;
use winit::event::{ElementState, MouseButton, WindowEvent};

pub const VIEWPORT: (f32, f32) = (1200.0, 760.0);

pub struct Harness {
    pub w: FileExplorerWidget,
    state: ui::State,
    tr: TextRenderer,
    taffy: TaffyTree<()>,
    node_map: HashMap<NodeId, *mut dyn Widget>,
    child: NodeId,
}

impl Harness {
    pub fn new(start_dir: &Path) -> Self {
        // No persistence dir / home so the test never touches the real user's
        // favourites or config.
        let mut w = FileExplorerWidget::with_persistence(None, None);
        w.navigate(start_dir.to_path_buf());
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let placeholder = taffy.new_leaf(Style::default()).unwrap();
        let mut h = Harness {
            w,
            state: ui::State::new(),
            tr: TextRenderer::new(),
            taffy,
            node_map: HashMap::new(),
            child: placeholder,
        };
        // Take the double-click window out of play. These tests drive two
        // presses around a full relayout, and on a loaded machine that costs
        // more than the real 450 ms — at which point a deliberate double-click
        // reads as two singles and the test fails for a reason it is not
        // about. Nothing here asserts on the window itself.
        ui::gesture::set_multi_click_interval(std::time::Duration::from_mins(1));
        // Two frames: the first lets the responsive `nav` learn its width and
        // settle to its docked (wide) mode; the second is the steady-state
        // geometry the user actually interacts with.
        h.layout();
        h.layout();
        h
    }

    /// Run the host's per-frame `sync → build_layout → compute → apply_layout`
    /// cycle. Calling this between events reproduces the structural rebuild that
    /// discards the previous frame's view instances.
    pub fn layout(&mut self) {
        self.w.sync(&mut self.state.doc, &self.state.root);
        self.taffy = TaffyTree::new();
        self.node_map.clear();
        let child = self
            .w
            .build_layout(&mut self.taffy, &ui::Theme::dark(), &mut self.node_map);
        let (vw, vh) = VIEWPORT;
        let root = self
            .taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(vw),
                        height: Dimension::length(vh),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();
        let measure_map = self.node_map.clone();
        let tr = &mut self.tr;
        self.taffy
            .compute_layout_with_measure(
                root,
                Size {
                    width: AvailableSpace::Definite(vw),
                    height: AvailableSpace::Definite(vh),
                },
                |known, available, node_id, _, _| {
                    if let Some(&ptr) = measure_map.get(&node_id) {
                        unsafe { (*ptr).measure(known, available, tr) }
                    } else {
                        Size::ZERO
                    }
                },
            )
            .unwrap();
        self.w.apply_layout(&self.taffy, child, Point::ORIGIN);
        self.child = child;
    }

    pub fn dispatch(&mut self, ev: WindowEvent) {
        let mut handled = false;
        let mut focus = None;
        let mods = winit::event::Modifiers::default();
        self.w.handle_event(
            &ev,
            1.0,
            &mods,
            &mut self.state.doc,
            &self.state.root,
            &mut handled,
            &mut focus,
            self.child,
            &self.taffy,
        );
    }

    pub fn move_cursor(&mut self, p: Point) {
        self.dispatch(WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: (p.x, p.y).into(),
        });
    }
    pub fn press(&mut self) {
        self.dispatch(WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        });
    }
    pub fn release(&mut self) {
        self.dispatch(WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button: MouseButton::Left,
        });
    }
}

/// Locate the on-screen centre of displayed row `index` by probing throwaway
/// harnesses: for each candidate y, a single click that selects exactly `index`
/// marks a hit. Returns the centre of the contiguous hit band, robust to the
/// exact header / row metrics. `x` is the middle of the content pane.
pub fn row_center(dir: &Path, index: usize) -> Point {
    let content = Harness::new(dir).w.debug_content_rect();
    let x = content.center().x;
    let mut hits: Vec<f64> = Vec::new();
    let mut y = content.y0 + 2.0;
    while y < content.y1 {
        let mut h = Harness::new(dir);
        h.move_cursor(Point::new(x, y));
        h.press();
        h.release();
        if h.w.selected_indices() == vec![index] && h.w.current_path() == dir {
            hits.push(y);
        } else if !hits.is_empty() {
            break;
        }
        y += 3.0;
    }
    assert!(
        !hits.is_empty(),
        "row {index} was never hit while probing the content pane"
    );
    let mid = hits[hits.len() / 2];
    Point::new(x, mid)
}
