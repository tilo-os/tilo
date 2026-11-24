//! The device's daemon as a test's session reaches it: a real workspace service holding the
//! device's workspaces, answering the person's shell on the link it hands the session, through
//! the same dispatch, reads and writes the daemon answers a desktop with. The session reads and
//! writes its workspaces' documents there, and the daemon signs and seals each write as the device.

// A fixture only tests compile.
#![cfg(all(test, target_os = "linux"))]
#![allow(
    clippy::expect_used,
    reason = "a daemon that cannot start is its test's failure"
)]

use std::os::unix::fs::PermissionsExt as _;
use std::sync::Arc;

use tilo_desktop_model::WorkspaceId;
use tilo_peer_state::workspace_documents::{DeviceDocuments, DeviceWrites};
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::apps::services::{
    Caller, ProcessServices, PublishedResources, Reading, ResourceWatch,
};
use tilo_workspace_service::apps::shell::{
    ShellActions, ShellCaller, ShellDispatch, ShellLinks, ShellServices,
};
use tilo_workspace_service::device_documents::{ServiceDocuments, ServiceWrites};
use tilo_workspace_service::workspaces::Workspaces;
use tilo_workspace_service::workspaces::fixtures::{TestHost, governor, test_host};

use crate::layout::Layouts;
use crate::workspace_view::{ShellAsks, WorkspaceView};

/// The device's daemon, holding its workspaces on a real workspace service, and the shell link it
/// answers the session on.
pub(crate) struct TestDaemon {
    host: TestHost,
    _dispatch: tilo_workspace_service::apps::collab_broker::BrokerHandle,
    key: ed25519_dalek::SigningKey,
    cert: DeviceCert,
    reads: Arc<dyn DeviceDocuments>,
    writes: Arc<dyn DeviceWrites>,
    feed: tilo_session_client::daemon_documents::DaemonWritesFeed,
    shell: tilo_session_client::daemon_documents::DaemonShell,
    view: Arc<WorkspaceView>,
    /// Where the daemon answers the person's shell.
    socket: std::path::PathBuf,
    /// The links the daemon made for the shells it starts.
    links: Arc<ShellLinks>,
    _dir: tempfile::TempDir,
}

impl TestDaemon {
    /// Start the daemon, and open the session's link to it.
    pub(crate) fn start() -> Self {
        let runtime = tilo_rt::app();
        let dir = tempfile::tempdir().expect("a state directory");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("the daemon's directory is its own");
        let socket = dir.path().join("collab.sock");
        let (key, cert) = governor();
        let host = runtime
            .block_on(test_host(&dir.path().join("host"), &key, &cert))
            .expect("the runtime runs");
        let workspaces = host.workspaces().clone();
        // The daemon's service acts as its device, as a device's daemon does: the record commands
        // it carries out for the session are the device's writes.
        workspaces.set_member_device(Arc::new(tilo_workspace_service::workspaces::MemberDevice {
            cert: cert.clone(),
            signer: Arc::new(key.clone()),
        }));
        // The daemon runs the typed actions of the workspace resources the session changes, as
        // the device's runtime does.
        runtime
            .block_on(async { tilo_workspace_service::resource_actions::register(&workspaces) })
            .expect("the runtime runs");
        let links = Arc::new(ShellLinks::default());
        let actions = Arc::new(Governance {
            workspaces: workspaces.clone(),
        });
        let services = ProcessServices {
            shell: Some(Arc::new(ShellServices {
                processes: None,
                links: Arc::clone(&links),
                socket: socket.clone(),
                documents: Arc::new(ServiceDocuments::of(workspaces.clone())),
                writes: Arc::new(ServiceWrites::of(workspaces.clone())),
                document_writes: Arc::new({
                    let workspaces = workspaces.clone();
                    move || workspaces.document_writes()
                }),
                actions,
                standing: Arc::new(NoStanding),
                published: None,
                workspaces: workspaces.clone(),
                identity: None,
                live: None,
            })),
            // The providers registered on the device, the seat a shell serves among them.
            providers: Some(workspaces.providers()),
            ..ProcessServices::default()
        };
        let dispatch = runtime
            .block_on(async {
                tilo_workspace_service::apps::collab_broker::spawn_with_services(
                    socket.clone(),
                    None,
                    tilo_workspace_service::apps::collab_outbound::PeerPublisher::disabled(),
                    None,
                    services,
                    Arc::default(),
                    Arc::new(tilo_workspace_service::apps::collab_broker::FanoutState::new()),
                )
            })
            .expect("the runtime runs")
            .expect("the daemon's dispatch starts");
        let (reads, writes, shell) = tilo_session_client::daemon_documents::on_daemon(
            Ok(link(&socket, &links, &cert)),
            runtime.handle().clone(),
        );
        let view = Arc::new(WorkspaceView::new(
            cert.user_public_key,
            cert.device_public_key,
            Arc::new(Unasked),
        ));
        Self {
            host,
            _dispatch: dispatch,
            key,
            cert,
            reads,
            writes,
            feed: tilo_session_client::daemon_documents::DaemonWritesFeed::start(&shell),
            shell,
            view,
            socket,
            links,
            _dir: dir,
        }
    }

    /// Found `workspace` as `name` here, as the person's shell asks the daemon to, and have the
    /// session's view read how it stands, as the daemon publishes it.
    pub(crate) fn found(&self, workspace: WorkspaceId, name: &str) {
        let id = workspace.to_string();
        let workspaces = self.host.workspaces().clone();
        let (_, peer) = tilo_rt::app()
            .block_on(async move {
                workspaces
                    .create_workspace(name, Some(&id))
                    .await
                    .expect("the workspace is founded");
                workspaces.governed_copy(workspace).await
            })
            .expect("the runtime runs")
            .expect("the daemon holds the workspace it founded");
        let reader = tilo_workspace_service::workspaces::standing::Reader {
            person: self.cert.user_public_key,
            device: self.cert.device_public_key,
            signer: &self.key,
        };
        let reading = tilo_workspace_service::workspaces::standing::reading(
            workspace,
            &peer,
            &reader,
            Some(self.cert.device_public_key),
            Vec::new(),
        );
        let id = workspace.to_string();
        self.view
            .apply(
                &tilo_peer_state::workspace_view::resource(&id),
                Some(&serde_json::to_string(&reading).expect("a reading encodes")),
            )
            .expect("the view reads the workspace");
    }

    /// The workspaces the daemon holds, as its own roles reach them.
    pub(crate) fn workspaces(&self) -> &Workspaces {
        self.host.workspaces()
    }

    /// The device's certificate: whose key the daemon signs its writes with.
    pub(crate) fn certificate(&self) -> DeviceCert {
        self.cert.clone()
    }

    /// The session's reads of the documents of the workspaces the daemon holds, on its link.
    pub(crate) fn reads(&self) -> Arc<dyn DeviceDocuments> {
        Arc::clone(&self.reads)
    }

    /// The session's writes of those documents, on its link: the daemon signs and seals them.
    pub(crate) fn writes(&self) -> Arc<dyn DeviceWrites> {
        Arc::clone(&self.writes)
    }

    /// A shell link of the daemon's person, opened: what a session serves its seat on.
    pub(crate) fn shell_client(&self) -> runtime_collab::ShellClient {
        link(&self.socket, &self.links, &self.cert)
    }

    /// The seat as the daemon's broker reaches it, while a shell serves it.
    pub(crate) fn seat(&self) -> tilo_workspace_service::apps::seat::Seat {
        tilo_workspace_service::apps::seat::Seat::new(
            self.workspaces().providers(),
            tilo_rt::app().handle().clone(),
        )
    }

    /// The session's link to the daemon: where its reads are followed and its actions run.
    pub(crate) fn shell(&self) -> tilo_session_client::daemon_documents::DaemonShell {
        self.shell.clone()
    }

    /// How the person's workspaces stand, as the session's view reads them: each workspace
    /// founded here, held by the daemon.
    pub(crate) fn view(&self) -> Arc<WorkspaceView> {
        Arc::clone(&self.view)
    }

    /// Have `layouts` read and write the workspaces through the daemon, through `reads` and
    /// `writes`, and read a record again whenever the daemon tells of a write to it.
    pub(crate) fn attach_as(
        &self,
        layouts: &Layouts,
        reads: &Arc<dyn DeviceDocuments>,
        writes: &Arc<dyn DeviceWrites>,
    ) {
        layouts.follow_writes(self.feed.clone());
        layouts.attach(&self.cert, &self.view(), reads, writes);
    }

    /// Have `layouts` read and write the workspaces through the daemon.
    pub(crate) fn attach(&self, layouts: &Layouts) {
        self.attach_as(layouts, &self.reads(), &self.writes());
    }

    /// The shell link the daemon made for the session, opened and handed over, as
    /// [`crate::Options::shell_link`] takes it for a session in this process: the daemon's
    /// workspace resources, people and settings are then the session's own.
    pub(crate) fn shell_link(&self) -> std::os::fd::OwnedFd {
        use std::io::Write as _;
        let mut link = std::os::unix::net::UnixStream::connect(&self.socket)
            .expect("the daemon answers on its socket");
        let secret = self.links.make(ShellCaller {
            person: self.cert.user_public_key,
        });
        let mut opening = runtime_collab::wire::shell::SHELL_MAGIC.to_vec();
        opening.extend_from_slice(&secret);
        link.write_all(&opening).expect("the link opens");
        std::os::fd::OwnedFd::from(link)
    }
}

/// A shell link the daemon on `socket` made for its person and handed the session, opened.
fn link(
    socket: &std::path::Path,
    links: &ShellLinks,
    cert: &DeviceCert,
) -> runtime_collab::ShellClient {
    use tokio::io::AsyncWriteExt as _;
    let secret = links.make(ShellCaller {
        person: cert.user_public_key,
    });
    tilo_rt::app()
        .block_on(async {
            let mut link = tokio::net::UnixStream::connect(socket)
                .await
                .expect("the daemon answers on its socket");
            let mut opening = runtime_collab::wire::shell::SHELL_MAGIC.to_vec();
            opening.extend_from_slice(&secret);
            link.write_all(&opening).await.expect("the link opens");
            runtime_collab::ShellClient::open(link).0
        })
        .expect("the runtime runs")
}

/// The daemon's one dispatch, as its person's shell reaches it: the device action service, which
/// carries out the workspaces' governance (their agents included) as this device, exactly as the
/// daemon does in a running session.
struct Governance {
    workspaces: Workspaces,
}

impl ShellActions for Governance {
    fn dispatch(
        &self,
        caller: &ShellCaller,
        workspace: Option<String>,
        _pressed: Option<runtime_collab::wire::shell::ShellPress>,
    ) -> ShellDispatch {
        let workspaces = self.workspaces.clone();
        let shell = *caller;
        Box::pin(std::future::ready({
            let cert = workspaces.device_cert();
            if shell.person != cert.user_public_key {
                Err("this device acts for its own person only".to_owned())
            } else {
                let person = tilo_actions::remote::Caller::authenticated(
                    cert.user_public_key,
                    cert.device_public_key,
                );
                let caller = match workspace {
                    Some(workspace) => person.in_workspace(workspace),
                    None => person,
                };
                let provider: Arc<dyn tilo_actions::remote::ActionProvider + Send + Sync> =
                    Arc::new(GovernanceCall { workspaces });
                Ok((provider, caller))
            }
        }))
    }
}

/// What the daemon answers a shell's governance calls: the device action service, asked as the
/// device's person in the workspace the call names.
struct GovernanceCall {
    workspaces: Workspaces,
}

impl tilo_actions::remote::ActionProvider for GovernanceCall {
    fn query(
        &self,
        _caller: &tilo_actions::remote::Caller,
        _query: tilo_actions::remote::CatalogueQuery,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::CataloguePage, tilo_actions::remote::ActionCallError>,
    > {
        tilo_actions::remote::Pending::ready(Err(
            tilo_actions::remote::ActionCallError::UnknownAction,
        ))
    }

    fn invoke(
        &self,
        _caller: &tilo_actions::remote::Caller,
        request: tilo_actions::remote::InvokeRequest,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionCallError>,
    > {
        let workspace =
            tilo_actions::remote::workspace_governance_operation(&request.target.address)
                .map(|(workspace, _)| workspace.to_owned());
        let Some(workspace) = workspace else {
            return tilo_actions::remote::Pending::ready(Err(
                tilo_actions::remote::ActionCallError::UnknownAction,
            ));
        };
        let cert = self.workspaces.device_cert();
        let caller = tilo_actions::remote::Caller::authenticated(
            cert.user_public_key,
            cert.device_public_key,
        )
        .in_workspace(workspace);
        self.workspaces.device_actions().invoke(&caller, request)
    }

    fn observe(
        &self,
        _caller: &tilo_actions::remote::Caller,
        _id: &tilo_actions::InvocationId,
    ) -> Option<tilo_actions::remote::InvocationState> {
        None
    }

    fn cancel(
        &self,
        _caller: &tilo_actions::remote::Caller,
        _id: &tilo_actions::InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), tilo_actions::remote::ActionCallError>> {
        tilo_actions::remote::Pending::ready(Err(
            tilo_actions::remote::ActionCallError::UnknownInvocation,
        ))
    }
}

/// The session asks the test's daemon nothing of its workspaces' people or settings.
struct Unasked;

impl ShellAsks for Unasked {
    fn account(
        &self,
        _resource: &str,
        _action: &str,
        _arguments: serde_json::Map<String, serde_json::Value>,
    ) -> tilo_session_client::engine::ServiceAnswer {
        Box::pin(std::future::ready(Err(
            "the test's daemon answers no account actions".to_owned(),
        )))
    }

    fn invoke(
        &self,
        _workspace: Option<String>,
        _request: tilo_actions::remote::InvokeRequest,
    ) -> tilo_session_client::engine::ServiceAnswer {
        Box::pin(std::future::ready(Err(
            "the test's daemon answers no actions".to_owned(),
        )))
    }
}

/// The workspaces' standing, which no test of the documents follows.
struct NoStanding;

impl PublishedResources for NoStanding {
    fn snapshot(
        &self,
        _caller: &Caller,
        _resource: &str,
    ) -> tokio::sync::oneshot::Receiver<Result<Reading, String>> {
        let (reply, answered) = tokio::sync::oneshot::channel();
        let _ = reply.send(Err("the test's daemon publishes no standing".to_owned()));
        answered
    }

    fn watch(
        &self,
        _caller: &Caller,
        _resource: &str,
        _sink: tokio::sync::mpsc::UnboundedSender<runtime_collab::wire::Event>,
    ) -> Result<ResourceWatch, String> {
        Err("the test's daemon publishes no standing".to_owned())
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
