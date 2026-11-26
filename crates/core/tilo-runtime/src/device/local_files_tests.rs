//! Calls on the workspace's own folder as the dispatch makes them: a file put where the
//! workspace's grants let the caller write its files and refused where they do not, a verb
//! decided as the tree's own verbs, and an agent refused before anything is written unless the
//! key its person certified signed it.

#![allow(
    clippy::significant_drop_tightening,
    reason = "fixtures keep the recorder while asserting on it"
)]

use std::sync::Mutex;
use std::time::Duration;

use serde_json::{Map, Value, json};
use tilo_action_policy::TreePath;
use tilo_action_services::device_files as wire;
use tilo_actions::ActionOutput;
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, Caller, InvokeRequest, ReceiptStatus,
};
use tilo_app_host::services::{FilesEntry, FilesPutCall, FilesTreePuts};
use tilo_app_host::test_support::{Grantee, Grants as _, KindSet, Role};
use tilo_broker_core::services::KEYLESS_AGENT;
use tilo_desktop_model::Workspace;
use tilo_server_protocol::WorkspaceId;

use super::*;

const WORKSPACE: u128 = 70;

fn workspace_id() -> String {
    WorkspaceId::from_u128(WORKSPACE).to_string()
}

/// One put or change that reached the folder: its workspace, the arguments, and the context the
/// tree's change is made in.
type Reached = (
    String,
    Map<String, Value>,
    Option<tilo_actions::ActionContext>,
);

/// What reached the folder: the puts and the changes, each with the call it was made for.
#[derive(Default)]
struct Recorder {
    puts: Mutex<Vec<Reached>>,
    changes: Mutex<Vec<(String, Reached)>>,
}

impl FilesTreePuts for Recorder {
    fn put(
        &self,
        workspace: WorkspaceId,
        arguments: &Map<String, Value>,
        call: Option<FilesPutCall<'_>>,
        confirm: &dyn Fn() -> bool,
    ) -> Result<Value, String> {
        if !confirm() {
            return Err("the caller may no longer write the workspace's files".to_owned());
        }
        self.puts.lock().unwrap().push((
            workspace.to_string(),
            arguments.clone(),
            call.map(|call| call.context),
        ));
        Ok(json!({"path": "Inbox/plan.md", "size": 2, "replaced": false}))
    }

    fn entries(
        &self,
        _workspace: WorkspaceId,
        _folder: &str,
        _below: bool,
    ) -> Result<Vec<FilesEntry>, String> {
        Ok(Vec::new())
    }

    fn read(
        &self,
        _workspace: WorkspaceId,
        _action: &str,
        _arguments: &Map<String, Value>,
    ) -> Result<Value, String> {
        Err("a read is no put".to_owned())
    }

    fn change(
        &self,
        workspace: WorkspaceId,
        verb: &str,
        arguments: &Map<String, Value>,
        call: Option<FilesPutCall<'_>>,
        confirm: &dyn Fn() -> bool,
    ) -> Result<Value, String> {
        if !confirm() {
            return Err("the caller may no longer change the workspace's files".to_owned());
        }
        self.changes.lock().unwrap().push((
            verb.to_owned(),
            (
                workspace.to_string(),
                arguments.clone(),
                call.map(|call| call.context),
            ),
        ));
        Ok(json!({"action": verb, "entries": []}))
    }
}

/// What decides a call on the folder, as the test sets it up.
struct Folder {
    workspace: Workspace,
    recorder: Arc<Recorder>,
}

impl Folders for Folder {
    fn workspace(&self, id: &str) -> Option<Workspace> {
        (id == self.workspace.id().to_string()).then(|| self.workspace.clone())
    }

    fn now(&self) -> u64 {
        tilo_clock::unix_now()
    }

    fn puts(&self) -> Option<Arc<dyn FilesTreePuts>> {
        Some(Arc::clone(&self.recorder) as Arc<dyn FilesTreePuts>)
    }
}

/// The person's root, and the id it names them by.
fn root() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[1; 32])
}

fn owner_id() -> String {
    hex::encode(root().verifying_key().to_bytes())
}

/// A workspace with an owner, a member who edits it but may not trash, and an agent the owner
/// lets put files into its folder.
fn a_folder() -> (Folder, Arc<Recorder>) {
    let mut workspace = Workspace::new(WorkspaceId::from_u128(WORKSPACE), "Folder");
    workspace.policy.set_owner(owner_id());
    let id = workspace.id().to_string();
    let records = collab_types::document_node(
        &id,
        fs_protocol::tree::FILES_APP,
        &fs_protocol::tree::document_id(),
    );
    workspace.allow(
        Grantee::agent("porter", owner_id()),
        &records,
        KindSet::READ_UNDOABLE,
    );
    for verb in collab_types::FILES_OPEN_VERBS {
        let node =
            TreePath::workspace(&id).joined(&fs_protocol::file_action::verb_node(None, verb));
        workspace.allow(
            Grantee::agent("porter", owner_id()),
            &node,
            KindSet::READ_UNDOABLE,
        );
    }
    workspace.policy.add_member("bea", Role::Editor);
    workspace.deny(
        Grantee::person("bea"),
        &fs_protocol::file_action::verb_node(None, fs_protocol::file_action::verbs::TRASH),
        KindSet::UNDOABLE,
    );
    let recorder = Arc::new(Recorder::default());
    (
        Folder {
            workspace,
            recorder: Arc::clone(&recorder),
        },
        recorder,
    )
}

/// The owner, acting as themselves.
fn owner() -> Caller {
    Caller::local_person(owner_id())
        .in_workspace(workspace_id())
        .in_new_execution()
}

/// A member of the workspace who edits it; `person` names them.
fn member(person: &str) -> Caller {
    Caller::local_person(person)
        .in_workspace(workspace_id())
        .in_new_execution()
}

/// An agent the owner admitted, acting for them.
fn agent() -> Caller {
    Caller::local_person(owner_id())
        .delegated_to("porter")
        .in_workspace(workspace_id())
        .in_new_execution()
}

/// The provider the test calls, as the dispatch calls it.
fn provider(held: Folder) -> LocalFiles {
    LocalFiles::new(Arc::new(held))
}

/// Call `request` on `provider` and read what it answered.
#[allow(
    clippy::panic_in_result_fn,
    reason = "a call that answers something else is this test's own failure"
)]
fn call(
    provider: &LocalFiles,
    caller: &Caller,
    request: InvokeRequest,
) -> Result<ActionOutput, ActionCallError> {
    let receipt = provider
        .invoke(caller, request)
        .wait_timeout(Duration::from_secs(5))
        .ok()
        .flatten()
        .expect("the device answers the call")?;
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output),
        ReceiptStatus::Failed { error } => Err(ActionCallError::Refused {
            reason: error.message,
        }),
        ReceiptStatus::Rejected { error } => Err(error),
        other => panic!("the call answered {other:?}"),
    }
}

fn open(arguments: &Value) -> InvokeRequest {
    let arguments = arguments.as_object().cloned().unwrap_or_default();
    wire::request(wire::OPEN, arguments, None)
}

fn verb(action: &str, arguments: &Value) -> InvokeRequest {
    let arguments = arguments.as_object().cloned().unwrap_or_default();
    wire::request(action, arguments, None)
}

#[test]
fn a_file_goes_into_the_folder_the_caller_may_write_and_not_one_it_may_not() {
    let (held, recorder) = a_folder();
    let files = provider(held);
    let answer = call(
        &files,
        &owner(),
        open(&json!({"content": {"text": "hello"}, "name": "plan.txt"})),
    )
    .expect("the owner puts a file");
    assert_eq!(
        answer.value,
        Some(json!({"path": "Inbox/plan.md", "size": 2, "replaced": false}))
    );
    {
        let puts = recorder.puts.lock().unwrap();
        assert_eq!(puts.len(), 1, "one put reached the folder");
        assert_eq!(puts[0].0, workspace_id());
        assert_eq!(puts[0].1.get("name"), Some(&json!("plan.txt")));
        assert!(
            puts[0]
                .2
                .as_ref()
                .is_some_and(|context| context.source_invocation.is_some()),
            "the change keeps the call it was made for"
        );
    }

    // A person the workspace does not let write its files is refused, in words they read, and
    // nothing reaches the tree.
    let (held, recorder) = a_folder();
    let stranger = member(&"b2".repeat(32));
    let files = provider(held);
    let refused = call(
        &files,
        &stranger,
        open(&json!({"content": {"text": "hello"}, "name": "plan.txt"})),
    )
    .expect_err("a stranger's put is refused");
    assert!(
        matches!(&refused, ActionCallError::Refused { reason } if reason.contains("write its files")),
        "{refused:?}"
    );
    assert!(recorder.puts.lock().unwrap().is_empty(), "nothing was put");

    // An agent the workspace lets put files asks under no key of its own: refused before
    // anything is written. A verb's key is the tree writer's to check, which keeps the request
    // and checks who asked on every replica.
    let (held, recorder) = a_folder();
    let porter = agent();
    let files = provider(held);
    let refused = call(
        &files,
        &porter,
        open(&json!({"content": {"text": "hello"}, "name": "plan.txt"})),
    )
    .expect_err("an agent under no key of its own is refused");
    assert!(
        matches!(&refused, ActionCallError::Refused { reason } if reason == KEYLESS_AGENT),
        "{refused:?}"
    );
    assert!(recorder.puts.lock().unwrap().is_empty());
}

#[test]
fn a_verb_is_decided_by_the_tree_verbs_it_is_carried_out_as() {
    let (held, recorder) = a_folder();
    // An editor may make a folder, and the workspace keeps them from the trash.
    let editor = member("bea");
    let files = provider(held);
    let answer = call(
        &files,
        &editor,
        verb("new-folder", &json!({"path": "Inbox"})),
    )
    .expect("the editor makes a folder");
    assert_eq!(
        answer.value,
        Some(json!({"action": "new-folder", "entries": []}))
    );
    {
        let changes = recorder.changes.lock().unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].0, "new-folder");
        assert_eq!(changes[0].1.1.get("path"), Some(&json!("Inbox")));
    }
    let refused = call(
        &files,
        &editor,
        verb("trash", &json!({"path": "Inbox/plan.md"})),
    )
    .expect_err("the trash is refused");
    assert!(
        matches!(&refused, ActionCallError::Refused { reason } if reason.contains("trash")),
        "{refused:?}"
    );
    assert_eq!(
        recorder.changes.lock().unwrap().len(),
        1,
        "nothing else reached the tree"
    );
}
