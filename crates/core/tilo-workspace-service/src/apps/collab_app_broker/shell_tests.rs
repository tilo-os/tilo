#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures: a failed step is the assertion"
)]

//! An app the person's shell launched, on the connection the shell attests and hands the device's
//! daemon over its link: served by the daemon as that app from then on.

use super::*;
use super::seat;
use crate::apps::services::{Caller, PublishedResources, Reading, ResourceWatch};
use crate::apps::shell::{ShellActions, ShellCaller, ShellDispatch, ShellLinks, ShellServices};
use ed25519_dalek::SigningKey;
use runtime_collab::wire::{
    Call, Event, FRAMING, HostFrame, Join, Operation, Receipt, Reply, Service,
};
use tilo_peer_state::workspace_documents::{
    DeviceDocuments, DeviceWrites, DocumentRead, DocumentServing, DocumentWrite, DocumentWriting,
};

/// Files: the person's app list is its only, and the shell's host keeps that list.
const APP: &str = tilo_app_id::apps::FILE_EXPLORER;
const RECORD: &str = "the-shelf";
/// What the daemon keeps of the app's record.
const KEPT: &[u8] = b"the record as the daemon keeps it";

fn device(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed.wrapping_add(100); 32])
}

fn root(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

/// The processes a host launched, as its launcher recorded them.
#[derive(Default)]
struct Launched(Mutex<Vec<(u32, String, String)>>);

impl Launched {
    fn record(&self, pid: u32, app: &str, workspace: &str) {
        self.0
            .lock()
            .unwrap()
            .push((pid, app.to_owned(), workspace.to_owned()));
    }
}

impl crate::apps::services::LauncherServices for Launched {
    fn applications(&self) -> Vec<crate::apps::services::LaunchableApplication> {
        Vec::new()
    }
    fn launch(
        &self,
        _: &str,
        _: &tilo_desktop_model::Workspace,
        _: Option<&str>,
        _: &crate::apps::services::LaunchPlace,
    ) -> Result<(), String> {
        Err("this host launches nothing".into())
    }
    fn attest_process(&self, pid: u32, app: &str, workspace: &str) -> bool {
        self.0
            .lock()
            .unwrap()
            .iter()
            .any(|(at, a, w)| *at == pid && a == app && w == workspace)
    }
    fn authenticated_launch(
        &self,
        pid: u32,
        app: &str,
        workspace: &str,
        _window_instance: Option<u64>,
    ) -> Result<Option<crate::apps::services::AuthenticatedApplicationLaunch>, String> {
        Ok(self.attest_process(pid, app, workspace).then_some(
            crate::apps::services::AuthenticatedApplicationLaunch {
                continuation_instance: None,
                launch_ticket: None,
                windowless: false,
                opened_for: None,
            },
        ))
    }
}

/// The daemon's own documents, which these tests read only through an app's channel.
struct NoDocuments;

impl DeviceDocuments for NoDocuments {
    fn read(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: Option<&[[u8; 32]]>,
    ) -> Result<DocumentRead, String> {
        Ok(DocumentRead::Absent)
    }
    fn listing(&self, _: &str) -> Result<Vec<(String, String)>, String> {
        Ok(Vec::new())
    }
    fn holds(&self, _: &str, _: &str, _: &str) -> bool {
        false
    }
    fn workspaces(&self) -> Vec<String> {
        Vec::new()
    }
}

impl DeviceWrites for NoDocuments {
    fn write(&self, _: &str, _: &[u8], _: DocumentWriting<'_>) -> DocumentWrite {
        Box::pin(std::future::ready(Err(
            tilo_peer_state::owner::WriteRefusal::HistoryFull {
                app: String::new(),
                record: String::new(),
            },
        )))
    }
    fn served(&self, _: &str, _: &[(&str, &str)]) -> Option<DocumentServing> {
        None
    }
}

impl ShellActions for NoDocuments {
    fn dispatch(&self, _: &ShellCaller, _: Option<String>) -> ShellDispatch {
        Box::pin(std::future::ready(Err("no actions here".to_owned())))
    }
}

/// The resources a host publishes: Quick Settings' changes taken, nothing to read.
struct Published;

impl PublishedResources for Published {
    fn snapshot(
        &self,
        _: &Caller,
        _: &str,
    ) -> tokio::sync::oneshot::Receiver<Result<Reading, String>> {
        let (reply, answered) = tokio::sync::oneshot::channel();
        let _ = reply.send(Ok((0, None)));
        answered
    }
    fn watch(
        &self,
        _: &Caller,
        _: &str,
        _: tokio::sync::mpsc::UnboundedSender<Event>,
    ) -> Result<ResourceWatch, String> {
        Err("nothing is watched here".to_owned())
    }
    fn unwatch(&self, _: ResourceWatch) {}
    fn invoke(
        &self,
        _: &Caller,
        resource: &str,
        action: &str,
        _: Vec<u8>,
    ) -> tokio::sync::oneshot::Receiver<Receipt> {
        let (reply, answered) = tokio::sync::oneshot::channel();
        let _ = reply.send(if (resource, action) == ("sound", "set-volume") {
            Receipt::Accepted
        } else {
            Receipt::Refused {
                code: "unknown_action".to_owned(),
                message: format!("{resource}#{action}"),
            }
        });
        answered
    }
}

/// The app's record as the daemon keeps it.
struct DaemonReads;

struct Kept;

impl DocumentBytes for Kept {
    fn bytes(&self) -> &[u8] {
        KEPT
    }
}

impl NativeDocumentReads for DaemonReads {
    fn resource_read(&self, _: &str, _: &str, _: &str) -> ResourceRead {
        ResourceRead::Sealed
    }
    fn authorize(&self, context: &NativeDocumentReadContext) -> Result<(), String> {
        context.revalidate()
    }
    fn capture(
        &self,
        context: &NativeDocumentReadContext,
        _: Option<[u8; 32]>,
        _: usize,
    ) -> Result<NativeDocumentRead, String> {
        self.authorize(context)?;
        Ok(NativeDocumentRead::Captured {
            revision: [5; 32],
            heads: Vec::new(),
            body: Box::new(Kept),
        })
    }
}

/// The device's daemon, its shell linked to it, and the shell's app socket with the launches the
/// shell recorded.
struct Device {
    _dir: tempfile::TempDir,
    _host: crate::workspaces::fixtures::TestHost,
    _daemon: crate::apps::collab_broker::BrokerHandle,
    daemon_socket: PathBuf,
    workspace: String,
    links: Arc<ShellLinks>,
    shell: runtime_collab::ShellClient,
    seat: seat::AppSocket,
    seat_socket: PathBuf,
    launched: Arc<Launched>,
    /// How many connections reached the device's processes.
    reached_processes: Arc<std::sync::atomic::AtomicUsize>,
}

async fn device_with_shell() -> Device {
    let dir = tempfile::tempdir().unwrap();
    super::tests::secure_socket_parent(dir.path()).unwrap();
    let mut workspace = crate::apps::test_support::Workspace::new(
        crate::apps::test_support::WorkspaceId::from_u128(41),
        "Shelves",
    );
    workspace.policy.add_member(
        person_id_for_root(root(3).verifying_key().to_bytes()),
        crate::apps::test_support::Role::Editor,
    );
    let id = workspace.id().to_string();
    let mut tree = crate::apps::test_support::WorkspaceTree::default();
    tree.workspaces.push(workspace);
    let membership = tree.shared();
    let trusted = || Some(Arc::new(vec![device(3).verifying_key().to_bytes()]));
    let (key, cert) = crate::workspaces::fixtures::governor();
    let host = crate::workspaces::fixtures::test_host(&dir.path().join("host"), &key, &cert).await;

    let daemon_socket = dir.path().join("daemon.sock");
    let links = Arc::new(ShellLinks::default());
    let reached_processes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let daemon = crate::apps::collab_broker::spawn_with_test_services(
        daemon_socket.clone(),
        Some(membership.authority()),
        PeerPublisher::disabled(),
        None,
        ProcessServices {
            committed_documents: Some(Arc::new(DaemonReads)),
            published: Some(Arc::new(Published)),
            shell: Some(Arc::new(ShellServices {
                processes: Some(processes_fake(Arc::clone(&reached_processes))),
                links: Arc::clone(&links),
                socket: daemon_socket.clone(),
                documents: Arc::new(NoDocuments),
                writes: Arc::new(NoDocuments),
                document_writes: Arc::new(|| tokio::sync::broadcast::channel(1).1),
                actions: Arc::new(NoDocuments),
                standing: Arc::new(Published),
                published: None,
                workspaces: host.workspaces().clone(),
                identity: None,
            })),
            ..membership.services()
        },
        trusted(),
        Arc::default(),
        Arc::new(FanoutState::new()),
    )
    .unwrap();

    // The link the daemon makes for the shell it starts.
    let secret = links.make(ShellCaller {
        person: root(3).verifying_key().to_bytes(),
    });
    let mut opening = runtime_collab::wire::shell::SHELL_MAGIC.to_vec();
    opening.extend_from_slice(&secret);
    let shell =
        runtime_collab::ShellClient::open(Endpoint::connect(&daemon_socket, &opening).await.stream)
            .0;
    let seat_socket = dir.path().join("seat.sock");
    let seat = seat::AppSocket::bind(&seat_socket).unwrap();
    Device {
        _dir: dir,
        _host: host,
        _daemon: daemon,
        daemon_socket,
        workspace: id,
        links,
        shell,
        seat,
        seat_socket,
        launched: Arc::new(Launched::default()),
        reached_processes,
    }
}

impl Device {
    /// An app connecting to the shell's app socket with its launcher's `opening`: the app's
    /// end, and the shell's taking of it, attested by its launches and handed to the daemon as
    /// the seat does.
    async fn app(
        &self,
        opening: &[u8],
    ) -> (Endpoint, std::pin::Pin<Box<dyn Future<Output = ()> + '_>>) {
        let listener = self.seat.listener().unwrap();
        let app = Endpoint::connect(&self.seat_socket, opening).await;
        (app, Box::pin(self.take(listener)))
    }

    async fn take(&self, listener: tokio::net::UnixListener) {
        let (stream, _) = listener.accept().await.unwrap();
        let launcher: Arc<dyn crate::apps::services::LauncherServices> =
            Arc::clone(&self.launched) as _;
        match seat::attest(stream, launcher).await {
            seat::Attested::App {
                connection,
                app: vouched,
                process,
                launcher,
            } => self
                .shell
                .vouch(connection, Some((vouched, process, launcher)))
                .await
                .unwrap(),
            seat::Attested::Unnamed(connection) => self.shell.vouch(connection, None).await.unwrap(),
            seat::Attested::Refused => {}
        }
    }
}

/// The device's tilolang processes as the dispatch reaches them: each connection counted in
/// `reached`, and its one ask answered as no process running.
fn processes_fake(
    reached: Arc<std::sync::atomic::AtomicUsize>,
) -> crate::apps::shell::ShellProcesses {
    use runtime_portal::device_processes::{FromDevice, ToDevice};
    use runtime_portal::packet::Packet as _;
    Arc::new(move |connection| {
        reached.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        std::thread::spawn(move || {
            if let Ok(Some(ToDevice::Running)) = ToDevice::receive(&connection) {
                FromDevice::Running(Vec::new()).send(&connection).unwrap();
            }
        });
    })
}

/// An endpoint speaking the wire on its connection.
struct Endpoint {
    stream: tokio::net::UnixStream,
    next: u64,
}

impl Endpoint {
    /// Connected at `socket`, opening with `opening`: what its launcher or its shell wrote first.
    async fn connect(socket: &Path, opening: &[u8]) -> Self {
        use tokio::io::AsyncWriteExt as _;
        let mut stream = tokio::net::UnixStream::connect(socket).await.unwrap();
        stream.write_all(opening).await.unwrap();
        Self { stream, next: 1 }
    }

    async fn call(&mut self, request: Operation) -> Result<Reply, String> {
        use tokio::io::AsyncWriteExt as _;
        let id = self.next;
        self.next = self.next.wrapping_add(1);
        FRAMING
            .write_value_async(&mut self.stream, &Call { id, request })
            .await
            .unwrap();
        self.stream.flush().await.unwrap();
        loop {
            let frame = tilo_test_env::within("the host's answer", async {
                FRAMING
                    .read_value_async::<_, HostFrame>(&mut self.stream)
                    .await
            })
            .await
            .unwrap()
            .expect("the host keeps the connection open");
            if let HostFrame::Reply {
                id: answered,
                result,
            } = frame
                && answered == id
            {
                return result;
            }
        }
    }

    async fn join(&mut self, workspace: &str) -> Result<Reply, String> {
        self.call(Operation::Join(Join {
            channel: CrdtRoute::record(APP, workspace, RECORD).channel_key(),
            identity: super::tests::certified_join(workspace, &root(3), &device(3))
                .encode()
                .unwrap(),
            member: None,
            handle: "Ada".into(),
            resource_type: None,
        }))
        .await
    }
}

/// What tilo-launcher writes first for `app` in `workspace`.
fn launch_opening(app: &str, workspace: &str) -> Vec<u8> {
    let handoff = serde_json::to_vec(&serde_json::json!({
        "protocol": launcher::services::COLLAB_LAUNCH_PROTOCOL_VERSION,
        "app_id": app,
        "workspace_id": workspace,
    }))
    .unwrap();
    let mut opening = launcher::services::COLLAB_LAUNCH_MAGIC.to_vec();
    opening.extend(
        launcher::services::COLLAB_LAUNCH_FRAMING
            .encode(&handoff)
            .unwrap(),
    );
    opening
}

/// An app the shell launched is attested by the shell and handed to the daemon, which answers it
/// as that app from then on: its record as the daemon keeps it, and the device's published
/// resources.
#[tokio::test(flavor = "current_thread")]
async fn an_app_the_shell_launched_is_served_by_the_daemon() {
    let device = device_with_shell().await;
    device
        .launched
        .record(std::process::id(), APP, &device.workspace);
    let (mut app, taken) = device.app(&launch_opening(APP, &device.workspace)).await;
    taken.await;
    let joined = app.join(&device.workspace).await;
    let Ok(Reply::Opened(channel)) = joined else {
        panic!("the app joins its record: {joined:?}");
    };
    let open = runtime_collab::committed_document::Request::Open {
        known_revision: None,
    }
    .encode()
    .unwrap();
    let Ok(Reply::Answer(answer)) = app
        .call(Operation::Command {
            channel,
            service: Service::CommittedDocument,
            body: open,
            on_behalf_of: Vec::new(),
        })
        .await
    else {
        panic!("the record reads");
    };
    match runtime_collab::committed_document::Reply::decode(&answer).unwrap() {
        runtime_collab::committed_document::Reply::Chunk {
            revision, bytes, ..
        } => {
            assert_eq!(revision, [5; 32]);
            assert_eq!(bytes, KEPT);
        }
        other => panic!("the daemon's record, not {other:?}"),
    }
    let Ok(Reply::Opened(services)) = app.call(Operation::Services).await else {
        panic!("the device's published resources open");
    };
    assert_eq!(
        app.call(Operation::Invoke {
            services,
            resource: "sound".into(),
            action: "set-volume".into(),
            arguments: br#"{"level":0.4}"#.to_vec(),
        })
        .await,
        Ok(Reply::Receipt(Receipt::Accepted))
    );
}

/// A process the shell never launched is refused at the shell's socket, saying why, and nothing
/// of it reaches the daemon.
#[tokio::test(flavor = "current_thread")]
async fn a_launch_the_shell_never_recorded_is_refused() {
    let device = device_with_shell().await;
    let (mut app, taken) = device.app(&launch_opening(APP, &device.workspace)).await;
    let ((), joined) = tokio::join!(taken, app.join(&device.workspace));
    let refused = joined.unwrap_err();
    assert!(
        refused.contains("not an authenticated shell-managed process"),
        "{refused}"
    );
}

/// A vouch is for a connection the shell handed over on its own link: one naming a descriptor the
/// link never carried admits nothing.
#[tokio::test(flavor = "current_thread")]
async fn a_vouch_for_a_connection_never_handed_over_is_refused() {
    let device = device_with_shell().await;
    let refused = device
        .shell
        .call(runtime_collab::wire::shell::ShellCall::Vouch(
            runtime_collab::wire::shell::Vouch {
                connection: 40,
                app: Some(runtime_collab::wire::shell::VouchedApp {
                    app: APP.to_owned(),
                    workspace: device.workspace.clone(),
                    window: None,
                    windowless: false,
                    ticket: None,
                    process: 41,
                    launcher: None,
                }),
                to: runtime_collab::wire::shell::HandedTo::Collaboration,
            },
        ))
        .await
        .unwrap_err();
    assert!(
        refused.to_string().contains("handed no connection"),
        "{refused}"
    );
}

/// Only the shell the daemon started reaches the device's processes: a connection it asked for on
/// its own link is handed to them, and one that opens with any other secret, from a process that
/// dialed the daemon's socket itself, reaches nothing.
#[tokio::test(flavor = "current_thread")]
async fn only_the_shell_hands_the_device_processes_an_instance() {
    use runtime_portal::device_processes::{FromDevice, ToDevice};
    use runtime_portal::packet::Packet as _;
    let device = device_with_shell().await;
    let ask = |connection: std::os::fd::OwnedFd| {
        tokio::task::spawn_blocking(move || {
            ToDevice::Running.send(&connection)?;
            FromDevice::receive(&connection)
        })
    };
    // A process that is not the shell, with a secret no link asked for.
    let (ours, theirs) = runtime_portal::packet::pair().unwrap();
    let dialed = std::os::unix::net::UnixStream::connect(&device.daemon_socket).unwrap();
    let mut opening = runtime_collab::wire::shell::PROCESSES_MAGIC.to_vec();
    opening.extend_from_slice(&[9; runtime_collab::wire::shell::SHELL_SECRET_BYTES]);
    runtime_portal::fd_transport::send_with_fd(&dialed, &opening, &theirs).unwrap();
    drop(theirs);
    let refused = ask(ours).await.unwrap();
    assert!(
        matches!(refused, Ok(None) | Err(_)),
        "a process that dialed the daemon itself is not answered: {refused:?}"
    );
    assert_eq!(
        device
            .reached_processes
            .load(std::sync::atomic::Ordering::SeqCst),
        0,
        "nothing of it reached the processes"
    );
    // The shell, on the link the daemon made for it.
    let secret = device.links.make(ShellCaller {
        person: root(3).verifying_key().to_bytes(),
    });
    let mut opening = runtime_collab::wire::shell::SHELL_MAGIC.to_vec();
    opening.extend_from_slice(&secret);
    let (shell, _) = runtime_collab::ShellClient::open(
        Endpoint::connect(&device.daemon_socket, &opening).await.stream,
    );
    let connection = shell.processes().await.unwrap();
    assert_eq!(
        ask(connection).await.unwrap().unwrap(),
        Some(FromDevice::Running(Vec::new())),
        "the shell's connection reaches the processes"
    );
    assert_eq!(
        device
            .reached_processes
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
}

/// A resource the device's services publish is answered on the person's shell link alone: a
/// connection that speaks no shell (an app's own at the daemon) is refused it, whichever app the
/// frame names.
#[tokio::test(flavor = "current_thread")]
async fn an_apps_connection_is_refused_the_persons_shell() {
    use tilo_actions::api::ActionAddress;
    use tilo_actions::remote::{
        InvokeArgument, InvokeRequest, RemoteActionFrame, RemoteActionMessage,
    };
    let frame = RemoteActionFrame::new(RemoteActionMessage::Invoke(InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: tilo_actions::remote::RemoteActionTarget {
            provider: "record:so.tilo.settings:device:server".to_owned(),
            address: ActionAddress::new("device:server", "set-work"),
        },
        argument: InvokeArgument::Json(serde_json::Map::from_iter([(
            "work".to_owned(),
            serde_json::Value::from("anyone"),
        )])),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }))
    .encode()
    .unwrap();
    let refused = super::shell::answer(
        None,
        runtime_collab::wire::shell::ShellCall::Action {
            workspace: None,
            frame,
            caller: None,
        },
    )
    .await
    .unwrap_err();
    assert!(
        refused.contains("not the person's shell"),
        "an app's connection speaks no shell: {refused}"
    );
}
