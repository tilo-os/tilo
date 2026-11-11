//! A press on the desk is the desk's: the chrome over it never hears it, so the menu hanging off
//! the bar learns from the stack where it stands, and a press that misses it takes it down — and
//! stays down for that press.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]
use tilo_desktop::about::{AboutLinks, AboutVariant, TiloMenu};
use tilo_desktop::desktop_host::{DesktopChrome, NoticeTiles, PermissiveRules};
use tilo_desktop::overlays::{Overlays, Slot};
use tilo_desktop::overlays::{POPOVER_MARGIN, POPOVER_WIDTH, Target};
use tilo_desktop::topbar::TopBarChrome;
use tilo_desktop_model::{
    DesktopTree, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceSlot,
};
use tilo_desktop_surface::notifications::Notification;
use tilo_desktop_surface::{DesktopNotifications, DesktopSurface};
use ui::headless::FrameHarness;
use ui::kurbo::Rect;

const DISPLAY: Rect = Rect::new(0.0, 0.0, 1280.0, 800.0);

fn desk_tree() -> DesktopTree {
    let tabs = vec![Tab {
        id: 20,
        title: "Notes".into(),
        tile: TileContent::LocalApp {
            app_id: "so.tilo.notes".into(),
            label: "Notes".into(),
            note: String::new(),
        },
        app_title: None,
        view: String::new(),
        pinned: false,
    }];
    let group = WindowGroup {
        id: 3,
        active_tab: tabs[0].id,
        tabs,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    };
    let mut workspace = Workspace::new(WorkspaceSlot(1), "Work");
    workspace.strips = vec![Strip {
        id: StripId(2),
        name: "Main".into(),
        window_groups: vec![group],
    }];
    let mut tree = DesktopTree {
        active: workspace.slot,
        workspaces: vec![workspace],
        ..Default::default()
    };
    tree.active_strip.insert(WorkspaceSlot(1), StripId(2));
    tree.active_group.insert(StripId(2), 3);
    tree
}

/// The chrome over a desk, with the stack the session shares with it.
fn chrome() -> (FrameHarness, Overlays) {
    chrome_over(None)
}

/// The chrome over a desk the host installed `notifications` on, with the stack the session shares
/// with it.
fn chrome_over(notifications: Option<DesktopNotifications>) -> (FrameHarness, Overlays) {
    let tree = desk_tree();
    // The desk's own tree stays unmounted: the chrome is what the press is judged against.
    let (mut desk, handle) = DesktopSurface::new(tree.clone(), 7, Box::new(NoticeTiles));
    if let Some(notifications) = notifications {
        desk.set_notifications(notifications);
    }
    let overlays = Overlays::new();
    overlays.set_displays(&[DISPLAY], Some(DISPLAY));
    let mut chrome = DesktopChrome::new(
        handle.clone(),
        "g".into(),
        std::rc::Rc::new(PermissiveRules::new(handle)),
        TopBarChrome::from_tree(&tree),
        TiloMenu::new(AboutVariant::Native, &AboutLinks::default()),
        false,
    );
    chrome.set_overlays(overlays.clone());
    let mut frame = FrameHarness::for_root(
        ui::state::State::new(),
        chrome,
        (1280, 800),
        ui::Theme::dark(),
    );
    frame.relayout();
    (frame, overlays)
}

/// The workspace's name typed in the bar, from its menu's Rename, holds the keyboard while it is
/// open, so what is typed reaches the field and never the window in front; Enter and Escape each
/// end it and give the keyboard back to the window.
#[test]
fn the_workspace_name_typed_in_the_bar_holds_the_keyboard() {
    use tilo_desktop::overlays::Target;
    let (mut frame, overlays) = chrome();
    let typed = |frame: &FrameHarness, text: &str| {
        frame
            .a11y_nodes()
            .iter()
            .any(|(_, node)| node.value() == Some(text))
    };
    assert_eq!(overlays.target(), Target::Window);
    with_chrome(&mut frame, |chrome| {
        chrome.apply_intent(tilo_desktop::chrome::ChromeIntent::ToggleWorkspaceMenu { at: None });
    });
    frame.relayout();
    with_chrome(&mut frame, |chrome| {
        chrome.bar_mut().begin_workspace_rename();
    });
    frame.relayout();
    assert_eq!(
        overlays.target(),
        Target::Surface(Slot::WorkspaceName),
        "the field holds the keyboard"
    );
    assert!(
        !overlays.is_open(Slot::WorkspaceMenu),
        "the menu gave way to it"
    );
    frame.type_text("Projekt");
    frame.relayout();
    assert!(typed(&frame, "Projekt"), "what is typed is the field's");
    frame.key("Enter");
    frame.relayout();
    assert_eq!(
        overlays.target(),
        Target::Window,
        "Enter gives the keyboard back"
    );

    with_chrome(&mut frame, |chrome| {
        chrome.bar_mut().begin_workspace_rename();
    });
    frame.relayout();
    frame.type_text("Nope");
    frame.key("Escape");
    frame.relayout();
    assert!(!typed(&frame, "Nope"));
    assert_eq!(
        overlays.target(),
        Target::Window,
        "Escape gives the keyboard back"
    );
}

fn with_chrome<R>(frame: &mut FrameHarness, visit: impl FnOnce(&mut DesktopChrome) -> R) -> R {
    let mut chrome = frame.root_mut::<DesktopChrome>();
    visit(&mut chrome)
}

/// A press on the desk, far from both menus.
const DESK_PRESS: (f64, f64) = (640.0, 700.0);

#[test]
fn a_press_on_the_desk_closes_the_workspace_menu_and_it_stays_down() {
    let (mut frame, overlays) = chrome();
    with_chrome(&mut frame, |chrome| {
        chrome.apply_intent(tilo_desktop::chrome::ChromeIntent::ToggleWorkspaceMenu { at: None });
    });
    frame.relayout();
    assert!(with_chrome(&mut frame, |chrome| chrome.menu_open()));
    let slot = overlays.press_outside(DESK_PRESS.0, DESK_PRESS.1);
    assert_eq!(
        slot,
        Some(Slot::WorkspaceMenu),
        "the desk press misses the menu the bar opened"
    );

    overlays.dismiss(Slot::WorkspaceMenu);
    frame.relayout();
    assert!(
        !with_chrome(&mut frame, |chrome| chrome.menu_open()),
        "the menu follows the stack down"
    );
    assert!(
        !overlays.is_open(Slot::WorkspaceMenu),
        "and the stack takes the widget's word for it again: it is not put back up"
    );
}

/// A press on the chip that opened the menu closes it, and the chip the same press also lands on
/// does not put it back up: one press, one answer.
#[test]
fn the_press_that_takes_the_menu_down_does_not_raise_it_again() {
    let (mut frame, overlays) = chrome();
    with_chrome(&mut frame, |chrome| {
        chrome.apply_intent(tilo_desktop::chrome::ChromeIntent::ToggleWorkspaceMenu { at: None });
    });
    frame.relayout();
    let chip = with_chrome(&mut frame, |chrome| chrome.bar().workspace_menu_rect());
    assert_eq!(
        overlays.press_outside(chip.center().x, chip.center().y),
        Some(Slot::WorkspaceMenu),
        "the chip is not where the menu paints itself"
    );
    overlays.dismiss(Slot::WorkspaceMenu);
    frame.relayout();
    assert!(!with_chrome(&mut frame, |chrome| chrome.menu_open()));

    // The same press reaches the chip, which asks for the menu as any click on it does: the menu it
    // just took down stays down.
    with_chrome(&mut frame, |chrome| {
        chrome.apply_intent(tilo_desktop::chrome::ChromeIntent::ToggleWorkspaceMenu { at: None });
    });
    frame.relayout();
    assert!(
        !with_chrome(&mut frame, |chrome| chrome.menu_open()),
        "the press that closed it is still being delivered"
    );
    assert!(!overlays.is_open(Slot::WorkspaceMenu));

    // The press is over: the next one on the chip opens the menu again.
    overlays.press_ended();
    overlays.settle_press();
    with_chrome(&mut frame, |chrome| {
        chrome.apply_intent(tilo_desktop::chrome::ChromeIntent::ToggleWorkspaceMenu { at: None });
    });
    frame.relayout();
    assert!(
        with_chrome(&mut frame, |chrome| chrome.menu_open()),
        "a later press opens it"
    );
}

/// The Tilo mark zooms out of the workspace to the overview of the person's Tilo, and a second
/// press zooms back in: no menu hangs off it for a desk press to take down.
#[test]
fn the_tilo_mark_zooms_out_and_back_in() {
    let (mut frame, overlays) = chrome();
    with_chrome(&mut frame, |chrome| {
        chrome.apply_intent(tilo_desktop::chrome::ChromeIntent::ToggleTiloMenu);
    });
    frame.relayout();
    assert!(with_chrome(&mut frame, |chrome| chrome.overview_open()));
    assert!(!with_chrome(&mut frame, |chrome| chrome.menu_open()));
    assert_ne!(
        overlays.press_outside(DESK_PRESS.0, DESK_PRESS.1),
        Some(Slot::TiloMenu)
    );
    with_chrome(&mut frame, |chrome| {
        chrome.apply_intent(tilo_desktop::chrome::ChromeIntent::ToggleTiloMenu);
    });
    frame.relayout();
    assert!(!with_chrome(&mut frame, |chrome| chrome.overview_open()));
}

#[test]
fn a_press_on_the_menu_itself_is_the_menus() {
    let (mut frame, overlays) = chrome();
    with_chrome(&mut frame, |chrome| {
        chrome.apply_intent(tilo_desktop::chrome::ChromeIntent::ToggleWorkspaceMenu { at: None });
    });
    frame.relayout();
    // The panel the menu paints stands under the bar's leading end; its own middle is not a press
    // outside it.
    assert_eq!(
        overlays.press_outside(140.0, 120.0),
        None,
        "a press on the menu leaves it up"
    );
    assert!(with_chrome(&mut frame, |chrome| chrome.menu_open()));
}

/// The bar's bell opens the notification centre and closes it again.
#[test]
fn the_bell_opens_and_closes_the_centre() {
    use tilo_desktop::chrome::ChromeIntent;
    let notifications = DesktopNotifications::default();
    notifications.post(Notification::new("Tea is ready", "The timer finished"));
    let (mut frame, overlays) = chrome_over(Some(notifications.clone()));
    with_chrome(&mut frame, |chrome| {
        chrome.apply_intent(ChromeIntent::ToggleNotifications);
    });
    frame.relayout();
    assert!(overlays.is_open(Slot::Centre), "the bell opens the centre");
    assert!(notifications.centre_open(), "and the chrome shows it");
    with_chrome(&mut frame, |chrome| {
        chrome.apply_intent(ChromeIntent::ToggleNotifications);
    });
    frame.relayout();
    assert!(
        !overlays.is_open(Slot::Centre),
        "a second press on the bell closes it"
    );
    assert!(!notifications.centre_open(), "the chrome follows it down");
}

/// Banners, the notification centre and quick settings hang from one corner margin: the three
/// share one trailing edge rather than nearly lining up.
#[test]
fn banners_line_up_with_the_centre_and_quick_settings() {
    let banner =
        tilo_desktop_surface::notifications::transient_slot(DISPLAY, 48.0, (360.0, 72.0), &[]);
    assert_eq!(
        banner.x1,
        DISPLAY.x1 - POPOVER_MARGIN,
        "a banner keeps the corner margin"
    );
    let notifications = DesktopNotifications::default();
    let (mut frame, overlays) = chrome_over(Some(notifications.clone()));
    notifications.ask_centre(true);
    frame.relayout();
    let centre = overlays.place(Slot::Centre).expect("the centre is placed");
    assert_eq!(
        banner.x1, centre.x1,
        "the banner's edge is the centre's edge"
    );
}

#[test]
fn nothing_up_means_the_desk_press_is_the_desks() {
    let (_frame, overlays) = chrome();
    assert_eq!(overlays.press_outside(DESK_PRESS.0, DESK_PRESS.1), None);
}

/// The notification centre is a surface over the windows like every other one on the stack: the
/// chrome above them draws it, where quick settings hangs under the bar's trailing end, it holds
/// the keyboard while it is up, and a press that misses it takes it down.
#[test]
fn the_centre_stands_on_the_stack_where_quick_settings_hangs() {
    let notifications = DesktopNotifications::default();
    notifications.post(Notification::new("Tea is ready", "The timer finished"));
    let (mut frame, overlays) = chrome_over(Some(notifications.clone()));
    notifications.ask_centre(true);
    frame.relayout();
    assert!(overlays.is_open(Slot::Centre), "the centre is on the stack");
    assert!(notifications.centre_open(), "and the chrome shows it");
    assert_eq!(overlays.target(), Target::Surface(Slot::Centre));
    let place = overlays.place(Slot::Centre).expect("the stack places it");
    let x1 = DISPLAY.x1 - POPOVER_MARGIN;
    let top = f64::from(tilo_desktop::topbar::TOPBAR_HEIGHT_PX).ceil();
    assert_eq!(
        (place.x0, place.y0, place.x1),
        (x1 - POPOVER_WIDTH, top, x1),
        "under the bar, against the trailing edge, as wide as quick settings"
    );
    let inside = (place.center().x, place.y0 + 20.0);
    assert_eq!(
        overlays.press_outside(inside.0, inside.1),
        None,
        "a press on the panel is the panel's"
    );
    assert_eq!(
        overlays.press_outside(DESK_PRESS.0, DESK_PRESS.1),
        Some(Slot::Centre),
        "a press on the windows misses it"
    );
    overlays.dismiss(Slot::Centre);
    frame.relayout();
    assert!(
        !notifications.centre_open(),
        "the centre follows the stack down"
    );
    assert!(!overlays.is_open(Slot::Centre), "and stays down");
}
