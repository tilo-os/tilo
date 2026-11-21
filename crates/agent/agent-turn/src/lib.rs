//! One agent turn over the workspace's MCP tools.
//!
//! Every agent in Tilo works through one tool set: what the workspace's MCP server lists
//! (`tilo_ls`, `tilo_read`, `tilo_do`, `tilo_find`, `tilo_view`, `tilo_watch` and the rest). A turn
//! lists those tools as the model's, sends every call the model makes to them, and hands the model
//! what each answered, until the model answers in words, its person stops it, its spending limit
//! is reached or, where it has a limit of rounds, its rounds are spent ([`work`]).
//!
//! What the turn works with is the caller's: where its tools are answered ([`Tools`]: an MCP
//! server in the same process on a server, or the device's session asked over the agent's own
//! link, [`SessionTools`]), the model that answers ([`Model`]), and what the agent's person is told as it
//! works ([`Doing`]). Tools of the caller's own beside the workspace's (a web search, a view the
//! answer is) are offered with [`Asked::own_tools`] and answered by [`Doing::own`]. What its
//! person writes while it works is taken in between its rounds ([`Doing::went_on`]). What the
//! model writes while it answers is handed on as it comes ([`Doing::hear`]), and each tool step
//! is told where it stands among the calls of its answer ([`Step`]). A log line of a call names
//! its tool and its arguments' names and sizes, never a value or an answer ([`logged`]).

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agent_provider_config::protocol::{
    Content, Message, ModelRequest, ModelResponse, Role, StopReason, StreamEvent, ToolCall,
    ToolChoice, ToolDefinition, ToolResult,
};
use serde_json::Value;

mod carried;
mod compact;
mod hearing;
pub mod logged;
mod session_tools;
pub use carried::{Carried, carried};
pub use hearing::Hearing;
pub use session_tools::SessionTools;
pub use tilo_session_actions::SessionActions;

/// How long the tools may take to say what they are or to answer one call.
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

    /// [`Self::turn`], handing what the model writes to `heard` while it writes: its words and
    /// thought as they arrive, and a break where a call is asked again. Presentation only: the
    /// answer returned is what counts. A model with nothing to hand on answers whole.
    ///
    /// # Errors
    /// The model did not answer, in words its person reads.
    fn turn_streaming(
        &mut self,
        request: &ModelRequest,
        key: &str,
        deadline: Instant,
        heard: &mut (dyn FnMut(StreamEvent) + Send),
    ) -> Result<ModelResponse, String> {
        let _ = heard;
        self.turn(request, key, deadline)
    }
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

    fn turn_streaming(
        &mut self,
        request: &ModelRequest,
        key: &str,
        deadline: Instant,
        heard: &mut (dyn FnMut(StreamEvent) + Send),
    ) -> Result<ModelResponse, String> {
        (**self).turn_streaming(request, key, deadline, heard)
    }
}

/// Where a tool step stands among the calls of the model's answer that asked for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Step {
    /// Which call it is, counted from nought.
    pub nth: usize,
    /// How many calls the answer holds.
    pub of: usize,
}

impl Step {
    /// Whether it is the answer's last call: once it is done, so are the answer's calls.
    #[must_use]
    pub const fn last(self) -> bool {
        self.nth.saturating_add(1) >= self.of
    }
}

/// Where a turn's tools are answered: what they are, and what one call of them answered.
pub trait Tools: Send + Sync {
    /// The tools, as a model is told them.
    ///
    /// # Errors
    /// They did not say what they are in time, or answered something else, in words its person
    /// reads.
    fn list(&self) -> impl Future<Output = Result<Vec<ToolDefinition>, String>> + Send;
    /// Call tool `name` with `arguments`: what it answered, as the model reads it.
    fn call(&self, name: &str, arguments: Value) -> impl Future<Output = Answer> + Send;
}

/// What a turn tells its person as it works, and how they stop it. `T` is where its tools are
/// answered.
pub trait Doing<T>: Send + Sync {
    /// Whether the person stopped the turn: no tool is begun after a stop.
    fn stopped(&self) -> bool;
    /// The turn is about to run `call`, at `step` of its answer's calls.
    fn calling(&self, call: &ToolCall, step: Step) -> impl Future<Output = ()> + Send {
        let _ = (call, step);
        async {}
    }
    /// Carry out `call`, at `step` of its answer's calls, by `work`, which answers it (the
    /// caller's own tool or the workspace's): what it answered. Between [`Self::calling`] and
    /// [`Self::called`], so whoever times or notes a step can wrap the call itself.
    fn carry_out<W>(
        &self,
        call: &ToolCall,
        step: Step,
        work: W,
    ) -> impl Future<Output = Answer> + Send
    where
        W: Future<Output = Answer> + Send,
    {
        let _ = (call, step);
        work
    }
    /// `call`, at `step` of its answer's calls, answered `answer`. After the last step the
    /// answer's calls are done ([`Step::last`]); a stop leaves the rest untold.
    fn called(&self, call: &ToolCall, step: Step, answer: &Answer) {
        let _ = (call, step, answer);
    }
    /// What the model writes while one ask goes on, as it comes; it ends with the ask. Nothing
    /// is shown by default.
    fn hear(&self, hearing: Hearing) -> impl Future<Output = ()> + Send {
        drop(hearing);
        async {}
    }
    /// The ask's answer, after everything it wrote was heard ([`Self::hear`]).
    fn heard(&self, response: &ModelResponse) -> impl Future<Output = ()> + Send {
        let _ = response;
        async {}
    }
    /// What the turn ends with when its model gave no answer for `why`, the conversation being
    /// `messages` by then: `why` itself by default.
    fn left_off(&self, why: String, messages: &[Message]) -> String {
        let _ = messages;
        why
    }
    /// The turn's model calls cost `total` millionths of the currency by now.
    fn spent(&self, total: u64) {
        let _ = total;
    }
    /// Answer `call` when it is one of the caller's own tools ([`Asked::own_tools`]); `None` sends
    /// it to the workspace's tools. Every call is asked here first.
    fn own(&self, call: &ToolCall) -> impl Future<Output = Option<Answer>> + Send {
        let _ = call;
        async { None }
    }
    /// What its person wrote since the turn's last step, taken into it; `None` when nothing came.
    ///
    /// # Errors
    /// What came cannot be taken in, in words its person reads: the turn ends with them.
    fn went_on(&self) -> impl Future<Output = Result<Option<WentOn<T>>, String>> + Send {
        async { Ok(None) }
    }
    /// Whether its person wrote while the model answered: the turn goes on with that rather than
    /// answering past it.
    fn waiting(&self) -> bool {
        false
    }
}

/// What its person wrote while a turn worked, as the turn takes it in.
pub struct WentOn<T> {
    /// What the model is told of it, after [`CAME_SINCE`].
    pub told: String,
    /// Where the turn's tools go on being answered, when what came is someone else's to act for.
    pub tools: Option<T>,
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
    /// Earlier conversation in model roles, ordered before the current request.
    pub history: Vec<Message>,
    /// The caller's own tools beside the workspace's, answered by [`Doing::own`].
    pub own_tools: Vec<ToolDefinition>,
    /// Whether the model is shown the pictures a tool answers with.
    pub sees: bool,
    /// What the turn may spend, in millionths of the currency; `None` for no limit.
    pub budget: Option<u64>,
    /// The most rounds of tool calls before the turn stops; `None` asks the model for as long
    /// as it calls tools, and only the budget, its person or its answer ends the turn.
    pub most_rounds: Option<usize>,
    /// The most bytes of one tool's words the model is handed; past it the answer says to ask
    /// for less.
    pub result_bytes: usize,
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
            history: Vec::new(),
            own_tools: Vec::new(),
            sees: false,
            budget: None,
            most_rounds: Some(40),
            result_bytes: MOST_RESULT_BYTES,
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

/// `text`, at most `most` bytes of it.
#[must_use]
pub fn cut(mut text: String, most: usize) -> String {
    if text.len() > most {
        let mut end = most;
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

/// Work on `asked` with `model` through `answering`'s tools until the model answers, a bound
/// stops it or its person does: how it ended, and what its model calls cost.
pub async fn work<T, M, D>(
    answering: &T,
    model: Arc<Mutex<M>>,
    asked: &Asked,
    doing: &D,
) -> (Outcome, u64)
where
    T: Tools,
    M: Model + 'static,
    D: Doing<T>,
{
    let tools = match listed(answering, asked).await {
        Ok(tools) => tools,
        Err(why) => return (Outcome::Failed(why), 0),
    };
    let mut messages = vec![Message::text(Role::System, asked.system.clone())];
    messages.extend(asked.history.iter().cloned());
    messages.push(Message::text(Role::User, asked.request.clone()));
    let mut cost = 0u64;
    let mut asked_for_words = false;
    let mut unusable = 0usize;
    // Where the turn's tools are answered once what its person wrote since moved it.
    let mut moved: Option<T> = None;
    for round in 0.. {
        if asked.most_rounds.is_some_and(|most| round >= most) {
            break;
        }
        if doing.stopped() {
            return (Outcome::Cancelled, cost);
        }
        match doing.went_on().await {
            Ok(Some(came)) => {
                told_since(&mut messages, &came.told);
                if let Some(tools) = came.tools {
                    moved = Some(tools);
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
        let key = format!("{}:{round}", asked.key);
        let deadline = later(asked.round_time);
        let response = match hearing::ask(&model, request, key, deadline, doing).await {
            Ok(response) => response,
            Err(_) if doing.stopped() => return (Outcome::Cancelled, cost),
            Err(why) => return (Outcome::Failed(doing.left_off(why, &messages)), cost),
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
        let answering = moved.as_ref().unwrap_or(answering);
        let Some((results, pictures)) = call_each(answering, calls, asked, doing).await else {
            return (Outcome::Cancelled, cost);
        };
        messages.push(results);
        if !pictures.is_empty() {
            messages.push(shown(pictures));
        }
    }
    said_where_it_got(&model, asked, doing, messages, tools, cost).await
}

/// The tools the model is offered: `answering`'s and the caller's own, or why there are none.
async fn listed<T: Tools>(answering: &T, asked: &Asked) -> Result<Vec<ToolDefinition>, String> {
    // Boxed: where the tools are answered is the caller's, and its futures stay out of the turn's.
    let mut tools = tokio::time::timeout(TOOL_TIME, Box::pin(answering.list()))
        .await
        .map_err(|_| "the agent's tools did not say what they are in time".to_owned())??;
    tools.extend(asked.own_tools.iter().cloned());
    Ok(tools)
}

/// Out of rounds, it may well have finished: it is asked once more, with no tools, to say what
/// it did, rather than its person being told only that it stopped.
async fn said_where_it_got<T, M: Model + 'static>(
    model: &Arc<Mutex<M>>,
    asked: &Asked,
    doing: &impl Doing<T>,
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
    let key = format!("{}:last", asked.key);
    let said = match hearing::ask(model, request, key, later(asked.round_time), doing).await {
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
async fn call_each<T: Tools>(
    answering: &T,
    calls: Vec<ToolCall>,
    asked: &Asked,
    doing: &impl Doing<T>,
) -> Option<(Message, Vec<(String, String)>)> {
    let mut results = Vec::new();
    let mut shown = Vec::new();
    let of = calls.len();
    for (nth, call) in calls.into_iter().enumerate() {
        if doing.stopped() {
            return None;
        }
        let step = Step { nth, of };
        doing.calling(&call, step).await;
        let work = async {
            let mut answer = match doing.own(&call).await {
                Some(answer) => answer,
                None => tokio::time::timeout(
                    TOOL_TIME,
                    Box::pin(answering.call(&call.name, call.input.clone())),
                )
                .await
                .unwrap_or_else(|_| {
                    Answer::failed(format!("{} did not answer in time", call.name))
                }),
            };
            answer.text = cut(std::mem::take(&mut answer.text), asked.result_bytes);
            answer
        };
        let answer = doing.carry_out(&call, step, work).await;
        doing.called(&call, step, &answer);
        logged(&call, &answer);
        if asked.sees {
            let headroom = MOST_PICTURES.saturating_sub(shown.len());
            shown.extend(answer.pictures.iter().take(headroom).cloned());
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

/// Say in the log what `call` came to: its tool and each argument by its name and the bytes of
/// its value, which is where making a refused call again by hand begins; of a refusal its
/// sentence without anything the call wrote, and of an answer its size. No value of an argument
/// and nothing of an answer stands in a line, at any level ([`logged`]).
fn logged(call: &ToolCall, answer: &Answer) {
    let tool = logged::offered_name(&call.name);
    let arguments = logged::arguments(&call.input);
    if answer.is_error {
        let said = logged::refusal(&answer.text, &call.input);
        tracing::info!(%tool, %arguments, %said, "an agent's tool was refused or failed");
    } else {
        tracing::debug!(%tool, %arguments, answered_bytes = answer.text.len(), "an agent used a tool");
    }
}

/// `wait` from now, or now when that is past what a clock reads.
fn later(wait: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(wait).unwrap_or(now)
}

#[cfg(test)]
mod tests;
