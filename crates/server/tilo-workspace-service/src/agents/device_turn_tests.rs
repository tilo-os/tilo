//! A turn answered by the engine on a person's own device: the same engine a workspace's host
//! runs, in the device's own service, its calls decided by the device's broker and signed with the
//! key the device makes for the agent.
//!
//! Everything here is the production path a native desktop takes: the workspace is the device's
//! own, the agent and the conversation are the records Chat and the agent app publish, the turn is
//! read from them (`ServerAgents::turns`), and the answer goes through the agent's tools as the
//! agent — here a scripted model standing where the workspace's linked model would be.

#![allow(
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture failing is its test failing"
)]

use std::sync::{Arc, Mutex};

use serde_json::Value;
use tilo_actions::MemberId;
use tilo_actions::remote::InvokeRequest;
use tilo_agent_session::model::PlanTurns;
use tilo_agent_session::{AppCatalogue, Replica, ReplicaActor, Seed};
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
    let agents_record = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
    workspaces
        .author_workspace_document(
            &workspace,
            &cert,
            (tilo_workspace_agent::OWNER, &agents_record),
            |doc| {
                let mut made = tilo_resource::Replica::<WorkspaceAgents>::new().as_author(&person);
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
            },
        )
        .await
        .unwrap();

    // The person writes to it, as their own window's composer does.
    let conversation = chat::Chat::direct_id([MemberId::person(&person), MemberId::agent(SCOUT)]);
    let posted = {
        let mut page = Replica::open(
            &catalogue,
            CHAT,
            &workspace,
            &MemberId::person(&person),
            ReplicaActor::Owner,
            Seed::New,
        )
        .unwrap();
        page.invoke(
            &tilo_actions::api::ActionAddress::new(
                collab_types::document::chain_resource_id(&chat::parts::chain()),
                "post",
            ),
            &serde_json::Map::from_iter([
                ("channel".to_owned(), conversation.as_str().into()),
                ("text".to_owned(), "what is planned?".into()),
            ]),
            false,
        )
        .unwrap();
        page.take_document_mutations().unwrap()
    };
    for (document, mutation) in posted {
        let collab_types::DocumentMutation::Snapshot(bytes) = mutation else {
            continue;
        };
        let document = document.as_str();
        workspaces
            .author_workspace_document(&workspace, &cert, (CHAT, document), |doc| {
                tilo_automerge::ApplyIncoming::load_incoming(doc, &bytes)
                    .map_err(|error| error.to_string())
            })
            .await
            .unwrap();
    }

    // The engine on this device reads the turn its workspace owes, and works it with the scripted
    // model through the device's broker.
    let provider = Arc::new(tilo_actions::remote::TestProvider::new());
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
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(
        turns.len(),
        1,
        "the message is the agent's one turn: {turns:?}"
    );
    assert_eq!(turns[0].person, person, "it works for whoever wrote to it");
    assert!(
        turns[0].request.contains("what is planned?"),
        "the turn asks what the person wrote: {}",
        turns[0].request
    );
    tokio::time::timeout(
        std::time::Duration::from_secs(30),
        agents.answer(&workspace, &turns[0]),
    )
    .await
    .expect("the turn ends")
    .unwrap();

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

    // What the turn did went through the device's broker as the agent: the note it was doing, and
    // the answer it wrote into its reply. It says first that it took the turn, then what it is at.
    let invoked = provider.asked();
    let note = invoked
        .iter()
        .filter(|request| request.target.address.action.as_str() == "record-work")
        .map(|request| argument(request, "work"))
        .find(|work| work["state"] == "doing")
        .unwrap_or_else(|| panic!("the turn says what it is doing: {invoked:?}"));
    assert_eq!(
        note["did"], "Looking at what the workspace can do…",
        "{note}"
    );
    let answered = invoked
        .iter()
        .filter(|request| request.target.address.action.as_str() == "record-work")
        .map(|request| argument(request, "say"))
        .any(|said| {
            said.as_str()
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
