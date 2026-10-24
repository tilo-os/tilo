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
    fn settle(&mut self, what: &str) {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            self.frame();
            let Some(busy) = self.busy() else {
                return;
            };
            assert!(
                Instant::now() < deadline,
                "{what}: the desktop never went idle; {busy}"
            );
            std::thread::sleep(Duration::from_millis(16));
        }
    }

    /// Returns whether the first frame after opening started an animation.
    /// The demand is read at an instant taken before that frame, so a frame
    /// slower than the animation itself still shows that it began one.
    fn open_and_close(
        &mut self,
        what: &str,
        open: impl FnOnce(&mut Self),
        close: impl FnOnce(&mut Self),
    ) -> bool {
        open(self);
        let before = Instant::now();
        self.frame();
        let animated = !self.fb.animation_owners_at(before).is_empty();
        self.settle(&format!("{what} open"));
        close(self);
        self.settle(&format!("after closing {what}"));
        animated
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
    assert!(opening, "Quick Settings' opening animation went unobserved");

    // Phase `search_idles_open_and_closed`.
    let opening = desk.open_and_close(
        "Search",
        |d| {
            d.fb.host_mut().open_global_search(&d.doc, &ROOT);
            d.fb.host_mut().set_palette_query("calc");
        },
        |d| d.fb.host_mut().set_palette_visible(false),
    );
    assert!(opening, "Search's opening animation went unobserved");

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
        + shell::shell::osd::osd_fade()
        + Duration::from_millis(50);
    while Instant::now() < fade_done {
        desk.frame();
        std::thread::sleep(Duration::from_millis(16));
    }
    desk.settle("after the brightness readout");
}

/// Three windows side by side, each narrower than the screen, so widening
/// them overflows the strip.
fn three_windows() -> WorkspaceTree {
    use shell::shell::state::{DesktopTree, Tab, WindowGroup, WindowWidth};
    let mut tree = WorkspaceTree::bootstrap_demo();
    let strip = &mut tree.desktop.workspaces[0].strips[0];
    let original = strip.window_groups.remove(0);
    let mut tabs = original.tabs.into_iter();
    let (notes, files) = (tabs.next().unwrap(), tabs.next().unwrap());
    let rest: Vec<Tab> = tabs.collect();
    let group = |id: u64, tabs: Vec<Tab>| WindowGroup {
        id,
        active_tab: tabs[0].id,
        tabs,
        width: WindowWidth::Fraction(300),
        maximized: false,
        focus_history: Vec::new(),
    };
    strip.window_groups = vec![
        group(920_001, vec![notes]),
        group(920_002, vec![files]),
        group(920_003, rest),
    ];
    let strip_id = strip.id;
    let DesktopTree { active_group, .. } = &mut tree.desktop;
    active_group.insert(strip_id, 920_001);
    tree
}

/// Windows resized on a strip wider than the screen spring to their widths,
/// and the springs end. Each width spring steps once per painted frame; a
/// frame the frame builder painted without the host also saying the frame
/// turned left every spring stopped after its first step, asking for frames
/// forever while nothing moved.
#[test]
fn windows_resized_on_an_overflowing_strip_reach_their_widths_and_idle() {
    let mut desk = Desk::new(three_windows());
    desk.settle("three windows");
    let strip = desk
        .fb
        .chrome()
        .active_strip_id()
        .expect("the desktop shows a strip");
    let row = desk
        .fb
        .host_mut()
        .group_row_mut_for_tests(strip)
        .expect("the strip has a row");
    assert_eq!(row.groups.len(), 3, "three windows on the strip");
    let narrow: Vec<f64> = row
        .groups
        .iter()
        .map(ui::widget::WindowGroup::layout_width_px)
        .collect();
    for group in &mut row.groups {
        group.set_width(ui::widget::WindowWidth::Fraction(700));
    }
    let before = Instant::now();
    desk.frame();
    assert!(
        !desk.fb.animation_owners_at(before).is_empty(),
        "the windows spring to their new widths"
    );
    desk.settle("after widening the windows");
    let row = desk
        .fb
        .host_mut()
        .group_row_mut_for_tests(strip)
        .expect("the strip has a row");
    for (group, narrow) in row.groups.iter().zip(narrow) {
        assert!(
            group.layout_width_px() > 2.0 * narrow,
            "a window widened from 30 % to 70 % arrives there: {narrow} then {}",
            group.layout_width_px()
        );
    }
    drop(desk);
}

/// A window made fullscreen by its key is whole at once, and the windows
/// it covers go with it: nothing is left fading under the fullscreen window,
/// where no frame of the fade could be seen, so the desktop asks for no
/// frame after the one that shows it.
#[test]
fn a_fullscreen_by_key_lands_and_asks_for_no_frames_after() {
    use winit::keyboard::{Key, ModifiersState};
    let mut desk = Desk::new(three_windows());
    desk.settle("three windows");
    let press = ui::synthetic_input::key_press(Key::Character("f".into()), Some("f"));
    let super_held = winit::event::Modifiers::from(ModifiersState::SUPER);
    let mut focus = None;
    assert!(
        desk.fb.dispatch_event(
            &press,
            &super_held,
            &mut ui::widget::Edits::lend(&mut desk.doc),
            &ROOT,
            &mut focus,
        ),
        "Super+F is the desktop's"
    );
    assert!(
        desk.fb.chrome().fullscreen_group().is_some(),
        "the focused window is fullscreen"
    );
    desk.frame();
    assert_eq!(desk.busy(), None, "landed: nothing is left moving under it");
    drop(desk);
}
