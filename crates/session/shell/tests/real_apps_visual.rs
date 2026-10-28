//! Visual probe: does every bootstrap tab come up as an app?
//!
//! Renders each boot-workspace tab (Notes/Files/Terminal) after activating
//! it, so the result can be looked at instead of inferred; the terminal's
//! frames are pumped over a short window since its PTY needs a moment to
//! emit a prompt.
//!
//! `#[ignore]` — needs a GPU adapter and spawns a shell; a review
//! tool, not a gate.
//! ```text
//! TILO_SHOT_DIR=/tmp/shots cargo test -p shell --test real_apps_visual \
//!     -- --ignored --nocapture
//! ```

#![allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]

use ui::offscreen::OffscreenRenderer;

use automerge::{AutoCommit, ROOT};
use shell::shell::frame::FrameBuilder;
use vello::kurbo::Point;
use vello::peniko::Color;
use winit::event::{Modifiers, WindowEvent};

const W: u32 = 1600;
const H: u32 = 900;

fn shot_dir() -> std::path::PathBuf {
    std::env::var("TILO_SHOT_DIR").map_or_else(
        |_| std::env::temp_dir().join("tilo-apps-shots"),
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
#[ignore = "a review tool: needs a GPU adapter and spawns a real shell"]
fn render_every_boot_tab() {
    let dir = shot_dir();
    std::fs::create_dir_all(&dir).expect("shot dir");
    let mut fb = FrameBuilder::new((W, H));
    let mut r = OffscreenRenderer::new(W, H);
    let mut d = Driver::new();
    fb.relayout();

    let strip_id = {
        let chrome = fb.chrome();
        let tree = chrome.tree();
        tree.desktop
            .active_workspace()
            .and_then(|w| w.strips.first())
            .map(|s| s.id)
            .expect("one strip")
    };

    {
        let g = fb.host_mut().group_at_for_tests(strip_id, 0).unwrap();
        for (i, t) in g.tabs().iter().enumerate() {
            println!("tab {i}: id={} title={:?}", t.id, t.title);
        }
        println!("content kinds: {:?}", g.tile_kind_labels_for_tests());
    }

    let tab_count = fb
        .host_mut()
        .group_at_for_tests(strip_id, 0)
        .map_or(0, |g| g.tabs().len());

    for idx in 0..tab_count {
        let (pill, title) = {
            let g = fb.host_mut().group_at_for_tests(strip_id, 0).unwrap();
            let pill = g.tab_rects()[idx];
            let title = g.tabs()[idx].title.clone();
            // Click the pill's left/label area, clear of the per-tab chips.
            (Point::new(pill.x0 + 10.0, pill.center().y), title)
        };
        d.cursor(&mut fb, pill);
        d.click(&mut fb);
        // Pump a few frames so async app content (PTY output) lands, mirroring
        // the production loop: pump_apps (sync + update) then relayout/paint.
        for _ in 0..40 {
            fb.pump_apps(&mut d.doc, &ROOT);
            fb.relayout();
            let _ = fb.paint();
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        fb.pump_apps(&mut d.doc, &ROOT);
        fb.relayout();
        let scene = fb.paint();
        let name = format!("{idx}-{}.png", title.to_lowercase());
        r.render_png(&scene, Color::BLACK, dir.join(&name));
        println!("wrote {name}");
    }
    println!("shots in {}", dir.display());
}
