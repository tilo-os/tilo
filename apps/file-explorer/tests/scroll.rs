//! Driven scrolling tests for the file list and the grid.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use file_explorer::FileExplorerWidget;
use file_explorer::model::ViewMode;
use ui::headless::Framed;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::Rect;
use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};

fn dir_with_files(n: usize) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    for i in 0..n {
        std::fs::write(d.path().join(format!("file_{i:04}.txt")), b"x").unwrap();
    }
    d
}

struct Harness {
    widget: Framed<FileExplorerWidget>,
}

impl Harness {
    fn new(dir: &std::path::Path, mode: ViewMode, w: f32, h: f32) -> Self {
        let widget = FileExplorerWidget::with_persistence(None, None);
        widget.state().set_view_mode(mode);
        widget.state().navigate(dir.to_path_buf());
        assert!(
            widget.state().entries_snapshot().len() >= 100,
            "test needs a long directory; got {}",
            widget.state().entries_snapshot().len()
        );
        Self {
            widget: Framed::new(
                ui::State::new(),
                widget,
                ui::headless::viewport((w, h)),
                Theme::dark(),
            ),
        }
    }

    fn relayout(&mut self) {
        Framed::harness_mut(&mut self.widget).relayout();
    }

    fn shot(&self, out: &std::path::Path) {
        Framed::harness(&self.widget).render_png(out);
    }

    fn dispatch(&mut self, event: &WindowEvent, modifiers: Modifiers) -> bool {
        Framed::harness_mut(&mut self.widget).dispatch_to_root_with(event, &modifiers)
    }

    fn wheel(&mut self, notches_down: f32) {
        let ev = ui::synthetic_input::mouse_wheel(0.0, -notches_down);
        self.dispatch(&ev, Modifiers::default());
    }

    fn cursor(&mut self, x: f64, y: f64) {
        let ev = ui::synthetic_input::cursor_moved(x, y);
        self.dispatch(&ev, Modifiers::default());
    }

    fn mouse(&mut self, state: ElementState) -> bool {
        let ev = ui::synthetic_input::mouse_input(state, MouseButton::Left);
        self.dispatch(&ev, Modifiers::default())
    }

    fn rect_of(&self, name: &str) -> Rect {
        fn find(w: &dyn Widget, name: &str) -> Option<Rect> {
            if w.name() == name {
                return Some(w.get_rect());
            }
            for c in w.children() {
                if let Some(r) = find(c, name) {
                    return Some(r);
                }
            }
            None
        }
        find(&*self.widget, name).unwrap_or_else(|| panic!("no `{name}` widget in the tree"))
    }
}

fn first_mounted_row(h: &Harness) -> u64 {
    fn find(w: &dyn Widget) -> Option<u64> {
        if w.name() == "FileView" {
            return w.accessibility().and_then(|node| {
                node.children
                    .iter()
                    .filter_map(|row| row.pos_in_set)
                    .min()
                    .map(|position| (position as u64).saturating_sub(1))
            });
        }
        w.children().into_iter().find_map(find)
    }
    find(&*h.widget).expect("the file view exposes its rows")
}

fn applied_offset(h: &Harness) -> f64 {
    let viewport = h.rect_of("Scrollable");
    let content = h.rect_of("FileView");
    viewport.y0 - content.y0
}

fn assert_list_scrolls_via_wheel() {
    let dir = dir_with_files(160);
    let mut h = Harness::new(dir.path(), ViewMode::List, 1000.0, 600.0);
    let table = h.rect_of("FileView");
    assert_eq!(first_mounted_row(&h), 0, "starts unscrolled");
    h.cursor(table.center().x, table.center().y);
    h.wheel(1000.0);
    h.relayout();
    assert!(
        first_mounted_row(&h) > 100,
        "flinging to the bottom scrolls the list to its last rows, got {}",
        first_mounted_row(&h)
    );
}

fn assert_scrolls_via_wheel(mode: ViewMode) {
    let dir = dir_with_files(160);
    let mut h = Harness::new(dir.path(), mode, 1000.0, 600.0);

    let viewport = h.rect_of("Scrollable");
    let content_before = h.rect_of("FileView");

    assert!(
        content_before.height() > viewport.height() + 50.0,
        "{mode:?}: content ({}) must overflow the viewport ({})",
        content_before.height(),
        viewport.height(),
    );
    assert!(
        content_before.y1 > viewport.y1 + 50.0,
        "{mode:?}: bottom entries must start off-screen (content.y1={} viewport.y1={})",
        content_before.y1,
        viewport.y1,
    );
    assert!(
        applied_offset(&h).abs() < 0.5,
        "{mode:?}: starts unscrolled"
    );

    // A wheel event contains no pointer position.  Put the cursor in the
    // viewport first, exactly as the window host does before a user wheels.
    h.cursor(viewport.center().x, viewport.center().y);

    h.wheel(3.0);
    h.relayout();
    let one_notch = applied_offset(&h);
    assert!(
        one_notch > 0.5,
        "{mode:?}: a wheel-down must scroll the list; offset stayed {one_notch}",
    );

    h.wheel(1000.0);
    h.relayout();
    let content_after = h.rect_of("FileView");
    let max_offset = (content_before.height() - viewport.height()).max(0.0);
    let bottom = applied_offset(&h);
    assert!(
        bottom > one_notch,
        "{mode:?}: flinging further must scroll further ({bottom} vs {one_notch})",
    );
    assert!(
        (bottom - max_offset).abs() < 2.0,
        "{mode:?}: scroll clamps at the end (got {bottom}, max {max_offset})",
    );
    assert!(
        (content_after.y1 - viewport.y1).abs() < 2.0,
        "{mode:?}: fully scrolled, the last entries end at the viewport's bottom \
         (content.y1={} bottom={})",
        content_after.y1,
        viewport.y1,
    );
}

#[test]
fn long_directories_scroll_by_wheel_and_thumb_with_optional_shots() {
    list_scrolls_with_the_mouse_wheel();
    grid_scrolls_with_the_mouse_wheel();
    list_scrolls_by_dragging_the_scrollbar_thumb();
    scroll_before_after_shots();
}

fn list_scrolls_with_the_mouse_wheel() {
    assert_list_scrolls_via_wheel();
}

fn grid_scrolls_with_the_mouse_wheel() {
    assert_scrolls_via_wheel(ViewMode::Grid);
}

fn scroll_before_after_shots() {
    let Ok(dir_out) = std::env::var("SCROLL_SHOT_DIR") else {
        return;
    };
    let out = std::path::PathBuf::from(dir_out);
    std::fs::create_dir_all(&out).unwrap();

    for (mode, tag) in [(ViewMode::List, "list"), (ViewMode::Grid, "grid")] {
        let dir = dir_with_files(160);
        let mut h = Harness::new(dir.path(), mode, 1000.0, 600.0);
        h.shot(&out.join(format!("{tag}-before.png")));

        h.wheel(1000.0);
        h.relayout();
        h.shot(&out.join(format!("{tag}-after.png")));

        let moved = match mode {
            ViewMode::List => first_mounted_row(&h) > 50,
            ViewMode::Grid => applied_offset(&h) > 50.0,
        };
        assert!(moved, "{mode:?}: shot pair captured a real scroll");
    }
}

fn list_scrolls_by_dragging_the_scrollbar_thumb() {
    let dir = dir_with_files(160);
    let mut h = Harness::new(dir.path(), ViewMode::List, 1000.0, 600.0);

    let table = h.rect_of("FileView");
    let bar_x = table.x1 - 6.0;
    let start_y = table.y0 + 64.0;

    h.cursor(bar_x, start_y);
    assert!(
        h.mouse(ElementState::Pressed),
        "press on the scrollbar thumb must be handled (bar is present)",
    );
    assert_eq!(first_mounted_row(&h), 0, "no movement before the drag");

    h.cursor(bar_x, table.y0 + 300.0);
    h.relayout();
    assert!(
        first_mounted_row(&h) > 0,
        "dragging the thumb down scrolls the list"
    );

    h.mouse(ElementState::Released);
}
