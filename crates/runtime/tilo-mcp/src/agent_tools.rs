//! The MCP tools as a turn works through them ([`tilo_agent_turn::Tools`]), and the agent tools a
//! device's session answers for an agent whose turn is worked in a process of its own
//! ([`tilo_actions::remote::agent_tools`]).
//!
//! An agent a server keeps in its own process holds an [`McpServer`] and asks it directly. An agent
//! in a process of its own (the Agent app) holds none: it asks the device's session, which answers
//! both of its steps here with one [`McpServer`] for each agent session, over the API the broker hands the session's
//! latest step. Every call is made through that API, so it is the agent's, in its session: decided
//! on its grants, asked of its person first where the workspace says so, and named in History as
//! the session, exactly as when the agent held the server itself.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use agent_provider_config::protocol::ToolDefinition;
use serde_json::{Value, json};
use tilo_actions::remote::agent_tools as wire;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, ConfirmedInvocation,
    InvocationReceipt, InvocationState, InvokeRequest, Pending, WatchScope,
};
use tilo_actions::{ActionFailure, ActionOutput, InvocationId};
use tilo_agent_turn::Answer;
use tilo_lock::MutexRecover as _;

use crate::McpServer;

impl<H: ActionProvider + Send + Sync + 'static> tilo_agent_turn::Tools for McpServer<H> {
    async fn list(&self) -> Result<Vec<ToolDefinition>, String> {
        let listed = self
            .handle(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
            .await
            .ok_or("the agent's tools did not answer")?;
        let tools = listed
            .pointer("/result/tools")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("the agent's tools answered {listed}"))?;
        Ok(tools
            .iter()
            .filter_map(|tool| {
                Some(ToolDefinition {
                    name: tool.get("name")?.as_str()?.to_owned(),
                    description: tool
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    input_schema: tool
                        .get("inputSchema")
                        .cloned()
                        .unwrap_or_else(|| json!({"type": "object"})),
                })
            })
            .collect())
    }

    async fn call(&self, name: &str, arguments: Value) -> Answer {
        let answer = self
            .handle(json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {"name": name, "arguments": arguments},
            }))
            .await;
        let Some(answer) = answer else {
            return Answer::failed("the tool did not answer");
        };
        if let Some(error) = answer.get("error") {
            return Answer::failed(
                error
                    .get("message")
                    .and_then(Value::as_str)
                    .map_or_else(|| error.to_string(), str::to_owned),
            );
        }
        answer_of(&answer.get("result").cloned().unwrap_or(Value::Null))
    }
}

/// A `tools/call` result as the model reads it: its text blocks joined, its image blocks kept.
#[must_use]
pub fn answer_of(result: &Value) -> Answer {
    let is_error = result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut texts = Vec::new();
    let mut pictures = Vec::new();
    for block in result
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match block.get("type").and_then(Value::as_str) {
            Some("image") => {
                if let (Some(data), Some(mime)) = (
                    block.get("data").and_then(Value::as_str),
                    block.get("mimeType").and_then(Value::as_str),
                ) {
                    pictures.push((mime.to_owned(), data.to_owned()));
                }
            }
            _ => texts.extend(block.get("text").and_then(Value::as_str)),
        }
    }
    let mut text = texts.join("\n");
    if text.is_empty() && pictures.is_empty() {
        text = result.to_string();
    }
    Answer {
        text,
        pictures,
        is_error,
    }
}

type Provider = Arc<dyn ActionProvider + Send + Sync>;

/// One agent session's tools, over the API of the session's latest step.
type Server = McpServer<Arc<Latest>>;

/// An agent session, by the workspace it works in, its agent and its id.
type SessionKey = (String, String, String);

/// Each agent session's server, with the API it calls through.
type Sessions = HashMap<SessionKey, (Arc<Latest>, Arc<Server>)>;

/// One step of an agent session's tools, as the broker admitted it.
pub struct Step<'a> {
    pub workspace: &'a str,
    /// The agent, by its bare id.
    pub agent: &'a str,
    /// The person the agent works for, by their person id.
    pub person: &'a str,
    pub session: &'a str,
    /// The session's own API: every call is made through it.
    pub provider: Provider,
    /// [`wire::LIST`] or [`wire::CALL`].
    pub action: &'a str,
    /// Its argument, as the invocation carries it.
    pub argument: Option<&'a str>,
}

/// How a step ended.
pub type StepOutcome = Result<ActionOutput, Box<ActionFailure>>;

/// The agent sessions' tools a device's session answers.
#[derive(Default)]
pub struct AgentTools {
    sessions: Mutex<Sessions>,
}

impl AgentTools {
    /// Start `step`: its outcome arrives once the tool answered. A call that waits (on the
    /// person's answer, on a long action) holds no other step up.
    ///
    /// # Errors
    /// The step is not one of the agent tools, or its argument does not read.
    pub fn start(&self, step: &Step<'_>) -> Result<Pending<StepOutcome>, String> {
        let asked = match step.action {
            wire::LIST => None,
            wire::CALL => {
                let carried = step
                    .argument
                    .and_then(|carried| carried.strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX))
                    .ok_or("a tool call names its tool")?;
                Some(
                    serde_json::from_str::<wire::ToolCall>(carried)
                        .map_err(|error| format!("a tool call does not read: {error}"))?,
                )
            }
            _ => return Err("unknown step of an agent's tools".into()),
        };
        let server = self.session_of(step);
        let (complete, answered) = tilo_actions::remote::pending();
        // A call waits on the tools and on every action it takes: never on the broker's thread,
        // nor on the runtime's own.
        std::thread::Builder::new()
            .name("agent-tool".into())
            .spawn(move || {
                let ran = match tilo_rt::try_app() {
                    Ok(runtime) => runtime
                        .block_on(answer(&server, asked))
                        .unwrap_or_else(|error| Err(Box::new(unavailable(error.to_string())))),
                    Err(error) => Err(Box::new(unavailable(error.to_string()))),
                };
                complete.complete(ran);
            })
            .map_err(|error| error.to_string())?;
        Ok(answered)
    }

    /// The agent session's server, started when it has none, calling through the step's API: the
    /// one the broker serves the session's latest step on, so everything a call does is a step
    /// the session admits even as one turn of it follows another.
    fn session_of(&self, step: &Step<'_>) -> Arc<Server> {
        let key = (
            step.workspace.to_owned(),
            step.agent.to_owned(),
            step.session.to_owned(),
        );
        let mut sessions = self.sessions.lock_recover();
        if let Some((latest, server)) = sessions.get(&key) {
            *latest.0.lock_recover() = Arc::clone(&step.provider);
            return Arc::clone(server);
        }
        let latest = Arc::new(Latest(Mutex::new(Arc::clone(&step.provider))));
        let server = Arc::new(McpServer::for_agent(
            Arc::clone(&latest),
            step.workspace.to_owned(),
            step.agent,
            step.person,
        ));
        sessions.insert(key, (latest, Arc::clone(&server)));
        server
    }
}

/// What step `asked` (a call, or the listing when `None`) answered on `server`.
async fn answer(server: &Server, asked: Option<wire::ToolCall>) -> StepOutcome {
    use tilo_agent_turn::Tools as _;
    let value = match asked {
        None => {
            let tools = server
                .list()
                .await
                .map_err(|why| Box::new(unavailable(why)))?;
            serde_json::to_value(wire::ToolList {
                tools: tools
                    .into_iter()
                    .map(|tool| wire::Tool {
                        name: tool.name,
                        description: tool.description,
                        input_schema: tool.input_schema,
                    })
                    .collect(),
            })
        }
        Some(call) => {
            let answer = server.call(&call.name, call.arguments).await;
            serde_json::to_value(wire::ToolAnswer {
                text: answer.text,
                pictures: answer
                    .pictures
                    .into_iter()
                    .map(|(media_type, base64)| wire::Picture { media_type, base64 })
                    .collect(),
                failed: answer.is_error,
            })
        }
    };
    Ok(ActionOutput {
        value: Some(value.map_err(|error| Box::new(unavailable(error.to_string())))?),
        resources: Vec::new(),
    })
}

/// The tools could not answer at all.
fn unavailable(message: String) -> ActionFailure {
    ActionFailure {
        code: "agent_tools_unavailable".into(),
        message,
        details: None,
        retryable: true,
    }
}

/// The API the broker serves an agent session's latest step on: each turn of the session is served
/// on its own, and its tools call through the turn going now.
pub struct Latest(Mutex<Provider>);

impl Latest {
    fn provider(&self) -> Provider {
        Arc::clone(&self.0.lock_recover())
    }
}

impl ActionProvider for Latest {
    fn retire_execution(&self, caller: &Caller) {
        self.provider().retire_execution(caller);
    }

    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        self.provider().query(caller, query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        self.provider().invoke(caller, request)
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.provider().observe(caller, id)
    }

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        self.provider().next_change(caller, id)
    }

    fn session_state(&self, caller: &Caller) -> Option<tilo_actions::remote::SessionState> {
        self.provider().session_state(caller)
    }

    fn session_state_changed(&self, caller: &Caller) -> Option<Pending<()>> {
        self.provider().session_state_changed(caller)
    }

    fn changed(
        &self,
        caller: &Caller,
        scope: WatchScope,
    ) -> Option<Pending<Result<(), ActionCallError>>> {
        self.provider().changed(caller, scope)
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        self.provider().cancel(caller, id)
    }

    fn confirm(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Pending<Result<ConfirmedInvocation, ActionCallError>> {
        self.provider().confirm(caller, request)
    }

    fn content_puts(&self) -> Option<Arc<dyn tilo_actions::remote::ContentPuts>> {
        self.provider().content_puts()
    }

    fn content_gets(&self) -> Option<Arc<dyn tilo_actions::remote::ContentGets>> {
        self.provider().content_gets()
    }

    fn here(&self, caller: &Caller) -> Option<Vec<tilo_actions::remote::Here>> {
        self.provider().here(caller)
    }

    fn resource_reads(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceReads>> {
        self.provider().resource_reads()
    }

    fn resource_changes(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceChanges>> {
        self.provider().resource_changes()
    }

    fn resource_search(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceSearch>> {
        self.provider().resource_search()
    }

    fn change_feed(&self) -> Option<Arc<dyn tilo_actions::remote::ChangeFeed>> {
        self.provider().change_feed()
    }

    fn installed_surface(&self, app_id: &str) -> Pending<Option<Vec<u8>>> {
        self.provider().installed_surface(app_id)
    }
}
