//! Removing a book from the library is asked first, because Undo puts the
//! row back but not this device's access to the book's file. Nothing is
//! forgotten until the person agreed: a cancelled removal leaves the book on
//! the shelf, opening from its file, and a confirmed one forgets the file,
//! through every door.

use std::path::{Path, PathBuf};

use ui::framework::testing::TestApp;

use super::{Action, BooksApp, View};
use crate::EbookShell;

fn books(app: &mut TestApp<EbookShell>) -> &mut BooksApp {
    app.surface_mut::<BooksApp>()
}

/// A fresh library with the sample book dropped on its window from a file
/// in `dir`, as a person adds one, with its workspace in `dir` too: the file
/// it opens from is this device's grant. Answers the window, the book's id
/// and the workspace's setting, held for as long as the window runs.
fn with_dropped_book(dir: &Path) -> (TestApp<EbookShell>, String, tilo_test_env::ScopedEnv) {
    let workspace = dir.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_WORKSPACE_DIR", &workspace);
    let path = dir.join("dropped.epub");
    std::fs::write(&path, crate::content::SAMPLE_EPUB).unwrap();
    let mut app = TestApp::<EbookShell>::new().unwrap();
    books(&mut app).query = super::query_view::BookQuery::hermetic();
    app.harness()
        .send(&winit::event::WindowEvent::DroppedFile(path));
    let id = crate::epub::content_id(crate::content::SAMPLE_EPUB);
    let chapter = crate::content::bundled_sample().unwrap().chapters[0]
        .title
        .clone();
    assert!(
        app.harness()
            .wait_until(tilo_test_env::hang_bound(), |harness| harness
                .a11y_node_named(&chapter)
                .is_some()),
        "the dropped book opens in the reader"
    );
    assert_eq!(app.state().entries.len(), 1, "it joined the library");
    let granted = grant(&app, &id).expect("this device opens it from its file");
    assert!(granted.is_file(), "{granted:?} is the book's file");
    (app, id, env)
}

/// Where this device opens the book `id` from, as its device-local settings
/// keep it: `None` once it forgot the book's file.
fn grant(app: &TestApp<EbookShell>, id: &str) -> Option<PathBuf> {
    let stored = app.settings::<crate::local_library::Stored>();
    crate::local_library::LocalLibrary::load(stored)
        .books
        .get(id)
        .cloned()
}

/// Activate the control the shelf or the reader publishes as `target`, as
/// assistive technology and a pointer both reach it.
fn activate(app: &mut TestApp<EbookShell>, target: &str) {
    app.harness()
        .perform_named(target, &ui::a11y::Action::Activate)
        .unwrap();
    app.settle();
}

/// Leave the reader for the shelf with the window bar's Back, as the
/// desktop hands the press to the app.
fn back_to_library(app: &mut TestApp<EbookShell>) {
    app.harness()
        .press_window_slot(ui::window_toolbar::ToolbarEvent::Back);
    app.settle();
}

/// Press the remove button on the cover of the book `id`, which shows while
/// the pointer is over its card on the shelf.
fn press_remove(app: &mut TestApp<EbookShell>, id: &str) {
    if books(app).view != View::Library {
        back_to_library(app);
    }
    app.harness().relayout();
    let books = books(app);
    let card = books
        .hits
        .iter()
        .find(|hit| match hit.action {
            Action::OpenBook(index) => books.shelf_book(index).is_some_and(|book| book.id == id),
            _ => false,
        })
        .map(|hit| hit.rect)
        .expect("the book's card is on the shelf");
    let centre = card.center();
    app.harness().send(&winit::event::WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(centre.x, centre.y),
    });
    app.harness().relayout();
    activate(app, &format!("books.book.remove:{id}"));
}

/// The book `id` opens in the reader from the shelf of a window opened
/// again, which reads it from what this device kept.
fn opens_after_reopening(app: TestApp<EbookShell>, id: &str) -> TestApp<EbookShell> {
    let mut app = app.reopen().unwrap();
    books(&mut app).query = super::query_view::BookQuery::hermetic();
    app.settle();
    assert!(
        books(&mut app).library.iter().any(|book| book.id == id),
        "the book is still here to open: {:?}",
        books(&mut app).load_error
    );
    if books(&mut app).view != View::Library {
        back_to_library(&mut app);
    }
    app.harness().relayout();
    activate(&mut app, &format!("books.book.open:{id}"));
    let books = books(&mut app);
    assert_eq!(books.view, View::Reader, "it opens");
    assert_eq!(
        books.open_book().map(|book| book.id.clone()),
        Some(id.to_owned())
    );
    app
}

#[test]
#[serial_test::serial]
fn a_cancelled_removal_leaves_the_book_opening_and_a_confirmed_one_forgets_its_file() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, id, _workspace) = with_dropped_book(dir.path());

    press_remove(&mut app, &id);
    assert!(app.asking().is_some(), "removing asks first");
    assert!(
        grant(&app, &id).is_some(),
        "nothing is forgotten while the person is asked"
    );
    app.harness().key("Escape");
    app.settle();
    assert!(app.asking().is_none(), "the person said no");
    assert_eq!(app.state().entries.len(), 1, "the book stays on the shelf");
    assert!(
        grant(&app, &id).is_some(),
        "and this device still opens it from its file"
    );
    let mut app = opens_after_reopening(app, &id);

    // A watched-folder entry of the book, and one of another book.
    let watched = PathBuf::from("/watched/removed.epub");
    let other = PathBuf::from("/watched/other.epub");
    books(&mut app)
        .watched_sources
        .insert(watched.clone(), id.clone());
    books(&mut app)
        .watched_sources
        .insert(other.clone(), "book:other".into());

    press_remove(&mut app, &id);
    assert!(app.confirm_asked().is_some(), "the person agreed");
    assert!(app.state().entries.is_empty(), "the book left the library");
    assert!(books(&mut app).library.is_empty(), "and the shelf");
    assert_eq!(grant(&app, &id), None, "this device forgot its file");
    assert!(
        !books(&mut app).watched_sources.contains_key(&watched),
        "its watched file may be found again"
    );
    assert!(
        books(&mut app).watched_sources.contains_key(&other),
        "another watched book stays known"
    );
}

#[test]
#[serial_test::serial]
fn removing_through_the_action_api_forgets_the_file_only_once_confirmed() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, id, _workspace) = with_dropped_book(dir.path());
    let remove = || serde_json::json!({ "book": id });

    assert!(
        app.call("remove-from-library", remove()).is_err(),
        "an unconfirmed removal is refused"
    );
    app.settle();
    assert_eq!(app.state().entries.len(), 1, "the book stays on the shelf");
    assert!(
        grant(&app, &id).is_some(),
        "and this device still opens it from its file"
    );
    let mut app = opens_after_reopening(app, &id);

    app.call_confirmed("remove-from-library", remove()).unwrap();
    app.settle();
    assert!(app.state().entries.is_empty(), "the book left the library");
    assert!(books(&mut app).library.is_empty(), "and the shelf");
    assert_eq!(grant(&app, &id), None, "this device forgot its file");
}
