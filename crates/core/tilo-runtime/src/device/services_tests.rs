use super::*;
use serde_json::json;
use std::{
    process::Stdio,
    time::{Duration, Instant},
};
use tilo_actions::remote::{InvokeArgument, PendingPoll, RemoteActionTarget};
use tilo_desktop_model::WorkspaceId;
use tilo_test_env::SpawnUnderEnvLock as _;
use tilo_workspace_service::apps::services::WorkspaceGrants;

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
    time: tilo_workspace_service::apps::policy_clock::PolicyTime,
}

impl tilo_workspace_service::apps::certificate_authority::CertificateAuthority
    for FixtureAuthority
{
    fn policy_time(
        &self,
    ) -> Result<Option<tilo_workspace_service::apps::policy_clock::PolicyTime>, String> {
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
        time: &tilo_workspace_service::apps::policy_clock::PolicyTime,
    ) -> Result<(), String> {
        if !time.same_origin(&self.time) {
            return Err("runtime origin changed".into());
        }
        self.authorize(workspace, cert)
    }
    fn governs(&self, workspace: uuid::Uuid, _device: Option<&[u8; 32]>) -> bool {
        workspace == self.workspace
    }
}

#[tokio::test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the broker fixture owns its authority and invocation registry for the whole scenario"
)]
#[allow(clippy::too_many_lines, reason = "one end-to-end ownership scenario")]
async fn runtime_invocation_ownership_survives_device_normalization() {
    use tilo_workspace_service::apps::{
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

    let placed = WorkspaceId::new();
    let (workspaces, directory) = ServiceWorkspaces::test_hosting(placed).await;
    let actor = tilo_workspace_service::apps::collab_app_broker::person_id_for_root(
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
    assert_eq!(
        provider
            .cancel(&other, &id)
            .wait_timeout(std::time::Duration::from_secs(30))
            .ok()
            .flatten()
            .expect("the provider answers"),
        Err(ActionCallError::NotPermitted)
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
pub(crate) fn found_here() {
    super::found_account_person(
        &tilo_identity::provisioning::collaboration_identity_paths(),
        "",
        rustix::process::geteuid().as_raw(),
    )
    .unwrap();
}

/// An SDK app's collaboration manager once it has joined the broker.
fn connected_manager() -> ui::collaboration::CollaborationManager {
    let mut manager = ui::collaboration::CollaborationManager::new();
    let started = Instant::now();
    while manager.state() == ui::collaboration::CollaborationState::Connecting
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
        manager.publish_action_directory(ui::collaboration::local_app_id());
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
    let listed = tilo_rt::try_app()
        .unwrap()
        .block_on(async {
            let server = session
                .mcp_session_factory(&session.workspace().unwrap())
                .unwrap()()
            .unwrap();
            server
                .handle(json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "resources/list",
                    "params": {}
                }))
                .await
                .unwrap()
        })
        .unwrap();
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
    // MCP lists each instance as a resource of its own, named by its app.
    let apps = listed["result"]["resources"]
        .as_array()
        .unwrap_or_else(|| panic!("{listed}"));
    assert_eq!(apps.len(), 2, "{listed}");
    assert_ne!(apps[0]["uri"], apps[1]["uri"], "{listed}");
    assert!(
        apps.iter().all(|app| app["name"]
            .as_str()
            .is_some_and(|name| name.starts_with("dev.tilo.same ("))),
        "{listed}"
    );
    let founded = session.workspace().expect("the workspace");
    drop(session);
    // The profile is released and served again with the same workspace.
    let reopened = Daemon::start_reporting(&root, &mut |_| {}).unwrap();
    assert_eq!(reopened.workspace().expect("the workspace"), founded);
}

const WRITER_TEST: &str = "device::services::tests::an_sdk_apps_sealed_write_in_the_runtimes_own_workspace_is_durable_across_restart";
const WRITER_HELPER: &str = "TILO_DAEMON_WRITER_HELPER";
const WRITER_APP: &str = "dev.tilo.writer";
const WRITER_KIND: &str = "dev.tilo.writer.note";
const WRITER_RECORD: &str = "0b7d9a3e-5c1f-4a8e-9d2b-7f6e5a4c3b21";
const WRITTEN: &str = "written by an SDK app";

/// What the workspace host this session runs keeps of the writer's record in the
/// profile's own workspace, opened with the channel's keys. `Err` says where the
/// write stops on the way there, so a write that never arrives is explained.
fn written(session: &Daemon) -> Result<Option<String>, String> {
    use automerge::ReadDoc as _;
    let workspace = WorkspaceId::parse(&session.workspace().map_err(|error| error.to_string())?)
        .ok_or_else(|| "the session names no workspace".to_owned())?;
    let record = ui::resource::DocumentId::for_file_document(
        uuid::Uuid::parse_str(WRITER_RECORD).map_err(|error| error.to_string())?,
    );
    let bytes = session
        .workspaces
        .test_kept_document(workspace, WRITER_APP, record.as_str())?;
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
pub(crate) fn wait_until_joined(manager: &mut ui::collaboration::CollaborationManager) {
    let started = Instant::now();
    loop {
        let _ = manager.poll();
        if manager.is_connected() {
            return;
        }
        if let Some(refusal) = manager.join_refusal() {
            panic!("the app's route was refused: {refusal}");
        }
        if let ui::collaboration::CollaborationState::Offline(reason) = manager.state() {
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
fn watch_for_stop(manager: &mut ui::collaboration::CollaborationManager) -> ! {
    loop {
        let _ = manager.poll();
        if let Some(ui::collaboration::CollaborationState::Offline(reason)) =
            manager.take_state_change()
        {
            panic!("the app stopped publishing its write: {reason}");
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
    // workspace and writes it once. The SDK holds the write until the runtime's
    // identity service hands out the route's keys, then seals and sends it.
    if std::env::var_os(WRITER_HELPER).is_some() {
        use automerge::transaction::Transactable as _;
        let checkout = ui::resource::DocumentCheckout::new(
            ui::resource::DocumentId::for_file_document(
                uuid::Uuid::parse_str(WRITER_RECORD).unwrap(),
            ),
            ui::resource::ResourceType::new(WRITER_KIND),
        );
        let mut manager = ui::collaboration::CollaborationManager::for_resource(&checkout);
        wait_until_joined(&mut manager);
        let mut doc = automerge::AutoCommit::new();
        doc.put(automerge::ROOT, "text", WRITTEN).unwrap();
        doc.commit();
        assert!(
            manager.send(ui::collaboration::CollabRequest::document_snapshot(
                doc.save()
            ))
        );
        watch_for_stop(&mut manager);
    }
    let Some(root) = isolated_root(WRITER_TEST) else {
        return;
    };
    let session = Daemon::start_in_dev_mode(&root, &mut |_| {}).unwrap();
    let log = std::fs::File::create(root.join("writer.log")).unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", WRITER_TEST, "--nocapture"])
        .env(WRITER_HELPER, "1")
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
/// this device takes one back the same way.
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

    grants.revoke(&workspace, &id);
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
        use tilo_workspace_service::apps::certificate_authority::CertificateAuthority as _;
        let placed = WorkspaceId::new();
        #[allow(
            clippy::significant_drop_tightening,
            reason = "the certificates read the workspaces through a weak reference while they are up"
        )]
        let (workspaces, _directory) = tilo_rt::app()
            .block_on(ServiceWorkspaces::test_hosting(placed))
            .unwrap();
        let origin = tilo_workspace_service::apps::policy_clock::OwnerPolicyClock::new()
            .unwrap()
            .new_origin();
        let certificates = Certificates {
            policy_time: origin.capture().unwrap(),
            workspaces: Arc::downgrade(&workspaces),
        };
        let workspace = uuid::Uuid::parse_str(&placed.to_string()).unwrap();
        let cert = workspaces.device_cert().clone();
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
        assert!(!certificates.governs(workspace, None), "no device governs by its certificate");
        drop(certificates);
    }

    phase runtime_original_policy_time_cannot_survive_origin_retirement;
    {
        use tilo_workspace_service::apps::certificate_authority::CertificateAuthority as _;
        let origin = Arc::new(
            tilo_workspace_service::apps::policy_clock::OwnerPolicyClock::new()
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
        let replacement = tilo_workspace_service::apps::policy_clock::OwnerPolicyClock::new()
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
    let endpoint = listen_endpoint(&key, port).unwrap();
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
    use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_DOCUMENT};
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
    let session = Daemon::start_in_dev_mode(&root, &mut |_| {}).unwrap();
    let socket = session.broker.app_socket().to_path_buf();
    let workspace = session.workspace().unwrap();
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
        // A write the shell sends with no keys: the daemon seals it with the group's keys and
        // signs it as the device. It waits while the device takes the keys in.
        let holds = shell_reads(&shell, &workspace, DESKTOP_APP, DESKTOP_DOCUMENT)
            .await
            .is_some();
        let mut layout = automerge::AutoCommit::new();
        automerge::transaction::Transactable::put(&mut layout, automerge::ROOT, "front", "shell")
            .unwrap();
        let written = tilo_test_env::within("the daemon takes the shell's write", async {
            loop {
                let reply = shell_asks(
                    &shell,
                    ShellCall::Write {
                        workspace: workspace.clone(),
                        app: DESKTOP_APP.to_owned(),
                        document: DESKTOP_DOCUMENT.to_owned(),
                        readers: None,
                        founding: !holds,
                        snapshot: true,
                        changes: layout.save(),
                    },
                )
                .await;
                match reply {
                    ShellReply::Written(Ok(written)) => return written,
                    ShellReply::Written(Err(refusal)) if refusal.contains("Waiting") => {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                    other => panic!("the daemon takes the shell's write: {other:?}"),
                }
            }
        })
        .await;
        assert_eq!(
            written.peer,
            neighbour_wire::peer_handle(&session.workspaces.device_cert().device_public_key),
            "the shell's write is signed as this device"
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
        let peer = session
            .workspaces
            .peer(WorkspaceId::parse(&workspace).unwrap())
            .unwrap();
        assert!(
            peer.publication()
                .is_sealed_document(DESKTOP_APP, DESKTOP_DOCUMENT),
            "the daemon sealed the shell's write with the layout's group keys"
        );
        // The resources of an app the shell runs itself, and the workspace's accounts.
        let counted = shell_asks(
            &shell,
            ShellCall::Resources {
                workspace: workspace.clone(),
                app: WRITER_APP.to_owned(),
                // `tilo_resource_store::wire::Command::Count`, as the shell sends it.
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
    let mut other = shell_child(&root, other, &format!("other:{}", socket.display()));
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
        let (command, log) = shell_child(&root, shell, "shell");
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
        .make(tilo_workspace_service::apps::shell::ShellCaller {
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
            .make(tilo_workspace_service::apps::shell::ShellCaller { person });
        let link = runtime.block_on(async {
            let mut stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
            stream.write_all(SHELL_MAGIC).await.unwrap();
            stream.write_all(&secret).await.unwrap();
            runtime_collab::ShellClient::open(stream).0
        });
        DeviceRecords::on(
            DaemonShell::new(Ok(link), runtime.handle().clone()),
            workspace.clone(),
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
    first.flush();
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
    next.flush();
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

/// `command` running [`SHELL_TEST`] as the child `role`, and the file its output goes to.
fn shell_child(
    root: &Path,
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
        .env(tilo_test_env::hang::HANG_BOUND_ENV, helper_hang_bound())
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::fs::File::create(&log).unwrap()))
        .stderr(Stdio::null());
    (command, log)
}

/// What the child of [`SHELL_TEST`] playing `role` checks.
#[cfg(unix)]
async fn shell_helper(role: &str) {
    use runtime_collab::wire::shell::{SHELL_MAGIC, SHELL_SECRET_BYTES, ShellCall, ShellReply};
    use tokio::io::AsyncWriteExt as _;
    if role == "shell" {
        let link = runtime_collab::ShellClient::inherited()
            .expect("the daemon handed its shell a link")
            .unwrap();
        let (shell, _) =
            runtime_collab::ShellClient::open(tokio::net::UnixStream::from_std(link).unwrap());
        let answer = shell.call(ShellCall::Workspaces).await;
        assert!(
            matches!(&answer, Ok(ShellReply::Workspaces(held)) if !held.is_empty()),
            "the shell the daemon started is answered: {answer:?}"
        );
        return;
    }
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
            document: String::new(),
            readers: None,
            founding: true,
            snapshot: true,
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
    // device key this computer holds.
    let dir = super::this_accounts_root_dir().unwrap();
    let record = tilo_identity::store::resolve_active_identity(&dir).unwrap();
    let identity = tilo_identity::store::load_from_path(record).unwrap();
    let keys = tilo_api::calls::device_keys();
    let (key, _) = tilo_device_identity::persist::load_or_create_device_key(&keys).unwrap();
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
            cert,
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
