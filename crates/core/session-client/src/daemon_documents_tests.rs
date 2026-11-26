#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures: a failed step is the assertion"
)]

//! The desktop's reads and writes of its person's workspaces, answered by a device's dispatch on the
//! shell link the daemon made, as the daemon answers them.

use std::os::unix::fs::PermissionsExt as _;
use std::sync::{Arc, Mutex};

use tilo_app_host::services::{
    Caller, ProcessServices, PublishedResources, Reading, ResourceWatch,
};
use tilo_app_host::shell::{ShellActions, ShellCaller, ShellDispatch, ShellLinks, ShellServices};
use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_DOCUMENT, DESKTOP_KIND};
use tilo_peer_state::owner::WriteRefusal;
use tilo_peer_state::workspace_documents::{
    DeviceWrites, DocumentRead, DocumentServing, DocumentWrite, DocumentWriting,
};
use tilo_workspace_service::device_documents::{ServiceDocuments, ServiceWrites};
use tilo_workspace_service::workspaces::ResourceWrite;

use super::{DaemonShell, DaemonWritesFeed, NOT_STARTED_BY_THE_DAEMON, on_daemon};

const WORKSPACE: &str = "0b9d3c34-4c2e-4d70-9a4e-6c0f2a3b1d77";

/// A write the daemon took: its workspace, record, changes and the kind it named.
type Written = (String, String, Vec<u8>, Option<String>);

/// The device's writes as a test's daemon takes them: each one kept, and refused, as a daemon
/// whose history is full refuses it.
struct Kept {
    written: Mutex<Vec<Written>>,
}

impl DeviceWrites for Kept {
    fn write(
        &self,
        workspace: &str,
        changes: &[u8],
        writing: DocumentWriting<'_>,
    ) -> DocumentWrite {
        self.written.lock().unwrap().push((
            workspace.to_owned(),
            writing.record.to_owned(),
            changes.to_vec(),
            writing.kind.map(str::to_owned),
        ));
        Box::pin(std::future::ready(Err(WriteRefusal::HistoryFull {
            app: writing.app.to_owned(),
            record: writing.record.to_owned(),
        })))
    }

    fn served(&self, _workspace: &str, _documents: &[(&str, &str)]) -> Option<DocumentServing> {
        Some(Box::pin(std::future::ready(Err(
            "the serving device refused the layout".to_owned(),
        ))))
    }
}

struct NoActions;

impl ShellActions for NoActions {
    fn dispatch(
        &self,
        _: &ShellCaller,
        _: Option<String>,
        _: Option<runtime_collab::wire::shell::ShellPress>,
    ) -> ShellDispatch {
        Box::pin(std::future::ready(Err("no actions here".to_owned())))
    }
}

struct NoStanding;

impl PublishedResources for NoStanding {
    fn snapshot(
        &self,
        _caller: &Caller,
        _resource: &str,
    ) -> tokio::sync::oneshot::Receiver<Result<Reading, String>> {
        let (reply, answered) = tokio::sync::oneshot::channel();
        let _ = reply.send(Ok((0, None)));
        answered
    }

    fn watch(
        &self,
        _caller: &Caller,
        _resource: &str,
        _sink: tokio::sync::mpsc::UnboundedSender<runtime_collab::wire::Event>,
    ) -> Result<ResourceWatch, String> {
        Err("nothing stands here".to_owned())
    }

    fn unwatch(&self, _watch: ResourceWatch) {}

    fn invoke(
        &self,
        _caller: &Caller,
        _resource: &str,
        _action: &str,
        _arguments: Vec<u8>,
    ) -> tokio::sync::oneshot::Receiver<runtime_collab::wire::Receipt> {
        tokio::sync::oneshot::channel().1
    }
}

/// A device's dispatch answering its person's shell from the workspace it holds, its writes taken
/// by `kept`, and what tells of its writes.
struct Daemon {
    _dir: tempfile::TempDir,
    host: tilo_workspace_service::workspaces::fixtures::TestHost,
    _dispatch: tilo_app_host::collab_broker::BrokerHandle,
    socket: std::path::PathBuf,
    links: Arc<ShellLinks>,
    kept: Arc<Kept>,
    writes: tokio::sync::broadcast::Sender<ResourceWrite>,
}

impl Daemon {
    /// Whom the daemon makes its shell's links for: the person its device acts for.
    fn person(&self) -> ShellCaller {
        ShellCaller {
            person: self.host.workspaces().device_cert().user_public_key,
        }
    }

    /// The shell link the daemon makes for the shell it starts, opened on the current runtime.
    async fn link(&self) -> runtime_collab::ShellClient {
        use tokio::io::AsyncWriteExt as _;
        let secret = self.links.make(self.person());
        let mut link = tokio::net::UnixStream::connect(&self.socket).await.unwrap();
        let mut opening = runtime_collab::wire::shell::SHELL_MAGIC.to_vec();
        opening.extend_from_slice(&secret);
        link.write_all(&opening).await.unwrap();
        runtime_collab::ShellClient::open(link).0
    }
}

/// The desktop's layout as the daemon keeps it, on the founding every copy of the record
/// derives and naming `person`, whose device the daemon is.
fn layout(person: &str) -> automerge::AutoCommit {
    let mut layout = tilo_desktop_model::record::founded(WORKSPACE).expect("the layout founds");
    automerge::transaction::Transactable::put(&mut layout, automerge::ROOT, "front", "Writer")
        .unwrap();
    layout.commit_with(
        automerge::transaction::CommitOptions::default().with_message(
            tilo_actions::ActionContext::whole_edit_by(&tilo_actions::Acting::person(person)),
        ),
    );
    layout
}

fn daemon(runtime: &tokio::runtime::Runtime) -> Daemon {
    use tilo_workspace_service::workspaces::fixtures::{hosting_device, test_host};
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = dir.path().join("collab.sock");
    let (key, cert) = hosting_device();
    let host = runtime.block_on(test_host(&dir.path().join("host"), &key, &cert));
    let workspaces = host.workspaces().clone();
    // The daemon holds the person's workspace and keeps its desktop's layout there, written as
    // the device, as its daemon writes what its desktop changes.
    let person = hex::encode(cert.user_public_key);
    workspaces.set_member_device(Arc::new(tilo_workspace_service::workspaces::MemberDevice {
        cert,
        signer: Arc::new(key),
    }));
    runtime
        .block_on(workspaces.create_workspace("Ada's", Some(WORKSPACE)))
        .unwrap();
    let documents = ServiceDocuments::of(workspaces.clone());
    runtime
        .block_on(ServiceWrites::of(workspaces.clone()).write(
            WORKSPACE,
            &layout(&person).save(),
            DocumentWriting {
                app: DESKTOP_APP,
                kind: Some(DESKTOP_KIND),
                record: DESKTOP_DOCUMENT,
            },
        ))
        .unwrap();
    let kept = Arc::new(Kept {
        written: Mutex::default(),
    });
    let links = Arc::new(ShellLinks::default());
    let (writes, _) = tokio::sync::broadcast::channel(16);
    let told = writes.clone();
    let services = ProcessServices {
        shell: Some(Arc::new(ShellServices {
            processes: None,
            links: Arc::clone(&links),
            socket: socket.clone(),
            documents: Arc::new(documents),
            writes: Arc::clone(&kept) as _,
            document_writes: Arc::new(move || told.subscribe()),
            actions: Arc::new(NoActions),
            standing: Arc::new(NoStanding),
            published: None,
            workspaces,
            identity: None,
            api: None,
            live: None,
            speaking: None,
        })),
        ..ProcessServices::default()
    };
    let dispatch = tilo_app_host::collab_broker::spawn_with_services(
        socket.clone(),
        None,
        tilo_app_host::collab_outbound::PeerPublisher::disabled(),
        None,
        services,
        Arc::default(),
        Arc::new(tilo_app_host::collab_broker::FanoutState::new()),
    )
    .unwrap();
    Daemon {
        _dir: dir,
        host,
        _dispatch: dispatch,
        socket,
        links,
        kept,
        writes,
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn the_desktop_reads_and_writes_its_workspaces_through_the_daemon() {
    let runtime = runtime();
    let daemon = daemon(&runtime);
    let link = runtime.block_on(daemon.link());
    let (documents, writes, _shell) = on_daemon(Ok(link), runtime.handle().clone());

    // What the daemon keeps of the person's workspace reads as it is kept there: the layout whole,
    // at the heads it stands at, whatever the desktop holds of it already; a note it never kept
    // absent.
    let DocumentRead::Whole { heads, bytes, .. } = documents
        .read(WORKSPACE, DESKTOP_APP, DESKTOP_DOCUMENT, None)
        .unwrap()
    else {
        panic!("the daemon reads the layout whole");
    };
    let read = automerge::AutoCommit::load(&bytes).unwrap();
    assert_eq!(
        automerge::ReadDoc::get(&read, automerge::ROOT, "front")
            .unwrap()
            .map(|(value, _)| value.to_string()),
        Some("\"Writer\"".to_owned())
    );
    assert!(matches!(
        documents
            .read(WORKSPACE, DESKTOP_APP, DESKTOP_DOCUMENT, Some(&heads))
            .unwrap(),
        DocumentRead::Whole { heads: again, .. } if again == heads
    ));
    assert_eq!(
        documents
            .read(WORKSPACE, "so.tilo.notes", "today", None)
            .unwrap(),
        DocumentRead::Absent
    );
    assert!(documents.readable(WORKSPACE, DESKTOP_APP, DESKTOP_DOCUMENT));
    assert!(!documents.readable(WORKSPACE, "so.tilo.notes", "today"));
    assert!(documents.holds(WORKSPACE, DESKTOP_APP, DESKTOP_DOCUMENT));
    assert!(!documents.holds(WORKSPACE, "so.tilo.notes", "today"));
    assert_eq!(documents.workspaces(), vec![WORKSPACE.to_owned()]);
    let listed = documents.listing(WORKSPACE).unwrap();
    assert!(
        listed
            .iter()
            .any(|listed| listed.address.app == DESKTOP_APP
                && listed.address.record == DESKTOP_DOCUMENT),
        "{listed:?}"
    );
    // A workspace the daemon does not keep is refused as the daemon says it.
    let elsewhere = documents
        .read(
            "5d0c1f0e-2b7a-4f3e-9c1d-8a6b4e2f0a11",
            DESKTOP_APP,
            DESKTOP_DOCUMENT,
            None,
        )
        .unwrap_err();
    assert!(
        elsewhere.contains(tilo_actions::remote::RESOURCE_READ_REFUSED),
        "{elsewhere}"
    );

    // A write reaches the daemon, which keeps it as its device's; its refusal comes back as the
    // daemon's own.
    let refused = runtime
        .block_on(writes.write(
            WORKSPACE,
            b"a moved window",
            DocumentWriting {
                app: DESKTOP_APP,
                kind: Some(DESKTOP_KIND),
                record: DESKTOP_DOCUMENT,
            },
        ))
        .expect_err("the daemon refused the write");
    assert_eq!(
        refused,
        WriteRefusal::HistoryFull {
            app: DESKTOP_APP.to_owned(),
            record: DESKTOP_DOCUMENT.to_owned(),
        }
    );
    assert_eq!(
        *daemon.kept.written.lock().unwrap(),
        vec![(
            WORKSPACE.to_owned(),
            DESKTOP_DOCUMENT.to_owned(),
            b"a moved window".to_vec(),
            Some(DESKTOP_KIND.to_owned())
        )]
    );
    let served = runtime
        .block_on(
            writes
                .served(WORKSPACE, &[(DESKTOP_APP, DESKTOP_DOCUMENT)])
                .unwrap(),
        )
        .unwrap_err();
    assert_eq!(served, "the serving device refused the layout");
}

#[test]
fn the_desktop_hears_each_write_the_daemon_took() {
    let runtime = runtime();
    let daemon = daemon(&runtime);
    let link = runtime.block_on(daemon.link());
    let shell = DaemonShell::new(Ok(link), runtime.handle().clone());
    let feed = DaemonWritesFeed::start(&shell);
    let mut followed = feed.follow();
    runtime.block_on(async {
        // The shell's connection follows the daemon's writes from the moment it opened: one taken
        // before the desktop asked is told too.
        shell
            .ask(runtime_collab::wire::shell::ShellCall::Workspaces)
            .await
            .unwrap();
        daemon
            .writes
            .send(ResourceWrite {
                workspace: WORKSPACE.to_owned(),
                app: DESKTOP_APP.to_owned(),
                record: DESKTOP_DOCUMENT.to_owned(),
                together: Vec::new(),
            })
            .unwrap();
        let told = tokio::time::timeout(std::time::Duration::from_secs(10), followed.recv())
            .await
            .expect("the write is told")
            .unwrap();
        assert_eq!(
            (
                told.workspace.as_str(),
                told.app.as_str(),
                told.record.as_str()
            ),
            (WORKSPACE, DESKTOP_APP, DESKTOP_DOCUMENT)
        );
    });
}

#[test]
fn a_desktop_the_daemon_did_not_start_says_so() {
    let runtime = runtime();
    let (documents, _, _) = on_daemon(
        Err(NOT_STARTED_BY_THE_DAEMON.to_owned()),
        runtime.handle().clone(),
    );
    let refused = documents
        .read(WORKSPACE, DESKTOP_APP, DESKTOP_DOCUMENT, None)
        .unwrap_err();
    assert_eq!(refused, NOT_STARTED_BY_THE_DAEMON);
    assert!(documents.workspaces().is_empty());
}

/// A desktop frames no device write itself: it holds no workspace and no key, so a write with no
/// daemon to sign it is refused as the daemon's absence, and none is drafted or kept here. The
/// daemon alone signs what the device writes, under its own sequence.
#[test]
fn a_desktop_the_daemon_did_not_start_frames_no_write_of_its_own() {
    let runtime = runtime();
    let (documents, writes, shell) = on_daemon(
        Err(NOT_STARTED_BY_THE_DAEMON.to_owned()),
        runtime.handle().clone(),
    );
    let refused = runtime
        .block_on(writes.write(
            WORKSPACE,
            b"a moved window",
            DocumentWriting {
                app: "so.tilo.desktop",
                kind: Some(DESKTOP_KIND),
                record: "layout",
            },
        ))
        .expect_err("no daemon, no write");
    assert_eq!(
        refused,
        WriteRefusal::Refused(NOT_STARTED_BY_THE_DAEMON.to_owned())
    );
    // A document the desktop would have founded is not founded here either, and nothing it read
    // is kept: there is no copy to frame a write into.
    assert!(!documents.holds(WORKSPACE, "so.tilo.desktop", "layout"));
    assert!(documents.workspaces().is_empty());
    // Nor does the shell reach any daemon to record one with.
    assert!(
        runtime
            .block_on(shell.ask(runtime_collab::wire::shell::ShellCall::Workspaces))
            .is_err(),
        "the link is not there to ask"
    );
}

#[test]
fn a_link_is_taken_once() {
    let runtime = runtime();
    let daemon = daemon(&runtime);
    let secret = daemon.links.make(daemon.person());
    let open = |secret: [u8; 32]| {
        let socket = daemon.socket.clone();
        runtime.block_on(async move {
            use tokio::io::AsyncWriteExt as _;
            let mut link = tokio::net::UnixStream::connect(&socket).await.unwrap();
            let mut opening = runtime_collab::wire::shell::SHELL_MAGIC.to_vec();
            opening.extend_from_slice(&secret);
            link.write_all(&opening).await.unwrap();
            runtime_collab::ShellClient::open(link).0
        })
    };
    let first = DaemonShell::new(Ok(open(secret)), runtime.handle().clone());
    assert!(
        runtime
            .block_on(first.ask(runtime_collab::wire::shell::ShellCall::Workspaces))
            .is_ok()
    );
    // A second connection naming the taken secret is closed unanswered.
    let again = DaemonShell::new(Ok(open(secret)), runtime.handle().clone());
    assert!(
        runtime
            .block_on(again.ask(runtime_collab::wire::shell::ShellCall::Workspaces))
            .is_err()
    );
}

/// A desktop carries its link on one thread, as the app runtime does. A blocking ask made on that
/// thread would wait for frames only it could carry, and every ask after it with it: it is
/// refused at once, and the link goes on answering every other thread.
#[test]
fn an_ask_on_the_thread_that_carries_the_link_is_refused_and_the_link_keeps_answering() {
    use runtime_collab::wire::shell::{ShellCall, ShellReply};
    let runtime = runtime();
    let daemon = daemon(&runtime);
    let carrying = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let handle = carrying.handle().clone();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let carrier = std::thread::spawn(move || {
        carrying.block_on(async move {
            let _ = stopped.await;
        });
    });
    let secret = daemon.links.make(daemon.person());
    let socket = daemon.socket;
    let (opened, link) = std::sync::mpsc::channel();
    handle.spawn(async move {
        use tokio::io::AsyncWriteExt as _;
        let mut link = tokio::net::UnixStream::connect(&socket).await.unwrap();
        let mut opening = runtime_collab::wire::shell::SHELL_MAGIC.to_vec();
        opening.extend_from_slice(&secret);
        link.write_all(&opening).await.unwrap();
        opened
            .send(runtime_collab::ShellClient::open(link).0)
            .unwrap();
    });
    let shell = DaemonShell::new(Ok(link.recv().unwrap()), handle.clone());

    let on_the_link = shell.clone();
    let (told, refused) = std::sync::mpsc::channel();
    handle.spawn(async move {
        told.send(on_the_link.ask_blocking(ShellCall::Workspaces))
            .unwrap();
    });
    let refused = refused
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("an ask on the link's own thread is answered at once, never left waiting");
    assert_eq!(refused.unwrap_err(), super::ASKED_ON_THE_LINK);

    assert!(
        matches!(
            shell.ask_blocking(ShellCall::Workspaces),
            Ok(ShellReply::Workspaces(_))
        ),
        "the link goes on answering another thread"
    );
    drop(stop);
    carrier.join().unwrap();
}

#[test]
fn a_read_asked_on_the_sessions_loop_stops_there_and_one_off_it_is_answered() {
    let runtime = runtime();
    let daemon = daemon(&runtime);
    let link = runtime.block_on(daemon.link());
    let shell = DaemonShell::new(Ok(link), runtime.handle().clone());
    let read = || super::whole(WORKSPACE, DESKTOP_APP, DESKTOP_DOCUMENT);

    let on_the_loop = shell.clone();
    let asked = std::thread::spawn(move || {
        let _pass = super::OnTheLoop::mark();
        on_the_loop.read_blocking(read())
    })
    .join();
    // A build with debug assertions, every test's, stops at once; any other is refused at once.
    let said = match asked {
        Err(stopped) => stopped
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_default(),
        Ok(answered) => answered.expect_err("a read on the session's loop is refused"),
    };
    assert!(
        said.starts_with(super::READ_ON_THE_LOOP),
        "it says why: {said}"
    );

    // The same thread once its pass ended, and every other thread meanwhile, reads as before.
    let after = shell;
    let answered = std::thread::spawn(move || {
        drop(super::OnTheLoop::mark());
        after.read_blocking(read())
    })
    .join()
    .unwrap();
    assert!(
        answered.is_ok(),
        "a read off the loop is answered: {answered:?}"
    );
}
