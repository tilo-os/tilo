//! The chooser service on its own socket, reached the way an app reaches it: the launcher's
//! handshake written first, then the app's own client (`tilo_dialogs::shell`).

use std::io::Read as _;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_app_launch::Launch;
use tilo_dialogs::portal::{
    LaunchOutcome, LaunchRequest, OpenFileOutcome, PickRequest, PortalOutcome, SaveOutcome,
};
use tilo_dialogs::shell::ShellLink;

use super::{Opens, Served, Serving, WorkspaceFiles, WorkspaceNode, serve_at};

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
/// launcher connects one, with the session's table of chosen files.
struct Rig {
    directory: tempfile::TempDir,
    socket: std::path::PathBuf,
    questions: tilo_service::AskReceiver,
    chosen: Arc<tilo_dialogs::chosen::ChosenTable>,
    served: Served,
    device: Device,
    link: ShellLink,
}

fn rig() -> Rig {
    rig_with(None)
}

fn rig_with(files: Option<Arc<dyn WorkspaceFiles>>) -> Rig {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("chooser");
    let (asks, questions) = tilo_service::asks::channel();
    let chosen = Arc::new(tilo_dialogs::chosen::ChosenTable::new());
    let served = serve_at(
        &socket,
        Serving {
            questions: Some(asks),
            chosen: std::sync::Arc::clone(&chosen),
            files,
            wake: Arc::new(|| {}),
        },
    )
    .unwrap();
    let link = app_connection(&socket, APP);
    Rig {
        directory,
        socket,
        questions,
        chosen,
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
        chosen: table,
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
    let (token, _) = tilo_dialogs::chosen::split_reference(&reference)
        .expect("a sandboxed app is answered a reference into the chosen files");
    let key = tilo_dialogs::chosen::AppKey::new(WORKSPACE, APP).unwrap();
    assert_eq!(
        table.document(&key, &token).unwrap().host,
        chosen.canonicalize().unwrap()
    );

    // The descriptor the answer carried reads the file the person chose, and one the app was
    // not given does not.
    let (tx, rx) = std::sync::mpsc::channel();
    link.reopen(&reference, false, move |opened| {
        let _ = tx.send(opened);
    });
    let (mut file, reopened, name) = rx.recv_timeout(answer()).unwrap().expect("a handle");
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
    let mut left: Vec<_> = std::fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    left.sort();
    assert_eq!(
        left,
        ["chooser", "report.txt"],
        "only the chosen file, and the socket, are left"
    );
}

#[test]
fn a_choice_hands_a_descriptor_only_for_what_the_person_chose() {
    let Rig {
        directory,
        questions,
        link,
        ..
    } = rig();

    let markdown = "# Lisbon in May\n".repeat(1500);
    let chosen = directory.path().join("Lisbon.md");
    std::fs::write(&chosen, &markdown).unwrap();
    let picker = link.picker(tilo_dialogs::unavailable_picker());
    let picking = picker.open_handle(PickRequest::open("Import Markdown", "Import"));
    assert_eq!(pending(&questions).app_id.as_deref(), Some(APP));
    answer_with(&questions, &[&chosen]);
    let OpenFileOutcome::Opened(opened) = picking.recv_timeout(answer()).unwrap() else {
        panic!("the app was not given the file");
    };
    assert_eq!(opened.display_name(), "Lisbon.md");
    let reference = opened
        .reference()
        .expect("a reference names it again")
        .to_path_buf();
    let mut file = opened.into_file();
    let mut contents = Vec::new();
    file.read_to_end(&mut contents).unwrap();
    assert_eq!(contents, markdown.as_bytes());

    // A reference this app was never given is refused: a path on the host, and another app's
    // choice in the same workspace.
    let (tx, rx) = std::sync::mpsc::channel();
    link.reopen(std::path::Path::new("/etc/passwd"), false, move |opened| {
        let _ = tx.send(opened);
    });
    assert!(
        rx.recv_timeout(answer()).unwrap().is_err(),
        "a reference the app was never given opens nothing"
    );
    assert_ne!(reference, std::path::Path::new("/etc/passwd"));
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
fn a_folder_choice_hands_a_descriptor_whose_files_walk_in_order() {
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
    let picking = picker.open_handle(PickRequest::directory("Import a folder", "Import"));
    let question = pending(&questions);
    assert_eq!(question.details["title"], "Import a folder");
    answer_with(&questions, &[&library]);
    let OpenFileOutcome::Opened(folder) = picking.recv_timeout(answer()).unwrap() else {
        panic!("the app was not given the folder");
    };
    let elapsed = started.elapsed();
    // The folder is a descriptor the app walks itself: every file inside it, by its path within
    // the folder, never the host path.
    let folder = folder.into_file().into_std();
    let walked = tilo_dialogs::portal::walk_chosen_folder(&folder, 64 * 1024 * 1024)
        .expect("the chosen folder walks");
    drop(folder);
    assert_eq!(walked.len(), count, "every file of the folder is reached");
    assert!(
        walked
            .iter()
            .all(|(name, bytes)| name.starts_with("album/f") && bytes == b"x"),
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
    assert!(folder.into_file().into_std().metadata().unwrap().is_dir());

    // A file inside the chosen folder reopens by its reference …
    let inside = reference.join("sub/kept.txt");
    let (tx, rx) = std::sync::mpsc::channel();
    link.reopen(&inside, false, move |opened| {
        let _ = tx.send(opened);
    });
    let (mut file, _, name) = rx.recv_timeout(answer()).unwrap().expect("a handle");
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
    let picking = picker.pick_multiple(PickRequest::open_multiple("Import notes", "Import"));
    answer_with(&questions, &[&one, &two]);
    let tilo_dialogs::portal::MultiOutcome::Selected(references) =
        picking.recv_timeout(answer()).unwrap()
    else {
        panic!("the app was not given the files");
    };
    let mut read = Vec::new();
    for reference in references {
        let (tx, rx) = std::sync::mpsc::channel();
        let ask = reference;
        link.reopen(&ask, false, move |opened| {
            let _ = tx.send(opened);
        });
        let (mut file, _, name) = rx.recv_timeout(answer()).unwrap().expect("a handle");
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        read.push((name, bytes));
    }
    assert_eq!(
        read,
        [
            ("one.txt".to_owned(), b"one".to_vec()),
            ("two.txt".to_owned(), b"two".to_vec())
        ]
    );
}

/// One workspace's tree as the chooser reaches it: the files' records and the workspace's policy,
/// which a grant changes through the same checked edit a device's workspace applies.
struct FakeWorkspace {
    workspace: String,
    person: String,
    nodes: Mutex<Vec<WorkspaceNode>>,
    policy: Mutex<tilo_action_policy::WorkspacePolicy>,
}

impl FakeWorkspace {
    fn new(workspace: &str) -> Self {
        let mut policy = tilo_action_policy::WorkspacePolicy::new(workspace, OWNER);
        policy.add_member(PERSON, tilo_action_policy::Role::Editor);
        Self {
            workspace: workspace.to_owned(),
            person: PERSON.to_owned(),
            nodes: Mutex::default(),
            policy: Mutex::new(policy),
        }
    }

    /// The person who chose loses the workspace: what they gave the app goes with it.
    fn remove_person(&self) {
        self.policy
            .lock()
            .unwrap()
            .change_as_at(
                OWNER,
                tilo_action_policy::Change::RemoveMember {
                    person: PERSON.to_owned(),
                },
                tilo_clock::unix_now(),
            )
            .unwrap();
    }

    /// Give the file at `host` its record in the tree, as the workspace gives a file made there.
    fn place(&self, host: &std::path::Path) -> fs_protocol::FileDocumentId {
        let id = fs_protocol::FileDocumentId::new();
        self.nodes.lock().unwrap().push(WorkspaceNode {
            workspace: self.workspace.clone(),
            id,
            host: host.canonicalize().unwrap(),
            directory: false,
        });
        id
    }

    /// Where the record `id` is now, as a rename or a move found it.
    fn move_to(&self, id: fs_protocol::FileDocumentId, host: &std::path::Path) {
        if let Some(node) = self
            .nodes
            .lock()
            .unwrap()
            .iter_mut()
            .find(|node| node.id == id)
        {
            node.host = host.canonicalize().unwrap();
        }
    }

    /// The grant the workspace holds by `id`.
    fn granted(&self, id: &str) -> Option<tilo_action_policy::Grant> {
        self.policy
            .lock()
            .unwrap()
            .grants
            .iter()
            .find(|held| held.id.as_str() == id)
            .cloned()
    }

    /// Take the grant `app` holds on the record `id` back, as the file's sharing does.
    fn revoke(&self, id: &str) {
        self.policy
            .lock()
            .unwrap()
            .change_as_at(
                &self.person,
                tilo_action_policy::Change::Revoke(tilo_action_policy::GrantId::new(id)),
                tilo_clock::unix_now(),
            )
            .unwrap();
    }
}

impl WorkspaceFiles for FakeWorkspace {
    fn node(&self, host: &std::path::Path) -> Option<WorkspaceNode> {
        let host = host.canonicalize().ok()?;
        self.nodes
            .lock()
            .unwrap()
            .iter()
            .find(|node| node.host == host)
            .cloned()
    }

    fn node_at(&self, workspace: &str, id: fs_protocol::FileDocumentId) -> Option<WorkspaceNode> {
        (workspace == self.workspace)
            .then(|| {
                self.nodes
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|node| node.id == id)
                    .cloned()
            })
            .flatten()
    }

    fn grant(&self, node: &WorkspaceNode, app: &str, write: bool) -> Result<(), String> {
        let grant = super::app_grant(node, app, &self.person, write);
        self.policy
            .lock()
            .unwrap()
            .change_as_at(
                &self.person,
                tilo_action_policy::Change::Grant(grant),
                tilo_clock::unix_now(),
            )
            .map_err(|refusal| refusal.to_string())
    }

    fn holds(&self, node: &WorkspaceNode, app: &str, write: bool) -> bool {
        super::granted_kinds(
            &self.policy.lock().unwrap(),
            node,
            app,
            write,
            tilo_clock::unix_now(),
        )
        .is_some()
    }
}

/// The workspace mount a rig places its files in: the person's own workspace, whose tree a
/// device's filesystem serves.
const FILES_WORKSPACE: &str = "11111111-1111-4111-8111-111111111111";
/// The person who chose, by the id their devices sign as.
const PERSON: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
/// The workspace's owner, who is not the person choosing.
const OWNER: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

/// The node's reference, as the chooser answers with it.
fn node_reference(workspace: &str, id: fs_protocol::FileDocumentId) -> std::path::PathBuf {
    std::path::PathBuf::from(super::CHOSEN_ROOT)
        .join(workspace)
        .join(id.0.to_string())
}

fn files(workspace: &Arc<FakeWorkspace>) -> Arc<dyn WorkspaceFiles> {
    Arc::clone(workspace) as Arc<dyn WorkspaceFiles>
}

#[test]
fn a_chosen_workspace_file_is_a_grant_on_its_node_and_reopen_reads_it() {
    let workspace = Arc::new(FakeWorkspace::new(FILES_WORKSPACE));
    let rig = rig_with(Some(files(&workspace)));
    let Rig {
        directory,
        questions,
        link,
        ..
    } = rig;
    let chosen = directory.path().join("plan.txt");
    std::fs::write(&chosen, "Spring planting").unwrap();
    let id = workspace.place(&chosen);

    let picking = link
        .picker(tilo_dialogs::unavailable_picker())
        .pick(PickRequest::open("Import a plan", "Import"));
    answer_with(&questions, &[&chosen]);
    let PortalOutcome::Opened(reference) = picking.recv_timeout(answer()).unwrap() else {
        panic!("the pick answered nothing");
    };
    assert_eq!(
        reference,
        node_reference(FILES_WORKSPACE, id),
        "the answer names the node, never a host path"
    );

    let key = format!("chosen:{APP}:{}:read", id.0);
    let held = workspace
        .granted(&key)
        .expect("the workspace keeps the app's grant");
    assert_eq!(
        held.node,
        tilo_action_policy::TreePath::workspace(FILES_WORKSPACE)
            .app(fs_protocol::tree::FILES_APP)
            .kind(tilo_actions::remote::FOUND_FILE)
            .resource(id.0.to_string())
    );
    assert_eq!(held.kinds, tilo_action_policy::KindSet::READ);
    assert_eq!(
        held.granted_by,
        tilo_action_policy::Grantee::person(PERSON),
        "history names the person who chose"
    );

    // The descriptor reads the file, and the reference opens it again.
    let (tx, rx) = std::sync::mpsc::channel();
    let ask = reference.clone();
    link.reopen(&ask, false, move |opened| {
        let _ = tx.send(opened);
    });
    let (mut file, reopened, name) = rx.recv_timeout(answer()).unwrap().expect("a handle");
    let mut contents = String::new();
    file.read_to_string(&mut contents).unwrap();
    assert_eq!(contents, "Spring planting");
    assert_eq!(reopened, reference);
    assert_eq!(name, "plan.txt");
}

#[test]
fn a_revoked_grant_opens_nothing_and_neither_app_reaches_another() {
    let workspace = Arc::new(FakeWorkspace::new(FILES_WORKSPACE));
    let rig = rig_with(Some(files(&workspace)));
    let Rig {
        directory,
        questions,
        socket,
        link,
        ..
    } = rig;
    let chosen = directory.path().join("plan.txt");
    std::fs::write(&chosen, "Spring planting").unwrap();
    let id = workspace.place(&chosen);

    let picking = link
        .picker(tilo_dialogs::unavailable_picker())
        .pick(PickRequest::open("Import a plan", "Import"));
    answer_with(&questions, &[&chosen]);
    let PortalOutcome::Opened(reference) = picking.recv_timeout(answer()).unwrap() else {
        panic!("the pick answered nothing");
    };

    // Another app of the same workspace holds no grant on the node.
    let other = app_connection(&socket, "so.tilo.browser");
    let (tx, rx) = std::sync::mpsc::channel();
    let ask = reference.clone();
    other.reopen(&ask, false, move |opened| {
        let _ = tx.send(opened);
    });
    assert!(
        rx.recv_timeout(answer()).unwrap().is_err(),
        "a second app reaches nothing the person chose for the first"
    );

    // Revoked through the file's sharing, the grant opens nothing again.
    workspace.revoke(&format!("chosen:{APP}:{}:read", id.0));
    let (tx, rx) = std::sync::mpsc::channel();
    let ask = reference;
    link.reopen(&ask, false, move |opened| {
        let _ = tx.send(opened);
    });
    assert!(
        rx.recv_timeout(answer()).unwrap().is_err(),
        "a revoked grant opens the file no more"
    );
}

#[test]
fn a_grant_follows_the_node_through_a_rename_and_a_new_file_at_the_old_path_is_not_it() {
    let workspace = Arc::new(FakeWorkspace::new(FILES_WORKSPACE));
    let rig = rig_with(Some(files(&workspace)));
    let Rig {
        directory,
        questions,
        link,
        ..
    } = rig;
    let old = directory.path().join("plan.txt");
    std::fs::write(&old, "Spring planting").unwrap();
    let id = workspace.place(&old);

    let picking = link
        .picker(tilo_dialogs::unavailable_picker())
        .pick(PickRequest::open("Import a plan", "Import"));
    answer_with(&questions, &[&old]);
    let PortalOutcome::Opened(reference) = picking.recv_timeout(answer()).unwrap() else {
        panic!("the pick answered nothing");
    };

    // The workspace moved the file: the reference names the record, so it opens where it is now.
    let renamed = directory.path().join("planting.md");
    std::fs::rename(&old, &renamed).unwrap();
    workspace.move_to(id, &renamed);
    let (tx, rx) = std::sync::mpsc::channel();
    let ask = reference.clone();
    link.reopen(&ask, false, move |opened| {
        let _ = tx.send(opened);
    });
    let (mut file, reopened, name) = rx.recv_timeout(answer()).unwrap().expect("a handle");
    let mut contents = String::new();
    file.read_to_string(&mut contents).unwrap();
    assert_eq!(contents, "Spring planting", "the grant followed the record");
    assert_eq!(name, "planting.md");
    assert_eq!(reopened, reference);

    // A new file taking the old name is a new record, and no grant names it.
    std::fs::write(&old, "Autumn planning").unwrap();
    let other = workspace.place(&old);
    let newer = node_reference(FILES_WORKSPACE, other);
    assert_ne!(newer, reference);
    let (tx, rx) = std::sync::mpsc::channel();
    link.reopen(&newer, false, move |opened| {
        let _ = tx.send(opened);
    });
    assert!(
        rx.recv_timeout(answer()).unwrap().is_err(),
        "a new file at the old path is not the one the person chose"
    );
}

#[test]
fn a_save_grants_writing_the_node_and_keeps_the_record() {
    let workspace = Arc::new(FakeWorkspace::new(FILES_WORKSPACE));
    let rig = rig_with(Some(files(&workspace)));
    let Rig {
        directory,
        questions,
        link,
        ..
    } = rig;
    let chosen = directory.path().join("report.txt");
    std::fs::write(&chosen, "the report").unwrap();
    let id = workspace.place(&chosen);

    let saving = link
        .picker(tilo_dialogs::unavailable_picker())
        .save_contents(
            PickRequest::save("Save the report", "Save", "report.txt"),
            b"the report, revised".to_vec(),
        );
    answer_with(&questions, &[&chosen]);
    assert_eq!(saving.recv_timeout(answer()).unwrap(), SaveOutcome::Saved);
    assert_eq!(std::fs::read(&chosen).unwrap(), b"the report, revised");
    let held = workspace
        .granted(&format!("chosen:{APP}:{}:write", id.0))
        .expect("the save's grant covers writing the node");
    assert_eq!(
        held.kinds,
        tilo_action_policy::KindSet::READ_UNDOABLE,
        "reading it and changing it"
    );
    assert_eq!(
        workspace.node_at(FILES_WORKSPACE, id).unwrap().host,
        chosen.canonicalize().unwrap(),
        "the file is the node it was, never a new record"
    );
}

#[test]
fn a_mounts_index_is_what_names_a_node_and_a_path_that_is_not_its_record_is_refused() {
    let mount = tempfile::tempdir().unwrap();
    let folder = mount.path().join("notes");
    std::fs::create_dir(&folder).unwrap();
    let file = folder.join("plan.txt");
    std::fs::write(&file, "Spring planting").unwrap();
    let id = fs_protocol::FileDocumentId::new();
    let index = fs_protocol::projection::DocumentIndexExactV2 {
        schema_version: fs_protocol::projection::DOCUMENT_INDEX_V2_VERSION,
        revision: "ab".repeat(32),
        record: fs_protocol::projection::DocumentIndexEntryV1 {
            document_id: id,
            path: "/notes/plan.txt".to_owned(),
            is_dir: false,
        },
    };
    let name = format!(
        "{}{}",
        fs_protocol::projection::DOCUMENT_INDEX_BY_ID_XATTR_PREFIX,
        id.0
    );
    host_fs::attributes::set(mount.path(), name, &serde_json::to_vec(&index).unwrap())
        .expect("the tests' filesystem keeps user attributes, as a mount's does");
    let resolved = super::mounted_node(mount.path(), FILES_WORKSPACE, id, Some(&file))
        .expect("the mount names where its record is");
    assert_eq!(resolved.host, file);
    assert!(!resolved.directory);
    // A file that carries another record's path is not the record.
    assert!(
        super::mounted_node(mount.path(), FILES_WORKSPACE, id, Some(mount.path())).is_none(),
        "a path its record does not name is refused"
    );
}

#[test]
fn a_grant_ends_when_the_person_who_gave_it_loses_the_workspace() {
    let workspace = Arc::new(FakeWorkspace::new(FILES_WORKSPACE));
    let rig = rig_with(Some(files(&workspace)));
    let Rig {
        directory,
        questions,
        link,
        ..
    } = rig;
    let chosen = directory.path().join("plan.txt");
    std::fs::write(&chosen, "Spring planting").unwrap();
    workspace.place(&chosen);

    let picking = link
        .picker(tilo_dialogs::unavailable_picker())
        .pick(PickRequest::open("Import a plan", "Import"));
    answer_with(&questions, &[&chosen]);
    let PortalOutcome::Opened(reference) = picking.recv_timeout(answer()).unwrap() else {
        panic!("the pick answered nothing");
    };

    workspace.remove_person();
    let (tx, rx) = std::sync::mpsc::channel();
    link.reopen(&reference, false, move |opened| {
        let _ = tx.send(opened);
    });
    assert!(
        rx.recv_timeout(answer()).unwrap().is_err(),
        "a grant its person can no longer make gives the app nothing"
    );
}
