//! A whole turn, through the production session over a stand-in host: the turn's model lists the
//! workspace's MCP tools, reads and changes through them, and every request it makes reaches the
//! host as a remote-action message on the agent's own link, which the host decides; the reply
//! and its steps are recorded on the conversation through Chat's `record-work`, and the turn's
//! answer ends with the model's words.

use std::collections::VecDeque;
use std::sync::Mutex;

use agent_provider_config::protocol::{
    Content, Message, ModelRequest, ModelResponse, Role, StopReason, ToolCall, Usage,
};
use serde_json::{Value, json};
use tilo_actions::remote::{Caller, RemoteActionMessage, TestProvider};
use tilo_actions::{ActionContract, ActionOutput, InvocationUpdate};
use ui::collaboration::{BrokerActionSurface, CatalogueChange, ExportedAction, ExportedInput};

use super::*;

/// The conversation's message the turn answers.
const MESSAGE: &str = "chat.workspace/channels/c1/messages/asked";

/// The host's side of the agent's link: remote-action messages are served by `provider` as the
/// turn's, and everything the agent sends is kept.
struct Host {
    provider: Arc<TestProvider>,
    runtime: tokio::runtime::Runtime,
    out: VecDeque<CollabEvent>,
    sent: Arc<Mutex<Vec<CollabRequest>>>,
}

impl ui::collaboration::PeerTransport for Host {
    fn is_connected(&self) -> bool {
        true
    }

    fn send(&mut self, request: CollabRequest) -> std::result::Result<(), String> {
        if let CollabRequest::RemoteAction { message } = &request {
            let caller = Caller::local_person("person").in_workspace("w");
            if let Some(answer) =
                tilo_actions::remote::serve(&self.provider, &caller, message.clone())
                && let Some(message) = self.runtime.block_on(answer)
            {
                self.out.push_back(CollabEvent::RemoteAction { message });
            }
        }
        self.sent.lock().unwrap().push(request);
        Ok(())
    }

    fn poll(&mut self) -> Vec<CollabEvent> {
        self.out.drain(..).collect()
    }
}

/// Answers each round with the next scripted message.
struct Script(Arc<Mutex<VecDeque<Message>>>);

impl tilo_agent_turn::Model for Script {
    fn turn(
        &mut self,
        _: &ModelRequest,
        _: &str,
        _: std::time::Instant,
    ) -> Result<ModelResponse, String> {
        let message = self
            .0
            .lock()
            .unwrap()
            .pop_front()
            .ok_or("script exhausted")?;
        let calls = message
            .content
            .iter()
            .any(|content| matches!(content, Content::ToolCall(_)));
        Ok(ModelResponse {
            message,
            stop_reason: if calls {
                StopReason::ToolCalls
            } else {
                StopReason::EndTurn
            },
            usage: Usage::default(),
            cost_micros: 0,
        })
    }
}

fn calling(id: &str, name: &str, input: Value) -> Message {
    Message {
        role: Role::Assistant,
        content: vec![Content::ToolCall(ToolCall {
            id: id.into(),
            name: name.into(),
            input,
            extra_content: None,
        })],
    }
}

/// Chat's surface, offering `record-work` on the turn's message.
fn chat() -> BrokerActionSurface {
    BrokerActionSurface {
        peer_id: 7,
        provider: None,
        app_id: "so.tilo.chat".into(),
        cluster_id: Uuid::from_u128(1).to_string(),
        document_id: String::new(),
        live: true,
        actions: vec![ExportedAction {
            did: None,
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            resource: MESSAGE.into(),
            action: "record-work".into(),
            resource_kind: "chat.message".into(),
            resource_label: "asked".into(),
            action_label: "Record work".into(),
            keywords: Vec::new(),
            icon: None,
            description: None,
            operation_kind: tilo_actions::OperationKind::Effect,
            params: Vec::new(),
            contract: ActionContract::default(),
            view: None,
            input: ExportedInput::None,
        }],
    }
}

fn session(
    provider: Arc<TestProvider>,
    answers: Vec<Message>,
) -> (Session, Arc<Mutex<Vec<CollabRequest>>>) {
    let sent = Arc::default();
    let host = Host {
        provider,
        runtime: tokio::runtime::Runtime::new().unwrap(),
        out: VecDeque::from([CollabEvent::ActionCatalogue {
            change: CatalogueChange::snapshot(&[chat()]),
        }]),
        sent: Arc::clone(&sent),
    };
    let link = crate::wire::test_link(host);
    let cfg = Config {
        agent_id: "agent-1".into(),
        endpoint: None,
        app_id: tilo_automation::AGENT_APP_ID.into(),
        cluster: Uuid::from_u128(1),
        record: None,
        name: "Ada".into(),
        model: true,
        model_timeout: Duration::from_secs(5),
        content: ui::content::ContentStore::memory(),
        provider: None,
        identity: None,
    };
    let selection = ProviderSelection {
        profile_id: "p".into(),
        profile_label: "Provider".into(),
        endpoint: String::new(),
        protocol: agent_provider_config::ProviderProtocol::Anthropic,
        model: agent_provider_config::ModelDescriptor::new("m"),
        credential: None,
    };
    let mut session = Session::over(cfg, link, None).unwrap();
    let script = Arc::new(Mutex::new(VecDeque::from(answers)));
    session.set_model(
        selection,
        Arc::new(move |_| Box::new(Script(Arc::clone(&script))) as Box<dyn tilo_agent_turn::Model>),
    );
    (session, sent)
}

fn asked() -> Asked {
    Asked {
        id: 3,
        invocation: tilo_actions::InvocationId::new(),
        asked: runtime_collab::TurnRequest {
            message: MESSAGE.into(),
            title: "Notes".into(),
            instruction: "Make a note".into(),
            agent_instruction: None,
            fold: None,
            subject: None,
            spend: None,
        },
        context: None,
    }
}

/// What the agent invoked through the host, by action.
fn invoked(provider: &TestProvider) -> Vec<(String, Value)> {
    provider
        .asked()
        .into_iter()
        .map(|request| {
            let argument = match request.argument {
                tilo_actions::remote::InvokeArgument::Json(map) => Value::Object(map),
                _ => Value::Null,
            };
            (request.target.address.action, argument)
        })
        .collect()
}

#[test]
fn a_turn_works_through_the_mcp_tools_and_records_its_reply() {
    let provider = Arc::new(TestProvider::new().completing(|_| ActionOutput::default()));
    let (mut session, sent) = session(
        Arc::clone(&provider),
        vec![
            calling("1", "tilo_ls", json!({})),
            calling("2", "tilo_do", json!({"path": "notes#open"})),
            Message::text(Role::Assistant, "I made the note."),
        ],
    );
    let run = asked();
    session.run_task(&run).unwrap();

    // The turn's answer ends with the model's words, after its progress.
    let updates: Vec<InvocationUpdate> = sent
        .lock()
        .unwrap()
        .iter()
        .filter_map(|request| match request {
            CollabRequest::UpdateInvocation { id: 3, update } => Some(update.clone()),
            _ => None,
        })
        .collect();
    assert!(
        updates
            .iter()
            .any(|update| matches!(update, InvocationUpdate::Progress { .. })),
        "{updates:?}"
    );
    match updates.last() {
        Some(InvocationUpdate::Succeeded { output, .. }) => {
            assert_eq!(output.value, Some(json!({"reply": "I made the note."})));
        }
        other => panic!("the turn did not succeed: {other:?}"),
    }
    // Every request the turn made went to the host as a remote action on the agent's link.
    assert!(
        sent.lock().unwrap().iter().any(|request| matches!(
            request,
            CollabRequest::RemoteAction {
                message: RemoteActionMessage::CatalogueRequest { .. }
            }
        )),
        "the tools asked the host's catalogue"
    );
    // The reply was told on the conversation: taken, the step that changed something, and the
    // answer last.
    let records: Vec<Value> = invoked(&provider)
        .into_iter()
        .filter(|(action, _)| action == "record-work")
        .map(|(_, argument)| argument)
        .collect();
    assert_eq!(records.first().unwrap()["work"]["state"], "taken");
    let last = records.last().unwrap();
    assert_eq!(last["work"]["state"], "provider_complete");
    assert_eq!(last["say"], "I made the note.");
    assert_eq!(last["message"], "asked");
}

#[test]
fn a_turn_without_a_model_says_so() {
    let provider = Arc::new(TestProvider::new());
    let (mut session, sent) = session(provider, Vec::new());
    session.model = None;
    session.run_task(&asked()).unwrap();
    let failed = sent
        .lock()
        .unwrap()
        .iter()
        .find_map(|request| match request {
            CollabRequest::UpdateInvocation {
                update: InvocationUpdate::Failed { error, .. },
                ..
            } => Some(error.message.clone()),
            _ => None,
        });
    assert!(
        failed.is_some_and(|said| said.contains("Agent models")),
        "the person reads where to choose a model"
    );
}

/// A turn's answer declares its reply as its result ([`answer_descriptor`]): the reply the
/// agent reports is admitted by the broker, and a result without one is not.
#[test]
fn a_turns_result_is_its_reply() {
    let contract = answer_descriptor().contract;
    let replied = ActionOutput {
        value: Some(json!({"reply": "Hi! I work across your Notes."})),
        resources: Vec::new(),
    };
    assert!(contract.result.validate(&replied).is_ok());
    assert!(contract.result.validate(&ActionOutput::default()).is_err());
}
