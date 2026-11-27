//! What the workspace's listing offers on its own folder is answered, each call made as its row
//! offers it.

#![allow(
    clippy::significant_drop_tightening,
    reason = "fixtures keep the recorder while asserting on it"
)]

use std::sync::Mutex;

use serde_json::{Map, Value, json};
use tilo_actions::remote::{
    APPLICATION_RESOURCE_PREFIX, Caller, InvokeArgument, InvokeRequest, RemoteActionTarget,
};
use tilo_actions::{InvocationId, InvocationUpdate};

use crate::services::{FilesEntry, FilesTreePuts};
use crate::test_support::{WorkspaceId, WorkspaceTree};

use super::*;

/// The person's root.
fn root() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[1; 32])
}

fn owner_id() -> String {
    hex::encode(root().verifying_key().to_bytes())
}

/// What reached the workspace's folder: the reads, each its action and arguments. The writer is
/// refused: no call here puts a file or changes the folder.
#[derive(Default)]
struct Recorder(Mutex<Vec<(String, Map<String, Value>)>>);

/// What the recorder's workspace folder holds: a folder `Inbox` with one file in it.
const HELD: [(&str, bool); 2] = [("Inbox", true), ("Inbox/plan.md", false)];

impl FilesTreePuts for Recorder {
    fn put(
        &self,
        _workspace: WorkspaceId,
        _arguments: &Map<String, Value>,
        _call: Option<crate::services::FilesPutCall<'_>>,
        _confirm: &dyn Fn() -> bool,
    ) -> Result<Value, String> {
        Err("no put reaches the folder here".to_owned())
    }

    fn entries(
        &self,
        _workspace: WorkspaceId,
        folder: &str,
        below: bool,
    ) -> Result<Vec<FilesEntry>, String> {
        Ok(HELD
            .into_iter()
            .filter(|(path, _)| {
                let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
                parent == folder || (below && path.starts_with(folder))
            })
            .map(|(path, folder)| FilesEntry {
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
        self.0
            .lock()
            .unwrap()
            .push((action.to_owned(), arguments.clone()));
        Ok(json!({"read": action}))
    }

    fn change(
        &self,
        _workspace: WorkspaceId,
        _verb: &str,
        _arguments: &Map<String, Value>,
        _call: Option<crate::services::FilesPutCall<'_>>,
        _confirm: &dyn Fn() -> bool,
    ) -> Result<Value, String> {
        Err("no change reaches the folder here".to_owned())
    }
}

struct Fixture {
    state: Arc<AppBrokerState>,
    recorder: Arc<Recorder>,
    workspace: String,
    _home: tempfile::TempDir,
}

/// A workspace with an owner, holding a folder `Inbox` with one file in it.
fn fixture() -> Fixture {
    let home = tempfile::tempdir().unwrap();
    let mut workspace = Workspace::new(WorkspaceId::from_u128(71), "Files listing");
    workspace.policy.set_owner(owner_id());
    let id = workspace.id().to_string();
    let mut tree = WorkspaceTree::default();
    tree.desktop.workspaces.push(workspace);
    tree.persist_path = Some(home.path().join("state.json"));
    let membership = tree.shared();
    let recorder = Arc::new(Recorder::default());
    let services = crate::services::ProcessServices {
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

/// The call of what `row` offers, as a caller going through the listing makes it.
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

/// The output the read `request` ends with, once it ends.
async fn finished(state: &Arc<AppBrokerState>, caller: &Caller, request: &InvokeRequest) -> Value {
    loop {
        if let Some(seen) = state.action_observe(caller, &request.invocation) {
            match seen.update {
                InvocationUpdate::Succeeded { output, .. } => {
                    return output.value.unwrap_or_default();
                }
                InvocationUpdate::Failed { error, .. } => panic!("the read failed: {error:?}"),
                InvocationUpdate::Cancelled { .. } => panic!("the read was cancelled"),
                _ => {}
            }
        }
        tokio::task::yield_now().await;
    }
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
        assert!(
            fixture
                .state
                .action_observe(&caller, &request.invocation)
                .is_some_and(|seen| seen.untrusted),
            "names and what a file says are content from outside, and its observer is told"
        );
        let reads = fixture.recorder.0.lock().unwrap();
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
        fixture.recorder.0.lock().unwrap().len(),
        calls.len(),
        "nothing was read for a refused caller"
    );
}
