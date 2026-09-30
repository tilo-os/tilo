//! A desktop nobody is touching must go quiet.
//!
//! The compositor paints only while something asks it to: an animation in
//! flight (`FrameBuilder::needs_animation_frame`) or a one-shot deadline
//! (`FrameBuilder::next_wake_deadline`). A widget that keeps asking after its
//! motion has finished — a spinner left running, a transition that never
//! reports settled, a poll re-armed every frame — keeps a laptop's GPU and
//! CPU busy for as long as the session runs, invisibly, because the screen
//! looks exactly as it should. That class has shipped repeatedly (the lock
//! backdrop, Quick Settings), each found by hand.
//!
//! Each case opens one surface through the production entry point, lets it
//! finish animating, and requires the frame builder to report nothing to do:
//! no animation demand, and no wake sooner than [`WAKE_FLOOR`]. Then it closes
//! the surface and requires the same again. A failure names what is still
//! asking — the chrome source, or the path of the deepest widget.

use std::time::{Duration, Instant};

use automerge::{AutoCommit, ROOT};
use shell::compositor::frame::FrameBuilder;
use shell::shell::chrome_state::{ChromeState, WsTab};
use shell::shell::osd::Osd;
use shell::shell::state::WorkspaceTree;

/// Nearest wake an idle desktop may schedule. A clock that changes once a
/// minute, a toast's expiry: fine. Anything that comes back several times
/// a second is a loop that never idles.
const WAKE_FLOOR: Duration = Duration::from_secs(1);

struct Desk {
    fb: FrameBuilder,
    doc: AutoCommit,
}

impl Desk {
    fn new(tree: WorkspaceTree) -> Self {
        let mut desk = Self {
            fb: FrameBuilder::with_tree(tree, (1920, 1080)),
            doc: AutoCommit::new(),
        };
        desk.frame();
        desk
    }

    /// A frame the way the compositor runs one: pump, lay out, paint.
    fn frame(&mut self) {
        self.fb.pump_apps(&mut self.doc, &ROOT);
        self.fb.relayout();
        let _ = self.fb.paint();
    }

    /// What would still wake an idle loop now, or `None` once quiet.
    fn busy(&self) -> Option<String> {
        let animating = self.fb.animation_owners();
        if self.fb.needs_animation_frame() || !animating.is_empty() {
            return Some(format!("animation demand from {animating:?}"));
        }
        let soon = Instant::now().checked_add(WAKE_FLOOR)?;
        match self.fb.next_wake_deadline() {
            Some(at) if at < soon => Some(format!(
                "a wake in {:?} held by {:?}",
                at.saturating_duration_since(Instant::now()),
                self.fb.wake_deadline_owners()
            )),
            _ => None,
        }
    }

    /// Paint frames, as the loop would, until nothing asks for another.
    /// Returns how many frames asked for a successor first.
    fn settle(&mut self, what: &str) -> usize {
        let deadline = tilo_test_env::hang_deadline();
        let mut busy_frames = 0;
        loop {
            self.frame();
            let Some(busy) = self.busy() else {
                return busy_frames;
            };
            busy_frames = busy_frames.saturating_add(1);
            assert!(
                Instant::now() < deadline,
                "{what}: the desktop never went idle; {busy}"
            );
            std::thread::sleep(Duration::from_millis(16));
        }
    }

    /// Returns the frames the surface animated for while opening.
    fn open_and_close(
        &mut self,
        what: &str,
        open: impl FnOnce(&mut Self),
        close: impl FnOnce(&mut Self),
    ) -> usize {
        open(self);
        let opening = self.settle(&format!("{what} open"));
        close(self);
        self.settle(&format!("after closing {what}"));
        opening
    }
}

#[test]
fn every_chrome_surface_lets_the_desktop_idle() {
    // Phase `an_empty_desktop_idles`.
    Desk::new(WorkspaceTree::bootstrap_default()).settle("empty desktop");

    // Phase `a_populated_desktop_idles`.
    let mut desk = Desk::new(WorkspaceTree::bootstrap_demo());
    desk.settle("populated desktop");

    // Phase `quick_settings_idles_open_and_closed`. It animates open, so
    // this also proves the gate sees animation demand when there is some.
    let opening = desk.open_and_close(
        "Quick Settings",
        |d| d.fb.with_chrome(ChromeState::open_quick_settings),
        |d| d.fb.with_chrome(|c| c.overlays.quick_settings = false),
    );
    assert!(
        opening > 0,
        "Quick Settings' opening animation went unobserved"
    );

    // Phase `search_idles_open_and_closed`.
    let opening = desk.open_and_close(
        "Search",
        |d| {
            d.fb.host_mut().open_global_search(&d.doc, &ROOT);
            d.fb.host_mut().set_palette_query("calc");
        },
        |d| d.fb.host_mut().set_palette_visible(false),
    );
    assert!(opening > 0, "Search's opening animation went unobserved");

    // Phase `overview_idles_open_and_closed`.
    desk.open_and_close(
        "Overview",
        |d| d.fb.with_chrome(|c| c.overlays.overview = true),
        |d| d.fb.with_chrome(|c| c.overlays.overview = false),
    );

    // Phase `workspace_settings_idles_open_and_closed`.
    desk.open_and_close(
        "workspace settings",
        |d| {
            d.fb.with_chrome(|c| c.open_workspace_settings(WsTab::default()));
        },
        |d| d.fb.with_chrome(|c| c.overlays.ws_settings = false),
    );

    // Phase `shortcut_guide_idles_open_and_closed`.
    desk.open_and_close(
        "the keyboard guide",
        |d| d.fb.with_chrome(|c| c.overlays.shortcut_help = true),
        |d| d.fb.with_chrome(|c| c.overlays.shortcut_help = false),
    );

    // Phase `an_osd_fades_and_stops`. It holds without animating, fades,
    // then must leave nothing behind.
    desk.fb.with_chrome(|c| c.show_osd(Osd::brightness(0.4)));
    let fade_done = Instant::now()
        + shell::shell::osd::OSD_HOLD
        + shell::shell::osd::OSD_FADE
        + Duration::from_millis(50);
    while Instant::now() < fade_done {
        desk.frame();
        std::thread::sleep(Duration::from_millis(16));
    }
    desk.settle("after the brightness readout");
}
