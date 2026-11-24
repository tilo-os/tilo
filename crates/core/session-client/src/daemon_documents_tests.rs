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

use tilo_peer_state::owner::WriteRefusal;
use tilo_peer_state::workspace_documents::{
    DeviceWrites, DocumentRead, DocumentServing, DocumentWrite, DocumentWriting,
};
use tilo_workspace_service::apps::services::{
    Caller, ProcessServices, PublishedResources, Reading, ResourceWatch,
};
use tilo_workspace_service::apps::shell::{
    ShellActions, ShellCaller, ShellDispatch, ShellLinks, ShellServices,
};
use tilo_workspace_service::device_documents::{ServiceDocuments, ServiceWrites};
use tilo_workspace_service::workspaces::ResourceWrite;

use super::{DaemonShell, DaemonWritesFeed, NOT_STARTED_BY_THE_DAEMON, on_daemon};

const WORKSPACE: &str = "0b9d3c34-4c2e-4d70-9a4e-6c0f2a3b1d77";

/// A write the daemon took: its workspace, document, changes and whether it founds the document.
type Written = (String, String, Vec<u8>, bool);

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
            writing.document.to_owned(),
            changes.to_vec(),
            writing.founding,
        ));
        Box::pin(std::future::ready(Err(WriteRefusal::HistoryFull {
            app: writing.app.to_owned(),
            record: writing.document.to_owned(),
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
    _dispatch: tilo_workspace_service::apps::collab_broker::BrokerHandle,
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

/// The desktop's layout as the daemon keeps it.
fn layout() -> automerge::AutoCommit {
    let mut layout = automerge::AutoCommit::new();
    automerge::transaction::Transactable::put(&mut layout, automerge::ROOT, "front", "Writer")
        .unwrap();
    layout
}

fn daemon(runtime: &tokio::runtime::Runtime) -> Daemon {
    use tilo_workspace_service::workspaces::fixtures::{governor, test_host};
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = dir.path().join("collab.sock");
    let (key, cert) = governor();
    let host = runtime.block_on(test_host(&dir.path().join("host"), &key, &cert));
    let workspaces = host.workspaces().clone();
    // The daemon holds the person's workspace and keeps its desktop's layout there, written as
    // the device, as its daemon writes what its desktop changes.
    workspaces.set_member_device(Arc::new(tilo_workspace_service::workspaces::MemberDevice {
        cert: cert.clone(),
        signer: Arc::new(key.clone()),
    }));
    runtime
        .block_on(workspaces.create_workspace("Ada's", Some(WORKSPACE)))
        .unwrap();
    let documents = ServiceDocuments::of(workspaces.clone());
    runtime
        .block_on(ServiceWrites::of(workspaces.clone()).write(
            WORKSPACE,
            &layout().save(),
            DocumentWriting {
                app: "so.tilo.desktop",
                document: "layout",
                readers: None,
                founding: true,
                snapshot: true,
                sealed: None,
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
            live: None,
        })),
        ..ProcessServices::default()
    };
    let dispatch = tilo_workspace_service::apps::collab_broker::spawn_with_services(
        socket.clone(),
        None,
        tilo_workspace_service::apps::collab_outbound::PeerPublisher::disabled(),
        None,
        services,
        Arc::default(),
        Arc::new(tilo_workspace_service::apps::collab_broker::FanoutState::new()),
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
        .read(WORKSPACE, "so.tilo.desktop", "layout", None)
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
            .read(WORKSPACE, "so.tilo.desktop", "layout", Some(&heads))
            .unwrap(),
        DocumentRead::Whole { heads: again, .. } if again == heads
    ));
    assert_eq!(
        documents
            .read(WORKSPACE, "so.tilo.notes", "today", None)
            .unwrap(),
        DocumentRead::Absent
    );
    assert!(documents.readable(WORKSPACE, "so.tilo.desktop", "layout"));
    assert!(!documents.readable(WORKSPACE, "so.tilo.notes", "today"));
    assert!(documents.holds(WORKSPACE, "so.tilo.desktop", "layout"));
    assert!(!documents.holds(WORKSPACE, "so.tilo.notes", "today"));
    assert_eq!(documents.workspaces(), vec![WORKSPACE.to_owned()]);
    let listed = documents.listing(WORKSPACE).unwrap();
    assert!(
        listed.contains(&("so.tilo.desktop".to_owned(), "layout".to_owned())),
        "{listed:?}"
    );
    // A workspace the daemon does not keep is refused as the daemon says it.
    let elsewhere = documents
        .read(
            "5d0c1f0e-2b7a-4f3e-9c1d-8a6b4e2f0a11",
            "so.tilo.desktop",
            "layout",
            None,
        )
        .unwrap_err();
    assert!(
        elsewhere.contains(tilo_actions::remote::RESOURCE_READ_REFUSED),
        "{elsewhere}"
    );

    // A write reaches the daemon, which signs it; its refusal comes back as the daemon's own.
    let refused = runtime
        .block_on(writes.write(
            WORKSPACE,
            b"a moved window",
            DocumentWriting {
                app: "so.tilo.desktop",
                document: "layout",
                readers: None,
                founding: false,
                snapshot: false,
                sealed: None,
            },
        ))
        .err()
        .expect("the daemon refused the write");
    assert_eq!(
        refused,
        WriteRefusal::HistoryFull {
            app: "so.tilo.desktop".to_owned(),
            record: "layout".to_owned(),
        }
    );
    assert_eq!(
        *daemon.kept.written.lock().unwrap(),
        vec![(
            WORKSPACE.to_owned(),
            "layout".to_owned(),
            b"a moved window".to_vec(),
            false
        )]
    );
    let served = runtime
        .block_on(
            writes
                .served(WORKSPACE, &[("so.tilo.desktop", "layout")])
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
                app: "so.tilo.desktop".to_owned(),
                record: "layout".to_owned(),
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
            (WORKSPACE, "so.tilo.desktop", "layout")
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
        .read(WORKSPACE, "so.tilo.desktop", "layout", None)
        .unwrap_err();
    assert_eq!(refused, NOT_STARTED_BY_THE_DAEMON);
    assert!(documents.workspaces().is_empty());
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
