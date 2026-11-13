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
#![allow(
    clippy::arithmetic_side_effects,
    reason = "indices into the few hundred probe heights of one pane"
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

/// The names of what `dir` holds, sorted: what a scenario left on disk.
pub fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// How many probe heights the first pass of [`row_center`] steps over: 12 px, less than
/// a row is tall.
const ROW_SEARCH_STRIDE: usize = 4;

/// The centre of row `index` of `dir`'s listing: the middle of the probe heights, one
/// every 3 px down the content pane, at which a click in a window of its own selects
/// that row alone. A row answers clicks in one band of the pane (the view asks which
/// row's rectangle holds the point), so the band is found by a hit inside it and by
/// halving towards its two edges, not by a click at every height above and inside it.
pub fn row_center(dir: &Path, index: usize) -> Point {
    let content = Harness::new(dir).w.debug_content_rect();
    let x = content.center().x;
    let ys: Vec<f64> = std::iter::successors(Some(content.y0 + 2.0), |&y| Some(y + 3.0))
        .take_while(|&y| y < content.y1)
        .collect();
    let hit = |at: usize| {
        let Some(&y) = ys.get(at) else {
            return false;
        };
        let mut h = Harness::new(dir);
        h.move_cursor(Point::new(x, y));
        h.press();
        h.release();
        let (selected, shown) = {
            let state = h.w.state();
            (state.selected_indices(), state.current_path())
        };
        selected == vec![index] && shown == dir
    };
    // A band narrower than the stride is still found, by trying every height.
    let found = (0..ys.len())
        .step_by(ROW_SEARCH_STRIDE)
        .find(|&at| hit(at))
        .or_else(|| (0..ys.len()).find(|&at| hit(at)));
    assert!(
        found.is_some(),
        "row {index} was never hit while probing the content pane"
    );
    let found = found.unwrap();

    // The band's first height lies after the miss one stride above the hit.
    let mut first = found;
    let mut above = found
        .checked_sub(ROW_SEARCH_STRIDE)
        .map_or(0, |miss| miss + 1);
    while above < first {
        let mid = above + (first - above) / 2;
        if hit(mid) {
            first = mid;
        } else {
            above = mid + 1;
        }
    }
    // Its last height lies before the first miss a stride at a time below the hit.
    let mut last = found;
    let mut below = found + ROW_SEARCH_STRIDE;
    while hit(below) {
        last = below;
        below += ROW_SEARCH_STRIDE;
    }
    while below - last > 1 {
        let mid = last + (below - last) / 2;
        if hit(mid) {
            last = mid;
        } else {
            below = mid;
        }
    }
    // The middle of the band's heights, `first..=last`: half their number past the first.
    Point::new(x, ys[first + (last - first).div_ceil(2)])
}
