//! Visual harness for expand-to-fullscreen and its floating pill decoration.
//! It renders the compositor's `FrameBuilder` before and after clicking a
//! tab's Expand chip and writes PNGs for inspection.
//!
//! `#[ignore]` by default — needs a GPU adapter, and is a review tool, not
//! a gate. Run with an output directory:
//!
//! ```text
//! TILO_SHOT_DIR=/tmp/shots cargo test -p shell --test expand_fullscreen_visual \
//!     -- --ignored --nocapture
//! ```

#![allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]

use ui::offscreen::OffscreenRenderer;

use automerge::{AutoCommit, ROOT};
use shell::compositor::frame::FrameBuilder;
use vello::kurbo::Point;
use vello::peniko::Color;
use winit::event::{Modifiers, WindowEvent};

const W: u32 = 1600;
const H: u32 = 900;

fn shot_dir() -> std::path::PathBuf {
    std::env::var("TILO_SHOT_DIR").map_or_else(
        |_| std::env::temp_dir().join("tilo-expand-shots"),
        std::path::PathBuf::from,
    )
}

struct Driver {
    doc: AutoCommit,
    focus: Option<taffy::NodeId>,
}

impl Driver {
    fn new() -> Self {
        Self {
            doc: AutoCommit::new(),
            focus: None,
        }
    }
    fn send(&mut self, fb: &mut FrameBuilder, ev: &WindowEvent) {
        let mods = Modifiers::default();
        fb.dispatch_event(
            ev,
            &mods,
            &mut ui::widget::Edits::lend(&mut self.doc),
            &ROOT,
            &mut self.focus,
        );
    }
    fn cursor(&mut self, fb: &mut FrameBuilder, p: Point) {
        self.send(fb, &ui::synthetic_input::cursor_moved(p.x, p.y));
    }
    fn click(&mut self, fb: &mut FrameBuilder) {
        self.send(fb, &ui::synthetic_input::mouse_press());
        self.send(fb, &ui::synthetic_input::mouse_release());
    }
}

#[test]
#[ignore = "a review tool: needs a GPU adapter to render the captures"]
fn render_expand_to_fullscreen() {
    let dir = shot_dir();
    std::fs::create_dir_all(&dir).expect("shot dir");
    let mut fb = FrameBuilder::new((W, H));
    let mut r = OffscreenRenderer::new(W, H);
    let mut d = Driver::new();

    fb.relayout();
    let scene = fb.paint();
    r.render_png(&scene, Color::BLACK, dir.join("01-tiled.png"));

    let strip_id = {
        let chrome = fb.chrome();
        let tree = chrome.tree();
        tree.desktop
            .active_workspace()
            .and_then(|w| w.strips.first())
            .map(|s| s.id)
            .expect("one strip")
    };

    // Per-tab chips only lay out hit rects once the pointer is over the pill.
    let pill_center = {
        let g = fb
            .host_mut()
            .group_at_for_tests(strip_id, 0)
            .expect("first group");
        g.tab_rects().first().copied().expect("one pill").center()
    };
    d.cursor(&mut fb, pill_center);
    // A pill opens once the pointer has rested on it for the hover dwell.
    let mut dwelt = false;
    if let Some(row) = fb.host_mut().group_row_mut_for_tests(strip_id) {
        for g in &mut row.groups {
            dwelt |= g.bar_mut_for_tests().elapse_hover_dwell();
        }
    }
    assert!(dwelt, "resting on the pill opened nothing");
    fb.relayout();
    // The chip cluster slides out on wall time, and the rects stay `Rect::ZERO`
    // until that curve has run, so hold it past the end and lay out again.
    // Without this the capture below shows a pill with no chips and the click
    // lands on nothing — which is what this file did, silently, because it
    // printed the outcome instead of asserting it.
    let mut held = false;
    if let Some(row) = fb.host_mut().group_row_mut_for_tests(strip_id) {
        for g in &mut row.groups {
            held |= g
                .bar_mut_for_tests()
                .hold_reveal_at(std::time::Duration::from_secs(1));
        }
    }
    assert!(held, "hovering the pill started no chip reveal");
    fb.relayout();

    let chip = {
        let g = fb.host_mut().group_at_for_tests(strip_id, 0).unwrap();
        let r = g
            .bar_for_tests()
            .max_rects()
            .first()
            .copied()
            .expect("Expand chip");
        assert!(r.area() > 0.0, "the Expand chip has no hit rect to click");
        r.center()
    };
    let scene = fb.paint();
    r.render_png(&scene, Color::BLACK, dir.join("02-pill-hovered.png"));

    d.cursor(&mut fb, chip);
    d.click(&mut fb);
    fb.host_mut().tick_pending_maximize_harvest_for_tests();
    fb.relayout();
    let maximized = fb
        .host_mut()
        .group_at_for_tests(strip_id, 0)
        .is_some_and(|g| g.maximized);
    assert!(
        maximized,
        "the Expand click must maximize the group — the shots below are of \
         that state, and are worthless if it never happened",
    );
    // Cursor in the top reveal band, so the floating pill is showing.
    d.cursor(&mut fb, Point::new(200.0, 20.0));
    fb.relayout();
    let scene = fb.paint();
    r.render_png(
        &scene,
        Color::BLACK,
        dir.join("03-fullscreen-chrome-shown.png"),
    );

    // Cursor away from the top: the floating pill must auto-hide.
    d.cursor(&mut fb, Point::new(800.0, 600.0));
    fb.relayout();
    let scene = fb.paint();
    r.render_png(
        &scene,
        Color::BLACK,
        dir.join("04-fullscreen-chrome-hidden.png"),
    );

    println!("wrote shots to {}", dir.display());
}
