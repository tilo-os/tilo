use super::*;
use serde_json::json;
use std::{
    process::Stdio,
    time::{Duration, Instant},
};
use tilo_actions::remote::{InvokeArgument, PendingPoll, RemoteActionTarget};
use tilo_app_host::services::WorkspaceGrants;
use tilo_desktop_model::WorkspaceId;
use tilo_test_env::SpawnUnderEnvLock as _;
use tilo_workspace_service::workspaces::app_invocations::AppInvocations as _;

/// The test whose phase runs in a fresh process; libtest selects it by this
/// exact path.
const PROCESS_TEST: &str = "device::services::tests::sdk_apps_join_the_daemon_and_publish_to_mcp";
pub(crate) const PROCESS_ROOT: &str = "TILO_DAEMON_TEST_ROOT";
const APP_HELPER: &str = "TILO_DAEMON_APP_HELPER";

/// A certificate authority over one held workspace and device, for the
/// broker's own certificates: no identity store is read.
struct FixtureAuthority {
    workspace: uuid::Uuid,
    cert: tilo_user_identity::DeviceCert,
    time: tilo_app_host::policy_clock::PolicyTime,
}

impl tilo_app_host::certificate_authority::CertificateAuthority for FixtureAuthority {
    fn policy_time(&self) -> Result<Option<tilo_app_host::policy_clock::PolicyTime>, String> {
        self.time.now_ns().map_err(|error| error.to_string())?;
        Ok(Some(self.time.clone()))
    }
    fn authorize(
        &self,
        workspace: uuid::Uuid,
        cert: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        if workspace != self.workspace || cert != &self.cert {
            return Err("unheld fixture identity".into());
        }
        tilo_user_identity::verify_device_cert_at(
            cert,
            self.time.now_ns().map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())
    }
    fn authorize_with_policy_time(
        &self,
        workspace: uuid::Uuid,
        cert: &tilo_user_identity::DeviceCert,
        time: &tilo_app_host::policy_clock::PolicyTime,
    ) -> Result<(), String> {
        if !time.same_origin(&self.time) {
            return Err("runtime origin changed".into());
        }
        self.authorize(workspace, cert)
    }
}

/// An app whose every call asks its person first.
struct AsksFirst;

impl tilo_actions::remote::ActionProvider for AsksFirst {
    fn query(
        &self,
        _: &Caller,
        _: tilo_actions::remote::CatalogueQuery,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::CataloguePage, tilo_actions::remote::ActionCallError>,
    > {
        tilo_actions::remote::Pending::ready(Err(
            tilo_actions::remote::ActionCallError::UnknownAction,
        ))
    }

    fn invoke(
        &self,
        _: &Caller,
        _: InvokeRequest,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionCallError>,
    > {
        tilo_actions::remote::Pending::ready(Err(tilo_actions::remote::ActionCallError::asks(
            tilo_actions::remote::asked::AskedFacts {
                verb: "refresh".into(),
                ..tilo_actions::remote::asked::AskedFacts::default()
            },
        )))
    }

    fn observe(
        &self,
        _: &Caller,
        _: &tilo_actions::InvocationId,
    ) -> Option<tilo_actions::remote::InvocationState> {
        None
    }

    fn cancel(
        &self,
        _: &Caller,
        _: &tilo_actions::InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), tilo_actions::remote::ActionCallError>> {
        tilo_actions::remote::Pending::ready(Ok(()))
    }
}

#[tokio::test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the broker fixture owns its authority and invocation registry for the whole scenario"
)]
#[allow(clippy::too_many_lines, reason = "one end-to-end ownership scenario")]
async fn runtime_invocation_ownership_survives_device_normalization() {
    use tilo_app_host::{
        collab_app_broker::{AppBrokerState, PublishedAction, PublishedInput, PublishedSurface},
        collab_broker::CrdtRoute,
        test_support::{Workspace, WorkspaceId, WorkspaceTree},
    };
    if isolated_signing_identity(
        "device::services::tests::runtime_invocation_ownership_survives_device_normalization",
    )
    .is_none()
    {
        return;
    }
    // No daemon runs here: this process signs as the device, as the daemon does.
    let _daemon_key = taking_the_device_key();

    let placed = WorkspaceId::new();
    let (workspaces, directory) = ServiceWorkspaces::test_hosting(placed).await;
    let actor = tilo_app_host::collab_app_broker::person_id_for_root(
        workspaces.device_cert().user_public_key,
    );
    let mut workspace = Workspace::new(placed, "Execution isolation");
    workspace.policy.set_owner(actor.clone());
    let workspace_id = workspace.id().to_string();
    let mut tree = WorkspaceTree {
        persist_path: Some(directory.path().join("workspace.json")),
        ..WorkspaceTree::default()
    };
    tree.desktop.workspaces.push(workspace);
    let membership = tree.shared();
    let (_origin, time) = execution_policy_clock().unwrap();
    let admission = {
        let workspaces = Arc::clone(&workspaces);
        let (workspace, actor) = (workspace_id.clone(), actor.clone());
        block_on_owned(async move { workspaces.admit_execution(&workspace, &actor).await }).unwrap()
    };
    // The person's devices record lists this device, as the daemon reads it at its start: the
    // host signs what it admits only as a device that record holds.
    let (_, signing) = tilo_identity::trusted_device::unlisted(&workspace_id).unwrap();
    super::devices_trust().reading(move || {
        Some(tilo_identity::sources::DevicesSeen::of_host(
            signing.user_public_key,
            [signing.device_public_key],
            [],
        ))
    });
    let mut services = membership.services();
    services.native_certificates = Some(Arc::new(FixtureAuthority {
        workspace: uuid::Uuid::parse_str(&workspace_id).unwrap(),
        cert: admission.certificate().clone(),
        time: time.clone(),
    }));
    let (broker, turn) = AppBrokerState::test_broker_with_authority_and_invoke_turn(
        membership.authority(),
        services,
    );
    let qualified = broker
        .certified_local_action_provider(admission.caller(), admission.presentation(), false)
        .unwrap();
    broker.replace_published_surfaces_for_test(vec![PublishedSurface {
        peer_id: 7,
        app_id: "so.tilo.files".into(),
        route: CrdtRoute::app("so.tilo.files", workspace_id.clone()),
        actions: vec![PublishedAction {
            did: None,
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            resource: "files.entry:/tmp/synthetic-report".into(),
            action: "refresh".into(),
            resource_kind: "files.entry".into(),
            resource_label: "Synthetic report".into(),
            action_label: "Refresh".into(),
            keywords: Vec::new(),
            icon: None,
            description: None,
            operation_kind: tilo_actions::OperationKind::Effect,
            params: Vec::new(),
            contract: tilo_actions::ActionContract {
                execution: tilo_actions::ExecutionMode::Background { cancellable: true },
                result: tilo_actions::ResultSpec::Job,
                ..Default::default()
            },
            input: PublishedInput::None,
            view: None,
        }],
        live: true,
    }]);
    let mut invocations = broker.capture_invocations_for_test(7);
    let owner = Caller::local_person(actor.clone())
        .in_workspace(&workspace_id)
        .in_new_execution();
    let provider = RuntimeActionProvider {
        broker,
        workspace: workspace_id.clone(),
        workspaces,
        apps: None,
        session: Some(Arc::new(AdmittedRuntimeSession {
            policy_time: time,
            provider: qualified,
            admission,
            agent: None,
            projection: Some(owner.clone()),
            pressed: None,
            own_shell: false,
        })),
        tickets: None,
        invocations: Arc::default(),
        kept: Arc::default(),
        handed: None,
        asked: None,
    };
    let other = owner.clone().in_new_execution();
    let id = tilo_actions::InvocationId::new();
    let request = InvokeRequest {
        on_behalf_of: None,
        invocation: id,
        target: RemoteActionTarget {
            provider: "7".into(),
            address: tilo_actions::ActionAddress::new(
                "files.entry:/tmp/synthetic-report",
                "refresh",
            ),
        },
        argument: InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    // The person's own shell runs a call they pressed at a point of a window, and the call waits
    // for their yes: its question keeps the window, and where in it they pressed.
    let store = tilo_remote_actions::asked::AskedStore::opened(None, None, Vec::new()).unwrap();
    let asked_here = super::super::asked::DeviceAsked::new(
        Arc::clone(&store),
        Arc::default(),
        Box::new(|_| Err("nothing waits from before".to_owned())),
    );
    let pressed_run = RuntimeActionProvider {
        session: provider.session.as_ref().map(|session| {
            Arc::new(AdmittedRuntimeSession {
                policy_time: session.policy_time.clone(),
                provider: Arc::new(AsksFirst),
                admission: Arc::clone(&session.admission),
                agent: None,
                projection: Some(owner.clone()),
                pressed: Some(runtime_collab::wire::shell::ShellPress {
                    instance: "p1".to_owned(),
                    resource: "files.entry:/tmp/synthetic-report".to_owned(),
                    at: Some(tilo_actions::remote::asked::PressPoint { x: 412, y: 96 }),
                }),
                own_shell: true,
            })
        }),
        asked: Some(asked_here),
        ..provider.clone()
    };
    let pressed_call = InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        ..request.clone()
    };
    assert!(matches!(
        pressed_run.invoke(&owner, pressed_call).poll_ready(),
        PendingPoll::Ready(Ok(_))
    ));
    let pressed: Vec<_> = store
        .waiting_in(&workspace_id)
        .into_iter()
        .map(|waiting| waiting.asked.pressed)
        .collect();
    assert_eq!(
        pressed,
        vec![Some(tilo_actions::remote::asked::Pressed {
            device: hex::encode(provider.workspaces.device_cert().device_public_key),
            app: String::new(),
            instance: "p1".to_owned(),
            resource: "files.entry:/tmp/synthetic-report".to_owned(),
            at: Some(tilo_actions::remote::asked::PressPoint { x: 412, y: 96 }),
        })],
        "the question stands where in the window the person pressed"
    );
    let mut pending = provider.invoke(&owner, request.clone());
    assert!(matches!(pending.poll_ready(), PendingPoll::Waiting));
    turn();
    // Answered once the admission is durable, from the journal's writer.
    let outcome = pending
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten();
    let Some(Ok(receipt)) = outcome else {
        panic!("the synthetic app must accept the invocation: {outcome:?}");
    };
    assert!(invocations.try_recv().is_ok());
    assert!(provider.observe(&owner, &id).is_some());
    assert!(provider.observe(&other, &id).is_none());
    // Waiting on a change is authorized exactly as observing.
    assert!(provider.next_change(&other, &id).is_none());
    let change = provider
        .next_change(&owner, &id)
        .expect("the owner may wait on its invocation");
    // Another execution may not stop it, and is told who may.
    let stopped = provider
        .cancel(&other, &id)
        .wait_timeout(std::time::Duration::from_secs(30))
        .ok()
        .flatten()
        .expect("the provider answers");
    assert!(
        matches!(
            &stopped,
            Err(ActionCallError::Refused { reason })
                if reason.starts_with("only the caller who made this invocation")
        ),
        "{stopped:?}"
    );
    assert!(matches!(
        provider.invoke(&owner, request.clone()).poll_ready(),
        PendingPoll::Ready(Ok(retry)) if retry == receipt
    ));
    assert!(matches!(
        provider.invoke(&other, request.clone()).poll_ready(),
        PendingPoll::Ready(Err(ActionCallError::NotPermitted))
    ));
    assert!(
        invocations.try_recv().is_err(),
        "retries must not dispatch again"
    );
    assert_eq!(
        provider
            .cancel(&owner, &id)
            .wait_timeout(std::time::Duration::from_secs(30))
            .ok()
            .flatten()
            .expect("the provider answers"),
        Ok(())
    );
    // The person's own run, as the daemon admits it for their programs: each program's session
    // calls in an execution of its own, and is the person all the same; another person is not.
    let person_run = RuntimeActionProvider {
        session: provider.session.as_ref().map(|session| {
            Arc::new(AdmittedRuntimeSession {
                policy_time: session.policy_time.clone(),
                provider: Arc::clone(&session.provider),
                admission: Arc::clone(&session.admission),
                agent: None,
                projection: Some(Caller::local_person(actor.clone()).in_workspace(&workspace_id)),
                pressed: None,
                own_shell: false,
            })
        }),
        ..provider.clone()
    };
    let program = Caller::local_person(actor)
        .in_workspace(&workspace_id)
        .in_new_execution();
    let asked = InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        ..request.clone()
    };
    assert!(!matches!(
        person_run.invoke(&program, asked.clone()).poll_ready(),
        PendingPoll::Ready(Err(_))
    ));
    let stranger = Caller::local_person("someone else")
        .in_workspace(&workspace_id)
        .in_new_execution();
    assert!(matches!(
        person_run.invoke(&stranger, asked).poll_ready(),
        PendingPoll::Ready(Err(_))
    ));
    // The cancellation request changed the invocation: the waiter wakes on
    // it, well before the broker's one-second safety interval. The paused
    // clock reaches that interval only once nothing else can run, so the
    // timeout below fires only if the change did not wake the waiter.
    tokio::time::pause();
    assert_eq!(
        tokio::time::timeout(Duration::from_millis(500), change).await,
        Ok(Some(())),
        "a change wakes its waiter"
    );
}

/// In the parent, runs the test at path `test` again in a child process
/// whose profile environment is private, and returns `None`; in that child,
/// returns the profile to serve.
pub(crate) fn isolated_root(test: &str) -> Option<PathBuf> {
    if let Some(root) = std::env::var_os(PROCESS_ROOT) {
        found_here();
        return Some(PathBuf::from(root));
    }
    let temp = tempfile::tempdir().unwrap();
    // Identity storage refuses symlinked paths, and a platform temp dir may
    // sit behind one (macOS `/var`).
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let log = root.join("child.stdout");
    let status = isolated_command(test, &root)
        .stdout(std::fs::File::create(&log).unwrap())
        .status_locked()
        .unwrap();
    let stdout = std::fs::read_to_string(&log).unwrap();
    assert!(status.success(), "{stdout}");
    // libtest exits 0 when `--exact` selects nothing; require the phase ran.
    assert!(
        stdout.contains("test result: ok. 1 passed"),
        "the isolated child ran no test: {stdout}"
    );
    None
}

/// This test binary running only `test`, as a process of its own whose home,
/// identity and profile are under `root`: the profile at `root/profile`,
/// which [`isolated_root`] answers there.
pub(crate) fn isolated_command(test: &str, root: &Path) -> std::process::Command {
    let environment = root.join("environment");
    let (config, data) = (environment.join("config"), environment.join("data"));
    std::fs::create_dir_all(&config).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    // The device this child serves relays through this machine's loopback
    // relay: no test reaches a relay, or a directory, out on the internet.
    let (relay, loopback) = test_server::loopback_relay();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .env(relay, loopback)
        .args(["--exact", test, "--nocapture"])
        .env(PROCESS_ROOT, root.join("profile"))
        .env("HOME", environment.join("home"))
        .env("XDG_CONFIG_HOME", &config)
        .env("XDG_DATA_HOME", &data)
        .env("XDG_CACHE_HOME", environment.join("cache"))
        .env("XDG_STATE_HOME", environment.join("state"))
        .env("TILO_IDENTITY_ROOT", environment.join("identity"))
        .env(tilo_identity::provisioning::FILE_SECRETS_ENV, "1")
        .env_remove("TILO_SVC_COLLAB_FD")
        .env_remove("TILO_SVC_IDENTITY_FD");
    command
}

/// The home of the process [`isolated_command`] runs under `root`.
pub(crate) fn isolated_home(root: &Path) -> PathBuf {
    root.join("environment").join("home")
}

/// Provision only the receipt signer's host ownership in an isolated process.
/// The synthetic caller still uses its independently certified admission.
pub(crate) fn isolated_signing_identity(test: &str) -> Option<PathBuf> {
    isolated_root(test)
}

/// Found a person on this computer, whose root it has, as a computer is set up explicitly
/// before its daemon serves anyone, as a daemon of one account's does at its first start.
///
/// Founding signs as the device, which only the process that took its key does: this one takes
/// the computer's key for the founding, made here when the computer has none yet, and lets go of
/// it after, for the daemon the test starts to take.
pub(crate) fn found_here() {
    as_the_daemon(|| {
        super::found_account_person(
            &tilo_identity::provisioning::collaboration_identity_paths(),
            "",
            rustix::process::geteuid().as_raw(),
        )
        .unwrap()
    });
}

/// This computer's device key, taken as its daemon takes it at a first start: made here when the
/// computer has none yet. What signs as the device, or opens an account's identity, in a test
/// with no daemon running does it while this is held; a daemon the test starts takes the key
/// once this is dropped.
pub(crate) fn taking_the_device_key() -> tilo_device_identity::persist::DeviceKeyLock {
    tilo_identity::provisioning::device_key_taken_as_daemon(
        &tilo_identity::provisioning::collaboration_identity_paths(),
    )
    .unwrap()
}

/// `step`, run as this computer's daemon runs it: holding the device key
/// ([`taking_the_device_key`]), which it lets go of after.
pub(crate) fn as_the_daemon<T>(step: impl FnOnce() -> T) -> T {
    let _taken = taking_the_device_key();
    step()
}

/// An SDK app's collaboration manager once it has joined the broker.
fn connected_manager() -> tilo_ui_base::collaboration::CollaborationManager {
    let mut manager = tilo_ui_base::collaboration::CollaborationManager::new();
    let started = Instant::now();
    while manager.state() == tilo_ui_base::collaboration::CollaborationState::Connecting
        && started.elapsed() < tilo_test_env::hang_bound()
    {
        let _ = manager.poll();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(manager.is_connected(), "{:?}", manager.state());
    manager
}

#[test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the session holds the profile and brokers for the whole scenario on purpose"
)]
fn sdk_apps_join_the_daemon_and_publish_to_mcp() {
    // Helper role: an ordinary SDK app launched by the session publishes its
    // action directory and keeps its connection until it is killed.
    if std::env::var_os(APP_HELPER).is_some() {
        let mut manager = connected_manager();
        manager.publish_action_directory(tilo_ui_base::collaboration::local_app_id());
        loop {
            let _ = manager.poll();
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    let Some(root) = isolated_root(PROCESS_TEST) else {
        return;
    };
    let session = Daemon::start_reporting(&root, &mut |_| {}).unwrap();
    // Just started on a new profile, with nothing written yet: the host acts as
    // the person this computer holds, in the workspace it founded for them.
    let identity = tilo_identity::store::identity_for_workspace(&session.workspace().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(session.actor(), identity.person_id());
    // Durable collaboration history and the invocation journal live in the
    // profile.
    assert!(root.join(COLLABORATION_HISTORY).is_dir());
    assert!(root.join(INVOCATION_JOURNAL).exists());
    assert!(
        Daemon::start_reporting(&root, &mut |_| {}).is_err(),
        "a second daemon must not serve a device one holds"
    );
    let executable = std::env::current_exe().unwrap();
    let mut children = Vec::new();
    for index in 0..2 {
        let log = std::fs::File::create(root.join(format!("helper-{index}.log"))).unwrap();
        let mut command = std::process::Command::new(&executable);
        command
            .args(["--exact", PROCESS_TEST, "--nocapture"])
            .env(APP_HELPER, "1")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log));
        children.push(session.launch("dev.tilo.same", command).unwrap());
    }
    let deadline = tilo_test_env::hang_deadline();
    let roots = loop {
        let roots = session
            .broker
            .app_action_broker()
            .unwrap()
            .state()
            .action_directory_root_page(None, 8)
            .roots;
        if roots.len() == 2 || Instant::now() >= deadline {
            break roots;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    for child in &mut children {
        let _ = child.kill();
        let _ = child.wait();
    }
    let logs = (0..2)
        .map(|index| std::fs::read_to_string(root.join(format!("helper-{index}.log"))))
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .join("\n");
    assert_eq!(roots.len(), 2, "both apps publish: {logs}");
    assert!(roots.iter().all(|root| root.app_id == "dev.tilo.same"));
    assert_ne!(roots[0].peer_id, roots[1].peer_id);
    let founded = session.workspace().expect("the workspace");
    drop(session);
    // The profile is released and served again with the same workspace.
    let reopened = Daemon::start_reporting(&root, &mut |_| {}).unwrap();
    assert_eq!(reopened.workspace().expect("the workspace"), founded);
}

const WRITER_TEST: &str = "device::services::tests::an_sdk_apps_sealed_write_in_the_runtimes_own_workspace_is_durable_across_restart";
const WRITER_HELPER: &str = "TILO_DAEMON_WRITER_HELPER";
/// The person the writer's helper edits the note as: the person this device serves.
const WRITER_PERSON: &str = "TILO_DAEMON_WRITER_PERSON";
const WRITER_APP: &str = "dev.tilo.writer";
const WRITER_KIND: &str = "dev.tilo.writer.note";
const WRITER_RECORD: &str = "0b7d9a3e-5c1f-4a8e-9d2b-7f6e5a4c3b21";
/// The writer's surface, in the canonical form a package ships it: the note it keeps, whose text
/// its plain edits write.
const WRITER_SURFACE: &str = r#"{"resources":[{"fields":[{"path":"text","stored":"value","type":"text"}],"kind":"dev.tilo.writer.note","label":"Note","verbs":[{"id":"type","kind":"undoable","params":[]}],"version":1}]}"#;
const WRITTEN: &str = "written by an SDK app";

/// The text of the writer's note as the store of the workspace host this session
/// runs keeps it in the profile's own workspace. `Err` says where the read stops,
/// so a write that never arrives is explained.
fn written(session: &Daemon) -> Result<Option<String>, String> {
    use automerge::ReadDoc as _;
    let workspace = WorkspaceId::parse(&session.workspace().map_err(|error| error.to_string())?)
        .ok_or_else(|| "the session names no workspace".to_owned())?;
    let record = ui::resource::DocumentId::for_file_document(
        uuid::Uuid::parse_str(WRITER_RECORD).map_err(|error| error.to_string())?,
    );
    let store = session.workspaces.service().workspaces();
    let Some(bytes) = store.stored_record(&workspace.to_string(), WRITER_APP, record.as_str())?
    else {
        return Err("the workspace's store holds nothing of the record".to_owned());
    };
    let doc = automerge::AutoCommit::load(&bytes).map_err(|error| error.to_string())?;
    Ok(doc
        .get(automerge::ROOT, "text")
        .ok()
        .flatten()
        .and_then(|(value, _)| value.into_string().ok()))
}

/// Wait until `manager`'s primary route is joined, failing the moment it
/// cannot be: a manager that went offline, or whose route the workspace
/// refused, never connects, so its reason is the failure rather than the hang
/// bound. One still joining says what holds it up.
pub(crate) fn wait_until_joined(manager: &mut tilo_ui_base::collaboration::CollaborationManager) {
    let started = Instant::now();
    loop {
        let _ = manager.poll();
        if manager.is_connected() {
            return;
        }
        if let Some(refusal) = manager.join_refusal() {
            panic!("the app's route was refused: {refusal}");
        }
        if let tilo_ui_base::collaboration::CollaborationState::Offline(reason) = manager.state() {
            panic!("the app never joined: {reason}");
        }
        assert!(
            started.elapsed() < tilo_test_env::hang_bound(),
            "the app never joined: {:?}, held by {:?}",
            manager.state(),
            manager.join_hold()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Stay in the helper until the test that started it stops it, failing the
/// moment the SDK stops publishing the write that was queued: a manager that
/// went offline keeps nothing more, so its reason is the failure rather than
/// the test waiting out its bound on a write that will never come.
fn watch_for_stop(manager: &mut tilo_ui_base::collaboration::CollaborationManager) -> ! {
    loop {
        let _ = manager.poll();
        if let Some(tilo_ui_base::collaboration::CollaborationState::Offline(reason)) =
            manager.take_state_change()
        {
            panic!("the app stopped publishing its write: {reason}");
        }
        if let Some(refused) = manager.refused_change() {
            panic!("the workspace refused the app's write: {refused}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// How much sooner than the test that started it a helper child gives up: the
/// child's reason is written to the test's log while the test still waits, so
/// a failure reports what the helper waited on instead of the test's own bound.
const HELPER_LEAD: u64 = 10;

/// The hang bound a helper child is started with, in seconds.
pub(crate) fn helper_hang_bound() -> String {
    tilo_test_env::hang_bound()
        .as_secs()
        .saturating_sub(HELPER_LEAD)
        .max(1)
        .to_string()
}

#[test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the session holds the profile and brokers for the whole scenario on purpose"
)]
fn an_sdk_apps_sealed_write_in_the_runtimes_own_workspace_is_durable_across_restart() {
    // Helper role: an ordinary SDK app joins one record of the runtime's
    // workspace and writes it once; the device's store seals and keeps it.
    if std::env::var_os(WRITER_HELPER).is_some() {
        use automerge::transaction::Transactable as _;
        // The app runs with its package's surface, which its writes name.
        collab_types::app_surface::register_package(WRITER_APP, WRITER_SURFACE.as_bytes());
        let checkout = ui::resource::DocumentCheckout::new(
            ui::resource::DocumentId::for_file_document(
                uuid::Uuid::parse_str(WRITER_RECORD).unwrap(),
            ),
            ui::resource::ResourceType::new(WRITER_KIND),
        );
        let mut manager =
            tilo_ui_base::collaboration::CollaborationManager::for_resource(&checkout);
        wait_until_joined(&mut manager);
        let mut doc = automerge::AutoCommit::new();
        doc.put(automerge::ROOT, "text", WRITTEN).unwrap();
        // An edit of the note's content, as the app's window makes it: its surface's `type` verb,
        // made by the person at the window.
        let person = std::env::var(WRITER_PERSON).unwrap();
        let invoker = tilo_actions::ActionInvoker::member(tilo_actions::MemberId::person(person));
        let edit = tilo_actions::ActionContext::default().with_invoker(invoker);
        doc.commit_with(
            automerge::transaction::CommitOptions::default()
                .with_message(edit.change_message().unwrap()),
        );
        assert!(
            manager.send(tilo_ui_base::collaboration::CollabRequest::document_snapshot(doc.save()))
        );
        watch_for_stop(&mut manager);
    }
    let Some(root) = isolated_root(WRITER_TEST) else {
        return;
    };
    // The writer is an app this device installed, whose package's surface declares its note.
    collab_types::app_surface::register_package(WRITER_APP, WRITER_SURFACE.as_bytes());
    let session = Daemon::start_in_dev_mode(&root, &mut |_| {}).unwrap();
    let log = std::fs::File::create(root.join("writer.log")).unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", WRITER_TEST, "--nocapture"])
        .env(WRITER_HELPER, "1")
        .env(WRITER_PERSON, session.person_id())
        .env(tilo_test_env::hang::HANG_BOUND_ENV, helper_hang_bound())
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone().unwrap()))
        .stderr(Stdio::from(log));
    let mut child = session.launch(WRITER_APP, command).unwrap();
    let deadline = tilo_test_env::hang_deadline();
    let mut stopped = "the provider keeps no body of the record".to_owned();
    let stored = loop {
        match written(&session) {
            Ok(Some(stored)) => break Some(stored),
            Ok(None) => stopped = format!("{WRITER_APP} keeps {WRITER_RECORD} without the write"),
            Err(reason) => stopped = reason,
        }
        // A writer that stopped writes nothing more: what it said, a refusal among it, is why.
        if let Ok(Some(status)) = child.try_wait() {
            let said = std::fs::read_to_string(root.join("writer.log")).unwrap_or_default();
            stopped = format!("the writer stopped ({status}) before its write was kept: {said}");
            break None;
        }
        if Instant::now() >= deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let _ = child.kill();
    let _ = child.wait();
    let logs = std::fs::read_to_string(root.join("writer.log")).unwrap_or_default();
    assert_eq!(
        stored.as_deref(),
        Some(WRITTEN),
        "the sealed write reaches the workspace's provider, which reports {stopped}: {logs}"
    );
    drop(session);
    // Served again, the profile's workspace host still keeps the write.
    let reopened = Daemon::start_in_dev_mode(&root, &mut |_| {}).unwrap();
    let again = written(&reopened);
    assert_eq!(
        again.as_ref().ok().and_then(|stored| stored.as_deref()),
        Some(WRITTEN),
        "the workspace provider serves the write again: {again:?}"
    );
}

const OFFLOAD_TEST: &str = "device::services::tests::the_session_hands_its_workspace_service_a_route_that_asks_the_workspaces_devices";

#[test]
fn the_session_hands_its_workspace_service_a_route_that_asks_the_workspaces_devices() {
    let Some(root) = isolated_root(OFFLOAD_TEST) else {
        return;
    };
    // A job no other device of the workspace is there to take.
    let request = |workspace: &str| tilo_actions::offload::OffloadRequest {
        workspace: workspace.to_owned(),
        app_id: "so.tilo.photos".into(),
        address: tilo_actions::ActionAddress::new("photos.library:main", "make-thumbnails"),
        offload: tilo_actions::Offload {
            gpu: false,
            cpu_time_ms: 120_000,
            memory_bytes: 1024 * 1024 * 1024,
            output_bytes: 16 * 1024 * 1024,
        },
        inputs: Vec::new(),
    };
    let invocation = || tilo_actions::remote::InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: tilo_actions::remote::RemoteActionTarget {
            provider: "photos.library".into(),
            address: tilo_actions::ActionAddress::new("photos.library:main", "make-thumbnails"),
        },
        argument: tilo_actions::remote::InvokeArgument::default(),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let run_here = |route: &Arc<dyn tilo_actions::offload::ActionOffload>, workspace: &str| {
        let answer = tilo_rt::app()
            .handle()
            .block_on(route.offload_invocation(request(workspace), invocation()));
        let tilo_actions::offload::Offloaded::RunHere { reason } = answer else {
            panic!("nothing here can run elsewhere: {answer:?}");
        };
        reason
    };

    // The broker holds the route: it asks the workspace's other devices and,
    // with none of them answering, runs the job here and says so.
    let session = Daemon::start_reporting(&root, &mut |_| {}).unwrap();
    let state = Arc::clone(session.broker.app_action_broker().unwrap().state());
    let route = state
        .action_offload()
        .expect("the session asks its workspaces' devices")
        .clone();
    let session_workspace = session.workspace().unwrap();
    // The broker asks the workspace's other devices to take the turn, and with
    // none of them there runs it here.
    assert_eq!(
        run_here(&route, &session_workspace),
        "no other device of the workspace is reachable"
    );
    drop(route);
    drop(state);
    drop(session);

    // Turned off, nothing is routed at all.
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(crate::offload::OFFLOAD_ENV, "off");
    let session = Daemon::start_reporting(&root, &mut |_| {}).unwrap();
    let state = Arc::clone(session.broker.app_action_broker().unwrap().state());
    assert!(state.action_offload().is_none(), "offload is off: no route");
    drop(state);
    drop(session);
}

fn app_grant(workspace: &str, person: &str) -> tilo_action_policy::Grant {
    tilo_action_policy::Grant::new(
        "caller-opens-target",
        tilo_action_policy::Grantee::person(person),
        tilo_action_policy::TreePath::workspace(workspace)
            .app("so.tilo.target-app")
            .any(tilo_action_policy::Level::Kind)
            .resource("target.document")
            .action("open"),
        tilo_action_policy::KindSet::ALL,
        tilo_action_policy::Answer::Allow,
        tilo_action_policy::Grantee::person(person),
    )
}

/// Whether `workspace`'s member list, as `workspaces` holds it, has the grant `id`.
fn granted(workspaces: &ServiceWorkspaces, workspace: WorkspaceId, id: &str) -> bool {
    workspaces.workspace(workspace).is_some_and(|held| {
        held.policy
            .grants
            .iter()
            .any(|grant| grant.id.as_str() == id)
    })
}

/// A grant is a change to the workspace's member list, in force once the list holds it, and
/// this device takes one back the same way when it is spent.
#[test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the workspaces and their service stay up until the test ends"
)]
fn a_grant_is_a_change_to_the_member_list() {
    let placed = WorkspaceId::new();
    let (workspaces, _directory) = tilo_rt::app()
        .block_on(ServiceWorkspaces::test_hosting(placed))
        .unwrap();
    let person = hex::encode(workspaces.device_cert().user_public_key);
    let grants = MemberListGrants {
        workspaces: Arc::clone(&workspaces),
    };
    let workspace = placed.to_string();
    let grant = app_grant(&workspace, &person);
    let id = grant.id.as_str().to_owned();
    tilo_rt::app()
        .block_on(workspaces.copies().commit_own_change(
            &workspace,
            tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::Member {
                change: tilo_action_policy::Change::Grant(grant),
            },
            None,
        ))
        .expect("the runtime runs the change")
        .expect("the owner's grant is committed");
    assert!(
        granted(&workspaces, placed, &id),
        "the grant is in the member list once it is committed"
    );

    assert!(
        grants.spend(&workspace, &id),
        "the first call to spend the grant spends it"
    );
    let deadline = tilo_test_env::hang_deadline();
    while granted(&workspaces, placed, &id) {
        assert!(Instant::now() < deadline, "the grant was never taken back");
        std::thread::sleep(Duration::from_millis(20));
    }
}

tilo_test_env::scenario! {
    fn the_daemons_certificates_hold_while_their_origin_lives;

    phase a_certificate_holds_where_the_member_list_admits_its_device;
    {
        use tilo_app_host::certificate_authority::CertificateAuthority as _;
        let placed = WorkspaceId::new();
        #[allow(
            clippy::significant_drop_tightening,
            reason = "the certificates read the workspaces through a weak reference while they are up"
        )]
        let (workspaces, _directory) = tilo_rt::app()
            .block_on(ServiceWorkspaces::test_hosting(placed))
            .unwrap();
        let origin = tilo_app_host::policy_clock::OwnerPolicyClock::new()
            .unwrap()
            .new_origin();
        let certificates = Certificates {
            policy_time: origin.capture().unwrap(),
            workspaces: Arc::downgrade(&workspaces),
        };
        let workspace = uuid::Uuid::parse_str(&placed.to_string()).unwrap();
        let cert = workspaces.device_cert();
        assert_eq!(certificates.authorize(workspace, &cert), Ok(()));
        assert!(
            certificates
                .authorize(uuid::Uuid::from_u128(8), &cert)
                .is_err(),
            "a workspace this device does not hold admits nobody here"
        );
        // The same device, confined by its person's root to another of their workspaces (a
        // workspace's VM): this workspace refuses it before its member list is asked, though the
        // list names its key.
        let mut confined = cert.clone();
        confined.capabilities = tilo_user_identity::DeviceCapabilities::workspace_device(
            *uuid::Uuid::from_u128(8).as_bytes(),
        );
        assert_eq!(
            certificates.authorize(workspace, &confined),
            Err("this device reaches another workspace alone".to_owned())
        );
        assert!(certificates.authorize_join(workspace, &confined).is_err());
        let mut at_home = cert.clone();
        at_home.capabilities =
            tilo_user_identity::DeviceCapabilities::workspace_device(*workspace.as_bytes());
        assert_eq!(
            certificates.authorize(workspace, &at_home),
            Ok(()),
            "its own workspace admits it"
        );
        let mut stranger = cert;
        stranger.device_public_key = [3; 32];
        assert!(
            certificates.authorize(workspace, &stranger).is_err(),
            "a device the member list does not name is not admitted"
        );
        drop(certificates);
    }

    phase runtime_original_policy_time_cannot_survive_origin_retirement;
    {
        use tilo_app_host::certificate_authority::CertificateAuthority as _;
        let origin = Arc::new(
            tilo_app_host::policy_clock::OwnerPolicyClock::new()
                .unwrap()
                .new_origin(),
        );
        let certificates = Certificates {
            policy_time: origin.capture().unwrap(),
            workspaces: std::sync::Weak::new(),
        };
        let retained = certificates.policy_time().unwrap().unwrap();
        origin.invalidate();
        assert!(certificates.policy_time().is_err());
        drop(certificates);
        assert!(retained.now_ns().is_err());
        let replacement = tilo_app_host::policy_clock::OwnerPolicyClock::new()
            .unwrap()
            .new_origin();
        assert!(!retained.same_origin(&replacement.capture().unwrap()));
        assert!(
            retained.now_ns().is_err(),
            "a new runtime clock cannot revive the original ownership"
        );
    }
}

/// A computer that logged in with a code holds no root: the certificate its login filed is what its
/// apps sign as, made this computer's identity as it logs in, and what its daemon serves as.
#[test]
fn a_logins_device_is_what_this_computers_apps_sign_as() {
    let dir = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(dir.path()).unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("XDG_STATE_HOME", dir.join("state"))
        .set("XDG_CONFIG_HOME", dir.join("config"))
        .set("XDG_DATA_HOME", dir.join("data"))
        .set("TILO_IDENTITY_ROOT", dir.join("identity"))
        .set(tilo_identity::provisioning::FILE_SECRETS_ENV, "1");
    let root = SigningKey::from_bytes(&[41; 32]);
    let key = SigningKey::from_bytes(&[42; 32]);
    let now = tilo_clock::unix_now_ns();
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "Ada's laptop".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: now.saturating_sub(60_000_000_000),
        expires_at_ns: Some(now.saturating_add(3_600_000_000_000)),
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
    let place = tilo_device_identity::login::Place::here(tilo_api::calls::device_keys()).unwrap();
    tilo_device_identity::persist::adopt_device_key(place.keys(), &key.to_bytes()).unwrap();
    // No daemon runs here: this process holds the key it adopted, as the daemon does.
    let _daemon_key = taking_the_device_key();
    tilo_device_identity::login::file_certificate(&place, &cert).unwrap();
    assert!(
        tilo_identity::trusted_device::current("").is_err(),
        "nothing signs before the login makes the certificate this computer's identity"
    );
    tilo_api::daemon::enter::adopt(&place, &cert).unwrap();
    // The person's devices record lists this device, as the daemon reads it from their workspace
    // once it holds it: a device signs only where that record holds it.
    let (person, device) = (cert.user_public_key, cert.device_public_key);
    super::devices_trust().reading(move || {
        Some(tilo_identity::sources::DevicesSeen::of_host(
            person,
            [device],
            [],
        ))
    });

    let identity = Identity::here(&dir).unwrap();
    assert_eq!(identity.cert, cert);
    let (signer, signed_for) = tilo_identity::trusted_device::current("").unwrap();
    assert_eq!(signed_for, cert, "apps sign as the login's device");
    assert_eq!(signer.verifying_key(), key.verifying_key());
    // A second start finds it enrolled and changes nothing.
    assert_eq!(Identity::here(&dir).unwrap().cert, cert);
}

/// A device reached through a forward to a fixed UDP port (a local VM's host forward) binds the
/// port its `tilo.port` credential names, or, without one, the one `TILO_DEVICE_PORT` names; with
/// neither, iroh picks the port as on every other device.
#[test]
fn a_configured_port_is_the_one_the_endpoint_binds() {
    let free = || {
        std::net::UdpSocket::bind("0.0.0.0:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    };
    let (handed, named) = (free(), free());
    let credentials = tempfile::tempdir().unwrap();
    assert_eq!(port_from(Some(credentials.path()), None).unwrap(), None);
    assert_eq!(
        port_from(Some(credentials.path()), Some(named.to_string())).unwrap(),
        Some(named),
        "the environment names the port where no credential is handed"
    );
    std::fs::write(
        credentials.path().join(PORT_CREDENTIAL),
        format!("{handed}\n"),
    )
    .unwrap();
    let port = port_from(Some(credentials.path()), Some(named.to_string())).unwrap();
    assert_eq!(port, Some(handed), "the handed credential names the port");
    assert!(
        port_from(None, Some("seventy".into())).is_err(),
        "a port that is no number is refused, not ignored"
    );

    let key = SigningKey::from_bytes(&[0x37; 32]);
    let endpoint = listen_endpoint(&key, port, None).unwrap();
    let bound: Vec<_> = endpoint
        .bound_sockets()
        .into_iter()
        .filter(std::net::SocketAddr::is_ipv4)
        .collect();
    tilo_rt::app().handle().block_on(endpoint.close());
    assert_eq!(
        bound
            .iter()
            .map(std::net::SocketAddr::port)
            .collect::<Vec<_>>(),
        vec![handed],
        "the endpoint binds the configured port: {bound:?}"
    );
}

const SHELL_TEST: &str = "device::services::tests::the_shell_the_daemon_starts_reaches_its_workspaces_and_no_other_process_of_the_account_does";
/// The role a child of [`SHELL_TEST`] plays: `shell`, started by the daemon as the shell, or
/// `other:<socket>`, any other process of the account, which dials the dispatch's socket itself.
const SHELL_HELPER: &str = "TILO_DAEMON_SHELL_HELPER";
/// Where the shell the daemon starts asks for its link: the daemon's own socket.
const SHELL_NATIVE: &str = "TILO_TEST_SHELL_NATIVE";

/// The daemon's dispatch as `shell` asks it `call`, failing on any refusal.
#[cfg(unix)]
async fn shell_asks(
    shell: &runtime_collab::ShellClient,
    call: runtime_collab::wire::shell::ShellCall,
) -> runtime_collab::wire::shell::ShellReply {
    shell
        .call(call)
        .await
        .unwrap_or_else(|error| panic!("the daemon answers its shell: {error:?}"))
}

/// A read of `app`'s `resource` of `workspace`, whole.
#[cfg(unix)]
fn resource_read(
    workspace: &str,
    app: &str,
    resource: &str,
) -> tilo_actions::remote::ResourceReadRequest {
    tilo_actions::remote::ResourceReadRequest {
        workspace: workspace.to_owned(),
        app: app.to_owned(),
        record: resource.to_owned(),
        joined: false,
        after: None,
        wait_ms: None,
        answer: tilo_actions::remote::ReadAnswer::Document,
    }
}

/// `app`'s `resource` of `workspace` as the shell reads it through the dispatch, whole: `None`
/// while the workspace has none of it.
#[cfg(unix)]
async fn shell_reads(
    shell: &runtime_collab::ShellClient,
    workspace: &str,
    app: &str,
    resource: &str,
) -> Option<Vec<u8>> {
    use runtime_collab::wire::shell::{ShellCall, ShellReply};
    let message = tilo_actions::remote::RemoteActionMessage::ResourceRead {
        correlation: 0,
        on_behalf_of: None,
        read: resource_read(workspace, app, resource),
    };
    let ShellReply::Action(frame) = shell_asks(
        shell,
        ShellCall::Action {
            workspace: None,
            frame: tilo_actions::remote::RemoteActionFrame::new(message)
                .encode()
                .unwrap(),
            caller: None,
            pressed: None,
        },
    )
    .await
    else {
        panic!("the daemon answers the shell's read");
    };
    let tilo_actions::remote::RemoteActionMessage::ResourceReading {
        result: Ok(readings),
        ..
    } = tilo_actions::remote::RemoteActionFrame::decode(&frame)
        .unwrap()
        .message
    else {
        panic!("the daemon answers the shell's read with its readings");
    };
    let readings: serde_json::Value = serde_json::from_str(&readings).unwrap();
    let document = readings["records"][0]["document"].as_str()?;
    Some(hex::decode(document).unwrap())
}

#[cfg(unix)]
#[test]
#[allow(
    clippy::significant_drop_tightening,
    clippy::too_many_lines,
    reason = "the daemon serves its profile for the whole scenario, read as one walk"
)]
fn the_shell_the_daemon_starts_reaches_its_workspaces_and_no_other_process_of_the_account_does() {
    use automerge::ReadDoc as _;
    use runtime_collab::wire::shell::{ShellCall, ShellReply};
    use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_DOCUMENT, DESKTOP_KIND};
    use tilo_peer_state::workspace_documents::DeviceDocuments as _;
    use tilo_workspace_service::device_documents::ServiceDocuments;
    let runtime = || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    };
    if let Some(role) = std::env::var_os(SHELL_HELPER) {
        let role = role.into_string().unwrap();
        runtime().block_on(shell_helper(&role));
        return;
    }
    let Some(root) = isolated_root(SHELL_TEST) else {
        return;
    };
    let session = std::sync::Arc::new(Daemon::start_in_dev_mode(&root, &mut |_| {}).unwrap());
    let socket = session.broker.app_socket().to_path_buf();
    let workspace = session.workspace().unwrap();
    // The shell the daemon starts asks for its link on the daemon's own socket. This daemon
    // serves its services alone, so its endpoint is made here, admitting the seat and answering
    // `shells#link` from the daemon that made the token.
    let native_socket = root.join("native.sock");
    let listener = tilo_files::native::private::bind_socket(&native_socket).unwrap();
    let keys = tilo_device_identity::IdentityPaths::under(root.join("identity"));
    let (key, _) = tilo_device_identity::persist::load_or_create_device_key(&keys).unwrap();
    let native = std::sync::Arc::new(crate::daemon::Native::new(
        crate::device::unclaimed::DeviceOnly::new(
            key,
            tilo_device_identity::login::Place::at(root.join("login"), keys),
        ),
        crate::seats::peer_of,
        tilo_api::daemon::admission::here(crate::seats::own_account),
    ));
    native
        .seats
        .serve(std::sync::Arc::new(ShellsOf(std::sync::Arc::clone(
            &session,
        ))));
    {
        let native = std::sync::Arc::clone(&native);
        std::thread::spawn(move || native.accept(&listener));
    }
    let link = session.shell_link().unwrap();
    runtime().block_on(async {
        link.set_nonblocking(true).unwrap();
        let link = tokio::net::UnixStream::from_std(link).unwrap();
        let (shell, _) = runtime_collab::ShellClient::open(link);
        // The person's workspaces, as the device holds them and as their standing reads.
        let ShellReply::Workspaces(held) = shell_asks(&shell, ShellCall::Workspaces).await else {
            panic!("the daemon lists the workspaces it holds");
        };
        assert!(
            held.contains(&workspace),
            "{workspace} is held here: {held:?}"
        );
        let listed = shell
            .standing()
            .snapshot(tilo_peer_state::workspace_view::LIST)
            .await
            .unwrap();
        let listed: Vec<tilo_peer_state::workspace_view::Listed> =
            serde_json::from_str(&listed.reading.expect("the workspace list reads")).unwrap();
        assert!(
            listed
                .iter()
                .any(|entry| entry.id == workspace && entry.own),
            "the person's own workspace, held here, stands in the list: {listed:?}"
        );
        // A write the shell sends: the daemon writes it into the workspace's store as the device,
        // on the founding every copy of the record derives.
        let person = tilo_actions::Acting::person(tilo_actions::PersonId::from_root(
            session.workspaces.device_cert().user_public_key,
        ));
        let mut layout =
            tilo_desktop_model::record::founded(&workspace).expect("the layout founds");
        automerge::transaction::Transactable::put(&mut layout, automerge::ROOT, "front", "shell")
            .unwrap();
        layout.commit_with(
            automerge::transaction::CommitOptions::default()
                .with_message(tilo_actions::ActionContext::whole_edit_by(&person)),
        );
        tilo_test_env::within("the daemon takes the shell's write", async {
            loop {
                let reply = shell_asks(
                    &shell,
                    ShellCall::Write {
                        workspace: workspace.clone(),
                        app: DESKTOP_APP.to_owned(),
                        kind: Some(DESKTOP_KIND.to_owned()),
                        record: DESKTOP_DOCUMENT.to_owned(),
                        changes: layout.save(),
                    },
                )
                .await;
                match reply {
                    ShellReply::Written(Ok(())) => return,
                    ShellReply::Written(Err(refusal)) if refusal.contains("Waiting") => {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                    other => panic!("the daemon takes the shell's write: {other:?}"),
                }
            }
        })
        .await;
        assert!(
            ServiceDocuments::of(session.workspaces.copies().clone()).holds(
                &workspace,
                DESKTOP_APP,
                DESKTOP_DOCUMENT
            ),
            "the shell's write is the device's own, kept in the workspace's store"
        );
        let bytes = shell_reads(&shell, &workspace, DESKTOP_APP, DESKTOP_DOCUMENT)
            .await
            .expect("the shell reads back the layout it wrote");
        let read = automerge::AutoCommit::load(&bytes).unwrap();
        assert_eq!(
            read.get(automerge::ROOT, "front")
                .unwrap()
                .and_then(|(value, _)| value.into_string().ok())
                .as_deref(),
            Some("shell")
        );
        // The resources of an app the shell runs itself, and the workspace's accounts.
        let counted = shell_asks(
            &shell,
            ShellCall::Resources {
                workspace: workspace.clone(),
                app: WRITER_APP.to_owned(),
                // `collab_types::store::wire::Command::Count`, as the shell sends it.
                command: json!({ "Count": { "kind": WRITER_KIND, "filters": [] } }).to_string(),
            },
        )
        .await;
        assert!(
            matches!(&counted, ShellReply::Resources(Ok(_))),
            "the daemon runs the shell's resource command: {counted:?}"
        );
        let ShellReply::Vault(released) = shell_asks(
            &shell,
            ShellCall::Vault(runtime_collab::wire::shell::VaultCall::Release {
                workspace: workspace.clone(),
                person: [0; 32],
                device: [0; 32],
                app: WRITER_APP.to_owned(),
                item: "vault.item:none".to_owned(),
            }),
        )
        .await
        else {
            panic!("the daemon answers the shell's vault ask");
        };
        assert!(
            released.is_err(),
            "an entry the workspace never had is released to no one: {released:?}"
        );
    });
    // Any other process of the account, which dials the dispatch itself, is refused; the shell
    // the daemon starts is admitted by the link it hands it.
    let other = std::process::Command::new(std::env::current_exe().unwrap());
    let native = root.join(tilo_machine::DAEMON_NATIVE);
    let mut other = shell_child(
        &root,
        &native,
        other,
        &format!("other:{}", socket.display()),
    );
    let status = other.0.spawn_locked().unwrap().wait().unwrap();
    assert!(
        status.success()
            && std::fs::read_to_string(&other.1)
                .unwrap()
                .contains("test result: ok. 1 passed"),
        "another process of the account is not the shell: {}",
        std::fs::read_to_string(&other.1).unwrap_or_default()
    );
    #[cfg(target_os = "linux")]
    {
        let shell = std::process::Command::new(std::env::current_exe().unwrap());
        let (command, log) = shell_child(&root, &native_socket, shell, "shell");
        let status = session.start_shell(command, None).unwrap().wait().unwrap();
        let logs = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            status.success() && logs.contains("test result: ok. 1 passed"),
            "the shell the daemon starts reaches its workspaces: {logs}"
        );
    }
}

/// The secret that admits a shell's link is used once: the connection that names it is answered
/// as the person's shell, and a second connection naming the same secret is refused.
#[cfg(unix)]
#[test]
fn a_shell_links_secret_admits_one_connection() {
    use runtime_collab::wire::shell::{SHELL_MAGIC, ShellCall, ShellReply};
    use tokio::io::AsyncWriteExt as _;
    const TEST: &str = "device::services::tests::a_shell_links_secret_admits_one_connection";
    let Some(root) = isolated_root(TEST) else {
        return;
    };
    let session = Daemon::start_in_dev_mode(&root, &mut |_| {}).unwrap();
    // The person's workspace is founded the first time it is asked for, as their session does.
    crate::RuntimeServices::workspace(&session).unwrap();
    let socket = session.broker.app_socket().to_path_buf();
    let secret = session
        .shell_links()
        .make(tilo_app_host::shell::ShellCaller {
            person: session.workspaces.device_cert().user_public_key,
        });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        for used in 1_u8..=2 {
            let mut stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
            stream.write_all(SHELL_MAGIC).await.unwrap();
            stream.write_all(&secret).await.unwrap();
            let (shell, _) = runtime_collab::ShellClient::open(stream);
            let answer = shell.call(ShellCall::Workspaces).await;
            match used {
                1 => assert!(
                    matches!(
                        &answer,
                        Ok(ShellReply::Workspaces(held)) if !held.is_empty()
                    ),
                    "the link's first connection is its shell: {answer:?}"
                ),
                _ => assert!(
                    answer.is_err(),
                    "a second connection naming the same secret is refused: {answer:?}"
                ),
            }
        }
    });
}

/// What a desktop keeps for its next session is one of the device's records: written with the
/// device's own action on the shell link the daemon handed it, kept by the daemon, and read back
/// by the desktop that follows, on a link of its own. Neither desktop serves the seat: a desktop
/// reads what it kept before it serves anything.
#[cfg(unix)]
#[test]
fn the_desktop_keeps_what_its_next_session_opens_on_as_the_devices_records() {
    use runtime_collab::wire::shell::SHELL_MAGIC;
    use tilo_session_client::daemon_documents::DaemonShell;
    use tilo_session_client::device_records::DeviceRecords;
    use tokio::io::AsyncWriteExt as _;
    const TEST: &str = "device::services::tests::the_desktop_keeps_what_its_next_session_opens_on_as_the_devices_records";
    let Some(root) = isolated_root(TEST) else {
        return;
    };
    let session = Daemon::start_in_dev_mode(&root, &mut |_| {}).unwrap();
    let socket = session.broker.app_socket().to_path_buf();
    let person = session.workspaces.device_cert().user_public_key;
    let workspace = tilo_action_policy::roles::person_workspace_id(&person);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    // A desktop as the daemon starts one: on a shell link its secret admits.
    let desktop = || {
        let secret = session
            .shell_links()
            .make(tilo_app_host::shell::ShellCaller { person });
        let link = runtime.block_on(async {
            let mut stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
            stream.write_all(SHELL_MAGIC).await.unwrap();
            stream.write_all(&secret).await.unwrap();
            runtime_collab::ShellClient::open(stream).0
        });
        DeviceRecords::on(
            DaemonShell::new(Ok(link), runtime.handle().clone()),
            tilo_session_client::device_records::Place::Own(workspace.clone()),
        )
    };

    let first = desktop();
    let shown = first.record("so.tilo.session-shown");
    assert_eq!(
        shown.read().unwrap(),
        None,
        "a device that kept nothing yet reads nothing"
    );
    shown.set(json!({ "active": "front" }));
    shown.set(json!({ "active": "moved" }));
    first
        .record("so.tilo.session-file-paste")
        .set_now(json!({ "version": 1 }))
        .expect("a receipt is kept before the paste goes on");
    assert!(
        first.flush(tilo_test_env::hang_bound()),
        "the daemon takes all the desktop's writes"
    );
    assert_eq!(
        session
            .workspaces
            .device_records()
            .expect("the daemon keeps the device's records")
            .get("so.tilo.session-shown"),
        Some(json!({ "active": "moved" })),
        "the daemon keeps the last of the desktop's writes, in the order it made them"
    );
    drop(first);

    let next = desktop();
    assert_eq!(
        next.record("so.tilo.session-shown").read().unwrap(),
        Some(json!({ "active": "moved" })),
        "the next session opens on what the last one kept"
    );
    assert_eq!(
        next.record("so.tilo.session-file-paste").read().unwrap(),
        Some(json!({ "version": 1 }))
    );

    // A record the session shows while it runs is followed: what another of the person's tools
    // writes there reaches it as it lands, and the session's own writes are not told back to it.
    let (hearing, heard) = std::sync::mpsc::channel();
    let wallpapers = next
        .record("so.tilo.session-wallpapers")
        .follow(move |value| {
            let _ = hearing.send(value);
        })
        .expect("the record is followed");
    wallpapers.set(json!({ "schema": 1, "this": "ours" }));
    assert!(
        next.flush(tilo_test_env::hang_bound()),
        "the daemon takes all the session's writes"
    );
    let elsewhere = desktop();
    elsewhere
        .record("so.tilo.session-wallpapers")
        .set_now(json!({ "schema": 1, "this": "Settings'" }))
        .expect("another tool writes the record");
    assert_eq!(
        heard
            .recv_timeout(std::time::Duration::from_secs(20))
            .expect("the change written elsewhere reaches the follower"),
        Some(json!({ "schema": 1, "this": "Settings'" })),
        "the follower hears the change written elsewhere, and not its own write first"
    );
    assert_eq!(
        wallpapers.read().unwrap(),
        Some(json!({ "schema": 1, "this": "Settings'" })),
        "a followed record reads as last heard"
    );
}

/// What a desktop says of its device (its screens, what it is, how its session stands) is kept in
/// the record of the person's devices by the device's daemon: the desktop runs the record's own
/// action on the shell link the daemon handed it, and reads the fact back from the record, where
/// the person's other devices see it.
#[cfg(unix)]
#[test]
fn the_desktop_keeps_what_its_device_says_of_itself_in_the_devices_record() {
    use runtime_collab::wire::shell::SHELL_MAGIC;
    use tilo_session_client::daemon_documents::{DaemonShell, document_read};
    use tilo_workspace_service::workspaces::person_workspace::devices::{DEVICES, Devices};
    use tokio::io::AsyncWriteExt as _;
    const TEST: &str = "device::services::tests::the_desktop_keeps_what_its_device_says_of_itself_in_the_devices_record";
    let Some(root) = isolated_root(TEST) else {
        return;
    };
    let session = Daemon::start_in_dev_mode(&root, &mut |_| {}).unwrap();
    let socket = session.broker.app_socket().to_path_buf();
    let cert = session.workspaces.device_cert();
    let workspace = tilo_action_policy::roles::person_workspace_id(&cert.user_public_key);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    // The person's workspace, as a device keeps it once it was set up.
    assert_eq!(
        runtime
            .block_on(tilo_workspace_service::workspaces::person_workspace::found(
                session.workspaces.copies()
            ))
            .unwrap(),
        workspace
    );
    let secret = session
        .shell_links()
        .make(tilo_app_host::shell::ShellCaller {
            person: cert.user_public_key,
        });
    let link = runtime.block_on(async {
        let mut stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
        stream.write_all(SHELL_MAGIC).await.unwrap();
        stream.write_all(&secret).await.unwrap();
        runtime_collab::ShellClient::open(stream).0
    });
    let desktop = DaemonShell::new(Ok(link), runtime.handle().clone());

    let key = hex::encode(cert.device_public_key);
    let said = hex::encode(b"two screens");
    let keep = Devices::KEEP_FACT.with((
        key.clone(),
        hex::encode(postcard::to_allocvec(&cert).unwrap()),
        "displays".to_owned(),
        said.clone(),
    ));
    desktop
        .invoke_blocking(
            &workspace,
            tilo_actions::remote::InvokeRequest {
                invocation: tilo_actions::InvocationId::new(),
                target: RemoteActionTarget {
                    provider: format!("record:{}:{}", DEVICES.0, DEVICES.1),
                    address: keep.address(),
                },
                argument: InvokeArgument::Json(keep.arguments),
                context: None,
                confirmation: None,
                revision: None,
                on_behalf_of: None,
                wait_ms: None,
                appearance: None,
                signed: None,
            },
        )
        .expect("the daemon keeps what the device says of itself");

    let readings = desktop
        .read_blocking(resource_read(&workspace, DEVICES.0, DEVICES.1))
        .expect("the desktop reads the record of the person's devices");
    let Ok(tilo_peer_state::workspace_documents::DocumentRead::Whole { bytes, .. }) =
        document_read(&readings)
    else {
        panic!("the record of the person's devices is read whole: {readings}");
    };
    let doc = automerge::AutoCommit::load(&bytes).unwrap();
    let reader = tilo_resource::Reader::<Devices>::read(&doc).unwrap();
    let device = reader
        .state()
        .devices
        .get(&tilo_resource::Id::from_raw(key))
        .expect("the record lists the device");
    assert_eq!(
        device.facts.get("displays"),
        Some(&said),
        "the record keeps the fact the device said"
    );
}

/// The daemon's shells as this test's native endpoint serves them: the daemon makes the token
/// and the link; the endpoint admits the connection and answers `shells#link`.
struct ShellsOf(std::sync::Arc<Daemon>);

#[cfg(unix)]
impl crate::seats::Shells for ShellsOf {
    fn token(&self, _: bool) -> std::io::Result<[u8; 32]> {
        Err(std::io::Error::other("a test starts no shell at a sign-in"))
    }

    fn token_holder(&self, token: &[u8; 32]) -> Option<[u8; 32]> {
        self.0.shell_token_holder(token)
    }

    fn redeem(
        &self,
        secret: [u8; 32],
        seat: u64,
    ) -> std::io::Result<std::os::unix::net::UnixStream> {
        self.0.shell_redeem(secret, seat)
    }

    fn link_for(
        &self,
        person: [u8; 32],
        seat: u64,
    ) -> std::io::Result<std::os::unix::net::UnixStream> {
        let secret = self.0.shell_token_for(person, seat)?;
        self.0.shell_link_of(secret)
    }

    fn live(&self, seat: u64) -> bool {
        self.0.shell_live(seat)
    }

    fn serves(&self, person: [u8; 32]) -> bool {
        self.0.serves(person)
    }

    fn hand_processes(
        &self,
        uid: u32,
        secret: &[u8; 32],
        packets: std::os::fd::OwnedFd,
    ) -> Result<(), String> {
        self.0.hand_processes(uid, secret, packets)
    }

    fn start(
        &self,
        _: Vec<std::ffi::OsString>,
        _: Vec<(std::ffi::OsString, std::ffi::OsString)>,
        _: Option<std::os::fd::OwnedFd>,
    ) -> std::io::Result<std::process::Child> {
        Err(std::io::Error::other("a test's endpoint starts no shell"))
    }
}

/// `command` running [`SHELL_TEST`] as the child `role`, and the file its output goes to.
fn shell_child(
    root: &Path,
    native: &Path,
    mut command: std::process::Command,
    role: &str,
) -> (std::process::Command, PathBuf) {
    let log = root.join(format!(
        "shell-{}.log",
        role.split(':').next().unwrap_or(role)
    ));
    command
        .args(["--exact", SHELL_TEST, "--nocapture"])
        .env(SHELL_HELPER, role)
        .env(SHELL_NATIVE, native)
        .env(tilo_test_env::hang::HANG_BOUND_ENV, helper_hang_bound())
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::fs::File::create(&log).unwrap()))
        // A helper that fails says why where the test that started it reads it.
        .stderr(Stdio::from(
            std::fs::OpenOptions::new().append(true).open(&log).unwrap(),
        ));
    (command, log)
}

/// What the child of [`SHELL_TEST`] playing `role` checks.
#[cfg(unix)]
async fn shell_helper(role: &str) {
    if role == "shell" {
        the_started_shell_is_answered().await;
    } else {
        another_roles_asks_are_refused(role).await;
    }
}

/// The shell the daemon started redeems the one-use token it was given, and the daemon answers
/// it on the link that token admits.
#[cfg(unix)]
async fn the_started_shell_is_answered() {
    use runtime_collab::wire::shell::{ShellCall, ShellReply};
    use std::io::Write as _;
    let token = std::env::var(runtime_collab::wire::shell::SHELL_TOKEN_ENV)
        .expect("the daemon gave its shell a token");
    let socket = std::env::var(SHELL_NATIVE).expect("the daemon's own socket");
    let stream = std::os::unix::net::UnixStream::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(20)))
        .unwrap();
    writeln!(
        &stream,
        "{}",
        serde_json::json!({"native": "shells#link", "token": token})
    )
    .unwrap();
    let (framed, handed) = runtime_portal::fd_transport::recv_with_fd(&stream).unwrap();
    let answer: serde_json::Value = serde_json::from_slice(&framed).unwrap();
    assert!(answer["error"].is_null(), "{answer}");
    let link = std::os::unix::net::UnixStream::from(
        handed.expect("the daemon hands its shell's token a link"),
    );
    link.set_nonblocking(true).unwrap();
    let (shell, _) =
        runtime_collab::ShellClient::open(tokio::net::UnixStream::from_std(link).unwrap());
    let answer = shell.call(ShellCall::Workspaces).await;
    assert!(
        matches!(&answer, Ok(ShellReply::Workspaces(held)) if !held.is_empty()),
        "the shell the daemon started is answered: {answer:?}"
    );
}

/// A process of the account the daemon did not start as the shell is refused every ask, on a
/// link it opens with a secret the daemon never made.
#[cfg(unix)]
async fn another_roles_asks_are_refused(role: &str) {
    use runtime_collab::wire::shell::{SHELL_MAGIC, SHELL_SECRET_BYTES, ShellCall};
    use tokio::io::AsyncWriteExt as _;
    let socket = role.strip_prefix("other:").expect("a role this test plays");
    let workspace = WorkspaceId::new().to_string();
    let membership = tilo_actions::remote::InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: tilo_actions::remote::RemoteActionTarget::workspace_governance(
            &workspace,
            "membership",
        ),
        argument: InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let asks = [
        ShellCall::Action {
            workspace: None,
            frame: tilo_actions::remote::RemoteActionFrame::new(
                tilo_actions::remote::RemoteActionMessage::ResourceRead {
                    correlation: 0,
                    on_behalf_of: None,
                    read: resource_read(&workspace, "so.tilo.desktop", ""),
                },
            )
            .encode()
            .unwrap(),
            caller: None,
            pressed: None,
        },
        ShellCall::Write {
            workspace: workspace.clone(),
            app: "so.tilo.desktop".to_owned(),
            kind: None,
            record: String::new(),
            changes: automerge::AutoCommit::new().save(),
        },
        ShellCall::Action {
            workspace: None,
            frame: tilo_actions::remote::RemoteActionFrame::new(
                tilo_actions::remote::RemoteActionMessage::Invoke(membership),
            )
            .encode()
            .unwrap(),
            caller: None,
            pressed: None,
        },
        // Nor does it serve a provider on the device's dispatch.
        ShellCall::Provide {
            provider: "session.asks".to_owned(),
            catalogue: "{}".to_owned(),
        },
    ];
    for ask in asks {
        // As a shell link opens, with a secret the daemon never made.
        let mut stream = tokio::net::UnixStream::connect(socket).await.unwrap();
        stream.write_all(SHELL_MAGIC).await.unwrap();
        stream.write_all(&[7; SHELL_SECRET_BYTES]).await.unwrap();
        let (shell, _) = runtime_collab::ShellClient::open(stream);
        let answer = shell.call(ask.clone()).await;
        assert!(
            answer.is_err(),
            "a process the daemon did not start as the shell is refused {ask:?}, not answered {answer:?}"
        );
    }
}

/// The identity steps Settings asks of the daemon: listing answers no secret and reads back as
/// Settings reads it, and every change waits for the person's confirmation, which never removes
/// the last identity.
#[test]
fn identity_steps_list_change_and_protect_the_store_without_answering_a_secret() {
    use tilo_device_identity::login::{
        IDENTITIES, PROTECT_IDENTITY, REMOVE_IDENTITY, USE_IDENTITY,
    };
    let profile = tempfile::tempdir().unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("XDG_CONFIG_HOME", profile.path())
        .set(tilo_identity::store::FILE_SECRETS_ENV, "1");
    // No daemon runs here: this process opens the account's identity holding the key, as the
    // daemon does.
    let _daemon_key = taking_the_device_key();
    let dir = super::this_accounts_root_dir().unwrap();
    tilo_identity::provisioning::create_person(
        &dir,
        // This computer's one device key, which seals the root it keeps for an account.
        &tilo_identity::provisioning::collaboration_identity_paths(),
        "Ada",
        "",
        tilo_identity::provisioning::IdentityStorage::Files,
    )
    .unwrap();

    let listed = identity_step(IDENTITIES, &json!({})).expect("listed");
    let entries = listed.as_array().expect("a list");
    assert_eq!(entries.len(), 1, "{listed}");
    assert_eq!(entries[0]["active"], true);
    let summary: tilo_identity_client::IdentitySummary =
        serde_json::from_value(entries[0].clone()).expect("Settings reads the answer back");
    let text = listed.to_string();
    assert!(
        !text.contains("secret") && !text.contains("resource_path"),
        "no secret or record path leaves the daemon: {text}"
    );

    let id = summary.id.as_str();
    for step in [USE_IDENTITY, PROTECT_IDENTITY, REMOVE_IDENTITY] {
        let unconfirmed = identity_step(step, &json!({ "id": id, "confirm": false }));
        assert!(
            unconfirmed
                .as_ref()
                .is_err_and(|error| error.to_string().contains("confirmation")),
            "{step} waits for the person's confirmation: {unconfirmed:?}"
        );
    }
    assert_eq!(
        identity_step(USE_IDENTITY, &json!({ "id": id, "confirm": true })).expect("used"),
        json!({ "id": id, "active": true })
    );
    let protected =
        identity_step(PROTECT_IDENTITY, &json!({ "id": id, "confirm": true })).expect("protected");
    assert_eq!(protected["id"], id);
    assert!(
        identity_step(REMOVE_IDENTITY, &json!({ "id": id, "confirm": true })).is_err(),
        "the active, final identity stays"
    );
    assert_eq!(
        tilo_identity::store::list_identities(&dir).unwrap().len(),
        1
    );
}

const CONNECTION_PERSON_TEST: &str =
    "device::services::tests::a_plain_connection_is_the_person_the_daemon_serves";

/// A connection from the person's own account that names no app is the person, admitted from who
/// is on the other end of the daemon's own socket: it acts as them, holds no agent of its own,
/// and may govern their workspace. Nothing the connection says makes it an agent: one that only
/// claims to be an agent of the person's, on their device's key, is named by no session and no
/// roster and is offered nothing.
#[test]
fn a_plain_connection_is_the_person_the_daemon_serves() {
    use tilo_action_policy::{Answer, Context, MemberAccess};

    let Some(root) = isolated_root(CONNECTION_PERSON_TEST) else {
        return;
    };
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());
    let workspace = daemon.workspace().unwrap();
    let (_, caller) = daemon.connection_tree_in(None).unwrap();
    let cert = daemon.workspaces.device_cert();
    let person = hex::encode(cert.user_public_key);
    assert_eq!(
        caller.acting().person_id(),
        Some(person.as_str()),
        "the person whose account is on the other end: {caller:?}"
    );
    assert_eq!(
        caller.acting_for_person(),
        None,
        "the person acts for nobody but themselves: {caller:?}"
    );
    assert_eq!(
        caller.acting().agent_id(),
        None,
        "a plain connection is no agent: {caller:?}"
    );
    assert!(
        caller.certified(&workspace).is_empty(),
        "a plain connection needs no certificate: {:?}",
        caller.certified(&workspace)
    );
    assert!(
        caller.is_own_connection(),
        "the person's own connection answers what the workspace asks of them: {caller:?}"
    );

    // Only a connection the daemon admitted as the person gets this far: a caller that only
    // claims to be an agent for the person, on the person's device key, is named by no session
    // and no roster, and the certificate's delegation is offered it nothing.
    let claimed = Caller::delegated_agent("scout", cert.user_public_key, cert.device_public_key)
        .in_workspace(&workspace);
    assert!(
        claimed.certified(&workspace).is_empty(),
        "a claim to be an agent is offered nothing: {:?}",
        claimed.certified(&workspace)
    );

    let policy = daemon
        .workspaces
        .policy_of(WorkspaceId::parse(&workspace).unwrap())()
    .expect("the daemon's workspace has a policy");
    let rename = tilo_action_policy::roles::governance_action(
        &workspace,
        tilo_action_policy::roles::governance::RENAME,
    );
    let kind = tilo_action_policy::roles::GOVERNANCE_KIND;
    let cx = Context::at(0);
    assert_eq!(
        MemberAccess::person(policy, person.as_str()).answer(&rename, kind, &cx),
        Answer::Allow,
        "the person the account is may rename their workspace"
    );
}

const CONFINED_CONNECTION_TEST: &str =
    "device::services::tests::a_confined_daemons_connection_is_no_person";

/// A device the person's root confined to one record (a workspace's VM, a hosted browser's
/// environment) is that record's tenant, never the person's own connection: a connection from
/// the same account, which the kernel says is theirs, is no more their own act there than a
/// program is, so what a workspace asks of them is put to them rather than answered by the call.
#[test]
fn a_confined_daemons_connection_is_no_person() {
    let Some(root) = isolated_root(CONFINED_CONNECTION_TEST) else {
        return;
    };
    // The identity this account founded, whose root signs the confined certificate, and the
    // device key this computer holds, taken as its daemon takes it until the daemon starts.
    let daemon_key = taking_the_device_key();
    let dir = super::this_accounts_root_dir().unwrap();
    let record = tilo_identity::store::resolve_active_identity(&dir).unwrap();
    let identity = tilo_identity::store::load_from_path(record).unwrap();
    let keys = tilo_api::calls::device_keys();
    let key = daemon_key.key().clone();
    let now = tilo_clock::unix_now_ns();
    let mut confined = tilo_user_identity::DeviceCert {
        user_public_key: identity.group_public_key(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "Confined".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::workspace_device(
            *uuid::Uuid::new_v4().as_bytes(),
        ),
        issued_at_ns: now.saturating_sub(60_000_000_000),
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut confined, &identity).unwrap();
    let place = tilo_device_identity::login::Place::here(keys).unwrap();
    tilo_device_identity::login::file_certificate(&place, &confined).unwrap();
    tilo_api::daemon::enter::adopt(&place, &confined).unwrap();
    drop(daemon_key);

    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());
    assert_eq!(
        daemon.cert.capabilities.workspace, confined.capabilities.workspace,
        "this daemon is its record's tenant"
    );
    let (_, caller) = daemon.connection_tree_in(None).unwrap();
    assert!(
        !caller.is_own_connection(),
        "the same account's connection is no person's own act here: {caller:?}"
    );
    assert_eq!(
        caller.acting().person_id(),
        Some(hex::encode(confined.user_public_key).as_str()),
        "it still acts for the person it is the tenant of"
    );
}

const AGENT_TICKET_TEST: &str =
    "device::services::tests::an_agent_asking_for_a_ticket_is_asked_of_its_person_and_gets_none";

/// The name the device's agent goes by in the ticket tests: an agent of the person's, as a
/// computer of theirs makes one for the runs it serves.
const AGENT_TICKET_AGENT: &str = "scout";

/// The device's own agent `name` in `workspace`, as the daemon makes one for a run it serves: the
/// provider its calls go through, and the caller its admission is — a real agent, admitted by this
/// device rather than one a test spells out.
fn the_devices_agent(
    daemon: &Arc<Daemon>,
    workspace: &str,
    name: &str,
) -> (RuntimeActionProvider, Caller) {
    let time = daemon
        .policy_origin
        .capture()
        .expect("the device's policy clock");
    // The second of the run is the admission's caller, the person: the agent's own caller is
    // what the run's calls go as.
    let (run, _) = daemon
        .provider_for(workspace)
        .expect("the device serves its own workspace")
        .agent_session(workspace, &daemon.actor, &time, |person| {
            (
                tilo_actions::remote::device_agent_id(person, name),
                name.to_owned(),
            )
        })
        .map(|(run, caller, _)| (run, caller))
        .expect("the device makes its agent a key");
    let caller = run
        .session
        .as_ref()
        .expect("the agent's run was admitted")
        .acting()
        .clone();
    (run, caller)
}

/// Adding a computer is the person's own act: an agent of theirs that asks for a ticket is
/// answered none, and the call waits as their asking card, showing the agent that asks. Waiting
/// for the ticket served last to be taken answers which computer took it, and issues nothing.
#[test]
fn an_agent_asking_for_a_ticket_is_asked_of_its_person_and_gets_none() {
    use tilo_actions::InvocationUpdate;
    use tilo_actions::remote::asked::AskedBy;
    use tilo_actions::remote::{ActionProvider as _, ReceiptStatus};

    let Some(root) = isolated_root(AGENT_TICKET_TEST) else {
        return;
    };
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());
    let workspace = daemon.workspace().unwrap();
    let (provider, caller) = the_devices_agent(&daemon, &workspace, AGENT_TICKET_AGENT);
    assert!(
        caller.acting().is_agent(),
        "the device's agent asks: {caller:?}"
    );
    let person_key = daemon.workspaces.device_cert().user_public_key;
    let issue = |wait: bool| {
        let mut argument = serde_json::Map::new();
        argument.insert("server".into(), true.into());
        if wait {
            argument.insert("wait".into(), true.into());
        }
        tilo_actions::remote::InvokeRequest {
            on_behalf_of: None,
            invocation: tilo_actions::InvocationId::new(),
            target: RemoteActionTarget {
                provider: tilo_device_identity::login::PROVIDER.to_owned(),
                address: tilo_actions::ActionAddress::new(
                    tilo_device_identity::login::RESOURCE,
                    tilo_device_identity::login::ISSUE,
                ),
            },
            argument: InvokeArgument::Json(argument),
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        }
    };

    let request = issue(false);
    let invocation = request.invocation;
    let receipt = provider
        .invoke(&caller, request)
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten()
        .expect("the daemon answers the call")
        .expect("the call is admitted");
    assert!(
        !matches!(receipt.status, ReceiptStatus::Completed { .. }),
        "the person is answered no ticket itself: {:?}",
        receipt.status
    );
    // It waits as the person's asked record, on a device that keeps their root.
    let asks = tilo_rt::app()
        .handle()
        .block_on(daemon.workspaces.copies().asked_kept());
    let ask = asks
        .iter()
        .find(|ask| ask.request.invocation == invocation)
        .expect("the call waits for the person");
    assert_eq!(ask.person, hex::encode(person_key));
    assert_eq!(
        ask.dispatch,
        tilo_actions::remote::asked::AT_ROOT,
        "the root of a device of theirs answers it"
    );
    assert!(
        matches!(&ask.facts.by, AskedBy::Agent(named) if named.name == AGENT_TICKET_AGENT),
        "an agent's call says which agent asks: {:?}",
        ask.facts.by
    );

    // Waiting for the ticket served last to be taken is no ticket of its own: it is answered by
    // the ticket server, which serves none, and never handed on to the person.
    let waiting = issue(true);
    let invocation = waiting.invocation;
    provider
        .invoke(&caller, waiting)
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten()
        .expect("the daemon answers the call")
        .expect("the call is admitted");
    let until = Instant::now() + Duration::from_secs(30);
    let ended = loop {
        if let Some(state) = crate::device::ticket::Tickets::observe(&daemon.tickets, &invocation)
            && state.update.is_terminal()
        {
            break state.update;
        }
        assert!(Instant::now() < until, "the call ends");
        std::thread::sleep(Duration::from_millis(25));
    };
    let InvocationUpdate::Failed { error, .. } = ended else {
        panic!("no ticket is waiting, which the call says: {ended:?}");
    };
    assert!(
        error.message.contains("No ticket is waiting"),
        "{:?}",
        error.message
    );
}

const AGENT_TICKET_ANSWER_TEST: &str = "device::services::tests::a_ticket_an_agent_asks_for_waits_for_its_person_and_only_a_yes_issues_it";

/// A ticket an agent of the person's asks for waits as their asking card on the device that
/// keeps their root: only their yes issues it, a no ends it with none, and stopping it withdraws
/// the question.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one agent's ticket, from the ask to the person's yes and no, read top to bottom"
)]
fn a_ticket_an_agent_asks_for_waits_for_its_person_and_only_a_yes_issues_it() {
    use tilo_actions::InvocationUpdate;
    use tilo_actions::remote::asked::{AskedRecords as _, Offer, Resolve};
    use tilo_actions::remote::{ActionProvider as _, Caller, ReceiptStatus};

    let Some(root) = isolated_root(AGENT_TICKET_ANSWER_TEST) else {
        return;
    };
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());
    let workspace = daemon.workspace().unwrap();
    let (provider, caller) = the_devices_agent(&daemon, &workspace, AGENT_TICKET_AGENT);
    assert!(
        caller.acting().is_agent(),
        "the device's agent asks: {caller:?}"
    );
    let cert = daemon.workspaces.device_cert();
    let person = Caller::authenticated(cert.user_public_key, cert.device_public_key);
    let person_key = cert.user_public_key;
    let issue = || {
        let mut argument = serde_json::Map::new();
        argument.insert("server".into(), true.into());
        tilo_actions::remote::InvokeRequest {
            on_behalf_of: None,
            invocation: tilo_actions::InvocationId::new(),
            target: RemoteActionTarget {
                provider: tilo_device_identity::login::PROVIDER.to_owned(),
                address: tilo_actions::ActionAddress::new(
                    tilo_device_identity::login::RESOURCE,
                    tilo_device_identity::login::ISSUE,
                ),
            },
            argument: InvokeArgument::Json(argument),
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        }
    };
    let store = super::asked_store(&daemon.workspaces).expect("the device keeps asked records");
    // The device keeps the person's root: it marks itself in their devices record, as a start
    // does, and answers what waits for that root from here.
    let kept_root = super::root()
        .expect("the device reads its root")
        .expect("it keeps one");
    let tickets = (*daemon.tickets).clone();
    tilo_rt::app().handle().block_on(async {
        tilo_workspace_service::workspaces::person_workspace::devices::mark_root_holder(
            daemon.workspaces.copies(),
            &kept_root,
            &cert,
        )
        .await
        .expect("the root marks the device that keeps it");
        daemon.asked.serve_at_root(tickets);
    });
    let requested = |invocation: tilo_actions::InvocationId| {
        store
            .asked_of(&person_key)
            .iter()
            .any(|ask| ask.record.record == invocation.0.to_string())
    };
    let wait_until = |what: &str, mut ready: Box<dyn FnMut() -> bool>| {
        let until = Instant::now() + Duration::from_secs(60);
        while !ready() {
            assert!(
                Instant::now() < until,
                "{what} (the device serves at root: {})",
                daemon.asked.serves_at_root()
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    };
    let ended = |invocation: tilo_actions::InvocationId| {
        daemon
            .asked
            .observe(&caller, &invocation)
            .map(|state| state.update)
            .filter(InvocationUpdate::is_terminal)
    };
    let ask = |request: InvokeRequest| {
        provider
            .invoke(&caller, request)
            .wait_timeout(tilo_test_env::hang_bound())
            .ok()
            .flatten()
            .expect("the daemon answers the call")
            .expect("the call is admitted")
    };

    // The agent asks, and is answered no ticket: the call waits as its person's to answer.
    let request = issue();
    let invocation = request.invocation;
    let receipt = ask(request);
    assert!(
        !matches!(receipt.status, ReceiptStatus::Completed { .. }),
        "the person is answered no ticket itself: {:?}",
        receipt.status
    );
    wait_until("the person is asked it", Box::new(|| requested(invocation)));

    // Their yes issues the ticket, as their own act, and it reaches the call that asked.
    let at = store
        .record(&invocation.0.to_string())
        .expect("the record waits for the person");
    tilo_rt::app()
        .handle()
        .block_on(store.resolve(
            &at,
            &person,
            &Resolve {
                resolution: Offer::Approve,
                ..Resolve::default()
            },
        ))
        .expect("the answer is decided at once")
        .expect("the person's yes is taken");
    wait_until(
        "the ticket comes back",
        Box::new(|| ended(invocation).is_some()),
    );
    let Some(InvocationUpdate::Succeeded { output, .. }) = ended(invocation) else {
        panic!("the yes issues a ticket: {:?}", ended(invocation));
    };
    let value = output.value.expect("the ticket is the answer");
    let text = value["ticket"].as_str().expect("the answer is a ticket");
    tilo_device_identity::login::Ticket::parse(text).expect("the issued ticket is a ticket");

    // A no issues nothing.
    let request = issue();
    let invocation = request.invocation;
    ask(request);
    wait_until(
        "the person is asked it too",
        Box::new(|| requested(invocation)),
    );
    let at = store
        .record(&invocation.0.to_string())
        .expect("the record waits for the person");
    tilo_rt::app()
        .handle()
        .block_on(store.resolve(
            &at,
            &person,
            &Resolve {
                resolution: Offer::Decline,
                ..Resolve::default()
            },
        ))
        .expect("the answer is decided at once")
        .expect("the person's no is taken");
    wait_until(
        "the no ends the call",
        Box::new(|| ended(invocation).is_some()),
    );
    assert!(
        matches!(ended(invocation), Some(InvocationUpdate::Cancelled { .. })),
        "a no ends the call with no ticket: {:?}",
        ended(invocation)
    );

    // Stopping a call that waits withdraws the question: nothing runs, and no ticket is issued.
    let request = issue();
    let invocation = request.invocation;
    ask(request);
    wait_until(
        "the person is asked it once more",
        Box::new(|| requested(invocation)),
    );
    daemon
        .asked
        .cancel(&caller, &invocation)
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten()
        .expect("the call is taken")
        .expect("a call that waits is stopped");
    assert!(
        matches!(ended(invocation), Some(InvocationUpdate::Cancelled { .. })),
        "a withdrawn call ends cancelled: {:?}",
        ended(invocation)
    );
}

const FOUND_ON_A_PERSONS_TEST: &str =
    "device::services::tests::founding_on_a_persons_device_is_told_it_has_one";

/// A device that serves its person, asked to found one (a dev session starting again, `tilo login
/// --new-identity` on a set-up computer), answers that it is a person's already: the refusal its
/// callers read to carry on, never an unknown action.
#[test]
fn founding_on_a_persons_device_is_told_it_has_one() {
    use tilo_device_identity::login::{ALREADY_A_PERSONS, FOUND, PROVIDER, RESOURCE, RESTORE};

    let Some(root) = isolated_root(FOUND_ON_A_PERSONS_TEST) else {
        return;
    };
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());
    let (tree, agent) = daemon.connection_tree_in(None).unwrap();
    for action in [FOUND, RESTORE] {
        let request = InvokeRequest {
            on_behalf_of: None,
            invocation: tilo_actions::InvocationId::new(),
            target: RemoteActionTarget {
                provider: PROVIDER.into(),
                address: tilo_actions::ActionAddress::new(RESOURCE, action),
            },
            argument: InvokeArgument::Json(serde_json::Map::new()),
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let answered = tree
            .invoke(&agent, request)
            .wait_timeout(tilo_test_env::hang_bound())
            .ok()
            .flatten();
        assert!(
            matches!(&answered, Some(Ok(_))) && format!("{answered:?}").contains(ALREADY_A_PERSONS),
            "{action} on a person's device is told it has one: {answered:?}"
        );
    }
}

/// The owner's root a deployment hands the daemon is kept here as on any of its
/// person's own devices: a root record under it and a certificate for this device's
/// key that root signed, so the device acts as the owner and founds as the root
/// holder. A root already kept is not made again, another person's stops the start,
/// and so does a file that is not an owner-only hex seed.
#[test]
fn a_deployment_is_handed_its_owner_root_once_and_kept_as_that_persons_device() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let identity = dir.path().join("identity");
    tilo_files::native::private::directory(&identity).unwrap();
    // The deployment's identity is this computer's, whose device key seals the account's root:
    // the daemon adopts the root it is handed once it has taken that key.
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_IDENTITY_ROOT", &identity);
    let keys = tilo_identity::provisioning::collaboration_identity_paths();
    let _daemon = tilo_identity::provisioning::device_key_taken_as_daemon(&keys).unwrap();
    let account = identity.join("accounts").join("1000");
    let write_root = |name: &str, seed: &[u8; 32], mode: u32| {
        let file = dir.path().join(name);
        std::fs::write(&file, hex::encode(seed)).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(mode)).unwrap();
        file
    };

    let seed = [7u8; 32];
    let root_file = write_root("owner-root", &seed, 0o600);
    adopt_root_file(&root_file, &account, &keys).expect("the owner root is kept here");

    let person = SigningKey::from_bytes(&seed).verifying_key().to_bytes();
    let record = tilo_identity::store::held_root(&account).expect("the root is kept here");
    let metadata = tilo_identity::store::load_metadata_from_path(&record).unwrap();
    assert_eq!(metadata.group_public_key, person);
    let (device, cert) = tilo_identity::trusted_device::serving_in(&account, &keys)
        .expect("this device's certificate is here");
    assert_eq!(
        cert.user_public_key, person,
        "the certificate is rooted at the owner"
    );
    assert_eq!(
        cert.device_public_key,
        device.verifying_key().to_bytes(),
        "the certificate is this device's key's"
    );

    // The root kept here is not made again, and another person's stops the start.
    adopt_root_file(&root_file, &account, &keys).expect("a start after the first adopts nothing");
    let another = write_root("another-root", &[9u8; 32], 0o600);
    let refused =
        adopt_root_file(&another, &account, &keys).expect_err("another person is no root here");
    assert!(
        refused.to_string().contains("another person's"),
        "{refused}"
    );

    let open = write_root("open-root", &[11u8; 32], 0o644);
    let refused =
        adopt_root_file(&open, &account, &keys).expect_err("a file others may read is no root");
    assert!(!refused.to_string().is_empty(), "{refused}");

    let malformed = dir.path().join("malformed-root");
    std::fs::write(&malformed, "not a root\n").unwrap();
    std::fs::set_permissions(&malformed, std::fs::Permissions::from_mode(0o600)).unwrap();
    let refused = adopt_root_file(&malformed, &account, &keys).expect_err("words are no root seed");
    assert!(refused.to_string().contains("64 hexadecimal"), "{refused}");
}

/// A device a ticket made, a server among them, keeps its person's public record in
/// its store and no key of theirs: that record opens, but it is no root this device
/// keeps, so the device is a joined one that founds nothing of its person's. The
/// root a device is handed opens as their whole key, and is.
#[test]
fn a_persons_public_record_is_no_root_this_device_keeps() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let identity = dir.path().join("identity");
    tilo_files::native::private::directory(&identity).unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_IDENTITY_ROOT", &identity);
    let keys = tilo_identity::provisioning::collaboration_identity_paths();
    let _daemon = tilo_identity::provisioning::device_key_taken_as_daemon(&keys).unwrap();

    let seed = [7u8; 32];
    let person = SigningKey::from_bytes(&seed).verifying_key().to_bytes();
    let joined = identity.join("accounts").join("1000");
    tilo_files::native::private::directory(&joined).unwrap();
    let public =
        tilo_identity::store::public_member_record("Ada", [1, 2, 3], person).expect("encodes");
    let record = joined.join("primary.json");
    std::fs::write(&record, public).unwrap();
    std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(
        tilo_identity::store::load_from_path(record).is_ok(),
        "the person's public record opens on the device a ticket made"
    );
    assert_eq!(
        opened_root_in(&joined),
        None,
        "a record that holds none of the person's key is no root of theirs kept here"
    );

    let rooted = identity.join("accounts").join("1001");
    let root_file = dir.path().join("owner-root");
    std::fs::write(&root_file, hex::encode(seed)).unwrap();
    std::fs::set_permissions(&root_file, std::fs::Permissions::from_mode(0o600)).unwrap();
    adopt_root_file(&root_file, &rooted, &keys).expect("the owner root is kept here");
    assert!(
        opened_root_in(&rooted).is_some(),
        "the root this device was handed opens as the person's whole key"
    );
}

/// A deployment given its owner's root is that owner's own device: the daemon it
/// starts founds the workspace its configuration keeps, with the entry the
/// deployment names, under the root rather than a key of the server's own.
const OWNER_ROOT_TEST: &str = "device::services::tests::a_deployment_given_its_owner_root_founds_its_kept_workspaces_entry_once";

/// The kept workspace's id, as the deployment names it.
const KEPT_WORKSPACE: &str = "4b0d9a52-7b4e-4e0d-9e0e-2b1c2a0c9a11";

/// What the deployment names for its wait list.
const CONTACT: &str = "hello@lounge.example";
const MAIL_FROM: &str = "invitations@lounge.example";

/// What the owner changes them to, after the workspace is founded.
const OWNER_CONTACT: &str = "owner@lounge.example";
const OWNER_LIST_MAX: usize = 7;

/// The first child start changes the entry, as the workspace's owner does later.
const CHANGE_ENTRY: &str = "TILO_TEST_OWNER_ENTRY_CHANGE";

#[test]
fn a_deployment_given_its_owner_root_founds_its_kept_workspaces_entry_once() {
    use std::os::unix::fs::PermissionsExt as _;
    if let Some(root) = std::env::var_os(PROCESS_ROOT) {
        // The deployment's own process: its root and its configuration, as a container's start.
        let root = PathBuf::from(root);
        let root_file = PathBuf::from(std::env::var_os(OWNER_ROOT_FILE).unwrap());
        let daemon = Daemon::start_reporting(&root, &mut |_| {}).expect("the deployment starts");
        let seed: [u8; 32] = hex::decode(std::fs::read_to_string(&root_file).unwrap().trim())
            .unwrap()
            .try_into()
            .unwrap();
        let owner = SigningKey::from_bytes(&seed).verifying_key().to_bytes();
        assert_eq!(
            daemon.cert.user_public_key, owner,
            "the device acts as the owner root"
        );
        let runtime = tilo_rt::daemon("owner-root-test").build().unwrap();
        let workspaces = daemon.workspaces.service().workspaces();
        let entry = runtime
            .block_on(workspaces.entry_policy(KEPT_WORKSPACE))
            .expect("the kept workspace is founded here");
        let listing = entry.listing.clone().expect("the workspace is listed");
        assert_eq!(listing.description, "Coffee and tea");
        assert_eq!(listing.default_role, tilo_action_policy::Role::Commenter);
        if std::env::var_os(CHANGE_ENTRY).is_some() {
            // The entry the deployment names is settled at founding: its wait list
            // keeps addresses, names a contact and mails them.
            assert!(
                entry.invitation_only,
                "the entry is the invitation-only one the deployment names"
            );
            assert_eq!(entry.wait_list_max, 4);
            assert_eq!(entry.contact.as_deref(), Some(CONTACT));
            assert_eq!(
                entry.mail.as_ref().map(|mail| mail.from.as_str()),
                Some(MAIL_FROM)
            );
            // The owner changes the wait list later, through the entry change every
            // owner makes.
            let changed = tilo_action_policy::EntryPolicy {
                wait_list_max: OWNER_LIST_MAX,
                contact: Some(OWNER_CONTACT.into()),
                ..entry
            };
            runtime
                .block_on(workspaces.found_entry(KEPT_WORKSPACE, &daemon.cert, changed))
                .expect("the owner changes the entry");
        } else {
            // A later start founds nothing and settles nothing: it reads the
            // owner's change, not the values its configuration still names.
            assert_eq!(entry.contact.as_deref(), Some(OWNER_CONTACT));
            assert_eq!(entry.wait_list_max, OWNER_LIST_MAX);
            assert_eq!(
                entry.mail.as_ref().map(|mail| mail.from.as_str()),
                Some(MAIL_FROM)
            );
        }
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let deployment = root.join("deployment");
    std::fs::create_dir_all(&deployment).unwrap();
    let root_file = deployment.join("owner-root");
    std::fs::write(&root_file, hex::encode([3u8; 32])).unwrap();
    std::fs::set_permissions(&root_file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let config = deployment.join("server.toml");
    std::fs::write(
        &config,
        format!(
            "web_entry = \"https://127.0.0.1:1/\"\nweb_listen = \"127.0.0.1:0\"\n\
             identity_root = \"{}\"\nstate_root = \"{}\"\n\n\
             [[kept]]\nid = \"{KEPT_WORKSPACE}\"\nname = \"Lounge\"\nentry = \"invite\"\n\
             default_role = \"commenter\"\ndescription = \"Coffee and tea\"\n\
             wait_list_max = 4\ncontact = \"{}\"\nmail_from = \"{}\"\n",
            deployment.join("server-identity").display(),
            deployment.join("server-state").display(),
            CONTACT,
            MAIL_FROM
        ),
    )
    .unwrap();
    // The deployment's own start, then a restart of the same volume: the first
    // founds the workspace and settles its entry, the second only reads it.
    let run = |change: bool, log: &Path| {
        let mut command = isolated_command(OWNER_ROOT_TEST, &root);
        command
            .env(OWNER_ROOT_FILE, &root_file)
            .env(crate::device::keep::CONFIG_ENV, &config);
        if change {
            command.env(CHANGE_ENTRY, "1");
        } else {
            command.env_remove(CHANGE_ENTRY);
        }
        let status = command
            .stdout(std::fs::File::create(log).unwrap())
            .status_locked()
            .unwrap();
        let stdout = std::fs::read_to_string(log).unwrap();
        assert!(status.success(), "{stdout}");
        assert!(
            stdout.contains("test result: ok. 1 passed"),
            "the isolated child ran no test: {stdout}"
        );
    };
    run(true, &root.join("first.stdout"));
    run(false, &root.join("restart.stdout"));
}

/// What a hosting server's daemon answers its workspaces' devices with, besides its own
/// governance: its Browser, which answers an environment's own ask, and its keys' sessions.
#[cfg(feature = "agents")]
const ENVIRONMENT_ASK_TEST: &str =
    "device::services::tests::a_hosting_daemon_answers_its_environments_media_key_ask";

/// A device `root` certifies as one of its person's, with `capabilities` and the key `byte`
/// seeds.
#[cfg(feature = "agents")]
fn certified_device(
    root: &ed25519_dalek::SigningKey,
    byte: u8,
    capabilities: tilo_user_identity::DeviceCapabilities,
) -> (ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert) {
    let key = ed25519_dalek::SigningKey::from_bytes(&[byte; 32]);
    let now = tilo_clock::unix_now_ns();
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "environment".into(),
        capabilities,
        issued_at_ns: now,
        expires_at_ns: Some(now.saturating_add(60 * 60 * 1_000_000_000)),
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, root)
        .expect("the device's certificate is signed");
    (key, cert)
}

/// The daemon a server's configuration starts answers its workspaces' environments: one placed
/// in a workspace hears the media keys of its own streams, sealed to it, and a device it placed
/// for nothing is refused.
#[test]
#[cfg(feature = "agents")]
#[allow(
    clippy::too_many_lines,
    reason = "one deployment's start, its environment's ask and another device's refusal read in order"
)]
fn a_hosting_daemon_answers_its_environments_media_key_ask() {
    use std::os::unix::fs::PermissionsExt as _;
    use tilo_action_services::environment_device;
    use tilo_actions::remote::{Caller, InvokeRequest, ReceiptStatus, RemoteActionMessage};
    use tilo_user_identity::DeviceCapabilities;

    if let Some(root) = std::env::var_os(PROCESS_ROOT) {
        // The deployment's own process: its root, its configuration and what its Browser
        // sessions are billed to, as a container's start hands them.
        let root = PathBuf::from(root);
        let root_file = PathBuf::from(std::env::var_os(OWNER_ROOT_FILE).unwrap());
        let daemon = Daemon::start_reporting(&root, &mut |_| {}).expect("the deployment starts");
        let owner = daemon.cert.user_public_key;
        let workspace = WorkspaceId::parse(KEPT_WORKSPACE).expect("the kept workspace's id");
        let seed: [u8; 32] = hex::decode(std::fs::read_to_string(&root_file).unwrap().trim())
            .unwrap()
            .try_into()
            .unwrap();
        let root_key = ed25519_dalek::SigningKey::from_bytes(&seed);
        // The workspace's VM device, made as its host makes one: placed for the workspace's
        // records alone, certified by the host's own device, and confined here as the host
        // confines it when it certifies it.
        let vm = {
            let key = ed25519_dalek::SigningKey::from_bytes(&[0x5a; 32]);
            let issued = tilo_clock::unix_now_ns().max(daemon.cert.issued_at_ns);
            let expires = issued
                .saturating_add(tilo_user_identity::MAX_PLACED_DEVICE_LIFETIME_NS)
                .min(daemon.cert.expires_at_ns.unwrap_or(u64::MAX));
            let mut cert = tilo_user_identity::prepare_placed_device_cert(
                &daemon.cert,
                key.verifying_key().to_bytes(),
                tilo_workspace_service::environment::vm_placed_records(workspace)
                    .expect("the workspace's records are well formed"),
                "environment",
                issued,
                expires,
            );
            tilo_user_identity::sign_device_cert(&mut cert, daemon.key())
                .expect("the host certified the workspace's VM");
            cert
        };
        let service = daemon.workspaces.service();
        assert!(
            service
                .sessions()
                .handler
                .environments
                .confine(&vm)
                .unwrap()
                .is_some(),
            "the host placed the workspace's VM"
        );
        let actions = service.actions();
        let runtime = tilo_rt::daemon("environment-ask-test").build().unwrap();
        // Asked from this device's runtime, as the host asks its app action server: what the
        // Browser spawns runs while the answer is waited on.
        let ask = |certificate: &tilo_user_identity::DeviceCert| {
            runtime.block_on(tilo_test_env::within(
                "the environment's media keys",
                async {
                    let message = RemoteActionMessage::Invoke(InvokeRequest::plain(
                        environment_device::target(environment_device::MEDIA_KEYS),
                        serde_json::Map::new(),
                    ));
                    let caller = Caller::authenticated(owner, certificate.device_public_key);
                    let Some(routing) =
                        actions.route_certified(workspace, &caller, certificate, &message)
                    else {
                        panic!("the daemon does not route its environment's ask");
                    };
                    let tilo_workspace_service::app_serving::Routing::Answer(answered) = routing
                    else {
                        panic!("the daemon answers the ask through another device");
                    };
                    match answered.await {
                        Some(RemoteActionMessage::Receipt(receipt)) => receipt.status,
                        other => panic!("the daemon answered {other:?}"),
                    }
                },
            ))
        };
        let answered = ask(&vm);
        let ReceiptStatus::Completed { output } = answered else {
            panic!("the environment's ask is not answered: {answered:?}");
        };
        let keys: environment_device::MediaKeys =
            serde_json::from_value(output.value.expect("the host lent the environment keys"))
                .expect("the lent keys are the media keys");
        assert!(
            !keys.sealed.is_empty(),
            "the host sealed the workspace's media keys to the environment's device"
        );
        // A device of the person's the host placed for nothing is answered by no one: its
        // ask is refused, as a host that serves no Browser refuses every one of them.
        let (_, stranger) = certified_device(&root_key, 0x5b, DeviceCapabilities::member());
        let refused = ask(&stranger);
        assert!(
            matches!(
                refused,
                ReceiptStatus::Rejected {
                    error: tilo_actions::remote::ActionCallError::NotPermitted
                }
            ),
            "another device's ask is answered: {refused:?}"
        );
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let deployment = root.join("deployment");
    std::fs::create_dir_all(&deployment).unwrap();
    let root_file = deployment.join("owner-root");
    std::fs::write(&root_file, hex::encode([3u8; 32])).unwrap();
    std::fs::set_permissions(&root_file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let config = deployment.join("server.toml");
    std::fs::write(
        &config,
        format!(
            "web_entry = \"https://127.0.0.1:1/\"\nweb_listen = \"127.0.0.1:0\"\n\
             identity_root = \"{}\"\nstate_root = \"{}\"\n\n\
             [[kept]]\nid = \"{KEPT_WORKSPACE}\"\nname = \"Lounge\"\n",
            deployment.join("server-identity").display(),
            deployment.join("server-state").display(),
        ),
    )
    .unwrap();
    // What its environments' Browser sessions are billed to, and where they are counted: a
    // host that pays for its workspaces' machines serves them.
    let served = ed25519_dalek::SigningKey::from_bytes(&[7; 32])
        .verifying_key()
        .to_bytes();
    let mut command = isolated_command(ENVIRONMENT_ASK_TEST, &root);
    command
        .env(OWNER_ROOT_FILE, &root_file)
        .env(crate::device::keep::CONFIG_ENV, &config)
        .env(ush::hosted_vm::ACCOUNT_ENV, "acct_test")
        // Where the page actions the Browser it serves runs are counted, as a host that
        // serves them names it.
        .env(
            "TILO_SERVER_BROWSER_ACTIONS",
            deployment.join("browser-actions.jsonl"),
        )
        .env(ush::hosted_vm::PLANS_ENV, deployment.join("plans.toml"))
        .env(
            tilo_server::hosted_browser::SERVER_ENV,
            format!("{}@127.0.0.1:1", hex::encode(served)),
        );
    let log = root.join("child.stdout");
    let status = command
        .stdout(std::fs::File::create(&log).unwrap())
        .status_locked()
        .unwrap();
    let stdout = std::fs::read_to_string(&log).unwrap();
    assert!(status.success(), "{stdout}");
    assert!(
        stdout.contains("test result: ok. 1 passed"),
        "the isolated child ran no test: {stdout}"
    );
}

const OWN_INSTALL_TEST: &str =
    "device::services::tests::a_development_build_installs_on_the_persons_own_connection";

/// The manifest of a development build for this computer's architecture, as the install action
/// reads it from the device's own content store.
fn development_build_manifest() -> String {
    let isa = match std::env::consts::ARCH {
        "x86_64" => "x86_64 = \"v3\"",
        other if other.starts_with("aarch64") => "aarch64 = \"aarch64\"",
        other => panic!("no package for the architecture {other}"),
    };
    format!(
        r#"[package]
id = "dev.tilo.own"
name = "Own"
version = "0.1.0"
kind = "app"

[package.isa]
{isa}

[publisher]
identity = "ed25519:0000000000000000000000000000000000000000000000000000000000000000"
signature = ""

[sandbox]
namespaces = ["user", "pid", "uts", "ipc", "mount"]
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[sandbox.services]
collab = "optional@51"

[entrypoint]
bin = "app"
type = "gui"
"#
    )
}

/// The person on their own daemon connection asks this device to install a development build,
/// with no rule set for it: the workspace decides it may run unasked, and the cache reads the
/// build's manifest and program from the device's own content store.
#[test]
fn a_development_build_installs_on_the_persons_own_connection() {
    use tilo_actions::remote::ActionProvider as _;

    let Some(root) = isolated_root(OWN_INSTALL_TEST) else {
        return;
    };
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());
    let (tree, caller) = daemon.connection_tree_in(None).unwrap();
    assert!(
        caller.is_own_connection(),
        "a plain connection is the person's own: {caller:?}"
    );
    let cert = daemon.workspaces.device_cert();
    let target = RemoteActionTarget {
        provider: crate::device::packages::PROVIDER.to_owned(),
        address: tilo_actions::ActionAddress::new(
            format!("devices/{}/packages", hex::encode(cert.device_public_key)),
            "install-dev",
        ),
    };
    // The build's manifest and program, put for the action itself: the store the cache reads.
    let store = tree
        .content_puts_for(Some(&target))
        .expect("this device keeps what its own actions read");
    let put = |bytes: &[u8]| -> String {
        let mut put = store
            .begin(&caller, u64::try_from(bytes.len()).unwrap())
            .expect("the device opens the upload");
        put.append(0, bytes).expect("the device keeps the bytes");
        put.finish().expect("the device stores the bytes")
    };
    let manifest = put(development_build_manifest().as_bytes());
    let program = put(b"a development build's own program");
    let request = InvokeRequest {
        on_behalf_of: None,
        invocation: tilo_actions::InvocationId::new(),
        target,
        argument: InvokeArgument::Json(
            json!({
                "manifest": {"source": manifest},
                "payload": [{"path": "app", "file": {"source": program}}],
            })
            .as_object()
            .cloned()
            .unwrap_or_default(),
        ),
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let invocation = request.invocation;
    let receipt = tree
        .invoke(&caller, request)
        .wait_timeout(Duration::from_secs(60))
        .ok()
        .flatten()
        .expect("the device answers the call")
        .expect("the device admits a development build from its person's own connection");
    assert!(
        matches!(
            receipt.status,
            tilo_actions::remote::ReceiptStatus::Accepted
                | tilo_actions::remote::ReceiptStatus::Completed { .. }
        ),
        "the cache took the call: {receipt:?}"
    );
    // A cache answers as it goes: the build is installed once its call settles.
    let deadline = Instant::now() + Duration::from_secs(120);
    let state = loop {
        if let Some(state) = tree.observe(&caller, &invocation)
            && state.update.is_terminal()
        {
            break state;
        }
        assert!(
            Instant::now() < deadline,
            "the development build did not settle"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    match &state.update {
        tilo_actions::InvocationUpdate::Succeeded { output, .. } => {
            let report = output.value.clone().unwrap_or_default();
            assert!(
                report
                    .get("installed")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|id| id.starts_with("dev.tilo.own")),
                "the device names the id it installed: {report}"
            );
        }
        other => panic!("the device did not install the build: {other:?}"),
    }
}

const OWN_DEVICE_PROBE: &str = "so.tilo.probe.device";

/// A provider offering `probe-device#publish`, an action of the invoker's own device whose
/// contract asks its person every time: it records what it was asked, and nothing else.
fn own_device_probe() -> Arc<tilo_action_services::TestProvider> {
    use tilo_action_services::TestProvider;
    use tilo_actions::remote::{CataloguePage, CatalogueProvider, CatalogueRow};
    Arc::new(TestProvider::new().with_catalogue(move |caller, query| {
        let row = CatalogueRow::for_action(
            OWN_DEVICE_PROBE,
            OWN_DEVICE_PROBE,
            tilo_actions::ActionDescriptor {
                did: None,
                state: tilo_actions::ActionState::default(),
                semantic_identity: None,
                address: tilo_actions::ActionAddress::new("probe-device", "publish"),
                resource_kind: "probe.device".into(),
                resource_label: "Probe".into(),
                action_label: "Publish".into(),
                keywords: Vec::new(),
                icon: None,
                description: None,
                locus: tilo_actions::ExecutionLocus::ClientLocal,
                kind: tilo_actions::OperationKind::Effect,
                contract: tilo_actions::ActionContract {
                    kind: tilo_actions::ActionKind::Outside,
                    confirmation: tilo_actions::ConfirmationPolicy::Confirm,
                    tags: vec![tilo_actions::OWN_DEVICE_SCOPE.to_owned()],
                    ..tilo_actions::ActionContract::default()
                },
                input: tilo_actions::InputSpec::None,
                params: Vec::new(),
                view: None,
            },
        );
        let mut page = CataloguePage::from_rows("1".into(), vec![row], &query);
        if query.provider.is_none() {
            page.providers.push(CatalogueProvider {
                provider: OWN_DEVICE_PROBE.into(),
                app_id: OWN_DEVICE_PROBE.into(),
                workspace: caller.workspace().unwrap_or_default().to_owned(),
                document_id: String::new(),
                browsable: true,
            });
        }
        Ok(page)
    }))
}

/// The call of the probe's action, as the person's own connection makes it.
fn own_device_call() -> InvokeRequest {
    InvokeRequest::plain(
        RemoteActionTarget {
            provider: OWN_DEVICE_PROBE.into(),
            address: tilo_actions::ActionAddress::new("probe-device", "publish"),
        },
        serde_json::Map::new(),
    )
}

/// What `pending` answers, within the hang bound.
fn answered<T: Send + 'static>(
    pending: tilo_actions::remote::Pending<Result<T, tilo_actions::remote::ActionCallError>>,
) -> Result<T, tilo_actions::remote::ActionCallError> {
    pending
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten()
        .expect("the daemon answered")
}

const OWN_ASK_TEST: &str =
    "device::services::tests::an_own_device_actions_ask_is_answered_by_the_persons_prompt";

/// An action of the person's own device whose contract asks them every time, called on their own
/// connection: it waits as its asked record and nothing runs until their prompt approves that
/// record, and an answer given again runs nothing more. A confirmation the call writes itself is
/// no answer to it.
#[test]
fn an_own_device_actions_ask_is_answered_by_the_persons_prompt() {
    use tilo_action_services::workspace_invocations;
    use tilo_actions::remote::asked::{AskedRecords as _, RESOLVE};
    use tilo_actions::remote::{ActionProvider as _, Confirmation, ReceiptStatus};

    let Some(root) = isolated_root(OWN_ASK_TEST) else {
        return;
    };
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());
    let probe = own_device_probe();
    daemon.provide(OWN_DEVICE_PROBE, Arc::clone(&probe) as _);
    let (tree, caller) = daemon.connection_tree_in(None).unwrap();
    assert!(
        caller.is_own_connection(),
        "a plain connection is the person's own: {caller:?}"
    );

    // Their call waits for them: its asked record is written, and nothing of it ran.
    let first = answered(tree.invoke(&caller, own_device_call())).expect("the call is admitted");
    assert!(
        matches!(first.status, ReceiptStatus::Accepted),
        "the call waits for its person: {first:?}"
    );
    assert!(probe.asked().is_empty(), "nothing of it ran yet");
    let asked_records = daemon
        .asked
        .store()
        .asked_of(&daemon.workspaces.device_cert().user_public_key);
    let question = asked_records
        .iter()
        .find(|asked| asked.action.ends_with("#publish"))
        .unwrap_or_else(|| panic!("the call's question is kept: {asked_records:?}"))
        .clone();

    // Their own connection answers it as their CLI prompt does: their yes runs the call, once.
    let resolve = || {
        let mut approve = serde_json::Map::new();
        approve.insert("resolution".into(), serde_json::json!("approve"));
        InvokeRequest::plain(
            RemoteActionTarget {
                provider: workspace_invocations::PROVIDER.into(),
                address: tilo_actions::ActionAddress::new(question.record.record.clone(), RESOLVE),
            },
            approve,
        )
    };
    answered(tree.invoke(&caller, resolve())).expect("their answer is admitted");
    let deadline = Instant::now() + tilo_test_env::hang_bound();
    while probe.asked().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(probe.asked().len(), 1, "their yes runs it, once");

    // The same answer again runs nothing more.
    let _ = answered(tree.invoke(&caller, resolve()));
    assert_eq!(probe.asked().len(), 1, "an answer given twice runs nothing");

    // A call that writes its own confirmation is no answer to the question that waits.
    let mut written = own_device_call();
    written.confirmation = Some(Confirmation::Operated);
    let waiting = answered(tree.invoke(&caller, written)).expect("the call is admitted");
    assert!(
        matches!(waiting.status, ReceiptStatus::Accepted),
        "a confirmation the call writes itself waits: {waiting:?}"
    );
    assert_eq!(
        probe.asked().len(),
        1,
        "nothing ran for it, whatever it wrote"
    );
}

const SECRET_RECORD_TEST: &str =
    "device::services::tests::a_secret_answer_is_kept_out_of_every_invocation_record";

/// A vault item's secret reaches the caller that asked in the answer alone: the device's record
/// of the call keeps only that it was given, and another connection reading the workspace's
/// invocations reads no secret there. Asked through the dispatch a plain connection of the
/// person's reaches, which records the call before the workspace's tree answers it.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one secret answer, from the item to the record another member reads"
)]
fn a_secret_answer_is_kept_out_of_every_invocation_record() {
    use serde_json::json;
    use tilo_actions::remote::{
        InvocationReceipt, InvokeRequest, ReceiptStatus, WORKSPACE_GOVERNANCE_PROVIDER,
        WORKSPACE_VAULT, workspace_governance_child,
    };
    use tilo_actions::{ActionAddress, InvocationId};
    use tilo_person_services::account_logins::{AccountLogin, Place};

    let Some(root) = isolated_root(SECRET_RECORD_TEST) else {
        return;
    };
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());
    let workspace = daemon.workspace.clone();
    let workspaces = daemon.workspaces();
    let (tree, caller) = daemon.connection_tree_in(Some(&workspace)).unwrap();
    assert!(
        caller.is_own_connection(),
        "a plain connection is the person's own"
    );
    // The item, as Passwords keeps it: the vault takes it from a device of the workspace's,
    // which this computer is.
    let cert = daemon.cert.clone();
    let device =
        tilo_actions::remote::Caller::authenticated(cert.user_public_key, cert.device_public_key)
            .in_workspace(&workspace);
    let reference = tilo_rt::app()
        .block_on(async {
            let place = Place::parse("https://bank.example.test").unwrap();
            tilo_person_services::PersonServices::of(workspaces)
                .keep_account_password(
                    &workspace,
                    (&device, None),
                    None,
                    AccountLogin {
                        title: "Bank".into(),
                        username: "ada".into(),
                        places: vec![place],
                    },
                    zeroize::Zeroizing::new("made-up-password".to_owned()),
                )
                .await
                .expect("the workspace's vault keeps the item")
        })
        .expect("the item is kept");

    let target = RemoteActionTarget {
        provider: WORKSPACE_GOVERNANCE_PROVIDER.to_owned(),
        address: ActionAddress::new(
            workspace_governance_child(&workspace, Some(WORKSPACE_VAULT)),
            "reveal",
        ),
    };
    let argument = json!({"item": reference.to_string(), "part": "secret"});
    let request = InvokeRequest {
        on_behalf_of: None,
        invocation: InvocationId::new(),
        target,
        argument: InvokeArgument::Json(argument.as_object().cloned().unwrap_or_default()),
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let id = request.invocation;
    let answered = tree
        .invoke(&caller, request)
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten();
    let Some(Ok(InvocationReceipt {
        status: ReceiptStatus::Completed { output },
        ..
    })) = answered
    else {
        panic!("the person is shown the item's secret: {answered:?}");
    };
    assert_eq!(
        output.value.unwrap_or_default()["value"],
        json!("made-up-password")
    );

    let record = daemon
        .invocations
        .get(&caller, &id)
        .expect("the call is recorded");
    assert!(record.secret, "a reveal answers a secret");
    let kept = serde_json::to_string(&(&record.request, &record.state)).unwrap();
    assert!(!kept.contains("made-up-password"), "{kept}");
    if let Some(state) = &record.state {
        let state = serde_json::to_string(state).unwrap();
        assert!(state.contains("given"), "only that it was given: {state}");
    }
    assert!(
        matches!(record.request.argument, InvokeArgument::None),
        "the call's arguments are kept out too"
    );

    // Another connection of the person, with Read on what the call addressed, reads the
    // workspace's invocations: neither the listing nor the record holds the secret.
    let member = device;
    let broker = daemon.broker.fanout.app_broker().expect("the broker runs");
    let listed = broker
        .invocation_listing(
            &workspace,
            &member,
            &tilo_action_services::workspace_invocations::InvocationsQuery {
                invocation: Some(id.to_string()),
                ..Default::default()
            },
        )
        .expect("a member with Read lists the invocations");
    let text = serde_json::to_string(&listed).unwrap();
    assert!(!text.contains("made-up-password"), "{text}");
    let read = broker
        .read_invocations(&member, &workspace, &id.to_string(), Vec::new())
        .expect("a member with Read reads the record");
    let text = format!("{read:?}");
    assert!(!text.contains("made-up-password"), "{text}");
}

const SECRET_ARGUMENT_TEST: &str =
    "device::services::tests::a_secret_argument_is_kept_out_of_every_invocation_record";

/// A parameter an action declares secret reaches the action in the call alone: the device's
/// record of the call keeps its name, not the code, the key or the seed, and another connection
/// reading the workspace's invocations reads none of them. The device reads the declarations it
/// was handed at its start, which is what every record of a call is decided by.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one code, one key and one seed, from the calls to the record another member reads"
)]
fn a_secret_argument_is_kept_out_of_every_invocation_record() {
    use serde_json::json;
    use tilo_actions::remote::InvokeRequest;
    use tilo_actions::{ActionAddress, InvocationId};
    use tilo_workspace_records::accounts as workspace_accounts;
    use tilo_workspace_records::models as model_providers;

    let Some(root) = isolated_root(SECRET_ARGUMENT_TEST) else {
        return;
    };
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());
    let workspace = daemon.workspace.clone();
    let (tree, caller) = daemon.connection_tree_in(Some(&workspace)).unwrap();
    assert!(
        caller.is_own_connection(),
        "a plain connection is the person's own"
    );
    let ask =
        |provider: &str, resource: &str, action: &str, argument: serde_json::Value| InvokeRequest {
            on_behalf_of: None,
            invocation: InvocationId::new(),
            target: RemoteActionTarget {
                provider: provider.to_owned(),
                address: ActionAddress::new(resource, action),
            },
            argument: match argument {
                serde_json::Value::Object(arguments) => InvokeArgument::Json(arguments),
                _ => InvokeArgument::None,
            },
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };

    // What the device was handed at its start: the one decision every record of a call reads.
    // The vault's own actions are served where the vault is, once the person's agents run.
    let _ = tilo_rt::app().block_on(async {
        tilo_app_host::vault_actions::register(daemon.workspaces());
    });
    let secrets = daemon.workspaces().secret_parameters();
    assert_eq!(
        secrets(&workspace_accounts::target(
            workspace_accounts::SUBMIT_SIGN_IN_CODE
        )),
        Some(vec!["code".to_owned()]),
        "a sign-in's code is declared secret, and its sign-in is not"
    );
    assert_eq!(
        secrets(&model_providers::target(model_providers::SET_KEY)),
        Some(vec!["key".to_owned()]),
        "a model provider's key is declared secret, and its provider is not"
    );
    let vault_keep = RemoteActionTarget {
        provider: "so.tilo.vault".to_owned(),
        address: ActionAddress::new("vault.item", "keep"),
    };
    assert_eq!(
        secrets(&vault_keep),
        Some(vec!["secret".to_owned()]),
        "the seed a code generator is kept with is declared secret"
    );

    // The one-time code a sign-in finishes with.
    let code = "made-up-code-9182";
    let request = ask(
        workspace_accounts::PROVIDER,
        workspace_accounts::RESOURCE,
        workspace_accounts::SUBMIT_SIGN_IN_CODE,
        json!({ "signIn": "made-up-sign-in", "code": code }),
    );
    let code_invocation = request.invocation;
    let _answer = tree
        .invoke(&caller, request)
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten();
    if let Some(record) = daemon.invocations.get(&caller, &code_invocation) {
        assert_eq!(
            record.sealed,
            vec!["code".to_owned()],
            "only the code is declared secret"
        );
        let kept = serde_json::to_string(&(&record.request, &record.state)).unwrap();
        assert!(!kept.contains(code), "{kept}");
        assert!(
            kept.contains("made-up-sign-in"),
            "what the declaration does not call secret stays: {kept}"
        );
    }

    // The key a model provider is signed in with.
    let key = "made-up-key-3317";
    let request = ask(
        model_providers::PROVIDER,
        model_providers::RESOURCE,
        model_providers::SET_KEY,
        json!({ "provider": "made-up-provider", "key": key }),
    );
    let key_invocation = request.invocation;
    let _answer = tree
        .invoke(&caller, request)
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten();
    if let Some(record) = daemon.invocations.get(&caller, &key_invocation) {
        assert_eq!(
            record.sealed,
            vec!["key".to_owned()],
            "only the key is declared secret"
        );
        let kept = serde_json::to_string(&(&record.request, &record.state)).unwrap();
        assert!(!kept.contains(key), "{kept}");
        assert!(kept.contains("made-up-provider"), "{kept}");
    }

    // A provider that publishes the action with its parameters: the seed a code generator is
    // kept with is as secret as the host's own declarations say.
    let seed = "JBSWY3DPEHPK3PXP";
    let request = ask(
        "so.tilo.vault",
        "vault.item",
        "keep",
        json!({ "issuer": "made-up-service", "secret": seed }),
    );
    let seed_invocation = request.invocation;
    let _answer = tree
        .invoke(&caller, request)
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten();
    let record = daemon
        .invocations
        .get(&caller, &seed_invocation)
        .expect("the call is recorded");
    assert_eq!(
        record.sealed,
        vec!["secret".to_owned()],
        "the seed is secret"
    );
    let kept = serde_json::to_string(&(&record.request, &record.state)).unwrap();
    assert!(!kept.contains(seed), "{kept}");
    assert!(kept.contains("made-up-service"), "{kept}");

    // Another connection of the person, with Read on what the calls addressed, reads the
    // workspace's invocations: no listing holds the code, the key or the seed.
    let cert = daemon.cert.clone();
    let member =
        tilo_actions::remote::Caller::authenticated(cert.user_public_key, cert.device_public_key)
            .in_workspace(&workspace);
    let broker = daemon.broker.fanout.app_broker().expect("the broker runs");
    let mut invocations = vec![seed_invocation];
    for invocation in [code_invocation, key_invocation] {
        if daemon.invocations.get(&caller, &invocation).is_some() {
            invocations.push(invocation);
        }
    }
    for invocation in invocations {
        let listed = broker
            .invocation_listing(
                &workspace,
                &member,
                &tilo_action_services::workspace_invocations::InvocationsQuery {
                    invocation: Some(invocation.to_string()),
                    ..Default::default()
                },
            )
            .expect("a member with Read lists the invocations");
        let text = serde_json::to_string(&listed).unwrap();
        assert!(!text.contains(code), "{text}");
        assert!(!text.contains(key), "{text}");
        assert!(!text.contains(seed), "{text}");
        let read = broker
            .read_invocations(&member, &workspace, &invocation.to_string(), Vec::new())
            .expect("a member with Read reads the record");
        let text = format!("{read:?}");
        assert!(!text.contains(code), "{text}");
        assert!(!text.contains(key), "{text}");
        assert!(!text.contains(seed), "{text}");
    }
}

/// A seat has one shell: its link is made once, and while it stands no other link for the seat is
/// made; the seat takes a new one only after its link ends.
#[cfg(unix)]
#[test]
fn a_seat_takes_one_shell_link_at_a_time() {
    const TEST: &str = "device::services::tests::a_seat_takes_one_shell_link_at_a_time";
    let Some(root) = isolated_root(TEST) else {
        return;
    };
    let session = Daemon::start_in_dev_mode(&root, &mut |_| {}).unwrap();
    crate::RuntimeServices::workspace(&session).unwrap();
    let person = session.workspaces.device_cert().user_public_key;
    let seat = 7;
    let token = session
        .shell_token_for(person, seat)
        .expect("a free seat takes a link");
    let link = session
        .link_with(&token)
        .expect("the link reaches the dispatch");
    let deadline = tilo_test_env::hang_deadline();
    while !session.shell_live(seat) {
        assert!(
            std::time::Instant::now() < deadline,
            "the dispatch admits the seat's link"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let refused = session.shell_token_for(person, seat);
    assert!(
        refused.is_err(),
        "a seat with a live link makes no second one"
    );
    // Another seat, and the same seat once its link ended, take a link.
    session
        .shell_token_for(person, seat + 1)
        .expect("another seat makes its own link");
    drop(link);
    let deadline = tilo_test_env::hang_deadline();
    while session.shell_live(seat) {
        assert!(
            std::time::Instant::now() < deadline,
            "the seat's link ended as its connection did"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    session
        .shell_token_for(person, seat)
        .expect("a seat whose link ended takes another");
}

/// A runtime's endpoint is reached through the server it logged in to, not number 0's relays: the
/// server's relay resolves no name of its own, and a relay answering on this machine is asked no
/// router for a port. A computer that logged in to nothing keeps the person's relays and their
/// directory.
#[test]
fn a_runtimes_endpoint_relays_as_the_server_it_logged_in_to() {
    let person = tilo_endpoint_reachability::EndpointRelays {
        relays: iroh::RelayMap::empty(),
        n0_dns: true,
    };
    let here: iroh::RelayUrl = "http://127.0.0.1:24106/".parse().unwrap();
    assert!(relay_on_this_machine(&here));
    assert!(relay_on_this_machine(
        &"http://localhost:24106/".parse().unwrap()
    ));
    assert!(relay_on_this_machine(
        &"http://[::1]:24106/".parse().unwrap()
    ));
    let (mode, portmapper, n0_dns) = endpoint_reach(Some(here.clone()), &person);
    assert_eq!(mode.relay_map().urls::<Vec<_>>(), vec![here]);
    assert!(
        !n0_dns,
        "the server's own relay needs no directory of number 0's"
    );
    assert!(
        matches!(portmapper, iroh::endpoint::PortmapperConfig::Disabled),
        "a relay on this machine is reached from no router"
    );

    let away: iroh::RelayUrl = "https://relay.example./".parse().unwrap();
    assert!(!relay_on_this_machine(&away));
    // A person who set a relay of their own: the portmapper is what a host at home keeps.
    let person = tilo_endpoint_reachability::EndpointRelays {
        relays: iroh::RelayMap::from(away.clone()),
        n0_dns: true,
    };
    let (mode, portmapper, n0_dns) = endpoint_reach(Some(away.clone()), &person);
    assert_eq!(mode.relay_map().urls::<Vec<_>>(), vec![away.clone()]);
    assert!(!n0_dns);
    assert!(
        !matches!(portmapper, iroh::endpoint::PortmapperConfig::Disabled),
        "a server reached through a public relay keeps the portmapper, as any host at home does"
    );

    let (mode, _, n0_dns) = endpoint_reach(None, &person);
    assert_eq!(mode.relay_map().urls::<Vec<_>>(), vec![away]);
    assert!(n0_dns);
}

const SHELL_AGENT_TEST: &str =
    "device::services::tests::an_agent_the_shell_names_lists_in_a_run_of_its_own";

/// An agent the person admitted on their shell, under the key this device made it, reaches the
/// device's dispatch through the shell's link, which names it as each call's caller: in its
/// session on the shell, each call in an execution of its own. The person's own run takes no
/// call of another caller; the agent's calls go in a run of its own, delegated by the person,
/// whose every call is the agent's, whatever execution it comes in.
#[test]
fn an_agent_the_shell_names_lists_in_a_run_of_its_own() {
    use tilo_actions::remote::{ActionCallError, ActionProvider as _, CatalogueQuery};

    let Some(root) = isolated_root(SHELL_AGENT_TEST) else {
        return;
    };
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());
    let workspace = daemon.workspace().unwrap();
    let person = daemon.person().unwrap();
    let persons = person.provider(&workspace).unwrap();
    let person_id = persons
        .admission_caller()
        .and_then(|caller| caller.acting().person_id().map(str::to_owned))
        .expect("the person's run names its person");
    // The session lists the agent in the workspace as it admits it, then has this daemon make
    // and certify its key (`AgentKeyCall::Grant`); the shell's link names it as each call's
    // caller.
    let root = daemon.workspaces.device_cert().user_public_key;
    let name = "cargo headless check";
    let id = tilo_actions::remote::device_agent_id(&root, name);
    let listed_as = Caller::delegated_agent(id.clone(), root, [7; 32]).named(name);
    let person_caller = persons.admission_caller().unwrap();
    tilo_rt::app()
        .handle()
        .block_on(tilo_app_host::agents::workspace_agent(
            daemon.workspaces.copies(),
            WorkspaceId::parse(&workspace).unwrap(),
            &listed_as,
            &person_caller,
        ));
    let keys = tilo_app_host::agents::agent_keys::this_device().unwrap();
    let (_, agent) = tilo_app_host::agents::agent_keys::AgentSigner::grant(
        keys,
        &workspace,
        (&id, name),
        &person_id,
    )
    .unwrap();
    let named = || {
        agent
            .clone()
            .in_agent_session("so.tilo.session/agent-session/test")
            .kept()
            .carried()
            .in_new_execution()
            .in_workspace(workspace.clone())
    };
    let everything: CatalogueQuery = serde_json::from_value(json!({})).unwrap();
    let listed = |provider: &RuntimeActionProvider, caller: &Caller| {
        provider
            .query(caller, everything.clone())
            .wait_timeout(tilo_test_env::hang_bound())
            .ok()
            .flatten()
            .expect("the daemon answers the listing")
    };

    assert!(
        matches!(
            listed(&persons, &named()),
            Err(ActionCallError::NotPermitted)
        ),
        "the person's own run takes no call of their agent's"
    );
    let run = person.agent_provider(&workspace, &named()).unwrap();
    for call in 0..2 {
        let page = listed(&run, &named());
        assert!(
            page.is_ok(),
            "call {call} of the agent's lists in its run: {page:?}"
        );
    }
}

/// An agent the person admitted on their shell, on the shell's own link: the workspace lists it,
/// this device made and certified its key, and the link names it as each call's caller.
#[cfg(unix)]
struct ShellAgent {
    daemon: Arc<Daemon>,
    workspace: String,
    /// The caller the shell's link names for the agent.
    agent: Caller,
    /// The caller the same link names for the person.
    person: Caller,
    /// The agent's id, and the person it acts for, as the workspace's record names them.
    id: String,
    person_id: String,
    client: runtime_collab::ShellClient,
    runtime: tokio::runtime::Runtime,
}

#[cfg(unix)]
impl ShellAgent {
    /// The person's daemon, its shell link, and the agent the workspace lists and this device
    /// made and certified a key for (`AgentKeyCall::Grant`).
    fn on(root: &std::path::Path) -> Self {
        use runtime_collab::wire::shell::SHELL_MAGIC;
        use tokio::io::AsyncWriteExt as _;

        let daemon = Arc::new(Daemon::start_in_dev_mode(root, &mut |_| {}).unwrap());
        let workspace = daemon.workspace().unwrap();
        let cert = daemon.workspaces.device_cert();
        let person = daemon.person().unwrap();
        let persons = person.provider(&workspace).unwrap();
        let person_caller = persons.admission_caller().unwrap();
        let person_id = person_caller
            .acting()
            .person_id()
            .expect("the person's run names its person")
            .to_owned();
        // The session lists the agent in the workspace as it admits it, then has this daemon make
        // and certify its key; the shell's link names it as each call's caller.
        let name = "shell agent";
        let id = tilo_actions::remote::device_agent_id(&cert.user_public_key, name);
        let listed_as =
            Caller::delegated_agent(id.clone(), cert.user_public_key, [7; 32]).named(name);
        tilo_rt::app()
            .handle()
            .block_on(tilo_app_host::agents::workspace_agent(
                daemon.workspaces.copies(),
                WorkspaceId::parse(&workspace).unwrap(),
                &listed_as,
                &person_caller,
            ));
        let keys = tilo_app_host::agents::agent_keys::this_device().unwrap();
        let (_, agent) = tilo_app_host::agents::agent_keys::AgentSigner::grant(
            keys,
            &workspace,
            (&id, name),
            &person_id,
        )
        .unwrap();
        let socket = daemon.broker.app_socket().to_path_buf();
        let secret = daemon
            .shell_links()
            .make(tilo_app_host::shell::ShellCaller {
                person: cert.user_public_key,
            });
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let client = runtime.block_on(async {
            let mut stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
            stream.write_all(SHELL_MAGIC).await.unwrap();
            stream.write_all(&secret).await.unwrap();
            runtime_collab::ShellClient::open(stream).0
        });
        Self {
            daemon,
            workspace,
            agent,
            person: person_caller,
            id,
            person_id,
            client,
            runtime,
        }
    }

    /// The agent's caller as the shell's link carries it: each call in an execution of its own.
    fn by_the_agent(&self) -> Caller {
        self.agent.clone().in_new_execution()
    }

    /// `request` asked on the shell's link as `caller`.
    fn asking(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::InvocationReceipt, ActionCallError>,
    > {
        self.client.dispatch().invoke(caller, request)
    }

    /// The agent's ask to invite someone into the workspace.
    fn invite(&self) -> InvokeRequest {
        use tilo_actions::remote::{RemoteActionTarget, WORKSPACE_INVITE};
        InvokeRequest::plain(
            RemoteActionTarget::workspace_governance(&self.workspace, WORKSPACE_INVITE),
            json!({ "role": "viewer" }).as_object().unwrap().clone(),
        )
    }

    /// An ask of the host's own operations: the workspaces this host serves.
    fn host_list() -> InvokeRequest {
        use tilo_server_protocol::workspaces::{
            HOST_ACTIONS_PROVIDER, HOST_WORKSPACES_RESOURCE, LIST_WORKSPACES,
        };
        InvokeRequest::plain(
            tilo_actions::remote::RemoteActionTarget {
                provider: HOST_ACTIONS_PROVIDER.to_owned(),
                address: tilo_actions::ActionAddress::new(
                    HOST_WORKSPACES_RESOURCE.to_owned(),
                    LIST_WORKSPACES,
                ),
            },
            serde_json::Map::new(),
        )
    }

    /// The person's own device grants its agent `answer` for inviting alone, as their Agents
    /// settings give one action, written as its own change to the workspace's people and
    /// settings.
    fn answer_the_agents_invites(&self, answer: tilo_action_policy::Answer) {
        use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand;

        let scope = tilo_action_policy::agent_answers::AnswerScope::action(
            tilo_action_policy::agent_answers::WORKSPACE_APP,
            tilo_actions::remote::WORKSPACE_GOVERNANCE_KIND,
            tilo_actions::remote::WORKSPACE_INVITE,
        );
        let grant = tilo_action_policy::agent_answers::grant(
            &self.workspace,
            (&self.id, &self.person_id),
            (&scope, tilo_action_policy::roles::GOVERNANCE_KIND),
            answer,
            tilo_action_policy::Grantee::person(self.person_id.clone()),
        );
        let copies = self.daemon.workspaces.copies().clone();
        let workspace = self.workspace.clone();
        self.runtime
            .block_on(async move {
                copies
                    .commit_own_change(
                        &workspace,
                        WorkspaceMembershipCommand::Grant { grant },
                        None,
                    )
                    .await
            })
            .expect("the person's own device writes the agent's answer");
    }
}

/// What a call on a shell's link was answered with.
#[cfg(unix)]
type ShellAnswer = Result<tilo_actions::remote::InvocationReceipt, ActionCallError>;

/// The answer the daemon gave a shell's call, or why it gave none.
#[cfg(unix)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "a test helper: a call the daemon never answered fails the test"
)]
fn answer_of(
    answered: Result<Option<ShellAnswer>, tilo_actions::remote::Pending<ShellAnswer>>,
) -> ShellAnswer {
    match answered {
        Ok(Some(answer)) => answer,
        Ok(None) => panic!("the daemon dropped the shell's call"),
        Err(mut pending) => match pending.poll_ready() {
            PendingPoll::Ready(answer) => answer,
            _ => panic!("the daemon did not answer the shell's call"),
        },
    }
}

const SHELL_AGENT_INVITE_TEST: &str =
    "device::services::tests::an_agent_the_shell_names_invites_only_as_its_person_says_yes";

/// An agent the person admitted on their shell asks to invite someone into the workspace over
/// that shell's link. The daemon answers as the agent: the grant it starts with asks its person
/// first, so nothing is issued on the person's own authority, and the invitation waits for their
/// yes.
#[cfg(unix)]
#[test]
fn an_agent_the_shell_names_invites_only_as_its_person_says_yes() {
    use tilo_actions::remote::ReceiptStatus;

    let Some(root) = isolated_root(SHELL_AGENT_INVITE_TEST) else {
        return;
    };
    let shell = ShellAgent::on(&root);
    let answer = answer_of(
        shell
            .asking(&shell.by_the_agent(), shell.invite())
            .wait_timeout(tilo_test_env::hang_bound()),
    );
    assert!(
        matches!(
            &answer,
            Ok(receipt) if matches!(receipt.status, ReceiptStatus::Accepted)
        ),
        "the agent's own grant asks its person before anything is issued: {answer:?}"
    );
}

const SHELL_AGENT_INVITE_DENIED_TEST: &str =
    "device::services::tests::an_agent_the_shell_names_invites_no_further_than_its_grant";

/// An agent the person admitted on their shell, whose grant refuses inviting, asks to invite
/// someone into the workspace over that shell's link: the daemon refuses the call as not
/// permitted, decided by the agent's grant, never by the person's own authority.
#[cfg(unix)]
#[test]
fn an_agent_the_shell_names_invites_no_further_than_its_grant() {
    use tilo_actions::remote::ActionCallError;

    let Some(root) = isolated_root(SHELL_AGENT_INVITE_DENIED_TEST) else {
        return;
    };
    let shell = ShellAgent::on(&root);
    shell.answer_the_agents_invites(tilo_action_policy::Answer::Deny);
    let refused = answer_of(
        shell
            .asking(&shell.by_the_agent(), shell.invite())
            .wait_timeout(tilo_test_env::hang_bound()),
    );
    assert!(
        matches!(refused, Err(ActionCallError::NotPermitted)),
        "the agent's grant does not cover inviting: {refused:?}"
    );
}

const SHELL_AGENT_HOST_TEST: &str =
    "device::services::tests::an_agent_the_shell_names_asks_no_host_operation";

/// An agent the person admitted on their shell asks the host for the workspaces it serves over
/// that shell's link: the host's own operations are this device's own for its person, so they are
/// refused as not permitted, never answered as the person. The person's own call on the same link
/// stays theirs.
#[cfg(unix)]
#[test]
fn an_agent_the_shell_names_asks_no_host_operation() {
    use tilo_actions::remote::{ActionCallError, ReceiptStatus};

    let Some(root) = isolated_root(SHELL_AGENT_HOST_TEST) else {
        return;
    };
    let shell = ShellAgent::on(&root);
    let refused = answer_of(
        shell
            .asking(&shell.by_the_agent(), ShellAgent::host_list())
            .wait_timeout(tilo_test_env::hang_bound()),
    );
    assert!(
        matches!(refused, Err(ActionCallError::NotPermitted)),
        "no agent asks the host's own operations: {refused:?}"
    );

    let own = answer_of(
        shell
            .asking(&shell.person, ShellAgent::host_list())
            .wait_timeout(tilo_test_env::hang_bound()),
    );
    assert!(
        matches!(
            &own,
            Ok(receipt) if matches!(receipt.status, ReceiptStatus::Completed { .. })
        ),
        "the person's own call on the same link lists the host's workspaces: {own:?}"
    );
}
