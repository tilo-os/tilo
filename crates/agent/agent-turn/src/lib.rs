//! One agent turn over the workspace's MCP tools.
//!
//! Every agent in Tilo works through one tool set: what an [`McpServer`] lists (`tilo_ls`,
//! `tilo_read`, `tilo_do`, `tilo_find`, `tilo_view`, `tilo_watch` and the rest). A turn lists the
//! server's `tools/list` as the model's tools, sends every call the model makes to the server's
//! `tools/call`, and hands the model what each answered, until the model answers in words, its
//! person stops it, its spending limit is reached or its rounds run out ([`work`]).
//!
//! What the turn runs on is the caller's: the server over the action provider of where the agent
//! runs (the session's on a device, [`carried`]; the host's on a server), the model that answers
//! ([`Model`]), and what the agent's person is told as it works ([`Doing`]). Tools of the caller's
//! own beside the server's (a web search, a view the answer is) are offered with
//! [`Asked::own_tools`] and answered by [`Doing::own`]. What its person writes while it works is
//! taken in between its rounds ([`Doing::went_on`]).

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agent_provider_config::protocol::{
    Content, Message, ModelRequest, ModelResponse, Role, StopReason, ToolCall, ToolChoice,
    ToolDefinition, ToolResult,
};
use serde_json::{Value, json};
use tilo_actions::remote::ActionProvider;
use tilo_lock::MutexRecover as _;
pub use tilo_mcp::McpServer;

mod carried;
mod compact;
pub use carried::{Carried, carried};
pub use tilo_session_client::remote::SessionActions;

/// How long the server may take to list its tools or answer one call.
pub const TOOL_TIME: Duration = Duration::from_secs(120);
/// The most bytes of one tool's words the model is handed; past it the answer says to ask for less.
pub const MOST_RESULT_BYTES: usize = 48 * 1024;
/// The most pictures one tool answer shows a model that sees.
const MOST_PICTURES: usize = 4;
/// How many answers in a row that could not be used are asked again before the turn ends.
const MOST_UNUSABLE: usize = 2;

/// What an agent that answered without words is asked, once.
const SAY_WHAT_YOU_DID: &str =
    "Answer the person now in a few plain sentences: what you did, and what you could not do.";
/// What an agent out of rounds is asked, with no tools, so its person hears where it got to.
const OUT_OF_STEPS: &str = "You have no steps left in this turn. Answer the person now in a few \
     plain sentences: what you did, what is still open, and how they can let you go on.";
/// What its person is told when an agent out of rounds said nothing either.
const STOPPED_UNFINISHED: &str =
    "I stopped after many steps without finishing. Tell me how to go on.";
/// What the model is told before what its person wrote while it worked.
pub const CAME_SINCE: &str = "While you were working, your person wrote again. Take it into your \
     work: answer what they wrote last, and keep anything from before that still holds.";
/// How much of a refused call's arguments and words its log line keeps.
const LOGGED_CHARS: usize = 300;
/// What the model reads before the reason its last answer was not used.
const UNUSABLE: &str = "Your last answer could not be used, and none of its calls ran";

/// The model a turn asks, holding its credential: one blocking call per round.
pub trait Model: Send {
    /// Answer `request` by `deadline`. `key` names the round, the same for every attempt at it.
    ///
    /// # Errors
    /// The model did not answer, in words its person reads.
    fn turn(
        &mut self,
        request: &ModelRequest,
        key: &str,
        deadline: Instant,
    ) -> Result<ModelResponse, String>;
}

impl Model for Box<dyn Model> {
    fn turn(
        &mut self,
        request: &ModelRequest,
        key: &str,
        deadline: Instant,
    ) -> Result<ModelResponse, String> {
        (**self).turn(request, key, deadline)
    }
}

/// What a turn tells its person as it works, and how they stop it. `H` is the action provider its
/// tools reach.
pub trait Doing<H>: Send + Sync {
    /// Whether the person stopped the turn: no tool is begun after a stop.
    fn stopped(&self) -> bool;
    /// The turn is about to run `call`.
    fn calling(&self, call: &ToolCall) -> impl Future<Output = ()> + Send {
        let _ = call;
        async {}
    }
    /// `call` answered `answer`.
    fn called(&self, call: &ToolCall, answer: &Answer) {
        let _ = (call, answer);
    }
    /// The turn's model calls cost `total` millionths of the currency by now.
    fn spent(&self, total: u64) {
        let _ = total;
    }
    /// Answer `call` when it is one of the caller's own tools ([`Asked::own_tools`]); `None` sends
    /// it to the server. Every call is asked here first.
    fn own(&self, call: &ToolCall) -> impl Future<Output = Option<Answer>> + Send {
        let _ = call;
        async { None }
    }
    /// What its person wrote since the turn's last step, taken into it; `None` when nothing came.
    ///
    /// # Errors
    /// What came cannot be taken in, in words its person reads: the turn ends with them.
    fn went_on(&self) -> impl Future<Output = Result<Option<WentOn<H>>, String>> + Send {
        async { Ok(None) }
    }
    /// Whether its person wrote while the model answered: the turn goes on with that rather than
    /// answering past it.
    fn waiting(&self) -> bool {
        false
    }
}

/// What its person wrote while a turn worked, as the turn takes it in.
pub struct WentOn<H> {
    /// What the model is told of it, after [`CAME_SINCE`].
    pub told: String,
    /// The server the turn's tools go on through, when what came is someone else's to act for.
    pub server: Option<McpServer<H>>,
}

/// What one tool call answered, as the model reads it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Answer {
    /// Its words.
    pub text: String,
    /// The pictures it showed, as `(media type, base64)`.
    pub pictures: Vec<(String, String)>,
    /// Whether it failed or was refused.
    pub is_error: bool,
}

impl Answer {
    /// A failure in `text`.
    #[must_use]
    pub fn failed(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            pictures: Vec::new(),
            is_error: true,
        }
    }
}

/// What a turn is asked to do, and its bounds.
#[derive(Clone, Debug)]
pub struct Asked {
    /// What the model is told before everything.
    pub system: String,
    /// The person's request, as the model reads it.
    pub request: String,
    /// The caller's own tools beside the server's, answered by [`Doing::own`].
    pub own_tools: Vec<ToolDefinition>,
    /// Whether the model is shown the pictures a tool answers with.
    pub sees: bool,
    /// What the turn may spend, in millionths of the currency; `None` for no limit.
    pub budget: Option<u64>,
    /// The most rounds of tool calls before the turn stops.
    pub most_rounds: usize,
    /// The most tokens one answer may take.
    pub output_tokens: u32,
    /// The most characters the conversation keeps before old answers give way.
    pub context_chars: usize,
    /// How long one round of the model may take.
    pub round_time: Duration,
    /// What names this turn's rounds to the model's meter.
    pub key: String,
}

impl Asked {
    /// `request` under `system`, with the bounds most turns keep.
    #[must_use]
    pub fn new(system: impl Into<String>, request: impl Into<String>) -> Self {
        Self {
            system: system.into(),
            request: request.into(),
            own_tools: Vec::new(),
            sees: false,
            budget: None,
            most_rounds: 40,
            output_tokens: 8_192,
            context_chars: 400_000,
            round_time: Duration::from_secs(90),
            key: String::new(),
        }
    }
}

/// How a turn ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The model answered the person in these words.
    Answered(String),
    /// A bound stopped it; the words say which and what to do.
    Stopped(String),
    /// It could not go on, in words its person reads.
    Failed(String),
    /// Its person stopped it.
    Cancelled,
}

/// The tools `server` offers, as a model is told them.
///
/// # Errors
/// The server did not list them in time, or answered something else.
pub async fn tools<H: ActionProvider + Send + Sync + 'static>(
    server: &McpServer<H>,
) -> Result<Vec<ToolDefinition>, String> {
    let listed = tokio::time::timeout(
        TOOL_TIME,
        server.handle(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})),
    )
    .await
    .map_err(|_| "the agent's tools did not say what they are in time".to_owned())?
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

/// Run tool `name` with `arguments` on `server`: what it answered, as the model reads it.
pub async fn call<H: ActionProvider + Send + Sync + 'static>(
    server: &McpServer<H>,
    name: &str,
    arguments: Value,
) -> Answer {
    let answer = tokio::time::timeout(
        TOOL_TIME,
        server.handle(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        })),
    )
    .await;
    let Ok(answer) = answer else {
        return Answer::failed(format!("{name} did not answer in time"));
    };
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
    let result = answer.get("result").cloned().unwrap_or(Value::Null);
    answer_of(&result)
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
        text: cut(text),
        pictures,
        is_error,
    }
}

/// `text`, at most [`MOST_RESULT_BYTES`] of it.
fn cut(mut text: String) -> String {
    if text.len() > MOST_RESULT_BYTES {
        let mut end = MOST_RESULT_BYTES;
        while !text.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        text.truncate(end);
        text.push_str("\n…(cut: ask for less)");
    }
    text
}

/// The words of a model's answer.
#[must_use]
pub fn words_of(response: &ModelResponse) -> String {
    response
        .message
        .content
        .iter()
        .filter_map(|content| match content {
            Content::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

/// Work on `asked` with `model` through `server`'s tools until the model answers, a bound stops
/// it or its person does: how it ended, and what its model calls cost.
pub async fn work<H, M, D>(
    server: &McpServer<H>,
    model: Arc<Mutex<M>>,
    asked: &Asked,
    doing: &D,
) -> (Outcome, u64)
where
    H: ActionProvider + Send + Sync + 'static,
    M: Model + 'static,
    D: Doing<H>,
{
    let mut tools = match tools(server).await {
        Ok(tools) => tools,
        Err(why) => return (Outcome::Failed(why), 0),
    };
    tools.extend(asked.own_tools.iter().cloned());
    let mut messages = vec![
        Message::text(Role::System, asked.system.clone()),
        Message::text(Role::User, asked.request.clone()),
    ];
    let mut cost = 0u64;
    let mut asked_for_words = false;
    let mut unusable = 0usize;
    // The server the turn's tools go through once what its person wrote since moved it.
    let mut moved: Option<McpServer<H>> = None;
    for round in 0..asked.most_rounds {
        if doing.stopped() {
            return (Outcome::Cancelled, cost);
        }
        match doing.went_on().await {
            Ok(Some(came)) => {
                told_since(&mut messages, &came.told);
                if let Some(server) = came.server {
                    moved = Some(server);
                }
            }
            Ok(None) => {}
            Err(why) => return (Outcome::Failed(why), cost),
        }
        if asked.budget.is_some_and(|budget| cost >= budget) {
            return (
                Outcome::Stopped(
                    "I stopped: today's spending limit for me is reached. Raise it in the \
                     workspace's settings to let me go on."
                        .to_owned(),
                ),
                cost,
            );
        }
        compact::compact(&mut messages, asked.context_chars);
        let request = ModelRequest {
            messages: messages.clone(),
            tools: tools.clone(),
            tool_choice: ToolChoice::Auto,
            max_output_tokens: asked.output_tokens,
        };
        let response = match ask(&model, request, format!("{}:{round}", asked.key), asked).await {
            Ok(response) => response,
            Err(_) if doing.stopped() => return (Outcome::Cancelled, cost),
            Err(why) => return (Outcome::Failed(why), cost),
        };
        if response.cost_micros > 0 {
            cost = cost.saturating_add(response.cost_micros);
            doing.spent(cost);
        }
        let calls = match read(&response, unusable) {
            Read::Calls(calls) => calls,
            Read::Again(why) => {
                unusable = unusable.saturating_add(1);
                messages.push(Message::text(Role::User, why));
                continue;
            }
            Read::Ended(outcome) => return (outcome, cost),
        };
        unusable = 0;
        messages.push(response.message.clone());
        if calls.is_empty() {
            if doing.stopped() {
                return (Outcome::Cancelled, cost);
            }
            if doing.waiting() {
                // Its person wrote while it answered: it goes on with that, not past it.
                continue;
            }
            let words = words_of(&response);
            if words.is_empty() && !asked_for_words {
                asked_for_words = true;
                messages.push(Message::text(Role::User, SAY_WHAT_YOU_DID.to_owned()));
                continue;
            }
            return (Outcome::Answered(words), cost);
        }
        let server = moved.as_ref().unwrap_or(server);
        let Some((results, pictures)) = run_calls(server, calls, asked, doing).await else {
            return (Outcome::Cancelled, cost);
        };
        messages.push(results);
        if !pictures.is_empty() {
            messages.push(shown(pictures));
        }
    }
    said_where_it_got(&model, asked, doing, messages, tools, cost).await
}

/// Out of rounds, it may well have finished: it is asked once more, with no tools, to say what
/// it did, rather than its person being told only that it stopped.
async fn said_where_it_got<H, M: Model + 'static>(
    model: &Arc<Mutex<M>>,
    asked: &Asked,
    doing: &impl Doing<H>,
    mut messages: Vec<Message>,
    tools: Vec<ToolDefinition>,
    mut cost: u64,
) -> (Outcome, u64) {
    if doing.stopped() {
        return (Outcome::Cancelled, cost);
    }
    messages.push(Message::text(Role::User, OUT_OF_STEPS.to_owned()));
    compact::compact(&mut messages, asked.context_chars);
    let request = ModelRequest {
        messages,
        tools,
        tool_choice: ToolChoice::None,
        max_output_tokens: asked.output_tokens,
    };
    let said = match ask(model, request, format!("{}:last", asked.key), asked).await {
        Ok(response) => {
            cost = cost.saturating_add(response.cost_micros);
            doing.spent(cost);
            words_of(&response)
        }
        Err(_) if doing.stopped() => return (Outcome::Cancelled, cost),
        Err(_) => String::new(),
    };
    let said = if said.is_empty() {
        STOPPED_UNFINISHED.to_owned()
    } else {
        said
    };
    (Outcome::Stopped(said), cost)
}

/// Ask `model` one round, named `key`, on a thread it may block: its answer, or why none came.
async fn ask<M: Model + 'static>(
    model: &Arc<Mutex<M>>,
    request: ModelRequest,
    key: String,
    asked: &Asked,
) -> Result<ModelResponse, String> {
    let deadline = later(asked.round_time);
    let carrier = Arc::clone(model);
    match tokio::task::spawn_blocking(move || carrier.lock_recover().turn(&request, &key, deadline))
        .await
    {
        Ok(answered) => answered,
        Err(error) => Err(error.to_string()),
    }
}

/// Tell the model what its person wrote since its last step: beside their last words when the
/// conversation ends with them, else as words of their own.
fn told_since(messages: &mut Vec<Message>, came: &str) {
    let told = format!("{CAME_SINCE}\n\n{came}");
    match messages.last_mut() {
        Some(last) if matches!(last.role, Role::User) => {
            last.content.push(Content::Text { text: told });
        }
        _ => messages.push(Message::text(Role::User, told)),
    }
}

/// What a round's answer comes to.
enum Read {
    /// The calls it makes, none when it answered in words.
    Calls(Vec<ToolCall>),
    /// It could not be used: the model is told this and asked again.
    Again(String),
    /// The turn ends with it.
    Ended(Outcome),
}

/// Read `response`, the `unusable`th answer in a row that could not be used before it.
fn read(response: &ModelResponse, unusable: usize) -> Read {
    match response.tool_calls() {
        Ok(calls) => Read::Calls(calls.into_iter().cloned().collect()),
        // An answer cut off at the output limit that says something is the answer; one that
        // only called tools, or was unreadable, is asked again a few times.
        Err(_)
            if response.stop_reason == StopReason::OutputLimit
                && !words_of(response).is_empty()
                && !has_calls(response) =>
        {
            Read::Ended(Outcome::Answered(words_of(response)))
        }
        Err(why) if unusable < MOST_UNUSABLE => Read::Again(format!(
            "{UNUSABLE}: {}. Answer again, shorter: call the tools you need, or say your answer.",
            why.message
        )),
        Err(why) => Read::Ended(Outcome::Failed(format!(
            "My model's answer was not usable: {}",
            why.message
        ))),
    }
}

/// The message showing a model that sees the `pictures` the tools answered with.
fn shown(pictures: Vec<(String, String)>) -> Message {
    Message {
        role: Role::User,
        content: std::iter::once(Content::Text {
            text: "The pictures the tools answered with:".to_owned(),
        })
        .chain(
            pictures
                .into_iter()
                .map(|(media_type, base64)| Content::Image { media_type, base64 }),
        )
        .collect(),
    }
}

/// Whether `response` carries a tool call, read or not.
fn has_calls(response: &ModelResponse) -> bool {
    response
        .message
        .content
        .iter()
        .any(|content| matches!(content, Content::ToolCall(_)))
}

/// Run the calls one round made, in order: the message answering them, and the pictures they
/// showed a model that sees. `None` when the person stopped the turn.
async fn run_calls<H: ActionProvider + Send + Sync + 'static>(
    server: &McpServer<H>,
    calls: Vec<ToolCall>,
    asked: &Asked,
    doing: &impl Doing<H>,
) -> Option<(Message, Vec<(String, String)>)> {
    let mut results = Vec::new();
    let mut shown = Vec::new();
    for call in calls {
        if doing.stopped() {
            return None;
        }
        doing.calling(&call).await;
        let answer = match doing.own(&call).await {
            Some(answer) => answer,
            None => self::call(server, &call.name, call.input.clone()).await,
        };
        doing.called(&call, &answer);
        // What it asked is logged with a refusal, so the call can be made again by hand.
        if answer.is_error {
            let asked: String = call.input.to_string().chars().take(LOGGED_CHARS).collect();
            let said: String = answer.text.chars().take(LOGGED_CHARS).collect();
            tracing::info!(tool = %call.name, %asked, %said, "an agent's tool was refused or failed");
        } else {
            tracing::debug!(tool = %call.name, "an agent used a tool");
        }
        if asked.sees {
            let room = MOST_PICTURES.saturating_sub(shown.len());
            shown.extend(answer.pictures.iter().take(room).cloned());
        }
        let mut content = answer.text.clone();
        if !answer.pictures.is_empty() && !asked.sees {
            content.push_str("\n(a picture this model cannot see)");
        }
        results.push(Content::ToolResult(ToolResult {
            call_id: call.id.clone(),
            content,
            is_error: answer.is_error,
        }));
    }
    Some((
        Message {
            role: Role::Tool,
            content: results,
        },
        shown,
    ))
}

/// `wait` from now, or now when that is past what a clock reads.
fn later(wait: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(wait).unwrap_or(now)
}

#[cfg(test)]
mod tests;
