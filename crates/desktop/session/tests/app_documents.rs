//! An app's records are kept by the workspace service the app reaches over its own connection:
//! its store reads and writes them with the record commands on the route the app joined, the
//! session's broker hands them to the device's one store, and that store answers from the
//! workspace it holds. Nothing here stands in for the service: the session is the shell of a
//! device daemon of the test's own ([`crate::common::on_its_daemon_with`]), which holds the
//! person's own workspace and serves the app wire its apps connect on, and the app's processes are
//! this test's, admitted as the launches the session recorded.
//!
//! The app is installed in the device, and in each workspace as its person installs it there: the
//! kinds its new records may be of are the ones its installed manifest declares, and keeping its
//! records is the grant installing it in the workspace gives it. Its surface, which this process
//! registers as the app's, declares the kind its records are of and the content edit the
//! framework writes them with: a record whose fields keep no container founds with the same
//! change on every device, which is what the store admits.
//!
//! The device is offline throughout: its only relay never answers, so every read and write is
//! answered by the device's own service.

#![cfg(target_os = "linux")]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc;

use collaboration::resource::{
    ConnectionDocuments, DocumentAsk, DocumentCheckout, DocumentId, DocumentPort, DocumentRead,
    ResourceOwners, ResourceSaveStatus, ResourceType, conformance,
};
use tilo_actions::remote::RemoteActionTarget;
use tilo_compositor::OutputConfig;
use tilo_session::{AppSocket, Launches, Options, Session};
use tilo_session_client::daemon_documents::{DaemonShell, inherited_link};
use tilo_session_core::desktop::verbs::CHROME_RESOURCE;

use crate::common::{reached, until, wait_for};

/// The app whose processes the test runs: one of the test's own, whose records are its own kind.
const APP: &str = "so.tilo.test-documents";

/// The kind the app's records are of: the scenarios write documents of it, under the id each
/// mints. It is the app's, as its surface and its installed manifest declare.
const KIND: &str = "note";

/// The app's surface, compiled: the record its processes write, whose fields are the ones the
/// scenarios write, and the content edit the framework writes a record with on every kind. A
/// record whose fields keep no container founds with a change every holder derives alike.
const SURFACE: &str = r#"{"reads":[],"resources":[{"fields":[{"path":"body","type":"text"},{"path":"first","type":"text"},{"path":"second","type":"text"}],"kind":"note","label":"Note","verbs":[{"id":"type","kind":"undoable","label":"Edit"}],"version":0}]}"#;

/// The app, as this process knows it: its surface registered ([`collab_types::app_surface`]), so
/// the store declares the kind its records are of, and the hash its installed version names.
fn register_app() -> String {
    let surface = collab_types::surface::canonical(
        &serde_json::from_str(SURFACE).expect("the app's surface is JSON"),
    );
    let bytes = surface.into_bytes();
    let hash = *blake3::hash(&bytes).as_bytes();
    collab_types::app_surface::Surfaces::process().register_package(APP, &bytes);
    hex::encode(hash)
}

/// Install `app` in the device's store, as a device has it: a package whose signed manifest
/// declares `kind` among the kinds its records are of, which is what the device reads before an
/// app founds a record of one.
fn install_on_device(store: &std::path::Path, app: &str, kind: &str) {
    let payload = store.join("payload").join(app);
    std::fs::create_dir_all(payload.join("bin")).expect("the app's payload");
    // A program the launcher may run: a package the device has installed is started when its
    // record is read, and this one runs nothing.
    let program = payload.join("bin/app");
    std::fs::write(&program, "#!/bin/sh\nexit 0\n").expect("the app's program");
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
            .expect("the app's program runs");
    }
    let manifest = payload.join("manifest.toml");
    std::fs::write(
        &manifest,
        format!(
            "[package]\n\
             id = \"{app}\"\n\
             version = \"1.0.0\"\n\
             kind = \"app\"\n\
             name = \"Documents\"\n\
             \n\
             [publisher]\n\
             identity = \"ed25519:{key}\"\n\
             signature = \"\"\n\
             \n\
             [sandbox]\n\
             namespaces = [\"user\"]\n\
             seccomp = \"tilo-default\"\n\
             landlock = \"strict\"\n\
             \n\
             [sandbox.net]\n\
             mode = \"off\"\n\
             \n\
             [entrypoint]\n\
             bin = \"bin/app\"\n\
             type = \"gui\"\n\
             \n\
             [resources]\n\
             kinds = [\"{kind}\"]\n\
             \n\
             [providers]\n\
             schemes = [\"documents\"]\n",
            key = "11".repeat(32),
        ),
    )
    .expect("the app's manifest");
    // The app's row goes beside the rows of the apps installed before it.
    let registry = store.join("registry.toml");
    let rows = std::fs::read_to_string(&registry).unwrap_or_default();
    std::fs::write(
        &registry,
        format!(
            "{rows}[[entry]]\nid = \"{app}\"\nversion = \"1.0.0\"\n\
             payload_root = \"{payload}\"\nmanifest_path = \"{manifest}\"\n",
            payload = payload.display(),
            manifest = manifest.display(),
        ),
    )
    .expect("the app's row");
}

/// A session as a computer starts one: the shell of a device daemon of its own
/// ([`crate::common::on_its_daemon_with`]), which holds the person's own workspace and the keys
/// its records are sealed with. The app is installed in the daemon's store, as a device has the
/// apps its person installed there. Answers the session once the shell shows the workspace it
/// holds, that workspace, and the person's own link to their daemon: the link an install in a
/// workspace goes over.
fn session(root: &std::path::Path, launches: Arc<Launches>) -> (Session, String, DaemonShell) {
    let link = RefCell::new(None);
    let mut session = crate::common::on_its_daemon_with(
        &[OutputConfig::new(
            "HEADLESS-1",
            crate::common::WIDTH,
            crate::common::HEIGHT,
        )],
        |device| {
            // The apps this computer installed: a new record is of a kind its installed manifest
            // declares, and the daemon reads the store a person's apps are installed in.
            install_on_device(&device.store(), APP, KIND);
            // The person's own link to their daemon: what installs the app in a workspace, as
            // their own install does.
            link.replace(Some(
                device
                    .daemon
                    .link_person(tilo_test_env::hang_bound())
                    .expect("the daemon makes the person's link"),
            ));
            Options {
                collab: true,
                state: Some(root.join("state")),
                asks: Some(tilo_service::asks::channel()),
                app_socket: AppSocket::At(root.join("app_wire.sock")),
                mcp_server: Some(root.join("agent.sock")),
                launches,
                ..Options::default()
            }
        },
    );
    let workspace = wait_for(
        &mut session,
        "the workspace the shell shows",
        front_workspace,
    );
    // The daemon holds the workspace once it says so: until then it answers no call in it, an
    // install's included.
    crate::common::kept_in_workspace(&mut session);
    let runtime = tilo_rt::app().handle().clone();
    let shell = DaemonShell::new(
        inherited_link(
            Some(link.into_inner().expect("the person's link")),
            &runtime,
        )
        .map(|(link, _)| link),
        runtime,
    );
    (session, workspace, shell)
}

/// Ask `verb` of the workspace's own actions, as the person does over their own link to their
/// daemon: what the workspace answers.
fn ask_the_workspace(
    session: &mut Session,
    shell: &DaemonShell,
    workspace: &str,
    verb: &str,
    arguments: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let request = crate::common::invocation(
        &RemoteActionTarget::workspace_governance(workspace, verb),
        arguments,
        None,
        None,
    );
    let (answered, answer) = mpsc::sync_channel(1);
    let (shell, workspace) = (shell.clone(), workspace.to_owned());
    let runtime = shell.runtime().clone();
    let _task = runtime.spawn(async move {
        let settled = match shell.link() {
            Ok(link) => link.dispatch().settled(Some(workspace), request).await,
            Err(error) => Err(error),
        };
        let _ = answered.send(settled);
    });
    wait_for(session, "the workspace's answer", |_| {
        answer.try_recv().ok()
    })
}

/// Install `app`, whose installed version names the surface `surface`, in `workspace` through the
/// workspace's own install action, as its person does: what lets the app keep its records there.
fn install_in_workspace(
    session: &mut Session,
    shell: &DaemonShell,
    workspace: &str,
    app: &str,
    surface: &str,
) {
    ask_the_workspace(
        session,
        shell,
        workspace,
        "install-app",
        &serde_json::json!({
            "app": app,
            "version": "1.0.0",
            "surface": surface,
            "package": "00".repeat(32),
            "publisher": "00".repeat(32),
            "dev": true,
        }),
    )
    .unwrap_or_else(|error| panic!("the person installs {app} in the workspace: {error}"));
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
    // shell shows the workspace, and the daemon's holder takes the workspace a moment after it
    // starts: until then a join is refused, as an app started that early would be, and it joins
    // again.
    let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
    loop {
        match join() {
            Ok(manager) => return manager,
            Err(why)
                if (why.contains("not a member")
                    || why.contains("not available on this device")
                    || why.contains("no identity to sign in with"))
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
        kind: &str,
        changes: Vec<u8>,
    ) -> collaboration::resource::Written {
        self.port.write(app, workspace, id, kind, changes)
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
            doc.commit_with(
                automerge::transaction::CommitOptions::default().with_message(
                    tilo_actions::ActionContext::whole_edit_by(&tilo_actions::Acting::by_app(app)),
                ),
            );
        });
        store.flush().expect("the workspace took the record");
    }

    let process = process(socket, launches, (app, kind), workspace);
    let mut answers: Vec<std::time::Duration> = (0..40)
        .map(|_| {
            let command = serde_json::to_vec(&collab_types::store::wire::Command::RecordChanges {
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
/// connection, in two workspaces the device holds while it is offline. Then a write the workspace
/// refuses, and one made after the service stopped, are each said, and nothing claims them saved.
#[test]
fn an_apps_records_are_kept_by_the_workspace_service_over_its_connection() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let root = crate::common::private_home();
    let launches = Arc::new(Launches::default());
    // The app's surface, before the device starts: what the store declares the kind its records
    // are of by, and the hash its installed version names.
    let surface = register_app();
    let (mut session, first, shell) = session(root.path(), Arc::clone(&launches));
    let socket = root.path().join("app_wire.sock");
    let kind = KIND;
    // The person installs the app in the workspace they show, through the workspace's own install
    // action: the grant that lets the app keep its records there.
    install_in_workspace(&mut session, &shell, &first, APP, &surface);

    // The app says when it is in the first workspace and when it holds a saved record; it hears
    // which the second workspace is, and when the service under it stopped.
    let (joined, in_first) = mpsc::channel::<()>();
    let (made, second) = mpsc::channel::<String>();
    let (holding, held) = mpsc::channel::<()>();
    let (stopped, stop) = mpsc::channel::<()>();
    std::thread::scope(|scope| {
        // The app's thread owns its ends of the channels; the rest it borrows.
        let (socket, launches, kind, first) = (&socket, &launches, &kind, &first);
        let app = scope.spawn(move || {
            let mut start = |workspace: &str| started(socket, launches, (APP, kind), workspace);
            drop(start(first));
            joined.send(()).expect("the test waits");
            let second = second.recv().expect("the second workspace");
            conformance::run(&mut start, APP, [first, &second]);
            a_record_the_service_answers_late_opens_without_waiting(
                socket,
                launches,
                (APP, kind),
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
                    automerge::transaction::CommitOptions::default().with_message(
                        tilo_actions::ActionContext::whole_edit_by(&tilo_actions::Acting::by_app(
                            APP,
                        )),
                    ),
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
            // The daemon holds the second workspace once it says so: until then it answers no
            // call in it, an install's included.
            until(
                &mut session,
                "the device holds the second workspace",
                |session| session.held_workspaces().iter().any(|held| held == &second),
            );
            // The app is installed in the second workspace too, as it is in the first: the
            // workspace keeps its records only where its person installed it.
            install_in_workspace(&mut session, &shell, &second, APP, &surface);
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
