//! A turn answered by the engine on a person's own device: the same engine a workspace's host
//! runs, in the device's own service, its calls decided by the device's broker and signed with the
//! key the device makes for the agent.
//!
//! Everything here is the production path a native desktop takes: the workspace is the device's
//! own, the agent is the record the agent app publishes, the turn is started by the change its
//! trigger fired on (`ServerAgents::triggered`), and the answer goes through the agent's tools as
//! the agent into a reply of Chat — here a scripted model standing where the workspace's linked
//! model would be.

#![allow(
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture failing is its test failing"
)]

use std::sync::{Arc, Mutex};

use serde_json::Value;
use tilo_actions::remote::InvokeRequest;
use tilo_agent_session::AppCatalogue;
use tilo_agent_session::model::PlanTurns;
use tilo_model_gateway::host_backend::protocol::{
    Content, Message, ModelError, ModelRequest, ModelResponse, Role, StopReason, Usage,
};
use tilo_server_protocol::WorkspaceId;
use tilo_workspace_agent::{WorkspaceAgents, workspace_agents};

use super::agent_keys::AgentKeys;
use super::{Model, ServerAgents};

const CHAT: &str = "so.tilo.chat";
const SCOUT: &str = "scout";

/// A model whose turns are written out: what each request asked is kept, so a test reads what the
/// engine on this device told it.
struct Scripted {
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
        let answer = if self.asked.lock().unwrap().len() == 1 {
            ModelResponse {
                message: Message {
                    role: Role::Assistant,
                    content: vec![Content::ToolCall(
                        tilo_model_gateway::host_backend::protocol::ToolCall {
                            id: "call-1".into(),
                            name: "tilo_ls".into(),
                            input: serde_json::json!({}),
                            extra_content: None,
                        },
                    )],
                },
                stop_reason: StopReason::ToolCalls,
                usage: Usage::default(),
                cost_micros: 0,
            }
        } else {
            ModelResponse {
                message: Message {
                    role: Role::Assistant,
                    content: vec![Content::Text {
                        text: "The plan is ready for you.".into(),
                    }],
                },
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                cost_micros: 0,
            }
        };
        Ok(answer)
    }

    fn model(&self) -> &'static str {
        "scripted"
    }
}

/// The keys a person's device makes for its agents: one fixed key here, which the test's provider
/// takes as the agent's own.
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
        _request: &mut InvokeRequest,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_of_a_persons_own_agent_is_answered_on_their_device() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = crate::workspaces::fixtures::governor();
    let catalogue = AppCatalogue::native();
    // The native apps name their surfaces as the host first runs them.
    crate::surfaces::decide_receipt(&scratch.path().join("surfaces"));
    let server = crate::workspaces::fixtures::test_host(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let workspace = workspaces.create("Launch").await.unwrap().workspace;
    let person = hex::encode(cert.user_public_key);

    // The workspace's chat, and its record of the agents it keeps.
    let chat_record = crate::agents::workspace_resource(&catalogue, CHAT);
    workspaces
        .prepare_application(&workspace, &cert, &catalogue, CHAT, &chat_record)
        .await
        .unwrap();
    workspaces
        .author_agents(&workspace, |doc| {
            let mut made = tilo_resource::Replica::<WorkspaceAgents>::from_doc(std::mem::take(doc))
                .as_author(&person);
            made.apply(
                WorkspaceAgents::CREATE_AGENT.with(workspace_agents::CreateAgent {
                    name: tilo_resource::NonBlank::parse("Scout").unwrap(),
                    id: Some(SCOUT.into()),
                    model: None,
                    mcp_client: None,
                }),
            )
            .map_err(|error| error.to_string())?;
            std::mem::swap(doc, made.doc_mut());
            Ok(())
        })
        .await
        .unwrap();

    // The person writes to it in their direct conversation with it, and the agent's trigger
    // invokes its answer with that change.
    let wrote = message_written(&workspace, &person, "what is planned?");
    let provider = Arc::new(
        tilo_actions::remote::TestProvider::new().completing(|request| {
            tilo_actions::ActionOutput {
                value: None,
                resources: if request.target.address.action == "send" {
                    vec!["tl:so.tilo.chat:reply-1".to_owned()]
                } else {
                    Vec::new()
                },
            }
        }),
    );
    let asked = Arc::new(Mutex::new(Vec::new()));
    let model: Model = {
        let asked = Arc::clone(&asked);
        Arc::new(move || {
            Some(Box::new(Scripted {
                asked: Arc::clone(&asked),
            }) as Box<dyn PlanTurns>)
        })
    };
    let agents = ServerAgents::on_device(workspaces.clone(), provider.clone(), Arc::new(Keys))
        .scripted(model);
    let invocation = tilo_actions::InvocationId::new();
    let started = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        agents.triggered(
            &workspace,
            SCOUT,
            wrote,
            person.clone(),
            (invocation, Arc::default()),
        ),
    )
    .await
    .expect("the turn ends")
    .unwrap();
    assert!(
        matches!(started, crate::work::Started::Taken(_)),
        "this device took the turn: {started:?}"
    );

    // The model was told the person's day and what was asked, on this device.
    let requests = asked.lock().unwrap().clone();
    assert_eq!(requests.len(), 2, "one tool call and one answer");
    let system = requests[0]
        .messages
        .iter()
        .find(|message| message.role == Role::System)
        .map(text_of)
        .unwrap_or_default();
    assert!(
        system.contains("Today is"),
        "the model reads the day: {system}"
    );
    assert!(
        requests[0]
            .messages
            .iter()
            .any(|message| text_of(message).contains("what is planned?")),
        "the model reads what the person asked"
    );

    // The reply is a message of Chat answering the person's, naming the turn's invocation, and
    // the answer is written into it, all as the agent.
    let invoked = provider.asked();
    let sent = invoked
        .iter()
        .find(|request| request.target.address.action == "send")
        .unwrap_or_else(|| panic!("the reply is sent: {invoked:?}"));
    assert_eq!(
        argument(sent, "turn"),
        Value::String(invocation.to_string())
    );
    assert_eq!(
        argument(sent, "reply_to"),
        Value::String("tl:so.tilo.chat:asked-1".to_owned())
    );
    let answered = invoked
        .iter()
        .filter(|request| request.target.address.action == "edit")
        .any(|request| {
            argument(request, "m") == "tl:so.tilo.chat:reply-1"
                && argument(request, "body")
                    .as_str()
                    .is_some_and(|said| said.contains("The plan is ready"))
        });
    assert!(
        answered,
        "the answer is written into the reply: {invoked:?}"
    );
    assert!(
        provider
            .callers()
            .iter()
            .any(|caller| caller.acting().agent_id() == Some(SCOUT)),
        "every call is the agent's own"
    );
}

/// What the trigger hands on when `person` wrote `text` to the agent in their direct
/// conversation with it in `workspace`.
fn message_written(
    workspace: &str,
    person: &str,
    text: &str,
) -> tilo_automation::dispatch::DocumentEvent {
    tilo_automation::dispatch::DocumentEvent {
        workspace: workspace.to_owned(),
        record: "asked-1".to_owned(),
        kind: super::triggered::MESSAGE.to_owned(),
        change: "change-1".to_owned(),
        actor: tilo_action_policy::Acting::person(person.to_owned()),
        did: tilo_actions::Did::verb(
            tilo_actions::ActionKey::new(CHAT, super::triggered::MESSAGE, "send"),
            [],
        ),
        arguments: serde_json::Map::new(),
        item: Some(tilo_automation::dispatch::ItemRef {
            kind: super::triggered::MESSAGE.to_owned(),
            id: "asked-1".to_owned(),
            label: String::new(),
        }),
        before: None,
        after: Some(serde_json::json!({
            "body": text,
            "$in": {"$id": "direct-1", "people": [{"person": person}, {"agent": SCOUT}]},
        })),
        origin: None,
        caused_by: None,
        incoming_message: false,
        housekeeping: false,
        shortcut: None,
        invocation: None,
    }
}

/// The text of `message`, whichever content part it holds it in.
fn text_of(message: &Message) -> String {
    message
        .content
        .iter()
        .filter_map(|content| match content {
            Content::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The argument `name` of `request`, as JSON.
fn argument(request: &InvokeRequest, name: &str) -> Value {
    match &request.argument {
        tilo_actions::remote::InvokeArgument::Json(arguments) => {
            arguments.get(name).cloned().unwrap_or(Value::Null)
        }
        other => panic!("the call carries JSON arguments: {other:?}"),
    }
}
