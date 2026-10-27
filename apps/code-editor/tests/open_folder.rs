//! Open Folder asks the host's file picker for a folder to change, and the
//! folder the person picks becomes the project: its tree is in the sidebar.

#![allow(clippy::panic, reason = "in a test a panic is the assertion")]

use code_editor::CodeEditorShell;
use ui::framework::testing::TestApp;
use ui::portal::PortalOutcome;

/// A project folder with a source file and a folder in it.
fn project_folder() -> tempfile::TempDir {
    let folder = tempfile::tempdir().unwrap();
    std::fs::create_dir(folder.path().join("src")).unwrap();
    std::fs::write(folder.path().join("Cargo.toml"), "[package]\n").unwrap();
    std::fs::write(folder.path().join("src/main.rs"), "fn main() {}\n").unwrap();
    folder
}

/// The sidebar's Open folder, shown while no project is open.
const OPEN_FOLDER: &str = "empty-state:No folder open";

fn shows(app: &mut TestApp<CodeEditorShell>, name: &str) -> bool {
    app.harness()
        .inspection_json()
        .is_ok_and(|inspection| inspection.contains(name))
}

#[test]
fn open_folder_picks_a_folder_and_shows_its_tree() {
    ui::i18n::I18nManager::init("en-US");
    let (services, recorder) = ui::host::HostServices::recording();
    let _host = ui::host::enter(&services);
    let folder = project_folder();
    recorder.respond_to_pick(PortalOutcome::Opened(folder.path().to_path_buf()));

    let mut app = TestApp::<CodeEditorShell>::new().unwrap();
    app.settle();
    assert!(!shows(&mut app, "Cargo.toml"), "no project is open yet");
    app.harness()
        .click_named(OPEN_FOLDER)
        .unwrap_or_else(|error| panic!("the sidebar offers Open folder: {error}"));
    app.settle();

    let asked = recorder.pick_requests();
    let [request] = asked.as_slice() else {
        panic!("one click asks the picker once: {asked:?}");
    };
    assert!(
        request.is_directory() && request.is_writable_directory(),
        "Open folder asks for a folder to change: {request:?}"
    );
    tilo_test_env::wait_until("the picked folder's tree in the sidebar", || {
        app.settle();
        shows(&mut app, "Cargo.toml") && shows(&mut app, "src")
    });
}

#[test]
fn a_picker_that_cannot_open_is_told_in_plain_words() {
    ui::i18n::I18nManager::init("en-US");
    let (services, recorder) = ui::host::HostServices::recording();
    let _host = ui::host::enter(&services);
    let reason = "This app's workspace isn't the one on screen. Switch to it, then try again.";
    recorder.respond_to_pick(PortalOutcome::Failed(reason.into()));

    let mut app = TestApp::<CodeEditorShell>::new().unwrap();
    app.settle();
    app.harness().click_named(OPEN_FOLDER).unwrap();
    app.settle();

    let said = format!("Couldn't open a folder. {reason}");
    // Code says what it was doing, then what happened and what to do.
    tilo_test_env::wait_until(format!("Code to say {said:?}"), || {
        app.settle();
        shows(&mut app, &said)
    });
    assert!(!shows(&mut app, "File picker unavailable"));
}
