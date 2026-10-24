//! The stack of chrome above a window on the laptop panel (2944×1840 at
//! scale 2): the top bar, then the window's tab row, then the window. One
//! small step sits between the bar and the tab row, so the two read as two
//! bars without leaving an empty band. A maximized window hides the bar on
//! its display and fills it, also when the maximize reaches a live desktop
//! without a rebuild. While a hidden top bar shows over the windows, they
//! move down below it, so a window's bar is never under the desktop's.

use automerge::{AutoCommit, ROOT};
use shell::compositor::backend::headless::parse_output_spec;
use shell::compositor::backend::{Output, combined_geometry, output_geometry};
use shell::compositor::frame::FrameBuilder;
use shell::shell::state::{
    AppKind, GroupId, RoomId, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth,
    Workspace, WorkspaceId, WorkspaceTree, compose_id,
};
use shell::shell::topbar_chrome::TOPBAR_HEIGHT_PX;
use ui::widget::WindowGroupBounds;
use ui::widget::tabs::{TAB_BAR_HEIGHT, TAB_PILL_INSET_Y};
use vello::kurbo::Rect;

const SALT: u16 = 1;
const STRIP: StripId = StripId(compose_id(SALT, 77));
const GROUP: GroupId = compose_id(SALT, 1001);
const ROOM: &str = "00000000-0000-4000-8000-0000000000b5";
/// The panel's physical pixels per logical one.
const SCALE: f64 = 2.0;
/// What the windows keep over the bottom edge.
const BOTTOM_MARGIN: f64 = 12.0;
/// The step between the top bar and the windows' tab rows.
const TOP_MARGIN: f64 = 4.0;

fn tree(width: WindowWidth) -> WorkspaceTree {
    let ws_id = WorkspaceId(7);
    let mut ws = Workspace::in_room(ws_id, RoomId::parse(ROOM).expect("room id"), "Desk");
    let tab = GROUP.saturating_add(5000);
    ws.strips = vec![Strip {
        id: STRIP,
        name: "Main".into(),
        window_groups: vec![WindowGroup {
            id: GROUP,
            tabs: vec![Tab {
                id: tab,
                title: "Editor".into(),
                tile: TileContent::Stub {
                    kind: AppKind::Other("Test".into()),
                    label: "Editor".into(),
                },
                app_title: None,
            }],
            active_tab: tab,
            width,
            maximized: false,
            focus_history: Vec::new(),
        }],
    }];
    let mut tree = WorkspaceTree::default();
    tree.set_salt_override(SALT);
    tree.desktop.workspaces = vec![ws];
    tree.desktop.active = ws_id;
    tree.desktop.active_strip.insert(ws_id, STRIP);
    tree.desktop.active_group.insert(STRIP, GROUP);
    tree
}

struct Panel {
    fb: FrameBuilder,
    output: Output,
}

impl Panel {
    fn new() -> Self {
        Self::with_width(WindowWidth::Normal)
    }

    /// The panel with its one window `width` wide.
    fn with_width(width: WindowWidth) -> Self {
        let outputs = parse_output_spec("2944x1840@2").expect("the panel spec parses");
        let mut fb = FrameBuilder::with_tree(tree(width), combined_geometry(&outputs));
        fb.set_output_geometry(output_geometry(&outputs));
        let mut panel = Self {
            fb,
            output: outputs.into_iter().next().expect("one output"),
        };
        panel.settle();
        panel
    }

    /// Frames the way the compositor runs them: pump, lay out, paint.
    fn settle(&mut self) {
        for _ in 0..4 {
            let mut doc = AutoCommit::new();
            self.fb.pump_apps(&mut doc, &ROOT);
            self.fb.relayout();
            let _ = self.fb.paint();
        }
    }

    fn top_bar(&self) -> Rect {
        let bars = self.fb.host().top_bar_rects();
        bars.iter().find(|(_, _, full)| *full).map_or_else(
            || panic!("the panel has a full top bar: {bars:?}"),
            |(_, rect, _)| *rect,
        )
    }

    /// Frames until the windows stop moving: a slide under a bar that shows
    /// or hides runs on the wall clock.
    fn settle_motion(&mut self) {
        let deadline = tilo_test_env::hang_deadline();
        let mut last = None;
        loop {
            self.settle();
            let now = Some(self.group().0.group);
            if now == last {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the windows never stopped moving"
            );
            last = now;
            std::thread::sleep(std::time::Duration::from_millis(60));
        }
    }

    /// What the tab row's maximize control does: the group flips, and the
    /// following frames write it back to the tree.
    fn toggle_maximized(&mut self) {
        self.fb
            .host_mut()
            .group_row_mut_for_tests(STRIP)
            .expect("the strip has a row")
            .groups
            .iter_mut()
            .find(|g| g.id == GROUP)
            .expect("the group is laid out")
            .toggle_maximized();
    }

    fn group(&mut self) -> (WindowGroupBounds, Vec<Rect>) {
        let row = self
            .fb
            .host_mut()
            .group_row_mut_for_tests(STRIP)
            .expect("the strip has a row");
        let group = row
            .groups
            .iter()
            .find(|g| g.id == GROUP)
            .expect("the group is laid out");
        (group.presentation_bounds(), group.tab_rects().to_vec())
    }
}

#[test]
fn the_tab_row_sits_one_small_step_under_the_top_bar_on_the_scale_2_panel() {
    let mut panel = Panel::new();
    let screen = panel.output.logical_rect();
    assert_eq!(
        (screen.width(), screen.height()),
        (1472.0, 920.0),
        "2944×1840 at scale 2"
    );
    let bar = panel.top_bar();
    let (bounds, pills) = panel.group();
    drop(panel);
    assert!(
        (bar.y0 - screen.y0).abs() < 0.5
            && (bar.height() - f64::from(TOPBAR_HEIGHT_PX)).abs() < 0.5,
        "the bar runs along the top edge: {bar:?}"
    );
    assert!(
        (bounds.tab_bar.y0 - bar.y1 - TOP_MARGIN).abs() < 0.5,
        "the tab row starts one small step under the bar: bar {bar:?}, tab row {:?}",
        bounds.tab_bar
    );
    let pill = pills.first().copied().expect("the tab has a pill");
    assert!(
        (pill.y0 - bar.y1 - TOP_MARGIN - TAB_PILL_INSET_Y).abs() < 0.5,
        "only the step and the tab row's own inset lie between the bar and its pills: {pill:?}"
    );
    let client = bounds.client.expect("the window's surface is laid out");
    let chrome = (client.y0 - screen.y0) * SCALE;
    let expected = (f64::from(TOPBAR_HEIGHT_PX) + TOP_MARGIN + TAB_BAR_HEIGHT) * SCALE;
    assert!(
        (chrome - expected).abs() < 1.0,
        "{chrome} physical px of chrome above the window, the bar, the step and the tab row are \
         {expected}"
    );
    assert!(
        (screen.y1 - client.y1 - BOTTOM_MARGIN).abs() < 0.5,
        "the window keeps its margin over the bottom edge: {client:?}"
    );
}

/// A maximize on the live desktop, without a rebuild: the top bar hides on
/// the display the window fills, and the window takes the whole display,
/// no margins; restored, the bar docks again and the margins come back.
#[test]
fn a_live_maximize_hides_the_bar_and_fills_the_display() {
    use tilo_desktop::chrome::TopBarPresence;

    let mut panel = Panel::new();
    let screen = panel.output.logical_rect();
    panel.toggle_maximized();
    panel.settle();
    assert!(
        panel.fb.chrome().tree().desktop.workspaces[0].strips[0].window_groups[0].maximized,
        "the maximize reached the tree"
    );
    assert_eq!(
        panel.fb.chrome().top_bar_presence(),
        TopBarPresence::Hidden,
        "the maximized window hides the bar on its display"
    );
    let (bounds, _) = panel.group();
    assert!(
        (bounds.group.y0 - screen.y0).abs() < 0.5 && (screen.y1 - bounds.group.y1).abs() < 0.5,
        "a maximize on the live desktop keeps no margins: window {:?}",
        bounds.group
    );
    panel.toggle_maximized();
    panel.settle();
    assert_eq!(panel.fb.chrome().top_bar_presence(), TopBarPresence::Docked);
    let bar = panel.top_bar();
    let (bounds, _) = panel.group();
    drop(panel);
    assert!(
        (bounds.tab_bar.y0 - bar.y1 - TOP_MARGIN).abs() < 0.5,
        "restoring docks the bar above the window again: bar {bar:?}, tab row {:?}",
        bounds.tab_bar
    );
    assert!(
        (screen.y1 - bounds.group.y1 - BOTTOM_MARGIN).abs() < 0.5,
        "restoring brings the bottom margin back: {:?}",
        bounds.group
    );
}

/// "Hide the top bar until the pointer is near it": the window's tab row
/// takes the bar's strip at the top of the panel. Pushing the pointer to the
/// top edge shows the bar over the windows, in its docked place, and the bar
/// (not the window under it) takes the pointer there; the windows move down
/// below it, so a window's bar is never under the desktop's. Heading back
/// down into the window hides it, and the windows move back up.
#[test]
fn a_hidden_top_bar_gives_the_window_its_strip_and_shows_over_it_when_reached_for() {
    use shell::shell::input_policy::chrome_captures_pointer;
    use tilo_desktop::chrome::TopBarPresence;
    use vello::kurbo::Point;

    let mut panel = Panel::new();
    let (docked, _) = panel.group();
    let bar = panel.top_bar();
    assert!(
        (docked.tab_bar.y0 - bar.y1 - TOP_MARGIN).abs() < 0.5,
        "docked: a small step under the bar: bar {bar:?}, tab row {:?}",
        docked.tab_bar
    );

    panel.fb.host_mut().set_hide_top_bar_for_tests(true);
    panel.settle_motion();
    assert_eq!(panel.fb.chrome().top_bar_presence(), TopBarPresence::Hidden);
    let (hidden, _) = panel.group();
    let screen = panel.output.logical_rect();
    assert!(
        (hidden.tab_bar.y0 - screen.y0 - TOP_MARGIN).abs() < 0.5,
        "the tab row takes the bar's strip: {:?}",
        hidden.tab_bar
    );

    // Resting in the window, nothing shows.
    panel.fb.host_mut().note_pointer(Point::new(700.0, 500.0));
    panel.settle();
    assert_eq!(panel.fb.chrome().top_bar_presence(), TopBarPresence::Hidden);
    assert!(
        !chrome_captures_pointer(&panel.fb),
        "the window has the pointer"
    );

    // A push up against the top edge shows it: at once when the path brakes
    // into the edge, else after the short rest against it that leaves a
    // click there to the window.
    for y in [400.0, 300.0, 200.0, 100.0, 40.0, 0.0] {
        panel.fb.host_mut().note_pointer(Point::new(700.0, y));
    }
    let pushed = panel.fb.chrome().top_bar_presence();
    if pushed != TopBarPresence::Shown {
        let due = panel
            .fb
            .host()
            .top_bar_reveal_deadline()
            .expect("resting against the edge is due to show the bar");
        panel.fb.host_mut().sync_top_bar_presence(due);
    }
    assert_eq!(panel.fb.chrome().top_bar_presence(), TopBarPresence::Shown);
    panel.settle_motion();
    assert_eq!(
        panel.fb.chrome().top_bar_presence(),
        TopBarPresence::Shown,
        "it stays while the pointer is on it"
    );
    assert!(
        chrome_captures_pointer(&panel.fb),
        "the shown bar, not the window under it, takes the pointer"
    );
    let (shown, _) = panel.group();
    assert!(
        (shown.tab_bar.y0 - hidden.tab_bar.y0 - bar.height()).abs() < 0.5,
        "the window moved down below the shown bar: {:?} from {:?}",
        shown.tab_bar,
        hidden.tab_bar
    );
    assert!(
        (shown.tab_bar.height() - hidden.tab_bar.height()).abs() < 0.5,
        "moved, not resized"
    );

    // Heading back down into the window hides it.
    for y in [12.0, 60.0, 160.0, 300.0] {
        panel.fb.host_mut().note_pointer(Point::new(700.0, y));
    }
    panel.settle_motion();
    assert_eq!(panel.fb.chrome().top_bar_presence(), TopBarPresence::Hidden);
    assert!(!chrome_captures_pointer(&panel.fb));
    assert_eq!(panel.group().0.tab_bar, hidden.tab_bar, "and back up");

    panel.fb.host_mut().set_hide_top_bar_for_tests(false);
    panel.settle_motion();
    assert_eq!(panel.fb.chrome().top_bar_presence(), TopBarPresence::Docked);
    let (again, _) = panel.group();
    drop(panel);
    assert!(
        (again.tab_bar.y0 - bar.y1 - TOP_MARGIN).abs() < 0.5,
        "docked again"
    );
}

/// A notification banner hangs over the window in the top-right corner. On
/// the banner, the shell takes the pointer, so a click opens or dismisses
/// it instead of landing in the window under it; beside it, the window
/// keeps the pointer.
#[test]
#[expect(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
fn a_banner_over_a_window_takes_the_pointer() {
    use shell::shell::input_policy::chrome_captures_pointer;
    use vello::kurbo::Point;

    let mut panel = Panel::new();
    let window = panel.group().0.group;
    panel.fb.with_chrome(|chrome| {
        chrome.push_notification_full(tilo_desktop::notifications::Notification::new(
            "scribe asks first: Rename",
            "Rename files based on their contents",
        ));
    });
    panel.settle();
    let banner = panel.fb.host().notification_banners_rect();
    assert!(banner.area() > 0.0, "the banner is drawn: {banner:?}");
    let on_banner = banner.center();
    panel.fb.host_mut().note_pointer(on_banner);
    assert!(
        chrome_captures_pointer(&panel.fb),
        "the banner, not the window under it, takes the pointer"
    );
    panel
        .fb
        .host_mut()
        .note_pointer(Point::new(window.center().x, window.y1 - 20.0));
    assert!(
        !chrome_captures_pointer(&panel.fb),
        "the window has the pointer"
    );
}

/// The bar hidden and a window maximized: the window takes the whole height
/// and keeps its own bar, Restore and close in it at rest. Reaching for the
/// top shows the desktop's bar, and the window moves down below it, so the
/// top stack reads the desktop's bar at the top and the window's bar under
/// it, neither covering the other.
#[test]
fn with_the_bar_hidden_a_maximized_window_keeps_its_own_bar() {
    use tilo_desktop::chrome::TopBarPresence;
    use vello::kurbo::Point;

    let mut panel = Panel::new();
    panel.toggle_maximized();
    panel.settle();
    panel.fb.host_mut().set_hide_top_bar_for_tests(true);
    panel.settle_motion();
    assert_eq!(panel.fb.chrome().top_bar_presence(), TopBarPresence::Hidden);
    let screen = panel.output.logical_rect();
    let (at_rest, _) = panel.group();
    assert!(
        (at_rest.group.y0 - screen.y0).abs() < 0.5,
        "the maximized window takes the whole height: {:?}",
        at_rest.group
    );
    let restore = at_rest.restore.expect("its bar offers Restore at rest");
    let close = at_rest.close.expect("with its close control");
    for control in [restore, close] {
        assert!(
            at_rest.tab_bar.contains(control.center()),
            "{control:?} stands in the window's bar {:?}",
            at_rest.tab_bar
        );
    }

    for y in [400.0, 300.0, 200.0, 100.0, 40.0, 0.0] {
        panel.fb.host_mut().note_pointer(Point::new(700.0, y));
    }
    let pushed = panel.fb.chrome().top_bar_presence();
    if pushed != TopBarPresence::Shown {
        let due = panel
            .fb
            .host()
            .top_bar_reveal_deadline()
            .expect("resting against the edge is due to show the bar");
        panel.fb.host_mut().sync_top_bar_presence(due);
    }
    assert_eq!(panel.fb.chrome().top_bar_presence(), TopBarPresence::Shown);
    panel.settle_motion();

    let bar = panel.top_bar();
    let (stack, _) = panel.group();
    drop(panel);
    let restore = stack.restore.expect("the window's bar shows");
    let close = stack.close.expect("with its close control");
    assert!(
        restore.y0 >= bar.y1 - 0.5 && close.y0 >= bar.y1 - 0.5,
        "the window's bar {restore:?} {close:?} stands below the desktop's {bar:?}"
    );
    assert!(
        (stack.group.y0 - at_rest.group.y0 - bar.height()).abs() < 0.5
            && (stack.group.height() - at_rest.group.height()).abs() < 0.5,
        "the window moved down below the bar {bar:?}, not resized: {:?} from {:?}",
        stack.group,
        at_rest.group
    );
}

/// An incoming call while a window fills the panel's width, docked under
/// the bar and maximized: the call's card stands where it covers none of
/// the window's bar, so History, maximize or restore and close stay visible,
/// and a click on each reaches the window rather than the card.
#[test]
#[expect(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
fn an_incoming_call_never_covers_the_window_bar() {
    use shell::shell::input_policy::chrome_captures_pointer;
    use tilo_desktop::notifications::{Addressed, Notification};

    for maximized in [false, true] {
        let mut panel = Panel::with_width(WindowWidth::Wide);
        if maximized {
            panel.toggle_maximized();
            panel.settle_motion();
        }
        panel.fb.with_chrome(|chrome| {
            let mut call = Notification::new("Jonas is calling", "Video call")
                .with_addressed(Addressed::IncomingCall);
            call.actions = vec![
                ("answer".into(), "Answer".into()),
                ("decline".into(), "Decline".into()),
            ];
            chrome.push_notification_full(call);
        });
        panel.settle();
        let banner = panel.fb.host().notification_banners_rect();
        let (window, _) = panel.group();
        assert!(banner.area() > 0.0, "the call is shown: {banner:?}");
        assert!(
            banner.x0 < window.tab_bar.x1 && banner.x1 > window.tab_bar.x0,
            "maximized {maximized}: the card {banner:?} stands over the window's column \
             {:?}, so the check means something",
            window.tab_bar
        );
        assert!(
            banner.intersect(window.tab_bar).area() <= 0.0,
            "maximized {maximized}: the card {banner:?} covers the window's bar {:?}",
            window.tab_bar
        );
        let toggle = if maximized {
            window
                .restore
                .expect("a maximized window's bar offers Restore")
        } else {
            window.maximize.expect("the window's bar offers maximize")
        };
        let close = window.close.expect("the window's bar offers close");
        for control in [toggle, close] {
            assert!(
                banner.intersect(control).area() <= 0.0,
                "maximized {maximized}: the card {banner:?} covers {control:?}"
            );
            panel.fb.host_mut().note_pointer(control.center());
            assert!(
                !chrome_captures_pointer(&panel.fb),
                "maximized {maximized}: a click on {control:?} reaches the window"
            );
        }
    }
}

/// A raw touch contact, as the compositor hands the chrome one.
fn touch(id: u64, phase: winit::event::TouchPhase, x: f64, y: f64) -> winit::event::WindowEvent {
    winit::event::WindowEvent::Touch(winit::event::Touch {
        device_id: winit::event::DeviceId::dummy(),
        phase,
        location: winit::dpi::PhysicalPosition::new(x, y),
        force: None,
        id,
    })
}

impl Panel {
    fn dispatch(&mut self, event: &winit::event::WindowEvent) -> bool {
        let mut doc = AutoCommit::new();
        let mut focus = None;
        self.fb.dispatch_event(
            event,
            &winit::event::Modifiers::default(),
            &mut ui::widget::Edits::lend(&mut doc),
            &ROOT,
            &mut focus,
        )
    }

    /// A finger coming off the bezel at the top edge above `x` and dragging
    /// down.
    fn swipe_down_from_top(&mut self, x: f64) {
        use winit::event::TouchPhase;
        let top = self.output.logical_rect().y0;
        assert!(
            self.fb
                .host()
                .takes_top_edge_touch(vello::kurbo::Point::new(x, top + 2.0)),
            "the top edge is the shell's while something hides there"
        );
        self.dispatch(&touch(9, TouchPhase::Started, x, top + 2.0));
        for y in [12.0, 24.0, 40.0, 60.0] {
            self.dispatch(&touch(9, TouchPhase::Moved, x, top + y));
        }
        self.dispatch(&touch(9, TouchPhase::Ended, x, top + 60.0));
    }
}

#[test]
fn a_swipe_down_from_the_top_edge_reveals_the_bar() {
    use tilo_desktop::chrome::TopBarPresence;

    // The top bar the person hides.
    let mut panel = Panel::new();
    panel.fb.host_mut().set_hide_top_bar_for_tests(true);
    panel.settle_motion();
    assert_eq!(panel.fb.chrome().top_bar_presence(), TopBarPresence::Hidden);
    panel.swipe_down_from_top(700.0);
    assert_eq!(
        panel.fb.chrome().top_bar_presence(),
        TopBarPresence::Shown,
        "a swipe down from the top edge shows the hidden top bar"
    );
    panel.fb.host_mut().set_hide_top_bar_for_tests(false);
    panel.settle_motion();

    // A fullscreen window's bar.
    panel
        .fb
        .with_chrome(|chrome| chrome.set_user_fullscreen_group(Some(GROUP)));
    panel.settle_motion();
    assert!(
        panel.fb.chrome().is_fullscreen(),
        "the window is fullscreen"
    );
    let before = panel.group().0.group;
    let shown = |panel: &mut Panel| {
        panel
            .fb
            .host_mut()
            .group_row_mut_for_tests(STRIP)
            .expect("the strip has a row")
            .groups
            .iter()
            .find(|g| g.id == GROUP)
            .is_some_and(ui::widget::WindowGroup::fullscreen_bar_shown)
    };
    assert!(!shown(&mut panel), "fullscreen, its bar hides");
    panel.swipe_down_from_top(700.0);
    panel.settle();
    assert!(
        shown(&mut panel),
        "a swipe down from the top edge shows the fullscreen window's bar"
    );
    let (after, _) = panel.group();
    assert_eq!(
        after.group, before,
        "over the window, which keeps its whole size"
    );
    assert!(after.tab_bar.area() > 0.0, "its bar is laid out: {after:?}");
}
