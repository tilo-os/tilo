//! `files#open` given content: who may put a file into a workspace's own folder, and what the
//! call answers; and what the workspace's listing offers on that folder, its `list` and
//! `read-text`, called as its rows offer them. The tree itself is the service's
//! (`files::OpenedFiles`), which its own tests take through a real workspace; here the dispatch
//! is what is checked.

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

/// The person's root.
fn root() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[1; 32])
}

fn owner_id() -> String {
    hex::encode(root().verifying_key().to_bytes())
}

/// `request` as the key its person's root certified for the agent `id` in `workspace` signed it.
fn signed_by_agent(id: &str, workspace: &str, mut request: InvokeRequest) -> InvokeRequest {
    let key = ed25519_dalek::SigningKey::from_bytes(&[51; 32]);
    let now = tilo_clock::unix_now_ns();
    let workspace_id = *WorkspaceId::parse(workspace).unwrap().as_uuid().as_bytes();
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: root().verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: id.to_owned(),
        capabilities: tilo_user_identity::DeviceCapabilities::agent_grant(
            tilo_user_identity::AgentScope::agent_in(workspace_id, id),
        ),
        issued_at_ns: now.saturating_sub(1),
        expires_at_ns: Some(now.saturating_add(3_600_000_000_000)),
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, &root()).unwrap();
    request.signed = Some(Box::new(
        tilo_server_protocol::invocation::sign_invocation_now(workspace, &request, &cert, &key, now)
            .unwrap(),
    ));
    request
}

/// One put that reached the workspace's folder: the workspace, the open's arguments, and the
/// context its change was to be made in.
type Reached = (
    String,
    Map<String, Value>,
    Option<tilo_actions::ActionContext>,
);

/// What reached the workspace's folder: the puts, the reads, each its action and arguments, and
/// the changes, each its verb and what a put records.
#[derive(Default)]
struct Recorder(
    Mutex<Vec<Reached>>,
    Mutex<Vec<(String, Map<String, Value>)>>,
    Mutex<Vec<(String, Reached)>>,
);

/// What the recorder's workspace folder holds: a folder `Inbox` with one file in it.
const HELD: [(&str, bool); 2] = [("Inbox", true), ("Inbox/plan.md", false)];

impl FilesTreePuts for Recorder {
    fn put(
        &self,
        workspace: WorkspaceId,
        arguments: &Map<String, Value>,
        call: Option<crate::apps::services::FilesPutCall<'_>>,
        confirm: &dyn Fn() -> bool,
    ) -> Result<Value, String> {
        if !confirm() {
            return Err("the caller may no longer write the workspace's files".to_owned());
        }
        self.0.lock().unwrap().push((
            workspace.to_string(),
            arguments.clone(),
            call.map(|call| call.context),
        ));
        Ok(json!({
            "path": "Inbox/plan.md",
            "content": arguments.get("content").cloned().unwrap_or_default(),
            "size": 5,
            "replaced": false,
        }))
    }

    fn entries(
        &self,
        _workspace: WorkspaceId,
        folder: &str,
        below: bool,
    ) -> Result<Vec<crate::apps::services::FilesEntry>, String> {
        Ok(HELD
            .into_iter()
            .filter(|(path, _)| {
                let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
                parent == folder || (below && path.starts_with(folder))
            })
            .map(|(path, folder)| crate::apps::services::FilesEntry {
                path: path.to_owned(),
                folder,
            })
            .collect())
    }

    fn read(
        &self,
        _workspace: WorkspaceId,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Value, String> {
        self.1
            .lock()
            .unwrap()
            .push((action.to_owned(), arguments.clone()));
        Ok(json!({"read": action}))
    }

    fn change(
        &self,
        workspace: WorkspaceId,
        verb: &str,
        arguments: &Map<String, Value>,
        call: Option<crate::apps::services::FilesPutCall<'_>>,
        confirm: &dyn Fn() -> bool,
    ) -> Result<Value, String> {
        if !confirm() {
            return Err("the caller may no longer change the workspace's files".to_owned());
        }
        let context = call.map(|call| call.context);
        let invocation = context
            .as_ref()
            .and_then(|context| context.source_invocation)
            .map(|invocation| invocation.0.to_string());
        self.2.lock().unwrap().push((
            verb.to_owned(),
            (workspace.to_string(), arguments.clone(), context),
        ));
        Ok(json!({"action": verb, "entries": [], "made": [], "invocation": invocation}))
    }
}

struct Fixture {
    state: Arc<AppBrokerState>,
    recorder: Arc<Recorder>,
    workspace: String,
    _home: tempfile::TempDir,
}

/// A workspace with an owner, and a member who may read but not write its files, and an
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
    // A member who edits the workspace, whom it keeps from its folder's trash.
    workspace.policy.add_member("bea", Role::Editor);
    workspace.deny(
        Grantee::person("bea"),
        &fs_protocol::file_action::verb_node(None, fs_protocol::file_action::verbs::TRASH),
        KindSet::UNDOABLE,
    );
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
    files_call(workspace, APPLICATION_OPEN_ACTION, arguments)
}

/// Files' `action` on the workspace's own folder, given `arguments`.
fn files_call(workspace: &str, action: &str, arguments: &Value) -> InvokeRequest {
    InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: tilo_actions::remote::workspace_provider(workspace),
            address: ActionAddress::new(
                format!(
                    "{APPLICATION_RESOURCE_PREFIX}{}",
                    fs_protocol::tree::FILES_APP
                ),
                action,
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

/// The rows the workspace's listing offers `caller` beneath `parent`, the top when `None`.
fn listed(
    state: &Arc<AppBrokerState>,
    caller: &Caller,
    workspace: &str,
    parent: Option<&str>,
) -> Vec<tilo_actions::remote::CatalogueRow> {
    state
        .action_query(
            caller,
            &tilo_actions::remote::CatalogueQuery {
                provider: Some(tilo_actions::remote::workspace_provider(workspace)),
                parent_resource: parent.map(str::to_owned),
                max_results: tilo_actions::remote::MAX_CATALOGUE_PAGE,
                ..tilo_actions::remote::CatalogueQuery::default()
            },
        )
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten()
        .expect("the broker answers")
        .expect("the listing answers")
        .rows
}

/// The call of what `row` offers, as a caller going through the listing makes it: its own
/// address, with nothing besides.
fn offered(row: &tilo_actions::remote::CatalogueRow) -> InvokeRequest {
    let action = row.action.as_ref().expect("the row offers an action");
    InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: row.provider.clone(),
            address: action.address.clone(),
        },
        argument: InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
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
        let acting = reached[0].2.as_ref().expect("the change names who made it");
        assert_eq!(acting.source_invocation, Some(request.invocation));
        assert_eq!(
            acting.invoker.as_ref().map(|invoker| &invoker.acting),
            Some(caller.acting()),
            "the person's change is theirs"
        );
    }

    // An agent asking under no key of its own puts nothing: every device that takes the files
    // checks who asked against the call's signature.
    let porter = agent("porter", &fixture.workspace);
    let keyless = open(
        &fixture.workspace,
        &json!({"content": "tilo.content:abc", "name": "plan.md"}),
    );
    match settled(fixture.state.action_invoke(&porter, None, None, &keyless)).await {
        Err(ActionCallError::Refused { reason }) => assert_eq!(reason, KEYLESS_AGENT),
        other => panic!("a keyless agent's put is refused with why: {other:?}"),
    }
    assert_eq!(fixture.recorder.0.lock().unwrap().len(), 1, "nothing was put");

    // An agent the workspace lets write the folder puts the open it signed with its own key.
    let request = signed_by_agent(
        "porter",
        &fixture.workspace,
        open(
            &fixture.workspace,
            &json!({"content": "tilo.content:abc", "name": "plan.md"}),
        ),
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
    {
        let reached = fixture.recorder.0.lock().unwrap();
        assert_eq!(reached.len(), 2);
        let acting = reached[1].2.as_ref().expect("the change names who made it");
        let invoker = acting.invoker.as_ref().expect("an invoker");
        assert_eq!(
            &invoker.acting,
            porter.acting(),
            "the agent's change names the agent, never only its person"
        );
        assert!(invoker.acting.is_agent());
    }

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

/// What the workspace's listing offers on its folder is answered, each called as its row offers
/// it: the folder's `list` reads what the workspace's own folder holds, a folder's `list` what
/// that folder does and a file's `read-text` what it says, the path named by the row's own
/// resource. A person who is no member of the workspace is refused, and nothing is read.
#[tokio::test]
async fn what_the_listing_offers_on_the_folder_is_answered_as_offered() {
    let fixture = fixture();
    let caller = owner(&fixture.workspace);
    let folder = format!(
        "{APPLICATION_RESOURCE_PREFIX}{}",
        fs_protocol::tree::FILES_APP
    );
    let top = listed(&fixture.state, &caller, &fixture.workspace, None);
    let files = top
        .iter()
        .find(|row| row.resource.id == folder)
        .unwrap_or_else(|| panic!("the top of the listing offers Files: {top:?}"));
    let beneath = listed(&fixture.state, &caller, &fixture.workspace, Some(&folder));
    let row = |path: &str| {
        beneath
            .iter()
            .find(|row| row.resource.label == path)
            .unwrap_or_else(|| panic!("the folder lists {path}: {beneath:?}"))
    };
    let calls = [
        (files, fs_protocol::tree::LIST, json!({})),
        (
            row("Inbox"),
            fs_protocol::tree::LIST,
            json!({"folder": "Inbox"}),
        ),
        (
            row("Inbox/plan.md"),
            fs_protocol::tree::READ_TEXT,
            json!({"file": "Inbox/plan.md"}),
        ),
    ];
    for (at, (row, action, arguments)) in calls.iter().enumerate() {
        let request = offered(row);
        assert_eq!(request.target.address.action, *action, "{row:?}");
        tilo_test_env::within(
            "the person's read of their folder is admitted",
            settled(fixture.state.action_invoke(&caller, None, None, &request)),
        )
        .await
        .unwrap_or_else(|error| panic!("{action} of {:?} is answered: {error:?}", row.resource));
        let answer = tilo_test_env::within(
            "the read of the workspace's folder completes",
            finished(&fixture.state, &caller, &request),
        )
        .await;
        assert_eq!(
            answer,
            json!({"read": action}),
            "the folder's answer is the call's"
        );
        let reads = fixture.recorder.1.lock().unwrap();
        assert_eq!(reads.len(), at + 1, "one read reached the folder");
        assert_eq!(reads[at].0, *action);
        assert_eq!(
            Value::Object(reads[at].1.clone()),
            *arguments,
            "the row's resource names what is read"
        );
    }

    // A person who is no member of the workspace reads none of its files.
    let stranger = Caller::local_person("b2".repeat(32))
        .in_workspace(&fixture.workspace)
        .in_new_execution();
    let request = offered(files);
    assert!(
        settled(fixture.state.action_invoke(&stranger, None, None, &request))
            .await
            .is_err(),
        "a person who is no member is refused"
    );
    assert_eq!(
        fixture.recorder.1.lock().unwrap().len(),
        calls.len(),
        "nothing was read for a refused caller"
    );
}

/// Files' verbs on the folder by paths in it reach the tree in the caller's context, each decided
/// by the tree's verbs it is carried out as: an agent the workspace lets make folders makes one,
/// an editor it keeps from the trash makes one and is refused the trash, the person takes a call
/// back, and a member who may only comment changes nothing, nor does a call naming another
/// workspace.
#[tokio::test]
async fn a_verb_by_path_is_decided_by_the_tree_verbs_it_is_carried_out_as() {
    let fixture = fixture();
    let invoke = |caller: Caller, request: InvokeRequest| {
        let state = Arc::clone(&fixture.state);
        async move {
            let admitted = settled(state.action_invoke(&caller, None, None, &request)).await;
            match admitted {
                Ok(_) => Ok(tilo_test_env::within(
                    "the change of the workspace's folder completes",
                    finished(&state, &caller, &request),
                )
                .await),
                Err(error) => Err(error),
            }
        }
    };

    let porter = agent("porter", &fixture.workspace);
    let made = files_call(
        &fixture.workspace,
        "new-folder",
        &json!({"parent": "Tasks", "name": "12 Plan"}),
    );
    let answer = invoke(porter.clone(), made.clone())
        .await
        .expect("the workspace lets the agent make folders");
    assert_eq!(
        answer["invocation"],
        made.invocation.0.to_string(),
        "the answer names the call that undo takes back: {answer}"
    );
    {
        let reached = fixture.recorder.2.lock().unwrap();
        assert_eq!(reached.len(), 1);
        let (verb, (workspace, arguments, acting)) = &reached[0];
        assert_eq!(verb, "new-folder");
        assert_eq!(workspace, &fixture.workspace);
        assert_eq!(arguments.get("parent"), Some(&json!("Tasks")));
        let acting = acting.as_ref().expect("the change names who made it");
        assert_eq!(acting.source_invocation, Some(made.invocation));
        assert_eq!(
            acting.invoker.as_ref().map(|invoker| &invoker.acting),
            Some(porter.acting()),
            "the agent's change names the agent"
        );
    }

    let bea = Caller::local_person("bea")
        .in_workspace(&fixture.workspace)
        .in_new_execution();
    let folder = files_call(
        &fixture.workspace,
        "new-folder",
        &json!({"parent": "Tasks", "name": "Notes"}),
    );
    invoke(bea.clone(), folder)
        .await
        .expect("the workspace lets the editor make folders");
    let trash = files_call(
        &fixture.workspace,
        "trash",
        &json!({"folder": "Tasks", "entries": ["12 Plan"]}),
    );
    assert!(matches!(
        invoke(bea, trash).await,
        Err(ActionCallError::NotPermitted)
    ));

    let undo = files_call(
        &fixture.workspace,
        "undo",
        &json!({"invocation": made.invocation.0.to_string()}),
    );
    invoke(owner(&fixture.workspace), undo)
        .await
        .expect("the person takes the agent's call back");

    let commenter = Caller::local_person("ada")
        .in_workspace(&fixture.workspace)
        .in_new_execution();
    let rename = files_call(
        &fixture.workspace,
        "rename",
        &json!({"folder": "Tasks", "entry": "12 Plan", "name": "13 Plan"}),
    );
    assert!(matches!(
        invoke(commenter, rename).await,
        Err(ActionCallError::NotPermitted)
    ));

    let elsewhere = Caller::local_person(owner_id())
        .in_workspace(WorkspaceId::from_u128(72).to_string())
        .in_new_execution();
    let write = files_call(
        &fixture.workspace,
        "write-file",
        &json!({"folder": "Tasks", "name": "a.md", "text": "a"}),
    );
    assert!(invoke(elsewhere, write).await.is_err());

    let reached = fixture.recorder.2.lock().unwrap();
    let verbs: Vec<&str> = reached.iter().map(|(verb, _)| verb.as_str()).collect();
    assert_eq!(
        verbs,
        ["new-folder", "new-folder", "undo"],
        "a refused call changed nothing"
    );
}
