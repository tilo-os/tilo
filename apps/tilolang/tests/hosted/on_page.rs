//! A hosted window shown on a web page (`tilolang_app::shown_on_page`): the page a browser loads of
//! a person's Tasks window over a workspace they have used for years, what they do on it acted on
//! the view, and what an answer costs.

use tilolang_app::shown_on_page::ShownOnPage;

use super::cost::{opened_at_rest_in, seeded_tasks};
use super::*;

/// What a page's open, and an answer with nothing changed, may cost in bytes: the rows the
/// window lays out around what it shows, never the workspace's ten thousand.
const OPEN_BYTES: usize = 512 * 1024;

/// The element a press of `action` reaches first on `body`: the number its element carries, as
/// the page's script reads the nearest `data-p` around what was pressed, and the semantic target
/// it names.
fn first_acting(body: &str, action: &str) -> (u32, Option<String>) {
    let marker = format!("data-a=\"{action}\"");
    let at = body
        .find(&marker)
        .unwrap_or_else(|| panic!("the page offers a {action}"));
    let tag_start = body[..at].rfind('<').expect("the press is in a tag");
    let tag_end = at + body[at..].find('>').expect("the tag closes");
    let tag = &body[tag_start..tag_end];
    let attr = |text: &str, name: &str| {
        let key = format!("{name}=\"");
        let from = text.rfind(&key)? + key.len();
        Some(text[from..from + text[from..].find('"')?].to_owned())
    };
    let id = attr(tag, "data-p")
        .or_else(|| attr(&body[..tag_start], "data-p"))
        .and_then(|id| id.parse().ok())
        .expect("a numbered element holds the press");
    (id, attr(tag, "data-t"))
}

/// A person's window of ten thousand tasks opens as a page of the rows it shows: the page is
/// the window's own composition, and a page answers nothing when nothing changed.
#[test]
fn a_page_of_ten_thousand_tasks_carries_the_rows_it_shows() {
    let bus = seeded_tasks(SEEDED_ON_PAGE);
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    let mut session = opened_at_rest_in(&bus, &route);
    let shown_first = session
        .titles()
        .first()
        .cloned()
        .expect("the list shows a row");
    let mut page = ShownOnPage::new();

    let began = std::time::Instant::now();
    let opened = page.open(session.window.harness(), None);
    let open_took = began.elapsed();
    assert!(
        opened
            .body
            .contains(&ui::composition::page::escape(&shown_first)),
        "the page shows the list's first task, {shown_first:?}"
    );
    assert!(
        opened.body.len() < OPEN_BYTES,
        "the page carries the rows the window lays out, not the workspace's {SEEDED_ON_PAGE}: \
         {} bytes",
        opened.body.len()
    );
    assert!(!opened.css.is_empty(), "the page carries its theme");

    let began = std::time::Instant::now();
    let unchanged = page.changed(session.window.harness());
    let again_took = began.elapsed();
    assert!(
        unchanged.is_empty(),
        "nothing changed, so the page is answered nothing: {} patches",
        unchanged.len()
    );
    // Generous bounds a debug build on a busy machine holds; the bytes above are the property.
    assert!(
        open_took < std::time::Duration::from_secs(5)
            && again_took < std::time::Duration::from_secs(2),
        "a page of ten thousand tasks answers at once: open {open_took:?}, again {again_took:?}"
    );
}

/// The person checks a task off on the page: the press is the row's own semantic action, the
/// app's instance runs it, the workspace takes the change, and the page is answered the elements
/// that changed.
#[test]
fn a_task_checked_off_on_the_page_is_done_in_the_workspace() {
    let _settled = ui::motion::testing::settled();
    let surface = package(package_of(TASKS))
        .surface()
        .expect("the package has a surface");
    collab_types::app_surface::register_package(TASKS, &surface);
    let mut session = Session::tasks();
    choose(&mut session, "New task");
    session.window.type_text("Water the plants");
    session
        .window
        .press_key(ui::winit::keyboard::NamedKey::Enter);
    session.settle();
    let mut page = ShownOnPage::new();
    let opened = page.open(session.window.harness(), None);
    assert!(
        opened.body.contains("Mark Water the plants done"),
        "the page shows the task's box"
    );
    let (id, target) = first_acting(&opened.body, "toggle");

    let mut patches = page.act(
        session.window.harness(),
        id,
        target.as_deref(),
        &ui::a11y::Action::ToggleCheck,
    );
    session.settle();
    patches.extend(page.changed(session.window.harness()));
    assert!(
        super::task_in_workspace(&session, "Water the plants", true),
        "the press on the page marked the task done in the workspace"
    );
    assert!(
        patches.iter().any(|patch| patch.html.contains("checked")),
        "the page is answered the box, checked: {patches:?}"
    );
}

/// How many tasks the large page is opened over: what a person who has used Tasks for years
/// keeps.
const SEEDED_ON_PAGE: usize = 10_000;
