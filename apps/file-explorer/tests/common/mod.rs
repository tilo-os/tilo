//! Shared harness for the file-explorer integration tests: a pinned locale, a
//! headless window, and one frame's steps.

#![allow(
    dead_code,
    reason = "each test binary that includes this harness uses only part of it"
)]
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::Path;

use file_explorer::FileExplorerWidget;
use ui::headless::Framed;
use vello::kurbo::Point;
use winit::event::{Modifiers, WindowEvent};

pub const VIEWPORT: (f32, f32) = (1200.0, 760.0);

pub const TEST_LOCALE: &str = "en-US";

/// Pin this test thread's UI locale. Files otherwise selects its catalog from
/// the host's `LC_ALL` / `LC_MESSAGES` / `LANGUAGE` / `LANG` the first time a
/// thread resolves a string, so assertions on copy would pass or fail by the
/// machine they run on. Call it before the test resolves any string.
pub fn pin_locale() {
    file_explorer::i18n::set_locale(TEST_LOCALE);
}

pub fn app_command() -> std::process::Command {
    let mut command = std::process::Command::new(tilo_test_env::package_binary!("file-explorer"));
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
        pin_locale();
        let w = FileExplorerWidget::with_persistence(None, None);
        w.state().navigate(start_dir.to_path_buf());
        ui::gesture::set_multi_click_interval(std::time::Duration::from_secs(60));
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
        self.dispatch(&ui::synthetic_input::mouse_press());
    }
    pub fn release(&mut self) {
        self.dispatch(&ui::synthetic_input::mouse_release());
    }
}

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
        let (selected, shown) = {
            let state = h.w.state();
            (state.selected_indices(), state.current_path())
        };
        if selected == vec![index] && shown == dir {
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
