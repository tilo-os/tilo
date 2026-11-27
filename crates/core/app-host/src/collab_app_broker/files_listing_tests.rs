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

use tilo_workspace_service::workspaces::SentMessage;

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
/// refused: no call here puts a file or changes the folder. Beside them, the messages the
/// workspace's records hold, each by its conversation and id.
#[derive(Default)]
struct Recorder(
    Mutex<Vec<(String, Map<String, Value>)>>,
    Vec<(tilo_action_policy::turn::Turn, SentMessage)>,
);

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

    fn sent_carrying(
        &self,
        _workspace: WorkspaceId,
        turn: &tilo_action_policy::turn::Turn,
        hash: &str,
    ) -> Option<SentMessage> {
        self.1
            .iter()
            .find(|(sent, _)| sent == turn)
            .map(|(_, message)| message.clone())
            .filter(|message| message.carries.iter().any(|carried| carried == hash))
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
    fixture_with(Recorder::default(), |_| {})
}

/// [`fixture`], its records `recorder`'s and its rules changed by `rules`.
fn fixture_with(recorder: Recorder, rules: impl FnOnce(&mut Workspace)) -> Fixture {
    let home = tempfile::tempdir().unwrap();
    let mut workspace = Workspace::new(WorkspaceId::from_u128(71), "Files listing");
    workspace.policy.set_owner(owner_id());
    rules(&mut workspace);
    let id = workspace.id().to_string();
    let mut tree = WorkspaceTree::default();
    tree.desktop.workspaces.push(workspace);
    tree.persist_path = Some(home.path().join("state.json"));
    let membership = tree.shared();
    let recorder = Arc::new(recorder);
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
        asks: false,
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

/// The file a message sends the agent, the version it names: what the turn may read.
const SENT: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";
/// Another file of the workspace, which no message sends.
const OTHER: &str = "b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2";
/// The version the sent file's path holds after it was sent: `Inbox/plan.md` edited.
const EDITED: &str = "c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3";

/// The person whose read of the workspace ended before they sent a file.
const LAPSED: &str = "d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4";

/// The conversation the messages are in.
const CONVERSATION: &str = "so.tilo.chat/channel-1";

/// When the lapsed person's read of the workspace ended, in Unix seconds.
fn lapsed_at() -> u64 {
    tilo_clock::unix_now().saturating_sub(3_600)
}

/// A turn of the message `message` in `conversation`.
fn turn(conversation: &str, message: &str) -> tilo_action_policy::turn::Turn {
    tilo_action_policy::turn::Turn::new(conversation, message)
}

/// The messages the workspace holds: the owner's that sends [`SENT`], the owner's that sends
/// nothing, the lapsed person's that sends [`SENT`] after their read ended, and the lapsed
/// person's that sent it while they could still read it.
fn messages() -> Recorder {
    let sent = |by: &str, at_s: u64, carries: &[&str]| SentMessage {
        by: by.to_owned(),
        at: jiff::Timestamp::from_second(i64::try_from(at_s).unwrap())
            .unwrap()
            .to_string(),
        carries: carries.iter().map(|hash| (*hash).to_owned()).collect(),
    };
    let now = tilo_clock::unix_now();
    Recorder(
        Mutex::default(),
        vec![
            (
                turn(CONVERSATION, "sends"),
                sent(&owner_id(), now.saturating_sub(60), &[SENT]),
            ),
            (
                turn(CONVERSATION, "says"),
                sent(&owner_id(), now.saturating_sub(60), &[]),
            ),
            (
                turn(CONVERSATION, "lapsed"),
                sent(LAPSED, lapsed_at().saturating_add(60), &[SENT]),
            ),
            (
                turn(CONVERSATION, "before"),
                sent(LAPSED, lapsed_at().saturating_sub(60), &[SENT]),
            ),
            (
                turn(CONVERSATION, "untimed"),
                SentMessage {
                    at: String::new(),
                    ..sent(&owner_id(), now, &[SENT])
                },
            ),
        ],
    )
}

/// The workspace's rules: the owner refuses their agent Files, and the lapsed person read the
/// workspace until [`lapsed_at`].
fn rules(workspace: &mut Workspace) {
    use tilo_action_policy::{Answer, Grant, Grantee, KindSet, Lifetime, TreePath};
    let id = workspace.id().to_string();
    workspace.policy.grant(Grant::new(
        "scout-reads-no-files",
        Grantee::agent("scout", owner_id()),
        TreePath::workspace(&id).app(fs_protocol::tree::FILES_APP),
        KindSet::READ_UNDOABLE,
        Answer::Deny,
        Grantee::person(owner_id()),
    ));
    workspace.policy.grant(
        Grant::new(
            "lapsed-reads",
            Grantee::person(LAPSED),
            TreePath::workspace(&id),
            KindSet::READ,
            Answer::Allow,
            Grantee::person(owner_id()),
        )
        .with_lifetime(Lifetime::Until(lapsed_at().saturating_mul(1_000))),
    );
}

/// The owner's agent inside the turn of `message` in `conversation`, holding what its
/// certificate delegates to it, as a triggered turn's calls are made.
fn agent_turn(workspace: &str, conversation: &str, message: &str) -> Caller {
    Caller::agent_turn(
        ("scout", owner_id()),
        owner_id(),
        turn(conversation, message),
    )
    .in_workspace(workspace)
    .granting(tilo_action_policy::roles::certified_agent(
        workspace,
        ("scout", &owner_id()),
        &owner_id(),
    ))
}

/// [`agent_turn`] in [`CONVERSATION`], in a run of its own.
fn agent_in(workspace: &str, message: &str) -> Caller {
    agent_turn(workspace, CONVERSATION, message).in_new_execution()
}

/// Files' `read-text` of the workspace's folder with `arguments`.
fn read_text(workspace: &str, arguments: Value) -> InvokeRequest {
    let Value::Object(arguments) = arguments else {
        panic!("the arguments are an object")
    };
    InvokeRequest {
        asks: false,
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: tilo_actions::remote::workspace_provider(workspace),
            address: tilo_actions::ActionAddress {
                resource: format!(
                    "{APPLICATION_RESOURCE_PREFIX}{}",
                    fs_protocol::tree::FILES_APP
                ),
                action: fs_protocol::tree::READ_TEXT.to_owned(),
            },
        },
        argument: InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// The address a message names `hash` by.
fn address(hash: &str) -> Value {
    json!({"content": format!("tilo.content:{hash}")})
}

/// `caller`'s read with `arguments` is refused as not permitted, and nothing reaches the folder.
async fn refused(fixture: &Fixture, caller: &Caller, arguments: Value, why: &str) {
    let before = fixture.recorder.0.lock().unwrap().len();
    let request = read_text(&fixture.workspace, arguments);
    let answer = tilo_test_env::within(
        "the broker answers the read",
        settled(fixture.state.action_invoke(caller, None, None, &request)),
    )
    .await;
    assert!(
        matches!(answer, Err(ActionCallError::NotPermitted)),
        "{why}: {answer:?}"
    );
    assert_eq!(
        fixture.recorder.0.lock().unwrap().len(),
        before,
        "{why}: nothing reached the folder"
    );
}

/// Once `caller`'s turn ended, its read with `arguments` is refused with the turn's run, and
/// nothing reaches the folder.
async fn refused_once_the_turn_ended(fixture: &Fixture, caller: &Caller, arguments: Value) {
    caller
        .execution_lease()
        .expect("the turn runs in a run of its own")
        .close();
    let reads = fixture.recorder.0.lock().unwrap().len();
    let request = read_text(&fixture.workspace, arguments);
    let answer = tilo_test_env::within(
        "the broker answers the read",
        settled(fixture.state.action_invoke(caller, None, None, &request)),
    )
    .await;
    assert!(
        matches!(answer, Err(ActionCallError::Unavailable { .. })),
        "a read after the turn ended is refused with the turn's run: {answer:?}"
    );
    assert_eq!(
        fixture.recorder.0.lock().unwrap().len(),
        reads,
        "nothing was read after the turn ended"
    );
}

/// A file a message sends the agent is shared with the turn the message started: that one
/// version, by its content hash, labelled as content from outside, while the turn goes. Another
/// file, another version of the same path and a message that sends none stay refused, as does
/// anything once the turn ended.
#[tokio::test]
async fn a_turn_reads_the_version_its_message_sends_and_nothing_else() {
    let fixture = fixture_with(messages(), rules);
    let workspace = fixture.workspace.clone();

    // The attached version is read, and its observer is told it is content from outside.
    let caller = agent_in(&workspace, "sends");
    let request = read_text(&workspace, address(SENT));
    tilo_test_env::within(
        "the turn's read of the file its message sends is admitted",
        settled(fixture.state.action_invoke(&caller, None, None, &request)),
    )
    .await
    .unwrap_or_else(|error| panic!("the turn reads the file its message sends: {error:?}"));
    let answer = tilo_test_env::within(
        "the read completes",
        finished(&fixture.state, &caller, &request),
    )
    .await;
    assert_eq!(answer, json!({"read": "read-text"}));
    assert!(
        fixture
            .state
            .action_observe(&caller, &request.invocation)
            .is_some_and(|seen| seen.untrusted),
        "what a sent file says reaches the turn labelled as content from outside"
    );
    {
        let reads = fixture.recorder.0.lock().unwrap();
        assert_eq!(reads.len(), 1, "one read reached the folder");
        assert_eq!(Value::Object(reads[0].1.clone()), address(SENT));
    }

    // Another file of the workspace, by its content or its path, is refused.
    refused(&fixture, &caller, address(OTHER), "another file").await;
    refused(
        &fixture,
        &caller,
        json!({"file": "Inbox/plan.md"}),
        "a file by its path",
    )
    .await;
    refused(&fixture, &caller, json!({"folder": "Inbox"}), "the folder").await;

    // The path's later version is another version, and is refused.
    refused(
        &fixture,
        &caller,
        address(EDITED),
        "the path's edited version",
    )
    .await;

    // A turn of a message that sends nothing claims the file in vain, as does a turn naming the
    // sending message in another conversation.
    let claims = agent_in(&workspace, "says");
    refused(
        &fixture,
        &claims,
        address(SENT),
        "a file the message does not send",
    )
    .await;
    let elsewhere = agent_turn(&workspace, "so.tilo.chat/channel-2", "sends").in_new_execution();
    refused(
        &fixture,
        &elsewhere,
        address(SENT),
        "the message, named in a conversation it is not in",
    )
    .await;

    // Once the turn ended, the file it read is refused.
    refused_once_the_turn_ended(&fixture, &caller, address(SENT)).await;
    // The agent holds no grant of the folder: without a turn, the same read is refused.
    let outside = agent_turn(&workspace, CONVERSATION, "sends");
    refused(
        &fixture,
        &outside,
        address(SENT),
        "a turn with no run of its own",
    )
    .await;
}

/// A file is shared as its sender stood when they sent it: one they sent while they could read
/// the workspace is read, one sent after their read ended is refused, and one whose message names
/// no time it was sent at is refused, whatever its sender may read now.
#[tokio::test]
async fn a_file_is_shared_as_its_sender_stood_when_they_sent_it() {
    let fixture = fixture_with(messages(), rules);
    let workspace = fixture.workspace.clone();

    // A file sent by someone who could no longer read it is refused; one they sent while they
    // could is read.
    let lapsed = agent_in(&workspace, "lapsed");
    refused(
        &fixture,
        &lapsed,
        address(SENT),
        "a file its sender could not read when they sent it",
    )
    .await;
    // A message that names no time it was sent at shares nothing, though its sender may read
    // the file now: what they could read when they sent it is not known.
    let untimed = agent_in(&workspace, "untimed");
    refused(
        &fixture,
        &untimed,
        address(SENT),
        "a file whose message names no time it was sent at",
    )
    .await;
    let before = agent_in(&workspace, "before");
    let request = read_text(&workspace, address(SENT));
    tilo_test_env::within(
        "a file sent while its sender could read it is read",
        settled(fixture.state.action_invoke(&before, None, None, &request)),
    )
    .await
    .unwrap_or_else(|error| panic!("the sender could read it when they sent it: {error:?}"));
    tilo_test_env::within(
        "the read completes",
        finished(&fixture.state, &before, &request),
    )
    .await;
}
