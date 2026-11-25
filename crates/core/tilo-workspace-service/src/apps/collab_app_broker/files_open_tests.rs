//! `files#open` given content: who may put a file into a workspace's own folder, and what the
//! call answers. The tree itself is the service's (`files::OpenedFiles`), which its own tests
//! take through a real workspace; here the dispatch is what is checked.

#![allow(
    clippy::significant_drop_tightening,
    reason = "fixtures keep the recorder while asserting on it"
)]

use std::sync::Mutex;

use serde_json::{Map, Value, json};
use tilo_action_policy::TreePath;
use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX, ActionCallError, Caller, InvokeArgument,
    InvokeRequest, RemoteActionTarget,
};
use tilo_actions::{ActionAddress, InvocationId, InvocationUpdate};

use crate::apps::services::{FilesTreePuts, ProcessServices};
use crate::apps::test_support::{Grantee, Grants as _, KindSet, Role, WorkspaceId, WorkspaceTree};

use super::*;

fn owner_id() -> String {
    "a1".repeat(32)
}

/// What reached the workspace's folder.
#[derive(Default)]
struct Recorder(Mutex<Vec<(String, Map<String, Value>)>>);

impl FilesTreePuts for Recorder {
    fn put(
        &self,
        workspace: WorkspaceId,
        arguments: &Map<String, Value>,
        _acting: Option<tilo_actions::ActionContext>,
        confirm: &dyn Fn() -> bool,
    ) -> Result<Value, String> {
        if !confirm() {
            return Err("the caller may no longer write the workspace's files".to_owned());
        }
        self.0
            .lock()
            .unwrap()
            .push((workspace.to_string(), arguments.clone()));
        Ok(json!({
            "path": "Inbox/plan.md",
            "content": arguments.get("content").cloned().unwrap_or_default(),
            "size": 5,
            "replaced": false,
        }))
    }
}

struct Fixture {
    state: Arc<AppBrokerState>,
    recorder: Arc<Recorder>,
    workspace: String,
    _home: tempfile::TempDir,
}

/// A workspace its owner governs, with a member who may read but not write its files, and an
/// agent the workspace lets write the folder.
fn fixture() -> Fixture {
    let home = tempfile::tempdir().unwrap();
    let mut workspace = Workspace::new(WorkspaceId::from_u128(71), "Files open");
    workspace.policy.set_owner(owner_id());
    workspace.policy.add_member("ada", Role::Commenter);
    let id = workspace.id().to_string();
    let records = collab_types::document_node(
        &id,
        fs_protocol::tree::FILES_APP,
        &fs_protocol::tree::document_id(),
    );
    workspace.allow(
        Grantee::agent("porter", owner_id()),
        &TreePath::root(),
        KindSet::READ,
    );
    workspace.allow(
        Grantee::agent("porter", owner_id()),
        &records,
        KindSet::READ_UNDOABLE,
    );
    for verb in [
        fs_protocol::file_action::verbs::NEW_FOLDER,
        fs_protocol::file_action::verbs::NEW_FILE,
        fs_protocol::file_action::verbs::WRITE,
    ] {
        let node =
            TreePath::workspace(&id).joined(&fs_protocol::file_action::verb_node(None, verb));
        workspace.allow(
            Grantee::agent("porter", owner_id()),
            &node,
            KindSet::READ_UNDOABLE,
        );
    }
    let mut tree = WorkspaceTree::default();
    tree.desktop.workspaces.push(workspace);
    tree.persist_path = Some(home.path().join("state.json"));
    let membership = tree.shared();
    let recorder = Arc::new(Recorder::default());
    let services = ProcessServices {
        files_open: Some(Arc::clone(&recorder) as Arc<dyn FilesTreePuts>),
        ..membership.services()
    };
    let broker = AppBrokerState::test_broker_with_authority(membership.authority(), services);
    let state = Arc::clone(broker.state());
    Fixture {
        state,
        recorder,
        workspace: id,
        _home: home,
    }
}

/// The owner, acting as themselves.
fn owner(workspace: &str) -> Caller {
    Caller::local_person(owner_id())
        .in_workspace(workspace)
        .in_new_execution()
}

/// An agent `id` acting for this device's person, in `workspace`.
fn agent(id: &str, workspace: &str) -> Caller {
    Caller::local_person(owner_id())
        .delegated_to(id)
        .in_workspace(workspace)
        .in_new_execution()
}

/// The workspace's `open` of Files, given `arguments`.
fn open(workspace: &str, arguments: &Value) -> InvokeRequest {
    InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: tilo_actions::remote::workspace_provider(workspace),
            address: ActionAddress::new(
                format!(
                    "{APPLICATION_RESOURCE_PREFIX}{}",
                    fs_protocol::tree::FILES_APP
                ),
                APPLICATION_OPEN_ACTION,
            ),
        },
        argument: InvokeArgument::Json(arguments.as_object().cloned().unwrap_or_default()),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// Wait for `pending` with this test's thread.
async fn settled<T>(mut pending: tilo_actions::remote::Pending<T>) -> T {
    loop {
        match pending.poll_ready() {
            tilo_actions::remote::PendingPoll::Ready(value) => return value,
            tilo_actions::remote::PendingPoll::Waiting => tokio::task::yield_now().await,
            tilo_actions::remote::PendingPoll::Abandoned => panic!("the call was abandoned"),
        }
    }
}

/// The output the put `request` ends with, once it ends.
async fn finished(state: &Arc<AppBrokerState>, caller: &Caller, request: &InvokeRequest) -> Value {
    loop {
        if let Some(seen) = state.action_observe(caller, &request.invocation) {
            match seen.update {
                InvocationUpdate::Succeeded { output, .. } => {
                    return output.value.unwrap_or_default();
                }
                InvocationUpdate::Failed { error, .. } => panic!("the put failed: {error:?}"),
                InvocationUpdate::Cancelled { .. } => panic!("the put was cancelled"),
                _ => {}
            }
        }
        tokio::task::yield_now().await;
    }
}

/// The person's own open puts the file and answers where it is; a member who may only comment
/// and an agent the workspace lets read alone are refused, as is one naming no workspace.
#[tokio::test]
async fn a_person_puts_a_file_and_a_caller_the_workspace_does_not_let_is_refused() {
    let fixture = fixture();
    let caller = owner(&fixture.workspace);
    let request = open(
        &fixture.workspace,
        &json!({"content": "tilo.content:abc", "name": "plan.md", "dir": "Inbox"}),
    );
    tilo_test_env::within(
        "the person's open is admitted",
        settled(fixture.state.action_invoke(&caller, None, None, &request)),
    )
    .await
    .expect("the person's open of their own workspace is admitted");
    let answer = tilo_test_env::within(
        "the person's put into the workspace's folder completes",
        finished(&fixture.state, &caller, &request),
    )
    .await;
    assert_eq!(answer["path"], "Inbox/plan.md", "{answer}");
    {
        let reached = fixture.recorder.0.lock().unwrap();
        assert_eq!(reached.len(), 1, "one put reached the folder");
        assert_eq!(reached[0].0, fixture.workspace);
        assert_eq!(
            reached[0].1.get("name").and_then(Value::as_str),
            Some("plan.md")
        );
    }

    // An agent the workspace lets write the folder puts as its person's device does.
    let porter = agent("porter", &fixture.workspace);
    let request = open(
        &fixture.workspace,
        &json!({"content": "tilo.content:abc", "name": "plan.md"}),
    );
    settled(fixture.state.action_invoke(&porter, None, None, &request))
        .await
        .expect("the workspace lets the agent write its files");
    let answer = tilo_test_env::within(
        "the agent's put into the workspace's folder completes",
        finished(&fixture.state, &porter, &request),
    )
    .await;
    assert_eq!(answer["path"], "Inbox/plan.md", "{answer}");
    assert_eq!(fixture.recorder.0.lock().unwrap().len(), 2);

    // A member who may read the workspace writes none of its files.
    let commenter = Caller::local_person("ada")
        .in_workspace(&fixture.workspace)
        .in_new_execution();
    let request = open(
        &fixture.workspace,
        &json!({"content": "tilo.content:abc", "name": "plan.md"}),
    );
    assert!(matches!(
        settled(
            fixture
                .state
                .action_invoke(&commenter, None, None, &request)
        )
        .await,
        Err(ActionCallError::NotPermitted)
    ));

    assert_eq!(
        fixture.recorder.0.lock().unwrap().len(),
        2,
        "nothing was put for a refused caller"
    );
}

/// Files' open of nothing is the app's open, which this does not answer: the call is looked up
/// as an app action, and no put reaches the folder.
#[tokio::test]
async fn an_open_of_nothing_is_no_put() {
    let fixture = fixture();
    let caller = owner(&fixture.workspace);
    let request = open(&fixture.workspace, &json!({}));
    assert!(
        settled(fixture.state.action_invoke(&caller, None, None, &request))
            .await
            .is_err(),
        "this host installs no Files app to start"
    );
    assert!(
        fixture.recorder.0.lock().unwrap().is_empty(),
        "no put reached the folder"
    );
}
