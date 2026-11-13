//! A phone's places screen: the sidebar in front of the list the window opens on.

use super::*;
use crate::gesture::NavDirection;

const W: u32 = 390;
const H: u32 = 844;

/// What takes Back's step out of the places: the shell's own chip, Escape, or the system's back
/// gesture.
#[derive(Clone, Copy, Debug)]
enum BackInput {
    Chip,
    Escape,
    Gesture,
}

fn go_back(h: &mut FrameHarness, input: BackInput) {
    match input {
        BackInput::Chip => {
            let back = geometry(h).back;
            assert!(
                back.width() > 0.0 && back.height() > 0.0,
                "the places offer a way back: {back:?}"
            );
            h.click_at(back.center().x, back.center().y);
        }
        BackInput::Escape => h.key("Escape"),
        BackInput::Gesture => assert!(
            h.nav_gesture(NavDirection::Back),
            "the places take the back gesture"
        ),
    }
    settle(h, W, H);
}

/// The places of a phone-wide window stand in front of the list the window opened on: their own
/// screen has one navigation affordance, Back, at the leading corner of its header, and it
/// takes the person to that list — its title starts after the chip rather than under it.
#[test]
fn a_phone_places_screen_goes_back_to_the_list_it_covered() {
    let _settled = crate::motion::testing::settled();
    for input in [BackInput::Chip, BackInput::Escape, BackInput::Gesture] {
        let mut h = harness(files_shell(), W, H, Theme::light());
        settle(&mut h, W, H);
        assert_eq!(
            h.root::<NavShell>().front_pane(),
            NavPane::Content,
            "{input:?}: the window opens on its folder"
        );
        h.root_mut::<NavShell>().show(NavPane::Sidebar);
        settle(&mut h, W, H);
        let shell = h.root::<NavShell>();
        assert_eq!(shell.front_pane(), NavPane::Sidebar, "{input:?}");
        assert!(shell.can_go_back(), "{input:?}: the places go back");
        assert!(
            rect_named(&h, "sidebar").is_some(),
            "{input:?}: the places are the screen"
        );
        assert_eq!(
            rect_named(&h, "nav-shell-sidebar"),
            None,
            "{input:?}: the places hold no second way to navigate"
        );
        let title = h
            .root::<NavShell>()
            .part(Part::Title(Column::Sidebar))
            .map(Widget::get_rect)
            .unwrap_or_else(|| panic!("{input:?}: the places' title"));
        let back = geometry(&h).back;
        assert!(
            title.x0 >= back.x1,
            "{input:?}: the title starts after the chip, not under it: {title:?} against {back:?}"
        );

        go_back(&mut h, input);
        assert_eq!(
            h.root::<NavShell>().front_pane(),
            NavPane::Content,
            "{input:?}: back reaches the folder the places covered"
        );
        assert_eq!(
            geometry(&h).back,
            Rect::ZERO,
            "{input:?}: the folder is where the window opened, with nothing to go back to"
        );
        assert!(
            rect_named(&h, "nav-shell-sidebar").is_some(),
            "{input:?}: it holds the way to the places again"
        );
    }
}

/// The list a phone-wide window opens on is home, and the places lead to the screen the person
/// last worked in rather than back to that list: a message read before the places were opened is
/// what Back returns to.
#[test]
fn the_places_go_back_to_the_screen_the_person_last_worked_in() {
    let _settled = crate::motion::testing::settled();
    let mut h = harness(mail_shell(), W, H, Theme::light());
    settle(&mut h, W, H);
    assert_eq!(
        geometry(&h).stack,
        [NavPane::Sidebar, NavPane::Overview, NavPane::Content]
    );
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Overview);
    let g = geometry(&h);
    assert_eq!(g.back, Rect::ZERO, "the list is where the window opened");
    assert!(
        g.toggle.width() > 0.0,
        "it holds the way to the places instead"
    );
    h.root_mut::<NavShell>().show(NavPane::Content);
    settle(&mut h, W, H);
    h.root_mut::<NavShell>().show(NavPane::Sidebar);
    settle(&mut h, W, H);
    assert_eq!(
        geometry(&h).back_column,
        Some(Column::Sidebar),
        "the places have the way back"
    );
    go_back(&mut h, BackInput::Chip);
    assert_eq!(
        h.root::<NavShell>().front_pane(),
        NavPane::Content,
        "back reaches the message, the screen the places covered"
    );
}

/// A window whose toolbar is its bar's keeps Back there: the places' screen offers it in the bar
/// and the sidebar's toggle stands aside for it, so the one leading control is the way back.
#[test]
fn a_places_screen_in_the_window_bar_offers_back_alone() {
    let _settled = crate::motion::testing::settled();
    let mut h = harness(
        NavShell::new()
            .content_kind(ContentKind::Reading)
            .sidebar(Probe::boxed("sidebar").0)
            .overview(Probe::boxed("overview").0)
            .content(Probe::boxed("content").0)
            .with_window_toolbar(),
        W,
        H,
        Theme::light(),
    );
    settle(&mut h, W, H);
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Overview);
    let toolbar = h.root::<NavShell>().window_toolbar();
    assert_eq!(toolbar.sidebar, Some(false), "the list offers the places");

    h.root_mut::<NavShell>().show(NavPane::Sidebar);
    settle(&mut h, W, H);
    let toolbar = h.root::<NavShell>().window_toolbar();
    assert_eq!(
        toolbar.sidebar, None,
        "the places hold no toggle: {toolbar:?}"
    );
    assert_eq!(
        toolbar.leading.len(),
        1,
        "one leading control, the way back: {toolbar:?}"
    );
    let event = crate::window_toolbar::ToolbarEvent::Action {
        id: WINDOW_BACK.into(),
    };
    assert!(h.root_mut::<NavShell>().window_toolbar_event(&event));
    settle(&mut h, W, H);
    assert_eq!(
        h.root::<NavShell>().front_pane(),
        NavPane::Overview,
        "the bar's Back returns to the list the places covered"
    );
}

/// A window that opens on its places — a page and its categories — keeps them a place with
/// nothing to go back to; the page chosen from them is what carries Back.
#[test]
fn a_window_that_opens_on_its_places_has_nothing_to_go_back_to() {
    let _settled = crate::motion::testing::settled();
    let mut h = harness(settings_shell(), W, H, Theme::light());
    settle(&mut h, W, H);
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Sidebar);
    assert_eq!(geometry(&h).back, Rect::ZERO, "no Back over the places");
    assert!(!h.root::<NavShell>().can_go_back());
    assert!(!h.root_mut::<NavShell>().back(), "Back leads nowhere");

    h.root_mut::<NavShell>().show(NavPane::Content);
    settle(&mut h, W, H);
    let back = geometry(&h).back;
    assert!(
        back.width() > 0.0,
        "the page shows the way back to the places: {back:?}"
    );
    go_back(&mut h, BackInput::Chip);
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Sidebar);
}

/// A sidebar floating over the window (a drawer) is put away by the same back gesture, before any
/// screen changes: a phone's gesture reaches the places however the window shows them.
#[test]
fn the_back_gesture_puts_a_floating_sidebar_away() {
    let _settled = crate::motion::testing::settled();
    let shell = NavShell::new()
        .content_kind(ContentKind::Document)
        .sidebar(Probe::boxed("sidebar").0)
        .content(Probe::boxed("content").0);
    let mut h = harness(shell, W, H, Theme::light());
    settle(&mut h, W, H);
    assert_eq!(geometry(&h).sidebar, SidebarPlacement::Drawer);
    assert!(!h.root::<NavShell>().is_sidebar_open());
    h.root_mut::<NavShell>().open_sidebar();
    settle(&mut h, W, H);
    assert!(h.root::<NavShell>().is_sidebar_open(), "the places show");
    let shell: &dyn Widget = h.root::<NavShell>();
    assert!(
        shell.nav_availability().back,
        "the gesture reaches the sidebar floating over the window"
    );
    assert!(h.nav_gesture(NavDirection::Back), "the gesture is taken");
    settle(&mut h, W, H);
    assert!(
        !h.root::<NavShell>().is_sidebar_open(),
        "and puts the sidebar away"
    );
}
