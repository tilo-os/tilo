//! The capture tool picks a window with the desktop's own navigation: a
//! click focuses the window and scrolls it into view, and moving focus the
//! usual way picks another one.
#![allow(
    clippy::significant_drop_tightening,
    reason = "the frame builder's chrome guards are held for one short assertion each"
)]

use automerge::{AutoCommit, ROOT};
use vello::kurbo::Rect;
use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::shell::frame::FrameBuilder;
use crate::shell::state::{
    AppKind, GroupId, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceId, WorkspaceTree, compose_id,
};
use ui::widget::Widget as _;

const SALT: u16 = 1;
const fn id(counter: u64) -> u64 {
    compose_id(SALT, counter)
}
const STRIP: StripId = StripId(id(91));
const FIRST: GroupId = id(2001);
const SECOND: GroupId = id(2002);
const THIRD: GroupId = id(2003);
const SIZE: (u32, u32) = (1600, 900);

fn group(id: GroupId) -> WindowGroup {
    let tab = id.saturating_add(5000);
    WindowGroup {
        id,
        tabs: vec![Tab {
            id: tab,
            title: format!("W{id}"),
            tile: TileContent::Stub {
                kind: AppKind::Other("Test".into()),
                label: format!("W{id}"),
            },
            app_title: None,
        }],
        active_tab: tab,
        width: WindowWidth::Fraction(700),
        maximized: false,
        focus_history: Vec::new(),
    }
}

/// Three windows each most of the screen wide: the first focused and on
/// screen, the second partly past the right edge, the third beyond it.
fn tree() -> WorkspaceTree {
    let ws_id = WorkspaceId(9);
    let mut ws = Workspace::new(ws_id, "Desk");
    ws.strips = vec![Strip {
        id: STRIP,
        name: "Main".into(),
        window_groups: vec![group(FIRST), group(SECOND), group(THIRD)],
    }];
    let mut tree = WorkspaceTree::default();
    tree.set_salt_override(SALT);
    tree.desktop.workspaces = vec![ws];
    tree.desktop.active = ws_id;
    tree.desktop.active_strip.insert(ws_id, STRIP);
    tree.desktop.active_group.insert(STRIP, FIRST);
    tree
}

struct Desk {
    fb: FrameBuilder,
    modifiers: Modifiers,
}

impl Desk {
    fn new() -> Self {
        let mut desk = Self {
            fb: FrameBuilder::with_tree(tree(), SIZE),
            modifiers: Modifiers::default(),
        };
        desk.settle();
        desk
    }

    fn settle(&mut self) {
        for _ in 0..4 {
            let mut doc = AutoCommit::new();
            self.fb.pump_apps(&mut doc, &ROOT);
            self.fb.relayout();
            let _ = self.fb.paint();
        }
    }

    fn dispatch(&mut self, event: &WindowEvent) {
        let mut doc = AutoCommit::new();
        let mut focus = None;
        self.fb.dispatch_event(
            event,
            &self.modifiers,
            &mut ui::widget::Edits::lend(&mut doc),
            &ROOT,
            &mut focus,
        );
    }

    fn click(&mut self, x: f64, y: f64) {
        self.dispatch(&ui::synthetic_input::cursor_moved(x, y));
        self.dispatch(&ui::synthetic_input::mouse_input(
            ElementState::Pressed,
            MouseButton::Left,
        ));
        self.dispatch(&ui::synthetic_input::mouse_input(
            ElementState::Released,
            MouseButton::Left,
        ));
    }

    fn window(&mut self, group: GroupId) -> Rect {
        let index = [FIRST, SECOND, THIRD]
            .iter()
            .position(|g| *g == group)
            .expect("one of the three");
        self.fb
            .host_mut()
            .group_at_for_tests(STRIP, index)
            .expect("the window")
            .get_rect()
    }

    fn active_group(&self) -> Option<GroupId> {
        self.fb.chrome().tree().desktop.active_group_for(STRIP)
    }

    fn picked(&self) -> Option<Rect> {
        self.fb
            .host()
            .capture_studio
            .test_rects()
            .into_iter()
            .find(|(name, _)| name == "capture.window-pick")
            .map(|(_, rect)| rect)
    }

    fn choose_window(&mut self) {
        assert!(
            self.fb
                .host_mut()
                .open_capture_studio(ui::widget::CaptureMode::Screenshot)
        );
        self.settle();
        self.dispatch(&ui::synthetic_input::char_key("2"));
        self.settle();
        assert_eq!(
            self.fb.host().capture_studio.source_mode(),
            ui::widget::SourceMode::Window
        );
    }
}

/// The picked window's outline is the part of it on screen, as of the last
/// update: the same rect as `window` shown on screen, to within the frame
/// a moving strip travels between the two reads.
fn same_window(picked: Option<Rect>, window: Rect) -> bool {
    let shown = window.intersect(Rect::new(0.0, 0.0, f64::from(SIZE.0), f64::from(SIZE.1)));
    picked.is_some_and(|picked| {
        (picked.x0 - shown.x0).abs() < 2.0
            && (picked.x1 - shown.x1).abs() < 2.0
            && (picked.y0 - shown.y0).abs() < 0.5
            && (picked.y1 - shown.y1).abs() < 0.5
    })
}

impl Desk {
    /// Settle until `group` stops moving (the strip's standard motion runs on
    /// the wall clock), and return where it rests.
    fn resting(&mut self, group: GroupId) -> Rect {
        let mut last = self.window(group);
        for _ in 0..120 {
            std::thread::sleep(std::time::Duration::from_millis(16));
            self.settle();
            let now = self.window(group);
            if (now.x0 - last.x0).abs() < 0.01 {
                return now;
            }
            last = now;
        }
        last
    }
}

fn on_screen(rect: Rect) -> bool {
    rect.x0 >= -0.5 && rect.x1 <= f64::from(SIZE.0) + 0.5
}

#[test]
fn picking_a_window_partly_off_screen_scrolls_it_into_view() {
    let mut desk = Desk::new();
    let partly = desk.window(SECOND);
    assert!(
        partly.x0 < f64::from(SIZE.0) && partly.x1 > f64::from(SIZE.0),
        "precondition: the second window is partly off screen: {partly:?}"
    );
    desk.choose_window();
    // Click the part of it that shows.
    desk.click(
        f64::midpoint(partly.x0, f64::from(SIZE.0)),
        partly.center().y,
    );
    desk.settle();
    assert_eq!(desk.active_group(), Some(SECOND), "the click focused it");
    // The strip scrolls it into view with the standard motion.
    let shown = desk.resting(SECOND);
    assert!(
        on_screen(shown),
        "the picked window came into view: {shown:?}"
    );
    assert!(
        same_window(desk.picked(), shown),
        "and it is the one picked: {:?} for {shown:?}",
        desk.picked()
    );
    assert!(
        desk.fb.host().capture_studio.is_open(),
        "picking a window does not take the capture"
    );
}

#[test]
fn moving_focus_the_usual_way_picks_the_next_window() {
    let mut desk = Desk::new();
    desk.choose_window();
    let first = desk.window(FIRST);
    assert!(same_window(desk.picked(), first), "{:?}", desk.picked());
    desk.modifiers = Modifiers::from(ModifiersState::SUPER);
    desk.dispatch(&ui::synthetic_input::key_press(
        Key::Named(NamedKey::ArrowRight),
        None,
    ));
    desk.modifiers = Modifiers::default();
    desk.settle();
    assert_eq!(
        desk.active_group(),
        Some(SECOND),
        "the desktop's own focus key reached the desktop"
    );
    let second = desk.resting(SECOND);
    assert!(
        same_window(desk.picked(), second),
        "the picked window followed focus: {:?} for {second:?}",
        desk.picked()
    );
}
