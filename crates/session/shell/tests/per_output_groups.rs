//! Each monitor holds its own windows.
//!
//! With more than one display, every window group belongs to exactly one of
//! them: a strip is one sub-row per display, each laid out, panned and
//! clipped on its own display, so no window straddles a seam and panning
//! one display never reveals another's windows. New windows open on the
//! display with focus, a chord moves a group to the neighbouring display,
//! and unplugging a display hands its groups to the nearest remaining one
//! until it comes back. Groups belong to the physical monitor, so outputs
//! renumbered by a restart or a port swap bring each back to its own. Every
//! display stacks all of the workspace's strips, and a group dragged onto
//! another display moves there. The primary display carries the wheel disc
//! and the full top bar; every other display a compact bar along its own
//! top edge.
//!
//! Each case lays the production `FrameBuilder` out across a named headless
//! fixture (`TILO_HEADLESS_OUTPUTS`) and drives it through the same entry
//! points the compositor uses: backend events via `dispatch_event`, frame
//! pumps, and the tree the control socket's `state-dump` serves.
#![allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};
use shell::compositor::backend::headless::{output_fixture, parse_output_spec};
use shell::compositor::backend::{
    Output, OutputId, OutputIdentity, combined_geometry, output_geometry,
};
use shell::compositor::frame::FrameBuilder;
use shell::shell::state::{
    AppKind, GroupId, PeerOrigin, RoomId, Strip, StripId, Tab, TileContent, WindowGroup,
    WindowWidth, Workspace, WorkspaceId, WorkspaceTree, compose_id,
};
use ui::widget::Widget as _;
use vello::kurbo::{Point, Rect};
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, Modifiers, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

/// This device's peer salt: room projection publishes only ids it minted.
const SALT: u16 = 1;
const fn id(counter: u64) -> u64 {
    compose_id(SALT, counter)
}
const STRIP: StripId = StripId(id(77));
const ROOM: &str = "00000000-0000-4000-8000-0000000000a1";
/// Three windows start on the primary display, three on the other.
const ON_PRIMARY: [GroupId; 3] = [id(1001), id(1002), id(1003)];
const ON_SECOND: [GroupId; 3] = [id(1004), id(1005), id(1006)];
/// A second strip, two windows on each display.
const STRIP_B: StripId = StripId(id(78));
const B_ON_PRIMARY: [GroupId; 2] = [id(2001), id(2002)];
const B_ON_SECOND: [GroupId; 2] = [id(2003), id(2004)];

/// The fixture's first output is this laptop panel, its second this
/// monitor, as their EDIDs name them; the partition keys groups by these.
const PANEL: (&str, &str, &str, &str) = ("eDP-1", "Tilo", "Test Panel", "P-0001");
const MONITOR: (&str, &str, &str, &str) = ("DP-1", "Tilo", "Test Monitor", "M-0001");
const PANEL_KEY: &str = "edid:Tilo|Test Panel|P-0001";
const MONITOR_KEY: &str = "edid:Tilo|Test Monitor|M-0001";

fn identity((connector, make, model, serial): (&str, &str, &str, &str)) -> OutputIdentity {
    OutputIdentity {
        connector: connector.into(),
        make: make.into(),
        model: model.into(),
        serial: serial.into(),
        physical_mm: (0, 0),
    }
}

/// The panel and monitor plugged into an arrangement's first two outputs.
fn with_monitors(mut outputs: Vec<Output>) -> Vec<Output> {
    for (output, monitor) in outputs.iter_mut().zip([PANEL, MONITOR]) {
        output.identity = identity(monitor);
    }
    outputs
}

fn tab_of(group: GroupId) -> u64 {
    group.saturating_add(5000)
}

fn group(id: GroupId) -> WindowGroup {
    let tab = tab_of(id);
    WindowGroup {
        id,
        tabs: vec![Tab {
            id: tab,
            title: format!("W{id}"),
            tile: TileContent::Stub {
                kind: AppKind::Other("Test".into()),
                label: format!("W{id}"),
            },
        }],
        active_tab: tab,
        // Wide enough that three overflow every fixture display, so there
        // is something to pan to.
        width: WindowWidth::Fraction(600),
        maximized: false,
        focus_history: Vec::new(),
    }
}

/// One strip of six windows whose partition was restored from this device's
/// state: the first three on the primary display, the rest on the second.
fn tree() -> WorkspaceTree {
    let ws_id = WorkspaceId(7);
    let mut ws = Workspace::new(ws_id, "Desk");
    ws.set_room(RoomId::parse(ROOM).expect("room id"));
    ws.strips = vec![Strip {
        id: STRIP,
        name: "Main".into(),
        window_groups: ON_PRIMARY
            .iter()
            .chain(ON_SECOND.iter())
            .map(|id| group(*id))
            .collect(),
    }];
    let mut tree = WorkspaceTree::default();
    tree.set_salt_override(SALT);
    tree.desktop.workspaces = vec![ws];
    tree.desktop.active = ws_id;
    tree.desktop.active_strip.insert(ws_id, STRIP);
    tree.desktop.active_group.insert(STRIP, ON_PRIMARY[0]);
    for id in ON_PRIMARY {
        tree.desktop.outputs.home.insert(id, PANEL_KEY.into());
    }
    for id in ON_SECOND {
        tree.desktop.outputs.home.insert(id, MONITOR_KEY.into());
    }
    tree
}

/// [`tree`] with a second strip below the first.
fn two_strip_tree() -> WorkspaceTree {
    let mut tree = tree();
    tree.desktop.workspaces[0].strips.push(Strip {
        id: STRIP_B,
        name: "Second".into(),
        window_groups: B_ON_PRIMARY
            .iter()
            .chain(B_ON_SECOND.iter())
            .map(|id| group(*id))
            .collect(),
    });
    for id in B_ON_PRIMARY {
        tree.desktop.outputs.home.insert(id, PANEL_KEY.into());
    }
    for id in B_ON_SECOND {
        tree.desktop.outputs.home.insert(id, MONITOR_KEY.into());
    }
    tree
}

struct Desk {
    fb: FrameBuilder,
    outputs: Vec<Output>,
}

impl Desk {
    fn across(outputs: Vec<Output>) -> Self {
        Self::with_tree(tree(), with_monitors(outputs))
    }

    fn with_tree(tree: WorkspaceTree, outputs: Vec<Output>) -> Self {
        let mut fb = FrameBuilder::with_tree(tree, combined_geometry(&outputs));
        fb.set_output_geometry(output_geometry(&outputs));
        fb.host_mut()
            .set_shortcut_config(settings_store::hardware::ShortcutConfig::default())
            .expect("the built-in shortcut table is valid");
        let mut desk = Self { fb, outputs };
        desk.settle();
        desk
    }

    fn fixture(name: &str) -> Self {
        Self::across(output_fixture(name).unwrap_or_else(|| panic!("fixture {name}")))
    }

    /// A frame the way the compositor runs one: pump, lay out, paint.
    fn frame(&mut self) {
        let mut doc = AutoCommit::new();
        self.fb.pump_apps(&mut doc, &ROOT);
        self.fb.relayout();
        let _ = self.fb.paint();
    }

    fn settle(&mut self) {
        for _ in 0..4 {
            self.frame();
        }
    }

    /// Frames until `done` holds, letting eased pans run out.
    fn settle_until(&mut self, what: &str, mut done: impl FnMut(&mut Self) -> bool) {
        let deadline = tilo_test_env::hang_deadline();
        while Instant::now() < deadline {
            self.frame();
            if done(self) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("never settled: {what}");
    }

    fn display(&self, id: u32) -> Rect {
        self.outputs
            .iter()
            .find(|o| o.id.0 == id)
            .map(Output::logical_rect)
            .expect("display")
    }

    /// Where each group is laid out, and on which display.
    fn layout(&mut self) -> Vec<(GroupId, Rect, Option<u32>)> {
        self.layout_of(STRIP)
    }

    /// Where each group of `strip` is laid out, and on which display.
    fn layout_of(&mut self, strip: StripId) -> Vec<(GroupId, Rect, Option<u32>)> {
        let row = self
            .fb
            .host_mut()
            .group_row_mut_for_tests(strip)
            .expect("the strip has a row");
        row.groups
            .iter()
            .map(|g| (g.id, g.get_rect(), row.output_of_group(g.id)))
            .collect()
    }

    fn rect_of(&mut self, id: GroupId) -> Rect {
        self.layout()
            .into_iter()
            .find(|(g, _, _)| *g == id)
            .map(|(_, r, _)| r)
            .expect("group laid out")
    }

    fn pan_of(&mut self, display: u32) -> f64 {
        self.fb
            .host_mut()
            .group_row_mut_for_tests(STRIP)
            .and_then(|row| row.output_pan(display).map(ui::widget::Pan::scroll_x))
            .expect("the display has a lane")
    }

    /// The `workspace_tree` half of `state-dump`, built the way the
    /// compositor builds it.
    fn state_dump(&self) -> serde_json::Value {
        serde_json::to_value(&*self.fb.chrome().tree()).expect("tree serialises")
    }

    fn display_of(&self, id: GroupId) -> Option<u32> {
        self.fb.chrome().tree().desktop.outputs.display_of(id)
    }

    fn focused_display(&self) -> Option<u32> {
        self.fb.chrome().tree().desktop.outputs.focused_display()
    }

    fn strip_order(&self) -> Vec<GroupId> {
        self.fb.chrome().tree().desktop.workspaces[0].strips[0]
            .window_groups
            .iter()
            .map(|g| g.id)
            .collect()
    }

    fn focus(&mut self, id: GroupId) {
        let _ = self
            .fb
            .host_mut()
            .focus_window_on_strip(STRIP, id, tab_of(id));
        self.settle();
        assert_eq!(
            self.fb.chrome().tree().desktop.active_group_for(STRIP),
            Some(id),
            "focus {id}"
        );
    }

    fn dispatch(&mut self, event: &WindowEvent, mods: ModifiersState) {
        let mut doc = AutoCommit::new();
        let mut focus = None;
        self.fb.dispatch_event(
            event,
            &Modifiers::from(mods),
            &mut ui::widget::Edits::lend(&mut doc),
            &ROOT,
            &mut focus,
        );
    }

    /// The pointer moving to a logical point, as the compositor hands it on.
    fn point_at(&mut self, p: Point) {
        self.dispatch(
            &ui::synthetic_input::cursor_moved(p.x, p.y),
            ModifiersState::empty(),
        );
        self.settle();
    }

    fn press(&mut self, key: NamedKey, mods: ModifiersState) {
        let ev = ui::synthetic_input::key_event(Key::Named(key), None, ElementState::Pressed);
        self.dispatch(&ui::synthetic_input::keyboard_input(ev), mods);
        self.settle();
    }

    fn unplug_all_but_primary(&mut self) {
        let remaining: Vec<Output> = self.outputs.iter().take(1).cloned().collect();
        self.fb.set_output_geometry(output_geometry(&remaining));
        self.settle();
    }

    fn replug(&mut self) {
        self.fb.set_output_geometry(output_geometry(&self.outputs));
        self.settle();
    }

    /// The display the monitor named `key` is connected as.
    fn output_of(&self, key: &str) -> u32 {
        let (_, make, model, serial) = if key == PANEL_KEY { PANEL } else { MONITOR };
        self.outputs
            .iter()
            .find(|o| {
                o.identity.make == make && o.identity.model == model && o.identity.serial == serial
            })
            .map(|o| o.id.0)
            .expect("the monitor is connected")
    }

    /// A point on `group`'s bar that lifts the whole group: past its last
    /// tab, clear of the new-tab button and the resize grip.
    fn grab_point(&mut self, strip: StripId, group: GroupId) -> Point {
        let row = self
            .fb
            .host_mut()
            .group_row_mut_for_tests(strip)
            .expect("the strip has a row");
        let g = row
            .groups
            .iter()
            .find(|g| g.id == group)
            .expect("group laid out");
        let bar = g.bar_rect();
        let last = g.tab_rects().last().copied().unwrap_or(bar);
        let p = Point::new(
            f64::midpoint(last.x1, bar.x1),
            f64::midpoint(bar.y0, bar.y1),
        );
        assert!(g.bar_grab_hit(p), "{p:?} lifts {group}");
        p
    }

    fn button(&mut self, state: ElementState) {
        self.dispatch(
            &ui::synthetic_input::mouse_input(state, winit::event::MouseButton::Left),
            ModifiersState::empty(),
        );
        self.frame();
    }

    fn cursor_to(&mut self, p: Point) {
        self.dispatch(
            &ui::synthetic_input::cursor_moved(p.x, p.y),
            ModifiersState::empty(),
        );
        self.frame();
    }

    fn finger(&mut self, phase: winit::event::TouchPhase, p: Point) {
        self.dispatch(
            &WindowEvent::Touch(winit::event::Touch {
                device_id: winit::event::DeviceId::dummy(),
                phase,
                location: PhysicalPosition::new(p.x, p.y),
                force: None,
                id: 7,
            }),
            ModifiersState::empty(),
        );
        self.frame();
    }

    /// Drag from `from` to `to` in steps, with the pointer or one finger,
    /// and let go.
    /// A finger grabs the group with a short downward pull first: a swipe
    /// that starts sideways on the tab bar pans the strip instead.
    fn drag(&mut self, from: Point, to: Point, touch: bool) {
        use winit::event::TouchPhase;
        if touch {
            self.finger(TouchPhase::Started, from);
            self.finger(TouchPhase::Moved, Point::new(from.x, from.y + 12.0));
        } else {
            self.cursor_to(from);
            self.button(ElementState::Pressed);
        }
        for step in 1..=8 {
            let t = f64::from(step) / 8.0;
            let p = Point::new(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t);
            if touch {
                self.finger(TouchPhase::Moved, p);
            } else {
                self.cursor_to(p);
            }
        }
        if touch {
            self.finger(TouchPhase::Ended, to);
        } else {
            self.button(ElementState::Released);
        }
        self.settle();
    }
}

/// What each group draws of itself: its rect cut to its display's lane.
fn assert_every_group_stays_on_its_display(desk: &mut Desk, fixture: &str) {
    let layout = desk.layout();
    assert_eq!(layout.len(), 6, "{fixture}: every window is laid out");
    for (id, rect, display) in layout {
        let display = display.unwrap_or_else(|| panic!("{fixture}: {id} has no display"));
        let own = desk.display(display);
        assert!(
            rect.y0 >= own.y0 - 0.5 && rect.y1 <= own.y1 + 0.5,
            "{fixture}: {id} runs past display {display}'s top or bottom edge: {rect:?} on {own:?}"
        );
        // Horizontally a window may hang off its display's end — that part
        // is panned out of view, clipped to its own display — but a window
        // starting inside its display never reaches onto another one.
        for other in desk.outputs.iter().filter(|o| o.id.0 != display) {
            let theirs = other.logical_rect();
            let drawn = rect.intersect(own);
            let spill = drawn.intersect(theirs);
            assert!(
                spill.width() <= 0.0 || spill.height() <= 0.0,
                "{fixture}: {id} is drawn onto display {}: {drawn:?}",
                other.id.0
            );
        }
    }
}

#[test]
fn no_window_straddles_a_seam() {
    for fixture in ["docked-laptop", "vertical-stack"] {
        let mut desk = Desk::fixture(fixture);
        assert_every_group_stays_on_its_display(&mut desk, fixture);
        // Unpanned, the second display's first window starts at that
        // display's own left edge, not wherever the first display's row
        // happened to end.
        let first_on_second = desk.rect_of(ON_SECOND[0]);
        assert!(
            (first_on_second.x0 - desk.display(1).x0).abs() < 0.5,
            "{fixture}: {first_on_second:?} should start at {:?}",
            desk.display(1)
        );
    }
}

#[test]
fn each_display_pans_on_its_own() {
    for fixture in ["docked-laptop", "vertical-stack"] {
        let mut desk = Desk::fixture(fixture);
        // Let the opening focus pan run out first.
        let mut last = f64::NAN;
        desk.settle_until("the primary display comes to rest", |d| {
            let now = d.pan_of(0);
            let rested = (now - last).abs() < f64::EPSILON;
            last = now;
            rested
        });
        let primary_before: Vec<Rect> = ON_PRIMARY.iter().map(|&id| desk.rect_of(id)).collect();
        let primary_pan = desk.pan_of(0);
        let last = ON_SECOND[2];
        assert!(
            desk.rect_of(last).x1 > desk.display(1).x1,
            "{fixture}: the last window starts off its display"
        );
        desk.focus(last);
        desk.settle_until("the second display pans to its last window", |d| {
            let r = d.rect_of(last);
            r.x1 <= d.display(1).x1 + 0.5
        });
        assert!(desk.pan_of(1) > 0.0, "{fixture}: the second display panned");
        assert_eq!(
            desk.pan_of(0),
            primary_pan,
            "{fixture}: the primary did not"
        );
        let primary_after: Vec<Rect> = ON_PRIMARY.iter().map(|&id| desk.rect_of(id)).collect();
        assert_eq!(
            primary_before, primary_after,
            "{fixture}: panning the second display moved the primary's windows"
        );
        assert_every_group_stays_on_its_display(&mut desk, fixture);
    }
}

#[test]
fn a_new_window_opens_on_the_display_with_focus() {
    for fixture in ["docked-laptop", "vertical-stack"] {
        let mut desk = Desk::fixture(fixture);
        assert_eq!(desk.focused_display(), Some(0), "{fixture}");
        // The pointer crossing onto the second display takes focus with it.
        let second = desk.display(1);
        desk.point_at(Point::new(second.center().x, second.center().y));
        assert_eq!(desk.focused_display(), Some(1), "{fixture}");
        let active = {
            let chrome = desk.fb.chrome();
            let tree = chrome.tree();
            tree.desktop.active_group_for(STRIP)
        };
        assert!(
            active.is_some_and(|g| ON_SECOND.contains(&g)),
            "{fixture}: focus follows the pointer to one of the second display's windows, got {active:?}"
        );

        // A client maps a toplevel: the production adoption path.
        let tab = {
            let chrome = desk.fb.chrome();
            let mut tree = chrome.tree_mut();
            shell::shell::tab_binding::adopt_toplevel(&mut tree, 4242, Some("org.test.New".into()))
        };
        desk.fb
            .with_chrome(shell::shell::chrome_state::ChromeState::bump);
        desk.settle();
        let new_group = {
            let chrome = desk.fb.chrome();
            let tree = chrome.tree();
            tree.desktop.workspaces[0].strips[0]
                .window_groups
                .iter()
                .find(|g| g.tabs.iter().any(|t| t.id == tab))
                .map(|g| g.id)
                .expect("the new window has a group")
        };
        assert_eq!(desk.display_of(new_group), Some(1), "{fixture}");
        let rect = desk.rect_of(new_group);
        assert!(
            desk.display(1)
                .contains(Point::new(rect.x0 + 1.0, rect.center().y)),
            "{fixture}: the new window is laid out on the second display: {rect:?}"
        );
    }
}

#[test]
fn the_chord_moves_the_focused_group_to_the_neighbouring_display() {
    for fixture in ["docked-laptop", "vertical-stack"] {
        let mut desk = Desk::fixture(fixture);
        let moving = ON_PRIMARY[1];
        desk.focus(ON_SECOND[0]);
        desk.focus(moving);
        let chord = ModifiersState::SUPER | ModifiersState::CONTROL | ModifiersState::SHIFT;
        desk.press(NamedKey::ArrowRight, chord);
        assert_eq!(desk.display_of(moving), Some(1), "{fixture}");
        assert_eq!(
            desk.focused_display(),
            Some(1),
            "{fixture}: focus went along"
        );
        // It takes its place in the strip's order among that display's
        // groups; the shared order itself is untouched.
        let on_second: Vec<GroupId> = desk
            .fb
            .host()
            .display_partition_summary()
            .into_iter()
            .find(|(d, _)| *d == 1)
            .map(|(_, groups)| groups)
            .expect("second display");
        assert_eq!(
            on_second,
            vec![moving, ON_SECOND[0], ON_SECOND[1], ON_SECOND[2]],
            "{fixture}"
        );
        assert_eq!(
            desk.layout()
                .iter()
                .find(|g| g.0 == moving)
                .and_then(|g| g.2),
            Some(1)
        );
        assert_takes_its_new_displays_height(&mut desk, fixture, moving);

        // And back.
        desk.press(NamedKey::ArrowLeft, chord);
        assert_eq!(desk.display_of(moving), Some(0), "{fixture}");
        // The plain reorder chord still reorders, within the display.
        let before = desk.strip_order();
        desk.press(
            NamedKey::ArrowLeft,
            ModifiersState::SUPER | ModifiersState::SHIFT,
        );
        assert_eq!(
            desk.display_of(moving),
            Some(0),
            "{fixture}: reorder stays put"
        );
        assert_ne!(
            desk.strip_order(),
            before,
            "{fixture}: reorder moved it along"
        );
    }
}

fn assert_takes_its_new_displays_height(desk: &mut Desk, fixture: &str, moved: GroupId) {
    let rect = desk.rect_of(moved);
    let own = desk.display(1);
    assert!(
        rect.y0 >= own.y0 - 0.5 && rect.y1 <= own.y1 + 0.5,
        "{fixture}: the moved window takes its new display's height: {rect:?} on {own:?}"
    );
}

#[test]
fn focus_crosses_displays_at_the_end_of_a_sub_row() {
    let mut desk = Desk::fixture("docked-laptop");
    desk.focus(ON_PRIMARY[2]);
    desk.press(NamedKey::ArrowRight, ModifiersState::SUPER);
    let active = desk.fb.chrome().tree().desktop.active_group_for(STRIP);
    assert_eq!(
        active,
        Some(ON_SECOND[0]),
        "past the panel's last window, the monitor's first"
    );
    assert_eq!(desk.focused_display(), Some(1));
    desk.press(NamedKey::ArrowLeft, ModifiersState::SUPER);
    let active = desk.fb.chrome().tree().desktop.active_group_for(STRIP);
    assert_eq!(active, Some(ON_PRIMARY[2]), "and back to the panel's last");
}

#[test]
fn unplugging_hands_groups_on_and_replugging_takes_them_back() {
    for fixture in ["docked-laptop", "vertical-stack"] {
        let mut desk = Desk::fixture(fixture);
        desk.focus(ON_SECOND[1]);
        desk.focus(ON_PRIMARY[1]);
        desk.unplug_all_but_primary();
        for id in ON_PRIMARY.iter().chain(ON_SECOND.iter()) {
            assert_eq!(desk.display_of(*id), Some(0), "{fixture}: {id}");
        }
        assert_eq!(
            desk.strip_order(),
            ON_PRIMARY
                .iter()
                .chain(ON_SECOND.iter())
                .copied()
                .collect::<Vec<_>>(),
            "{fixture}: the strip's own order"
        );
        let dump = desk.state_dump();
        assert_eq!(
            dump["outputs"]["origin"][ON_SECOND[1].to_string()],
            MONITOR_KEY,
            "{fixture}: {dump:#}"
        );
        // One display left: one continuous row again, sized to the panel.
        let panel = desk.display(0);
        for (id, rect, display) in desk.layout() {
            assert_eq!(display, None, "{fixture}: {id} is in a partitioned row");
            assert!(
                rect.width() > 0.0 && rect.y0 >= panel.y0 && rect.y1 <= panel.y1 + 0.5,
                "{fixture}: {id} is laid out on the remaining display: {rect:?}"
            );
        }

        desk.replug();
        for id in ON_SECOND {
            assert_eq!(desk.display_of(id), Some(1), "{fixture}: {id} returned");
        }
        let summary = desk.fb.host().display_partition_summary();
        assert_eq!(
            summary,
            vec![(0, ON_PRIMARY.to_vec()), (1, ON_SECOND.to_vec())],
            "{fixture}"
        );
        assert_every_group_stays_on_its_display(&mut desk, fixture);
    }
}

/// The backend lists the primary chosen in Settings first, whatever its
/// output number; the wheel disc, the full top bar and the partition's
/// primary follow it there.
#[test]
fn the_primary_is_the_one_settings_chose() {
    let mut outputs = with_monitors(output_fixture("docked-laptop").expect("fixture"));
    outputs.reverse();
    assert_eq!(
        outputs[0].id,
        OutputId(1),
        "the monitor is the chosen primary"
    );
    let desk = Desk::with_tree(tree(), outputs);
    let monitor = desk.display(1);
    assert_eq!(
        desk.fb.chrome().tree().desktop.outputs.primary(),
        Some(1),
        "the partition's primary"
    );
    let bars = desk.fb.host().top_bar_rects();
    let full: Vec<u32> = bars.iter().filter(|b| b.2).map(|b| b.0).collect();
    assert_eq!(
        full,
        vec![1],
        "the full bar is on the chosen primary: {bars:?}"
    );
    let compact: Vec<u32> = bars.iter().filter(|b| !b.2).map(|b| b.0).collect();
    assert_eq!(compact, vec![0], "{bars:?}");
    let wheel = desk
        .fb
        .host()
        .sidebar_rect_for_tests()
        .expect("the desktop has a wheel disc");
    assert!(
        monitor.contains(Point::new(
            f64::midpoint(wheel.x0, wheel.x1),
            f64::midpoint(wheel.y0, wheel.y1)
        )),
        "the wheel disc {wheel:?} is on the chosen primary {monitor:?}"
    );
}

#[test]
fn the_wheel_disc_sits_on_the_primary_display() {
    // The primary (listed first) is the right-hand monitor here, so the
    // disc is not simply wherever x = 0 happens to be.
    let outputs = parse_output_spec("1920x1080@1+1280+0,2560x1600@2+0+0").expect("spec");
    let mut desk = Desk::across(outputs);
    let primary = desk.display(0);
    assert!(primary.x0 > 0.0, "the primary is the right-hand display");
    let wheel = desk
        .fb
        .host()
        .sidebar_rect_for_tests()
        .expect("the desktop has a wheel disc");
    assert!(wheel.width() > 0.0 && wheel.height() > 0.0, "{wheel:?}");
    // The disc rides along with the primary's own pan.
    let pan = desk.pan_of(0);
    assert!(
        wheel.x0 >= primary.x0 - pan - 0.5 && wheel.x1 <= primary.x1 + 0.5,
        "the wheel disc {wheel:?} is on the primary {primary:?}"
    );
    assert!(
        wheel.y1 <= primary.y1 + 0.5,
        "{wheel:?} fits the primary's height"
    );
    // The primary's first window clears the disc; the other display's
    // first window starts at its own left edge.
    let first = desk.rect_of(ON_PRIMARY[0]);
    assert!(
        first.x0 >= wheel.x1 - 0.5,
        "{first:?} clears the disc {wheel:?}"
    );
    let other_first = desk.rect_of(ON_SECOND[0]);
    assert!(
        (other_first.x0 - desk.display(1).x0).abs() < 0.5,
        "{other_first:?}"
    );
}

#[test]
fn every_display_has_a_top_bar() {
    for fixture in ["docked-laptop", "vertical-stack"] {
        let desk = Desk::fixture(fixture);
        let bars = desk.fb.host().top_bar_rects();
        let full: Vec<_> = bars.iter().filter(|b| b.2).collect();
        assert_eq!(full.len(), 1, "{fixture}: one full top bar: {bars:?}");
        let (full_on, full_rect, _) = *full[0];
        assert_eq!(full_on, 0, "{fixture}: the full bar is the primary's");
        let primary = desk.display(0);
        assert!(
            (full_rect.y0 - primary.y0).abs() < 0.5
                && (full_rect.x0 - primary.x0).abs() < 0.5
                && (full_rect.x1 - primary.x1).abs() < 0.5,
            "{fixture}: the full bar spans the primary's top edge: {full_rect:?} vs {primary:?}"
        );
        let compact: Vec<_> = bars.iter().filter(|b| !b.2).collect();
        assert_eq!(compact.len(), 1, "{fixture}: {bars:?}");
        let (on, rect, _) = *compact[0];
        assert_eq!(on, 1, "{fixture}");
        let second = desk.display(1);
        assert!(
            (rect.y0 - second.y0).abs() < 0.5
                && (rect.x0 - second.x0).abs() < 0.5
                && (rect.x1 - second.x1).abs() < 0.5
                && rect.height() > 0.0,
            "{fixture}: the compact bar sits along its display's own top edge: {rect:?} vs {second:?}"
        );
        // It is a real, reachable toolbar, not only paint.
        let update = desk.fb.build_a11y_tree_update(None, 1.0);
        let compact_bar = update
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some("Display navigation"))
            .and_then(|(_, node)| node.bounds())
            .unwrap_or_else(|| panic!("{fixture}: no compact bar in the accessibility tree"));
        assert!(
            second.contains(Point::new(
                f64::midpoint(compact_bar.x0, compact_bar.x1),
                f64::midpoint(compact_bar.y0, compact_bar.y1)
            )),
            "{fixture}: {compact_bar:?}"
        );
        // The windows start below each display's own bar.
        let mut desk = desk;
        for (id, r, display) in desk.layout() {
            if display == Some(1) {
                assert!(
                    r.y0 >= rect.y1 - 0.5,
                    "{fixture}: {id} {r:?} under the bar {rect:?}"
                );
            }
        }
    }
}

#[test]
fn the_state_dump_carries_the_partition() {
    let mut desk = Desk::fixture("docked-laptop");
    desk.focus(ON_PRIMARY[0]);
    let chord = ModifiersState::SUPER | ModifiersState::CONTROL | ModifiersState::SHIFT;
    desk.press(NamedKey::ArrowRight, chord);
    let dump = desk.state_dump();
    let home = &dump["outputs"]["home"];
    assert_eq!(home[ON_PRIMARY[0].to_string()], MONITOR_KEY, "{dump:#}");
    assert_eq!(home[ON_PRIMARY[1].to_string()], PANEL_KEY);
    assert_eq!(home[ON_SECOND[2].to_string()], MONITOR_KEY);
    let displays = dump["outputs"]["displays"].as_array().expect("displays");
    assert_eq!(displays.len(), 2);
    assert_eq!(displays[0]["id"], 0, "primary first");
    assert_eq!(displays[0]["monitor"], PANEL_KEY);

    // What `state.json` holds loads back with the partition intact.
    let saved = serde_json::to_string(&*desk.fb.chrome().tree()).expect("the state saves");
    let loaded: WorkspaceTree = serde_json::from_str(&saved).expect("the saved state loads");
    assert_eq!(
        loaded.desktop.outputs.home,
        desk.fb.chrome().tree().desktop.outputs.home
    );
}

/// Every map key anywhere in an automerge document.
fn keys(doc: &AutoCommit, obj: &ObjId, out: &mut BTreeSet<String>) {
    match doc.object_type(obj) {
        Ok(ObjType::Map | ObjType::Table) => {
            for key in doc.keys(obj) {
                if let Ok(Some((Value::Object(_), child))) = doc.get(obj, key.as_str()) {
                    keys(doc, &child, out);
                }
                out.insert(key);
            }
        }
        Ok(ObjType::List) => {
            for i in 0..doc.length(obj) {
                if let Ok(Some((Value::Object(_), child))) = doc.get(obj, i) {
                    keys(doc, &child, out);
                }
            }
        }
        _ => {}
    }
}

#[test]
fn the_partition_is_never_replicated() {
    let mut desk = Desk::fixture("docked-laptop");
    desk.focus(ON_PRIMARY[0]);
    let chord = ModifiersState::SUPER | ModifiersState::CONTROL | ModifiersState::SHIFT;
    desk.press(NamedKey::ArrowRight, chord);
    let chrome = desk.fb.chrome();
    let mut tree = chrome.tree_mut();
    assert!(
        !tree.desktop.outputs.home.is_empty(),
        "there is a partition to leak"
    );
    let room = RoomId::parse(ROOM).expect("room");
    tree.seed_doc_from_cache();
    tree.sync_room_doc(room);
    let forbidden = [
        "outputs",
        "home",
        "origin",
        "displays",
        "focused",
        "last_active",
    ];
    let mut desktop_keys = BTreeSet::new();
    keys(&tree.doc, &ROOT, &mut desktop_keys);
    let mut room_keys = BTreeSet::new();
    keys(
        tree.room_docs.get(&room).expect("the room was projected"),
        &ROOT,
        &mut room_keys,
    );
    drop(tree);
    drop(chrome);
    assert!(
        desktop_keys.contains("window_groups"),
        "the desktop document carries the strip's windows: {desktop_keys:?}"
    );
    assert!(
        room_keys.contains(&format!("{:016x}", ON_PRIMARY[0])),
        "the room document carries the strip's windows: {room_keys:?}"
    );
    for (name, found) in [("desktop", &desktop_keys), ("room", &room_keys)] {
        for key in forbidden {
            assert!(
                !found.contains(key),
                "the {name} document carries `{key}`: {found:?}"
            );
        }
    }
}

/// A collaborator's device holding the same room, fed whatever this device
/// publishes for it.
struct Collaborator {
    tree: WorkspaceTree,
    room: RoomId,
}

impl Collaborator {
    fn of(desk: &Desk) -> Self {
        let room = RoomId::parse(ROOM).expect("room");
        let mut tree = WorkspaceTree::default();
        tree.set_salt_override(SALT.wrapping_add(1));
        let ws = tree.append_empty_workspace("Joined");
        assert!(tree.set_room(ws, room));
        // The desk's invite names it as the room's governor.
        assert!(tree.pin_room_authority(room, PeerOrigin::peer_id("docked-laptop")));
        let mut collaborator = Self { tree, room };
        collaborator.receive(desk);
        collaborator
    }

    /// Take in everything `desk` publishes for the room now, after
    /// mirroring its cache into its documents the way any later edit does.
    fn receive(&mut self, desk: &Desk) {
        let changes = {
            let chrome = desk.fb.chrome();
            let mut tree = chrome.tree_mut();
            tree.seed_doc_from_cache();
            tree.export_room_changes_since(self.room, &[])
                .expect("the room is published")
        };
        self.tree
            .apply_room_changes(self.room, &changes, &PeerOrigin::peer_id("docked-laptop"))
            .expect("the room's changes apply");
    }

    fn strip_order(&self) -> Vec<GroupId> {
        self.tree
            .desktop
            .workspaces
            .iter()
            .flat_map(|w| &w.strips)
            .find(|s| s.id == STRIP)
            .map(|s| s.window_groups.iter().map(|g| g.id).collect())
            .unwrap_or_default()
    }
}

#[test]
fn display_changes_never_reorder_a_collaborators_strip() {
    let shared: Vec<GroupId> = ON_PRIMARY.iter().chain(ON_SECOND.iter()).copied().collect();
    for fixture in ["docked-laptop", "vertical-stack"] {
        let mut desk = Desk::fixture(fixture);
        let mut collaborator = Collaborator::of(&desk);
        assert_eq!(collaborator.strip_order(), shared, "{fixture}: in sync");

        // Unplugged: the monitor's groups join the panel.
        desk.focus(ON_PRIMARY[0]);
        desk.unplug_all_but_primary();
        collaborator.receive(&desk);
        assert_eq!(collaborator.strip_order(), shared, "{fixture}: unplug");
        assert_eq!(desk.strip_order(), shared, "{fixture}: unplug");

        // Replugged, then a group sent to the other display.
        desk.replug();
        desk.focus(ON_SECOND[1]);
        desk.focus(ON_PRIMARY[2]);
        let chord = ModifiersState::SUPER | ModifiersState::CONTROL | ModifiersState::SHIFT;
        desk.press(NamedKey::ArrowRight, chord);
        assert_eq!(desk.display_of(ON_PRIMARY[2]), Some(1), "{fixture}: moved");
        collaborator.receive(&desk);
        assert_eq!(
            collaborator.strip_order(),
            shared,
            "{fixture}: move to display"
        );
        assert_eq!(desk.strip_order(), shared, "{fixture}: move to display");
    }
}

/// Each group lies on the display its monitor is connected as, and is laid
/// out within it.
fn assert_groups_on_their_monitors(desk: &mut Desk, what: &str) {
    for (ids, key) in [(ON_PRIMARY, PANEL_KEY), (ON_SECOND, MONITOR_KEY)] {
        let display = desk.output_of(key);
        let own = desk.display(display);
        for id in ids {
            assert_eq!(desk.display_of(id), Some(display), "{what}: {id} on {key}");
            let (_, rect, lane) = desk
                .layout()
                .into_iter()
                .find(|(g, _, _)| *g == id)
                .expect("group laid out");
            assert!(
                lane == Some(display) && rect.y0 >= own.y0 - 0.5 && rect.y1 <= own.y1 + 0.5,
                "{what}: {id} is laid out on {key}: {rect:?} in lane {lane:?} on {own:?}"
            );
        }
    }
}

/// The same two monitors with their output numbers and connectors swapped,
/// as a restart or moving the cables to other ports enumerates them.
fn swapped(outputs: &[Output]) -> Vec<Output> {
    let mut outputs = outputs.to_vec();
    let (a, b) = (
        outputs[0].identity.connector.clone(),
        outputs[1].identity.connector.clone(),
    );
    outputs[0].id = OutputId(1);
    outputs[0].identity.connector = b;
    outputs[1].id = OutputId(0);
    outputs[1].identity.connector = a;
    outputs.sort_by_key(|o| o.id.0);
    outputs
}

#[test]
fn renumbered_outputs_return_every_group_to_its_monitor() {
    for fixture in ["docked-laptop", "vertical-stack"] {
        let mut desk = Desk::fixture(fixture);
        assert_groups_on_their_monitors(&mut desk, fixture);
        // What `state.json` holds.
        let saved = serde_json::to_string(&*desk.fb.chrome().tree()).expect("the state saves");

        // Replugged on each other's ports while running.
        desk.outputs = swapped(&desk.outputs);
        desk.replug();
        assert_eq!(desk.output_of(MONITOR_KEY), 0, "{fixture}: renumbered");
        assert_groups_on_their_monitors(&mut desk, &format!("{fixture}, port swap"));
        assert_every_group_stays_on_its_display(&mut desk, fixture);

        // Restarted from the saved state with the outputs renumbered.
        let mut tree: WorkspaceTree = serde_json::from_str(&saved).expect("the saved state loads");
        tree.set_salt_override(SALT);
        let outputs = desk.outputs.clone();
        let mut restarted = Desk::with_tree(tree, outputs);
        assert_groups_on_their_monitors(&mut restarted, &format!("{fixture}, restart"));
        let dump = restarted.state_dump();
        assert!(
            dump["outputs"]["origin"]
                .as_object()
                .is_none_or(serde_json::Map::is_empty),
            "{fixture}: nothing was stranded: {dump:#}"
        );
    }
}

#[test]
fn every_display_shows_every_strips_groups() {
    for fixture in ["docked-laptop", "vertical-stack"] {
        let outputs = with_monitors(output_fixture(fixture).expect("fixture"));
        let mut desk = Desk::with_tree(two_strip_tree(), outputs);
        for display in [0, 1] {
            let own = desk.display(display);
            let mut bands = Vec::new();
            for strip in [STRIP, STRIP_B] {
                let on: Vec<Rect> = desk
                    .layout_of(strip)
                    .into_iter()
                    .filter(|(_, _, d)| *d == Some(display))
                    .map(|(_, r, _)| r)
                    .collect();
                assert!(
                    !on.is_empty(),
                    "{fixture}: strip {strip:?} has windows on {display}"
                );
                for rect in &on {
                    let shown = rect.intersect(own);
                    assert!(
                        shown.height() > 100.0
                            && rect.y0 >= own.y0 - 0.5
                            && rect.y1 <= own.y1 + 0.5,
                        "{fixture}: a window of strip {strip:?} is shown on display {display}: {rect:?} on {own:?}"
                    );
                }
                let first = on[0];
                assert!(
                    first.intersect(own).width() > 0.0,
                    "{fixture}: the first window of strip {strip:?} is in view on {display}: {first:?}"
                );
                bands.push((
                    on.iter().map(|r| r.y0).fold(f64::INFINITY, f64::min),
                    on.iter().map(|r| r.y1).fold(f64::NEG_INFINITY, f64::max),
                ));
            }
            assert!(
                bands[0].1 <= bands[1].0 + 0.5,
                "{fixture}: the strips stack on display {display} without overlapping: {bands:?}"
            );
        }
    }
}

#[test]
fn dragging_a_group_onto_another_display_moves_it_there() {
    for touch in [false, true] {
        let how = if touch { "touch" } else { "pointer" };
        let mut desk = Desk::fixture("docked-laptop");
        let moving = ON_PRIMARY[1];
        let from = desk.grab_point(STRIP, moving);
        // Between the monitor's first and second windows.
        let a = desk.rect_of(ON_SECOND[0]);
        let to = Point::new(a.x1 - 20.0, from.y);
        assert!(
            desk.display(1).contains(to),
            "{how}: {to:?} is on the monitor"
        );
        desk.drag(from, to, touch);

        assert_eq!(
            desk.display_of(moving),
            Some(1),
            "{how}: it belongs to the monitor"
        );
        let dump = desk.state_dump();
        assert_eq!(
            dump["outputs"]["home"][moving.to_string()],
            MONITOR_KEY,
            "{how}: {dump:#}"
        );
        let on_second: Vec<GroupId> = desk
            .fb
            .host()
            .display_partition_summary()
            .into_iter()
            .find(|(d, _)| *d == 1)
            .map(|(_, groups)| groups)
            .expect("second display");
        assert_eq!(
            on_second,
            vec![ON_SECOND[0], moving, ON_SECOND[1], ON_SECOND[2]],
            "{how}: dropped between the two"
        );
        assert_eq!(
            desk.layout()
                .iter()
                .find(|g| g.0 == moving)
                .and_then(|g| g.2),
            Some(1),
            "{how}: laid out on the monitor"
        );
        assert_takes_its_new_displays_height(&mut desk, how, moving);
        assert_every_group_stays_on_its_display(&mut desk, how);
        // Nothing overlaps on either display.
        let layout = desk.layout();
        for (i, (a, ra, da)) in layout.iter().enumerate() {
            for (b, rb, db) in layout.iter().skip(i + 1) {
                let overlap = ra.intersect(*rb);
                assert!(
                    da != db || overlap.width() <= 0.5 || overlap.height() <= 0.5,
                    "{how}: {a} {ra:?} and {b} {rb:?} overlap"
                );
            }
        }
    }
}

/// `output` mirrored onto another connector as output `id`: the same
/// logical rectangle and the same place in the scene, as the backend lays
/// out a `mirror_of` pair.
fn mirrored(output: &Output, id: u32, monitor: (&str, &str, &str, &str)) -> Output {
    let mut mirror = output.clone();
    mirror.id = OutputId(id);
    mirror.identity = identity(monitor);
    mirror
}

const PROJECTOR: (&str, &str, &str, &str) = ("HDMI-1", "Tilo", "Test Projector", "J-0001");

/// A projector mirroring the panel shows the same image: one display to
/// the partition, not a second lane laid over the first.
#[test]
fn a_mirrored_pair_is_one_display() {
    let panel = parse_output_spec("1920x1080").expect("spec").remove(0);
    let outputs = vec![panel.clone(), mirrored(&panel, 1, MONITOR)];
    let mut desk = Desk::across(outputs);
    let displays = desk.fb.chrome().tree().desktop.outputs.displays.clone();
    assert_eq!(
        displays.iter().map(|d| d.id).collect::<Vec<_>>(),
        vec![0],
        "the mirror is not a display of its own: {displays:?}"
    );
    assert!(!desk.fb.host().is_partitioned(), "one image, one row");
    let panel = desk.display(0);
    for (id, rect, display) in desk.layout() {
        assert_eq!(display, None, "{id} is in a partitioned row");
        assert!(
            rect.width() > 0.0 && rect.y1 <= panel.y1 + 0.5,
            "{id} is laid out on the shared image: {rect:?}"
        );
    }
    let bars = desk.fb.host().top_bar_rects();
    assert!(
        bars.iter().all(|b| b.2),
        "no compact bar over the mirrored image: {bars:?}"
    );
}

/// A mirrored pair beside a third monitor: two displays, and the mirror
/// gets neither a lane nor a compact bar over the image it copies.
#[test]
fn a_mirrored_pair_beside_a_monitor_is_two_displays() {
    let mut outputs = with_monitors(parse_output_spec("1920x1080,1920x1080").expect("spec"));
    let projector = mirrored(&outputs[0], 2, PROJECTOR);
    outputs.push(projector);
    let desk = Desk::with_tree(tree(), outputs);
    let displays = desk.fb.chrome().tree().desktop.outputs.displays.clone();
    assert_eq!(
        displays.iter().map(|d| d.id).collect::<Vec<_>>(),
        vec![0, 1],
        "{displays:?}"
    );
    let bars = desk.fb.host().top_bar_rects();
    let compact: Vec<u32> = bars.iter().filter(|b| !b.2).map(|b| b.0).collect();
    assert_eq!(compact, vec![1], "{bars:?}");
    assert_eq!(
        desk.fb.host().display_partition_summary(),
        vec![(0, ON_PRIMARY.to_vec()), (1, ON_SECOND.to_vec())],
        "each monitor keeps its own windows"
    );
}

/// A desktop-wide overlay opens on the display in use rather than centred
/// on the seam between two: the palette and toasts on the focused display,
/// Quick Settings under the tray on the primary's full top bar.
#[test]
fn overlays_open_on_the_focused_display() {
    let mut desk = Desk::fixture("docked-laptop");
    let primary = desk.display(0);
    let second = desk.display(1);
    let within = |outer: Rect, inner: Rect| {
        inner.width() > 0.0
            && inner.x0 >= outer.x0 - 0.5
            && inner.x1 <= outer.x1 + 0.5
            && inner.y0 >= outer.y0 - 0.5
            && inner.y1 <= outer.y1 + 0.5
    };
    for (group, display) in [(ON_SECOND[0], second), (ON_PRIMARY[0], primary)] {
        desk.focus(group);
        let area = desk.fb.host().overlay_area();
        assert!(
            (area.x0 - display.x0).abs() < 0.5 && (area.x1 - display.x1).abs() < 0.5,
            "overlays follow focus to {display:?}: {area:?}"
        );
        desk.fb.host_mut().set_palette_visible(true);
        desk.fb.host_mut().force_palette_settled();
        desk.settle();
        let card = desk.fb.host().palette_rect_for_tests();
        assert!(
            within(display, card),
            "the palette {card:?} is on the focused display {display:?}"
        );
        assert!(
            (card.center().x - display.center().x).abs() < 1.0,
            "the palette {card:?} centres on {display:?}"
        );
        desk.fb.host_mut().set_palette_visible(false);
        desk.settle();
        let quick_settings = desk.fb.host().quick_settings_rect_for_tests();
        assert!(
            within(primary, quick_settings),
            "Quick Settings {quick_settings:?} hangs from the tray on {primary:?}"
        );
    }
}

/// Super+Shift+arrow reorders within a display until the group is its
/// outermost one; one more press carries it across the seam, where it
/// lands beside the seam rather than at its old place in the strip order.
#[test]
fn reordering_past_a_displays_edge_carries_the_group_across() {
    let mut desk = Desk::fixture("docked-laptop");
    let on = |desk: &Desk, display: u32| -> Vec<GroupId> {
        desk.fb
            .host()
            .display_partition_summary()
            .into_iter()
            .find(|(d, _)| *d == display)
            .map(|(_, groups)| groups)
            .expect("display")
    };
    // Give the second display a group that comes first in the strip, so
    // the strip order and the seam disagree.
    desk.focus(ON_PRIMARY[0]);
    let across = ModifiersState::SUPER | ModifiersState::CONTROL | ModifiersState::SHIFT;
    desk.press(NamedKey::ArrowRight, across);
    assert_eq!(on(&desk, 1)[0], ON_PRIMARY[0]);

    let moving = ON_SECOND[2];
    desk.focus(moving);
    let reorder = ModifiersState::SUPER | ModifiersState::SHIFT;
    for _ in 0..3 {
        desk.press(NamedKey::ArrowLeft, reorder);
    }
    assert_eq!(desk.display_of(moving), Some(1), "still reordering within");
    assert_eq!(on(&desk, 1)[0], moving, "outermost on the second display");

    desk.press(NamedKey::ArrowLeft, reorder);
    assert_eq!(desk.display_of(moving), Some(0), "carried across the seam");
    assert_eq!(desk.focused_display(), Some(0), "focus went along");
    assert_eq!(
        on(&desk, 0),
        vec![ON_PRIMARY[1], ON_PRIMARY[2], moving],
        "it lands beside the seam it crossed"
    );

    desk.press(NamedKey::ArrowRight, reorder);
    assert_eq!(desk.display_of(moving), Some(1), "and back");
    assert_eq!(on(&desk, 1)[0], moving, "first again beside the seam");
}

/// One strip of four default-width windows, all on the primary display:
/// more than its display shows, the shape a session reaches after opening a
/// few apps.
fn primary_heavy_tree() -> WorkspaceTree {
    let ids = [id(3001), id(3002), id(3003), id(3004)];
    let mut tree = tree();
    tree.desktop.workspaces[0].strips[0].window_groups = ids
        .iter()
        .map(|&id| WindowGroup {
            width: WindowWidth::Normal,
            ..group(id)
        })
        .collect();
    tree.desktop.active_group.insert(STRIP, ids[0]);
    tree.desktop.outputs.home.clear();
    for id in ids {
        tree.desktop.outputs.home.insert(id, PANEL_KEY.into());
    }
    tree
}

/// The containers around a strip keep the width they were given, however
/// far its windows run past their display: those windows are the strip's to
/// pan and clip. Every rect comes from the registry `list_targets` serves,
/// which is what the acceptance layout check reads; a desktop column
/// reported as wide as the strip's content reaches past every display.
/// Checked on the acceptance VM's pair of displays and on one display.
#[test]
fn a_strip_running_past_its_display_never_widens_the_desktop_around_it() {
    for spec in ["1920x1080@1,1280x800@1.25", "1920x1080@1"] {
        let outputs = with_monitors(parse_output_spec(spec).expect("spec"));
        let desktop = outputs
            .iter()
            .map(Output::logical_rect)
            .reduce(|a, b| a.union(b))
            .expect("a display");
        let mut desk = Desk::with_tree(primary_heavy_tree(), outputs);
        let max_scroll = desk
            .fb
            .host_mut()
            .group_row_mut_for_tests(STRIP)
            .expect("the strip has a row")
            .max_scroll_x();
        assert!(
            max_scroll > 0.0,
            "{spec}: the windows run past their display, so the strip pans ({max_scroll})"
        );
        let registry = desk.fb.widget_registry();
        let rows: Vec<&String> = registry.keys().filter(|n| n.contains("GroupRow")).collect();
        assert!(!rows.is_empty(), "{spec}: the strip is a target");
        // Every target the rows sit inside: an indexed name is a path
        // segment of theirs, an unindexed one names its last segment bare.
        let around: Vec<(&String, &Rect)> = registry
            .iter()
            .filter(|(name, _)| !name.contains("GroupRow"))
            .filter(|(name, _)| {
                let probe = if name.ends_with(']') {
                    format!("{name}/")
                } else {
                    format!("{name}[")
                };
                rows.iter().any(|row| row.starts_with(&probe))
            })
            .collect();
        assert!(
            around
                .iter()
                .any(|(name, _)| name.contains("DesktopSurface")),
            "{spec}: the desktop surface holds the strip: {around:?}"
        );
        for (name, rect) in around {
            assert!(
                rect.x0 >= desktop.x0 - 0.5
                    && rect.x1 <= desktop.x1 + 0.5
                    && rect.y0 >= desktop.y0 - 0.5
                    && rect.y1 <= desktop.y1 + 0.5,
                "{spec}: {name} is laid out at {rect:?}, past the displays {desktop:?}"
            );
        }
    }
}

/// Print opens Capture Studio on the focused display, at that display's own
/// size: its picker bars along that display's top and bottom edges. Spread
/// over the whole desktop of a 2x panel beside a 1x monitor, the bars were
/// centred on the combined width and hung from its tallest edge, on the
/// monitor and below the panel's bottom, while the panel only greyed.
#[test]
fn capture_studio_opens_on_the_focused_display() {
    let mut desk = Desk::fixture("mixed-scale-offset");
    for (display, group) in [(0, ON_PRIMARY[0]), (1, ON_SECOND[0]), (0, ON_PRIMARY[1])] {
        desk.focus(group);
        assert_eq!(
            desk.focused_display(),
            Some(display),
            "focus is on {display}"
        );
        assert!(
            desk.fb
                .host_mut()
                .open_capture_studio(ui::widget::CaptureMode::Screenshot),
            "Print opens the studio"
        );
        desk.settle();
        let studio = desk
            .fb
            .widget_registry()
            .get("shell.capture-studio")
            .copied()
            .expect("the studio is a target");
        let expected = desk.display(display);
        assert!(
            (studio.x0 - expected.x0).abs() < 0.5
                && (studio.y0 - expected.y0).abs() < 0.5
                && (studio.width() - expected.width()).abs() < 0.5
                && (studio.height() - expected.height()).abs() < 0.5,
            "the studio {studio:?} covers display {display} {expected:?}"
        );
        desk.dispatch(
            &ui::synthetic_input::named_key(NamedKey::Escape),
            ModifiersState::empty(),
        );
        desk.settle();
        assert!(!desk.fb.host().capture_studio_open(), "Escape closes it");
    }
}
