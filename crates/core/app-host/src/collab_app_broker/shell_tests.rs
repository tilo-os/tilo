#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures: a failed step is the assertion"
)]

//! An app the person's shell launched, on the connection the shell attests and hands the device's
//! daemon over its link: served by the daemon as that app from then on.

use super::seat;
use super::*;
use crate::services::{Caller, PublishedResources, Reading, ResourceWatch};
use crate::shell::{ShellActions, ShellCaller, ShellDispatch, ShellLinks, ShellServices};
use ed25519_dalek::SigningKey;
use runtime_collab::wire::{
    Call, Event, FRAMING, HostFrame, InvocationCall, Operation, Publication, Receipt, Record,
    Reply, Service, Topic,
};
use tilo_actions::remote::asked::{PressPoint, Pressed};
use tilo_peer_state::workspace_documents::{
    DeviceDocuments, DeviceWrites, DocumentRead, DocumentServing, DocumentWrite, DocumentWriting,
};
use tilo_workspace_service::workspaces::Workspaces;

/// Files: the person's app list is its only, and the shell's host keeps that list.
const APP: &str = tilo_app_id::apps::FILE_EXPLORER;
const RECORD: &str = "the-shelf";
/// What the daemon keeps of the app's record.
const KEPT: &[u8] = b"the record as the daemon keeps it";
/// The Browser app's id, and the page whose prompts it raises.
const BROWSER: &str = "so.tilo.browser";
const PAGE: &str = "tl:so.tilo.browser:page-1";
/// The window the Browser runs in.
const WINDOW: u64 = 42;

fn device(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed.wrapping_add(100); 32])
}

fn root(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

/// A launch a host's launcher recorded: its process, the app, its workspace, and the window it
/// runs in, if one.
type Launch = (u32, String, String, Option<u64>);

/// The processes a host launched, as its launcher recorded them, each with the window it runs
/// in, if one.
#[derive(Default)]
struct Launched(Mutex<Vec<Launch>>);

impl Launched {
    fn record(&self, pid: u32, app: &str, workspace: &str) {
        self.record_in(pid, app, workspace, None);
    }

    /// `app` launched as `pid` in `workspace`, into the shell's window `window`.
    fn record_in(&self, pid: u32, app: &str, workspace: &str, window: Option<u64>) {
        self.0
            .lock()
            .unwrap()
            .push((pid, app.to_owned(), workspace.to_owned(), window));
    }

    /// The window the launch of `app` as `pid` in `workspace` runs in.
    fn window_of(&self, pid: u32, app: &str, workspace: &str) -> Option<u64> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .find(|(at, a, w, _)| *at == pid && a == app && w == workspace)
            .and_then(|(.., window)| *window)
    }
}

impl crate::services::LauncherServices for Launched {
    /// The Browser is installed, under the name its manifest gives it.
    fn applications(&self) -> Vec<crate::services::LaunchableApplication> {
        vec![crate::services::LaunchableApplication {
            package_id: "so.tilo.browser".into(),
            display_name: "Browser".into(),
            unconfined: false,
            offers: Vec::new(),
            capabilities: Vec::new(),
            surface: None,
        }]
    }
    fn launch(
        &self,
        _: &str,
        _: &tilo_desktop_model::Workspace,
        _: Option<&str>,
        _: &crate::services::LaunchPlace,
    ) -> Result<(), String> {
        Err("this host launches nothing".into())
    }
    fn attest_process(&self, pid: u32, app: &str, workspace: &str) -> bool {
        self.0
            .lock()
            .unwrap()
            .iter()
            .any(|(at, a, w, _)| *at == pid && a == app && w == workspace)
    }
    fn authenticated_launch(
        &self,
        pid: u32,
        app: &str,
        workspace: &str,
        _window_instance: Option<u64>,
    ) -> Result<Option<crate::services::AuthenticatedApplicationLaunch>, String> {
        Ok(self.attest_process(pid, app, workspace).then_some(
            crate::services::AuthenticatedApplicationLaunch {
                continuation_instance: self.window_of(pid, app, workspace),
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
    fn listing(&self, _: &str) -> Result<Vec<tilo_actions::remote::ListedResource>, String> {
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
    fn dispatch(
        &self,
        _: &ShellCaller,
        _: Option<String>,
        _: Option<runtime_collab::wire::shell::ShellPress>,
    ) -> ShellDispatch {
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
    ) -> crate::collab_app_broker::ReadAnswering<'_, NativeDocumentRead> {
        Box::pin(std::future::ready(
            (|| -> Result<NativeDocumentRead, String> {
                self.authorize(context)?;
                Ok(NativeDocumentRead::Captured {
                    revision: [5; 32],
                    heads: Vec::new(),
                    body: Box::new(Kept),
                })
            })(),
        ))
    }
}

/// The device's daemon, its shell linked to it, and the shell's app socket with the launches the
/// shell recorded.
struct Device {
    _dir: tempfile::TempDir,
    /// The host holding the person's record, kept for the run of the tests that use it.
    record_host: tilo_workspace_service::workspaces::fixtures::TestHost,
    _daemon: crate::collab_broker::BrokerHandle,
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
    device_carrying(Some(echoed_live())).await
}

/// The device's live session as the daemon carries a shell's link into it: each byte the link
/// sends comes back on it, until the shell closes it.
fn echoed_live() -> crate::shell::LiveLinks {
    Arc::new(|stream: tilo_socket::tokio::Stream| {
        Box::pin(async move {
            let (mut read, mut write) = stream.into_split();
            let _ = tokio::io::copy(&mut read, &mut write).await;
        })
    })
}

/// A device whose daemon carries its shell's live links with `live`, or runs no live session.
async fn device_carrying(live: Option<crate::shell::LiveLinks>) -> Device {
    let dir = tempfile::tempdir().unwrap();
    super::tests::secure_socket_parent(dir.path()).unwrap();
    let mut workspace = crate::test_support::Workspace::new(
        crate::test_support::WorkspaceId::from_u128(41),
        "Shelves",
    );
    workspace.policy.add_member(
        person_id_for_root(root(3).verifying_key().to_bytes()),
        crate::test_support::Role::Editor,
    );
    let id = workspace.id().to_string();
    let mut tree = crate::test_support::WorkspaceTree::default();
    tree.workspaces.push(workspace);
    let membership = tree.shared();
    let trusted = || Some(Arc::new(vec![device(3).verifying_key().to_bytes()]));
    let (key, cert) = tilo_workspace_service::workspaces::fixtures::hosting_device();
    let host = tilo_workspace_service::workspaces::fixtures::test_host(
        &dir.path().join("host"),
        &key,
        &cert,
    )
    .await;

    let daemon_socket = dir.path().join("daemon.sock");
    let links = Arc::new(ShellLinks::default());
    let reached_processes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let launched = Arc::new(Launched::default());
    let daemon = crate::collab_broker::spawn_with_test_services(
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
                api: None,
                live,
                speaking: None,
            })),
            // A person's call an app makes is admitted with its evidence, as on a device.
            journal_root: Some(dir.path().join("invocations")),
            // What is installed here, as the device's launcher knows it.
            launcher: Some(Arc::clone(&launched) as _),
            ..membership.services()
        },
        trusted(),
        Arc::default(),
        Arc::new(FanoutState::new()),
    )
    .unwrap();
    // The device's person, whom every app it launched acts as.
    assert!(
        daemon
            .fanout
            .app_broker()
            .unwrap()
            .certificate_authority
            .set(Arc::new(super::tests::DeviceOf(
                super::tests::device_certificate(&root(3), &device(3))
            )))
            .is_ok()
    );

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
        record_host: host,
        _daemon: daemon,
        daemon_socket,
        workspace: id,
        links,
        shell,
        seat,
        seat_socket,
        launched,
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
        let launcher: Arc<dyn crate::services::LauncherServices> = Arc::clone(&self.launched) as _;
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
            seat::Attested::Unnamed(connection) => {
                self.shell.vouch(connection, None).await.unwrap();
            }
            seat::Attested::Refused => {}
        }
    }
}

/// The device's tilolang processes as the dispatch reaches them: each connection counted in
/// `reached`, and its one ask answered as no process running.
fn processes_fake(reached: Arc<std::sync::atomic::AtomicUsize>) -> crate::shell::ShellProcesses {
    use runtime_portal::device_processes::{FromDevice, ToDevice};
    use runtime_portal::packet::Packet as _;
    Arc::new(move |connection| {
        reached.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        std::thread::spawn(move || {
            if matches!(ToDevice::receive(&connection), Ok(Some(ToDevice::Running))) {
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

    /// The next frame the host sends this endpoint.
    async fn frame(&mut self) -> HostFrame {
        tilo_test_env::within("the host's next frame", async {
            FRAMING
                .read_value_async::<_, HostFrame>(&mut self.stream)
                .await
        })
        .await
        .unwrap()
        .expect("the host keeps the connection open")
    }

    /// The app's record in `workspace`, which its first call naming it opens.
    async fn open(&mut self, workspace: &str) -> Result<Record, String> {
        self.open_as(APP, workspace).await
    }

    /// `app`'s record in `workspace`, as the app the endpoint's launch named: its first call
    /// naming the record opens it.
    async fn open_as(&mut self, app: &str, workspace: &str) -> Result<Record, String> {
        let record = Record::new(CrdtRoute::record(app, workspace, RECORD).channel_key(), "");
        self.call(Operation::Command {
            record: record.clone(),
            service: Service::CommittedDocument,
            body: runtime_collab::committed_document::Request::Writes
                .encode()
                .unwrap(),
            on_behalf_of: Vec::new(),
        })
        .await
        .map(|_| record)
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
    let opened = app.open(&device.workspace).await;
    let Ok(record) = opened else {
        panic!("the app opens its record: {opened:?}");
    };
    let open = runtime_collab::committed_document::Request::Open {
        known_revision: None,
    }
    .encode()
    .unwrap();
    let Ok(Reply::Answer(answer)) = app
        .call(Operation::Command {
            record,
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
    let ((), joined) = tokio::join!(taken, app.open(&device.workspace));
    let refused = joined.unwrap_err();
    assert!(
        refused.contains("is not one this computer made"),
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
                    member: None,
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

/// A link of the shell's live session is one end of a pair the shell hands over on its own link:
/// the daemon carries the other into the device's live session, and what the shell sends on its
/// end comes back from there.
#[tokio::test(flavor = "current_thread")]
async fn the_daemon_carries_a_live_link_the_shell_hands_over() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let device = device_with_shell().await;
    let mut link = device.shell.live().await.unwrap();
    link.write_all(b"voice before video").await.unwrap();
    let mut heard = [0_u8; 18];
    link.read_exact(&mut heard).await.unwrap();
    assert_eq!(&heard, b"voice before video");
}

/// A device that runs no live session refuses a live link, saying so.
#[tokio::test(flavor = "current_thread")]
async fn a_live_link_is_refused_where_no_live_session_runs() {
    let device = device_carrying(None).await;
    let refused = device.shell.live().await.unwrap_err();
    assert!(
        refused.to_string().contains("runs no live session"),
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
    let ask = |connection: runtime_portal::packet::Descriptor| {
        tokio::task::spawn_blocking(move || {
            ToDevice::Running.send(&connection)?;
            FromDevice::receive(&connection)
        })
    };
    // A process that is not the shell, with a secret no link asked for.
    let (ours, theirs) = runtime_portal::packet::pair().unwrap();
    let dialed = tokio::net::UnixStream::connect(&device.daemon_socket)
        .await
        .unwrap();
    let sender = runtime_portal::descriptors::DescriptorSender::new();
    let mut dialed = runtime_portal::descriptors::DescriptorStream::sending(dialed, sender.clone());
    let number = sender.send(theirs).unwrap();
    let mut opening = runtime_collab::wire::shell::PROCESSES_MAGIC.to_vec();
    opening.extend_from_slice(&[9; runtime_collab::wire::shell::SHELL_SECRET_BYTES]);
    opening.extend_from_slice(&number.to_le_bytes());
    tokio::io::AsyncWriteExt::write_all(&mut dialed, &opening)
        .await
        .unwrap();
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
        Endpoint::connect(&device.daemon_socket, &opening)
            .await
            .stream,
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
            frame: frame.clone(),
            caller: None,
            pressed: None,
        },
    )
    .await
    .unwrap_err();
    assert!(
        refused.contains("not the person's shell"),
        "an app's connection speaks no shell: {refused}"
    );
    // A connection that speaks no shell cannot claim a window of the person's either: the press
    // is the shell's word alone, and one from anywhere else is refused whole.
    let refused = super::shell::answer(
        None,
        runtime_collab::wire::shell::ShellCall::Action {
            workspace: None,
            frame,
            caller: None,
            pressed: Some(runtime_collab::wire::shell::ShellPress {
                instance: "another-app's-window".to_owned(),
                resource: "device:server".to_owned(),
                at: None,
            }),
        },
    )
    .await
    .unwrap_err();
    assert!(
        refused.contains("not the person's shell"),
        "an app's connection claims no window: {refused}"
    );
}

/// The Browser's page action a site's request for the camera, microphone, location or
/// notifications raises, as its declaration publishes it: destructive, asking first, and showing
/// the site and what it wants.
fn allow_permission(page: &str) -> tilo_actions::published::PublishedAction {
    use tilo_actions::remote::asked::{Shown, ShownAs};
    tilo_actions::published::PublishedAction {
        did: None,
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        resource: page.into(),
        action: "allow_permission".into(),
        resource_kind: "Page".into(),
        resource_label: "Page".into(),
        action_label: "Allow the site".into(),
        keywords: Vec::new(),
        icon: Some("shield".into()),
        description: None,
        operation_kind: tilo_actions::OperationKind::Effect,
        params: Vec::new(),
        contract: tilo_actions::ActionContract {
            kind: tilo_actions::ActionKind::Outside,
            risk: tilo_actions::RiskLevel::Destructive,
            confirmation: tilo_actions::ConfirmationPolicy::Confirm,
            shows: ["origin", "wants"]
                .map(|param| Shown {
                    param: param.into(),
                    field: None,
                    render: ShownAs::Text,
                })
                .into(),
            ..Default::default()
        },
        input: tilo_actions::published::PublishedInput::None,
        view: None,
    }
}

/// What the daemon answers `message`, sent on the Browser's connection about `record`.
async fn remote_action(
    browser: &mut Endpoint,
    record: &Record,
    message: tilo_actions::remote::RemoteActionMessage,
) -> tilo_actions::remote::RemoteActionMessage {
    pressed_action(browser, record, message, (None, None)).await
}

/// What the daemon answers `message`, sent on the Browser's connection about `record`, naming
/// the input serial `press` of the person's press that made it.
async fn pressed_action(
    browser: &mut Endpoint,
    record: &Record,
    message: tilo_actions::remote::RemoteActionMessage,
    (press, press_at): (Option<u32>, Option<tilo_actions::remote::asked::PressPoint>),
) -> tilo_actions::remote::RemoteActionMessage {
    use tilo_actions::remote::RemoteActionFrame;
    let mut frame = RemoteActionFrame::new(message);
    frame.press = press;
    frame.press_at = press_at;
    let answered = browser
        .call(Operation::Command {
            record: record.clone(),
            service: Service::RemoteAction,
            body: frame.encode().unwrap(),
            on_behalf_of: Vec::new(),
        })
        .await;
    let Ok(Reply::Answer(frame)) = answered else {
        panic!("the daemon answers the Browser: {answered:?}");
    };
    RemoteActionFrame::decode(&frame).unwrap().message
}

/// The Browser's own call of `allow_permission` on `page` in `workspace`, for the page's prompt
/// `id`: `origin` asks for `wants`.
fn asking_for(
    workspace: &str,
    page: &str,
    (id, origin, wants): (u32, &str, &str),
) -> tilo_actions::remote::InvokeRequest {
    tilo_actions::remote::InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: tilo_actions::remote::application_action_target(
            workspace,
            "so.tilo.browser",
            page,
            "allow_permission",
        ),
        argument: tilo_actions::remote::InvokeArgument::Json(serde_json::Map::from_iter([
            ("id".to_owned(), serde_json::Value::from(id)),
            ("origin".to_owned(), serde_json::Value::from(origin)),
            ("wants".to_owned(), serde_json::Value::from(wants)),
        ])),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// The person's `resolution` of the asked record `at`, as `tilo.invocations#resolve` reads it.
fn resolving(
    at: &tilo_actions::remote::asked::RecordRef,
    resolution: &str,
) -> tilo_actions::remote::InvokeRequest {
    tilo_actions::remote::InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: tilo_actions::remote::RemoteActionTarget {
            provider: tilo_actions::remote::workspace_invocations::PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new(
                at.record.clone(),
                tilo_actions::remote::asked::RESOLVE,
            ),
        },
        argument: tilo_actions::remote::InvokeArgument::Json(serde_json::Map::from_iter([(
            "resolution".to_owned(),
            serde_json::Value::from(resolution),
        )])),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// The Browser's follow of its call `invocation`, waiting up to the longest a follow waits.
fn following(invocation: tilo_actions::InvocationId) -> tilo_actions::remote::RemoteActionMessage {
    tilo_actions::remote::RemoteActionMessage::Observe {
        correlation: 0,
        invocation,
        wait_ms: Some(10_000),
        seen: None,
    }
}

/// How the call an `Update` answers stands, as its caller reads it.
fn stands(
    update: &tilo_actions::remote::RemoteActionMessage,
) -> tilo_actions::remote::ReceiptStatus {
    let tilo_actions::remote::RemoteActionMessage::Update {
        state: Some(state), ..
    } = update
    else {
        panic!("the call's state answers its follow: {update:?}");
    };
    state.receipt().status
}

/// The device's own identity, which signs what it admits for its person, set up in a home of its
/// own, kept for the test that made it. An account's identity opens only in the process whose
/// device service took the key, so the test takes it before it opens the identity.
fn device_identity() -> crate::test_support::ProvisionedHome {
    crate::test_support::ProvisionedHome::new()
}

/// What a test's step answers, driven on the test's one thread.
type Driven<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + 'a>>;

/// A page's prompt the Browser raises as its own call of a page action that asks first (a site
/// asking for the camera) waits for the person in its asked record at the device's daemon, naming
/// the Browser by its name and the site: nobody's screen answers it in the Browser's place. The
/// Browser follows the call; the person's decline of its record ends it cancelled, which is the
/// page's no, and so does the Browser's own withdraw when the page stops asking. Their approve,
/// through `tilo.invocations#resolve`, hands the Browser the very call, carrying their approve,
/// and its action answers the page.
#[tokio::test(flavor = "current_thread")]
async fn a_page_prompt_the_browser_raises_waits_for_its_person_in_an_asked_record() {
    // The device key, taken as its daemon takes it, is held for as long as the test runs.
    let (mut asking, _home) = page_asking().await;
    declining_a_sites_prompt(&mut asking).await;
    withdrawing_a_stopped_prompts_call(&mut asking).await;
    approving_a_sites_prompt(&mut asking).await;
}

/// The device's identity, its key taken as its daemon takes it and the person's devices listed;
/// the Browser connected with its record opened and its page's prompt published; the daemon's
/// asked records; and the person using the Browser: what a page's prompt the Browser raises is
/// driven through.
struct PageAsking {
    host: Device,
    workspaces: Workspaces,
    browser: Endpoint,
    channel: Record,
    store: Arc<tilo_actions::remote::asked::AskedStore>,
    person: tilo_actions::remote::Caller,
}

/// Set the device's identity up, connect the Browser and publish its page's prompt, and read the
/// person using it. The device key, taken as its daemon takes it, answers beside it, for the test
/// to keep while it runs.
fn page_asking() -> Driven<'static, (PageAsking, crate::test_support::ProvisionedHome)> {
    Box::pin(async move {
        // The device's own identity, which signs what it admits for its person.
        let home = device_identity();
        // The person's devices, as the host holding their record answers: this one is listed.
        let host = device_with_shell().await;
        let workspaces = host.record_host.workspaces().clone();
        host.launched
            .record(std::process::id(), BROWSER, &host.workspace);
        let (mut browser, taken) = host.app(&launch_opening(BROWSER, &host.workspace)).await;
        taken.await;
        let channel = browser
            .open_as(BROWSER, &host.workspace)
            .await
            .expect("the Browser opens its record");
        assert_eq!(
            browser
                .call(Operation::Subscribe {
                    record: channel.clone(),
                    topic: Topic::Invocations { stable_ids: true },
                })
                .await,
            Ok(Reply::Subscribed)
        );
        let (surface, _) = runtime_collab::SurfacePublication::next(
            None,
            std::slice::from_ref(&allow_permission(PAGE)),
        );
        assert_eq!(
            browser
                .call(Operation::Publish {
                    record: channel.clone(),
                    publication: Publication::Surface(serde_json::to_string(&surface).unwrap()),
                })
                .await,
            Ok(Reply::Done)
        );
        let store = workspaces
            .asked_store()
            .expect("the daemon keeps asked records");
        let person = tilo_actions::remote::Caller::authenticated(
            root(3).verifying_key().to_bytes(),
            device(3).verifying_key().to_bytes(),
        );
        (
            PageAsking {
                host,
                workspaces,
                browser,
                channel,
                store,
                person,
            },
            home,
        )
    })
}

/// Raise the Browser's call for the page's prompt `prompt` (`id`, `origin`, `wants`): answered as
/// going on, kept as its one asked record.
fn raise_prompt<'a>(
    asking: &'a mut PageAsking,
    prompt: (u32, &'a str, &'a str),
) -> Driven<
    'a,
    (
        tilo_actions::InvocationId,
        tilo_actions::remote::asked::Waiting,
    ),
> {
    Box::pin(async move {
        use tilo_actions::remote::{ReceiptStatus, RemoteActionMessage};
        let request = asking_for(&asking.host.workspace, PAGE, prompt);
        let invocation = request.invocation;
        assert_eq!(
            remote_action(
                &mut asking.browser,
                &asking.channel,
                RemoteActionMessage::Invoke(request)
            )
            .await,
            RemoteActionMessage::Receipt(tilo_actions::remote::InvocationReceipt {
                invocation,
                status: ReceiptStatus::Accepted,
            }),
            "the call goes on, waiting for its person, rather than refused for want of their \
         yes"
        );
        let waiting = asking.store.waiting_in(&asking.host.workspace);
        let [asked] = waiting.as_slice() else {
            panic!("the Browser's call is kept as its one asked record: {waiting:?}");
        };
        assert_eq!(asked.invocation, invocation);
        (invocation, asked.clone())
    })
}

/// A site asks for the camera; the person turns it down while the Browser follows its call.
fn declining_a_sites_prompt(asking: &mut PageAsking) -> Driven<'_, ()> {
    Box::pin(async move {
        use tilo_actions::remote::ReceiptStatus;
        use tilo_actions::remote::asked::AskedBy;
        let (invocation, asked) = raise_prompt(asking, (7, "https://meet.example", "camera")).await;
        assert_eq!(
            asked.person,
            hex::encode(root(3).verifying_key().to_bytes()),
            "the person using the Browser is asked"
        );
        assert_eq!(
            asked.asked.facts.by,
            AskedBy::App {
                app: BROWSER.to_owned(),
                title: "Browser".to_owned(),
                author: None,
            },
            "the Browser asks, by the name its manifest gives it"
        );
        assert_eq!(
            asked.asked.facts.fact("origin"),
            Some("https://meet.example"),
            "the card reads the site from the call: {:?}",
            asked.asked.facts
        );
        assert_eq!(asked.asked.facts.fact("wants"), Some("camera"));
        let (followed, declined) = tokio::join!(
            remote_action(&mut asking.browser, &asking.channel, following(invocation)),
            asking
                .workspaces
                .resolve_asked(&asking.person, &resolving(&asked.asked.record, "decline")),
        );
        assert!(
            matches!(
                declined.expect("the resolve answers"),
                Ok(tilo_actions::remote::InvocationReceipt {
                    status: ReceiptStatus::Completed { .. },
                    ..
                })
            ),
            "the decline is taken"
        );
        assert_eq!(
            stands(&followed),
            ReceiptStatus::Cancelled {
                message: Some(tilo_actions::remote::asked::DECLINED.to_owned()),
            },
            "the Browser's follow hears the call ended unrun: the page's no"
        );
        assert!(asking.store.waiting_in(&asking.host.workspace).is_empty());
    })
}

/// A site asks for the microphone, and stops asking before anyone answers: the Browser withdraws
/// its call, which ends it unrun too.
fn withdrawing_a_stopped_prompts_call(asking: &mut PageAsking) -> Driven<'_, ()> {
    Box::pin(async move {
        use tilo_actions::remote::{ReceiptStatus, RemoteActionMessage};
        let (invocation, _) = raise_prompt(asking, (8, "https://meet.example", "microphone")).await;
        assert_eq!(
            remote_action(
                &mut asking.browser,
                &asking.channel,
                RemoteActionMessage::Cancel { invocation }
            )
            .await,
            RemoteActionMessage::CancelResponse {
                invocation,
                result: Ok(()),
            }
        );
        assert!(
            asking.store.waiting_in(&asking.host.workspace).is_empty(),
            "nothing is asked of the person any more"
        );
        assert!(
            matches!(
                stands(
                    &remote_action(&mut asking.browser, &asking.channel, following(invocation))
                        .await
                ),
                ReceiptStatus::Cancelled { .. }
            ),
            "the withdrawn call ended unrun"
        );
    })
}

/// The site asks for the camera again, and this time the person allows it: the Browser is handed
/// its own call, carrying the approve, and its action answers the page.
fn approving_a_sites_prompt(asking: &mut PageAsking) -> Driven<'_, ()> {
    Box::pin(async move {
        use tilo_actions::remote::ReceiptStatus;
        let (invocation, asked) = raise_prompt(asking, (9, "https://meet.example", "camera")).await;
        let at = asked.asked.record.clone();
        let resolve = resolving(&at, "approve");
        let resolved = asking
            .workspaces
            .resolve_asked(&asking.person, &resolve)
            .await
            .expect("the resolve answers");
        assert!(
            matches!(
                resolved,
                Ok(tilo_actions::remote::InvocationReceipt {
                    status: ReceiptStatus::Completed { .. },
                    ..
                })
            ),
            "the approve is taken: {resolved:?}"
        );
        assert!(
            asking.store.waiting_in(&asking.host.workspace).is_empty(),
            "nothing waits for the person any more"
        );

        // The Browser is handed its own call, carrying the approve: its action answers the page.
        let handed = loop {
            if let HostFrame::Event {
                event: Event::Invocation(handed),
                ..
            } = asking.browser.frame().await
            {
                break handed;
            }
        };
        assert_eq!(handed.invocation, *invocation.0.as_bytes());
        assert_eq!(
            (handed.resource.as_str(), handed.action.as_str()),
            (PAGE, "allow_permission")
        );
        let context: tilo_actions::ActionContext =
            serde_json::from_str(handed.context.as_deref().expect("the call's context")).unwrap();
        assert_eq!(
            context.approval.map(|approval| approval.record),
            Some(at),
            "the call carries the person's approve of its record"
        );
        assert_eq!(
            asking
                .browser
                .call(Operation::Invocation {
                    record: asking.channel.clone(),
                    id: handed.id,
                    call: InvocationCall::Complete {
                        code: String::new(),
                        message: String::new(),
                    },
                })
                .await,
            Ok(Reply::Done),
            "the Browser answers the page and ends its call"
        );
    })
}

/// The device, its shell linked to it, the Browser connected with window `window` and its surface
/// published, and the presses the compositor attested there: what a pressed prompt is raised
/// through.
struct PressedAsking {
    _home: crate::test_support::ProvisionedHome,
    host: Device,
    workspaces: Workspaces,
    browser: Endpoint,
    channel: Record,
    store: Arc<tilo_actions::remote::asked::AskedStore>,
    window: u64,
    point: PressPoint,
}

/// Set the device's identity up, connect the Browser with window `window`, and attest the
/// presses its window and another saw.
fn pressed_asking(window: u64) -> Driven<'static, PressedAsking> {
    Box::pin(async move {
        use runtime_collab::wire::shell::{ShellCall, ShellReply};
        let home = device_identity();
        let host = device_with_shell().await;
        let workspaces = host.record_host.workspaces().clone();
        host.launched
            .record_in(std::process::id(), BROWSER, &host.workspace, Some(window));
        let (mut browser, taken) = host.app(&launch_opening(BROWSER, &host.workspace)).await;
        taken.await;
        let channel = browser
            .open_as(BROWSER, &host.workspace)
            .await
            .expect("the Browser opens its record");
        let (surface, _) = runtime_collab::SurfacePublication::next(
            None,
            std::slice::from_ref(&allow_permission(PAGE)),
        );
        assert_eq!(
            browser
                .call(Operation::Publish {
                    record: channel.clone(),
                    publication: Publication::Surface(serde_json::to_string(&surface).unwrap()),
                })
                .await,
            Ok(Reply::Done)
        );
        let store = workspaces
            .asked_store()
            .expect("the daemon keeps asked records");
        // The compositor saw the person press the Browser's window at (140, 62), press a key in
        // it, and press in another window.
        let point = PressPoint { x: 140, y: 62 };
        for (pressed_window, serial, at) in [
            (window, 5, Some(point)),
            (window, 7, None),
            (window.saturating_add(1), 6, None),
        ] {
            assert_eq!(
                host.shell
                    .call(ShellCall::Press {
                        window: pressed_window,
                        serial,
                        at,
                    })
                    .await
                    .expect("the shell link takes the press"),
                ShellReply::Pressed
            );
        }
        PressedAsking {
            _home: home,
            host,
            workspaces,
            browser,
            channel,
            store,
            window,
            point,
        }
    })
}

/// Raise the Browser's call for the page's prompt `prompt` naming the press `press`, its window
/// saying the control the press answered stands at: the press its asked record keeps, which is
/// withdrawn once read.
fn pressed_prompt<'a>(
    browser: &'a mut Endpoint,
    channel: &'a Record,
    host: &'a Device,
    store: &'a tilo_actions::remote::asked::AskedStore,
    prompt: (u32, &'a str, &'a str),
    press: (Option<u32>, Option<PressPoint>),
) -> Driven<'a, Option<Pressed>> {
    Box::pin(async move {
        use tilo_actions::remote::{InvocationReceipt, ReceiptStatus, RemoteActionMessage};
        let request = asking_for(&host.workspace, PAGE, prompt);
        let invocation = request.invocation;
        assert_eq!(
            pressed_action(
                browser,
                channel,
                RemoteActionMessage::Invoke(request),
                press
            )
            .await,
            RemoteActionMessage::Receipt(InvocationReceipt {
                invocation,
                status: ReceiptStatus::Accepted,
            }),
            "the call waits for its person"
        );
        let waiting = store.waiting_in(&host.workspace);
        let asked = waiting
            .iter()
            .find(|waiting| waiting.invocation == invocation)
            .expect("the call is kept as its asked record");
        // Withdrawn once read: the person is asked only one question at a time here.
        store.withdraw(&asked.asked.record, "done looking");
        asked.asked.pressed.clone()
    })
}

/// Each prompt the Browser raises stands where the compositor says its press was, never where
/// the app says the control is; a call no press made, or one naming another window's press,
/// stands nowhere.
fn assert_pressed_prompts(asking: &mut PressedAsking) -> Driven<'_, ()> {
    Box::pin(async move {
        let in_window = |at| {
            Some(Pressed {
                device: hex::encode(asking.workspaces.device_cert().device_public_key),
                app: BROWSER.to_owned(),
                instance: asking.window.to_string(),
                resource: PAGE.to_owned(),
                at,
            })
        };
        // Where the Browser says the control a press answered stands, which the compositor never saw.
        let control = PressPoint { x: 610, y: 400 };
        assert_eq!(
            pressed_prompt(
                &mut asking.browser,
                &asking.channel,
                &asking.host,
                &asking.store,
                (7, "https://meet.example", "camera"),
                (Some(5), None),
            )
            .await,
            in_window(Some(asking.point)),
            "the question stands at the press in the Browser's window"
        );
        assert_eq!(
            pressed_prompt(
                &mut asking.browser,
                &asking.channel,
                &asking.host,
                &asking.store,
                (10, "https://meet.example", "camera"),
                (Some(5), Some(control)),
            )
            .await,
            in_window(Some(asking.point)),
            "where a pointer pressed is the compositor's word, never the app's"
        );
        assert_eq!(
            pressed_prompt(
                &mut asking.browser,
                &asking.channel,
                &asking.host,
                &asking.store,
                (11, "https://meet.example", "microphone"),
                (Some(7), Some(control)),
            )
            .await,
            in_window(Some(control)),
            "a key press stands at the control its window says the key answered"
        );
        assert_eq!(
            pressed_prompt(
                &mut asking.browser,
                &asking.channel,
                &asking.host,
                &asking.store,
                (8, "https://meet.example", "microphone"),
                (None, Some(control)),
            )
            .await,
            None,
            "a call no press made stands in no window"
        );
        assert_eq!(
            pressed_prompt(
                &mut asking.browser,
                &asking.channel,
                &asking.host,
                &asking.store,
                (9, "https://meet.example", "camera"),
                (Some(6), Some(control)),
            )
            .await,
            None,
            "a press in another window is not this one's"
        );
    })
}

/// A call the person's press in an app's window made, and that asks first, is kept with where
/// they pressed it: this device, the app, the window's tab key as the session publishes it and
/// the point in the window the compositor attested over the shell link. The shell stands the
/// question there. A call no press made, or one naming a press in another window, asks with no
/// window.
#[tokio::test(flavor = "current_thread")]
async fn a_press_that_asks_first_is_kept_where_it_was_pressed() {
    let mut asking = pressed_asking(WINDOW).await;
    assert_pressed_prompts(&mut asking).await;
}
