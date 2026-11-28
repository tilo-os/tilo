//! A hosted window shown on a web page (`tilolang_process::window::shown_on_page`): the page a browser loads of
//! a person's Tasks window over a workspace they have used for years, what they do on it acted on
//! the view, and what an answer costs.

use tilolang_process::window::shown_on_page::ShownOnPage;

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
    let before = body.get(..at).expect("the press starts on a character");
    let tag_start = before.rfind('<').expect("the press is in a tag");
    let tag_end = at
        + body
            .get(at..)
            .and_then(|rest| rest.find('>'))
            .expect("the tag closes");
    let tag = body
        .get(tag_start..tag_end)
        .expect("the tag is whole characters");
    let attr = |text: &str, name: &str| {
        let key = format!("{name}=\"");
        let value = text.get(text.rfind(&key)? + key.len()..)?;
        Some(value.get(..value.find('"')?)?.to_owned())
    };
    let id = attr(tag, "data-p")
        .or_else(|| attr(before.get(..tag_start)?, "data-p"))
        .and_then(|id| id.parse().ok())
        .expect("a numbered element holds the press");
    (id, attr(tag, "data-t"))
}

/// A person's window of ten thousand tasks opens as a page of the rows it shows, asked over the
/// page link a device holds to the window: the page is the window's own composition, and a page
/// answers nothing when nothing changed.
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
    // The device's end of the page link asks from its own thread; the window answers on this
    // one, as the window process does, until the device lets go.
    let (device, window) = std::os::unix::net::UnixStream::pair().expect("a page link");
    let asking = std::thread::spawn(move || {
        let mut page = tilo_server_protocol::page::window::WindowLink::over(device);
        let began = std::time::Instant::now();
        let (_, css, body) = page.open(None).expect("the window answers the page");
        let open_took = began.elapsed();
        let began = std::time::Instant::now();
        let unchanged = page.changed().expect("the window answers what changed");
        (css, body, open_took, unchanged, began.elapsed())
    });
    let mut link = window;
    tilolang_process::window::shown_on_page::serve(session.window.harness(), &mut link)
        .expect("the window answers its page link");
    let (css, body, open_took, unchanged, again_took) =
        asking.join().expect("the device's asks finish");

    assert!(
        body.contains(&ui::composition::page::escape(&shown_first)),
        "the page shows the list's first task, {shown_first:?}"
    );
    assert!(
        body.len() < OPEN_BYTES,
        "the page carries the rows the window lays out, not the workspace's {SEEDED_ON_PAGE}: \
         {} bytes",
        body.len()
    );
    assert!(!css.is_empty(), "the page carries its theme");

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
        ui::composition::page::PageAction::Toggle,
    );
    session.settle();
    patches.extend(page.changed(session.window.harness()));
    assert!(
        super::task_in_workspace(&session, "Water the plants", true),
        "the press on the page marked the task done in the workspace"
    );
    assert!(
        patches
            .iter()
            .any(|patch| matches!(patch, ui::composition::page::Patch::Replace { html, .. } | ui::composition::page::Patch::Insert { html, .. } if html.contains("checked"))),
        "the page is answered the box, checked: {patches:?}"
    );
}

/// The numbered element a press on the page reaches for the control showing `words`, and the
/// semantic target it names: a menu button shows its words on the bar or in a row.
fn pressable_words(body: &str, words: &str) -> (u32, Option<String>) {
    let at = body
        .find(&format!(">{words}<"))
        .unwrap_or_else(|| panic!("the page shows {words}"));
    let before = body.get(..at).expect("the words start on a character");
    let tag_start = before.rfind('<').expect("the words are in a tag");
    let tag_end = at
        + body
            .get(at..)
            .and_then(|rest| rest.find('>'))
            .expect("the tag closes");
    let tag = body
        .get(tag_start..tag_end)
        .expect("the tag is whole characters");
    let attr = |text: &str, name: &str| {
        let key = format!("{name}=\"");
        let value = text.get(text.rfind(&key)? + key.len()..)?;
        Some(value.get(..value.find('"')?)?.to_owned())
    };
    let id = attr(tag, "data-p")
        .and_then(|id| id.parse().ok())
        .expect("a numbered element holds the control");
    (id, attr(tag, "data-t"))
}

/// A menu opened by a press on the page stands in that press's own answer: the rows it lists
/// are elements the page can number, drawn on the layer over the window as the window shows
/// them.
#[test]
fn a_menu_a_press_opens_on_the_page_is_in_that_answer() {
    let _settled = ui::motion::testing::settled();
    let surface = package(package_of(TASKS))
        .surface()
        .expect("the package has a surface");
    collab_types::app_surface::register_package(TASKS, &surface);
    let mut session = Session::tasks();
    choose(&mut session, "New task");
    let said = "Water the plants";
    session.window.type_text(said);
    session
        .window
        .press_key(ui::winit::keyboard::NamedKey::Enter);
    session.settle();
    // The task's own page, where its menus stand.
    let anchor = {
        let host = session.instance_host();
        let root = host.root_of(TASKS).expect("the root");
        let items = match host.runtime().read(&root, "items") {
            Ok(tilolang_tilo::runtime::Value::List(items)) => items,
            other => panic!("the workspace lists its tasks: {other:?}"),
        };
        items
            .into_iter()
            .find_map(|item| match item {
                tilolang_tilo::runtime::Value::Ref(item)
                    if matches!(
                        host.runtime().read(&item.target, "title"),
                        Ok(tilolang_tilo::runtime::Value::Text(title)) if title == said
                    ) =>
                {
                    Some(format!("tl:{TASKS}:{}", item.target.id_string()))
                }
                _ => None,
            })
            .expect("the task the list shows")
    };
    session.instance_mut().reveal(anchor);
    session.settle();
    let mut page = ShownOnPage::new();
    let opened = page.open(session.window.harness(), None);
    assert!(
        !opened.body.contains("No estimate"),
        "the menu stands closed on the page"
    );
    let (id, target) = pressable_words(&opened.body, "Estimate");
    let patches = page.act(
        session.window.harness(),
        id,
        target.as_deref(),
        ui::composition::page::PageAction::Activate,
    );
    let answered: String = patches
        .iter()
        .filter_map(|patch| match patch {
            ui::composition::page::Patch::Replace { html, .. }
            | ui::composition::page::Patch::Insert { html, .. } => Some(html.as_str()),
            ui::composition::page::Patch::Remove { .. } => None,
        })
        .collect();
    assert!(
        answered.contains("No estimate"),
        "the menu a press opened stands in that press's answer: {answered}"
    );
}

/// How many tasks the large page is opened over: what a person who has used Tasks for years
/// keeps.
const SEEDED_ON_PAGE: usize = 10_000;
