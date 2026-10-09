use super::workspace::{
    DESKTOP_WORKSPACE, INVOCATION_JOURNAL, ProfileWorkspace, STANDALONE_WORKSPACE, bootstrap,
    read_tree,
};
use super::*;
use crate::mcp::McpServer;
use serde_json::json;
use std::{
    process::Stdio,
    time::{Duration, Instant},
};
use tilo_action_broker::services::{
    AppActionGrants, LegacyWorkspaceProjection, WorkspaceAuthority,
};
use tilo_desktop_model::{RoomId, WorkspaceTaskStatus};
use tilo_identity_host::broker::WorkspaceMembership;

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
async fn runtime_invocation_ownership_survives_device_normalization() {
    use tilo_action_broker::{
        collab_broker::CrdtRoute,
        collab_broker_capnp::{
            CapnpBrokerState, PublishedAction, PublishedInput, PublishedSurface,
        },
        test_support::{Workspace, WorkspaceId, WorkspaceTree},
    };
    use tilo_actions::remote::{InvokeArgument, PendingPoll, RemoteActionTarget};

    let mut workspace = Workspace::new(WorkspaceId(41), "Execution isolation");
    workspace.set_owner("a1".repeat(32));
    let room = workspace.room.to_string();
    let mut tree = WorkspaceTree::default();
    tree.desktop.workspaces.push(workspace);
    let membership = tree.shared();
    let broker =
        CapnpBrokerState::test_broker_with_authority(membership.authority(), membership.services());
    broker.replace_published_surfaces_for_test(vec![PublishedSurface {
        peer_id: 7,
        app_id: "so.tilo.files".into(),
        route: CrdtRoute::app("so.tilo.files", room.clone()),
        actions: vec![PublishedAction {
            semantic_identity: None,
            resource: "files.entry:/tmp/synthetic-report".into(),
            action: "refresh".into(),
            resource_kind: "files.entry".into(),
            resource_label: "Synthetic report".into(),
            action_label: "Refresh".into(),
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
    let (joined, _directory) = Joined::test_empty().await;
    let host = RuntimeActionHost {
        broker,
        room: room.clone(),
        joined,
        session: None,
    };
    let owner = Caller::local_person("a1".repeat(32))
        .in_room(room)
        .in_new_execution();
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
    };
    let PendingPoll::Ready(Ok(receipt)) = host.invoke(&owner, request.clone()).poll_ready() else {
        panic!("the synthetic app must accept the invocation");
    };
    assert!(invocations.try_recv().is_ok());
    assert!(host.observe(&owner, &id).is_some());
    assert!(host.observe(&other, &id).is_none());
    assert_eq!(
        host.cancel(&other, &id),
        Err(ActionHostError::UnknownInvocation)
    );
    assert!(matches!(
        host.invoke(&owner, request.clone()).poll_ready(),
        PendingPoll::Ready(Ok(retry)) if retry == receipt
    ));
    assert!(matches!(
        host.invoke(&other, request).poll_ready(),
        PendingPoll::Ready(Err(ActionHostError::InvocationConflict))
    ));
    assert!(
        invocations.try_recv().is_err(),
        "retries must not dispatch again"
    );
    assert_eq!(host.cancel(&owner, &id), Ok(()));
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
    let environment = root.join("environment");
    let log = root.join("child.stdout");
    let status = std::process::Command::new(std::env::current_exe().unwrap())
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
        .env_remove("TILO_SVC_IDENTITY_FD")
        .stdout(std::fs::File::create(&log).unwrap())
        .status()
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
            let mut server = McpServer::new(
                session.action_host().unwrap(),
                session.room().to_owned(),
                session.actor().to_owned(),
            );
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
    assert_eq!(reopened.room(), listed_room(&root));
}

const WRITER_TEST: &str = "standalone::tests::an_sdk_apps_sealed_write_in_the_runtimes_own_room_is_durable_across_restart";
const WRITER_HELPER: &str = "TILO_STANDALONE_WRITER_HELPER";
const WRITER_APP: &str = "dev.tilo.writer";
const WRITER_KIND: &str = "dev.tilo.writer.note";
const WRITER_RECORD: &str = "0b7d9a3e-5c1f-4a8e-9d2b-7f6e5a4c3b21";
const WRITTEN: &str = "written by an SDK app";

/// What the runtime's own room owner holds of the writer's record, read
/// from the profile as a fresh owner restores it.
fn written(profile: &Path) -> Option<String> {
    use automerge::ReadDoc as _;
    let workspace = Arc::new(ProfileWorkspace::open(profile).ok()?);
    let room = workspace.room();
    let owner = owner::RoomOwner::open(
        profile,
        &room,
        tilo_identity_host::broker::trusted_current_device(&room).ok()?,
        owner::RecordCreation {
            declared_kinds: Box::new(|_| None),
            dev: true,
        },
        workspace,
        tilo_action_broker::channel_encryption::ChannelKeyProvider::default(),
    )
    .ok()?;
    let record = ui::record::RecordId::for_file_record(uuid::Uuid::parse_str(WRITER_RECORD).ok()?);
    assert!(
        owner.document(WRITER_APP, record.as_str()).is_none()
            || owner.encrypted(WRITER_APP, record.as_str()),
        "the SDK's write arrived sealed"
    );
    let bytes = owner.document(WRITER_APP, record.as_str())?;
    let doc = automerge::AutoCommit::load(&bytes).ok()?;
    doc.get(automerge::ROOT, "text")
        .ok()?
        .and_then(|(value, _)| value.into_string().ok())
}

#[test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the session holds the profile and brokers for the whole scenario on purpose"
)]
fn an_sdk_apps_sealed_write_in_the_runtimes_own_room_is_durable_across_restart() {
    // Helper role: an ordinary SDK app joins one record of the runtime's
    // room and writes it once. The SDK holds the write until the runtime's
    // identity service hands out the route's keys, then seals and sends it.
    if std::env::var_os(WRITER_HELPER).is_some() {
        use automerge::transaction::Transactable as _;
        let checkout = ui::record::RecordCheckout::new(
            ui::record::RecordId::for_file_record(uuid::Uuid::parse_str(WRITER_RECORD).unwrap()),
            ui::record::RecordKind::new(WRITER_KIND),
        );
        let mut manager = ui::collaboration::CollaborationManager::for_record(&checkout);
        let started = Instant::now();
        while !manager.is_connected() && started.elapsed() < tilo_test_env::hang_bound() {
            let _ = manager.poll();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(manager.is_connected(), "{:?}", manager.state());
        let mut doc = automerge::AutoCommit::new();
        doc.put(automerge::ROOT, "text", WRITTEN).unwrap();
        doc.commit();
        assert!(
            manager.send(ui::collaboration::CollabRequest::document_snapshot(
                doc.save()
            ))
        );
        loop {
            let _ = manager.poll();
            std::thread::sleep(Duration::from_millis(10));
        }
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
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone().unwrap()))
        .stderr(Stdio::from(log));
    let mut child = session.launch(WRITER_APP, command).unwrap();
    let deadline = tilo_test_env::hang_deadline();
    let stored = loop {
        let stored = written(&root);
        if stored.is_some() || Instant::now() >= deadline {
            break stored;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let _ = child.kill();
    let _ = child.wait();
    let logs = std::fs::read_to_string(root.join("writer.log")).unwrap_or_default();
    assert_eq!(
        stored.as_deref(),
        Some(WRITTEN),
        "the sealed write reaches the owner: {logs}"
    );
    drop(session);
    // Served again, the profile's owner still holds the write.
    drop(Standalone::start_in_dev_mode(&root, &mut |_| {}).unwrap());
    assert_eq!(written(&root).as_deref(), Some(WRITTEN));
}

fn listed_room(root: &Path) -> String {
    let tree = read_tree(&root.join(STANDALONE_WORKSPACE)).unwrap();
    tree.workspaces[0].room.to_string()
}

#[test]
fn a_new_profile_bootstraps_one_owned_workspace_and_keeps_it() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = ProfileWorkspace::open(temp.path()).unwrap();
    let room = workspace.room();
    assert!(!room.is_empty());
    workspace.adopt("person-1").unwrap();
    let path = temp.path().join(STANDALONE_WORKSPACE);
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    let reopened = ProfileWorkspace::open(temp.path()).unwrap();
    assert_eq!(reopened.room(), room);
    let tree = reopened.snapshot().unwrap();
    assert_eq!(tree.workspaces.len(), 1);
    assert_eq!(tree.workspaces[0].owner.as_deref(), Some("person-1"));
    assert_eq!(tree.workspaces[0].strips.len(), 1);
    // An owner is kept, not replaced by the next actor.
    reopened.adopt("person-2").unwrap();
    let tree = ProfileWorkspace::open(temp.path())
        .unwrap()
        .snapshot()
        .unwrap();
    assert_eq!(tree.workspaces[0].owner.as_deref(), Some("person-1"));
    let mut seen = None;
    reopened.with_workspace(&tree.workspaces[0].room, &mut |workspace| {
        seen = Some(workspace.name.clone());
    });
    assert_eq!(seen.as_deref(), Some("Home"));
    reopened.with_workspace(&RoomId::new(), &mut |_| panic!("no such room"));
}

fn task(id: &str) -> tilo_desktop_model::WorkspaceTask {
    tilo_desktop_model::WorkspaceTask {
        id: id.into(),
        title: "Draft".into(),
        created_by: "person-1".into(),
        assigned_agent: None,
        route: tilo_desktop_model::WorkspaceTaskRoute::Manual,
        status: WorkspaceTaskStatus::Queued,
        resource: None,
        created_at: 1,
        updated_at: 1,
        audit: Vec::new(),
    }
}

#[test]
fn effect_and_task_records_persist_in_the_profile() {
    let temp = tempfile::tempdir().unwrap();
    let mut tree = bootstrap();
    tree.workspaces[0].tasks.push(task("task-1"));
    let room = tree.workspaces[0].room.to_string();
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
                &room,
                invocation,
                "agent-1",
                context.clone(),
                status.clone(),
            )
            .unwrap();
    }
    workspace
        .record_task_update(
            &room,
            "task-1",
            "agent-1",
            Some(WorkspaceTaskStatus::Done),
            "Wrote the outline".into(),
        )
        .unwrap();
    assert!(
        workspace
            .record_task_update(&room, "absent", "agent-1", None, "x".into())
            .is_err()
    );
    assert!(
        workspace
            .record_effect(&RoomId::new().to_string(), invocation, "a", context, status)
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
    let task = &workspace.tasks[0];
    assert_eq!(task.status, WorkspaceTaskStatus::Done);
    let summaries: Vec<_> = task.audit.iter().map(|event| &*event.summary).collect();
    assert_eq!(
        summaries,
        [
            format!("Status · {}", WorkspaceTaskStatus::Done.label()).as_str(),
            "Wrote the outline"
        ]
    );
}

#[test]
fn the_desktop_sessions_workspace_refuses_records() {
    let temp = tempfile::tempdir().unwrap();
    let mut tree = bootstrap();
    tree.workspaces[0].set_owner("desktop-owner".to_owned());
    tree.workspaces[0].tasks.push(task("task-1"));
    let room = tree.workspaces[0].room.to_string();
    let bytes = serde_json::to_vec_pretty(&tree).unwrap();
    let desktop = temp.path().join(DESKTOP_WORKSPACE);
    std::fs::write(&desktop, &bytes).unwrap();
    let workspace = ProfileWorkspace::open(temp.path()).unwrap();

    let invocation = tilo_actions::InvocationId(uuid::Uuid::from_u128(1));
    let status = tilo_actions::EffectReceiptStatus::Succeeded;
    assert!(
        workspace
            .record_effect(
                &room,
                invocation,
                "agent-1",
                tilo_actions::ActionContext::default(),
                status
            )
            .is_err()
    );
    assert!(
        workspace
            .record_task_update(&room, "task-1", "agent-1", None, "x".into())
            .is_err()
    );
    assert_eq!(std::fs::read(&desktop).unwrap(), bytes, "never rewritten");
    let tree = workspace.snapshot().unwrap();
    assert!(tree.workspaces[0].effect_receipts.is_empty());
    assert!(tree.workspaces[0].tasks[0].audit.is_empty());
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
    assert_eq!(workspace.room(), tree.workspaces[0].room.to_string());
    workspace.adopt("someone-else").unwrap();
    assert_eq!(std::fs::read(&desktop).unwrap(), bytes, "never rewritten");
    assert!(!temp.path().join(STANDALONE_WORKSPACE).exists());
    assert_eq!(
        workspace.snapshot().unwrap().workspaces[0].owner.as_deref(),
        Some("desktop-owner")
    );

    // Without an owner there is nobody this host could act as.
    value["workspaces"][0]["owner"] = serde_json::Value::Null;
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

fn consent(room: &str, person: &str) -> tilo_action_broker::app_actions::AppActionConsentRequest {
    tilo_action_broker::app_actions::AppActionConsentRequest {
        handle: "question-1".into(),
        room: room.into(),
        person: person.into(),
        app_id: "so.tilo.caller".into(),
        target_app: "so.tilo.target-app".into(),
        resource: "target.document".into(),
        action: "open".into(),
        resource_label: "Document".into(),
        action_label: "Open".into(),
        risk: tilo_actions::RiskLevel::default(),
        needs_confirmation: false,
    }
}

fn app_grant() -> collab_types::caps::CapOverride {
    use collab_types::caps::{CapOverride, Scope, Subject, Verb};
    CapOverride::grant(
        Subject::app("so.tilo.caller"),
        Scope::Action {
            app: "so.tilo.target-app".into(),
            resource: "target.document".into(),
            action: "open".into(),
        },
        Verb::Write,
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
    let room = workspace.room();
    let grants = grants(workspace, temp.path());

    assert!(grants.may_grant(&room, "owner"));
    assert!(!grants.may_grant(&room, "stranger"));
    assert!(!grants.may_grant(&RoomId::new().to_string(), "owner"));
    assert!(
        grants
            .record(
                &consent(&room, "stranger"),
                app_grant(),
                AppActionConsent::Always
            )
            .is_none()
    );

    let id = grants
        .record(
            &consent(&room, "owner"),
            app_grant(),
            AppActionConsent::Always,
        )
        .unwrap();
    let saved = read_tree(&temp.path().join(STANDALONE_WORKSPACE)).unwrap();
    let workspace = &saved.workspaces[0];
    assert!(workspace.capabilities.iter().any(|cap| cap.id == id));
    let receipt = workspace.effect_receipts.last().unwrap();
    assert_eq!(receipt.actor, "owner");
    let Some(tilo_actions::Did::Verb(verb)) = &receipt.context.what else {
        panic!("the grant is recorded as a verb: {:?}", receipt.context);
    };
    assert_eq!(verb.args["app"].as_str(), Some("Caller"));
    assert_eq!(verb.args["target"].as_str(), Some("Target App"));
    assert_eq!(verb.args["lifetime"].as_str(), Some("workspace"));

    grants.revoke(&room, &id);
    let saved = read_tree(&temp.path().join(STANDALONE_WORKSPACE)).unwrap();
    assert!(
        saved.workspaces[0]
            .capabilities
            .iter()
            .all(|cap| cap.id != id)
    );
}

#[test]
fn the_desktop_sessions_workspace_takes_no_grants_here() {
    use tilo_action_broker::app_actions::AppActionConsent;
    let temp = tempfile::tempdir().unwrap();
    let mut tree = bootstrap();
    tree.workspaces[0].set_owner("owner".to_owned());
    let room = tree.workspaces[0].room.to_string();
    let bytes = serde_json::to_vec_pretty(&tree).unwrap();
    let desktop = temp.path().join(DESKTOP_WORKSPACE);
    std::fs::write(&desktop, &bytes).unwrap();
    let grants = grants(ProfileWorkspace::open(temp.path()).unwrap(), temp.path());

    assert!(!grants.may_grant(&room, "owner"));
    assert!(
        grants
            .record(
                &consent(&room, "owner"),
                app_grant(),
                AppActionConsent::Always
            )
            .is_none()
    );
    grants.revoke(&room, "owner-1");
    assert_eq!(std::fs::read(&desktop).unwrap(), bytes, "never rewritten");
    assert!(!temp.path().join(STANDALONE_WORKSPACE).exists());
    assert_eq!(
        grants.workspace.snapshot().unwrap().workspaces[0]
            .capabilities
            .len(),
        1,
        "only the owner's membership"
    );
}

#[test]
fn the_runtime_governs_only_its_own_room_from_its_own_device() {
    use tilo_action_broker::peer_actions::CertificateAuthority as _;
    let room = uuid::Uuid::from_u128(7);
    let origin = Arc::new(
        tilo_action_broker::peer_actions::OwnerPolicyClock::new()
            .unwrap()
            .new_origin(),
    );
    let certificates = Certificates {
        room: room.to_string(),
        device: [2; 32],
        policy_time: origin.capture().unwrap(),
        joined: std::sync::Weak::new(),
    };
    assert!(certificates.governs(room, Some(&[2; 32])));
    assert!(certificates.governs(room, None));
    assert!(!certificates.governs(room, Some(&[3; 32])));
    assert!(!certificates.governs(uuid::Uuid::from_u128(8), None));
    drop(certificates);
}

#[test]
fn runtime_original_policy_time_cannot_survive_origin_retirement() {
    use tilo_action_broker::peer_actions::CertificateAuthority as _;
    let origin = Arc::new(
        tilo_action_broker::peer_actions::OwnerPolicyClock::new()
            .unwrap()
            .new_origin(),
    );
    let certificates = Certificates {
        room: uuid::Uuid::from_u128(7).to_string(),
        device: [2; 32],
        policy_time: origin.capture().unwrap(),
        joined: std::sync::Weak::new(),
    };
    let retained = certificates.policy_time().unwrap().unwrap();
    origin.invalidate();
    assert!(certificates.policy_time().is_err());
    drop(certificates);
    assert!(retained.now_ns().is_err());
    let replacement = tilo_action_broker::peer_actions::OwnerPolicyClock::new()
        .unwrap()
        .new_origin();
    assert!(!retained.same_origin(&replacement.capture().unwrap()));
    assert!(
        retained.now_ns().is_err(),
        "a new runtime clock cannot revive the original custody"
    );
}
