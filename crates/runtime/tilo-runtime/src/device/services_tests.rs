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
    workspace.set_owner(actor.clone());
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
        .certified_local_action_provider(admission.caller(), admission.presentation())
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
        })),
        tickets: None,
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
    test_server::relay_through_loopback_in(&config, &data);
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
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
    super::found_a_person().unwrap();
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
    // Just started on a new profile, with nothing written yet: the host holds
    // the person's own documents store.
    holds_the_persons_own_documents_store(&session);
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
            let server = session.mcp_session_factory(session.workspace()).unwrap()().unwrap();
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
    drop(session);
    // The profile is released and served again with the same workspace.
    let reopened = Daemon::start_reporting(&root, &mut |_| {}).unwrap();
    assert_eq!(reopened.workspace(), listed_workspace());
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
    let workspace = WorkspaceId::parse(session.workspace())
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
            app: "so.tilo.thumbnails".into(),
            entrypoint: "thumbnail-batch".into(),
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
    let session_workspace = session.workspace().to_owned();
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

/// The workspace founded on this computer, as its device's directory keeps it.
fn listed_workspace() -> String {
    let place = tilo_device_identity::login::Place::here(tilo_api::client::device_keys()).unwrap();
    let mut kept = std::fs::read_dir(place.dir())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(FOUNDED))
        });
    let path = kept.next().expect("a workspace founded here");
    assert!(kept.next().is_none(), "one person founds here");
    std::fs::read_to_string(path).unwrap().trim().to_owned()
}

/// A running host holds the person's own documents store: asserted on the
/// host a test starts first on its new profile, before anything is written.
fn holds_the_persons_own_documents_store(session: &Daemon) {
    let identity = tilo_identity::store::identity_for_workspace(session.workspace())
        .unwrap()
        .unwrap();
    // The daemon acts as the person this computer holds, in the workspace it founded for them.
    assert_eq!(session.actor(), identity.person_id());
    let personal = session.personal();
    // The store opens on the identity the profile acts as, so every device of
    // this person finds the same documents without naming a workspace.
    assert_eq!(
        personal.read(tilo_peer_state::PersonalStore::root),
        identity.user_public_key
    );
    // Its key is the cluster every record of the person's own is routed
    // under: what an app's own record names, and the only thing two devices
    // of theirs need agree on.
    assert_eq!(
        personal.store_key(),
        collab_types::personal_cluster(&identity.user_public_key).to_string()
    );
    assert!(personal.rows().is_empty(), "nothing is written yet");
}

fn consent(
    workspace: &str,
    person: &str,
) -> tilo_workspace_service::apps::app_actions::AppActionConsentRequest {
    tilo_workspace_service::apps::app_actions::AppActionConsentRequest {
        handle: "question-1".into(),
        workspace: workspace.into(),
        person: person.into(),
        app_id: "so.tilo.caller".into(),
        agent_session: None,
        target_app: "so.tilo.target-app".into(),
        resource: "target.document".into(),
        action: "open".into(),
        resource_label: "Document".into(),
        action_label: "Open".into(),
        risk: tilo_actions::RiskLevel::default(),
        needs_confirmation: false,
        confirming: false,
        moderation: None,
        scope: None,
        draft: None,
        ask: None,
    }
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

/// A grant a person gives an app is a change to the workspace's member list: recorded by the
/// person the device acts for, who may grant there, in force once it is recorded, and taken
/// back the same way. Nobody else records one here.
#[test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the workspaces and their service stay up until the test ends"
)]
fn a_grant_is_a_change_to_the_member_list() {
    let placed = WorkspaceId::new();
    let (workspaces, directory) = tilo_rt::app()
        .block_on(ServiceWorkspaces::test_hosting(placed))
        .unwrap();
    let person = hex::encode(workspaces.device_cert().user_public_key);
    let grants = MemberListGrants {
        workspaces: Arc::clone(&workspaces),
        person: person.clone(),
        apps: Arc::new(crate::apps::Apps::from_env(
            directory.path(),
            &placed.to_string(),
        )),
    };
    let workspace = placed.to_string();
    assert!(grants.may_record(&workspace, &person), "the owner grants");
    assert!(!grants.may_record(&workspace, "stranger"));
    assert!(!grants.may_record(&WorkspaceId::new().to_string(), &person));

    let id = tilo_rt::app()
        .block_on(grants.record(
            &consent(&workspace, &person),
            app_grant(&workspace, &person),
        ))
        .unwrap()
        .expect("the owner's grant is recorded");
    assert!(
        granted(&workspaces, placed, &id),
        "the grant is in the member list once it is recorded"
    );
    let receipts = workspaces
        .workspace(placed)
        .map(|held| held.effect_receipts)
        .unwrap_or_default();
    assert!(
        receipts
            .iter()
            .any(|receipt| receipt.actor == tilo_actions::MemberId::person(person.clone())),
        "the workspace's history says who gave the grant: {receipts:?}"
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

/// A computer that logged in with a code holds no root: its login's device is what its apps sign
/// as, enrolled as this computer's identity when the daemon starts.
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
    let place = tilo_device_identity::login::Place::here(tilo_api::client::device_keys()).unwrap();
    tilo_device_identity::persist::adopt_device_key(place.keys(), &key.to_bytes()).unwrap();
    tilo_device_identity::login::store(
        &place,
        &Device {
            site: "https://tilo.test/".into(),
            host_origin: "https://eu1.tilo.test".into(),
            workspace: "6f1c2b9e-1c1f-4f0e-9a51-3c4d5e6f7a8b".into(),
            workspace_name: Some("Studio".into()),
            agent: "Claude Code".into(),
            server_cert: cert.clone(),
            cert: cert.clone(),
            server_address: None,
        },
    )
    .unwrap();
    assert!(
        tilo_identity::broker::trusted_current_device("").is_err(),
        "nothing signs before the daemon starts"
    );

    let identity = Identity::here().unwrap();
    assert_eq!(identity.cert, cert);
    let (signer, signed_for) = tilo_identity::broker::trusted_current_device("").unwrap();
    assert_eq!(signed_for, cert, "apps sign as the login's device");
    assert_eq!(signer.verifying_key(), key.verifying_key());
    // A second start finds it enrolled and changes nothing.
    assert_eq!(Identity::here().unwrap().cert, cert);
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
    let (mode, n0_dns, portmapper) = endpoint_reach(Some(here.clone()), &person);
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
    let (mode, n0_dns, portmapper) = endpoint_reach(Some(away.clone()), &person);
    assert_eq!(mode.relay_map().urls::<Vec<_>>(), vec![away]);
    assert!(!n0_dns);
    assert!(
        !matches!(portmapper, iroh::endpoint::PortmapperConfig::Disabled),
        "a server reached through a public relay keeps the portmapper, as any host at home does"
    );

    let (mode, n0_dns, _) = endpoint_reach(None, &person);
    assert!(mode.relay_map().is_empty(), "the person's relays, as set");
    assert!(n0_dns);
}
