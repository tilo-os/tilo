//! The macOS title bar and menus a framework History app gets: its recent changes, and search
//! from the field and the Edit menu.
use std::cell::RefCell;
use std::rc::Rc;

use tilo_ui::objc2::{MainThreadMarker, msg_send};
use tilo_ui::objc2_app_kit::NSMenu;

use tilo_ui::host_chrome_macos_testing::{ChromeRequest, MacChrome};

use tilo_ui::app::testing;

fn titles(menu: &NSMenu) -> Vec<String> {
    (0..menu.numberOfItems())
        .filter_map(|index| menu.itemAtIndex(index))
        .map(|item| item.title().to_string())
        .collect()
}

fn submenu(bar: &NSMenu, title: &str) -> tilo_ui::objc2::rc::Retained<NSMenu> {
    bar.itemWithTitle(&tilo_ui::objc2_foundation::NSString::from_str(title))
        .and_then(|item| item.submenu())
        .unwrap_or_else(|| panic!("{title} menu"))
}

#[test]
fn framework_history_menu_lists_recent_changes_and_opens_at_the_chosen_one() {
    // SAFETY: as above; the menus never leave this thread.
    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    let mut app = framework_history_tests::native_chrome_app();

    let chosen: Rc<RefCell<Vec<ChromeRequest>>> = Rc::default();
    let sink = Rc::clone(&chosen);
    let chrome = MacChrome::new(mtm, move |request| sink.borrow_mut().push(request));
    chrome.update(app.host_commands_for_test(true));

    let history = submenu(&chrome.menu_bar, "History");
    let items = titles(&history);
    assert_eq!(items[0], "Show History");
    assert!(items[1].is_empty(), "recent changes follow a separator");
    assert_eq!(items.len(), 4, "two recent changes: {items:?}");
    assert!(
        items[2..].iter().all(|title| title.contains(" by ")),
        "a change names who made it: {items:?}"
    );

    let newest = history.itemAtIndex(2).unwrap();
    let target = &*chrome.target;
    let enabled: bool = unsafe { msg_send![target, validateMenuItem: &*newest] };
    assert!(enabled);
    let _: () = unsafe { msg_send![target, performHostCommand: &*newest] };
    let Some(ChromeRequest::Command(invocation)) = chosen.borrow_mut().pop() else {
        panic!("choosing a recent change dispatches its command");
    };
    assert!(app.invoke_host_command_for_test(&invocation));
    framework_history_tests::layout(&mut app);
    chrome.update(app.host_commands_for_test(true));
    assert_eq!(
        titles(&submenu(&chrome.menu_bar, "History"))[0],
        "Hide History"
    );
}

#[test]
fn framework_history_apps_search_from_the_title_bar_and_the_edit_menu() {
    // SAFETY: as above; the menus never leave this thread.
    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    let mut app = framework_history_tests::native_chrome_app();
    let chosen: Rc<RefCell<Vec<ChromeRequest>>> = Rc::default();
    let sink = Rc::clone(&chosen);
    let chrome = MacChrome::new(mtm, move |request| sink.borrow_mut().push(request));
    chrome.update(app.host_commands_for_test(false));
    assert!(titles(&submenu(&chrome.menu_bar, "Edit")).contains(&"Search…".to_owned()));

    let field = tilo_ui::objc2_app_kit::NSSearchField::new(mtm);
    field.setStringValue(&tilo_ui::objc2_foundation::NSString::from_str("notes"));
    let target = &*chrome.target;
    let _: () = unsafe { msg_send![target, searchFieldDidStartSearching: &*field] };
    let Some(ChromeRequest::Search(query)) = chosen.borrow_mut().pop() else {
        panic!("the title-bar field hands its query over");
    };
    assert_eq!(query, "notes");
    assert!(
        app.invoke_host_shortcut_for_test(tilo_ui_base::platform::HostShortcut::Search),
        "the palette opens"
    );
}
