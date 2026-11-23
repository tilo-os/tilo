//! A press on a row that presents a resource opens it: its type's default view beside the list
//! the row is in, or, for a type its app gives no view of, the platform's page of the resource.
//! Both are the window's detail level, in the app's plain window with no session
//! (`tilolang dev`'s) — the same one path a session's window takes.

use tilolang_process::window::app::TlApp;
use ui::framework::testing::TestApp;

const DESKTOP: (u32, u32) = (1280, 800);

fn staged() -> TestApp<TlApp> {
    crate::support::installed("openby");
    let mut app =
        TestApp::<TlApp>::capture_at(Some("two books"), DESKTOP).expect("the scenario stages");
    settle(&mut app);
    app
}

fn settle(app: &mut TestApp<TlApp>) {
    crate::change_marks::captured(app, 8);
}

/// Every text the window says, as a screen reader hears it.
fn said(app: &mut TestApp<TlApp>) -> Vec<String> {
    crate::change_marks::said(app)
}

/// The middle of the row whose words are `title`, as the window draws it.
fn row_at(app: &mut TestApp<TlApp>, title: &str) -> (f64, f64) {
    let snapshot = app.harness().inspection();
    let all = snapshot.root.nodes();
    let row = all
        .into_iter()
        .filter(|node| node.widget == "ListRow")
        .find(|node| {
            node.semantics
                .as_ref()
                .and_then(|access| access.name.as_deref())
                .is_some_and(|name| name.contains(title))
        })
        .unwrap_or_else(|| panic!("the window shows a row `{title}`"));
    let rect = row.rect;
    (
        f64::midpoint(rect[0], rect[0] + rect[2]),
        f64::midpoint(rect[1], rect[1] + rect[3]),
    )
}

/// The press shows nothing of the app failing, whatever it opened.
fn nothing_failed(said: &[String]) {
    assert!(
        !said.iter().any(|text| text.contains("could not be opened")),
        "a press in an app never says the app could not be opened: {said:?}"
    );
}

#[test]
fn a_press_on_a_row_opens_its_item_in_the_window() {
    let mut app = staged();
    assert!(
        !said(&mut app).iter().any(|text| text == "Author"),
        "the book's page is not shown before a press"
    );
    let (x, y) = row_at(&mut app, "Piranesi");
    app.harness().click_at(x, y);
    settle(&mut app);
    let said = said(&mut app);
    assert!(
        said.iter().any(|text| text == "Author"),
        "the press opens the book's default view beside the list: {said:?}"
    );
    nothing_failed(&said);
}

#[test]
fn a_press_on_a_row_of_a_type_with_no_view_shows_the_item() {
    let mut app = staged();
    let (x, y) = row_at(&mut app, "Favourite");
    app.harness().click_at(x, y);
    settle(&mut app);
    let said = said(&mut app);
    nothing_failed(&said);
    assert!(
        said.iter().any(|text| text == "Label"),
        "the page says what the item is: {said:?}"
    );
    // Its facts, a label beside each value, as a reader hears them.
    let facts: Vec<(String, String)> = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .filter_map(|(_, node)| Some((node.label()?.to_owned(), node.value()?.to_owned())))
        .collect();
    assert!(
        facts.contains(&("Note".to_owned(), "Dog-eared".to_owned())),
        "the page says what the item holds, field by field: {facts:?}"
    );
    assert!(
        said.iter().any(|text| text == "Back"),
        "the page is a step away from the list, as every page of the window is: {said:?}"
    );
}
