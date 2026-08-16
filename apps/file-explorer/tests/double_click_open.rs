//! Double-click-to-open must be reliable, including the case the review flagged
//! as "only sometimes works": a *still* double-click whose second press arrives
//! with no pointer movement after the host has rebuilt the view.
//!
//! The file explorer rebuilds its whole child tree every frame. The file view
//! hit-tests a press against its own `last_cursor`; a freshly rebuilt view that
//! is not seeded with the real cursor starts at `(0, 0)`, so a press with no
//! preceding `CursorMoved` misses the row and the folder never opens. This test
//! drives the real widget through a rebuild between the two clicks and proves
//! the second click still opens the folder.

use std::collections::HashMap;
use std::path::Path;

use file_explorer::FileExplorerWidget;
use taffy::prelude::*;
use ui::text_render::TextRenderer;
use ui::widget::Widget;
use vello::kurbo::Point;
use winit::event::{ElementState, MouseButton, WindowEvent};

const VIEWPORT: (f32, f32) = (1200.0, 760.0);

struct Harness {
    w: FileExplorerWidget,
    state: ui::State,
    tr: TextRenderer,
    taffy: TaffyTree<()>,
    node_map: HashMap<NodeId, *mut dyn Widget>,
    child: NodeId,
}

impl Harness {
    fn new(start_dir: &Path) -> Self {
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
        // Two frames: the first lets the responsive `nav` learn its width and
        // settle to its docked (wide) mode; the second is the steady-state
        // geometry the user actually interacts with. (The live app has already
        // settled long before the first click.)
        h.layout();
        h.layout();
        h
    }

    /// Run the host's per-frame `sync → build_layout → compute → apply_layout`
    /// cycle. Calling this between two clicks reproduces the structural rebuild
    /// that discards the previous frame's view instances.
    fn layout(&mut self) {
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
                    size: Size { width: Dimension::length(vw), height: Dimension::length(vh) },
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

    fn dispatch(&mut self, ev: WindowEvent) {
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

    fn move_cursor(&mut self, p: Point) {
        self.dispatch(WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: (p.x, p.y).into(),
        });
    }
    fn press(&mut self) {
        self.dispatch(WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        });
    }
    fn release(&mut self) {
        self.dispatch(WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button: MouseButton::Left,
        });
    }
}

/// A temp dir containing a single folder (row 0) plus a file.
fn dir_with_folder() -> (tempfile::TempDir, std::path::PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let folder = d.path().join("Aardvark"); // folders lead the sort anyway
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(d.path().join("z-note.txt"), b"x").unwrap();
    (d, folder)
}

/// Locate the on-screen centre of displayed row 0 by probing throwaway
/// harnesses: for each candidate y, a single click that selects exactly row 0
/// marks a hit. Returns the centre of the contiguous hit band, so the point is
/// robust to the exact header / row metrics. `x` is the middle of the content
/// pane (well within the flex Name column).
fn row0_center(dir: &Path) -> Point {
    let content = Harness::new(dir).w.debug_content_rect();
    let x = content.center().x;
    let mut hits: Vec<f64> = Vec::new();
    let mut y = content.y0 + 2.0;
    while y < content.y1 {
        let mut h = Harness::new(dir);
        h.move_cursor(Point::new(x, y));
        h.press();
        h.release();
        if h.w.selected_indices() == vec![0] && h.w.current_path() == dir {
            hits.push(y);
        } else if !hits.is_empty() {
            break; // left the row-0 band
        }
        y += 3.0;
    }
    assert!(!hits.is_empty(), "row 0 was never hit while probing the content pane");
    let mid = hits[hits.len() / 2];
    Point::new(x, mid)
}

#[test]
fn single_click_selects_row_zero_but_does_not_open() {
    let (dir, _folder) = dir_with_folder();
    let p = row0_center(dir.path());
    let mut h = Harness::new(dir.path());
    let start = h.w.current_path();
    h.move_cursor(p);
    h.press();
    h.release();
    assert_eq!(h.w.selected_indices(), vec![0], "the row under the cursor is selected");
    assert_eq!(h.w.current_path(), start, "a single click never navigates");
}

#[test]
fn double_click_across_a_rebuild_opens_the_folder() {
    // The exact flaky path: the first click triggers a structural rebuild, and
    // the second press arrives with NO CursorMoved after it. A view not seeded
    // with the real cursor would hit-test the press against (0,0) and miss.
    let (dir, folder) = dir_with_folder();
    let p = row0_center(dir.path());
    let mut h = Harness::new(dir.path());

    // First click (with the pointer move a real gesture has).
    h.move_cursor(p);
    h.press();
    h.release();
    assert_eq!(h.w.selected_indices(), vec![0]);

    // The host rebuilds the tree after the selection change.
    h.layout();

    // Second click — deliberately WITHOUT moving the pointer first.
    h.press();
    h.release();

    assert_eq!(
        h.w.current_path(),
        folder,
        "the still double-click opened the folder despite the intervening rebuild",
    );
}

#[test]
fn fast_double_click_without_a_rebuild_also_opens() {
    // The normal path (no rebuild between the two clicks) must keep working.
    let (dir, folder) = dir_with_folder();
    let p = row0_center(dir.path());
    let mut h = Harness::new(dir.path());
    h.move_cursor(p);
    h.press();
    h.release();
    h.press();
    h.release();
    assert_eq!(h.w.current_path(), folder, "a fast double-click opens the folder");
}
