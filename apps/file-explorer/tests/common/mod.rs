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

use std::path::Path;

use file_explorer::FileExplorerWidget;
use ui::headless::Framed;
use vello::kurbo::Point;
use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};

pub const VIEWPORT: (f32, f32) = (1200.0, 760.0);

/// The locale every test runs under: the catalog's source locale.
pub const TEST_LOCALE: &str = "en-US";

/// Pin this test thread's UI locale. Files otherwise selects its catalog from
/// the host's `LC_ALL` / `LC_MESSAGES` / `LANGUAGE` / `LANG` the first time a
/// thread resolves a string, so assertions on copy would pass or fail by the
/// machine they run on. Call it before the test resolves any string.
pub fn pin_locale() {
    file_explorer::i18n::set_locale(TEST_LOCALE);
}

/// The Files binary with a pinned locale environment, so a spawned run
/// renders the same copy on every host.
pub fn app_command() -> std::process::Command {
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_file-explorer"));
    command
        .env("LC_ALL", "en_US.UTF-8")
        .env("LANG", "en_US.UTF-8")
        .env_remove("LANGUAGE")
        .env_remove("LC_MESSAGES");
    command
}

pub struct Harness {
    pub w: Framed<FileExplorerWidget>,
}

impl Harness {
    pub fn new(start_dir: &Path) -> Self {
        // No persistence dir / home so the test never touches the real user's
        // favourites or config.
        pin_locale();
        let mut w = FileExplorerWidget::with_persistence(None, None);
        w.navigate(start_dir.to_path_buf());
        // Take the double-click window out of play. These tests drive two
        // presses around a full relayout, and on a loaded machine that costs
        // more than the real 450 ms — at which point a deliberate double-click
        // reads as two singles and the test fails for a reason it is not
        // about. Nothing here asserts on the window itself.
        ui::gesture::set_multi_click_interval(std::time::Duration::from_secs(60));
        // Two frames: the first (at mount) lets the responsive `nav` learn its
        // width and settle to its docked (wide) mode; the second is the
        // steady-state geometry the user actually interacts with.
        let mut h = Harness {
            w: Framed::new(
                ui::State::new(),
                w,
                ui::headless::viewport(VIEWPORT),
                ui::Theme::dark(),
            ),
        };
        h.layout();
        h
    }

    /// Run the host's full frame (`sync → build_layout → compute →
    /// apply_layout → draw`), rebuilding the whole tree. Calling this between
    /// events reproduces the structural rebuild that discards the previous
    /// frame's view instances. `FileView` publishes only the folders
    /// intersecting Scrollable's viewport as drop targets during draw, which
    /// a production frame always runs after layout.
    pub fn layout(&mut self) {
        Framed::harness_mut(&mut self.w).recompose().unwrap();
    }

    pub fn dispatch(&mut self, ev: &WindowEvent) {
        Framed::harness_mut(&mut self.w).dispatch_to_root_with(ev, &Modifiers::default());
    }

    pub fn move_cursor(&mut self, p: Point) {
        self.dispatch(&WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: (p.x, p.y).into(),
        });
    }
    pub fn press(&mut self) {
        self.dispatch(&WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        });
    }
    pub fn release(&mut self) {
        self.dispatch(&WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
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
    let ys = std::iter::successors(Some(content.y0 + 2.0), |&y| Some(y + 3.0))
        .take_while(|&y| y < content.y1);
    for y in ys {
        let mut h = Harness::new(dir);
        h.move_cursor(Point::new(x, y));
        h.press();
        h.release();
        if h.w.selected_indices() == vec![index] && h.w.current_path() == dir {
            hits.push(y);
        } else if !hits.is_empty() {
            break;
        }
    }
    assert!(
        !hits.is_empty(),
        "row {index} was never hit while probing the content pane"
    );
    let mid = hits[hits.len() / 2];
    Point::new(x, mid)
}
