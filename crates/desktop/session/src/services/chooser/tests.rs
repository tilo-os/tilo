//! The chooser service on its own socket, reached the way an app reaches it: the launcher's
//! handshake written first, then the app's own client (`tilo_dialogs::shell`).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_app_launch::Launch;
use tilo_dialogs::portal::{
    ContentsOutcome, LaunchOutcome, LaunchRequest, PickRequest, PortalOutcome, SaveOutcome,
};
use tilo_dialogs::shell::ShellLink;

use super::{Opens, Served, Serving, serve_at};

const APP: &str = "so.tilo.settings";
const WORKSPACE: &str = "5ba25f73-87c5-447f-a5a6-880dc4249a81";

fn answer() -> Duration {
    tilo_test_env::hang_bound()
}

/// The session's apps as the loop opens with them: the link routing the shell's search uses, over
/// a device whose Browser claims web links, recording what it would start.
#[derive(Default)]
struct Device {
    started: Mutex<Vec<(String, Launch)>>,
}

impl Opens for Device {
    fn open_claimed(
        &self,
        workspace: &str,
        uri: &str,
    ) -> Result<Option<()>, tilo_actions::ApiError> {
        let browser = desktop_policy::AppCandidate::package(
            "so.tilo.browser".into(),
            "Browser".into(),
            &[],
            &[],
            &["http".into(), "https".into()],
            false,
        );
        let personal = desktop_policy::PersonalDefaults::default();
        let defaults = tilo_app_launch::routing::Defaults {
            person: "ada",
            personal: &personal,
            workspace: None,
            host: None,
        };
        Ok(
            tilo_app_launch::routing::link(uri, &[browser], &defaults).map(|launch| {
                self.started
                    .lock()
                    .unwrap()
                    .push((workspace.to_owned(), launch));
            }),
        )
    }
}

/// Turn the session's loop over `device` until `done` holds.
fn loop_until(served: &Served, device: &Device, done: impl Fn() -> bool) {
    let deadline = std::time::Instant::now().checked_add(answer()).unwrap();
    while !done() {
        assert!(
            std::time::Instant::now() < deadline,
            "the loop was never asked"
        );
        served.follow(device);
        std::thread::yield_now();
    }
}

/// The question the chooser put up, as the session's sheet finds it.
fn pending(questions: &tilo_service::AskReceiver) -> tilo_service::Question {
    let deadline = std::time::Instant::now().checked_add(answer()).unwrap();
    loop {
        if let Some(question) = questions.pending().into_iter().next() {
            return question;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no question was asked"
        );
        std::thread::yield_now();
    }
}

/// Answer the question now up with `paths`.
fn answer_with(questions: &tilo_service::AskReceiver, paths: &[&std::path::Path]) {
    let question = pending(questions);
    assert!(
        questions.answer(
            &question.handle,
            tilo_service::Answer {
                uris: paths
                    .iter()
                    .map(|path| url::Url::from_file_path(path).unwrap().to_string())
                    .collect(),
                ..tilo_service::Answer::default()
            },
        ),
        "the question is up"
    );
}

/// An app's connection as its launcher hands it over: connected, and named by the launcher.
fn app_connection(socket: &std::path::Path, app: &str) -> ShellLink {
    use std::io::Write as _;

    let mut stream = std::os::unix::net::UnixStream::connect(socket).unwrap();
    stream
        .write_all(&launcher::services::chooser_handshake(app, WORKSPACE).unwrap())
        .unwrap();
    ShellLink::connect(stream).unwrap()
}

/// A session serving the chooser on a socket of its own, and an app connected to it as its
/// launcher connects one, with the documents store the session serves its sandboxes.
struct Rig {
    directory: tempfile::TempDir,
    socket: std::path::PathBuf,
    questions: tilo_service::AskReceiver,
    documents: tilo_documents::Documents,
    served: Served,
    device: Device,
    link: ShellLink,
}

fn rig() -> Rig {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("chooser");
    let (asks, questions) = tilo_service::asks::channel();
    let documents = tilo_documents::Documents::new();
    let served = serve_at(
        &socket,
        Serving {
            questions: Some(asks),
            documents: documents.clone(),
            wake: Arc::new(|| {}),
        },
    )
    .unwrap();
    let link = app_connection(&socket, APP);
    Rig {
        directory,
        socket,
        questions,
        documents,
        served,
        device: Device::default(),
        link,
    }
}

#[test]
fn a_link_opens_in_the_app_that_claims_it_and_one_nothing_claims_goes_back() {
    let Rig {
        served,
        device,
        link,
        directory: _directory,
        ..
    } = rig();

    let elsewhere = Arc::new(Elsewhere::default());
    let opener = link.opener(Arc::clone(&elsewhere) as Arc<dyn tilo_dialogs::UriOpener>);
    let login = "https://cloud.example.test/login/v2/flow/abc";
    let asking = std::thread::spawn({
        let opener = Arc::clone(&opener);
        move || opener.open(login)
    });
    loop_until(&served, &device, || asking.is_finished());
    asking.join().unwrap().unwrap();
    assert_eq!(
        *device.started.lock().unwrap(),
        [(
            WORKSPACE.to_owned(),
            Launch::Package {
                app: "so.tilo.browser".into(),
                open: Some(tilo_app_launch::Open::Uri(login.into())),
            }
        )],
        "the Browser starts on the login page, in the workspace the launcher named"
    );
    assert!(elsewhere.0.lock().unwrap().is_empty());

    // A link nothing here takes goes back to the app, which hands it to its desktop.
    let asking = std::thread::spawn({
        let opener = Arc::clone(&opener);
        move || opener.open("mailto:ada@example.test")
    });
    loop_until(&served, &device, || asking.is_finished());
    asking.join().unwrap().unwrap();
    assert_eq!(*elsewhere.0.lock().unwrap(), ["mailto:ada@example.test"]);
}

#[test]
fn a_pick_is_the_sessions_question_and_answers_the_exported_reference() {
    let Rig {
        directory,
        questions,
        documents,
        served,
        device,
        link,
        ..
    } = rig();

    let chosen = directory.path().join("plan.txt");
    std::fs::write(&chosen, "Spring planting").unwrap();
    let picker = link.picker(tilo_dialogs::unavailable_picker());
    let picking = picker.pick(PickRequest::open("Import a plan", "Import"));
    let question = pending(&questions);
    assert_eq!(
        question.kind,
        tilo_portal::file_chooser::FILE_CHOOSER_QUESTION
    );
    assert_eq!(
        question.app_id.as_deref(),
        Some(APP),
        "the launcher's name, not the app's"
    );
    assert_eq!(question.details["title"], "Import a plan");
    answer_with(&questions, &[&chosen]);
    let PortalOutcome::Opened(reference) = picking.recv_timeout(answer()).unwrap() else {
        panic!("the pick answered nothing");
    };
    let (token, _) = tilo_documents::split_sandbox_path(&reference)
        .expect("a sandboxed app is answered a path in its view of the documents store");
    let key = tilo_documents::AppKey::new(WORKSPACE, APP).unwrap();
    assert_eq!(
        documents.document(&key, &token).unwrap().host,
        chosen.canonicalize().unwrap()
    );

    // The descriptor the answer carried reads the file the person chose, and one the app was
    // not given does not.
    let (tx, rx) = std::sync::mpsc::channel();
    link.reopen(&reference, false, move |opened| {
        let _ = tx.send(opened);
    });
    let (mut file, reopened, name) = rx.recv_timeout(answer()).unwrap().expect("a handle");
    use std::io::Read as _;
    let mut contents = String::new();
    file.read_to_string(&mut contents).unwrap();
    assert_eq!(contents, "Spring planting");
    assert_eq!(reopened, reference);
    assert_eq!(name, "plan.txt");
    let (tx, rx) = std::sync::mpsc::channel();
    link.reopen(std::path::Path::new("/etc/passwd"), false, move |opened| {
        let _ = tx.send(opened);
    });
    assert!(
        rx.recv_timeout(answer()).unwrap().is_err(),
        "a host path is not a document of this app"
    );

    // The file it was given opens in the app that takes it; one it was not given does not.
    let opening = std::thread::spawn({
        let picker = Arc::clone(&picker);
        let reference = reference.clone();
        move || {
            picker
                .launch(LaunchRequest::open(reference))
                .recv_timeout(answer())
        }
    });
    loop_until(&served, &device, || opening.is_finished());
    assert_eq!(
        opening.join().unwrap().unwrap(),
        LaunchOutcome::Failed(tilo_dialogs::chooser_service::NOTHING_OPENS.into(),),
        "the device here has no app for text files, so the open reached the routing"
    );
    assert_eq!(
        picker
            .launch(LaunchRequest::open("/etc/passwd"))
            .recv_timeout(answer())
            .unwrap(),
        LaunchOutcome::Failed(tilo_dialogs::chooser_service::NOT_CHOSEN.into())
    );
}

#[test]
fn a_save_writes_beside_the_file_and_the_chooser_commits_it() {
    let Rig {
        directory,
        questions,
        link,
        ..
    } = rig();

    let destination = directory.path().join("report.txt");
    let picker = link.picker(tilo_dialogs::unavailable_picker());
    let saving = picker.save_contents(
        PickRequest::save("Save the report", "Save", "report.txt"),
        b"the report".to_vec(),
    );
    let question = pending(&questions);
    assert_eq!(question.details["title"], "Save the report");
    answer_with(&questions, &[&destination]);
    assert_eq!(saving.recv_timeout(answer()).unwrap(), SaveOutcome::Saved);
    assert_eq!(std::fs::read(&destination).unwrap(), b"the report");
    assert_eq!(
        std::fs::read_dir(directory.path()).unwrap().count(),
        2,
        "only the chosen file, and the tempdir, are left"
    );
}

#[test]
fn a_choice_of_contents_is_only_the_bytes_the_person_chose() {
    let Rig {
        directory,
        questions,
        link,
        ..
    } = rig();

    // Past one chunk, so the app reads it in several.
    let markdown = "# Lisbon in May\n".repeat(1500);
    let chosen = directory.path().join("Lisbon.md");
    std::fs::write(&chosen, &markdown).unwrap();
    let picker = link.picker(tilo_dialogs::unavailable_picker());
    let picking = picker.pick_contents(PickRequest::open("Import Markdown", "Import"));
    assert_eq!(pending(&questions).app_id.as_deref(), Some(APP));
    answer_with(&questions, &[&chosen]);
    let ContentsOutcome::Picked(files) = picking.recv_timeout(answer()).unwrap() else {
        panic!("the app was not given the file");
    };
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].name, "Lisbon.md");
    assert_eq!(files[0].bytes, markdown.as_bytes());
    assert_eq!(
        files[0].location,
        Some(std::path::PathBuf::from("Lisbon.md")),
        "the app learns where it stands in what the person chose, never the host path"
    );
}

#[test]
fn a_cancelled_pick_and_an_unserved_chooser_answer_the_app() {
    let Rig {
        questions,
        link,
        directory: _directory,
        ..
    } = rig();

    let picker = link.picker(tilo_dialogs::unavailable_picker());
    let picking = picker.pick(PickRequest::open("Open", "Open"));
    let handle = pending(&questions).handle;
    assert!(questions.decline(&handle));
    assert_eq!(
        picking.recv_timeout(answer()).unwrap(),
        PortalOutcome::Cancelled
    );

    let (chosen_tx, chosen) = std::sync::mpsc::channel();
    link.choose_app("Paste in apps", move |outcome| {
        let _ = chosen_tx.send(outcome);
    });
    assert_eq!(
        chosen.recv_timeout(answer()).unwrap(),
        Err(tilo_dialogs::chooser_service::NO_APP_CHOOSER.to_owned())
    );
}

#[test]
fn a_connection_the_launcher_did_not_name_is_closed() {
    use std::io::{Read as _, Write as _};

    let Rig {
        socket,
        directory: _directory,
        ..
    } = rig();
    let mut stream = std::os::unix::net::UnixStream::connect(&socket).unwrap();
    stream
        .write_all(b"{\"type\":\"open_uri\",\"id\":1,\"uri\":\"https://example.test/\"}\n")
        .unwrap();
    stream.set_read_timeout(Some(answer())).unwrap();
    let mut rest = Vec::new();
    stream.read_to_end(&mut rest).unwrap();
    assert!(
        rest.is_empty(),
        "nothing is answered to a connection no launcher named"
    );
}

#[derive(Default)]
struct Elsewhere(Mutex<Vec<String>>);

impl tilo_dialogs::UriOpener for Elsewhere {
    fn open(&self, uri: &str) -> std::io::Result<()> {
        self.0.lock().unwrap().push(uri.to_owned());
        Ok(())
    }
}

#[test]
fn a_folder_choice_hands_over_its_files_with_their_paths_inside_it() {
    let Rig {
        directory,
        questions,
        link,
        ..
    } = rig();

    let library = directory.path().join("library");
    let album = library.join("album");
    std::fs::create_dir_all(&album).unwrap();
    let count = 10_000;
    for at in 0..count {
        std::fs::write(album.join(format!("f{at:05}.txt")), b"x").unwrap();
    }

    let picker = link.picker(tilo_dialogs::unavailable_picker());
    let started = std::time::Instant::now();
    let picking = picker.pick_contents(PickRequest::directory("Import a folder", "Import"));
    let question = pending(&questions);
    assert_eq!(question.details["title"], "Import a folder");
    answer_with(&questions, &[&library]);
    let ContentsOutcome::Picked(files) = picking.recv_timeout(answer()).unwrap() else {
        panic!("the app was not given the folder's files");
    };
    let elapsed = started.elapsed();
    assert_eq!(
        files.len(),
        count,
        "every file of the folder is handed over"
    );
    assert!(
        files.iter().all(
            |file| file.location == Some(std::path::PathBuf::from(&file.name))
                && file.name.starts_with("album/f")
        ),
        "each file says where it stands in the folder, never the host path"
    );
    assert!(
        elapsed < answer(),
        "a folder of {count} files is answered within the wait an app allows: {elapsed:?}"
    );
}

#[test]
fn a_folder_handle_reopens_a_file_inside_it_and_refuses_one_outside() {
    let Rig {
        directory,
        questions,
        link,
        ..
    } = rig();

    let library = directory.path().join("library");
    std::fs::create_dir_all(library.join("sub")).unwrap();
    std::fs::write(library.join("sub/kept.txt"), "kept").unwrap();
    let secret = directory.path().join("secret.txt");
    std::fs::write(&secret, "secret").unwrap();

    let picker = link.picker(tilo_dialogs::unavailable_picker());
    let picking = picker.open_handle(PickRequest::directory("Choose a folder", "Choose"));
    answer_with(&questions, &[&library]);
    let tilo_dialogs::portal::OpenFileOutcome::Opened(folder) =
        picking.recv_timeout(answer()).unwrap()
    else {
        panic!("the folder was not handed over");
    };
    let reference = folder.reference().unwrap().to_path_buf();
    assert!(folder.file().metadata().unwrap().is_dir());

    // A file inside the chosen folder reopens by its reference …
    let inside = reference.join("sub/kept.txt");
    let (tx, rx) = std::sync::mpsc::channel();
    link.reopen(&inside, false, move |opened| {
        let _ = tx.send(opened);
    });
    let (mut file, _, name) = rx.recv_timeout(answer()).unwrap().expect("a handle");
    use std::io::Read as _;
    let mut contents = String::new();
    file.read_to_string(&mut contents).unwrap();
    assert_eq!(contents, "kept");
    assert_eq!(name, "kept.txt");

    // … and one that climbs out of it does not.
    let outside = reference.join("../secret.txt");
    let (tx, rx) = std::sync::mpsc::channel();
    link.reopen(&outside, false, move |opened| {
        let _ = tx.send(opened);
    });
    assert!(
        rx.recv_timeout(answer()).unwrap().is_err(),
        "a chosen folder does not admit what climbs out of it"
    );
}

#[test]
fn several_files_chosen_at_once_are_all_handed_over() {
    let Rig {
        directory,
        questions,
        link,
        ..
    } = rig();

    let one = directory.path().join("one.txt");
    let two = directory.path().join("two.txt");
    std::fs::write(&one, "one").unwrap();
    std::fs::write(&two, "two").unwrap();
    let picker = link.picker(tilo_dialogs::unavailable_picker());
    let picking = picker.pick_contents(PickRequest::open_multiple("Import notes", "Import"));
    answer_with(&questions, &[&one, &two]);
    let ContentsOutcome::Picked(files) = picking.recv_timeout(answer()).unwrap() else {
        panic!("the app was not given the files");
    };
    assert_eq!(
        files
            .iter()
            .map(|file| (file.name.as_str(), file.bytes.as_slice()))
            .collect::<Vec<_>>(),
        [("one.txt", &b"one"[..]), ("two.txt", &b"two"[..])]
    );
}
