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
use tilo_agent_session::model::PlanTurns;
use tilo_model_gateway::host_backend::protocol::{
    Content, Message, ModelError, ModelRequest, ModelResponse, Role, StopReason, ToolCall, Usage,
};

use tilo_app_host::agents::{Model, ServerAgents};
use tilo_workspace_service::workspaces::fixtures::answer_grant;

const CHAT: &str = "so.tilo.chat";
const MESSAGE: &str = "tl.so.tilo.chat.Message";
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
        let asked = self.asked.lock().unwrap().len();
        let call = match asked {
            1 => Some(tool_call(
                "read",
                "activity",
                &json!({"app": CHAT, "person": self.person}),
            )),
            2 => {
                // What the read answered, as its model reads it: the changes it must take back.
                let changes: Vec<String> = request
                    .messages
                    .iter()
                    .flat_map(|message| message.content.iter())
                    .filter_map(|content| match content {
                        Content::ToolResult(result) => Some(changes_named(&result.content)),
                        _ => None,
                    })
                    .flatten()
                    .collect();
                Some(tool_call(
                    "undo",
                    "undo",
                    &json!({"app": CHAT, "person": self.person, "changes": changes}),
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
/// The changes a read's answer names, as its model reads them: each version's `change`, wherever
/// the answer carries the versions it read.
fn changes_named(answer: &str) -> Vec<String> {
    let unquoted = answer.replace("\\\"", "\"").replace("\": \"", "\":\"");
    unquoted
        .match_indices("\"change\":\"")
        .filter_map(|(at, said)| {
            let rest = &unquoted[at + said.len()..];
            rest.split('"').next().map(str::to_owned)
        })
        .collect()
}

fn tool_call(id: &str, action: &str, arguments: &Value) -> Content {
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

/// Make `scout` through the workspace's typed create, with the moderation watch: what a
/// Moderator's device asks.
async fn create_watching_agent(
    workspaces: &tilo_workspace_service::workspaces::Workspaces,
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

/// The device a moderation turn runs on: the person's workspace with Chat declared, Scout
/// watching it, and the grants its undo is admitted by.
async fn moderation_device() -> super::Device {
    let device = super::Device::start("Moderation", Some(CHAT)).await;
    let owner = device.person();
    create_watching_agent(device.workspaces(), &device.workspace, &device.cert).await;
    grant_scout(device.workspaces(), &device.workspace, &owner).await;
    device
}

/// The model the device runs Scout's turns with, keeping what it was asked.
fn scripted(person: String) -> (Model, Arc<Mutex<Vec<ModelRequest>>>) {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let model: Model = {
        let asked = Arc::clone(&asked);
        Arc::new(move || {
            Some(Box::new(Scripted {
                person: person.clone(),
                asked: Arc::clone(&asked),
            }) as Box<dyn PlanTurns>)
        })
    };
    (model, asked)
}

/// The workspace lets `scout` change its records, so its undo is admitted.
async fn grant_scout(
    workspaces: &tilo_workspace_service::workspaces::Workspaces,
    workspace: &str,
    owner: &str,
) {
    let grant = Grant::new(
        "scout-chat",
        Grantee::agent(SCOUT.to_owned(), owner.to_owned()),
        TreePath::workspace(workspace.to_owned()),
        KindSet::READ_UNDOABLE,
        tilo_action_policy::Answer::Allow,
        Grantee::person(owner.to_owned()),
    );
    answer_grant(workspaces, workspace, grant).await.unwrap();
}

/// The channel `general`, made by the workspace's agent: the person's own changes are only the
/// messages they post in it.
async fn general(
    workspaces: &tilo_workspace_service::workspaces::Workspaces,
    workspace: &str,
    owner: &[u8; 32],
) {
    let scout = tilo_actions::remote::Caller::delegated_agent(SCOUT, *owner, [7; 32])
        .in_workspace(workspace);
    tilolang_workspace::records::provided::calls::call(
        workspaces,
        &scout,
        CHAT,
        "new_channel",
        json!({"name": "general"}),
    )
    .await
    .expect("the agent makes the channel");
}

/// The person posts `words` in the channel, through tl Chat's `send`.
async fn post(
    workspaces: &tilo_workspace_service::workspaces::Workspaces,
    workspace: &str,
    cert: &tilo_user_identity::DeviceCert,
    words: &str,
) {
    let caller =
        tilo_actions::remote::Caller::authenticated(cert.user_public_key, cert.device_public_key)
            .in_workspace(workspace);
    tilolang_workspace::records::provided::calls::call(
        workspaces,
        &caller,
        CHAT,
        "send",
        json!({"body": words}),
    )
    .await
    .expect("the person posts");
}

/// The versions `person` wrote by actions of theirs, as the record's history lists them: not what
/// the platform wrote for them, such as the record's root.
fn versions_of<'a>(versions: &'a [Value], person: &str) -> Vec<&'a Value> {
    let who = serde_json::to_value(tilo_workspace_service::tl_records::member(
        tilo_actions::MemberId::person(person.to_owned()),
    ))
    .unwrap();
    versions
        .iter()
        .filter(|version| version["who"] == who)
        .filter(|version| {
            !version["action"]
                .as_str()
                .unwrap_or_default()
                .starts_with('$')
        })
        .collect()
}

/// The changes `person` made by actions of theirs. A change that wrote several resources is a
/// version of each, and one change.
fn changes_of(versions: &[Value], person: &str) -> Vec<String> {
    versions_of(versions, person)
        .into_iter()
        .filter_map(|version| version["change"].as_str())
        .map(str::to_owned)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// A chat message's change, as a trigger hands it on.
fn posted(
    workspace: &str,
    person: &str,
    message: &str,
) -> tilo_automation::dispatch::DocumentEvent {
    tilo_automation::dispatch::DocumentEvent {
        invoked: None,
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

/// The agent's turn, under the agent's own key, reads the person's two changes and takes exactly
/// them back as one run, under the call it signed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_moderation_turn_reads_a_persons_changes_and_takes_them_back_in_one_run() {
    super::set_surfaces();
    let device = moderation_device().await;
    let cert = &device.cert;
    let workspaces = device.workspaces();
    let workspace = device.workspace.clone();
    let owner = device.person();

    general(workspaces, &workspace, &cert.user_public_key).await;
    post(workspaces, &workspace, cert, "a spam offer").await;
    post(workspaces, &workspace, cert, "and another").await;
    let before = workspaces
        .record_changes(&workspace, &super::record_route(&workspace, CHAT), &owner)
        .unwrap();
    let taken = changes_of(&before, &owner);
    assert!(
        taken.len() >= 2,
        "the person's posts are in the record's history: {before:#?}"
    );

    let (model, asked) = scripted(owner.clone());
    let provider = Arc::new(tilo_actions::remote::TestProvider::new());
    // Scout asks under its own key, which the person's root certified for it: no device of the
    // person's signs for it.
    let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
    let keys = super::CertifiedKeys::of(&root, &workspace, SCOUT);
    let agents =
        ServerAgents::on_device(workspaces.clone(), provider, Arc::new(keys)).scripted(model);
    let event = posted(&workspace, &owner, "m-1");
    let place = tilo_workspace_service::work::Where::of(&event).expect("a message is a place");
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
        matches!(started, tilo_workspace_service::work::Started::Taken(_)),
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
        versions_of(&before, &owner).len(),
        "only the person's changes: {shown}"
    );

    // The undo took exactly those back, as one run.
    let after = workspaces
        .record_changes(&workspace, &super::record_route(&workspace, CHAT), &owner)
        .unwrap();
    let undone: Vec<&Value> = after
        .iter()
        .filter(|version| {
            version["action"].as_str() == Some(tilolang_workspace::runtime::store::STEP_ACTION)
        })
        .collect();
    assert_eq!(
        undone.len(),
        taken.len(),
        "each change is taken back; the undo answered {:?}; the record's history is {:?}",
        asked.last().and_then(|request| request.messages.last()),
        after
            .iter()
            .map(|version| {
                (
                    version["action"].as_str().unwrap_or_default(),
                    version["change"].as_str().unwrap_or_default(),
                    version["invocation"].as_str().unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>()
    );
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
    // The agent's own call took the changes back, under its own key: every undo names that one
    // invocation, none of the person's, and the workspace admitted it as the step the call
    // declares.
    let by_call: std::collections::BTreeSet<&str> = undone
        .iter()
        .map(|version| version["invocation"].as_str().unwrap_or_default())
        .collect();
    let theirs: std::collections::BTreeSet<&str> = versions_of(&before, &owner)
        .iter()
        .map(|version| version["invocation"].as_str().unwrap_or_default())
        .collect();
    assert!(
        by_call.len() == 1 && !by_call.contains("") && by_call.is_disjoint(&theirs),
        "the undo names the call that took the changes back: {by_call:?}, theirs {theirs:?}"
    );
}

/// The same turn, saved by someone who may not change the record: the read is refused and
/// nothing is taken back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_moderation_turn_is_refused_when_the_saver_may_not() {
    super::set_surfaces();
    let device = moderation_device().await;
    let cert = &device.cert;
    let workspaces = device.workspaces();
    let workspace = device.workspace.clone();
    let owner = device.person();
    general(workspaces, &workspace, &cert.user_public_key).await;
    post(workspaces, &workspace, cert, "a spam offer").await;

    let (model, asked) = scripted(owner.clone());
    let provider = Arc::new(tilo_actions::remote::TestProvider::new());
    let agents =
        ServerAgents::on_device(workspaces.clone(), provider, Arc::new(super::Keys([7; 32])))
            .scripted(model);
    let saver = "c2".repeat(32); // a member whose grants do not reach the record
    let event = posted(&workspace, &owner, "m-1");
    let place = tilo_workspace_service::work::Where::of(&event).expect("a message is a place");
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
    assert!(matches!(
        started,
        tilo_workspace_service::work::Started::Taken(_)
    ));

    let asked = asked.lock().unwrap().clone();
    let shown = tool_results(&asked[1]);
    assert!(
        shown.contains("not_permitted"),
        "the turn is told it may not: {shown}"
    );
    let after = workspaces
        .record_changes(&workspace, &super::record_route(&workspace, CHAT), &owner)
        .unwrap();
    assert!(
        !after.iter().any(|version| version["action"]
            .as_str()
            .is_some_and(|action| action == tilolang_workspace::runtime::store::STEP_ACTION)),
        "a refused turn takes nothing back: {after:#?}"
    );
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
