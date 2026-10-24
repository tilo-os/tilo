//! Strips picked in one place, people shown once, nothing over a window's
//! bar, and touch reaching everything.

use vello::kurbo::Point;
use winit::event::{MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use super::super::driver::ShellDriver;
use super::{Action, Scenario};

/// The strip switcher as the one place strips are picked, by pointer and by
/// touch, and what stays clear of the window bars.
pub(super) fn one_place() -> Scenario {
    Scenario {
        name: "one_place",
        description: "Strips picked in one place, people shown once, nothing over a \
         window's bar, touch reaching everything: the bar naming only the strip shown, \
         its name opening the switcher, a tap on the ribbon opening it pinned, a \
         long-press putting a strip's Rename and Remove beside its label, arriving said \
         on the bar's faces, a banner and the follow bar in the trailing zone below the \
         window bars, a swipe down from the top edge revealing the hidden top bar, and \
         a fullscreen window's bar.",
        actions: vec![
            Action::SeedStripsWithPeople,
            Action::MoveCursor(Point::new(800.0, 500.0)),
            Action::WaitWall(1700),
            Action::Wait(4),
            Action::Snapshot("01-the-bar-names-the-strip-shown"),
            Action::ClickNamed("topbar_strip_name"),
            Action::Wait(10),
            Action::Snapshot("02-its-name-opens-the-switcher"),
            Action::Key(Key::Named(NamedKey::Escape)),
            Action::Wait(4),
            Action::MoveCursor(Point::new(800.0, 500.0)),
            Action::WaitWall(300),
            Action::ClickNamed("strip_switcher"),
            Action::Wait(10),
            Action::Snapshot("03-a-tap-on-the-ribbon-opens-it"),
            Action::SecondaryClickNamed("strip_switcher.strip.2"),
            Action::Wait(6),
            Action::Snapshot("04-a-long-press-puts-out-the-strip-actions"),
            Action::Key(Key::Named(NamedKey::Escape)),
            Action::Key(Key::Named(NamedKey::Escape)),
            Action::MoveCursor(Point::new(800.0, 500.0)),
            Action::Wait(4),
            Action::Arrive { public: false },
            Action::Wait(3),
            Action::Snapshot("05-arriving-is-said-on-the-faces"),
            Action::WaitWall(4_500),
            Action::Notify(
                "scribe asks first: Rename",
                "Rename files based on their contents",
            ),
            Action::FollowPeer(Some("cy")),
            Action::Wait(6),
            Action::Snapshot("06-banner-and-follow-bar-below-the-window-bars"),
            Action::FollowPeer(None),
            Action::Wait(2),
            Action::HideTopBar(true),
            Action::Wait(6),
            Action::Snapshot("07-the-top-bar-hidden"),
            Action::TouchSwipe(Point::new(800.0, 2.0), Point::new(800.0, 90.0)),
            Action::Wait(6),
            Action::Snapshot("08-a-swipe-down-reveals-it"),
            Action::HideTopBar(false),
            Action::FullScreenFocused(true),
            Action::Wait(6),
            Action::Snapshot("09-full-screen"),
            Action::TouchSwipe(Point::new(800.0, 2.0), Point::new(800.0, 90.0)),
            Action::Wait(6),
            Action::Snapshot("10-a-swipe-down-reveals-the-window-bar"),
            Action::FullScreenFocused(false),
            Action::Wait(4),
        ],
    }
}

/// A fullscreen window's bar: shown as the window enters, collapsed after a
/// short idle, and back over the window, which never moves, for the key, a
/// swipe down from the top edge and the pointer pushed to the top.
pub(super) fn fullscreen_bar() -> Scenario {
    Scenario {
        name: "fullscreen_bar",
        description: "A fullscreen window's bar: shown over the window as it enters \
         fullscreen, collapsed after a short idle with the window unmoved, back for the \
         Super key, for a swipe down from the top edge and for the pointer pushed to \
         the top.",
        actions: vec![
            Action::MoveCursor(Point::new(800.0, 500.0)),
            Action::WaitWall(600),
            Action::Wait(4),
            Action::FullScreenFocused(true),
            Action::Wait(20),
            Action::Snapshot("01-entering-full-screen-shows-the-bar"),
            Action::WaitWall(2_300),
            Action::Wait(20),
            Action::Snapshot("02-idle-it-collapses"),
            Action::FullscreenBarKey,
            Action::Wait(20),
            Action::Snapshot("03-the-key-brings-it-back"),
            Action::WaitWall(2_300),
            Action::Wait(20),
            Action::TouchSwipe(Point::new(800.0, 2.0), Point::new(800.0, 90.0)),
            Action::Wait(20),
            Action::Snapshot("04-a-swipe-down-brings-it-back"),
            Action::WaitWall(2_300),
            Action::Wait(20),
            Action::Snapshot("05-idle-again"),
            Action::CompositorPointer(Point::new(800.0, 300.0)),
            Action::CompositorPointer(Point::new(800.0, 120.0)),
            Action::CompositorPointer(Point::new(800.0, 30.0)),
            Action::CompositorPointer(Point::new(800.0, 0.0)),
            Action::WaitWall(300),
            Action::Wait(20),
            Action::Snapshot("06-the-pointer-at-the-top-brings-it-back"),
            Action::FullScreenFocused(false),
            Action::Wait(4),
        ],
    }
}

/// A finger dragged from `from` to `to`, as raw touch in nine steps.
pub(super) fn touch_swipe(driver: &mut ShellDriver, from: Point, to: Point) {
    let touch = |phase, p: Point| {
        WindowEvent::Touch(winit::event::Touch {
            device_id: winit::event::DeviceId::dummy(),
            phase,
            location: winit::dpi::PhysicalPosition::new(p.x, p.y),
            force: None,
            id: 41,
        })
    };
    driver.dispatch(&touch(winit::event::TouchPhase::Started, from));
    for k in 1..=8 {
        let p = from.lerp(to, f64::from(k) / 8.0);
        driver.dispatch(&touch(winit::event::TouchPhase::Moved, p));
    }
    driver.dispatch(&touch(winit::event::TouchPhase::Ended, to));
}

/// A secondary click on the named target: what a long-press by touch becomes.
pub(super) fn secondary_click_named(driver: &mut ShellDriver, name: &str) {
    let Some(p) = driver.resolve_named_centre(name) else {
        eprintln!("scenario: could not resolve named target '{name}'");
        return;
    };
    driver.move_cursor(p);
    for action in [
        ui::synthetic_input::PointerAction::Press(MouseButton::Right),
        ui::synthetic_input::PointerAction::Release(MouseButton::Right),
    ] {
        driver.dispatch(&action.event(1.0));
    }
}

/// A notification banner arrives.
pub(super) fn notify(driver: &ShellDriver, title: &'static str, body: &'static str) {
    driver
        .chrome_handle()
        .borrow_mut()
        .push_notification(title, body);
}

/// The focused window goes full screen, or leaves it.
pub(super) fn full_screen_focused(driver: &ShellDriver, on: bool) {
    let handle = driver.chrome_handle();
    let group = {
        let st = handle.borrow();
        let tree = st.tree();
        let strip = tree.desktop.active_strip.get(&tree.desktop.active).copied();
        let group = strip.and_then(|strip| tree.desktop.active_group_for(strip));
        drop(tree);
        group
    };
    handle
        .borrow_mut()
        .set_user_fullscreen_group(if on { group } else { None });
}
