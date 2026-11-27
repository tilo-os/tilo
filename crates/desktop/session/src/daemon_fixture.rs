//! The device's daemon as a test's session reaches it: a real workspace service holding the
//! device's workspaces, answering the person's shell on the link it hands the session, through
//! the same dispatch, reads and writes the daemon answers a desktop with. The session reads and
//! writes its workspaces' documents there, and the daemon signs and seals each write as the device.

// A fixture only tests compile.
#![cfg(all(any(test, feature = "test-support"), target_os = "linux"))]
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
use tilo_app_host::services::{Caller, PublishedResources, Reading, ResourceWatch};
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
use tilo_workspace_service::workspaces::person_workspace::devices::OnlineDevices;

#[cfg(test)]
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

/// Read `record` until it holds what `done` asks for, and fail saying so when it does not, so a
/// test that waits on the daemon fails fast instead of hanging.
fn settle<F>(
    record: &crate::followed_resource::FollowedResource<
        tilo_workspace_service::workspaces::person_workspace::devices::Devices,
    >,
    done: F,
) where
    F: Fn(&tilo_workspace_service::workspaces::person_workspace::devices::Devices) -> bool,
{
    let deadline = std::time::Instant::now()
        .checked_add(std::time::Duration::from_secs(20))
        .unwrap_or_else(std::time::Instant::now);
    loop {
        let read = record.read_now();
        assert!(
            read.is_ok(),
            "a test waits on the daemon, which answers: {:?}",
            read.err()
        );
        if done(&record.get()) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the daemon's record of the person's devices never had what the test wrote"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// The speakers frames a session's link asked this daemon to carry, in order: what the person's
/// other devices were sent, as the session wrote it.
#[derive(Default)]
pub struct SpeakersSaid {
    frames: std::sync::Mutex<Vec<tilo_session_client::speakers::Frame>>,
    /// Whether a session holds the link now.
    attached: std::sync::atomic::AtomicBool,
    /// The link a session holds, while it holds one: where what its person's other devices said
    /// here is written.
    hearing: std::sync::Mutex<
        Option<tokio::sync::mpsc::UnboundedSender<tilo_session_client::speakers::Frame>>,
    >,
}

impl SpeakersSaid {
    fn keep(&self, frame: tilo_session_client::speakers::Frame) {
        self.frames.lock().expect("the speakers frames").push(frame);
    }

    /// Whether a session holds this daemon's speakers link now: what a test waits for before it
    /// presses a row that sends.
    #[must_use]
    pub fn attached(&self) -> bool {
        self.attached.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Every frame the link carried so far, in order.
    #[must_use]
    pub fn frames(&self) -> Vec<tilo_session_client::speakers::Frame> {
        self.frames.lock().expect("the speakers frames").clone()
    }

    /// Write `frame` to the session's link, as another of the person's devices said it here.
    pub fn hearing(&self, frame: tilo_session_client::speakers::Frame) {
        if let Some(link) = self.hearing.lock().expect("the speakers link").as_ref() {
            let _ = link.send(frame);
        }
    }
}

/// What carries a session's speakers link for as long as it lasts: the frames it writes, kept.
fn speakers_links(said: &Arc<SpeakersSaid>) -> tilo_app_host::shell::SpeakersLinks {
    let said = Arc::clone(said);
    Arc::new(move |stream| {
        let said = Arc::clone(&said);
        Box::pin(async move {
            said.attached
                .store(true, std::sync::atomic::Ordering::Relaxed);
            let (mut reader, mut writer) = tokio::io::split(stream);
            let (hearing, mut to_hear) = tokio::sync::mpsc::unbounded_channel();
            *said.hearing.lock().expect("the speakers link") = Some(hearing);
            loop {
                tokio::select! {
                    frame = tilo_session_client::speakers::read(&mut reader) => {
                        let Ok(frame) = frame else {
                            return;
                        };
                        said.keep(frame);
                    }
                    Some(frame) = to_hear.recv() => {
                        if tilo_session_client::speakers::write(&mut writer, &frame)
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                }
            }
        })
    })
}

/// The device's daemon, holding its workspaces on a real workspace service, and the shell link it
/// answers the session on.
pub struct TestDaemon {
    host: TestHost,
    _dispatch: tilo_app_host::collab_broker::BrokerHandle,
    key: ed25519_dalek::SigningKey,
    cert: DeviceCert,
    reads: Arc<dyn DeviceDocuments>,
    writes: Arc<dyn DeviceWrites>,
    /// What the workspaces' writes reach this daemon as, for the roles a session attaches to them.
    #[cfg(test)]
    feed: tilo_session_client::daemon_documents::DaemonWritesFeed,
    shell: tilo_session_client::daemon_documents::DaemonShell,
    view: Arc<WorkspaceView>,
    /// Where the daemon answers the person's shell.
    socket: std::path::PathBuf,
    /// The links the daemon made for the shells it starts.
    links: Arc<ShellLinks>,
    /// The session's one stream of record changes, as its roles reach the workspaces here.
    changes: crate::platform::records::Changes,
    /// The person's devices this daemon says are online now: the reading the session follows.
    online: Arc<Online>,
    /// The person's other devices this daemon hears on this machine's local network.
    nearby: Arc<Online>,
    /// What a session's speakers link asked this daemon to carry for it.
    speakers: Arc<SpeakersSaid>,
    /// The device key this process took as the daemon's, where a session in this process reads
    /// its identity from: held for the daemon's life, as the one writer of the key.
    identity: std::sync::OnceLock<tilo_device_identity::persist::DeviceKeyLock>,
    _dir: tempfile::TempDir,
}

impl TestDaemon {
    /// Start the daemon, and open the session's link to it.
    pub fn start() -> Self {
        Self::launching(None)
    }

    /// Start the daemon, admitting an app's connection as the launch `launcher` recorded, and
    /// open the session's link to it. The daemon carries out the shell's window steps on the
    /// apps it admits, as a device's daemon does.
    pub fn launching(launcher: Option<Arc<dyn LauncherServices>>) -> Self {
        Self::start_as(hosting_device(), None, launcher)
    }

    /// The person's two devices, each with its daemon: the first founds `workspace` as `name`
    /// and holds it, and the second, a further device of the same person, joined it from the
    /// first, so the two daemons carry each other's writes to it. Both hold the workspace's keys.
    pub fn two_devices(workspace: WorkspaceId, name: &str) -> (Self, Self) {
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
        let online =
            Online::new(tilo_workspace_service::workspaces::person_workspace::devices::ONLINE);
        let nearby =
            Online::new(tilo_workspace_service::workspaces::person_workspace::devices::NEARBY);
        let speakers = Arc::new(SpeakersSaid::default());
        let services = ProcessServices {
            shell: Some(Arc::new(ShellServices {
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
                published: Some(Arc::new(
                    tilo_runtime::device::published::Published::new()
                        .serving(
                            &[tilo_workspace_service::workspaces::person_workspace::devices::ONLINE],
                            Arc::clone(&online) as Arc<dyn PublishedResources>,
                        )
                        .serving(
                            &[tilo_workspace_service::workspaces::person_workspace::devices::NEARBY],
                            Arc::clone(&nearby) as Arc<dyn PublishedResources>,
                        ),
                )),
                workspaces: workspaces.clone(),
                identity: None,
                api: None,
                live: None,
                vpn: None,
                speakers: Some(speakers_links(&speakers)),
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
            #[cfg(test)]
            feed: tilo_session_client::daemon_documents::DaemonWritesFeed::start(&shell),
            shell,
            view,
            socket,
            links,
            changes: crate::platform::records::Changes::default(),
            online,
            nearby,
            speakers,
            identity: std::sync::OnceLock::new(),
            _dir: dir,
        }
    }

    /// Keep this daemon's device identity where a session in this process reads its own: its
    /// key and certificate, in `home`'s identity directory, and the file secret store a test's
    /// identity keeps its keys in.
    pub fn serve_identity(&self, home: &mut tilo_test_env::TestHome) {
        let root = home.root().join("identity");
        home.env()
            .set("TILO_IDENTITY_ROOT", &root)
            .set(tilo_identity::provisioning::FILE_SECRETS_ENV, "1");
        let paths = tilo_identity::provisioning::collaboration_identity_paths();
        paths
            .ensure_directories()
            .expect("the identity directories are made");
        tilo_device_identity::persist::write_device_key(&paths.device_privkey, &self.key)
            .expect("the device key is written");
        tilo_device_identity::persist::write_public_key(
            &paths.user_pubkey,
            &self.cert.user_public_key,
        )
        .expect("the device's person is written");
        tilo_device_identity::persist::write_device_cert(
            &paths.device_cert_for_user(&self.cert.user_public_key),
            &self.cert,
        )
        .expect("the device certificate is written");
        // The one process that writes as the device is this one: a session here reads the key it
        // took, as it reads the key of the daemon that started it.
        let lock = tilo_device_identity::persist::take_device_key(&paths)
            .expect("this process takes the device's key");
        let _ = self.identity.set(lock);
    }

    /// Say these devices of the person's are heard on this machine's local network, as this
    /// daemon's own reading does.
    pub fn set_nearby(&self, devices: impl IntoIterator<Item = [u8; 32]>) {
        self.nearby.set(devices);
    }

    /// Found the person's own workspace here, as the device that first sets their identity up
    /// does: where the record of their devices is kept.
    pub fn found_person_workspace(&self) {
        let workspaces = self.workspaces().clone();
        let founded = tilo_rt::app()
            .block_on(async move {
                tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    tilo_workspace_service::workspaces::person_workspace::found(&workspaces),
                )
                .await
            })
            .expect("the runtime runs")
            .expect("founding the person's own workspace answers in time")
            .expect("the daemon holds the person's own workspace");
        let _ = founded;
    }

    /// The person's record of their devices, read and changed through this daemon as their shell
    /// does.
    pub(crate) fn person_devices(
        &self,
    ) -> crate::followed_resource::FollowedResource<
        tilo_workspace_service::workspaces::person_workspace::devices::Devices,
    > {
        use tilo_workspace_service::workspaces::person_workspace::devices::DEVICES;
        let person = self.cert.user_public_key;
        let member = tilo_member_types::PersonId::from_root(person);
        crate::followed_resource::FollowedResource::follow(
            &self.shell(),
            (
                &tilo_server_protocol::servers::person_workspace_id(&person),
                DEVICES.0,
                DEVICES.1,
            ),
            member.as_str(),
            None,
            std::sync::Arc::new(|| {}),
        )
    }

    /// The certificate of `device`, one of `label`'s, as this fixture's person certifies it.
    pub fn certifying(&self, device: [u8; 32], label: &str) -> DeviceCert {
        let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let mut cert = DeviceCert {
            user_public_key: self.cert.user_public_key,
            device_public_key: device,
            device_label: label.into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, &root)
            .expect("the fixture's person signs the device");
        cert
    }

    /// Keep `cert` in the person's record of their devices, saying what the device is and the
    /// outputs it offers, as that device writes it of itself.
    pub fn keeping(
        &self,
        cert: &DeviceCert,
        about: &crate::platform::fabric::DeviceAbout,
        endpoints: &[tilo_workspace_service::workspaces::person_workspace::speaker::SpeakerEndpoint],
    ) {
        use tilo_workspace_service::workspaces::person_workspace::devices::Devices;
        let record = self.person_devices();
        record
            .invoke(&Devices::keeping(cert).expect("the device certificate encodes"))
            .expect("the record keeps the device");
        record
            .invoke(&Devices::KEEP_FACT.with((
                hex::encode(cert.device_public_key),
                hex::encode(postcard::to_allocvec(cert).expect("the certificate encodes")),
                tilo_network_actor::fabric::ABOUT_FACT.to_owned(),
                hex::encode(postcard::to_allocvec(about).expect("what it is encodes")),
            )))
            .expect("the device says what it is");
        record
            .invoke(&Devices::SAY_SPEAKER_ENDPOINTS.with((
                hex::encode(cert.device_public_key),
                hex::encode(postcard::to_allocvec(endpoints).expect("endpoints encode")),
            )))
            .expect("the device offers its output");
        let key = tilo_resource::Id::from_raw(hex::encode(cert.device_public_key));
        settle(&record, |record| record.devices.get(&key).is_some());
    }

    /// The person's devices as this daemon's shell reads them: their record and its readings of
    /// which devices are online and which are heard here, and the link that carries a route to
    /// another device.
    pub fn fabric(&self) -> crate::platform::fabric::SharedLink {
        use tilo_workspace_service::workspaces::person_workspace::devices::{NEARBY, ONLINE};
        let person = self.cert.user_public_key;
        let workspace = tilo_server_protocol::servers::person_workspace_id(&person);
        let wake = std::sync::Arc::new(|| {});
        let record = self.person_devices();
        // Nothing else drives this follow here, as the person's session does in product: read the
        // record until it holds what this daemon keeps, so what a test published is what its
        // fabric reads, and fail the test rather than wait forever when it never does.
        settle(&record, |record| !record.devices.is_empty());
        let online = crate::person_store::DaemonReading::follow(
            &self.shell(),
            &workspace,
            ONLINE,
            wake.clone(),
        );
        let nearby =
            crate::person_store::DaemonReading::follow(&self.shell(), &workspace, NEARBY, wake);
        let local = Some(
            crate::platform::fabric::DeviceId::new(hex::encode(self.cert.device_public_key))
                .expect("the daemon's device key names its device"),
        );
        // Continuity as this device has it: a switch nobody turned off is on.
        let continuity = crate::platform::fabric::Continuity::default();
        continuity.read(&serde_json::Map::new());
        std::sync::Arc::new(crate::person_store::DaemonFabric::new(
            record.shared(),
            online,
            nearby,
            crate::person_store::SpeakersLink::attach(&self.shell(), local),
            self.cert.clone(),
            continuity,
        ))
    }

    /// Write `frame` to the session's link, as another of the person's devices said it here.
    pub fn hearing(&self, frame: tilo_session_client::speakers::Frame) {
        self.speakers.hearing(frame);
    }

    /// Whether a session holds this daemon's speakers link now.
    #[must_use]
    pub fn speakers_attached(&self) -> bool {
        self.speakers.attached()
    }

    /// What this daemon's shell was asked to hand another of the person's devices, as the session
    /// wrote it: each envelope with the device it is for.
    pub fn sent_speakers(&self) -> Vec<([u8; 32], Vec<u8>)> {
        let mut sent = Vec::new();
        for frame in self.speakers.frames() {
            if let tilo_session_client::speakers::Frame::Send { peer, envelope } = frame {
                sent.push((peer, envelope));
            }
        }
        sent
    }

    /// Say these devices of the person's are online now, as this daemon's own reading does.
    pub fn set_online(&self, devices: impl IntoIterator<Item = [u8; 32]>) {
        self.online.set(devices);
    }

    /// The session's one stream of record changes, as its roles reach the workspaces here.
    pub fn changes(&self) -> crate::platform::records::Changes {
        self.changes.clone()
    }

    /// A daemon holding each of `workspaces` whose person's shell writes their stores, as a
    /// device's own daemon does: it acts as the device it runs on, and holds each workspace's
    /// keys, as the device that founded it does.
    pub fn writing(workspaces: &[&str]) -> Self {
        Self::writing_launched(workspaces, None)
    }

    /// [`Self::writing`], admitting an app's connection as the launch `launcher` recorded.
    pub fn writing_launched(
        workspaces: &[&str],
        launcher: Option<Arc<dyn LauncherServices>>,
    ) -> Self {
        Self::writing_launched_for(workspaces, None, launcher)
    }

    /// [`Self::writing_launched`] with the workspace `elsewhere` names founded for `owner`
    /// instead of this person: this device stays one of its administrators, as in a workspace
    /// another person made.
    pub fn writing_launched_for(
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
    pub fn holding(workspaces: &[&str]) -> Self {
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
    #[cfg(test)]
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
    pub fn found(&self, workspace: WorkspaceId, name: &str) {
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
    pub fn copy(&self, workspace: WorkspaceId) -> tilo_peer_state::StatePeer {
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
    pub fn read_standing(&self, workspace: WorkspaceId) {
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
    pub fn socket(&self) -> &std::path::Path {
        &self.socket
    }

    /// The workspaces the daemon holds, as its own roles reach them.
    pub fn workspaces(&self) -> &Workspaces {
        self.host.workspaces()
    }

    /// The device's key, which the daemon signs its writes with.
    pub fn key(&self) -> ed25519_dalek::SigningKey {
        self.key.clone()
    }

    /// The device's certificate: whose key the daemon signs its writes with.
    pub fn certificate(&self) -> DeviceCert {
        self.cert.clone()
    }

    /// The session's reads of the documents of the workspaces the daemon holds, on its link.
    pub fn reads(&self) -> Arc<dyn DeviceDocuments> {
        Arc::clone(&self.reads)
    }

    /// The session's writes of those documents, on its link: the daemon signs and seals them.
    pub fn writes(&self) -> Arc<dyn DeviceWrites> {
        Arc::clone(&self.writes)
    }

    /// A shell link of the daemon's person, opened: what a session serves its seat on.
    pub fn shell_client(&self) -> runtime_collab::ShellClient {
        link(&self.socket, &self.links, &self.cert)
    }

    /// The seat as the daemon's broker reaches it, while a shell serves it.
    pub fn seat(&self) -> tilo_app_host::seat::Seat {
        tilo_app_host::seat::Seat::new(
            self.workspaces().providers(),
            tilo_rt::app().handle().clone(),
        )
    }

    /// The session's link to the daemon: where its reads are followed and its actions run.
    pub fn shell(&self) -> tilo_session_client::daemon_documents::DaemonShell {
        self.shell.clone()
    }

    /// How the person's workspaces stand, as the session's view reads them: each workspace
    /// founded here, held by the daemon.
    pub fn view(&self) -> Arc<WorkspaceView> {
        Arc::clone(&self.view)
    }

    /// Have `layouts` read and write the workspaces through the daemon, through `reads` and
    /// `writes`, and read a record again whenever the daemon tells of a write to it.
    #[cfg(test)]
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
    #[cfg(test)]
    pub(crate) fn attach(&self, layouts: &Layouts) {
        self.attach_as(layouts, &self.reads(), &self.writes());
    }

    /// The shell link the daemon made for the session, opened and handed over, as
    /// [`crate::Options::shell_link`] takes it for a session in this process: the daemon's
    /// workspace resources, people and settings are then the session's own.
    pub fn shell_link(&self) -> std::os::fd::OwnedFd {
        use std::io::Write as _;
        let mut link = tilo_files::native::socket::connect(&self.socket)
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
            let mut link = tilo_files::native::socket::tokio::connect(socket)
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
                let command: Result<
                    tilo_peer_state::workspace_membership::WorkspaceMembershipCommand,
                    String,
                > = match &request.argument {
                    tilo_actions::remote::InvokeArgument::Json(arguments) => arguments
                        .get("command")
                        .cloned()
                        .ok_or_else(|| {
                            "a change to a workspace's people names its `command`".to_owned()
                        })
                        .and_then(|command| {
                            serde_json::from_value(command).map_err(|error| {
                                format!("the `command` a workspace's people change names does not read: {error}")
                            })
                        }),
                    _ => Err(
                        "a change to a workspace's people names its `command`".to_owned(),
                    ),
                };
                let command = match command {
                    Ok(command) => command,
                    Err(reason) => {
                        return tilo_actions::remote::Pending::ready(Err(
                            tilo_actions::remote::ActionCallError::InvalidRequest { reason },
                        ));
                    }
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

/// The person's devices this daemon says are online now, as the daemon's own reading publishes
/// them: what the session follows over the shell link.
pub struct Online {
    /// The resource this reading is published under.
    resource: &'static str,
    listed: std::sync::Mutex<OnlineDevices>,
    revision: std::sync::atomic::AtomicU64,
    changed: tokio::sync::watch::Sender<u64>,
    watches: std::sync::Mutex<Vec<u64>>,
    next_watch: std::sync::atomic::AtomicU64,
    /// This provider, for the tasks its watches run: what they read while a watch lives.
    me: std::sync::OnceLock<std::sync::Weak<Self>>,
}

impl Online {
    /// The reading `resource`, with no device in it yet.
    pub fn new(resource: &'static str) -> Arc<Self> {
        let online = Arc::new(Self {
            resource,
            listed: std::sync::Mutex::default(),
            revision: std::sync::atomic::AtomicU64::default(),
            changed: tokio::sync::watch::channel(0).0,
            watches: std::sync::Mutex::default(),
            next_watch: std::sync::atomic::AtomicU64::default(),
            me: std::sync::OnceLock::new(),
        });
        let _ = online.me.set(Arc::downgrade(&online));
        online
    }

    /// Say `devices` are online now.
    pub fn set(&self, devices: impl IntoIterator<Item = [u8; 32]>) {
        let mut online = self.listed.lock().expect("the online reading");
        online.devices = devices.into_iter().map(hex::encode).collect();
        drop(online);
        self.revision
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.changed
            .send_modify(|changed| *changed = changed.wrapping_add(1));
    }

    fn reading(&self) -> Reading {
        let online = self.listed.lock().expect("the online reading").clone();
        let bytes = serde_json::to_vec(&online).expect("the online reading encodes");
        (
            self.revision.load(std::sync::atomic::Ordering::Relaxed),
            Some(bytes),
        )
    }
}

impl PublishedResources for Online {
    fn snapshot(
        &self,
        _caller: &Caller,
        resource: &str,
    ) -> tokio::sync::oneshot::Receiver<Result<Reading, String>> {
        let (answer, answered) = tokio::sync::oneshot::channel();
        let reading = (resource == self.resource).then(|| self.reading());
        let _ = answer.send(Ok(reading.unwrap_or((0, None))));
        answered
    }

    fn watch(
        &self,
        _caller: &Caller,
        resource: &str,
        sink: tokio::sync::mpsc::UnboundedSender<runtime_collab::wire::Event>,
    ) -> Result<ResourceWatch, String> {
        if resource != self.resource {
            return Err(format!("this daemon publishes no `{resource}` resource"));
        }
        let id = self
            .next_watch
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.watches.lock().expect("the online watches").push(id);
        let (revision, reading) = self.reading();
        if sink
            .send(runtime_collab::wire::Event::Reading {
                resource: resource.to_owned(),
                revision,
                reading,
            })
            .is_err()
        {
            return Err("the reader of the reading went away".to_owned());
        }
        let mut changed = self.changed.subscribe();
        let online = self
            .me
            .get()
            .and_then(std::sync::Weak::upgrade)
            .ok_or_else(|| "the reading's provider is not kept".to_owned())?;
        let named = resource.to_owned();
        tokio::spawn(async move {
            while changed.changed().await.is_ok() {
                let (revision, reading) = online.reading();
                if sink
                    .send(runtime_collab::wire::Event::Reading {
                        resource: named.clone(),
                        revision,
                        reading,
                    })
                    .is_err()
                {
                    return;
                }
            }
        });
        Ok(ResourceWatch::new(id))
    }

    fn unwatch(&self, watch: ResourceWatch) {
        self.watches
            .lock()
            .expect("the online watches")
            .retain(|id| *id != watch.id());
    }

    fn invoke(
        &self,
        _caller: &Caller,
        resource: &str,
        action: &str,
        _arguments: Vec<u8>,
    ) -> tokio::sync::oneshot::Receiver<runtime_collab::wire::Receipt> {
        let (answer, answered) = tokio::sync::oneshot::channel();
        let _ = answer.send(runtime_collab::wire::Receipt::Refused {
            code: "unknown_action".to_owned(),
            message: format!("`{resource}` is only read; `{action}` is not its"),
        });
        answered
    }
}
