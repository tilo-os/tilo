//! A person's file chooser in the window: the page's own chooser, asked of
//! the host's picker and answered with exactly what the person picked
//! ([`crate::page_files`]). The host here records what it is asked, so no
//! chooser opens on anybody's screen.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ui::host::HostServices;
use ui::portal::{MultiOutcome, PortalOutcome};
use ui::tilo_actions::ResourceSpec;
use ui::webview::{FileChooser, FileChooserMode, WebViewEngine as _};

use super::tests::synced;
use super::*;

/// A window in `mode` on a page.
fn window(mode: BrowsingMode) -> (Arc<TestEngine>, BrowserWidget) {
    let engine = TestEngine::new();
    engine.send(WebViewCommand::Navigate(
        "https://visa.example/apply".into(),
    ));
    let mut widget = BrowserWidget::with_engine_mode(engine.clone(), true, mode);
    let _ = synced(&mut widget);
    (engine, widget)
}

/// The engine says a person opened the page's chooser `id`.
fn asks(engine: &TestEngine, id: u32, mode: FileChooserMode, accept: &[&str]) {
    engine.push_event(WebViewEvent::FileChooser(FileChooser {
        id,
        mode,
        title: String::new(),
        accept: accept
            .iter()
            .map(|accepted| (*accepted).to_owned())
            .collect(),
    }));
}

/// Every answer the engine was sent: the chooser, and the paths.
fn answers(engine: &TestEngine) -> Vec<(u32, Vec<String>)> {
    engine
        .commands()
        .into_iter()
        .filter_map(|command| match command {
            WebViewCommand::AnswerFileChooser { id, paths } => Some((id, paths)),
            _ => None,
        })
        .collect()
}

/// Files a person could pick: each exists, as a picker's answer does.
fn files(names: &[&str]) -> (tempfile::TempDir, Vec<PathBuf>) {
    let folder = tempfile::tempdir().unwrap();
    let paths = names
        .iter()
        .map(|name| {
            let path = folder.path().join(name);
            std::fs::write(&path, b"%PDF-1.4").unwrap();
            path
        })
        .collect();
    (folder, paths)
}

fn named(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Everything `resource` and the rows under it say and offer: their ids,
/// names and previews, and their actions.
fn said(resource: &ResourceSpec, into: &mut Vec<String>) {
    into.push(resource.id.as_str().to_owned());
    into.push(resource.label.clone());
    into.extend(
        resource
            .presentation
            .as_ref()
            .and_then(|shown| shown.preview.clone()),
    );
    for action in &resource.actions {
        into.push(action.id.clone());
        into.push(action.label.clone());
    }
    for child in &resource.children {
        said(child, into);
    }
}

#[test]
fn a_persons_chooser_asks_the_hosts_picker_and_the_page_gets_the_one_file_picked() {
    let (services, recorder) = HostServices::recording();
    let _host = ui::host::enter(&services);
    let (engine, mut widget) = window(BrowsingMode::Normal);
    assert!(
        engine.accepts_file_choosers(),
        "a person's choosers come to their own window"
    );
    let (_folder, picked) = files(&["passport.pdf"]);
    recorder.respond_to_pick(PortalOutcome::Opened(picked[0].clone()));

    asks(&engine, 4, FileChooserMode::One, &[".pdf", "image/*"]);
    widget.apply_engine_events();

    let requests = recorder.pick_requests();
    assert_eq!(requests.len(), 1, "the host's picker is asked once");
    let request = &requests[0];
    assert!(request.is_open() && !request.is_multiple());
    assert_eq!(
        (request.title(), request.accept_label()),
        ("Choose a file", "Choose")
    );
    let filters = request.filters();
    assert_eq!(filters[0].globs(), ["*.pdf"], "what the input accepts");
    assert_eq!(filters[0].mimetypes(), ["image/*"]);
    assert_eq!(
        filters.last().unwrap().globs(),
        ["*"],
        "and every file, so nobody is kept from the one they came for"
    );
    assert_eq!(
        answers(&engine),
        [(4, vec![named(&picked[0])])],
        "exactly the file picked reaches the page"
    );
    // What the person picked is the page's alone: the window publishes
    // nothing of it for anyone else to read or answer.
    let mut published = Vec::new();
    said(&widget.published(), &mut published);
    assert!(
        !published
            .iter()
            .any(|said| said.contains("passport.pdf") || said.contains("chooser")),
        "{published:?}"
    );
    // Answered once.
    widget.apply_engine_events();
    assert_eq!(answers(&engine).len(), 1);
    assert_eq!(recorder.pick_requests().len(), 1);
}

#[test]
fn several_files_and_a_folder_reach_the_page_as_they_were_picked() {
    let (services, recorder) = HostServices::recording();
    let _host = ui::host::enter(&services);
    let (engine, mut widget) = window(BrowsingMode::Normal);
    let (folder, picked) = files(&["two.png", "one.png", "three.png"]);
    recorder.respond_to_pick_multiple(MultiOutcome::Selected(picked.clone()));
    asks(&engine, 5, FileChooserMode::Several, &[]);
    widget.apply_engine_events();
    let requests = recorder.pick_requests();
    assert!(requests[0].is_open() && requests[0].is_multiple());
    assert_eq!(requests[0].title(), "Choose files");
    assert_eq!(
        requests[0].filter_names().len(),
        1,
        "an input that accepts anything offers every file, once"
    );
    assert_eq!(
        answers(&engine),
        [(5, picked.iter().map(|path| named(path)).collect())],
        "every file picked, in the order picked, and no other"
    );

    recorder.respond_to_pick(PortalOutcome::Opened(folder.path().to_path_buf()));
    asks(&engine, 6, FileChooserMode::Folder, &[]);
    widget.apply_engine_events();
    let request = &recorder.pick_requests()[1];
    assert!(
        request.is_directory() && !request.is_writable_directory(),
        "a page is given a folder to read, never to write"
    );
    assert_eq!(request.title(), "Choose a folder");
    assert_eq!(answers(&engine)[1], (6, vec![named(folder.path())]));
}

#[test]
fn a_cancelled_or_failed_picker_cancels_the_pages_chooser() {
    let (services, recorder) = HostServices::recording();
    let _host = ui::host::enter(&services);
    let (engine, mut widget) = window(BrowsingMode::Normal);
    // Nothing queued: the person closed the picker.
    asks(&engine, 7, FileChooserMode::One, &[]);
    widget.apply_engine_events();
    recorder.respond_to_pick(PortalOutcome::Failed(
        "The file chooser didn't open.".into(),
    ));
    asks(&engine, 8, FileChooserMode::One, &[]);
    widget.apply_engine_events();
    asks(&engine, 9, FileChooserMode::Several, &[]);
    widget.apply_engine_events();
    assert_eq!(recorder.pick_requests().len(), 3);
    assert_eq!(
        answers(&engine),
        [(7, vec![]), (8, vec![]), (9, vec![])],
        "the page is told nothing was chosen, and does not wait"
    );
}

#[test]
fn what_no_picker_could_have_given_is_never_handed_to_the_page() {
    let (services, recorder) = HostServices::recording();
    let _host = ui::host::enter(&services);
    let (engine, mut widget) = window(BrowsingMode::Normal);
    let (folder, picked) = files(&["passport.pdf"]);
    let missing = folder.path().join("missing.pdf");
    // A path that is not absolute, a file that is not there, a folder where
    // a file was asked for, and one unreadable file among good ones.
    recorder.respond_to_pick(PortalOutcome::Opened("passport.pdf".into()));
    asks(&engine, 1, FileChooserMode::One, &[]);
    widget.apply_engine_events();
    recorder.respond_to_pick(PortalOutcome::Opened(missing.clone()));
    asks(&engine, 2, FileChooserMode::One, &[]);
    widget.apply_engine_events();
    recorder.respond_to_pick(PortalOutcome::Opened(folder.path().to_path_buf()));
    asks(&engine, 3, FileChooserMode::One, &[]);
    widget.apply_engine_events();
    recorder.respond_to_pick_multiple(MultiOutcome::Selected(vec![picked[0].clone(), missing]));
    asks(&engine, 4, FileChooserMode::Several, &[]);
    widget.apply_engine_events();
    // A file where a folder was asked for.
    recorder.respond_to_pick(PortalOutcome::Opened(picked[0].clone()));
    asks(&engine, 5, FileChooserMode::Folder, &[]);
    widget.apply_engine_events();
    assert_eq!(recorder.pick_requests().len(), 5);
    assert_eq!(
        answers(&engine),
        [
            (1, vec![]),
            (2, vec![]),
            (3, vec![]),
            (4, vec![]),
            (5, vec![])
        ],
        "all of a pick or none of it: the page never gets a part or a guess"
    );
}

#[test]
fn a_newer_chooser_supersedes_the_one_before_it() {
    let (services, recorder) = HostServices::recording();
    let _host = ui::host::enter(&services);
    let (engine, mut widget) = window(BrowsingMode::Normal);
    let (_folder, picked) = files(&["first.pdf", "second.pdf"]);
    recorder.respond_to_pick(PortalOutcome::Opened(picked[0].clone()));
    recorder.respond_to_pick(PortalOutcome::Opened(picked[1].clone()));
    // The page asked again before the first picker's answer was taken.
    asks(&engine, 1, FileChooserMode::One, &[]);
    asks(&engine, 2, FileChooserMode::One, &[]);
    widget.apply_engine_events();
    assert_eq!(recorder.pick_requests().len(), 2);
    assert_eq!(
        answers(&engine),
        [(2, vec![named(&picked[1])])],
        "the first picker's file reaches no chooser, least of all the second"
    );
}

#[test]
fn a_chooser_whose_tab_closed_or_left_is_never_answered() {
    let (services, recorder) = HostServices::recording();
    let _host = ui::host::enter(&services);
    let (engine, mut widget) = window(BrowsingMode::Normal);
    let (_folder, picked) = files(&["passport.pdf"]);
    recorder.respond_to_pick(PortalOutcome::Opened(picked[0].clone()));
    // The engine let the chooser go (its tab closed, its page went
    // elsewhere, nobody answered in time) before the picker's answer was
    // taken.
    asks(&engine, 3, FileChooserMode::One, &[]);
    engine.push_event(WebViewEvent::FileChooserClosed { id: 3 });
    widget.apply_engine_events();
    assert_eq!(recorder.pick_requests().len(), 1, "the picker was open");
    widget.apply_engine_events();
    assert_eq!(
        answers(&engine),
        [],
        "what was picked for a page that is gone reaches no page"
    );

    // Another chooser's end is not this one's.
    recorder.respond_to_pick(PortalOutcome::Opened(picked[0].clone()));
    asks(&engine, 4, FileChooserMode::One, &[]);
    engine.push_event(WebViewEvent::FileChooserClosed { id: 3 });
    widget.apply_engine_events();
    assert_eq!(answers(&engine), [(4, vec![named(&picked[0])])]);
}

#[test]
fn a_window_an_agent_operates_never_puts_a_picker_in_front_of_the_person() {
    let (services, recorder) = HostServices::recording();
    let _host = ui::host::enter(&services);
    let (engine, mut widget) = window(BrowsingMode::Visitor);
    assert!(
        !engine.accepts_file_choosers(),
        "the engine keeps an agent's window's choosers to itself"
    );
    let (_folder, picked) = files(&["passport.pdf"]);
    recorder.respond_to_pick(PortalOutcome::Opened(picked[0].clone()));
    // Even told of one, the window asks nobody and the page stops waiting.
    asks(&engine, 9, FileChooserMode::One, &[]);
    widget.apply_engine_events();
    assert_eq!(
        recorder.pick_requests().len(),
        0,
        "no picker opens for a click the person did not make"
    );
    assert_eq!(answers(&engine), [(9, vec![])]);
}
