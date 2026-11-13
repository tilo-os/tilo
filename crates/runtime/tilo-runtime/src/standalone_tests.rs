use super::workspace::{
    DESKTOP_WORKSPACE, INVOCATION_JOURNAL, ProfileWorkspace, STANDALONE_WORKSPACE, bootstrap,
    read_tree,
};
use super::*;
use serde_json::json;
use std::{
    process::Stdio,
    time::{Duration, Instant},
};
use tilo_action_broker::services::{
    LegacyWorkspaceProjection, WorkspaceAuthority, WorkspaceGrants,
};
use tilo_actions::remote::{InvokeArgument, PendingPoll, RemoteActionTarget};
use tilo_desktop_model::WorkspaceId;
use tilo_identity_host::broker::WorkspaceMembership;
use tilo_test_env::SpawnUnderEnvLock as _;

/// The test whose phase runs in a fresh process; libtest selects it by this
/// exact path.
const PROCESS_TEST: &str =
    "standalone::tests::sdk_apps_join_a_standalone_session_and_publish_to_mcp";
const PROCESS_ROOT: &str = "TILO_STANDALONE_TEST_ROOT";
const PUBLICATION_HELPER: &str = "TILO_STANDALONE_PUBLICATION_HELPER";

#[tokio::test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the broker fixture owns its authority and invocation registry for the whole scenario"
)]
#[allow(clippy::too_many_lines, reason = "one end-to-end ownership scenario")]
async fn runtime_invocation_ownership_survives_device_normalization() {
    use tilo_action_broker::{
        collab_app_broker::{AppBrokerState, PublishedAction, PublishedInput, PublishedSurface},
        collab_broker::CrdtRoute,
        test_support::{Workspace, WorkspaceSlot, WorkspaceTree},
    };
    if isolated_signing_identity(
        "standalone::tests::runtime_invocation_ownership_survives_device_normalization",
    )
    .is_none()
    {
        return;
    }

    let placed = WorkspaceId::new();
    let (joined, directory) = Joined::test_hosting(placed).await;
    let actor = tilo_action_broker::collab_app_broker::principal_id_for_root(
        joined.test_certificate().user_public_key,
    );
    let mut workspace = Workspace::in_workspace(WorkspaceSlot(41), placed, "Execution isolation");
    workspace.set_owner(actor.clone());
    let workspace_id = workspace.id().to_string();
    let mut tree = WorkspaceTree {
        persist_path: Some(directory.path().join("workspace.json")),
        ..WorkspaceTree::default()
    };
    tree.desktop.workspaces.push(workspace);
    let membership = tree.shared();
    let (_origin, time) = execution_policy_clock().unwrap();
    let admission = joined.admit_mcp_execution(&workspace_id, &actor).unwrap();
    let mut services = membership.services();
    services.native_certificates = Some(Arc::new(joined::admission_tests::FixtureAuthority {
        workspace: uuid::Uuid::parse_str(&workspace_id).unwrap(),
        cert: admission.certificate().clone(),
        time: time.clone(),
    }));
    let (broker, turn) = AppBrokerState::test_broker_with_authority_and_invoke_turn(
        membership.authority(),
        services,
    );
    let qualified = broker
        .certified_local_action_host(admission.caller(), admission.presentation())
        .unwrap();
    broker.replace_published_surfaces_for_test(vec![PublishedSurface {
        peer_id: 7,
        app_id: "so.tilo.files".into(),
        route: CrdtRoute::app("so.tilo.files", workspace_id.clone()),
        actions: vec![PublishedAction {
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
    let owner = Caller::local_person(actor)
        .in_workspace(&workspace_id)
        .in_new_execution();
    let host = RuntimeActionHost {
        broker,
        workspace: workspace_id.clone(),
        joined,
        session: Some(Arc::new(AdmittedRuntimeSession {
            policy_time: time,
            host: qualified,
            admission,
            agent: None,
            projection: Some(owner.clone()),
        })),
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
    };
    let mut pending = host.invoke(&owner, request.clone());
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
    assert!(host.observe(&owner, &id).is_some());
    assert!(host.observe(&other, &id).is_none());
    // Waiting on a change is authorized exactly as observing.
    assert!(host.next_change(&other, &id).is_none());
    let change = host
        .next_change(&owner, &id)
        .expect("the owner may wait on its invocation");
    assert_eq!(
        host.cancel(&other, &id)
            .wait_timeout(std::time::Duration::from_secs(30))
            .ok()
            .flatten()
            .expect("the host answers"),
        Err(ActionHostError::NotPermitted)
    );
    assert!(matches!(
        host.invoke(&owner, request.clone()).poll_ready(),
        PendingPoll::Ready(Ok(retry)) if retry == receipt
    ));
    assert!(matches!(
        host.invoke(&other, request).poll_ready(),
        PendingPoll::Ready(Err(ActionHostError::NotPermitted))
    ));
    assert!(
        invocations.try_recv().is_err(),
        "retries must not dispatch again"
    );
    assert_eq!(
        host.cancel(&owner, &id)
            .wait_timeout(std::time::Duration::from_secs(30))
            .ok()
            .flatten()
            .expect("the host answers"),
        Ok(())
    );
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
pub(super) fn isolated_root(test: &str) -> Option<PathBuf> {
    if let Some(root) = std::env::var_os(PROCESS_ROOT) {
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
pub(super) fn isolated_command(test: &str, root: &Path) -> std::process::Command {
    let environment = root.join("environment");
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", test, "--nocapture"])
        .env(PROCESS_ROOT, root.join("profile"))
        .env("HOME", environment.join("home"))
        .env("XDG_CONFIG_HOME", environment.join("config"))
        .env("XDG_DATA_HOME", environment.join("data"))
        .env("XDG_CACHE_HOME", environment.join("cache"))
        .env("XDG_STATE_HOME", environment.join("state"))
        .env("TILO_IDENTITY_ROOT", environment.join("identity"))
        .env(tilo_identity_host::provisioning::FILE_SECRETS_ENV, "1")
        .env_remove("TILO_SVC_COLLAB_FD")
        .env_remove("TILO_SVC_IDENTITY_FD");
    command
}

/// The home of the process [`isolated_command`] runs under `root`.
pub(super) fn isolated_home(root: &Path) -> PathBuf {
    root.join("environment").join("home")
}

/// Provision only the receipt signer's host custody in an isolated process.
/// The synthetic caller still uses its independently certified admission.
pub(super) fn isolated_signing_identity(test: &str) -> Option<PathBuf> {
    let root = isolated_root(test)?;
    let registry = tilo_identity_host::registry::IdentityRegistryStore::from_env().unwrap();
    tilo_identity_host::provisioning::ensure_collaboration_identity(
        registry.identity_dir(),
        &tilo_identity_host::provisioning::collaboration_identity_paths(),
        tilo_identity_host::provisioning::IdentityStorage::Files,
    )
    .unwrap();
    Some(root)
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
fn sdk_apps_join_a_standalone_session_and_publish_to_mcp() {
    // Helper role: an ordinary SDK app launched by the session publishes its
    // action directory and keeps its connection until it is killed.
    if std::env::var_os(PUBLICATION_HELPER).is_some() {
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
    let session = Standalone::start_reporting(&root, &mut |_| {}).unwrap();
    // Durable collaboration history and the invocation journal live in the
    // profile.
    assert!(root.join(COLLABORATION_HISTORY).is_dir());
    assert!(root.join(INVOCATION_JOURNAL).exists());
    assert!(
        Standalone::start_reporting(&root, &mut |_| {}).is_err(),
        "a second host must not serve a held profile"
    );
    let executable = std::env::current_exe().unwrap();
    let mut children = Vec::new();
    for index in 0..2 {
        let log = std::fs::File::create(root.join(format!("helper-{index}.log"))).unwrap();
        let mut command = std::process::Command::new(&executable);
        command
            .args(["--exact", PROCESS_TEST, "--nocapture"])
            .env(PUBLICATION_HELPER, "1")
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
                    "method": "tools/call",
                    "params": {"name": "tilo_list_apps", "arguments": {}}
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
    let apps = listed["result"]["structuredContent"]["apps"]
        .as_array()
        .unwrap_or_else(|| panic!("{listed}"));
    assert_eq!(apps.len(), 2, "{listed}");
    assert!(
        apps.iter().all(|app| app["app_id"] == "dev.tilo.same"),
        "{listed}"
    );
    drop(session);
    // The profile is released and served again with the same workspace.
    let reopened = Standalone::start_reporting(&root, &mut |_| {}).unwrap();
    assert_eq!(reopened.workspace(), listed_workspace(&root));
}

const WRITER_TEST: &str = "standalone::tests::an_sdk_apps_sealed_write_in_the_runtimes_own_workspace_is_durable_across_restart";
const WRITER_HELPER: &str = "TILO_STANDALONE_WRITER_HELPER";
const WRITER_APP: &str = "dev.tilo.writer";
const WRITER_KIND: &str = "dev.tilo.writer.note";
const WRITER_RECORD: &str = "0b7d9a3e-5c1f-4a8e-9d2b-7f6e5a4c3b21";
const WRITTEN: &str = "written by an SDK app";

/// What the workspace host this session runs keeps of the writer's record in the
/// profile's own workspace, opened with the channel's keys. `Err` says where the
/// write stops on the way there, so a write that never arrives is explained.
fn written(session: &Standalone) -> Result<Option<String>, String> {
    use automerge::ReadDoc as _;
    let workspace = WorkspaceId::parse(session.workspace())
        .ok_or_else(|| "the session names no workspace".to_owned())?;
    let record = ui::record::RecordId::for_file_record(
        uuid::Uuid::parse_str(WRITER_RECORD).map_err(|error| error.to_string())?,
    );
    let bytes = session
        .joined
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
        let checkout = ui::record::RecordCheckout::new(
            ui::record::RecordId::for_file_record(uuid::Uuid::parse_str(WRITER_RECORD).unwrap()),
            ui::record::RecordKind::new(WRITER_KIND),
        );
        let mut manager = ui::collaboration::CollaborationManager::for_record(&checkout);
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
    let session = Standalone::start_in_dev_mode(&root, &mut |_| {}).unwrap();
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
    let mut stopped = "the host keeps no body of the record".to_owned();
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
        "the sealed write reaches the workspace's host, which reports {stopped}: {logs}"
    );
    drop(session);
    // Served again, the profile's workspace host still keeps the write.
    let reopened = Standalone::start_in_dev_mode(&root, &mut |_| {}).unwrap();
    let again = written(&reopened);
    assert_eq!(
        again.as_ref().ok().and_then(|stored| stored.as_deref()),
        Some(WRITTEN),
        "the workspace host serves the write again: {again:?}"
    );
}

const OFFLOAD_TEST: &str =
    "standalone::tests::the_session_hands_its_broker_the_round_its_workspaces_devices_answer";

#[test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "each session holds the profile for its whole phase on purpose"
)]
fn the_session_hands_its_broker_the_round_its_workspaces_devices_answer() {
    let Some(root) = isolated_root(OFFLOAD_TEST) else {
        return;
    };
    // The job nothing here holds the inputs of: the round weighs no transfer,
    // and no other device of the workspace is there to answer it.
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

    // The broker holds the round: it asks the workspace's other devices and,
    // with none of them answering, runs the job here and says so.
    let session = Standalone::start_reporting(&root, &mut |_| {}).unwrap();
    let state = Arc::clone(session.broker.app_action_broker().unwrap().state());
    let route = state
        .action_offload()
        .expect("the session asks its workspaces' devices")
        .clone();
    let session_workspace = session.workspace().to_owned();
    // A job never run here has no run to beat, so the broker asks nobody and
    // runs it here.
    assert_eq!(
        run_here(&route, &session_workspace),
        "nothing is quicker than a run it knows nothing of"
    );
    // Having run it here, there is a run to beat: the broker asks the
    // workspace's other devices, and with none of them answering runs it here.
    let ran = request(&session_workspace);
    let tilo_actions::offload::Offloaded::RunHere { .. } = tilo_rt::app()
        .handle()
        .block_on(route.offload_invocation(ran.clone(), invocation()))
    else {
        panic!("the first run is here");
    };
    route.ran_here(&ran, Duration::from_secs(10));
    assert_eq!(
        run_here(&route, &session_workspace),
        "no device offered to run it"
    );
    drop(route);
    drop(state);
    drop(session);

    // Turned off, nothing is routed at all.
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(crate::offload::OFFLOAD_ENV, "off");
    let session = Standalone::start_reporting(&root, &mut |_| {}).unwrap();
    let state = Arc::clone(session.broker.app_action_broker().unwrap().state());
    assert!(state.action_offload().is_none(), "offload is off: no route");
    drop(state);
    drop(session);
}

fn listed_workspace(root: &Path) -> String {
    let tree = read_tree(&root.join(STANDALONE_WORKSPACE)).unwrap();
    tree.workspaces[0].id().to_string()
}

const PERSONAL_STORE_TEST: &str =
    "standalone::tests::a_running_host_holds_the_persons_own_documents_store";

#[test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the session holds the profile and brokers for the whole scenario on purpose"
)]
fn a_running_host_holds_the_persons_own_documents_store() {
    let Some(root) = isolated_root(PERSONAL_STORE_TEST) else {
        return;
    };
    let session = Standalone::start_reporting(&root, &mut |_| {}).unwrap();
    let identity = tilo_identity_host::registry::identity_for_workspace(session.workspace())
        .unwrap()
        .unwrap();
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

#[test]
fn a_new_profile_bootstraps_one_owned_workspace_and_keeps_it() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = ProfileWorkspace::open(temp.path()).unwrap();
    let workspace_id = workspace.workspace();
    assert!(!workspace_id.is_empty());
    workspace.adopt("person-1").unwrap();
    let path = temp.path().join(STANDALONE_WORKSPACE);
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    let reopened = ProfileWorkspace::open(temp.path()).unwrap();
    assert_eq!(reopened.workspace(), workspace_id);
    let tree = reopened.snapshot().unwrap();
    assert_eq!(tree.workspaces.len(), 1);
    assert_eq!(
        tree.workspaces[0].owner().map(String::as_str),
        Some("person-1")
    );
    assert_eq!(tree.workspaces[0].strips.len(), 1);
    // An owner is kept, not replaced by the next actor.
    reopened.adopt("person-2").unwrap();
    let tree = ProfileWorkspace::open(temp.path())
        .unwrap()
        .snapshot()
        .unwrap();
    assert_eq!(
        tree.workspaces[0].owner().map(String::as_str),
        Some("person-1")
    );
    let mut seen = None;
    WorkspaceMembership::with_workspace(&reopened, &tree.workspaces[0].id(), &mut |workspace| {
        seen = Some(workspace.name.clone());
    });
    assert_eq!(
        seen.as_deref(),
        Some(""),
        "a workspace nobody named keeps no name of its own"
    );
    WorkspaceMembership::with_workspace(&reopened, &WorkspaceId::new(), &mut |_| {
        panic!("no such workspace");
    });
}

#[test]
fn the_first_desk_a_profile_starts_on_is_the_one_its_next_start_reads() {
    let temp = tempfile::tempdir().unwrap();
    let first = ProfileWorkspace::open(temp.path()).unwrap();
    let made = first.snapshot().unwrap();
    assert!(!first.workspace().is_empty());
    // The next start reads the desk this one made, rather than making another
    // with fresh ids: what the person opened stays in the workspace it was
    // opened in.
    let second = ProfileWorkspace::open(temp.path()).unwrap();
    let read = second.snapshot().unwrap();
    assert_eq!(second.workspace(), first.workspace());
    assert_eq!(read.workspaces.len(), 1, "one desk, not one per start");
    assert_eq!(read.workspaces[0].slot, made.workspaces[0].slot);
    assert_eq!(
        read.workspaces[0].strips[0].id,
        made.workspaces[0].strips[0].id
    );
}

#[test]
fn effect_records_persist_in_the_profile() {
    let temp = tempfile::tempdir().unwrap();
    let tree = bootstrap();
    let workspace_id = tree.workspaces[0].id().to_string();
    std::fs::write(
        temp.path().join(STANDALONE_WORKSPACE),
        serde_json::to_vec_pretty(&tree).unwrap(),
    )
    .unwrap();
    let workspace = ProfileWorkspace::open(temp.path()).unwrap();

    let invocation = tilo_actions::InvocationId(uuid::Uuid::from_u128(1));
    let context = tilo_actions::ActionContext::default().with_described(
        tilo_actions::Described::new(tilo_actions::Did::verb(
            tilo_actions::ActionKey::new("org.tilo.Mail", "mail.message", "send"),
            [],
        )),
    );
    let status = tilo_actions::EffectReceiptStatus::Succeeded;
    for _ in 0..2 {
        workspace
            .record_effect(
                &workspace_id,
                invocation,
                "agent-1",
                context.clone(),
                status.clone(),
            )
            .unwrap();
    }
    assert!(
        workspace
            .record_effect(
                &WorkspaceId::new().to_string(),
                invocation,
                "a",
                context,
                status
            )
            .is_err()
    );

    let reopened = ProfileWorkspace::open(temp.path())
        .unwrap()
        .snapshot()
        .unwrap();
    let workspace = &reopened.workspaces[0];
    assert_eq!(
        workspace.effect_receipts.len(),
        1,
        "a retry keeps one receipt"
    );
    assert_eq!(workspace.effect_receipts[0].invocation, invocation);
}

#[test]
fn the_desktop_sessions_workspace_refuses_records() {
    let temp = tempfile::tempdir().unwrap();
    let mut tree = bootstrap();
    tree.workspaces[0].set_owner("desktop-owner".to_owned());
    let workspace_id = tree.workspaces[0].id().to_string();
    let bytes = serde_json::to_vec_pretty(&tree).unwrap();
    let desktop = temp.path().join(DESKTOP_WORKSPACE);
    std::fs::write(&desktop, &bytes).unwrap();
    let workspace = ProfileWorkspace::open(temp.path()).unwrap();

    let invocation = tilo_actions::InvocationId(uuid::Uuid::from_u128(1));
    let status = tilo_actions::EffectReceiptStatus::Succeeded;
    assert!(
        workspace
            .record_effect(
                &workspace_id,
                invocation,
                "agent-1",
                tilo_actions::ActionContext::default(),
                status
            )
            .is_err()
    );
    assert_eq!(std::fs::read(&desktop).unwrap(), bytes, "never rewritten");
    let tree = workspace.snapshot().unwrap();
    assert!(tree.workspaces[0].effect_receipts.is_empty());
    assert!(!temp.path().join(STANDALONE_WORKSPACE).exists());
}

#[test]
fn the_desktop_sessions_workspace_is_served_read_only() {
    let temp = tempfile::tempdir().unwrap();
    let mut tree = bootstrap();
    tree.workspaces[0].set_owner("desktop-owner".to_owned());
    let mut value = serde_json::to_value(&tree).unwrap();
    // The desktop session's file carries its native layout and fields.
    value["schema_version"] = json!(3);
    value["native_authority"] = json!({"kept": true});
    let bytes = serde_json::to_vec_pretty(&value).unwrap();
    let desktop = temp.path().join(DESKTOP_WORKSPACE);
    std::fs::write(&desktop, &bytes).unwrap();
    let workspace = ProfileWorkspace::open(temp.path()).unwrap();
    assert_eq!(workspace.workspace(), tree.workspaces[0].id().to_string());
    workspace.adopt("someone-else").unwrap();
    assert_eq!(std::fs::read(&desktop).unwrap(), bytes, "never rewritten");
    assert!(!temp.path().join(STANDALONE_WORKSPACE).exists());
    assert_eq!(
        workspace.snapshot().unwrap().workspaces[0]
            .owner()
            .map(String::as_str),
        Some("desktop-owner")
    );

    // Without an owner there is nobody this host could act as.
    value["workspaces"][0]["policy"]["owner"] = serde_json::Value::Null;
    std::fs::write(&desktop, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(
        ProfileWorkspace::open(temp.path())
            .unwrap()
            .adopt("someone-else")
            .is_err()
    );
    // A file that is not a workspace tree fails closed.
    std::fs::write(&desktop, b"not json").unwrap();
    assert_eq!(
        ProfileWorkspace::open(temp.path()).err().unwrap().kind(),
        io::ErrorKind::InvalidData
    );
}

fn consent(
    workspace: &str,
    person: &str,
) -> tilo_action_broker::app_actions::AppActionConsentRequest {
    tilo_action_broker::app_actions::AppActionConsentRequest {
        handle: "question-1".into(),
        workspace: workspace.into(),
        person: person.into(),
        app_id: "so.tilo.caller".into(),
        target_app: "so.tilo.target-app".into(),
        resource: "target.document".into(),
        action: "open".into(),
        resource_label: "Document".into(),
        action_label: "Open".into(),
        risk: tilo_actions::RiskLevel::default(),
        needs_confirmation: false,
        confirming: false,
        moderation: None,
        preview: None,
        scope: None,
        draft: None,
    }
}

fn app_grant(workspace: &str) -> tilo_action_policy::Grant {
    tilo_action_policy::Grant::new(
        "caller-opens-target",
        tilo_action_policy::Grantee::person("owner"),
        tilo_action_policy::TreePath::workspace(workspace)
            .app("so.tilo.target-app")
            .any(tilo_action_policy::Level::Kind)
            .resource("target.document")
            .action("open"),
        tilo_action_policy::KindSet::ALL,
        tilo_action_policy::Answer::Allow,
        tilo_action_policy::Grantee::person("owner"),
    )
}

fn grants(workspace: ProfileWorkspace, root: &Path) -> ProfileGrants {
    ProfileGrants {
        workspace: Arc::new(workspace),
        apps: Arc::new(apps::Apps::new(None, root, "", PathBuf::new(), Vec::new())),
    }
}

#[test]
fn grants_in_the_runtimes_own_workspace_are_recorded_by_its_administrator_and_saved() {
    use tilo_action_broker::app_actions::AppActionConsent;
    let temp = tempfile::tempdir().unwrap();
    let workspace = ProfileWorkspace::open(temp.path()).unwrap();
    workspace.adopt("owner").unwrap();
    let workspace_id = workspace.workspace();
    let grants = grants(workspace, temp.path());

    let recorded = |person: &str| {
        tilo_rt::app()
            .block_on(grants.record(
                &consent(&workspace_id, person),
                app_grant(&workspace_id),
                AppActionConsent::Always,
            ))
            .unwrap()
    };
    assert!(recorded("stranger").is_err());

    let id = recorded("owner").unwrap();
    let saved = read_tree(&temp.path().join(STANDALONE_WORKSPACE)).unwrap();
    let workspace = &saved.workspaces[0];
    assert!(
        workspace
            .policy
            .grants
            .iter()
            .any(|grant| grant.id.as_str() == id)
    );
    let receipt = workspace.effect_receipts.last().unwrap();
    assert_eq!(receipt.actor, "owner");
    let Some(tilo_actions::Did::Verb(verb)) = &receipt.context.what else {
        panic!("the grant is recorded as a verb: {:?}", receipt.context);
    };
    assert_eq!(verb.args["app"].as_str(), Some("Caller"));
    assert_eq!(verb.args["target"].as_str(), Some("Target App"));
    assert_eq!(verb.args["lifetime"].as_str(), Some("workspace"));

    grants.revoke(&workspace_id, &id);
    let saved = read_tree(&temp.path().join(STANDALONE_WORKSPACE)).unwrap();
    assert!(
        saved.workspaces[0]
            .policy
            .grants
            .iter()
            .all(|grant| grant.id.as_str() != id)
    );
}

#[test]
fn the_desktop_sessions_workspace_takes_no_grants_here() {
    use tilo_action_broker::app_actions::AppActionConsent;
    let temp = tempfile::tempdir().unwrap();
    let mut tree = bootstrap();
    tree.workspaces[0].set_owner("owner".to_owned());
    let workspace_id = tree.workspaces[0].id().to_string();
    let bytes = serde_json::to_vec_pretty(&tree).unwrap();
    let desktop = temp.path().join(DESKTOP_WORKSPACE);
    std::fs::write(&desktop, &bytes).unwrap();
    let grants = grants(ProfileWorkspace::open(temp.path()).unwrap(), temp.path());

    assert!(
        tilo_rt::app()
            .handle()
            .block_on(grants.record(
                &consent(&workspace_id, "owner"),
                app_grant(&workspace_id),
                AppActionConsent::Always
            ))
            .is_err()
    );
    grants.revoke(&workspace_id, "owner-1");
    assert_eq!(std::fs::read(&desktop).unwrap(), bytes, "never rewritten");
    assert!(!temp.path().join(STANDALONE_WORKSPACE).exists());
    assert!(
        grants.workspace.snapshot().unwrap().workspaces[0]
            .policy
            .grants
            .iter()
            .all(|grant| grant.id.as_str() != "caller-opens-target"),
        "nothing recorded here"
    );
}

#[test]
fn the_runtime_governs_only_its_own_workspace_from_its_own_device() {
    use tilo_action_broker::certificate_authority::CertificateAuthority as _;
    let workspace = uuid::Uuid::from_u128(7);
    let origin = Arc::new(
        tilo_action_broker::policy_clock::OwnerPolicyClock::new()
            .unwrap()
            .new_origin(),
    );
    let certificates = Certificates {
        workspace: workspace.to_string(),
        device: [2; 32],
        policy_time: origin.capture().unwrap(),
        joined: std::sync::Weak::new(),
    };
    assert!(certificates.governs(workspace, Some(&[2; 32])));
    assert!(certificates.governs(workspace, None));
    assert!(!certificates.governs(workspace, Some(&[3; 32])));
    assert!(!certificates.governs(uuid::Uuid::from_u128(8), None));
    drop(certificates);
}

#[test]
fn runtime_original_policy_time_cannot_survive_origin_retirement() {
    use tilo_action_broker::certificate_authority::CertificateAuthority as _;
    let origin = Arc::new(
        tilo_action_broker::policy_clock::OwnerPolicyClock::new()
            .unwrap()
            .new_origin(),
    );
    let certificates = Certificates {
        workspace: uuid::Uuid::from_u128(7).to_string(),
        device: [2; 32],
        policy_time: origin.capture().unwrap(),
        joined: std::sync::Weak::new(),
    };
    let retained = certificates.policy_time().unwrap().unwrap();
    origin.invalidate();
    assert!(certificates.policy_time().is_err());
    drop(certificates);
    assert!(retained.now_ns().is_err());
    let replacement = tilo_action_broker::policy_clock::OwnerPolicyClock::new()
        .unwrap()
        .new_origin();
    assert!(!retained.same_origin(&replacement.capture().unwrap()));
    assert!(
        retained.now_ns().is_err(),
        "a new runtime clock cannot revive the original custody"
    );
}
