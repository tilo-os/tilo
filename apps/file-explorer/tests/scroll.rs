//! Driven scrolling tests for the file list + grid.
//!
//! A directory with far more entries than fit on screen must scroll. These
//! tests build the *real* composed widget tree, lay it out at a fixed
//! viewport, then drive scrolling through the genuine winit event path
//! (mouse wheel + a scrollbar-thumb drag) and assert that
//!  * the list actually moves (the content offset changes), and
//!  * entries that were below the fold become visible above the reserved
//!    status-bubble lane once scrolled to the end.
//!
//! Geometry is read only through the public `Widget` trait (`name`,
//! `get_rect`, `children`) so the assertions ride the production layout — no
//! private hooks, no re-created widgets.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use file_explorer::FileExplorerWidget;
use file_explorer::files_app::status_bubble_safe_inset;
use file_explorer::model::ViewMode;
use ui::headless::Framed;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::Rect;
use winit::event::{
    ElementState, Modifiers, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent,
};

/// A temp dir with `n` uniformly-named files (sorted, so index maps to a
/// predictable row).
fn dir_with_files(n: usize) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    for i in 0..n {
        std::fs::write(d.path().join(format!("file_{i:04}.txt")), b"x").unwrap();
    }
    d
}

/// The production host: mount the widget tree at `w`×`h` and run its frames,
/// so the caller can drive events and re-run the frame after a scroll.
struct Harness {
    widget: Framed<FileExplorerWidget>,
}

impl Harness {
    fn new(dir: &std::path::Path, mode: ViewMode, w: f32, h: f32) -> Self {
        let mut widget = FileExplorerWidget::with_persistence(None, None);
        widget.set_view_mode(mode);
        widget.navigate(dir.to_path_buf());
        assert!(
            widget.entries_snapshot().len() >= 100,
            "test needs a long directory; got {}",
            widget.entries_snapshot().len()
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

    /// Run the next frame. The scroll offset lives in the Scrollable and is
    /// applied to descendants there; with no structural change the host only
    /// re-applies the retained layout, the exact pass the App re-runs after a
    /// wheel/drag flags the layout dirty.
    fn relayout(&mut self) {
        Framed::harness_mut(&mut self.widget).relayout();
    }

    /// Render the current frame to a PNG — the same offscreen path `--shots`
    /// uses, so the image is what a live window would show.
    fn shot(&self, out: &std::path::Path) {
        Framed::harness(&self.widget).render_png(out);
    }

    fn dispatch(&mut self, event: &WindowEvent, modifiers: Modifiers) -> bool {
        Framed::harness_mut(&mut self.widget).dispatch_to_root_with(event, &modifiers)
    }

    fn wheel(&mut self, notches_down: f32) {
        // winit y-positive = scroll up; the Scrollable negates, so a negative
        // y scrolls DOWN (reveals content below).
        let ev = WindowEvent::MouseWheel {
            device_id: unsafe { std::mem::zeroed() },
            delta: MouseScrollDelta::LineDelta(0.0, -notches_down),
            phase: TouchPhase::Moved,
        };
        self.dispatch(&ev, Modifiers::default());
    }

    fn cursor(&mut self, x: f64, y: f64) {
        let ev = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(x, y),
        };
        self.dispatch(&ev, Modifiers::default());
    }

    fn mouse(&mut self, state: ElementState) -> bool {
        let ev = WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state,
            button: MouseButton::Left,
        };
        self.dispatch(&ev, Modifiers::default())
    }

    /// Rect of the first descendant whose `name()` matches, searched over the
    /// live widget tree (the production composition).
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

/// The offset the Scrollable has applied to its content = how far the content's
/// top sits above the viewport's top.
fn applied_offset(h: &Harness) -> f64 {
    let viewport = h.rect_of("Scrollable");
    let content = h.rect_of("FileView");
    viewport.y0 - content.y0
}

fn assert_scrolls_via_wheel(mode: ViewMode) {
    let dir = dir_with_files(160);
    let mut h = Harness::new(dir.path(), mode, 1000.0, 600.0);

    let viewport = h.rect_of("Scrollable");
    let content_before = h.rect_of("FileView");

    // Precondition: the directory genuinely overflows the viewport, and its
    // bottom is well below the fold (there ARE entries you can't see).
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

    // One wheel notch down must move the list.
    h.wheel(3.0);
    h.relayout();
    let one_notch = applied_offset(&h);
    assert!(
        one_notch > 0.5,
        "{mode:?}: a wheel-down must scroll the list; offset stayed {one_notch}",
    );

    // Fling to the bottom: the content's bottom edge reaches the top of the
    // reserved status-bubble lane, so the last entries remain visible rather
    // than sitting under the floating status pill.
    h.wheel(1000.0);
    h.relayout();
    let content_after = h.rect_of("FileView");
    let safe_inset = f64::from(status_bubble_safe_inset(&ui::Theme::dark()));
    let max_offset = (content_before.height() - (viewport.height() - safe_inset)).max(0.0);
    let bottom = applied_offset(&h);
    assert!(
        bottom > one_notch,
        "{mode:?}: flinging further must scroll further ({bottom} vs {one_notch})",
    );
    assert!(
        (bottom - max_offset).abs() < 2.0,
        "{mode:?}: scroll clamps above the status lane (got {bottom}, max {max_offset})",
    );
    assert!(
        (content_after.y1 - (viewport.y1 - safe_inset)).abs() < 2.0,
        "{mode:?}: fully scrolled, the last entries sit above the status lane \
         (content.y1={} safe-bottom={})",
        content_after.y1,
        viewport.y1 - safe_inset,
    );
}

/// List and grid wheel scrolling, the list thumb drag, and the opt-in
/// (`SCROLL_SHOT_DIR`) before/after renders — each phase on a fresh directory.
#[test]
fn long_directories_scroll_by_wheel_and_thumb_with_optional_shots() {
    list_scrolls_with_the_mouse_wheel();
    grid_scrolls_with_the_mouse_wheel();
    list_scrolls_by_dragging_the_scrollbar_thumb();
    scroll_before_after_shots();
}

fn list_scrolls_with_the_mouse_wheel() {
    assert_scrolls_via_wheel(ViewMode::List);
}

fn grid_scrolls_with_the_mouse_wheel() {
    assert_scrolls_via_wheel(ViewMode::Grid);
}

/// Visual before/after evidence: render a long directory unscrolled, then
/// scrolled to the bottom, into two PNGs. Opt-in via `SCROLL_SHOT_DIR` (needs
/// a GPU/software-wgpu path) so the standard `cargo test` run stays fast and
/// display-free; the driven-scroll asserts above are the always-on proof.
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

        // The two frames must genuinely differ (the list moved).
        assert!(
            applied_offset(&h) > 50.0,
            "{mode:?}: shot pair captured a real scroll"
        );
    }
}

fn list_scrolls_by_dragging_the_scrollbar_thumb() {
    let dir = dir_with_files(160);
    let mut h = Harness::new(dir.path(), ViewMode::List, 1000.0, 600.0);

    let viewport = h.rect_of("Scrollable");
    // The vertical bar hugs the right edge; its thumb centre near the top.
    let bar_x = viewport.x1 - 6.0;
    let start_y = viewport.y0 + 8.0;

    // Hover the thumb, press it (begins a drag), drag down, release — all
    // through the real event routing.
    h.cursor(bar_x, start_y);
    assert!(
        h.mouse(ElementState::Pressed),
        "press on the scrollbar thumb must be handled (bar is present)",
    );
    assert!(
        applied_offset(&h).abs() < 0.5,
        "no movement before the drag"
    );

    h.cursor(bar_x, viewport.y0 + 250.0);
    h.relayout();
    let dragged = applied_offset(&h);
    assert!(
        dragged > 0.5,
        "dragging the thumb down scrolls the list; got {dragged}"
    );

    h.mouse(ElementState::Released);

    // The content actually moved under the viewport: an entry that was at the
    // top is now above it.
    let content = h.rect_of("FileView");
    assert!(
        content.y0 < viewport.y0 - 0.5,
        "content top scrolled above the viewport (content.y0={} viewport.y0={})",
        content.y0,
        viewport.y0,
    );
}
