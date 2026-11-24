//! A moderation turn reads a reported person's recorded changes and takes exactly those back,
//! through the workspace service's own provider and the same grants every action is decided by.

#![allow(
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture failing is its test failing"
)]

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tilo_action_policy::{Grant, Grantee, KindSet, TreePath};
use tilo_actions::InvocationId;
use tilo_actions::remote::WORKSPACE_AGENT_CREATE;
use tilo_actions::remote::{
    ActionProvider as _, InvokeArgument, InvokeRequest, RemoteActionTarget,
};
use tilo_agent_session::AppCatalogue;
use tilo_agent_session::model::PlanTurns;
use tilo_model_gateway::host_backend::protocol::{
    Content, Message, ModelError, ModelRequest, ModelResponse, Role, StopReason, ToolCall, Usage,
};
use tilo_resource_store::wire::Command;
use tilo_resource_store::{Cell, Resource, Stamp, Value as StoreValue};
use tilo_server_protocol::WorkspaceId;

use super::agent_keys::AgentKeys;
use super::{Model, ServerAgents};

const CHAT: &str = "so.tilo.chat";
const MESSAGE: &str = "tl.so.tilo.lang.chat.Message";
const SCOUT: &str = "scout";

/// A model that reads a person's changes through the turn's history action, then takes exactly
/// what the read returned back, then answers.
struct Scripted {
    person: String,
    asked: Arc<Mutex<Vec<ModelRequest>>>,
}

impl PlanTurns for Scripted {
    fn turn(
        &mut self,
        request: &ModelRequest,
        _: &str,
        _: std::time::Instant,
        _: bool,
    ) -> Result<ModelResponse, ModelError> {
        self.asked.lock().unwrap().push(request.clone());
        let call = match self.asked.lock().unwrap().len() {
            1 => Some(tool_call(
                "read",
                "activity",
                json!({"app": CHAT, "person": self.person}),
            )),
            2 => {
                // What the read answered, as its model reads it: the changes it must take back.
                let changes: Vec<String> = request
                    .messages
                    .iter()
                    .flat_map(|message| message.content.iter())
                    .filter_map(|content| match content {
                        Content::ToolResult(result) => {
                            let value: Value = serde_json::from_str(&result.content).ok()?;
                            Some(
                                value["versions"]
                                    .as_array()?
                                    .iter()
                                    .filter_map(|version| {
                                        version["change"].as_str().map(str::to_owned)
                                    })
                                    .collect::<Vec<_>>(),
                            )
                        }
                        _ => None,
                    })
                    .flatten()
                    .collect();
                Some(tool_call(
                    "undo",
                    "undo",
                    json!({"app": CHAT, "person": self.person, "changes": changes}),
                ))
            }
            _ => None,
        };
        let (content, stop_reason) = match call {
            Some(call) => (vec![call], StopReason::ToolCalls),
            None => (
                vec![Content::Text {
                    text: "I read what they did and took it back.".into(),
                }],
                StopReason::EndTurn,
            ),
        };
        Ok(ModelResponse {
            message: Message {
                role: Role::Assistant,
                content,
            },
            stop_reason,
            usage: Usage::default(),
            cost_micros: 0,
        })
    }

    fn model(&self) -> &'static str {
        "scripted"
    }
}

/// A `tilo_do` call of the history provider.
fn tool_call(id: &str, action: &str, arguments: Value) -> Content {
    Content::ToolCall(ToolCall {
        id: id.to_owned(),
        name: "tilo_do".into(),
        input: json!({
            "path": format!("tilo.moderation/moderation.history#{action}"),
            "args": arguments,
        }),
        extra_content: None,
    })
}

/// The key a person's device makes for its agents: one fixed key here.
struct Keys;

impl AgentKeys for Keys {
    fn grant(
        &self,
        _workspace: WorkspaceId,
        _agent: &str,
        _name: &str,
        _person: [u8; 32],
    ) -> Result<[u8; 32], String> {
        Ok([7; 32])
    }

    fn sign(
        &self,
        _workspace: WorkspaceId,
        _key: &[u8; 32],
        _request: &mut tilo_actions::remote::InvokeRequest,
    ) -> Result<(), String> {
        Ok(())
    }

    fn made(
        &self,
        _key: &[u8; 32],
    ) -> Option<(tilo_user_identity::DeviceCert, ed25519_dalek::SigningKey)> {
        None
    }
}

/// Make `scout` through the workspace's typed create, with the moderation watch: what a
/// Moderator's device asks.
async fn create_watching_agent(
    workspaces: &crate::workspaces::Workspaces,
    workspace: &str,
    cert: &tilo_user_identity::DeviceCert,
) {
    let caller =
        tilo_actions::remote::Caller::authenticated(cert.user_public_key, cert.device_public_key)
            .in_workspace(workspace);
    let request = InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget::workspace_governance(workspace, WORKSPACE_AGENT_CREATE),
        argument: InvokeArgument::Json(serde_json::Map::from_iter([
            ("name".to_owned(), json!("Scout")),
            ("id".to_owned(), json!(SCOUT)),
            ("moderates".to_owned(), json!(true)),
        ])),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let mut pending = workspaces.device_actions().invoke(&caller, request);
    loop {
        match pending.poll_ready() {
            tilo_actions::remote::PendingPoll::Ready(Ok(receipt)) => {
                assert!(
                    matches!(
                        receipt.status,
                        tilo_actions::remote::ReceiptStatus::Completed { .. }
                    ),
                    "the agent is made: {receipt:?}"
                );
                return;
            }
            tilo_actions::remote::PendingPoll::Ready(Err(error)) => {
                panic!("the agent is not made: {error}")
            }
            tilo_actions::remote::PendingPoll::Waiting => tokio::task::yield_now().await,
            tilo_actions::remote::PendingPoll::Abandoned => panic!("the agent's making is dropped"),
        }
    }
}

/// The workspace lets `scout` change its records, so its undo is admitted.
async fn grant_scout(workspaces: &crate::workspaces::Workspaces, workspace: &str, owner: &str) {
    let grant = Grant::new(
        "scout-chat",
        Grantee::agent(SCOUT.to_owned()),
        TreePath::workspace(workspace.to_owned()),
        KindSet::READ_UNDOABLE,
        tilo_action_policy::Answer::Allow,
        Grantee::person(owner.to_owned()),
    );
    workspaces
        .commit_own_membership(
            workspace,
            tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::Grant { grant },
            None,
        )
        .await
        .unwrap();
}

/// The person posts `words` in the channel, as one invocation of theirs.
async fn post(
    workspaces: &crate::workspaces::Workspaces,
    workspace: &str,
    cert: &tilo_user_identity::DeviceCert,
    id: &str,
    words: &str,
) -> String {
    let invocation = InvocationId::new();
    let by = tilo_actions::Acting::person(hex::encode(cert.user_public_key)).by;
    let stamp = Stamp {
        verb: "send".into(),
        invoker: Stamp::by(&by),
        invocation: invocation.0.to_string(),
        at: jiff::Timestamp::now().to_string(),
        ..Stamp::default()
    };
    let commands = vec![Command::Put {
        kind: MESSAGE.into(),
        id: id.into(),
        resource: Resource {
            readers: "*".into(),
            parent: Some("general".into()),
            order: None,
            fields: std::collections::BTreeMap::from([(
                "text".to_owned(),
                Cell::One(StoreValue::Text(words.to_owned())),
            )]),
        },
        stamp: stamp.clone(),
    }];
    let caller =
        tilo_actions::remote::Caller::authenticated(cert.user_public_key, cert.device_public_key)
            .in_workspace(workspace);
    let context =
        crate::apps::services::ResourceRequestContext::authenticated(CHAT, workspace, caller)
            .serving(vec![(invocation, by)]);
    workspaces
        .resource_command(&context, Command::Batch(commands))
        .await
        .unwrap();
    id.to_owned()
}

/// The change a message's post is, as the record's history names it.
fn change_of(versions: &[Value], message: &str) -> String {
    versions
        .iter()
        .find(|version| {
            version["resource"]
                .as_str()
                .is_some_and(|resource| resource.ends_with(&format!(":{message}")))
        })
        .and_then(|version| version["change"].as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| panic!("{message} is in the record's history: {versions:#?}"))
}

/// A chat message's change, as a trigger hands it on.
fn posted(
    workspace: &str,
    person: &str,
    message: &str,
) -> tilo_automation::dispatch::DocumentEvent {
    tilo_automation::dispatch::DocumentEvent {
        workspace: workspace.to_owned(),
        record: "general".to_owned(),
        app: CHAT.to_owned(),
        kind: MESSAGE.to_owned(),
        change: format!("change-{message}"),
        actor: tilo_actions::Acting::person(person),
        did: tilo_actions::Did::verb(tilo_actions::ActionKey::new(CHAT, MESSAGE, "send"), []),
        arguments: serde_json::Map::new(),
        item: Some(tilo_automation::dispatch::ItemRef {
            kind: MESSAGE.to_owned(),
            id: message.to_owned(),
            label: "something".to_owned(),
        }),
        before: None,
        after: Some(json!({"$in": {"$id": "general"}, "text": "something"})),
        caused_by: None,
        origin: None,
        incoming_message: false,
        housekeeping: false,
        shortcut: None,
        invocation: None,
    }
}

/// The agent's turn reads the person's two changes and takes exactly them back as one run; a
/// turn whose saver may not records nothing at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_moderation_turn_reads_a_persons_changes_and_takes_them_back_in_one_run() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = crate::workspaces::fixtures::governor();
    let catalogue = AppCatalogue::native();
    crate::surfaces::decide_receipt(&scratch.path().join("surfaces"));
    let server = crate::workspaces::fixtures::test_host(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let workspace = workspaces.create("Moderation").await.unwrap().workspace;
    let owner = hex::encode(cert.user_public_key);
    let chat_record = crate::agents::workspace_resource(&catalogue, CHAT);
    workspaces
        .prepare_application(&workspace, &cert, &catalogue, CHAT, &chat_record)
        .await
        .unwrap();
    create_watching_agent(&workspaces, &workspace, &cert).await;
    grant_scout(&workspaces, &workspace, &owner).await;

    post(&workspaces, &workspace, &cert, "m-1", "a spam offer").await;
    post(&workspaces, &workspace, &cert, "m-2", "and another").await;
    let before = workspaces
        .record_changes(&workspace, &route(&workspace))
        .unwrap();
    let taken: Vec<String> = ["m-1", "m-2"]
        .into_iter()
        .map(|message| change_of(&before, message))
        .collect();

    let asked = Arc::new(Mutex::new(Vec::new()));
    let model: Model = {
        let asked = Arc::clone(&asked);
        let person = owner.clone();
        Arc::new(move || {
            Some(Box::new(Scripted {
                person: person.clone(),
                asked: Arc::clone(&asked),
            }) as Box<dyn PlanTurns>)
        })
    };
    let provider = Arc::new(tilo_actions::remote::TestProvider::new());
    let agents =
        ServerAgents::on_device(workspaces.clone(), provider, Arc::new(Keys)).scripted(model);
    let event = posted(&workspace, &owner, "m-1");
    let place = crate::agents::triggered::Where::of(&event).expect("a message is a place");
    let started = agents
        .triggered(
            &workspace,
            SCOUT,
            place,
            event,
            owner.clone(),
            (InvocationId::new(), Arc::new(AtomicBool::new(false))),
        )
        .await
        .expect("the turn ends");
    assert!(
        matches!(started, crate::work::Started::Taken(_)),
        "this device took the turn: {started:?}"
    );

    // The read showed the person's two changes with their invocations, and nothing else.
    let asked = asked.lock().unwrap().clone();
    let shown = tool_results(&asked[1]);
    assert!(
        taken.iter().all(|change| shown.contains(change.as_str())),
        "the read answered the person's changes: {shown}"
    );
    assert_eq!(
        shown.matches("\"who\"").count(),
        2,
        "only the person's two changes: {shown}"
    );

    // The undo took exactly those two back, as one run, and the messages are gone.
    let after = workspaces
        .record_changes(&workspace, &route(&workspace))
        .unwrap();
    let undone: Vec<&Value> = after
        .iter()
        .filter(|version| {
            version["action"].as_str() == Some(crate::tl_records::runtime::store::STEP_ACTION)
        })
        .collect();
    assert_eq!(undone.len(), 2, "each change is taken back: {undone:#?}");
    let run = undone[0]["task"]["id"].as_str().unwrap_or_default();
    assert!(
        run.starts_with("moderation:"),
        "the undo is grouped as one moderation run: {run}"
    );
    assert!(
        undone
            .iter()
            .all(|version| version["task"]["id"].as_str() == Some(run)),
        "both undos are one run"
    );
}

/// The same turn, saved by someone who may not change the record: the read is refused and
/// nothing is taken back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_moderation_turn_is_refused_when_the_saver_may_not() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = crate::workspaces::fixtures::governor();
    let catalogue = AppCatalogue::native();
    crate::surfaces::decide_receipt(&scratch.path().join("surfaces"));
    let server = crate::workspaces::fixtures::test_host(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let workspace = workspaces.create("Moderation").await.unwrap().workspace;
    let owner = hex::encode(cert.user_public_key);
    let chat_record = crate::agents::workspace_resource(&catalogue, CHAT);
    workspaces
        .prepare_application(&workspace, &cert, &catalogue, CHAT, &chat_record)
        .await
        .unwrap();
    create_watching_agent(&workspaces, &workspace, &cert).await;
    grant_scout(&workspaces, &workspace, &owner).await;
    post(&workspaces, &workspace, &cert, "m-1", "a spam offer").await;

    let asked = Arc::new(Mutex::new(Vec::new()));
    let model: Model = {
        let asked = Arc::clone(&asked);
        let person = owner.clone();
        Arc::new(move || {
            Some(Box::new(Scripted {
                person: person.clone(),
                asked: Arc::clone(&asked),
            }) as Box<dyn PlanTurns>)
        })
    };
    let provider = Arc::new(tilo_actions::remote::TestProvider::new());
    let agents =
        ServerAgents::on_device(workspaces.clone(), provider, Arc::new(Keys)).scripted(model);
    let saver = "c2".repeat(32); // a member whose grants do not reach the record
    let event = posted(&workspace, &owner, "m-1");
    let place = crate::agents::triggered::Where::of(&event).expect("a message is a place");
    let started = agents
        .triggered(
            &workspace,
            SCOUT,
            place,
            event,
            saver,
            (InvocationId::new(), Arc::new(AtomicBool::new(false))),
        )
        .await
        .expect("the turn ends");
    assert!(matches!(started, crate::work::Started::Taken(_)));

    let asked = asked.lock().unwrap().clone();
    let shown = tool_results(&asked[1]);
    assert!(
        shown.contains("not_permitted"),
        "the turn is told it may not: {shown}"
    );
    let after = workspaces
        .record_changes(&workspace, &route(&workspace))
        .unwrap();
    assert!(
        !after.iter().any(|version| version["action"]
            .as_str()
            .is_some_and(|action| action == crate::tl_records::runtime::store::STEP_ACTION)),
        "a refused turn takes nothing back: {after:#?}"
    );
}

/// The record's route.
fn route(workspace: &str) -> tilo_peer_state::document_frame::DocumentRoute {
    tilo_peer_state::document_frame::DocumentRoute {
        app_id: CHAT.to_owned(),
        cluster_id: workspace.to_owned(),
        document: String::new(),
    }
}

/// Every tool answer `request` carries, as its text.
fn tool_results(request: &ModelRequest) -> String {
    request
        .messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|content| match content {
            Content::ToolResult(result) => Some(result.content.clone()),
            Content::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}
