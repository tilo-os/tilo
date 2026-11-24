//! An app's records are kept by the workspace service the app reaches over its own connection:
//! its store reads and writes them with the record commands on the route the app joined, the session's broker hands them to the device's one store, and that store answers from
//! the workspace it holds. Nothing here stands in for the service: the session is a person's own,
//! with its identity, its workspace service and the app wire its apps connect on, and the app's
//! processes are this test's, admitted as the launches the session recorded.
//!
//! The device is offline throughout: its only relay never answers, so every read and write is
//! answered by the device's own service.

#![cfg(target_os = "linux")]

use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc;

use collaboration::resource::{
    ConnectionDocuments, DocumentAsk, DocumentCheckout, DocumentId, DocumentPort, DocumentRead,
    ResourceOwners, ResourceSaveStatus, ResourceType, conformance,
};
use tilo_session::{AppSocket, Launches, Options, Session};
use tilo_session_core::desktop::verbs::CHROME_RESOURCE;

use crate::common::{PERSON, reached, until, wait_for};

/// The app whose processes the test runs.
const APP: &str = "so.tilo.lang.tasks";

/// A relay nobody answers at: an address of the block kept for documentation, which routes
/// nowhere.
const SILENT_RELAY: &str = "https://192.0.2.1/";

/// A person's own session in `home`, with its identity, its workspace service and its app wire in
/// `root`, given only a relay that never answers. Answers the session once it holds the workspace
/// it founds, and that workspace.
fn offline_session(
    home: &mut tilo_test_env::TestHome,
    root: &std::path::Path,
    launches: Arc<Launches>,
) -> (Session, String) {
    let identity = home.config().join("tilo/identity");
    std::fs::create_dir_all(&identity).expect("the identity's directory");
    let system = home.root().join("system");
    let secrets = home.root().join("secrets");
    std::fs::create_dir_all(&secrets).expect("the secret store's directory");
    home.env()
        .set("TILO_IDENTITY_ROOT", &system)
        .set("TILO_SECRETS_DIR", &secrets);
    tilo_identity::provisioning::ensure_throwaway_test_identity(
        &identity,
        &tilo_identity::provisioning::collaboration_identity_paths(),
        tilo_identity::provisioning::IdentityStorage::Files,
    )
    .expect("the throwaway identity");
    let silent = SILENT_RELAY.parse::<iroh::RelayUrl>().expect("a relay URL");
    let mut session = crate::common::headless(Options {
        person: PERSON.to_owned(),
        collab: true,
        state: Some(home.state().to_path_buf()),
        relays: Some(tilo_endpoint_reachability::EndpointRelays {
            relays: iroh::RelayMap::from(silent),
            n0_dns: false,
        }),
        app_socket: AppSocket::At(root.join("app_wire.sock")),
        launches,
        ..Options::default()
    });
    let workspace = wait_for(
        &mut session,
        "the workspace the shell shows",
        front_workspace,
    );
    (session, workspace)
}

/// The workspace the shell shows.
fn front_workspace(session: &mut Session) -> Option<String> {
    session.handle().read(|tree| {
        tree.active_workspace()
            .map(|workspace| workspace.id().to_string())
    })
}

/// A process of `app`, whose records are of `kind`, in `workspace`, connected as the launcher
/// connects one: its collaboration connection and its identity handed over on the session's app
/// wire, its route joined. The session admits it as the launch it records for this test's process.
fn connect(
    socket: &std::path::Path,
    launches: &Launches,
    (app, kind): (&str, &str),
    workspace: &str,
) -> collaboration::CollaborationManager {
    launches.started(std::process::id(), app, workspace, true);
    let workspace_id = uuid::Uuid::parse_str(workspace).expect("a workspace id");
    let join = || -> Result<collaboration::CollaborationManager, String> {
        let mut app_wire =
            std::os::unix::net::UnixStream::connect(socket).map_err(|e| e.to_string())?;
        launcher::services::write_collab_handoff(&mut app_wire, app, workspace)
            .map_err(|e| e.to_string())?;
        let mut identity =
            std::os::unix::net::UnixStream::connect(launcher::services::identity_socket())
                .map_err(|e| e.to_string())?;
        launcher::services::write_identity_handshake(&mut identity, app, workspace)
            .map_err(|e| e.to_string())?;
        collaboration::CollaborationManager::join_instance(
            &collab_types::document::ResourceType::new(kind),
            std::os::fd::OwnedFd::from(app_wire),
            &std::os::fd::OwnedFd::from(identity),
            collaboration::RouteSpec::app(app.to_owned(), workspace_id),
        )
        .map_err(|error| format!("the app joins: {error:?}"))?
        .finish_join()
        .map_err(|error| format!("the app's route is joined: {error:?}"))
    };
    // A workspace, and its founding membership, reach the identity service a few turns after the
    // shell shows the workspace: until then a join is refused, as an app started that early would
    // be, and it joins again.
    let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
    loop {
        match join() {
            Ok(manager) => return manager,
            Err(why)
                if (why.contains("not a member")
                    || why.contains("not available on this device"))
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::park_timeout(std::time::Duration::from_millis(100));
            }
            Err(why) => panic!("{why}"),
        }
    }
}

/// A process's port onto its workspace: the store over its connection, held while the connection
/// lives, so the process ends when the last store holding it is dropped.
struct Process {
    _connection: collaboration::CollaborationManager,
    port: ConnectionDocuments,
}

/// Wait for what `port` has on its way, if anything: whether it had something.
fn answered(port: &dyn DocumentPort) -> bool {
    let (woken, wake) = mpsc::channel();
    let on_its_way = port.when_answered(Arc::new(move || {
        let _ = woken.send(());
    }));
    if on_its_way {
        wake.recv_timeout(tilo_test_env::hang_bound())
            .expect("the workspace answers what it was asked");
    }
    on_its_way
}

/// The scenarios read what the workspace answers, so each read here waits for the answer the
/// port has on its way rather than taking a record on its way as one the window shows arriving:
/// a window does not wait ([`a_record_the_service_answers_late_opens_without_waiting`]).
impl DocumentPort for Process {
    fn read(
        &self,
        app: &str,
        workspace: &str,
        reads: Option<Vec<DocumentAsk>>,
    ) -> Result<Vec<(String, DocumentRead)>, String> {
        let Some(reads) = reads else {
            let listed = self.port.read(app, workspace, None)?;
            // A listing on its way answered what was listed before it.
            return if answered(&self.port) {
                self.port.read(app, workspace, None)
            } else {
                Ok(listed)
            };
        };
        let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
        loop {
            let read = self.port.read(app, workspace, Some(reads.clone()))?;
            if !read.iter().any(|(_, read)| *read == DocumentRead::Pending)
                || std::time::Instant::now() >= deadline
            {
                return Ok(read);
            }
            if !answered(&self.port) {
                // The workspace itself has it on its way: it reads once it is here.
                std::thread::park_timeout(std::time::Duration::from_millis(20));
            }
        }
    }

    fn write(
        &self,
        app: &str,
        workspace: &str,
        id: &str,
        changes: Vec<u8>,
        founding: bool,
        readers: Option<&str>,
    ) -> collaboration::resource::Written {
        self.port
            .write(app, workspace, id, changes, founding, readers)
    }
}

/// A process of `app` started in `workspace` ([`connect`]), as the port its records are read and
/// written through.
fn started(
    socket: &std::path::Path,
    launches: &Launches,
    app: (&str, &str),
    workspace: &str,
) -> Rc<dyn DocumentPort> {
    Rc::new(process(socket, launches, app, workspace))
}

/// [`started`], as the process itself.
fn process(
    socket: &std::path::Path,
    launches: &Launches,
    app: (&str, &str),
    workspace: &str,
) -> Process {
    let connection = connect(socket, launches, app, workspace);
    let port = ConnectionDocuments::connected()
        .expect("a joined process has a connection to send its records on");
    let process = Process {
        _connection: connection,
        port,
    };
    // The device's service holds the workspace a few turns after the shell shows it: until then
    // what the process asks for is on its way, as a record whose keys have not come yet is, and
    // the process asks again. Once it is held, a record nobody wrote is one it holds none of.
    let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
    let unwritten = DocumentAsk {
        id: DocumentId::minted().route().to_owned(),
        since: None,
    };
    loop {
        let read = process
            .read(app.0, workspace, Some(vec![unwritten.clone()]))
            .unwrap_or_else(|why| panic!("the workspace answers its app: {why}"));
        match read.as_slice() {
            [(_, DocumentRead::Absent)] => break,
            [(_, DocumentRead::Pending)] if std::time::Instant::now() < deadline => {
                std::thread::park_timeout(std::time::Duration::from_millis(100));
            }
            other => panic!("the workspace holds no record nobody wrote: {other:?}"),
        }
    }
    process
}

/// A window's read of its record never waits for the workspace service: the store answers at
/// once, with the record or with it on its way, and once the service answered the record opens
/// as it was written. How long the service on this device takes to answer is printed: what the
/// port's first wait is measured against.
fn a_record_the_service_answers_late_opens_without_waiting(
    socket: &std::path::Path,
    launches: &Launches,
    (app, kind): (&str, &str),
    workspace: &str,
) {
    let note = DocumentCheckout::new(DocumentId::minted(), ResourceType::new("note"));
    {
        let mut owners = ResourceOwners::writing(started(socket, launches, (app, kind), workspace));
        let store = owners.open(app, workspace).expect("the store opens");
        store.create_resource(&note).expect("the record is made");
        store.with(&note.id, |doc| {
            automerge::transaction::Transactable::put(doc, automerge::ROOT, "body", "kept")
                .expect("a text puts");
            doc.commit();
        });
        store.flush().expect("the workspace took the record");
    }

    let process = process(socket, launches, (app, kind), workspace);
    let mut answers: Vec<std::time::Duration> = (0..40)
        .map(|_| {
            let command = serde_json::to_vec(&tilo_resource_store::wire::Command::RecordChanges {
                record: Some(note.id.route().to_owned()),
                after: Vec::new(),
                from: 0,
            })
            .expect("a read encodes");
            let asked = std::time::Instant::now();
            collaboration::resource_command(
                app,
                uuid::Uuid::parse_str(workspace).expect("a workspace id"),
                command,
            )
            .expect("the read is sent")
            .recv()
            .expect("the service answers")
            .expect("the service reads the record");
            asked.elapsed()
        })
        .collect();
    answers.sort();
    eprintln!(
        "the workspace service answers a read in {:?} (median), {:?} (slowest of {})",
        answers[answers.len() / 2],
        answers[answers.len() - 1],
        answers.len()
    );

    let raw: Rc<dyn DocumentPort> = Rc::new(process.port);
    let mut owners = ResourceOwners::writing(raw);
    let store = owners.open(app, workspace).expect("the store opens");
    let asked = std::time::Instant::now();
    let opened = store.try_open_resource(&note);
    let took = asked.elapsed();
    assert!(
        took < std::time::Duration::from_millis(250),
        "the window's thread waited {took:?} for the workspace"
    );
    match opened {
        Ok(_) => {}
        Err(collaboration::resource::ResourceOpenError::Arriving) => {
            answered(&process.port);
            store
                .try_open_resource(&note)
                .expect("the record opens once the service answered");
        }
        Err(error) => panic!("the record neither opened nor is on its way: {error}"),
    }
    assert_eq!(
        store
            .read(&note.id, |doc| {
                automerge::ReadDoc::get(doc, automerge::ROOT, "body")
                    .ok()
                    .flatten()
                    .and_then(|(value, _)| value.into_string().ok())
            })
            .flatten()
            .as_deref(),
        Some("kept")
    );
}

/// The scenarios every port answers alike run against the workspace service, over the app's
/// connection, in two workspaces the device holds while it is offline. Then a write the
/// workspace refuses, and one made after the service stopped, are each said, and nothing claims
/// them saved.
#[test]
fn an_apps_records_are_kept_by_the_workspace_service_over_its_connection() {
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    let launches = Arc::new(Launches::default());
    let (mut session, first) = offline_session(&mut home, root.path(), Arc::clone(&launches));
    let socket = root.path().join("app_wire.sock");
    let tasks = format!("tl.{APP}.Tasks");

    // The app says when it is in the first workspace and when it holds a saved record; it hears
    // which the second workspace is, and when the service under it stopped.
    let (joined, in_first) = mpsc::channel::<()>();
    let (made, second) = mpsc::channel::<String>();
    let (holding, held) = mpsc::channel::<()>();
    let (stopped, stop) = mpsc::channel::<()>();
    std::thread::scope(|scope| {
        // The app's thread owns its ends of the channels; the rest it borrows.
        let (socket, launches, tasks, first) = (&socket, &launches, &tasks, &first);
        let app = scope.spawn(move || {
            let mut start = |workspace: &str| started(socket, launches, (APP, tasks), workspace);
            drop(start(first));
            joined.send(()).expect("the test waits");
            let second = second.recv().expect("the second workspace");
            conformance::run(&mut start, APP, [first, &second]);
            a_record_the_service_answers_late_opens_without_waiting(
                socket,
                launches,
                (APP, tasks),
                first,
            );

            // A workspace this device is not in refuses the app's record, and the app says so
            // rather than claiming it kept.
            let stranger = uuid::Uuid::new_v4().to_string();
            let port = start(first);
            let note = DocumentCheckout::new(DocumentId::minted(), ResourceType::new("note"));
            let mut owners = ResourceOwners::writing(port.clone());
            let store = owners.open(APP, &stranger).expect("the store opens");
            let refused = store
                .create_resource(&note)
                .map_err(|error| error.to_string())
                .and_then(|()| store.flush());
            let why = refused.expect_err("a workspace the device is not in keeps nothing");
            assert!(!why.is_empty(), "the refusal says why");
            assert_ne!(
                store.save_status(&note.id).ok(),
                Some(ResourceSaveStatus::Saved),
                "a refused record is never claimed saved"
            );
            drop((store, owners));

            // The service stops while the app holds a record with an edit not sent yet: the
            // write is said to have failed, never claimed saved.
            let mut owners = ResourceOwners::writing(port);
            let store = owners.open(APP, first).expect("the store opens");
            store.create_resource(&note).expect("the record is made");
            store.flush().expect("the workspace took the record");
            holding.send(()).expect("the test waits");
            stop.recv().expect("the service stopped");
            store.with(&note.id, |doc| {
                automerge::transaction::Transactable::put(
                    doc,
                    automerge::ROOT,
                    "body",
                    "typed after the service stopped",
                )
                .expect("a text puts");
                doc.commit_with(
                    automerge::transaction::CommitOptions::default()
                        .with_message(tilo_actions::FrameworkChange::Native.change_message()),
                );
            });
            let why = store
                .flush()
                .expect_err("a write with no service to take it is said");
            assert!(!why.is_empty(), "the failure says why");
            assert_ne!(
                store.save_status(&note.id).expect("the record is open"),
                ResourceSaveStatus::Saved,
                "an edit the workspace never took is not claimed saved: {why}"
            );
        });
        // The workspace the shell started with is the person's first once the device knows they
        // have no other, and only then are apps let into it: one made before that would stand in
        // its place. So the second is made once the app is in the first.
        assert!(
            reached(&mut session, |_| in_first.try_recv().is_ok()
                || app.is_finished()),
            "the app never joined the first workspace"
        );
        if !app.is_finished() {
            let mut arguments = serde_json::Map::new();
            arguments.insert("name".into(), "Second".into());
            session
                .invoke(CHROME_RESOURCE, "new-workspace", &arguments)
                .expect("the person makes a second workspace");
            until(
                &mut session,
                "the session holds a second workspace",
                |session| front_workspace(session).is_some_and(|front| front != *first),
            );
            let second = front_workspace(&mut session).expect("the second workspace");
            made.send(second).expect("the app waits for it");
            assert!(
                reached(&mut session, |_| held.try_recv().is_ok()
                    || app.is_finished()),
                "the app's processes never finished"
            );
        }
        drop(session);
        let _ = stopped.send(());
        app.join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
    });
}

/// The slide titles of `app`'s deck, in order.
fn slide_titles(app: &tilo_app_test_host::TestApp<slides::SlidesShell>) -> Vec<String> {
    slides::deck::mirror(&app.state())
        .slides
        .iter()
        .map(|slide| {
            slide
                .elements
                .iter()
                .find(|element| element.kind == slides::model::ElementKind::Text)
                .map(|element| element.text.clone())
                .unwrap_or_default()
        })
        .collect()
}

/// A deck as the workspace service keeps it: a title and two slides written in a Slides process,
/// the process ended, and a new one started on a new connection in the same tab, as the shell
/// restores it, reopens the deck whole. Opened in a new tab, as the shell opens the app after it
/// quit, Slides starts a new deck.
#[test]
fn slides_reopens_its_deck_after_a_restart_from_the_workspace_service() {
    use tilo_app::framework::ResourceApp as _;
    use tilo_app::tilo_resource::Resource as _;
    use tilo_app_test_host::TestApp;

    type Slides = TestApp<slides::SlidesShell>;
    let deck = (slides::SlidesShell::ID, slides::Deck::KIND);
    let written_whole = |app: &Slides| {
        assert_eq!(app.state().title, "Riverside review");
        assert_eq!(slide_titles(app), ["Kickoff", "Plan"]);
    };

    let _settled = ui::motion::testing::settled();
    ui::i18n::I18nManager::init("en-US");
    let root = crate::common::private_home();
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    let launches = Arc::new(Launches::default());
    let (mut session, workspace) = offline_session(&mut home, root.path(), Arc::clone(&launches));
    let socket = root.path().join("app_wire.sock");
    // Slides writes as the person the session runs as, whom the workspace's store names on every
    // write the person's apps make.
    let person = tilo_session::identity::own_person(&Options {
        person: PERSON.to_owned(),
        ..Options::default()
    });
    std::thread::scope(|scope| {
        let app = scope.spawn(|| {
            // Slides started in the shell's tab `tab`, as a process of its own on a new connection.
            let start = |tab: u64| {
                let documents = started(&socket, &launches, deck, &workspace);
                Slides::over_workspace(documents, &workspace, tab, &person)
            };
            let mut app = start(41).expect("Slides opens");
            app.submit(
                slides::Deck::paths()
                    .title
                    .replace("Riverside review".to_owned()),
            )
            .expect("Slides takes the title");
            for title in ["Kickoff", "Plan"] {
                app.call("add-slide", serde_json::json!({ "title": title }))
                    .expect("Slides adds the slide");
            }
            app.settle();
            written_whole(&app);
            let written = app.record().clone();
            // The process ends: its window closes, keeping what it wrote, and its connection goes
            // with it.
            app.flush()
                .expect("the workspace keeps what the window wrote");
            drop(app);

            let app = start(41).expect("Slides opens again");
            assert_eq!(app.record(), &written, "the tab reopens its deck");
            written_whole(&app);
            drop(app);

            let window = start(42).expect("a new tab");
            assert_ne!(window.record(), &written, "a new tab starts a new deck");
        });
        assert!(
            reached(&mut session, |_| app.is_finished()),
            "Slides' processes never finished"
        );
        drop(session);
        app.join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
    });
}

/// The labels of the rows the shell's search shows, wherever among what the session publishes.
fn search_rows(resources: &[tilo_actions::ResourceSpec]) -> Vec<String> {
    resources
        .iter()
        .find_map(|resource| {
            if resource.id.as_str() == tilo_session_core::desktop::verbs::SEARCH_RESOURCE {
                Some(
                    resource
                        .children
                        .iter()
                        .map(|row| row.label.clone())
                        .collect(),
                )
            } else {
                let rows = search_rows(&resource.children);
                (!rows.is_empty()).then_some(rows)
            }
        })
        .unwrap_or_default()
}

/// What a window writes is a record of its workspace, found where the workspace's records are
/// listed: a Slides deck under its title, written in a process of its own app over the workspace
/// service, is found by the shell's search as a person types its name. The app names nothing of it
/// itself: its window does, as every app's window does.
#[test]
fn what_an_apps_window_writes_is_listed_among_the_workspaces_records() {
    use tilo_app::framework::ResourceApp as _;
    use tilo_app::tilo_resource::Resource as _;
    use tilo_app_test_host::TestApp;

    type Slides = TestApp<slides::SlidesShell>;
    let deck = (slides::SlidesShell::ID, slides::Deck::KIND);

    let _settled = ui::motion::testing::settled();
    ui::i18n::I18nManager::init("en-US");
    let root = crate::common::private_home();
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    let launches = Arc::new(Launches::default());
    let (mut session, workspace) = offline_session(&mut home, root.path(), Arc::clone(&launches));
    let socket = root.path().join("app_wire.sock");
    let person = tilo_session::identity::own_person(&Options {
        person: PERSON.to_owned(),
        ..Options::default()
    });
    std::thread::scope(|scope| {
        let app = scope.spawn(|| {
            let documents = started(&socket, &launches, deck, &workspace);
            let mut slides =
                Slides::over_workspace(documents, &workspace, 42, &person).expect("Slides opens");
            slides
                .submit(
                    slides::Deck::paths()
                        .title
                        .replace("Riverside review".to_owned()),
                )
                .expect("Slides takes the title");
            slides.settle();
            slides
                .flush()
                .expect("the workspace keeps what the window wrote");
        });
        assert!(
            reached(&mut session, |_| app.is_finished()),
            "the app's process never finished"
        );
        app.join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
    });

    let name = "Riverside review";
    let mut arguments = serde_json::Map::new();
    arguments.insert("query".into(), name.into());
    session
        .invoke(
            tilo_session_core::desktop::verbs::SHELL_RESOURCE,
            "search",
            &arguments,
        )
        .expect("the person opens search on the name");
    assert!(
        reached(&mut session, |session| {
            search_rows(&session.resources())
                .iter()
                .any(|row| row.starts_with(name))
        }),
        "search finds {name:?} among the workspace's records: {:?}",
        search_rows(&session.resources())
    );
}
