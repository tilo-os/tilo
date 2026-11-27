use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use tilo_ui_app::app::driver::Driver;
use tilo_ui_app::headless::*;
use tilo_ui_app::kurbo::*;
use tilo_ui_app::retained_present::*;
use tilo_ui_app::testing::*;
use tilo_ui_app::vello::*;
use tilo_ui_app::widget::context_menu_area::test_support::*;
use tilo_ui_app::widget::*;
use tilo_ui_base::a11y::*;
use tilo_ui_base::component::*;
use tilo_ui_base::i18n::*;
use tilo_ui_base::icons::*;
use tilo_ui_base::interaction::*;
use tilo_ui_base::paint::*;
use tilo_ui_base::presence::*;
use tilo_ui_base::synthetic_input::*;
use tilo_ui_base::text_selection::*;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::*;
use tilo_ui_base::widget::*;
use winit::event::*;
use winit::keyboard::*;
/// The other kind of entry: a row that is the widget's own declared action. An agent
/// activating it runs that action where the widget stands and the menu closes — the copy is
/// the link's own, not a press on the row.
#[test]
fn an_agent_chooses_a_widgets_own_action_from_its_row() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = tilo_ui_base::motion::testing::settled();
    use tilo_ui_app::clipboard::{ClipboardBackend as _, ClipboardTarget, InProcessClipboard};
    let clipboard = std::sync::Arc::new(InProcessClipboard::default());
    let services = tilo_ui_app::process::ProcessServices::null().with_clipboard(clipboard.clone());
    tilo_ui_app::process::scoped(&services, || {
        let area = ContextMenuArea::new(
            Box::new(
                tilo_ui_app::widget::Link::new("https://example.org/page").with_label("The page"),
            ),
            |_| {
                vec![MenuEntry::bound(
                    "Open",
                    tilo_ui_app::testing::bound_press("test:menu", "open"),
                )]
            },
        );
        let mut harness = tilo_ui_app::headless::Driver::for_root(
            tilo_ui_app::State::new(),
            area,
            (400, 120),
            tilo_ui_base::theme::Theme::dark(),
        );
        harness.relayout();
        let page = harness
            .root::<ContextMenuArea>()
            .content()
            .get_rect()
            .center();
        harness.send(&tilo_ui_base::synthetic_input::cursor_moved(page.x, page.y));
        right_click(&mut harness);
        let target = tilo_ui_base::a11y_child_route::encode(
            &[harness.root::<ContextMenuArea>().content().children().len()],
            Some("context-menu:item:1"),
        );
        harness
            .perform_semantic_named("Link", Some(&target), &tilo_ui_base::a11y::Action::Activate)
            .expect("the link's copy row is reachable");
        assert_eq!(
            clipboard.text(ClipboardTarget::Clipboard).as_deref(),
            Some("https://example.org/page"),
            "the row ran the link's own action where the link stands"
        );
        assert!(
            !harness.root::<ContextMenuArea>().is_open(),
            "the menu closed on its choice"
        );
    });
}

/// A 300 px area over an app whose record offers the four rows, in a window whose host
/// replays a still pointer after the widgets change, the pointer resting at (100, 100).
fn four_entries() -> (tilo_ui_app::headless::Driver, Chosen) {
    let chosen = Chosen::default();
    let content = tilo_ui_app::testing::MenuRows::new(&MENU_LABELS, Rc::clone(&chosen));
    let area = ContextMenuArea::new(Box::new(content), |_| {
        MENU_LABELS
            .into_iter()
            .map(|label| {
                MenuEntry::bound(
                    label,
                    tilo_ui_app::testing::bound_press(
                        tilo_ui_app::testing::MENU_ROW_RESOURCE,
                        &tilo_ui_app::testing::MenuRows::action_of(label),
                    ),
                )
            })
            .collect()
    });
    let mut harness = tilo_ui_app::app::driver::Driver::for_root(
        tilo_ui_app::State::new(),
        area,
        (400, 400),
        tilo_ui_base::theme::Theme::dark(),
    );
    harness.relayout();
    harness.send(&tilo_ui_base::synthetic_input::cursor_moved(100.0, 100.0));
    (harness, chosen)
}

fn mouse(harness: &mut tilo_ui_app::app::driver::Driver, state: ElementState, button: MouseButton) {
    harness.send(&tilo_ui_base::synthetic_input::mouse_input(state, button));
}

/// Right-click where the pointer rests, with the frame that rebuilds
/// the changed content and paints the opened menu landing between press
/// and release, as it does live.
fn right_click(harness: &mut tilo_ui_app::app::driver::Driver) {
    mouse(harness, ElementState::Pressed, MouseButton::Right);
    assert!(
        harness.root::<ContextMenuArea>().is_open(),
        "a right-click opens the menu"
    );
    harness.relayout();
    mouse(harness, ElementState::Released, MouseButton::Right);
    assert!(
        harness.root::<ContextMenuArea>().is_open(),
        "the release that ends the right-click leaves the menu open"
    );
}

/// The fourth entry is three rows down, as Canvas's Duplicate is: a
/// keyboard row one off lands on another entry.
#[test]
fn a_right_click_menu_runs_the_entry_the_keyboard_chooses() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut harness, chosen) = four_entries();
    right_click(&mut harness);
    assert_eq!(
        harness.root::<ContextMenuArea>().menu().hovered(),
        Some(0),
        "the menu opens on its first entry, though the still pointer was replayed"
    );
    for _ in 0..3 {
        harness.key("ArrowDown");
    }
    harness.key("Enter");
    assert_eq!(*chosen.borrow(), vec!["Fourth"]);
    assert!(!harness.root::<ContextMenuArea>().is_open());
}

#[test]
fn a_right_click_menu_runs_the_entry_the_pointer_clicks() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut harness, chosen) = four_entries();
    right_click(&mut harness);
    let second = harness
        .root::<ContextMenuArea>()
        .menu()
        .item_rect(1)
        .expect("the second entry is laid out")
        .center();
    harness.send(&tilo_ui_base::synthetic_input::cursor_moved(
        second.x, second.y,
    ));
    assert_eq!(
        harness.root::<ContextMenuArea>().menu().hovered(),
        Some(1),
        "hover follows the pointer once it moves"
    );
    mouse(&mut harness, ElementState::Pressed, MouseButton::Left);
    harness.relayout();
    mouse(&mut harness, ElementState::Released, MouseButton::Left);
    assert_eq!(*chosen.borrow(), vec!["Second"]);
    assert!(!harness.root::<ContextMenuArea>().is_open());
}

/// An open menu's entries are the typed actions behind them: the row names the app's action,
/// an agent chooses it by activating the row's node as a screen reader does, and the app's
/// action runs on its record and the menu closes. An agent's choice is the action, never a
/// press where the row is drawn.
#[test]
fn an_agent_chooses_a_menu_entry_through_the_action_its_row_names() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut harness, chosen) = four_entries();
    right_click(&mut harness);
    let address = harness
        .root::<ContextMenuArea>()
        .menu()
        .items()
        .get(2)
        .and_then(MenuItem::action_address)
        .expect("the third entry names the action it runs");
    assert_eq!(
        address,
        tilo_actions::ActionAddress::new(tilo_ui_app::testing::MENU_ROW_RESOURCE, "third")
    );
    assert_eq!(
        harness
            .root::<ContextMenuArea>()
            .menu()
            .items()
            .get(2)
            .and_then(MenuItem::label),
        Some("Third")
    );
    let record = harness
        .resources()
        .into_iter()
        .find(|resource| resource.id.0 == tilo_ui_app::testing::MENU_ROW_RESOURCE)
        .expect("the app publishes the record its rows act on");
    let third = record
        .actions
        .iter()
        .find(|action| action.id == "third")
        .expect("the third action");
    assert_eq!(third.kind, tilo_actions::OperationKind::Effect);
    // The row node an agent reaches, under the area the menu stands over.
    let target = tilo_ui_base::a11y_child_route::encode(
        &[harness.root::<ContextMenuArea>().content().children().len()],
        Some("context-menu:item:2"),
    );
    harness
        .perform_semantic_named(
            "MenuRows",
            Some(&target),
            &tilo_ui_base::a11y::Action::Activate,
        )
        .expect("the entry is reachable");
    assert_eq!(
        *chosen.borrow(),
        vec!["Third".to_owned()],
        "the app's action ran on its record"
    );
    assert!(
        !harness.root::<ContextMenuArea>().is_open(),
        "the menu closed on its choice"
    );
}

/// The reported capture's gesture: Menu/Shift+F10 on a focused row opens
/// its menu under that row, not at the list's foot.
#[test]
fn the_keyboard_opens_the_menu_under_the_focused_row() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = tilo_ui_base::motion::testing::settled();
    let anchor = Rect::new(20.0, 200.0, 300.0, 238.0);
    let area = ContextMenuArea::new(
        Box::new(RowWithTheKeyboard {
            inner: Spacer::fixed(300.0),
            anchor,
        }),
        |_| {
            vec![MenuEntry::bound(
                "First",
                tilo_ui_app::testing::bound_press("test:menu", "first"),
            )]
        },
    );
    let mut harness = tilo_ui_app::app::driver::Driver::for_root(
        tilo_ui_app::State::new(),
        area,
        (400, 400),
        tilo_ui_base::theme::Theme::dark(),
    );
    // The frame that gives the area the window its menu stands in.
    harness.relayout();
    harness.key("Shift+F10");
    assert!(
        harness.root::<ContextMenuArea>().is_open(),
        "Shift+F10 on the focused item opens its menu"
    );
    harness.relayout();
    let rect = harness.root::<ContextMenuArea>().menu().get_rect();
    let gap = f64::from(tilo_ui_base::tokens::Spacing::BASE.s1);
    assert!(
        rect.y0 >= anchor.y1 && rect.y0 - anchor.y1 <= gap + 0.5,
        "the menu stands under the row it opened from: {rect:?} vs {anchor:?}"
    );
}

/// A widget's own action is offered beside its owner's rows where the person right-clicks
/// it, and choosing it runs on the widget itself: a link's Copy link copies its address and
/// opens nothing.
#[test]
fn a_link_under_the_pointer_offers_its_copy_beside_the_owners_rows() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = tilo_ui_base::motion::testing::settled();
    use tilo_ui_app::clipboard::{ClipboardBackend as _, ClipboardTarget, InProcessClipboard};
    let clipboard = std::sync::Arc::new(InProcessClipboard::default());
    let services = tilo_ui_app::process::ProcessServices::null().with_clipboard(clipboard.clone());
    tilo_ui_app::process::scoped(&services, || {
        let area = ContextMenuArea::new(
            Box::new(
                tilo_ui_app::widget::Link::new("https://example.org/page").with_label("The page"),
            ),
            |_| {
                vec![MenuEntry::bound(
                    "Open",
                    tilo_ui_app::testing::bound_press("test:menu", "open"),
                )]
            },
        );
        let mut harness = tilo_ui_app::headless::Driver::for_root(
            tilo_ui_app::State::new(),
            area,
            (400, 120),
            tilo_ui_base::theme::Theme::dark(),
        );
        harness.relayout();
        let page = harness
            .root::<ContextMenuArea>()
            .content()
            .get_rect()
            .center();
        harness.send(&tilo_ui_base::synthetic_input::cursor_moved(page.x, page.y));
        right_click(&mut harness);
        let labels: Vec<String> = harness
            .root::<ContextMenuArea>()
            .menu()
            .items()
            .iter()
            .filter_map(MenuItem::label)
            .map(str::to_owned)
            .collect();
        assert_eq!(
            labels
                .iter()
                .take(2)
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["Open", "Copy link"],
            "the link's copy stands beside what its row offers"
        );
        let copy = harness
            .root::<ContextMenuArea>()
            .menu()
            .item_rect(1)
            .expect("the copy row is laid out")
            .center();
        harness.send(&tilo_ui_base::synthetic_input::cursor_moved(copy.x, copy.y));
        mouse(&mut harness, ElementState::Pressed, MouseButton::Left);
        harness.relayout();
        mouse(&mut harness, ElementState::Released, MouseButton::Left);
        assert_eq!(
            clipboard.text(ClipboardTarget::Clipboard).as_deref(),
            Some("https://example.org/page"),
            "choosing the copy put the address it goes to on the clipboard"
        );
        assert!(
            !harness.root::<ContextMenuArea>().is_open(),
            "the menu closed after its choice"
        );
    });
}

/// A widget that offers no action of its own adds no row: the menu is its owner's alone.
#[test]
fn a_widget_with_no_own_action_adds_no_row() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut harness, _) = four_entries();
    right_click(&mut harness);
    let labels: Vec<String> = harness
        .root::<ContextMenuArea>()
        .menu()
        .items()
        .iter()
        .filter_map(MenuItem::label)
        .map(str::to_owned)
        .collect();
    assert_eq!(
        labels
            .iter()
            .take(4)
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["First", "Second", "Third", "Fourth"]
    );
}

/// A row whose menu the host has not answered yet opens no menu: not even the rows the menu
/// adds itself — the one that asks what is typed, and the send row after it — which would
/// show a menu of nothing and never take the row's commands. It opens where it was asked
/// once the row comes.
#[test]
fn a_menu_waiting_for_its_row_opens_on_the_row_and_not_on_the_rows_it_adds() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = tilo_ui_base::motion::testing::settled();
    tilo_ui_app::shared_selection::set_send_host_available(true);
    let arrived = Rc::new(Cell::new(false));
    let area = ContextMenuArea::new(Box::new(Spacer::fixed(300.0)), {
        let arrived = Rc::clone(&arrived);
        move |_| {
            if arrived.get() {
                vec![MenuEntry::bound(
                    "Delete",
                    tilo_ui_app::testing::bound_press("test:menu", "delete"),
                )]
            } else {
                Vec::new()
            }
        }
    })
    .waiting_on({
        let arrived = Rc::clone(&arrived);
        move |_, _| !arrived.get()
    });
    let mut harness = tilo_ui_app::app::driver::Driver::for_root(
        tilo_ui_app::State::new(),
        area,
        (400, 400),
        tilo_ui_base::theme::Theme::dark(),
    );
    harness.relayout();
    let at = Point::new(100.0, 100.0);
    let press =
        tilo_ui_base::synthetic_input::mouse_input(ElementState::Pressed, MouseButton::Right);
    tilo_ui_app::widget::context_menu::note_input(&press);
    harness.send(&tilo_ui_base::synthetic_input::cursor_moved(at.x, at.y));
    harness.send(&press);
    assert!(
        !harness.root::<ContextMenuArea>().is_open(),
        "the menu waits for the row rather than opening on the rows it adds itself"
    );
    // The app answers with the row, and the frame that lays it out opens the menu on it.
    arrived.set(true);
    harness.relayout();
    let area = harness.root::<ContextMenuArea>();
    assert!(area.is_open(), "the menu opened on the row that came");
    assert_eq!(
        area.menu().items().first().and_then(MenuItem::label),
        Some("Delete")
    );
    tilo_ui_app::shared_selection::set_send_host_available(false);
}
