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

use tilo_app_host::services::{
    AuthenticatedApplicationLaunch, LaunchPlace, LaunchableApplication, LauncherServices,
    ProcessServices,
};
use tilo_app_host::shell::{ShellActions, ShellCaller, ShellDispatch, ShellLinks, ShellServices};
use tilo_desktop_model::WorkspaceId;
use tilo_peer_state::workspace_documents::{DeviceDocuments, DeviceWrites};
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::device_documents::{ServiceDocuments, ServiceWrites};
use tilo_workspace_service::workspaces::Workspaces;
use tilo_workspace_service::workspaces::fixtures::{
    TestGroupKeys, TestHost, device_cert, hosting_device, signed_pre_admission, test_host,
    test_host_holding,
};

use crate::layout::Layouts;
use crate::workspace_view::WorkspaceView;

/// The device's certificate authority as a device's daemon keeps it: the device acts under its
/// own certificate in every workspace it holds, and admits no other.
struct ThisDevice(DeviceCert);

impl tilo_app_host::certificate_authority::CertificateAuthority for ThisDevice {
    fn authorize(&self, _: uuid::Uuid, certificate: &DeviceCert) -> Result<(), String> {
        (certificate == &self.0)
            .then_some(())
            .ok_or_else(|| "a certificate other than this device's".to_owned())
    }

    fn app_certificate(&self, _: &str) -> Result<DeviceCert, String> {
        Ok(self.0.clone())
    }
}

/// The device's daemon, holding its workspaces on a real workspace service, and the shell link it
/// answers the session on.
pub(crate) struct TestDaemon {
    host: TestHost,
    _dispatch: tilo_app_host::collab_broker::BrokerHandle,
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
    /// The session's one stream of record changes, as its roles reach the workspaces here.
    changes: crate::platform::records::Changes,
    _dir: tempfile::TempDir,
}

impl TestDaemon {
    /// Start the daemon, and open the session's link to it.
    pub(crate) fn start() -> Self {
        Self::launching(None)
    }

    /// Start the daemon, admitting an app's connection as the launch `launcher` recorded, and
    /// open the session's link to it. The daemon carries out the shell's window steps on the
    /// apps it admits, as a device's daemon does.
    pub(crate) fn launching(launcher: Option<Arc<dyn LauncherServices>>) -> Self {
        Self::start_as(hosting_device(), None, launcher)
    }

    /// The person's two devices, each with its daemon: the first founds `workspace` as `name`
    /// and holds it, and the second, a further device of the same person, joined it from the
    /// first, so the two daemons carry each other's writes to it. Both hold the workspace's keys.
    pub(crate) fn two_devices(workspace: WorkspaceId, name: &str) -> (Self, Self) {
        let keys = Arc::new(TestGroupKeys::default());
        let first = Self::start_as(hosting_device(), Some(Arc::clone(&keys)), None);
        first.found(workspace, name);
        let person = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let second = Self::start_as(
            device_cert(
                &person,
                "laptop",
                tilo_user_identity::DeviceCapabilities::member(),
            ),
            Some(keys),
            None,
        );
        let id = workspace.to_string();
        let (host, cert) = (first.workspaces().clone(), first.cert.clone());
        let recipient = second.cert.user_public_key;
        let issued = tilo_rt::app()
            .block_on(async move {
                host.pre_admit(
                    &cert,
                    tilo_workspace_service::workspaces::PreAdmitRequest {
                        request_id: [0x5a; 32],
                        grant: signed_pre_admission(&id, &[0x5a; 8]),
                        recipient_user_public_key: Some(recipient),
                        inviter_name: None,
                    },
                )
                .await
            })
            .expect("the runtime runs")
            .expect("the first device invites its person's second");
        let invitation = tilo_peer_state::join::encode_invitation_text(&issued.signed_invitation);
        let joining = second.workspaces().clone();
        tilo_rt::app()
            .block_on(async move { joining.join_member(&invitation).await })
            .expect("the runtime runs")
            .expect("the second device joins the workspace from the first");
        second.read_standing(workspace);
        (first, second)
    }

    /// Start the daemon of the device `(key, cert)`, holding the group keys `keys` when the
    /// test's devices share them, and admitting an app's connection as the launch `launcher`
    /// recorded, and open the session's link to it.
    #[expect(
        clippy::too_many_lines,
        reason = "one test's whole flow, from its setup to what it proves"
    )]
    fn start_as(
        (key, cert): (ed25519_dalek::SigningKey, DeviceCert),
        keys: Option<Arc<TestGroupKeys>>,
        launcher: Option<Arc<dyn LauncherServices>>,
    ) -> Self {
        let runtime = tilo_rt::app();
        let dir = tempfile::tempdir().expect("a state directory");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("the daemon's directory is its own");
        let socket = dir.path().join("collab.sock");
        let state = dir.path().join("host");
        let host = runtime
            .block_on(async {
                match keys {
                    Some(keys) => test_host_holding(&state, &key, &cert, keys).await,
                    None => test_host(&state, &key, &cert).await,
                }
            })
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
        let launcher_present = launcher.is_some();
        // The launcher this test starts its apps through execs them without a launcher's launch
        // hold, so an app can reach the daemon before the session has recorded its launch.
        let launcher =
            launcher.map(|launcher| Arc::new(Held(launcher)) as Arc<dyn LauncherServices>);
        let links = Arc::new(ShellLinks::default());
        // The person's workspaces as the daemon publishes them to the shell: the same reading the
        // device's own daemon makes, following what changes here, so the session that links to
        // this daemon holds what it holds.
        let standing = tilo_runtime::device::Standing::new(
            workspaces.clone(),
            cert.clone(),
            Arc::new(key.clone()),
        );
        standing.follow();
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
                standing,
                published: None,
                workspaces: workspaces.clone(),
                identity: None,
                api: None,
                live: None,
                vpn: None,
                speaking: None,
            })),
            // The providers registered on the device, the seat a shell serves among them.
            providers: Some(workspaces.providers()),
            // The apps it admits act as the device's person, as the device's certificate
            // authority answers for each workspace.
            native_certificates: launcher_present.then(|| {
                Arc::new(ThisDevice(cert.clone()))
                    as Arc<dyn tilo_app_host::certificate_authority::CertificateAuthority>
            }),
            launcher,
            ..ProcessServices::default()
        };
        // The apps it admits are checked against the workspaces it holds, as a device's daemon
        // checks them.
        let membership = launcher_present.then(|| {
            Arc::new(workspaces.clone()) as tilo_app_host::services::WorkspaceAuthorityHandle
        });
        let fanout = Arc::new(tilo_app_host::collab_broker::FanoutState::new());
        tilo_runtime::device::provide_window_steps(Arc::clone(&fanout), &workspaces.providers());
        let dispatch = runtime
            .block_on(async {
                tilo_app_host::collab_broker::spawn_with_services(
                    socket.clone(),
                    membership,
                    tilo_app_host::collab_outbound::PeerPublisher::disabled(),
                    None,
                    services,
                    Arc::default(),
                    fanout,
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
            crate::device_shell::asks(link(&socket, &links, &cert)),
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
            changes: crate::platform::records::Changes::default(),
            _dir: dir,
        }
    }

    /// The session's one stream of record changes, as its roles reach the workspaces here.
    pub(crate) fn changes(&self) -> crate::platform::records::Changes {
        self.changes.clone()
    }

    /// A daemon holding each of `workspaces` whose person's shell writes their stores, as a
    /// device's own daemon does: it acts as the device it runs on, and holds each workspace's
    /// keys, as the device that founded it does.
    pub(crate) fn writing(workspaces: &[&str]) -> Self {
        Self::writing_launched(workspaces, None)
    }

    /// [`Self::writing`], admitting an app's connection as the launch `launcher` recorded.
    pub(crate) fn writing_launched(
        workspaces: &[&str],
        launcher: Option<Arc<dyn LauncherServices>>,
    ) -> Self {
        Self::writing_launched_for(workspaces, None, launcher)
    }

    /// [`Self::writing_launched`] with the workspace `elsewhere` names founded for `owner`
    /// instead of this person: this device stays one of its administrators, as in a workspace
    /// another person made.
    pub(crate) fn writing_launched_for(
        workspaces: &[&str],
        elsewhere: Option<(&str, [u8; 32])>,
        launcher: Option<Arc<dyn LauncherServices>>,
    ) -> Self {
        let daemon = Self::holding_launched_for(workspaces, elsewhere, launcher);
        let service = daemon.workspaces().clone();
        service.set_member_device(Arc::new(tilo_workspace_service::workspaces::MemberDevice {
            cert: daemon.cert.clone(),
            signer: Arc::new(daemon.key.clone()),
        }));
        for workspace in workspaces {
            let route = tilo_peer_state::document_frame::DocumentRoute {
                app_id: crate::references::REFERENCES_APP.into(),
                cluster_id: (*workspace).to_owned(),
                document: String::new(),
            };
            let service = service.clone();
            tilo_rt::app()
                .block_on(async move { service.own_route_keys(&route, true).await })
                .expect("the runtime runs")
                .expect("the device takes part in the workspace's keys")
                .expect("the workspace's keys open");
        }
        daemon
    }

    /// A daemon holding each of `workspaces`.
    pub(crate) fn holding(workspaces: &[&str]) -> Self {
        Self::holding_launched_for(workspaces, None, None)
    }

    /// A daemon holding each of `workspaces`, and the one `elsewhere` names founded for its
    /// owner.
    fn holding_launched_for(
        workspaces: &[&str],
        elsewhere: Option<(&str, [u8; 32])>,
        launcher: Option<Arc<dyn LauncherServices>>,
    ) -> Self {
        let daemon = Self::launching(launcher);
        for workspace in workspaces {
            let id = WorkspaceId::parse(workspace).expect("a workspace");
            match elsewhere {
                Some((elsewhere, owner)) if elsewhere == *workspace => {
                    daemon.found_for(id, workspace, Some(owner));
                }
                _ => daemon.found(id, workspace),
            }
        }
        daemon
    }

    /// The workspaces the daemon holds, as the session reads and writes a record of theirs
    /// itself.
    pub(crate) fn device_workspaces(&self) -> crate::device_workspaces::DeviceWorkspaces {
        crate::device_workspaces::DeviceWorkspaces::new(
            self.view(),
            self.reads(),
            self.writes(),
            self.changes.clone(),
        )
    }

    /// Found `workspace` as `name` here, as the person's shell asks the daemon to, and have the
    /// session's view read how it stands, as the daemon publishes it.
    pub(crate) fn found(&self, workspace: WorkspaceId, name: &str) {
        self.found_for(workspace, name, None);
    }

    /// [`Self::found`] for `owner`, when named: the workspace is theirs, and this device stays
    /// one of its administrators.
    fn found_for(&self, workspace: WorkspaceId, name: &str, owner: Option<[u8; 32]>) {
        let id = workspace.to_string();
        let workspaces = self.host.workspaces().clone();
        tilo_rt::app()
            .block_on(async move {
                match owner {
                    Some(owner) => workspaces.create_for(name, Some(&id), owner).await,
                    None => workspaces.create_workspace(name, Some(&id)).await,
                }
                .expect("the workspace is founded");
            })
            .expect("the runtime runs");
        self.read_standing(workspace);
    }

    /// The daemon's copy of `workspace`: the one it hosts, or the one it has as a member.
    pub(crate) fn copy(&self, workspace: WorkspaceId) -> tilo_peer_state::StatePeer {
        let workspaces = self.host.workspaces().clone();
        tilo_rt::app()
            .block_on(async move {
                match workspaces.hosted_copy(workspace).await {
                    Some(listed) => Some(listed),
                    None => workspaces.member_copy(workspace).await,
                }
            })
            .expect("the runtime runs")
            .expect("the daemon holds the workspace")
            .1
    }

    /// Have the session's view read how `workspace` stands here, as the daemon publishes it.
    pub(crate) fn read_standing(&self, workspace: WorkspaceId) {
        let peer = self.copy(workspace);
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

    /// Where the daemon answers the apps it admits, and the person's shell.
    pub(crate) fn socket(&self) -> &std::path::Path {
        &self.socket
    }

    /// The workspaces the daemon holds, as its own roles reach them.
    pub(crate) fn workspaces(&self) -> &Workspaces {
        self.host.workspaces()
    }

    /// The device's key, which the daemon signs its writes with.
    pub(crate) fn key(&self) -> ed25519_dalek::SigningKey {
        self.key.clone()
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
    pub(crate) fn seat(&self) -> tilo_app_host::seat::Seat {
        tilo_app_host::seat::Seat::new(
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

/// How long an app's connection waits for the launch record the session makes of it: the real
/// launcher holds an app until that record exists, and the test's stand-in does not.
const WAITING_FOR_LAUNCH: std::time::Duration = std::time::Duration::from_millis(500);

/// The launcher a test hands the daemon, with the launch hold's promise: an app that connects
/// before the session has recorded the launch of its process waits for that record here instead
/// of being refused.
struct Held(Arc<dyn LauncherServices>);

impl LauncherServices for Held {
    fn applications(&self) -> Vec<LaunchableApplication> {
        self.0.applications()
    }

    fn launch(
        &self,
        app: &str,
        workspace: &tilo_desktop_model::Workspace,
        record: Option<&str>,
        place: &LaunchPlace,
    ) -> Result<(), String> {
        self.0.launch(app, workspace, record, place)
    }

    fn attest_process(&self, pid: u32, app: &str, workspace: &str) -> bool {
        self.0.attest_process(pid, app, workspace)
    }

    fn authenticated_launch(
        &self,
        pid: u32,
        app: &str,
        workspace: &str,
        window_instance: Option<u64>,
    ) -> Result<Option<AuthenticatedApplicationLaunch>, String> {
        let deadline = std::time::Instant::now()
            .checked_add(WAITING_FOR_LAUNCH)
            .unwrap_or_else(std::time::Instant::now);
        loop {
            if let Some(launch) =
                self.0
                    .authenticated_launch(pid, app, workspace, window_instance)?
            {
                return Ok(Some(launch));
            }
            if std::time::Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
}

/// The daemon's one dispatch, as its person's shell reaches it: the workspaces' governance (their
/// agents included) carried out as this device, and every other call handed to the provider that
/// serves it, exactly as the daemon does in a running session.
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
            if shell.person == cert.user_public_key {
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
            } else {
                Err("this device acts for its own person only".to_owned())
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
        caller: &tilo_actions::remote::Caller,
        request: tilo_actions::remote::InvokeRequest,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionCallError>,
    > {
        // A governance operation is the device action service's answer; every other call is the
        // workspace's own, handed to the provider that serves it as a device's daemon hands it —
        // the window steps the shell asks of its apps among them.
        if let Some((workspace, _)) =
            tilo_actions::remote::workspace_governance_operation(&request.target.address)
        {
            let workspace = workspace.to_owned();
            // A change to the workspace's people and settings is the device's own, signed with
            // its key, as the daemon's shell answers it.
            if request.target.address.action == tilo_peer_state::workspace_view::MEMBERSHIP {
                let command = match &request.argument {
                    tilo_actions::remote::InvokeArgument::Json(arguments) => arguments
                        .get("command")
                        .cloned()
                        .and_then(|command| serde_json::from_value(command).ok()),
                    _ => None,
                };
                let Some(command) = command else {
                    return tilo_actions::remote::Pending::ready(Err(
                        tilo_actions::remote::ActionCallError::InvalidRequest {
                            reason: "a change to a workspace's people names its `command`"
                                .to_owned(),
                        },
                    ));
                };
                let (answer, pending) = tilo_actions::remote::pending();
                let workspaces = self.workspaces.clone();
                drop(tilo_rt::app().spawn(async move {
                    let status = match workspaces
                        .commit_own_change(&workspace, command, None)
                        .await
                    {
                        Ok(()) => tilo_actions::remote::ReceiptStatus::Completed {
                            output: tilo_actions::ActionOutput::default(),
                        },
                        Err(reason) => tilo_actions::remote::ReceiptStatus::Rejected {
                            error: tilo_actions::remote::ActionCallError::Refused { reason },
                        },
                    };
                    answer.complete(Ok(tilo_actions::remote::InvocationReceipt {
                        invocation: tilo_actions::InvocationId::new(),
                        status,
                    }));
                }));
                return pending;
            }
            let cert = self.workspaces.device_cert();
            let caller = tilo_actions::remote::Caller::authenticated(
                cert.user_public_key,
                cert.device_public_key,
            )
            .in_workspace(workspace);
            return self.workspaces.device_actions().invoke(&caller, request);
        }
        self.workspaces
            .providers()
            .invoke(caller, request)
            .unwrap_or_else(|| {
                tilo_actions::remote::Pending::ready(Err(
                    tilo_actions::remote::ActionCallError::UnknownAction,
                ))
            })
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
