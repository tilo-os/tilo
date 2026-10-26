//! The Add feed dialog through the real host loop: its address field takes
//! typed text after the host lays out between every event.

use ui::framework::testing::TestApp;
use ui::headless::FrameHarness;
use ui::window_toolbar::ToolbarEvent;

fn value_of(harness: &FrameHarness, name: &str) -> Option<String> {
    harness
        .controls()
        .into_iter()
        .find(|control| control.name.as_deref() == Some(name))
        .and_then(|control| control.value)
}

#[test]
fn the_add_feed_field_takes_typed_text() {
    let mut app = TestApp::<feeds::FeedsShell>::new().unwrap();
    let harness = app.harness();
    harness.present_in_window_bar();
    harness.relayout();
    // Add subscription is the window bar's.
    harness.press_window_slot(ToolbarEvent::Action {
        id: "feeds.add".into(),
    });
    harness.relayout();
    assert!(
        value_of(harness, "Feed URL").is_some(),
        "the dialog opened with its field"
    );
    // A browser page delivers text only to a focused field it can place its
    // input-method editor over.
    assert!(
        harness.ime_window().is_some(),
        "the field holds the host's focus: {:?}",
        harness.focused_access_node()
    );
    // A page's input-method editor commits the text.
    harness.commit_text("https://example.com/feed");
    assert_eq!(
        value_of(harness, "Feed URL").as_deref(),
        Some("https://example.com/feed")
    );
}

/// A guest's New folder in the sidebar opens its dialog at once: it opens
/// something here and reaches nothing outside, so the workspace asks
/// nothing first.
#[test]
fn a_guests_new_folder_opens_its_dialog_unasked() {
    let mut app = TestApp::<feeds::FeedsShell>::new().unwrap();
    app.harness().relayout();
    app.set_role(ui::permissions::Role::Editor);
    app.harness().present_in_window_bar();
    app.harness().relayout();
    let new_folder = app
        .harness()
        .controls()
        .into_iter()
        .find(|control| control.name.as_deref() == Some("New folder Feeds"))
        .expect("the sidebar's Feeds heading holds New folder")
        .id;
    app.harness()
        .operate_control(&new_folder, &ui::headless::ControlInput::Press)
        .expect("New folder takes a press");
    app.settle();
    app.harness().relayout();
    assert!(
        app.asking().is_none(),
        "New folder asks nothing: {:?}",
        app.harness().asking_why()
    );
    assert!(
        app.harness()
            .controls()
            .iter()
            .any(|control| control.name.as_deref() == Some("Create folder")),
        "the New folder dialog opened: {:?}",
        app.harness().control_failures()
    );
}

/// What the open search lists, by name.
fn listed(harness: &FrameHarness) -> Vec<String> {
    harness
        .a11y_nodes()
        .into_iter()
        .filter(|(_, node)| node.role() == ui::accesskit::Role::ListItem)
        .filter_map(|(_, node)| node.label().map(str::to_owned))
        .collect()
}

/// Search in Feeds offers what the app does as well as what it holds, as
/// the window's own palette does: with nothing typed it lists the app's
/// actions beside its feeds, and a typed word finds an action too.
#[test]
fn search_offers_the_apps_actions_beside_what_it_holds() {
    let mut app = TestApp::<feeds::FeedsShell>::capture(Some("detail")).unwrap();
    let harness = app.harness();
    harness.relayout();
    harness
        .open_search(&ui::search::SearchRequest::this_app())
        .expect("search opens");
    harness.relayout();
    let open = listed(harness);
    assert!(
        open.iter().any(|name| name == "Mark all articles as read"),
        "the app's actions are offered: {open:?}"
    );
    assert!(
        open.iter().any(|name| name == "Rust Blog"),
        "and what it holds: {open:?}"
    );
    harness.key("Escape");
    harness.relayout();
    harness
        .open_search(&ui::search::SearchRequest::this_app().with_query("mark all"))
        .expect("search opens again");
    harness.relayout();
    let typed = listed(harness);
    assert!(
        typed.iter().any(|name| name == "Mark all articles as read"),
        "a typed word finds an action: {typed:?}"
    );
}
