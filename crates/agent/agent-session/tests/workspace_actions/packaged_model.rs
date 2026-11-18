//! An agent applies an installed app's actions on a server that has nothing
//! of the app compiled in: the workspace actions apply the app version's
//! model from its package, admit and publish what it wrote under the agent's
//! own actor, and another holder of the resource replays it to the same heads.
//!
//! Tasks is the app, installed into a workspace the server governs and keeps
//! sealed. The server's catalogue of apps is empty: the only Tasks code in
//! reach is the module its `.tapp` carries.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard; one scenario in order"
)]

use super::*;
use tilo_actions::remote::Caller;
use tilo_app_package::{Manifest, SigningIdentity, pack, tapp};
use tilo_peer_state::workspace_membership::{AppRelease, WorkspaceMembershipCommand};
use tilo_server_protocol::WorkspaceId;
use ui::tilo_resource::Resource as _;

/// The app the test installs, which the server does not have compiled in.
const TASKS: &str = "so.tilo.tasks";
/// The resource the app keeps per workspace.
const TRACKER: &str = "tasks.tracker";
/// The signed surface the Tasks app ships: its verbs, and how each is decided.
const TASKS_SURFACE: &[u8] = include_bytes!("../../../../../apps/tasks/surface.toml");

/// The Tasks model module, built for wasm32 by `tilo-app model` beside the app
/// (`apps/tasks-model-wasm`). A tree keeps no built module, so the check that
/// builds them keeps it under the target directory (`t outputs app-models`).
fn tasks_model() -> Vec<u8> {
    let directory = std::env::var_os("TILO_APP_MODELS").map_or_else(
        || {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../..")
                .join("target/app-models")
        },
        std::path::PathBuf::from,
    );
    let module = directory.join("tasks").join(tilo_app_package::MODEL_MODULE);
    let asked = format!(
        "reading {}; build it with `t outputs app-models`",
        module.display()
    );
    std::fs::read(&module).expect(&asked)
}

/// The app's package: the model module and the surface file at its payload
/// root, signed.
fn package(app: &str, model: &[u8], surface: &[u8]) -> Vec<u8> {
    let scratch = tempfile::tempdir().unwrap();
    let payload = scratch.path().join("payload");
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    std::fs::write(payload.join("bin/tasks"), b"the app's window").unwrap();
    std::fs::write(payload.join(tilo_app_package::MODEL_MODULE), model).unwrap();
    std::fs::write(payload.join("surface.toml"), surface).unwrap();
    let identity = SigningIdentity::generate();
    let manifest = Manifest::from_toml_str(&format!(
        r#"
[package]
id = "{app}"
version = "1.0.0"
kind = "app"

[publisher]
identity = "ed25519:{}"
signature = ""

[sandbox]
namespaces = ["user"]
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
bin = "bin/tasks"
type = "gui"
"#,
        hex::encode(identity.public_key_bytes())
    ))
    .unwrap();
    let packed = pack(&payload, &manifest, &identity).unwrap();
    let source: &dyn tilo_app_package::BlobSource = &packed;
    tapp::to_bytes(&packed.meta, Some(source)).unwrap()
}

/// `name` as the action API binds it.
fn create_area(name: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut arguments = serde_json::Map::new();
    arguments.insert("name".into(), serde_json::Value::String(name.into()));
    arguments
}

/// The provider a resource's actions are asked at.
fn provider(route: &DocumentRoute) -> String {
    format!(
        "{}:{}:{}",
        tilo_actions::remote::RESOURCE_PROVIDER,
        route.app_id,
        route.document
    )
}

/// The members that wrote the changes of `document`.
fn invokers(document: &[u8]) -> Vec<tilo_actions::ActionInvoker> {
    automerge::AutoCommit::load(document)
        .unwrap()
        .get_changes(&[])
        .iter()
        .filter_map(|change| change.message())
        .filter_map(tilo_actions::ActionContext::from_change_message)
        .filter_map(|context| context.invoker)
        .collect()
}

/// The receipt a served invocation settled with: a service still settling the
/// workspace is asked again, and an invocation accepted while it saves is
/// observed until it ends.
fn settled(
    actions: &tilo_agent_session::workspace_actions::WorkspaceActions,
    workspace: WorkspaceId,
    caller: &Caller,
    request: &InvokeRequest,
) -> ReceiptStatus {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let mut attempt = request.clone();
        attempt.invocation = InvocationId::new();
        let mut status = match actions
            .serve(
                workspace,
                caller,
                RemoteActionMessage::Invoke(attempt.clone()),
            )
            .unwrap()
            .wait_timeout(tilo_test_env::hang_bound())
            .ok()
            .flatten()
            .unwrap()
        {
            RemoteActionMessage::Receipt(receipt) => receipt.status,
            other => panic!("expected a receipt, got {other:?}"),
        };
        while matches!(status, ReceiptStatus::Accepted) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
            status = match actions
                .serve(
                    workspace,
                    caller,
                    RemoteActionMessage::observe(attempt.invocation),
                )
                .unwrap()
                .wait_timeout(tilo_test_env::hang_bound())
                .ok()
                .flatten()
                .unwrap()
            {
                RemoteActionMessage::Update {
                    state: Some(state), ..
                } => state.receipt().status,
                other => panic!("expected an observation, got {other:?}"),
            };
        }
        let again = matches!(
            &status,
            ReceiptStatus::Rejected {
                error: ActionCallError::Unavailable { .. }
            }
        ) || matches!(&status, ReceiptStatus::Failed { error } if error.retryable);
        if !again || Instant::now() >= deadline {
            return status;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
#[ignore = "reads the model module the app-models check built (t test test-packaged-model)"]
fn an_agent_applies_a_packaged_apps_action_on_a_server_that_never_built_it() {
    // The invocation's origin clock is the wall clock, which the replay has to
    // read again: this test moves its process's clock and takes it to itself.
    tilo_test_env::in_own_process(
        module_path!(),
        "an_agent_applies_a_packaged_apps_action_on_a_server_that_never_built_it",
        || {
            let process = tilo_clock::testing::OwnProcess::of_a_test_in_its_own_process();
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .enable_all()
                .build()
                .expect("a runtime for this test alone")
                .block_on(the_packaged_action(process));
        },
    );
}

async fn the_packaged_action(process: tilo_clock::testing::OwnProcess) {
    // The clock the server reads for the invocation, held for every thread.
    let moment = tilo_clock::wall()
        .checked_add(std::time::Duration::from_secs(60))
        .expect("a wall time a minute on");
    let now_ms = u64::try_from(
        moment
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let _held = tilo_clock::testing::hold_wall_everywhere(process, moment);
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let server_device = enrol(&root, "server", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    // The server's catalogue is empty: nothing of Tasks is compiled in.
    let (server, actions) = crate::harness::hosted::serving_documents_with_actions(
        &root,
        &server_device,
        state.path(),
        || tilo_agent_session::AppCatalogue::new(Vec::new()),
    )
    .await;
    let workspaces = server.workspaces().unwrap().clone();
    // What a server's start does: the surfaces its changes are decided
    // against, kept under its state.
    tilo_server::surfaces::decide_receipt(&state.path().join("surfaces"));
    let created = workspaces.create("Packaged Tasks").await.unwrap();
    let model = tasks_model();
    let package = package(TASKS, &model, TASKS_SURFACE);
    // The workspace's server keeps the package file its governance names, as
    // an upload would leave it.
    let file = blake3::hash(&package).to_hex().to_string();
    let kept = state
        .path()
        .join("server/workspaces/app-files")
        .join(&created.workspace);
    std::fs::create_dir_all(&kept).unwrap();
    std::fs::write(kept.join(format!("{file}.tapp")), &package).unwrap();
    workspaces
        .govern_app(
            &created.workspace,
            &server_device.cert,
            [7; 32],
            WorkspaceMembershipCommand::InstallApp(AppRelease {
                app: TASKS.into(),
                version: "1.0.0".into(),
                surface: blake3::hash(TASKS_SURFACE).to_hex().to_string(),
                package: blake3::hash(&package).to_hex().to_string(),
                publisher: hex::encode(server_device.cert.user_public_key),
                file: Some(file),
                dev: true,
            }),
        )
        .await
        .unwrap();
    assert!(
        workspaces
            .prepare_packaged_application(
                &created.workspace,
                &server_device.cert,
                TASKS,
                "",
                &package,
            )
            .await
            .unwrap(),
        "the server makes the app's resource from its package"
    );
    let before = workspaces
        .resource_document(&created.workspace, TASKS, "")
        .await
        .unwrap()
        .expect("the workspace holds the resource the module made");

    // The agent the server's own session was handed to: its person's device,
    // acting as the agent, as tilo-mcp does on a server.
    let caller = Caller::authenticated(
        root.verifying_key().to_bytes(),
        server_device.cert.device_public_key,
    )
    .delegated_to("assistant")
    .named("Claude Code")
    .in_workspace(&created.workspace)
    .in_new_execution();
    let route = DocumentRoute {
        app_id: TASKS.into(),
        cluster_id: created.workspace.clone(),
        document: String::new(),
    };
    let request = request(
        &provider(&route),
        ActionAddress::new(TRACKER, "create-area"),
        InvokeArgument::Json(create_area("Ship it")),
    );
    let acting = caller.acting().clone();
    let status = settled(&actions, created.workspace_id, &caller, &request);
    assert!(
        matches!(status, ReceiptStatus::Completed { .. }),
        "the agent's action is applied from the package: {status:?}"
    );

    let after = workspaces
        .resource_document(&created.workspace, TASKS, "")
        .await
        .unwrap()
        .expect("the document still exists");
    let mut document = automerge::AutoCommit::load(&after).unwrap();
    let tracker = tasks_model::Tracker::read(&document).unwrap();
    assert!(
        tracker
            .areas
            .iter()
            .any(|(_, area)| area.name.as_str() == "Ship it"),
        "the tracker holds the area the agent made"
    );
    // History names the agent, and the person whose device handed it the
    // action, never the governor's person as the actor.
    let wrote = invokers(&after);
    let person = hex::encode(root.verifying_key().to_bytes());
    assert!(
        wrote.iter().any(|invoker| {
            invoker.acting.agent_id() == Some("assistant")
                && invoker.acting.acted_for_person() == Some(person.as_str())
        }),
        "the change is the agent's, for its person: {wrote:?}"
    );
    // The resource is sealed with the workspace's keys: the server holding
    // them writes it as the workspace does.
    assert!(
        workspaces
            .state(&created.workspace)
            .await
            .unwrap()
            .publication()
            .is_sealed_document(TASKS, ""),
        "the packaged resource is sealed"
    );

    // Another holder of the resource replays the invocation with the same
    // module, as the same actor and at the same origin clock: the change it
    // writes is the change the server published.
    let replayed = tilo_model_engine::Model::compile(&model)
        .unwrap()
        .ask(&tilo_model_engine::Request {
            op: tilo_model_engine::Op::Apply,
            id: "",
            home: &created.workspace,
            bytes: &before,
            member: "agent:assistant",
            acting: Some(serde_json::to_value(&acting).unwrap()),
            action: "create-area",
            arguments: create_area("Ship it"),
            now: i64::try_from(now_ms).unwrap(),
        })
        .unwrap();
    let mut replayed_document = automerge::AutoCommit::load(&before).unwrap();
    tilo_automerge::ApplyIncoming::load_incoming(
        &mut replayed_document,
        &replayed
            .changes
            .expect("the module wrote the action's change"),
    )
    .unwrap();
    assert_eq!(
        replayed_document.get_heads(),
        document.get_heads(),
        "the same module replays the action to the same document"
    );
    server.shutdown().await;
}
