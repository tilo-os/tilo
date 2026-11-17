//! The discover → invoke → observe loop of a task.
//!
//! The agent is a member like any person at the palette: it reads the
//! live, permission-filtered catalogue the broker publishes to it, invokes
//! one published operation at a time through the broker's invocation path,
//! and reads back what each step did — its result, how its own view changed,
//! and which operations became available because of it — before deciding the
//! next. Nothing here knows any application: there are no recipes, no
//! per-application tools and no hand-built edits. A reply to the person is
//! the terminal `plan` call's text, or — as a chat-style model ends a turn —
//! the words of a turn that called nothing; text *in* a document is whatever
//! the document's own published operations write.
//!
//! Latency matters as much as generality, so a turn is not a round trip:
//!
//! * the first turn already carries a compact catalogue, so a simple request
//!   needs no discovery turn at all;
//! * the model may call several tools in one turn, and the terminal `plan`
//!   in the same turn as its last `invoke` calls;
//! * the turn streams on its own thread ([`crate::model::TurnTransport`]);
//!   previews are inert until the complete response validates, so failed,
//!   truncated or malformed turns cannot dispatch tools;
//! * an `invoke` the model marks `standalone` (independent of the steps
//!   before it) is started without waiting for them to settle in their applications when
//!   it acts on another record than every step still running
//!   ([`Session::start_independent`]); everything else waits for them first,
//!   so a step that reads what an earlier one wrote stays after it;
//! * the turn's running steps are followed by asks their host holds until a
//!   step moves on ([`Session::follow_native`]), not on a timer;
//! * the catalogue survey is rebuilt only when the broker republishes it.
//!
//! Every bound is enforced here, not asked for: one model turn's time, as
//! the runner sets it. A long turn keeps within its model's context by
//! compacting what it read long ago ([`compaction`]).

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError};
use ui::time::Duration;

use anyhow::Result;
use serde_json::Value;
use ui::collaboration::CollabEvent;

use super::{
    Asked, Following, Session, StepOutcome, TaskAction, TaskActionStop, TurnUpdate,
    arguments_for_step, task_action_from_offer,
};
use crate::model::{Conversation, FirstTurn, TurnEvent};
use crate::plan::{self, ConfirmationEvidence, Instruction};
use crate::surface;
use crate::tools::{self, InvokeReport, Revealed};
use tilo_actions::task_step::TaskStep;

/// Most steps a run tells the person about, per item of a request of several
/// items (and per run otherwise): the task keeps a bounded number of public
/// step notes; its per-item progress lines, its outcome and its questions are
/// told apart from them.
const MOST_STEPS_TOLD: usize = 64;

mod pictures;
use pictures::MOST_PICTURES;
mod program_steps;

mod answer;
mod compaction;
mod failure;
use answer::SAY_WHAT_YOU_DID;
pub(crate) use failure::Failure;
mod progress;

/// After this many turns that only looked things up, every discovery answer
/// also says how many turns are left and asks the model to act.
const DISCOVERY_NUDGE: usize = 3;
/// After an open, the most to wait for the opened application's operations
/// to be published (its window starts and binds a new record). The wait
/// ends on the catalogue that carries them, at once when they already are.
const OPEN_SETTLE: Duration = Duration::from_secs(8);
/// Most newly available operations an open lists.
const MOST_NEWLY_OPENED: usize = 40;
/// The most one model turn may take: the model gateway's own ceiling for a
/// provider call, and a managed agent's model budget. Hosted reasoning
/// models answer most turns in seconds but take up to a minute on some
/// (Gemini on Vertex AI did on several turns of one session); cutting a turn
/// shorter fails it after the provider already did, and billed, the work.
const MAX_LOOP_TURN_TIME: Duration = Duration::from_secs(60);
/// After a view operation, the most the loop keeps listening for the
/// catalogue the new view unlocks — only when no further call is ready to
/// run. A complete tool-only turn can need this republication for its next
/// observation too ([`Session::settle_view`]).
const VIEW_SETTLE: Duration = Duration::from_millis(400);

#[cfg(test)]
thread_local! {
    /// Settles after a view or an open that ended at their deadline rather
    /// than on the event they listen for.
    static SETTLES_RAN_OUT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
/// Most newly available operations one observation lists.
const MOST_NEWLY_AVAILABLE: usize = 16;
/// Most independent invokes of one turn running at once; one more waits for
/// them to settle and runs on its own.
pub(super) const MAX_IN_FLIGHT: usize = 4;
/// Contract tag prefix by which an application marks an operation
/// deprecated: `tilo.deprecated:<replacement>` (Writer's `DEPRECATED_TAG`).
pub(super) const DEPRECATED_TAG: &str = "tilo.deprecated:";

/// How many turns one task answers with "what was wrong" after refusing the
/// model's own calls for their shape or arguments (a call missing an argument
/// its declaration requires, a value outside the choices it advertised). A
/// model that corrects itself takes one of them; one that keeps calling the
/// same way ends the task rather than looping.
const MOST_MALFORMED_TURNS: usize = 2;
/// How many times one task asks the model again after a turn whose answer
/// could not be used — one that arrived unreadable, or that the provider failed
/// while writing — before the run ends with the reason. None of such a turn's
/// calls ran, so asking again repeats nothing.
const MOST_UNUSABLE_TURNS: usize = 2;
/// What the model reads before the reason its last answer was not used.
const UNUSABLE_ANSWER: &str = "Your last answer did not arrive in a form that could be used";
/// What the model reads after that reason: nothing of that answer ran.
const ANSWER_AGAIN: &str = "None of its calls ran. Answer again from here, keeping \
     the answer short: call the tools you need, or `plan` with your reply.";
/// How many finishes in a row a run answers with "nothing was done and nothing
/// was said" before it ends with the clean failure row. A finish that carries
/// neither a completed step nor words for the person is not an answer, whatever
/// it says of itself; one that keeps making it ends the task as it began.
const MOST_SILENT_FINISHES: usize = 2;
/// Turns in a row that do nothing in any app after which a run is asked to finish
/// ([`LoopState::stalled`]); it ends one turn later.
const STALLED_TURNS: usize = 4;
/// What a run that is not getting further is told.
const FINISH_NOW: &str = "You have taken several responses without getting further. Finish \
     now: call `plan` alone, saying in `reply` what you did, what you could not do and why. Call \
     no other tool.";
/// How many times in a row a turn cut off at the model's output limit is
/// continued from where it stopped before the run fails. Reasoning models
/// spend part of their answer's budget thinking; an answer that keeps being
/// cut off is the model's own, not the loop's to retry forever.
const MOST_CONTINUATIONS: usize = 4;

/// The error results answering `body`, the model's latest turn, when the loop
/// refused its calls for their shape or arguments rather than for failing to
/// be a turn at all: what is wrong with each call and what it needs, by call
/// id, ready for [`Conversation::refuse`]. `None` when nothing can be fed
/// back — a body that is not a readable model turn, a consumed call id, a
/// refusal or truncation, or a call with no id or name to answer — and the
/// run ends with the reason instead.
fn malformed_turn(conversation: &Conversation, body: &str) -> Option<Vec<(String, String)>> {
    let calls = conversation.read_calls(body).ok()?;
    let refused = tools::refused_results(&calls)?;
    (!refused.is_empty()).then_some(refused)
}

/// One tool call whose arguments are complete.
#[derive(Clone, Debug)]
pub(super) struct StreamedCall {
    /// Its position among this turn's calls.
    index: usize,
    id: String,
    name: String,
    input: Value,
}

/// The model turn in flight.
pub(super) struct ActiveTurn {
    rx: Receiver<TurnEvent>,
    /// Calls whose `tool_call` frame arrived and that are not handled yet.
    ready: VecDeque<StreamedCall>,
    /// The turn's end, once it has one: the model's complete body, or the
    /// failure it ended in, split into the sentence a person reads and the
    /// record kept for debugging.
    finished: Option<Result<String, Failure>>,
    presentation: agent_provider_config::protocol::StreamState,
    /// Completed calls seen (`tool_call` frames): calls below this index
    /// are complete.
    completed: usize,
    /// Calls the stream has begun, by index.
    begun: HashSet<usize>,
    /// The body as the model sent it and why the loop refused its calls for
    /// their shape or arguments, kept whole so the loop answers them with
    /// what was wrong rather than ending the run on the model's own slip.
    refused: Option<(String, Failure)>,
    /// Stops the turn when the loop lets go of it.
    #[allow(
        dead_code,
        reason = "held only to be dropped: letting go of the turn stops its model call"
    )]
    stop: Option<crate::model::TurnStop>,
}

impl ActiveTurn {
    fn new(rx: Receiver<TurnEvent>) -> Self {
        Self {
            rx,
            ready: VecDeque::new(),
            finished: None,
            presentation: agent_provider_config::protocol::StreamState::default(),
            completed: 0,
            begun: HashSet::new(),
            refused: None,
            stop: None,
        }
    }

    /// Fold one event in.
    fn absorb(&mut self, event: TurnEvent) {
        if self.finished.is_some() {
            return;
        }
        match event {
            TurnEvent::Neutral(event) => {
                self.presentation.apply(&event);
                // A cut-off answer is not a failed turn here: the final body
                // still arrives, and the loop continues it from where it
                // stopped. Every other failure ends the turn now.
                if let Some(error) = &self.presentation.error
                    && error.kind != agent_provider_config::protocol::ErrorKind::Truncated
                {
                    self.finished = Some(Err(self.retained_failure(&Failure::model(error))));
                }
            }
            TurnEvent::Frame { event, output } => {
                if event == tilo_model_client::EVENT_TOOL_CALL_DELTA {
                    if let Ok(frame) = serde_json::from_str::<Value>(&output)
                        && let Some(index) = frame
                            .get("index")
                            .and_then(Value::as_u64)
                            .and_then(|index| usize::try_from(index).ok())
                        && frame.get("name").and_then(Value::as_str).is_some()
                    {
                        self.begun.insert(index);
                    }
                } else if event == tilo_model_client::EVENT_TOOL_CALL
                    && let Some(call) = streamed_call(&output, self.completed)
                {
                    self.completed += 1;
                    self.ready.push_back(call);
                }
            }
            TurnEvent::Finished(result) => {
                self.finished = Some(
                    result.map_err(|error| self.retained_failure(&Failure::unreachable(error))),
                );
            }
        }
    }

    /// The failure, with what the turn streamed before it ended kept under
    /// labels in the run's record: the partial answer and the provider's own
    /// exposed reasoning are for debugging, never the person's reply.
    fn retained_failure(&self, failure: &Failure) -> Failure {
        failure.retained(&self.presentation.text, &self.presentation.reasoning)
    }
}

/// The session API's programs (`tilo.task-programs`, `task#run-program`) as the step a turn
/// takes them by, when the catalogue the agent's connection holds does not list them.
///
/// The broker lists an agent session's own steps only to a caller working in a session
/// (`collab_app_broker`, "A step of an agent session's own ... is listed to an agent while it
/// works in a session"); the agent's connection is not one, while every turn it answers is: the
/// steps of a turn are requests on its connection served as the turn's. So the listing the
/// agent keeps never carries the API its turns run their programs through, and without this
/// every `tilo` call was answered "this session runs no programs" and the model fell back to
/// looking operations up one by one. The broker still decides each call as a step of the
/// turn's session; this only names the step the protocol defines.
fn task_programs_offer(offers: &[surface::Offer]) -> Option<surface::Offer> {
    use tilo_actions::remote::task_programs as wire;
    if offers.iter().any(|offer| {
        offer.descriptor.address.resource == wire::RESOURCE
            && offer.descriptor.address.action == wire::RUN
    }) {
        return None;
    }
    Some(surface::Offer {
        app: wire::APP.to_owned(),
        broker_peer_id: None,
        broker_provider: Some(wire::PROVIDER.to_owned()),
        descriptor: tilo_actions::ActionDescriptor {
            did: None,
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            address: tilo_actions::ActionAddress::new(wire::RESOURCE, wire::RUN),
            resource_kind: wire::RESOURCE.into(),
            resource_label: "Task programs".into(),
            action_label: "Run a program".into(),
            keywords: Vec::new(),
            icon: None,
            description: None,
            locus: tilo_actions::ExecutionLocus::default(),
            kind: tilo_actions::OperationKind::View,
            contract: tilo_actions::ActionContract {
                result: tilo_actions::ResultSpec::Json {
                    schema: serde_json::json!({ "type": "object", "required": ["text", "failed"] }),
                },
                ..tilo_actions::ActionContract::view()
            },
            input: tilo_actions::InputSpec::None,
            params: vec![
                tilo_actions::ActionParam::required(
                    "program",
                    tilo_actions::ParamType::Text,
                    "The program",
                ),
                tilo_actions::ActionParam::optional(
                    "confirm",
                    tilo_actions::ParamType::Boolean,
                    "Send again a previewed program",
                ),
                tilo_actions::ActionParam::optional(
                    "this",
                    tilo_actions::ParamType::Text,
                    "What the person pointed at",
                ),
            ],
            view: None,
        },
    })
}

/// Whether `body` is a model turn the model itself cut off at its output
/// limit: not a failed call, an answer that stopped early and is continued.
fn is_output_limit(body: &str) -> bool {
    serde_json::from_str::<agent_provider_config::protocol::ModelResponse>(body).is_ok_and(
        |response| response.stop_reason == agent_provider_config::protocol::StopReason::OutputLimit,
    )
}

/// The day the person asks on, as the model reads it: a program reads no clock, so a request
/// about "this week" or "overdue" is answered from this, never found out by reading change times.
fn today_line(today: ui::format::NaiveDate) -> String {
    let week = today.week(ui::format::Weekday::Mon);
    format!(
        "Today is {} where the person is ({today}); this week runs from Monday {} to Sunday {}.",
        ui::format::day_heading(today, true),
        week.first_day(),
        week.last_day()
    )
}

/// The input tokens the provider counted for the request a turn's `body`
/// answers, when it said.
fn input_tokens(body: &str) -> Option<u64> {
    serde_json::from_str::<agent_provider_config::protocol::ModelResponse>(body)
        .ok()
        .map(|response| response.usage.input_tokens)
        .filter(|tokens| *tokens > 0)
}

/// The moment the person asks at, and how a moment they name is written: a program's `Time` is an
/// instant in UTC (`2026-11-17T17:00:00Z`; a time with no zone does not compile in a program,
/// E121), so a time of day the person names ("at 6pm") is theirs, in their zone, and is written
/// shifted by its offset. Without this a model leaves the time out, writes it as UTC, or spends
/// a turn on the compiler's refusal.
fn clock_line(local: ui::format::NaiveDateTime, utc: ui::format::NaiveDateTime) -> String {
    let offset = local - utc;
    let minutes = offset.num_minutes();
    let sign = if minutes < 0 { '-' } else { '+' };
    let zone = ui::format::machine!(
        "the offset a model shifts a time of day by, in ISO 8601's own form",
        format!(
            "UTC{sign}{:02}:{:02}",
            minutes.abs() / 60,
            minutes.abs() % 60
        )
    );
    let six = local.date().and_hms_opt(18, 0, 0).unwrap_or(local) - offset;
    let now = ui::format::machine!(
        "the clock a model reads to write an instant, in 24-hour ISO 8601 form",
        local.format("%H:%M").to_string()
    );
    format!(
        "It is {now} there now ({zone}). A `Time` in a program is an instant in UTC with a `Z`: 6 \
         pm there today is `{}`. Keep every time they name.",
        six.format("%Y-%m-%dT%H:%M:%SZ")
    )
}

/// The longest prefix of `text` within `max` bytes, cut on a character
/// boundary.
fn head(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.get(..end).unwrap_or_default()
}

/// A completed call from a gateway `tool_call` frame, or `None` when its
/// arguments are not a JSON object (the final body then carries it).
fn streamed_call(output: &str, index: usize) -> Option<StreamedCall> {
    let frame: Value = serde_json::from_str(output).ok()?;
    let id = frame.get("id")?.as_str()?.to_owned();
    let name = frame.get("name")?.as_str()?.to_owned();
    let arguments = frame.get("arguments").and_then(Value::as_str).unwrap_or("");
    let input = if arguments.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str::<Value>(arguments).ok()?
    };
    (!id.is_empty() && input.is_object()).then_some(StreamedCall {
        index,
        id,
        name,
        input,
    })
}

/// Durable state of one task's loop.
pub(super) struct LoopState {
    pub(super) revealed: Revealed,
    /// Invokes started so far.
    pub(super) step_index: usize,
    /// What the next content-consuming step is built from.
    pub(super) previous: Option<StepOutcome>,
    /// Steps that succeeded.
    pub(super) completed: usize,
    /// The request's parts, as the model listed them (or the loop implied).
    parts: Vec<tools::Part>,
    /// Successful operations in this task, by application.
    done_apps: HashMap<String, usize>,
    /// Parts the model explained it cannot do.
    unable: Vec<String>,
    /// Finishes refused for undone parts in this task.
    refusals: usize,
    /// Turns so far that only looked things up.
    discovery_turns: usize,
    /// The turn being run.
    turn: usize,
    /// The item of a many-item request the person was last told the run is
    /// at ([`progress`]).
    progress: Option<progress::Progress>,
    /// `operation error` of every failed invoke so far, to tell the model
    /// when it repeats one unchanged.
    failures: HashSet<String>,
    /// Independent invokes of this turn the host accepted and that have not
    /// settled yet, in call order (at most [`MAX_IN_FLIGHT`]).
    in_flight: Vec<InFlight>,
    /// Results of in-flight invokes that settled while another call was
    /// handled, by call id, for the turn to take in.
    settled: Vec<(String, Handled)>,
    /// Results too long for one observation, read on a page at a time.
    pages: crate::pages::Pages,
    /// The step the person was last told the run is at, and how many
    /// steps they were told: told once each time it changes, a bounded
    /// number of times per run.
    told_step: Option<String>,
    steps_told: usize,
    /// Content addresses of pictures this turn's invokes answered with, to
    /// show a model that sees once the turn's results are in.
    pictures: Vec<String>,
    /// The workspace's tilolang apps as this task's programs reach them.
    programs: program_steps::ProgramWorld,
    /// Turns answered with "what was wrong" after the loop refused the
    /// model's own calls for their shape or arguments, so it may correct:
    /// bounded, so a model that keeps calling the same way ends the task
    /// rather than looping.
    malformed_turns: usize,
    /// Turns asked again after an answer that could not be used (one that
    /// arrived unreadable, or that the provider failed while writing):
    /// bounded, so a provider that keeps failing ends the task.
    unusable_turns: usize,
    /// Continuations asked so far of answers the model cut off at its output
    /// limit, in a row: a turn cut off is continued from where it stopped,
    /// a bounded number of times, before the run fails with the clean
    /// sentence. A turn that is not cut off resets it.
    continuations: usize,
    /// The last words the model wrote for the person, on whatever call carried
    /// them, whether or not that call succeeded: they are the reply when the
    /// run ends without a fresh one (a closing turn lost, a finish whose call
    /// failed). Never a line the run writes itself.
    reply: Option<String>,
    /// Finishes so far answered with "nothing was done and nothing was said",
    /// bounded so a model that keeps finishing silently ends the run as failed
    /// instead of reporting a success it never earned.
    silent_finishes: usize,
    /// Whether the model was asked, once, to say what it did when it finished
    /// with work done and no words.
    asked_for_words: bool,
    /// The last turn's request as compaction estimated it, and the input
    /// tokens the provider counted for it, when it said.
    counted: Option<(usize, u64)>,
    /// Turns in a row after which nothing more was done in any app: a run that keeps looking
    /// without getting further is told to finish, then ends ([`Self::stalled`]).
    idle_turns: usize,
    /// How many operations had succeeded when the last turn ended.
    done_before: usize,
    /// Steps that got the run further without changing an app: a web search or page read, and
    /// each item a many-item request said it finished.
    progressed: usize,
}

/// An independent invoke running in its application while the turn goes on.
struct InFlight {
    call: StreamedCall,
    offer: surface::Offer,
    /// What was available when it started, to report what it unlocked.
    before: HashMap<String, bool>,
    /// Its trace index.
    index: usize,
    following: Following,
}

/// What [`Session::start_independent`] did with a call.
enum Started {
    /// The host accepted it; it settles while the turn goes on.
    Running,
    /// It was refused before anything ran (by the live surface or the
    /// broker): its report.
    Refused(Box<InvokeReport>),
    /// It runs in order instead, after everything in flight.
    InOrder,
    /// The task was cancelled before it started.
    Cancelled(String),
}

/// An invoke checked against the live catalogue, ready to run.
struct Prepared {
    offer: surface::Offer,
    action: TaskAction,
    arguments: serde_json::Map<String, Value>,
    /// What was available before it ran.
    before: HashMap<String, bool>,
}

/// What a successful invoke is reported against.
struct Succeeded<'a> {
    offer: &'a surface::Offer,
    before: &'a HashMap<String, bool>,
    before_revision: u64,
    /// The application an open started, and the windows it had before.
    opened: Option<(String, HashSet<(u64, String)>)>,
    index: usize,
}

/// Fold the results of invokes that settled meanwhile into the turn's
/// results; `Some` is a cancellation.
fn fold_settled(
    state: &mut LoopState,
    results: &mut HashMap<String, String>,
    finish: &mut Option<Finishing>,
) -> Option<String> {
    let mut cancelled = None;
    for (id, handled) in std::mem::take(&mut state.settled) {
        if let Some(message) = settle(handled, &id, results, finish) {
            cancelled.get_or_insert(message);
        }
    }
    cancelled
}

/// Most finishes refused for undone parts before the loop accepts one.
const MOST_REFUSALS: usize = 2;

/// Words that ask for a message to someone.
const NOTIFY_WORDS: &[&str] = &[
    "ping",
    "tell",
    "notify",
    "post",
    "message",
    "let me know",
    "send",
    "say ",
    "write to",
    "remind",
];

/// Words that open a question or a request for an explanation, which is
/// answered in the reply rather than by an operation.
const QUESTION_OPENERS: &[&str] = &[
    "what",
    "why",
    "how",
    "who",
    "whom",
    "whose",
    "when",
    "where",
    "which",
    "is",
    "are",
    "does",
    "do",
    "did",
    "can",
    "could",
    "would",
    "should",
    "explain",
    "translate",
    "define",
    "describe",
    "summarise",
    "summarize",
];

/// Whether `request` (lower-cased) asks a question rather than for
/// something to be done: it ends in a question mark or opens with a
/// question word, and asks for nothing to be sent or written anywhere.
fn asks_question(request: &str) -> bool {
    let request = request.trim();
    let opener = request
        .split(|c: char| !c.is_alphanumeric())
        .find(|word| !word.is_empty())
        .unwrap_or("");
    let questioning = request.ends_with('?')
        || QUESTION_OPENERS.contains(&opener)
        || request.starts_with("tell me ");
    let asks_for_more = [
        " and send",
        " and post",
        " and write",
        " and save",
        " and put",
        " into ",
        " in a note",
        " to chat",
        " in chat",
    ]
    .iter()
    .any(|more| request.contains(more));
    questioning && !asks_for_more
}

/// Whether `request` (lower-cased) asks for a message to someone: a
/// notify word, where "tell me" is the person asking for an answer, not
/// for a message ("ping me" still is one).
fn asks_for_message(request: &str) -> bool {
    let to_me = |rest: &str| {
        rest.strip_prefix(" me")
            .is_some_and(|after| !after.starts_with(char::is_alphanumeric))
    };
    NOTIFY_WORDS.iter().any(|word| {
        request
            .match_indices(word)
            .any(|(at, _)| *word != "tell" || !request.get(at + word.len()..).is_some_and(to_me))
    })
}

/// Whether a part naming `declared` is satisfied by a success in `app_id`.
fn app_matches(declared: &str, app_id: &str) -> bool {
    let declared = declared.trim().to_lowercase();
    let app = app_id.to_lowercase();
    if declared.is_empty() {
        return true;
    }
    let tail = app.rsplit('.').next().unwrap_or(&app).to_owned();
    app == declared
        || app.ends_with(&declared)
        || declared.contains(&tail)
        || tail.split('-').all(|word| declared.contains(word))
        // The product name people use for the word processor.
        || (declared.contains("writer") && tail == "word-processor")
}

impl LoopState {
    fn new() -> Self {
        Self {
            revealed: Revealed::new(),
            step_index: 0,
            previous: None,
            completed: 0,
            parts: Vec::new(),
            done_apps: HashMap::new(),
            unable: Vec::new(),
            refusals: 0,
            discovery_turns: 0,
            turn: 0,
            progress: None,
            failures: HashSet::new(),
            in_flight: Vec::new(),
            settled: Vec::new(),
            pages: crate::pages::Pages::new(),
            told_step: None,
            steps_told: 0,
            pictures: Vec::new(),
            programs: program_steps::ProgramWorld::default(),
            malformed_turns: 0,
            unusable_turns: 0,
            continuations: 0,
            counted: None,
            reply: None,
            silent_finishes: 0,
            asked_for_words: false,
            idle_turns: 0,
            done_before: 0,
            progressed: 0,
        }
    }

    /// After a turn that did not end the run: whether it got any further. A run whose turns
    /// keep doing nothing in any app (looking operations up, programs that do not compile,
    /// searching for a way the apps do not offer) is told after [`STALLED_TURNS`] of them to
    /// finish with what it did and what is left, and ends one turn later with its own words,
    /// or the readable failure without them: never dozens of calls that end in nothing. A run
    /// that keeps doing things, item after item, is not stalled however long it runs.
    fn stalled(&mut self, conversation: &mut Conversation) -> Option<LoopEnd> {
        let done = self.done_apps.values().sum::<usize>() + self.progressed;
        if done > self.done_before {
            self.done_before = done;
            self.idle_turns = 0;
            return None;
        }
        self.idle_turns += 1;
        if self.idle_turns == STALLED_TURNS {
            tracing::info!(
                turns = self.idle_turns,
                "the run is not getting further; asking it to finish"
            );
            conversation.push_user(FINISH_NOW);
            return None;
        }
        if self.idle_turns <= STALLED_TURNS {
            return None;
        }
        tracing::info!(
            turns = self.idle_turns,
            "the run did not finish when asked; ending it"
        );
        Some(match self.reply.clone() {
            Some(reply) => LoopEnd::Finished(
                tools::Finish {
                    reply: Some(reply),
                    done: false,
                    view: None,
                },
                self.completed,
            ),
            None => LoopEnd::Failed(Failure::said(crate::i18n::t("agent-failure-stalled"))),
        })
    }

    /// Parts with no successful operation in their application that were
    /// not explained as impossible.
    /// Each successful operation satisfies one part in its application, in
    /// order, so two messages to post need two posts.
    fn undone_parts(&self) -> Vec<&tools::Part> {
        let mut left = self.done_apps.clone();
        self.parts
            .iter()
            .filter(|part| {
                let done = left
                    .iter_mut()
                    .find(|(app, count)| **count > 0 && app_matches(&part.app, app));
                match done {
                    Some((_, count)) => {
                        *count -= 1;
                        false
                    }
                    None => true,
                }
            })
            .filter(|part| {
                let text = part.text.to_lowercase();
                !self.unable.iter().any(|reason| {
                    let reason = reason.to_lowercase();
                    reason.contains(&text)
                        || text
                            .split_whitespace()
                            .filter(|w| w.len() > 3)
                            .any(|w| reason.contains(w))
                        || (!part.app.is_empty() && reason.contains(&part.app.to_lowercase()))
                })
            })
            .collect()
    }

    /// Count a successful operation as done in its app: the app that published it, and the
    /// tilolang app whose record it acted on. A tilolang app's record is published under the
    /// session that serves it (`tl:<app>:<record>`): a step on it is a step in that app, which
    /// is how the model names the part it does.
    fn did_in(&mut self, offer: &surface::Offer) {
        *self.done_apps.entry(offer.app.clone()).or_default() += 1;
        if let Some(app) = tools::canvas::tilolang_app_of(&offer.address().resource)
            && app != offer.app
        {
            *self.done_apps.entry(app.to_owned()).or_default() += 1;
        }
    }

    /// Count what a `tilo` program changed as done in each app it changed: a program that
    /// made the task the request asked for did that part, exactly as an `invoke` of the app's
    /// own operation would have. Without this a part done by a program stays undone, the
    /// finish is refused, and the model is pushed to open the app's window to satisfy it.
    ///
    /// A program that changed nothing and answered — "summarise what changed in my notes" read
    /// and printed — did the part in each app it read: a question is done by its answer, and its
    /// finish was refused for want of a change nobody asked for.
    pub(super) fn program_did(&mut self, step: &ui::tilo_reference::ProgramStep) {
        if step.outcome != ui::tilo_reference::StepOutcome::Done {
            return;
        }
        let mut apps: Vec<String> = step
            .changes
            .iter()
            .map(|change| change.resource.app.clone())
            .filter(|app| !app.is_empty())
            .collect();
        if apps.is_empty() && step.answer.iter().any(|line| !line.trim().is_empty()) {
            apps = self.programs.apps_named(&step.program);
        }
        apps.sort();
        apps.dedup();
        for app in apps {
            *self.done_apps.entry(app).or_default() += 1;
        }
    }

    /// Record `parts`/`unable` a call carried; the first listing wins.
    fn absorb_parts(&mut self, input: &Value) {
        let parts = tools::read_parts(input);
        if !parts.is_empty()
            && self
                .parts
                .iter()
                .all(|part| part.text.starts_with("(implied)"))
        {
            // The model's own listing replaces an implied one, keeping the
            // implied app requirement unless the model covers that app.
            let implied: Vec<tools::Part> = self
                .parts
                .drain(..)
                .filter(|implied| {
                    !parts
                        .iter()
                        .any(|part| app_matches(&part.app, &implied.app))
                })
                .collect();
            self.parts = parts;
            self.parts.extend(implied);
        } else {
            // A later listing can only add parts, never drop one.
            for part in parts {
                if self.parts.len() < 12 && !self.parts.iter().any(|known| known.text == part.text)
                {
                    self.parts.push(part);
                }
            }
        }
        self.unable.extend(tools::read_unable(input));
    }
}

/// How the loop ended.
#[derive(Debug)]
pub(super) enum LoopEnd {
    /// The model finished with `plan`, after this many successful steps.
    Finished(tools::Finish, usize),
    /// The loop stopped without finishing.
    Failed(Failure),
    /// Cancelled from Task Center, or a step was cancelled.
    Cancelled(String),
}

/// What handling one call produced.
enum Handled {
    /// The tool result, by call id.
    Result(String),
    /// The terminal `plan` (its result is written at the end of the turn).
    Finish(tools::Finish),
    /// A successful `invoke` that carried the task's reply: its result, and
    /// the finish it asks for.
    ResultFinish(String, tools::Finish),
    /// An independent `invoke` is running; its result comes when it settles
    /// ([`LoopState::settled`]).
    Pending,
    /// Stop the whole loop: cancelled.
    Cancelled(String),
}

/// How the turn asked to finish.
struct Finishing {
    /// The call that carried it.
    id: String,
    said: tools::Finish,
    /// `plan` (its tool result is written at the end of the turn) rather
    /// than an `invoke` carrying the reply (its result is the invoke's).
    by_plan: bool,
}

/// Fold one handled call into the turn's results; `Some` is a cancellation.
fn settle(
    handled: Handled,
    id: &str,
    results: &mut HashMap<String, String>,
    finish: &mut Option<Finishing>,
) -> Option<String> {
    let mut finishing = |said: tools::Finish, by_plan: bool| {
        if finish.is_none() {
            *finish = Some(Finishing {
                id: id.to_owned(),
                said,
                by_plan,
            });
            true
        } else {
            false
        }
    };
    match handled {
        Handled::Result(result) => {
            results.insert(id.to_owned(), result);
        }
        Handled::ResultFinish(result, said) => {
            finishing(said, false);
            results.insert(id.to_owned(), result);
        }
        Handled::Finish(said) => {
            if !finishing(said, true) {
                results.insert(
                    id.to_owned(),
                    "Ignored: the task was already finished in this turn.".into(),
                );
            }
        }
        Handled::Pending => {}
        Handled::Cancelled(message) => return Some(message),
    }
    None
}

/// The report of an invoke that did not succeed, saying why.
fn refused_report(error: String) -> InvokeReport {
    InvokeReport {
        succeeded: false,
        error: Some(error),
        ..InvokeReport::default()
    }
}

/// What the run in progress may spend today, as its keeper counted it, and
/// what the run's own calls cost: the run stops at the limit rather than
/// after it.
#[derive(Clone, Debug)]
pub(super) struct Spending {
    /// The day, the limit and the spend the keeper counted when the run was
    /// admitted.
    allowed: runtime_collab::AgentSpend,
    /// What this run's calls cost by now, in millionths of the currency.
    run: u64,
}

/// What the gateway charged for the answer `body`, when its answer says.
fn charged(body: &str) -> Option<u64> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()?
        .get("cost_micros")?
        .as_u64()
}

impl Session {
    /// Why the run may make no further model call: the limit its keeper set
    /// for the day is used up.
    fn spend_exhausted(&self) -> Option<String> {
        let spending = self.spending.as_ref()?;
        let left = spending.allowed.left(&ui::clock::today().to_string())?;
        (spending.run >= left).then(|| {
            let used = ui::format::money(spending.allowed.limit_micros.unwrap_or_default());
            crate::i18n::t_args(
                "agent-fail-spend-limit",
                &[
                    ("agent", ui::i18n::ArgValue::from(self.cfg.name.as_str())),
                    ("spent", ui::i18n::ArgValue::from(used)),
                ],
            )
        })
    }

    /// Add what the gateway charged for this turn to what its run spent, and
    /// tell the host the run's total, as its keeper counts the day by.
    fn charge_turn(&mut self, run: &Asked, body: &str) -> Result<()> {
        let Some(spending) = self.spending.as_mut() else {
            return Ok(());
        };
        let Some(cost) = charged(body) else {
            return Ok(());
        };
        if cost == 0 {
            return Ok(());
        }
        spending.run = spending.run.saturating_add(cost);
        let total = spending.run;
        self.report_task(run, TurnUpdate::Spent { cost_micros: total })
    }

    /// Apply a change to the broker catalogue and invalidate the cached
    /// survey. A change that does not apply leaves the catalogue as it was:
    /// the connection asks the broker for the whole catalogue, whose answer
    /// comes as a reset.
    pub(super) fn apply_broker_catalogue(&mut self, change: &ui::collaboration::CatalogueChange) {
        if let Err(error) = self.broker_catalogue.apply(change) {
            tracing::warn!(
                %error,
                "the broker's catalogue change did not apply; keeping the catalogue held"
            );
        }
        self.broker_revision = self.broker_revision.wrapping_add(1);
    }

    /// Wait, at most `most`, until the catalogue the broker republishes holds what `published`
    /// asks of it: on its republications, never on a timer. The wait sleeps until the link's
    /// next republication; events other than catalogue republications keep their place for
    /// the session loop. Whether it came.
    pub(super) fn await_catalogue(
        &mut self,
        most: Duration,
        mut published: impl FnMut(&mut Self) -> bool,
    ) -> bool {
        let deadline = ui::time::now() + most;
        loop {
            self.absorb_catalogue_updates();
            if published(self) {
                return true;
            }
            match self.link.next_catalogue(deadline) {
                Some(change) => self.apply_broker_catalogue(&change),
                None => return false,
            }
        }
    }

    /// Apply catalogue republications already queued on the link, without
    /// waiting: while the model thinks nothing else reads the link.
    fn absorb_catalogue_updates(&mut self) {
        while let Some(change) = self.link.take_catalogue() {
            self.apply_broker_catalogue(&change);
        }
    }

    /// The task catalogue — every broker route an unattended task may
    /// invoke — rebuilt only when the broker republished it, and shared
    /// rather than copied between rebuilds.
    pub(super) fn task_catalogue(&mut self) -> std::sync::Arc<surface::Catalogue> {
        if let Some((revision, catalogue)) = &self.catalogue_cache
            && *revision == self.broker_revision
        {
            return std::sync::Arc::clone(catalogue);
        }
        let offers = surface::broker_offers(self.broker_catalogue.surfaces())
            .into_iter()
            .filter(|offer| {
                (offer.broker_peer_id.is_some() || offer.broker_provider.is_some())
                    // The agent's own AI transforms are not task steps.
                    && !crate::transform::is_transform_kind(&offer.descriptor.resource_kind)
                    // A host program's open is no step of a task: its window
                    // publishes nothing to work with. The session's `open-app`
                    // still opens an app the person names.
                    && !tools::canvas::is_host_app(offer)
                    // An operation needing confirmation is offered too: the
                    // step pauses until the person answers it.
                    // An operation its application marks deprecated names
                    // its replacement; offer only the replacement.
                    && !offer
                        .descriptor
                        .contract
                        .tags
                        .iter()
                        .any(|tag| tag.starts_with(DEPRECATED_TAG))
            })
            .collect::<Vec<_>>();
        let mut offers = offers;
        if let Some(api) = task_programs_offer(&offers) {
            offers.push(api);
        }
        // Content-free: which applications, how many operations each.
        let mut per_app = BTreeMap::<&str, usize>::new();
        for offer in &offers {
            *per_app.entry(offer.app.as_str()).or_default() += 1;
        }
        tracing::info!(revision = self.broker_revision, ?per_app, "task catalogue");
        let catalogue = std::sync::Arc::new(surface::Catalogue {
            offers,
            facts: Vec::new(),
            held: Vec::new(),
        });
        self.catalogue_cache = Some((self.broker_revision, std::sync::Arc::clone(&catalogue)));
        catalogue
    }

    /// Drain whatever the turn in flight has produced, without waiting.
    /// Called from every wait on the collaboration link, so completed calls
    /// queue up while a step runs.
    pub(super) fn pump_turn(&mut self) {
        let Some(mut turn) = self.turn.take() else {
            return;
        };
        loop {
            match turn.rx.try_recv() {
                Ok(event) => turn.absorb(event),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if turn.finished.is_none() {
                        turn.finished = Some(Err(Failure::said(crate::i18n::t(
                            "agent-failure-no-answer",
                        ))));
                    }
                    break;
                }
            }
        }
        self.turn = Some(turn);
    }

    /// Whether the model is still writing the turn in flight, so a wait
    /// should keep it moving.
    pub(super) fn turn_streaming(&self) -> bool {
        self.turn
            .as_ref()
            .is_some_and(|turn| turn.finished.is_none())
    }

    /// Wait up to `wait` for the turn's next event, then drain the rest.
    fn pump_turn_waiting(&mut self, wait: Duration) {
        let Some(mut turn) = self.turn.take() else {
            return;
        };
        if turn.ready.is_empty() && turn.finished.is_none() {
            match turn.rx.recv_timeout(wait) {
                Ok(event) => turn.absorb(event),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    turn.finished = Some(Err(Failure::said(crate::i18n::t(
                        "agent-failure-no-answer",
                    ))));
                }
            }
        }
        self.turn = Some(turn);
        self.pump_turn();
    }

    /// Run the loop for `run` until the model finishes, a bound is reached,
    /// or the task is cancelled.
    ///
    /// # Errors
    ///
    /// Only this session's own I/O failing; everything else is a [`LoopEnd`].
    pub(super) fn run_task_loop(&mut self, run: &Asked) -> Result<LoopEnd> {
        let mut state = LoopState::new();
        self.spending = run
            .spend
            .clone()
            .map(|allowed| Spending { allowed, run: 0 });
        // Each run is its issue's own permission scope: nothing of one
        // reaches the model in another. The conversation holds this run's
        // turns only.
        let result = self.run_task_loop_with(run, &mut state);
        self.spending = None;
        result
    }

    /// The longest one model turn may take.
    fn turn_budget(&self) -> Duration {
        MAX_LOOP_TURN_TIME.min(self.chooser.timeout().max(Duration::from_secs(1)))
    }

    fn run_task_loop_with(&mut self, run: &Asked, state: &mut LoopState) -> Result<LoopEnd> {
        let (protocol, model) = match self.chooser.loop_model() {
            Ok(model) => model,
            Err(route) => {
                return Ok(LoopEnd::Failed(super::planning::no_model(&route)));
            }
        };
        // A model that cannot call tools cannot carry out a task: the run
        // says which model it is rather than failing on its first answer.
        if let Some(refusal) = self.chooser.refuses_tasks() {
            return Ok(LoopEnd::Failed(Failure::said(refusal)));
        }
        let turn_budget = self.turn_budget();
        // The answer's own room comes out of the context, so a model whose
        // declared limit is small is compacted harder and one with a large
        // limit is not made to overflow by its answer.
        let mut context = compaction::Context::of_context(
            self.chooser.context_tokens(),
            self.chooser.declared_output_tokens(),
        );
        self.chooser.begin_trace();
        let catalogue = self.task_catalogue();
        Self::note_implied_parts(run, &catalogue, state);
        let summary = Self::operations_summary(run, &catalogue, state); // Stable first, volatile last. System and tools are the same bytes on
        // every turn of every task; this catalogue changes only when the
        // applications republish, so it opens the first turn in a block of
        // its own and the earlier conversation and the request follow it.
        // The gateway's own cache breakpoints make a provider read the
        // prefix back instead of prefilling it.
        let request = super::selection_prompt::task_request(run, &catalogue);
        // The request as the model reads it, the selection it is about and
        // the operations named for it included, so a run that acted at the
        // wrong place can be told apart from one that was never told where.
        tracing::info!(message = %run.message, %request, "task request");
        // The operations the first turn shows, as the model reads them: a run that wandered can
        // be told apart from one that was never shown the step it needed.
        tracing::info!(operations = %head(&summary, 12_000), "task operations");
        self.program_world(run, state)?;
        let place = program_steps::where_section(run, &state.programs);
        let now = ui::clock::local_now();
        let first = FirstTurn {
            operations: format!("Operations published right now:\n\n{summary}\n"),
            task: format!(
                "{place}\n{} {}\n\nThe request:\n\n{request}",
                today_line(now.date_naive()),
                clock_line(now.naive_local(), now.naive_utc())
            ),
        };
        let system = tools::system_for(run.agent_instruction.as_deref());
        let mut conversation = if self.chooser.uses_neutral_protocol() {
            Conversation::neutral_first(&system, &first)
        } else {
            Conversation::new(protocol, &model, &system, &first)
        };
        if let Some(selection) = self.chooser.selection() {
            conversation = conversation.budgeted_by(&selection.model);
        }
        let mut turn = 0;
        loop {
            state.turn = turn;
            if let Some(end) = self.run_turn(run, &mut conversation, state, turn, turn_budget)? {
                return Ok(end);
            }
            // What the next turn is sent stays within the model's context:
            // results read long ago give way to their handles.
            if let Some((sent, input_tokens)) = state.counted.take() {
                context.counted(sent, input_tokens);
            }
            context.compact(&mut conversation, &mut state.pages);
            if let Some(end) = state.stalled(&mut conversation) {
                return Ok(end);
            }
            turn += 1;
        }
    }

    /// What the request implies of the run, whatever the model later claims: a Chat operation when
    /// it asks for a message to be sent, and an operation in each application it names by the title
    /// it publishes. A question implies nothing of itself — it is answered in the reply — and only
    /// the person's own words imply anything: the selection quoted as context and earlier answers
    /// in the conversation do not.
    fn note_implied_parts(run: &Asked, catalogue: &surface::Catalogue, state: &mut LoopState) {
        let request = run.request().to_lowercase();
        let question = asks_question(&request);
        if !question
            && request.contains("chat")
            && asks_for_message(&request)
            && let Some(chat) = catalogue
                .offers
                .iter()
                .map(|offer| offer.app.as_str())
                .find(|app| app.to_lowercase().contains("chat"))
        {
            state.parts.push(tools::Part {
                text: "(implied) the message in Chat the request asks for".into(),
                app: chat.to_owned(),
            });
        }
        for (app, title) in tools::named_applications(catalogue, &request)
            .into_iter()
            .filter(|_| !question)
        {
            if !state.parts.iter().any(|part| app_matches(&part.app, app)) {
                state.parts.push(tools::Part {
                    text: format!("(implied) what the request asks for in {title}"),
                    app: app.to_owned(),
                });
            }
        }
    }

    /// The operations the first turn shows, as the model reads them. Asked on the canvas, the
    /// canvas' own steps come first, whole: showing what answers the question beside the
    /// conversation is the answer's normal shape, and the applications' operations are summarised
    /// after them.
    fn operations_summary(
        run: &Asked,
        catalogue: &surface::Catalogue,
        state: &mut LoopState,
    ) -> String {
        let canvas = tools::canvas::canvas_section(
            catalogue,
            &mut state.revealed,
            tools::canvas::conversation_of(&run.message),
        );
        if canvas.is_empty() {
            return tools::catalogue_summary(
                catalogue,
                &mut state.revealed,
                tools::MOST_SUMMARISED,
            );
        }
        // The apps' own records are read and changed by programs, whose apps the first turn lists
        // with their types and actions: the operations every open window and record publishes
        // beside them (hundreds, under long ids) would only make the first answer slower to come,
        // and are a `list_operations` away for the request that needs one.
        if catalogue.offers.iter().any(|offer| {
            offer.descriptor.address.resource == tilo_actions::remote::task_programs::RESOURCE
                && offer.descriptor.address.action == tilo_actions::remote::task_programs::RUN
        }) {
            return format!(
                "{canvas}Other applications' operations (windows, settings, apps without \
                 programs): ask `list_operations` with an application or `search` when the \
                 request needs one.\n"
            );
        }
        let rest = tools::canvas::without_canvas(catalogue);
        let summary = tools::catalogue_summary(&rest, &mut state.revealed, tools::MOST_SUMMARISED);
        format!("{canvas}Applications:\n\n{summary}")
    }

    /// Stop the turn in flight, keeping what it streamed in the record: `said`
    /// is what a person reads, in their own terms, and `reason` — a timeout,
    /// the task's time running out, a person's Stop — is kept in the record.
    fn interrupt_turn(&mut self, reason: &str, said: &str, cancelled: bool) -> Failure {
        if let Some(turn) = &mut self.turn {
            use agent_provider_config::protocol::{ErrorKind, ModelError};
            turn.presentation.fail(ModelError {
                kind: if cancelled {
                    ErrorKind::Cancelled
                } else {
                    ErrorKind::TimedOut
                },
                message: reason.into(),
            });
            turn.finished = Some(Err(Failure::spoken(said, reason)));
        }
        // The person reads what happened, in their terms; the reason the loop
        // stopped the turn is the record kept beside it.
        let failure = Failure::spoken(said, reason);
        match self.turn.take() {
            Some(turn) => turn.retained_failure(&failure),
            None => failure,
        }
    }

    /// One model turn: stream it, validate its complete batch, execute and record
    /// the results. `Some` ends the loop. A turn that ends early (cancelled,
    /// out of time, a broken model turn) asks the host to stop every
    /// independent invoke it left running.
    fn run_turn(
        &mut self,
        run: &Asked,
        conversation: &mut Conversation,
        state: &mut LoopState,
        turn: usize,
        timeout: Duration,
    ) -> Result<Option<LoopEnd>> {
        let end = self.run_turn_calls(run, conversation, state, turn, timeout);
        let mut left: Vec<Following> = std::mem::take(&mut state.in_flight)
            .into_iter()
            .map(|running| running.following)
            .collect();
        self.cancel_running(&mut left)?;
        state.settled.clear();
        end
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one model turn keeps streaming, call execution, timeouts and result recording in a single auditable sequence"
    )]
    fn run_turn_calls(
        &mut self,
        run: &Asked,
        conversation: &mut Conversation,
        state: &mut LoopState,
        turn: usize,
        timeout: Duration,
    ) -> Result<Option<LoopEnd>> {
        if self.task_cancelled(run) {
            return Ok(Some(LoopEnd::Cancelled(crate::i18n::t(
                "agent-session-cancelled-before-planning",
            ))));
        }
        if let Some(message) = self.spend_exhausted() {
            return Ok(Some(LoopEnd::Failed(Failure::said(message))));
        }
        let body = conversation.body(&tools::typed_loop_definitions());
        let sent = compaction::Context::tokens_of(&body);
        let turn_began = ui::time::now();
        let (rx, stop) = self.chooser.start_turn(body, timeout, turn);
        let mut active = ActiveTurn::new(rx);
        active.stop = Some(stop);
        self.turn = Some(active);
        if self.task_cancelled(run) {
            self.turn = None;
            return Ok(Some(LoopEnd::Cancelled(crate::i18n::t(
                "agent-session-cancelled-before-step",
            ))));
        }
        let mut results: HashMap<String, String> = HashMap::new();
        let mut finish: Option<Finishing> = None;
        let mut failed = false;
        // The gateway enforces the model's own deadline; this only abandons
        // a turn whose transport stopped answering. Time spent running steps
        // does not count against the model.
        let mut stepping = Duration::ZERO;
        let mut validated = false;
        let (body, refused_turn) = loop {
            self.pump_turn_waiting(Duration::from_millis(50));
            self.absorb_catalogue_updates();
            if self.task_cancelled(run) {
                // The person stopped it: the record needs no sentence beside
                // the one their own Stop gave it.
                let reason = self.interrupt_turn(
                    &crate::i18n::t("agent-cancel-thinking"),
                    &crate::i18n::t("agent-failure-cancelled"),
                    true,
                );
                return Ok(Some(LoopEnd::Cancelled(reason.detail().to_owned())));
            }
            // Streamed calls are presentation only. Dispatch the complete batch,
            // in order, only after validating every call and the terminal status.
            if !validated && let Some(active) = &mut self.turn {
                active.ready.clear();
                active.refused = None;
                // A call id the provider reused from an earlier turn is the
                // new call's own from here on: the answer is read, not
                // thrown away for its provider's numbering.
                if let Some(Ok(body)) = &mut active.finished {
                    *body = conversation.with_own_call_ids(body);
                }
                if let Some(Ok(body)) = &active.finished {
                    if is_output_limit(body) {
                        // A cut-off turn runs no call of its own: the loop
                        // continues it from where it stopped below, never
                        // dispatching a partial batch.
                        active.completed = 0;
                        validated = true;
                        continue;
                    }
                    match conversation.turn(body) {
                        Ok(turn) => {
                            active.completed = turn.calls.len();
                            active.ready.extend(turn.calls.into_iter().enumerate().map(
                                |(index, call)| StreamedCall {
                                    index,
                                    id: call.id,
                                    name: call.name,
                                    input: call.input,
                                },
                            ));
                            validated = true;
                        }
                        // The model's calls were refused for their shape or
                        // arguments: the body is kept whole beside the reason
                        // so the loop can answer its calls with what was wrong
                        // instead of ending the run. Retaining the text as a
                        // failure stays for a turn that cannot be answered,
                        // which the loop judges below. The person's sentence is
                        // the catalogue's, never the parser's own words ("invoke:
                        // missing operation"): those are the record kept beside
                        // it, for whoever debugs the run.
                        Err(error) => {
                            let failure = Failure::unusable(&error).retained(
                                &active.presentation.text,
                                &active.presentation.reasoning,
                            );
                            active.refused = Some((body.clone(), failure.clone()));
                            active.finished = Some(Err(failure));
                        }
                    }
                }
            }
            while let Some(call) = self.turn.as_mut().and_then(|turn| turn.ready.pop_front()) {
                let handling = ui::time::now();
                let handled = self.handle_call(run, state, &call, &mut failed)?;
                stepping += handling.elapsed();
                let cancelled = fold_settled(state, &mut results, &mut finish);
                if let Some(message) =
                    cancelled.or_else(|| settle(handled, &call.id, &mut results, &mut finish))
                {
                    self.interrupt_turn(&message, "The model call was stopped.", true);
                    return Ok(Some(LoopEnd::Cancelled(message)));
                }
            }
            let finished = self.turn.as_mut().and_then(|turn| {
                turn.ready
                    .is_empty()
                    .then(|| (turn.finished.take(), turn.refused.take()))
                    .and_then(|(finished, refused)| finished.map(|finished| (finished, refused)))
            });
            if let Some(finished) = finished {
                break finished;
            }
            if turn_began.elapsed().saturating_sub(stepping) > timeout + Duration::from_secs(5) {
                let message = self.interrupt_turn(
                    &crate::i18n::t("agent-fail-turn-not-finished"),
                    &crate::i18n::t("agent-failure-timed-out"),
                    false,
                );
                return Ok(Some(LoopEnd::Failed(message)));
            }
        };
        self.turn = None;
        // Every invoke the stream started settles before the turn is judged.
        if let Some(message) =
            self.settle_turn(run, state, &mut results, &mut finish, &mut failed)?
        {
            return Ok(Some(LoopEnd::Cancelled(message)));
        }
        tracing::info!(
            turn,
            turn_ms = turn_began.elapsed().as_millis() as u64,
            ok = body.is_ok(),
            steps = state.step_index,
            "agent loop turn"
        );
        let body = match body {
            Ok(body) => body,
            Err(error) => {
                self.chooser
                    .mark(tilo_model_client::TraceEvent::PlanFailed, turn);
                let asked_before = state.asked_for_words;
                if let Some(end) = state.finished_despite_failed_plan(conversation, error.detail())
                {
                    return Ok(Some(end));
                }
                // It has just asked the model for its words: the next turn gives them. Ending the
                // run here, in the turn that asked, told the person the agent did nothing after it
                // had done every step.
                if state.asked_for_words && !asked_before {
                    return Ok(None);
                }
                if let Some(end) = state.failed_after_asking_for_words(error.detail()) {
                    // The closing turn failed even after the model was asked
                    // for its words: the run ends without a reply rather than
                    // with one nobody said.
                    return Ok(Some(end));
                }
                // The model's own calls were refused for their shape or
                // arguments (a call missing an argument its declaration
                // requires, one naming a tool that is not offered): it is the
                // model's to correct, not the model or transport failing.
                // Answer its calls with what was wrong as tool error results
                // and take another turn, bounded, so one that keeps calling
                // the same way ends the task instead of looping.
                let malformed = refused_turn
                    .as_ref()
                    .and_then(|(raw, _)| Some((raw, malformed_turn(conversation, raw)?)));
                if let Some((raw, refused)) = &malformed
                    && state.malformed_turns < MOST_MALFORMED_TURNS
                {
                    state.malformed_turns += 1;
                    // What the model sent, as it sent it: the record of a refused call is the
                    // call, not only the parser's reason.
                    let sent = conversation
                        .read_calls(raw)
                        .map(|calls| {
                            calls
                                .iter()
                                .map(|call| {
                                    format!("{}({})", call.name, head(&call.input.to_string(), 600))
                                })
                                .collect::<Vec<_>>()
                                .join("; ")
                        })
                        .unwrap_or_default();
                    tracing::info!(
                        turn,
                        %error,
                        calls = refused.len(),
                        %sent,
                        "the model's calls were refused; answering them with what was wrong"
                    );
                    conversation
                        .refuse(raw, refused)
                        .map_err(anyhow::Error::msg)?;
                    // Nothing of this turn ran; the run takes another turn,
                    // as after any turn that dispatched no finish.
                    return Ok(None);
                }
                // An answer that arrived and could not be read, or one the
                // provider failed while writing (a stream that ended in an
                // error, a model that spent its whole output on nothing), ran
                // none of its calls: a batch runs only once the whole answer
                // is in and read. The model is asked again from the history
                // the run committed, told why its last answer was not used,
                // a bounded number of times, before the run ends on it. Calls
                // refused for their shape were answered with what was wrong
                // already (above): a model that keeps making them has had its
                // turns, and is not asked once more.
                if malformed.is_none()
                    && error.asks_again()
                    && state.unusable_turns < MOST_UNUSABLE_TURNS
                {
                    state.unusable_turns += 1;
                    tracing::info!(
                        turn,
                        %error,
                        again = state.unusable_turns,
                        "the model's answer could not be used; asking again"
                    );
                    conversation.push_user(&format!(
                        "{UNUSABLE_ANSWER} ({}). {ANSWER_AGAIN}",
                        head(error.detail().lines().next().unwrap_or_default(), 300)
                    ));
                    return Ok(None);
                }
                tracing::info!(turn, %error, "the model's turn failed; ending the run");
                // `error` is the failure's record: its sentence is what a
                // person reads, and what the turn kept goes beside it.
                return Ok(Some(LoopEnd::Failed(error)));
            }
        };
        self.charge_turn(run, &body)?;
        if let Some(input_tokens) = input_tokens(&body) {
            state.counted = Some((sent, input_tokens));
        }
        // A turn the model cut off at its output limit is not a failed plan:
        // it is an answer that stopped early. Its text — what it wrote of the
        // answer or of its calls — is kept, and the model is asked to go on
        // from there, a bounded number of times. None of a cut-off turn's
        // calls ran, so nothing is replayed.
        if let Ok(response) =
            serde_json::from_str::<agent_provider_config::protocol::ModelResponse>(&body)
            && response.stop_reason == agent_provider_config::protocol::StopReason::OutputLimit
        {
            self.chooser
                .mark(tilo_model_client::TraceEvent::PlanFailed, turn);
            if state.continuations >= MOST_CONTINUATIONS {
                // The model kept running into its output limit: the answer is
                // the one a person already knows from any cut-off turn.
                return Ok(Some(LoopEnd::Failed(Failure::model(
                    &agent_provider_config::protocol::ModelError::new(
                        agent_provider_config::protocol::ErrorKind::Truncated,
                    ),
                ))));
            }
            state.continuations += 1;
            let written = crate::model::written_text(&response.message);
            tracing::info!(
                turn,
                continuation = state.continuations,
                written = written.len(),
                "the model's answer was cut off at its output limit; continuing it"
            );
            conversation.continue_from(&written);
            return Ok(None);
        }
        state.continuations = 0;
        let (assistant, calls, words) = match conversation.turn(&body) {
            Ok(crate::model::Turn {
                assistant,
                calls,
                said,
            }) => {
                conversation.continued();
                // Debug capture (the launcher keeps the agent's stderr):
                // each tool call with its arguments, truncated.
                for call in &calls {
                    tracing::info!(
                        turn,
                        tool = %call.name,
                        args = %head(&call.input.to_string(), 400),
                        "agent tool call"
                    );
                }
                (assistant, calls, said)
            }
            Err(error) => {
                // A turn that called tools right after an answer was cut off
                // is not that answer's continuation: the joined text is the
                // record, and the run carries on with a fresh answer.
                conversation.continued();
                self.chooser
                    .mark(tilo_model_client::TraceEvent::PlanFailed, turn);
                let asked_before = state.asked_for_words;
                if let Some(end) = state.finished_despite_failed_plan(conversation, &error) {
                    return Ok(Some(end));
                }
                if state.asked_for_words && !asked_before {
                    return Ok(None);
                }
                if let Some(end) = state.failed_after_asking_for_words(&error) {
                    // The closing turn failed even after the model was asked
                    // for its words: the run ends without a reply rather than
                    // with one nobody said.
                    return Ok(Some(end));
                }
                // The person reads one clean sentence of what happened; the
                // parser's own reason (a turn with neither a call nor a word,
                // a call the loop cannot read) is the record kept beside it,
                // never the reply's words.
                return Ok(Some(LoopEnd::Failed(Failure::spoken(
                    &crate::i18n::t("agent-failure-invalid-response"),
                    &error,
                ))));
            }
        };
        // Calls the turn did not report as they completed (a transport that
        // does not stream, or a gateway past its event bound) run now, in
        // order.
        for (index, call) in calls.iter().enumerate() {
            if results.contains_key(&call.id)
                || finish
                    .as_ref()
                    .is_some_and(|finishing| finishing.id == call.id)
            {
                continue;
            }
            let streamed = StreamedCall {
                index,
                id: call.id.clone(),
                name: call.name.clone(),
                input: call.input.clone(),
            };
            let handled = self.handle_call(run, state, &streamed, &mut failed)?;
            let cancelled = fold_settled(state, &mut results, &mut finish);
            if let Some(message) =
                cancelled.or_else(|| settle(handled, &call.id, &mut results, &mut finish))
            {
                return Ok(Some(LoopEnd::Cancelled(message)));
            }
        }
        if let Some(message) =
            self.settle_turn(run, state, &mut results, &mut finish, &mut failed)?
        {
            return Ok(Some(LoopEnd::Cancelled(message)));
        }
        // Reading on in a long result is reading what was asked for, not
        // looking for what to do: a turn of only that is not a lookup.
        if calls.iter().all(|call| {
            call.name != tools::INVOKE
                && call.name != tools::PLAN
                && call.name != crate::programs::TILO
                && call.name != tools::web::SEARCH_WEB
                && call.name != tools::web::READ_PAGE
        }) && calls
            .iter()
            .any(|call| call.name != crate::pages::READ_RESULT)
        {
            state.discovery_turns += 1;
        }
        // Completion check: a finish while a part of the request has no
        // successful operation (and no explanation) is refused, a bounded
        // number of times, and the model gets another turn.
        if let Some(Finishing { id, by_plan, said }) = &finish
            && !failed
            && said.done
            && state.refusals < MOST_REFUSALS
        {
            let undone: Vec<String> = state
                .undone_parts()
                .iter()
                .map(|part| format!("{} (in {})", part.text, part.app))
                .collect();
            if !undone.is_empty() {
                state.refusals += 1;
                tracing::info!(parts = ?undone, "finish refused: parts not done");
                let note = format!(
                    "Not finished: these parts of the request have no successful operation \
                     yet: {}. Do them now with invoke (a reply in your panel does not reach \
                     anyone), or finish with plan listing them under `unable` with the reason.",
                    undone.join("; ")
                );
                let entry = results.entry(id.clone()).or_default();
                if *by_plan || entry.is_empty() {
                    *entry = note;
                } else {
                    entry.push('\n');
                    entry.push_str(&note);
                }
                finish = None;
            }
        }
        if let Some(Finishing {
            id, by_plan: true, ..
        }) = &finish
        {
            results.insert(
                id.clone(),
                if failed {
                    "Not finished: an invoke in this turn did not succeed. Read its result, \
                     then continue or call plan again."
                        .into()
                } else {
                    "Finished.".into()
                },
            );
        }
        // A finish that did nothing and said nothing is not an answer, whatever
        // it says of itself: the person would get a run that reports success
        // for work nobody read about. Feed it back as the call's error, as a
        // malformed call is, bounded — a model that keeps finishing that way
        // ends the run with the clean failure row, never a success.
        let silent = finish.as_ref().is_some_and(|Finishing { said, .. }| {
            !failed && said.done && said.reply.is_none() && state.completed == 0
        }) && tools::says_nothing(&words);
        if silent {
            let id = finish.as_ref().map(|finishing| finishing.id.clone());
            state.silent_finishes += 1;
            tracing::info!(
                turn,
                "finish refused: nothing was done and nothing was said"
            );
            if state.silent_finishes > MOST_SILENT_FINISHES {
                return Ok(Some(LoopEnd::Failed(Failure::spoken(
                    &crate::i18n::t("agent-failure-no-answer"),
                    "the model finished without running a step or saying anything",
                ))));
            }
            if let Some(id) = id {
                *results.entry(id).or_default() = tools::NOTHING_DONE_REPLY.into();
            }
            finish = None;
        } else {
            // A turn that did something, or said something, is progress: the
            // count of silent finishes is how many in a row cost the run, so a
            // real turn clears it.
            state.silent_finishes = 0;
        }
        let ordered = calls
            .iter()
            .map(|call| {
                (
                    call.id.clone(),
                    results
                        .remove(&call.id)
                        .unwrap_or_else(|| "Not handled.".into()),
                )
            })
            .collect::<Vec<(String, String)>>();
        // Debug capture beside "agent tool call": what each call answered, truncated.
        for (id, result) in &ordered {
            tracing::info!(turn, %id, result = %head(result, 600), "agent tool result");
        }
        conversation
            .push(assistant, ordered)
            .map_err(anyhow::Error::msg)?;
        self.show_pictures(conversation, state);
        // What the model wrote for the person in this turn is the last thing it
        // said, whether it rode on a call or stood alone. A run that ends
        // without fresh words (a closing turn lost) answers with these.
        if let Some(Finishing { said, .. }) = &finish
            && let Some(reply) = &said.reply
        {
            state.reply = Some(reply.clone());
        } else if let Some(spoken) = tools::spoken_reply(&words) {
            state.reply = Some(spoken);
        }
        // A turn that called nothing and answered in words is the model's
        // answer: a chat-style model ends a turn that way, so its words are the
        // turn's reply. Every step the turn started has settled by now
        // (`settle_turn`), so nothing the answer could describe is still
        // running. A model that ends with `plan` keeps that finish. Words that
        // say nothing ("…", "placeholder") are no answer: asked once more, then
        // the readable failure.
        if calls.is_empty() && !failed {
            let Some(answer) = tools::spoken_reply(&words) else {
                self.chooser
                    .mark(tilo_model_client::TraceEvent::PlanFailed, turn);
                return Ok(state.unanswered(conversation, &words));
            };
            self.chooser
                .mark(tilo_model_client::TraceEvent::PlanReady, turn);
            return Ok(Some(LoopEnd::Finished(
                tools::Finish {
                    reply: Some(answer),
                    done: true,
                    view: None,
                },
                state.completed,
            )));
        }
        // The model's own words are the reply when its `plan` carried none: a
        // model that wrote its answer in the turn's text said it, and no line
        // stands in for what it said. A finish that left the person without
        // words, with steps done, is asked once for them; a model that does not
        // give them ends the run with what it did say earlier, and with none,
        // the readable failure: an empty answer never passes as one.
        match finish {
            Some(Finishing { mut said, .. }) if !failed => {
                if said.reply.is_none() {
                    said.reply = tools::spoken_reply(&words);
                }
                if said.reply.is_none() && state.completed > 0 && !state.asked_for_words {
                    state.asked_for_words = true;
                    tracing::info!(turn, "finished with work done and no reply: asking for it");
                    conversation.push_user(SAY_WHAT_YOU_DID);
                    self.chooser
                        .mark(tilo_model_client::TraceEvent::PlanFailed, turn);
                    return Ok(None);
                }
                if said.reply.is_none() {
                    said.reply.clone_from(&state.reply);
                }
                if said.done && said.reply.is_none() {
                    self.chooser
                        .mark(tilo_model_client::TraceEvent::PlanFailed, turn);
                    return Ok(state.unanswered(conversation, &words));
                }
                self.chooser
                    .mark(tilo_model_client::TraceEvent::PlanReady, turn);
                Ok(Some(LoopEnd::Finished(said, state.completed)))
            }
            _ => Ok(None),
        }
    }

    /// Handle one completed call. `failed` records that an `invoke` of this
    /// turn did not succeed; the rest of the turn's invokes are then not run.
    /// What `call` does, as the step the person is told the run is at:
    /// searching the web, reading or changing an application's documents,
    /// or finding out what an application can do. `None` for a call that
    /// is no step of its own (finishing, reading on in a long result).
    fn step_of(&mut self, run: &Asked, state: &LoopState, call: &StreamedCall) -> Option<TaskStep> {
        let catalogue = self.task_catalogue();
        // The application's name as the person sees it: the selection's
        // application names itself, an openable one is offered under its
        // name, and any other is named by its id's last part.
        let app_name = |app: &str| {
            if let Some(subject) = run.subject.as_ref().filter(|subject| subject.app_id == app) {
                return subject.app_label.clone();
            }
            catalogue
                .offers
                .iter()
                .find(|offer| {
                    offer.app == app && offer.descriptor.resource_kind == surface::APPLICATION_KIND
                })
                .map_or_else(
                    || {
                        let last = app.rsplit('.').next().unwrap_or(app).replace('-', " ");
                        ui::format::capitalize_first(&last)
                    },
                    |offer| offer.descriptor.resource_label.clone(),
                )
        };
        match call.name.as_str() {
            tools::INVOKE => {
                let id = call.input.get("operation").and_then(Value::as_str)?;
                let id = state.revealed.resolve(id).unwrap_or_else(|_| id.to_owned());
                let offer = surface::find(&catalogue.offers, &id)?;
                let descriptor = &offer.descriptor;
                if descriptor.resource_kind == surface::APPLICATION_KIND {
                    return None;
                }
                let reads = descriptor.kind == tilo_actions::OperationKind::View
                    || descriptor.contract.kind == tilo_actions::ActionKind::Read;
                let app = app_name(&offer.app);
                Some(if reads {
                    TaskStep::Reading(app)
                } else {
                    TaskStep::Editing(app)
                })
            }
            tools::web::SEARCH_WEB => Some(TaskStep::Searching(
                call.input
                    .get("query")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            )),
            tools::web::READ_PAGE => Some(TaskStep::Reading(
                call.input
                    .get("url")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            )),
            tools::LIST_OPERATIONS | tools::DESCRIBE_OPERATION => {
                let app = call
                    .input
                    .get("application")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| {
                        let id = call.input.get("operation").and_then(Value::as_str)?;
                        let id = state.revealed.resolve(id).unwrap_or_else(|_| id.to_owned());
                        surface::find(&catalogue.offers, &id).map(|offer| offer.app.clone())
                    })?;
                Some(TaskStep::Exploring(app_name(&app)))
            }
            _ => None,
        }
    }

    /// Tell the person the run is at `call`'s step, once each time the step
    /// changes: what the composer and Task Center show while it runs.
    fn tell_step(&mut self, run: &Asked, state: &mut LoopState, call: &StreamedCall) -> Result<()> {
        let Some(step) = self.step_of(run, state, call) else {
            return Ok(());
        };
        let note = crate::i18n::step(&step);
        if state.told_step.as_deref() == Some(note.as_str()) || state.steps_told >= MOST_STEPS_TOLD
        {
            return Ok(());
        }
        state.steps_told += 1;
        state.told_step = Some(note.clone());
        self.report_task(run, TurnUpdate::Progress { message: note })
    }

    /// Tell the person where a request of several items is, once per item:
    /// "3 of 8: reviewed Jane Doe", in the issue's thread. Each item gets
    /// its own bounded number of step notes.
    fn tell_progress(&mut self, run: &Asked, state: &mut LoopState, input: &Value) -> Result<()> {
        let Some(progress) = progress::Progress::of_input(input) else {
            return Ok(());
        };
        if state.progress.as_ref() == Some(&progress) {
            return Ok(());
        }
        let line = progress.line();
        state.progress = Some(progress);
        state.progressed += 1;
        state.steps_told = 0;
        self.report_task(run, TurnUpdate::Progress { message: line })
    }

    fn handle_call(
        &mut self,
        run: &Asked,
        state: &mut LoopState,
        call: &StreamedCall,
        failed: &mut bool,
    ) -> Result<Handled> {
        // Both are about the step and neither depends on the other: the note
        // is shown once per distinct step, while every resource the step
        // reads is listed, however many steps share a note.
        self.tell_step(run, state, call)?;
        match call.name.as_str() {
            tools::PLAN => {
                let mut input = call.input.clone();
                self.name_part_applications(&mut input);
                state.absorb_parts(&input);
                let finish = tools::read_finish(&input);
                if let Some(view) = &finish.view
                    && let Err(refused) = self.keep_view_answer(run, state, view)?
                {
                    return Ok(Handled::Result(refused));
                }
                Ok(Handled::Finish(finish))
            }
            crate::programs::TILO => {
                // A program may be the request's first step: what a step says of the request (the
                // separate things it asks for, how far it got, what cannot be done) rides on
                // whichever tool the step is.
                let mut input = call.input.clone();
                self.name_part_applications(&mut input);
                state.absorb_parts(&input);
                self.tell_progress(run, state, &input)?;
                self.run_program(run, state, call)
            }
            tools::INVOKE => {
                // An id copied without its routing prefix names the one
                // shown operation with that address; everything below sees
                // the id as listed.
                let mut input = call.input.clone();
                if let Some(Ok(id)) = input
                    .get("operation")
                    .and_then(Value::as_str)
                    .map(|operation| state.revealed.resolve(operation))
                    && let Some(object) = input.as_object_mut()
                {
                    object.insert("operation".into(), Value::String(id));
                }
                self.name_part_applications(&mut input);
                let call = &StreamedCall {
                    index: call.index,
                    id: call.id.clone(),
                    name: call.name.clone(),
                    input,
                };
                state.absorb_parts(&call.input);
                self.tell_progress(run, state, &call.input)?;
                let independent = tools::invoke_independent(&call.input);
                if independent {
                    match self.start_independent(run, state, call)? {
                        Started::Running => return Ok(Handled::Pending),
                        Started::Refused(report) => {
                            return Ok(self.conclude_invoke(state, call, *report, failed));
                        }
                        Started::Cancelled(message) => return Ok(Handled::Cancelled(message)),
                        Started::InOrder => {}
                    }
                }
                // Whatever this call may read, the invokes still running
                // write first: they settle before it starts.
                if let Some(message) = self.drain_in_flight(run, state, failed)? {
                    return Ok(Handled::Cancelled(message));
                }
                if *failed && !independent {
                    return Ok(Handled::Result(tools::invoke_result(
                        &InvokeReport {
                            skipped: Some("an earlier invoke in this turn did not succeed".into()),
                            ..InvokeReport::default()
                        },
                        &mut state.pages,
                    )));
                }
                let (report, cancelled) = self.invoke_from_loop(run, state, &call.input)?;
                if let Some(message) = cancelled {
                    return Ok(Handled::Cancelled(message));
                }
                Ok(self.conclude_invoke(state, call, report, failed))
            }
            crate::pages::READ_RESULT => Ok(Handled::Result(state.pages.read(&call.input))),
            tools::web::SEARCH_WEB | tools::web::READ_PAGE => {
                self.run_web(run, state, call, failed)
            }
            name => {
                let catalogue = self.task_catalogue();
                let mut answer = tools::answer(name, &call.input, &catalogue, &mut state.revealed);
                if state.discovery_turns >= DISCOVERY_NUDGE {
                    let _ = std::fmt::Write::write_fmt(
                        &mut answer,
                        format_args!(
                            "\n(You have spent {} turns only looking things up. Act now: \
                             invoke the operations you need with ids already shown, or finish \
                             with plan listing what you cannot do under `unable`.)",
                            state.discovery_turns
                        ),
                    );
                }
                Ok(Handled::Result(answer))
            }
        }
    }

    /// Record how one invoke went and turn its report into the call's
    /// result. `failed` records that it did not succeed.
    fn conclude_invoke(
        &mut self,
        state: &mut LoopState,
        call: &StreamedCall,
        report: InvokeReport,
        failed: &mut bool,
    ) -> Handled {
        if report.succeeded && self.chooser.sees() {
            for address in &report.resources {
                if tilo_actions::content_hash(address).is_some()
                    && !state.pictures.contains(address)
                    && state.pictures.len() < MOST_PICTURES
                {
                    state.pictures.push(address.clone());
                }
            }
        }
        if !report.succeeded {
            tracing::info!(
                turn = state.turn,
                error = %head(
                    report.error.as_deref().unwrap_or("not run"),
                    300
                ),
                "agent invoke failed"
            );
            *failed = true;
        } else if let Some(operation) = call.input.get("operation").and_then(Value::as_str) {
            let catalogue = self.task_catalogue();
            if let Some(offer) = surface::find(&catalogue.offers, operation) {
                state.did_in(offer);
                // Showing or arranging an app's thing on the canvas is what a
                // request naming that app asked of it there.
                if tools::canvas::is_canvas_step(offer) {
                    let parameters = call
                        .input
                        .get(tools::INVOKE_PARAMETERS)
                        .unwrap_or(&Value::Null);
                    for app in tools::canvas::apps_shown(&catalogue, offer, parameters) {
                        *state.done_apps.entry(app).or_default() += 1;
                    }
                }
            }
        }
        let mut report = report;
        if !report.succeeded
            && report.skipped.is_none()
            && let Some(error) = report.error.clone()
        {
            let operation = call.input.get("operation").and_then(Value::as_str);
            let key = format!("{} {error}", operation.unwrap_or("?"));
            if !state.failures.insert(key) {
                report.note = Some(
                    "This invoke already failed the same way in this task; do not \
                     repeat it unchanged. Change the operation or its parameters, or \
                     finish with plan listing the part under `unable`."
                        .into(),
                );
            }
        }
        // Opening an application or changing only the view is a step
        // towards the request, never its end: a reply on one does not
        // finish the task.
        let navigation = call
            .input
            .get("operation")
            .and_then(Value::as_str)
            .and_then(|operation| {
                let catalogue = self.task_catalogue();
                surface::find(&catalogue.offers, operation).map(|offer| {
                    tools::opened_app(offer).is_some()
                        || offer.descriptor.kind == tilo_actions::OperationKind::View
                })
            })
            .unwrap_or(false);
        let reply = call
            .input
            .get(tools::INVOKE_REPLY)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|reply| !tools::says_nothing(reply));
        if navigation && reply.is_some() && report.succeeded {
            report.note = Some(
                report
                    .note
                    .take()
                    .map_or_else(String::new, |note| note + " ")
                    + "Not finished: opening an application or changing your view \
                       does not finish a task. Now do what the request asks there, and \
                       put your reply on the last of those invokes.",
            );
        }
        let result = tools::invoke_result(&report, &mut state.pages);
        // The last words the model wrote for the person are the reply whatever
        // becomes of the call they rode on: a failed invoke loses no sentence,
        // and the run's end keeps what the model said rather than inventing a
        // line for it.
        if let Some(reply) = reply {
            state.reply = Some(tools::bound_reply(reply));
        }
        let reply = reply.filter(|_| !navigation);
        match reply {
            Some(reply) if report.succeeded => Handled::ResultFinish(
                result,
                tools::Finish {
                    reply: Some(tools::bound_reply(reply)),
                    done: true,
                    view: None,
                },
            ),
            _ => Handled::Result(result),
        }
    }

    /// Run one of the agent's own web tools. The search or the read happens
    /// here, as this step of the run, and its answer is the call's result: the
    /// person was told what it is about as the step began, and the turn's
    /// record keeps what it asked and what came of it beside every other step.
    fn run_web(
        &mut self,
        run: &Asked,
        state: &mut LoopState,
        call: &StreamedCall,
        failed: &mut bool,
    ) -> Result<Handled> {
        let web_call = match tools::web::Call::read(&call.name, &call.input) {
            Ok(web_call) => web_call,
            // The refusal says what the tool takes, so the next call can be
            // right without a describe turn.
            Err(refused) => return Ok(Handled::Result(refused)),
        };
        let report = match self.web().run(&web_call) {
            Ok(value) => InvokeReport {
                succeeded: true,
                value: Some(value),
                ..InvokeReport::default()
            },
            Err(error) => refused_report(error),
        };
        self.record_web(run, &web_call, &report)?;
        if report.succeeded {
            state.progressed += 1;
        }
        if !report.succeeded {
            tracing::info!(
                tool = call.name,
                error = %head(report.error.as_deref().unwrap_or("not run"), 300),
                "agent web tool failed"
            );
            *failed = true;
        }
        Ok(Handled::Result(tools::invoke_result(
            &report,
            &mut state.pages,
        )))
    }

    /// Check one `invoke` against the live catalogue and the task's bounds,
    /// exactly as every invoke is checked before it runs; `Err` is the
    /// report of a call refused before anything ran.
    fn prepare_invoke(
        &mut self,
        run: &Asked,
        state: &LoopState,
        input: &Value,
    ) -> Result<Prepared, Box<InvokeReport>> {
        let refused = |error: String| Box::new(refused_report(error));
        let mut invocation = tools::read_invoke(input, &state.revealed).map_err(refused)?;
        let catalogue = self.task_catalogue();
        if let Some(offer) = surface::find(&catalogue.offers, &invocation.operation) {
            tools::decode_structured_arguments(offer, &mut invocation.arguments);
        }
        // An argument error says what the operation takes, so the next
        // attempt needs no describe turn.
        let with_hint = |why: String| match surface::find(&catalogue.offers, &invocation.operation)
        {
            Some(offer) => format!("{why}. {}", tools::parameter_hint(offer)),
            None => why,
        };
        let instruction = match Instruction::from_answer(
            &invocation.operation,
            run.instruction(),
            Some(&invocation.arguments),
            &catalogue.offers,
        ) {
            Ok(instruction) => instruction,
            Err(plan::Refused::NotExported(id)) => {
                return Err(refused(format!("{id} is no longer published")));
            }
            Err(plan::Refused::Unusable(why)) => return Err(refused(with_hint(why))),
        };
        let Instruction::Perform {
            id,
            arguments,
            confirmation,
        } = instruction
        else {
            return Err(refused("that names no operation".into()));
        };
        if !matches!(confirmation, ConfirmationEvidence::None) {
            return Err(refused(
                "never pass `confirm` or `confirmation_phrase`: invoke the operation without \
                 them and the person is asked to confirm it"
                    .into(),
            ));
        }
        let Some(offer) = surface::find(&catalogue.offers, &id).cloned() else {
            return Err(refused(format!("{id} is no longer published")));
        };
        let action =
            task_action_from_offer(&offer, arguments).map_err(|error| refused(with_hint(error)))?;
        let arguments = arguments_for_step(
            action.carries.as_deref(),
            &action.arguments,
            state.previous.as_ref(),
        )
        .map_err(refused)?;
        Ok(Prepared {
            before: availability(&catalogue),
            offer,
            action,
            arguments,
        })
    }

    /// Check and perform one `invoke`. The second value is set when the task
    /// was cancelled and the loop must stop.
    fn invoke_from_loop(
        &mut self,
        run: &Asked,
        state: &mut LoopState,
        input: &Value,
    ) -> Result<(InvokeReport, Option<String>)> {
        let Prepared {
            offer,
            action,
            arguments,
            before,
        } = match self.prepare_invoke(run, state, input) {
            Ok(prepared) => prepared,
            Err(report) => return Ok((*report, None)),
        };
        let before_revision = self.broker_revision;
        // The windows the application an open starts already has, so the
        // new window's operations can be told apart.
        let opened = tools::opened_app(&offer).map(|app| (app.to_owned(), self.app_windows(app)));
        let index = state.completed;
        let settled = self.run_task_step(run, index, &action, &arguments)?;
        state.step_index += 1;
        self.resource_action(run, &action, settled.as_ref())?;
        match settled {
            Ok(outcome) => {
                let succeeded = Succeeded {
                    offer: &offer,
                    before: &before,
                    before_revision,
                    opened,
                    index,
                };
                Ok((self.invoke_succeeded(state, succeeded, outcome)?, None))
            }
            Err(TaskActionStop::Failed(message)) => Ok((refused_report(message), None)),
            Err(TaskActionStop::Cancelled(message)) => Ok((
                InvokeReport::default(),
                Some(
                    message.unwrap_or_else(|| crate::i18n::t("agent-session-cancelled-in-action")),
                ),
            )),
        }
    }

    /// Report an invoke that succeeded: record it, wait for what an open or
    /// a view change publishes, and say what it returned and unlocked.
    fn invoke_succeeded(
        &mut self,
        state: &mut LoopState,
        succeeded: Succeeded<'_>,
        outcome: StepOutcome,
    ) -> Result<InvokeReport> {
        let Succeeded {
            offer,
            before,
            before_revision,
            opened,
            index,
        } = succeeded;
        self.chooser
            .mark(tilo_model_client::TraceEvent::ActionSettled, index);
        state.completed += 1;
        state.previous = Some(outcome.clone());
        let view = offer.descriptor.kind == tilo_actions::OperationKind::View;
        let mut note = None;
        if let Some((app, opened_before)) = &opened {
            let records = opened_records(outcome.output.value.as_ref(), app);
            let appeared = self.settle_opened(app, opened_before, &records, OPEN_SETTLE)?;
            let new_window = self
                .app_windows(app)
                .iter()
                .any(|window| !opened_before.contains(window));
            note = Some(if appeared && !new_window {
                format!(
                    "{app} is open in the window it already had; its operations were \
                     already published (ask `{}` with application=\"{app}\" for them).",
                    tools::LIST_OPERATIONS
                )
            } else if appeared {
                format!(
                    "{app} is open in a new window. Use the ids under newly_available \
                     for it (ask `{}` with application=\"{app}\" for the rest).",
                    tools::LIST_OPERATIONS
                )
            } else {
                format!(
                    "{app} is opening but has not published its operations yet; ask \
                     `{}` with application=\"{app}\" next.",
                    tools::LIST_OPERATIONS
                )
            });
        } else if view {
            self.settle_view(before_revision, VIEW_SETTLE)?;
        }
        let after = self.task_catalogue();
        let most = if opened.is_some() {
            MOST_NEWLY_OPENED
        } else {
            MOST_NEWLY_AVAILABLE
        };
        let newly_available = newly_available(before, &after, &mut state.revealed, most);
        let value = outcome.output.value;
        let resources = outcome.output.resources;
        Ok(InvokeReport {
            succeeded: true,
            changed_view: if view { value.clone() } else { None },
            value: if view { None } else { value },
            resources,
            newly_available,
            note,
            ..InvokeReport::default()
        })
    }

    /// Start an `invoke` the model marked independent of the steps before
    /// it, without waiting for those still running to settle, when nothing
    /// it could read is written by them: it is checked exactly as any invoke,
    /// then started as its own broker invocation (authorized and journaled
    /// on its own) and followed with the others when the turn needs their
    /// results.
    ///
    /// Only a plain invocation qualifies — a Task Center action or a native
    /// task's child — on a record no running step acts on: an open, a view
    /// change (both change what is published next) and a step taking the
    /// previous step's result all run in order.
    fn start_independent(
        &mut self,
        run: &Asked,
        state: &mut LoopState,
        call: &StreamedCall,
    ) -> Result<Started> {
        if state.in_flight.len() >= MAX_IN_FLIGHT {
            return Ok(Started::InOrder);
        }
        let prepared = match self.prepare_invoke(run, state, &call.input) {
            Ok(prepared) => prepared,
            Err(report) => return Ok(Started::Refused(report)),
        };
        let peer = prepared.action.peer_id;
        if tools::opened_app(&prepared.offer).is_some()
            || prepared.offer.descriptor.kind == tilo_actions::OperationKind::View
            || prepared.action.carries.is_some()
            || state
                .in_flight
                .iter()
                .any(|running| running.following.action.peer_id == peer)
        {
            return Ok(Started::InOrder);
        }
        if self.task_cancelled(run) {
            return Ok(Started::Cancelled(crate::i18n::t(
                "agent-session-cancelled-before-step",
            )));
        }
        let index = state.completed + state.in_flight.len();
        self.chooser
            .mark(tilo_model_client::TraceEvent::ActionSubmitted, index);
        let started = self.start_task_invocation(run, &prepared.action, &prepared.arguments)?;
        state.step_index += 1;
        match started {
            Ok(following) => {
                state.in_flight.push(InFlight {
                    call: call.clone(),
                    offer: prepared.offer,
                    before: prepared.before,
                    index,
                    following,
                });
                Ok(Started::Running)
            }
            Err(stop) => {
                self.resource_action(run, &prepared.action, Err(&stop))?;
                Ok(match stop {
                    TaskActionStop::Failed(message) => {
                        Started::Refused(Box::new(refused_report(message)))
                    }
                    TaskActionStop::Cancelled(message) => Started::Cancelled(
                        message
                            .unwrap_or_else(|| crate::i18n::t("agent-session-cancelled-in-action")),
                    ),
                })
            }
        }
    }

    /// Follow every independent invoke still running until each settles,
    /// and queue their results, in call order, for the turn
    /// ([`LoopState::settled`]). `Some` is a cancellation: every one still
    /// running was cancelled.
    fn drain_in_flight(
        &mut self,
        run: &Asked,
        state: &mut LoopState,
        failed: &mut bool,
    ) -> Result<Option<String>> {
        if state.in_flight.is_empty() {
            return Ok(None);
        }
        let (started, mut followed): (Vec<_>, Vec<_>) = std::mem::take(&mut state.in_flight)
            .into_iter()
            .map(|running| {
                (
                    (running.call, running.offer, running.before, running.index),
                    running.following,
                )
            })
            .unzip();
        self.follow_native(run, &mut followed)?;
        let mut cancelled = None;
        for ((call, offer, before, index), following) in started.into_iter().zip(followed) {
            let action = following.action.clone();
            let settled = following.into_outcome();
            self.resource_action(run, &action, settled.as_ref())?;
            let report = match settled {
                Ok(outcome) => {
                    let succeeded = Succeeded {
                        offer: &offer,
                        before: &before,
                        before_revision: self.broker_revision,
                        opened: None,
                        index,
                    };
                    self.invoke_succeeded(state, succeeded, outcome)?
                }
                Err(TaskActionStop::Failed(message)) => refused_report(message),
                Err(TaskActionStop::Cancelled(message)) => {
                    cancelled.get_or_insert_with(|| {
                        message
                            .unwrap_or_else(|| crate::i18n::t("agent-session-cancelled-in-action"))
                    });
                    continue;
                }
            };
            let handled = self.conclude_invoke(state, &call, report, failed);
            state.settled.push((call.id, handled));
        }
        Ok(cancelled)
    }

    /// Let every invoke the turn started settle and take in their results;
    /// `Some` is a cancellation.
    fn settle_turn(
        &mut self,
        run: &Asked,
        state: &mut LoopState,
        results: &mut HashMap<String, String>,
        finish: &mut Option<Finishing>,
        failed: &mut bool,
    ) -> Result<Option<String>> {
        let cancelled = self.drain_in_flight(run, state, failed)?;
        let folded = fold_settled(state, results, finish);
        Ok(cancelled.or(folded))
    }

    /// Whether there is no next call to run. Tools execute only after the
    /// complete turn validates, so its final view call must also be allowed
    /// to receive a late republication before recording its observation.
    fn turn_idle(&self) -> bool {
        self.turn
            .as_ref()
            .is_some_and(|turn| turn.ready.is_empty() && !turn.begun.contains(&turn.completed))
    }

    /// After a view step, take up the catalogue republication it caused —
    /// on the event, never on a timer. One the broker already delivered
    /// (with or before the step's reply) is taken at once. Beyond that the
    /// loop listens only when no next call is ready ([`Self::turn_idle`]),
    /// at most `most`. In particular a terminal plan or another invoke is
    /// never held up; a tool-only turn waits for the observation it needs
    /// to make its next decision.
    fn settle_view(&mut self, since: u64, most: Duration) -> Result<()> {
        let deadline = ui::time::now() + most;
        let mut deferred = Vec::new();
        loop {
            self.pump_turn();
            self.absorb_catalogue_updates();
            let left = deadline.saturating_duration_since(ui::time::now());
            if self.broker_revision != since || !self.turn_idle() {
                break;
            }
            if left.is_zero() {
                #[cfg(test)]
                SETTLES_RAN_OUT.with(|ran_out| ran_out.set(ran_out.get() + 1));
                break;
            }
            match self.link.recv(left.min(Duration::from_millis(5)))? {
                Some(CollabEvent::ActionCatalogue { change }) => {
                    self.apply_broker_catalogue(&change);
                }
                Some(event) => deferred.push(event),
                None => {}
            }
        }
        self.link.defer(deferred);
        Ok(())
    }

    /// The windows of `app` the task catalogue publishes operations for, as
    /// `(broker peer, record)`: each window is its own connection serving
    /// its own record, so a second window is told apart from the first even
    /// when it publishes the same resource ids.
    fn app_windows(&mut self, app: &str) -> HashSet<(u64, String)> {
        let peers: HashSet<u64> = self
            .task_catalogue()
            .offers
            .iter()
            .filter(|offer| offer.app == app)
            .filter_map(|offer| offer.broker_peer_id)
            .collect();
        self.broker_catalogue
            .surfaces()
            .iter()
            .filter(|surface| peers.contains(&surface.peer_id))
            .map(|surface| (surface.peer_id, surface.document_id.clone()))
            .collect()
    }

    /// After an open of `app`, wait up to `wait` for the opened window's
    /// operations, ending on the catalogue that brings them: a window for
    /// every record the open's result names (`records`), or else any window
    /// of `app` not in `before`. Returns at once when they are already
    /// published, as for an open that lands on a window already there.
    /// Whether they are.
    fn settle_opened(
        &mut self,
        app: &str,
        before: &HashSet<(u64, String)>,
        records: &[String],
        wait: Duration,
    ) -> Result<bool> {
        let appeared = |session: &mut Self| {
            let windows = session.app_windows(app);
            if records.is_empty() {
                windows.iter().any(|window| !before.contains(window))
            } else {
                records
                    .iter()
                    .all(|record| windows.iter().any(|(_, published)| published == record))
            }
        };
        let deadline = ui::time::now() + wait;
        let mut deferred = Vec::new();
        let mut found = appeared(self);
        while !found && ui::time::now() < deadline {
            self.pump_turn();
            let left = deadline.saturating_duration_since(ui::time::now());
            match self.link.recv(left.min(Duration::from_millis(20)))? {
                Some(CollabEvent::ActionCatalogue { change }) => {
                    self.apply_broker_catalogue(&change);
                    found = appeared(self);
                }
                Some(event) => deferred.push(event),
                None => {}
            }
        }
        self.link.defer(deferred);
        #[cfg(test)]
        if !found {
            SETTLES_RAN_OUT.with(|ran_out| ran_out.set(ran_out.get() + 1));
        }
        Ok(found)
    }

    /// A part's `app` written as a resource or operation id (models copy
    /// what they see) is replaced by the application that publishes it.
    fn name_part_applications(&mut self, input: &mut Value) {
        let Some(parts) = input.get_mut(tools::PARTS).and_then(Value::as_array_mut) else {
            return;
        };
        let catalogue = self.task_catalogue();
        for part in parts {
            let Some(app) = part.get("app").and_then(Value::as_str).map(str::trim) else {
                continue;
            };
            // Opening an application is a step towards using it, not the
            // request itself: a part that opens one ("Open Writer", or the
            // open's own resource) is done by an operation in that
            // application, so an open alone cannot finish the task.
            let text = part
                .get("part")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_lowercase();
            let lowered = app.to_lowercase();
            let opened = catalogue.offers.iter().find_map(|o| {
                let opened = tools::opened_app(o)?;
                (text.contains(&o.descriptor.action_label.to_lowercase())
                    || lowered.contains(&opened.to_lowercase()))
                .then(|| opened.to_owned())
            });
            if let Some(opened) = opened {
                part["app"] = Value::String(opened);
                continue;
            }
            if catalogue.offers.iter().any(|o| o.app == app) {
                continue;
            }
            let publisher = catalogue
                .offers
                .iter()
                .find(|o| {
                    o.address().resource == app
                        || o.id() == app
                        || surface::encode_address(o.address()) == app
                })
                .map(|o| o.app.clone())
                // A listing header copied whole ("so.tilo.chat: # general …").
                .or_else(|| {
                    catalogue
                        .offers
                        .iter()
                        .map(|o| o.app.as_str())
                        .filter(|id| {
                            app.strip_prefix(id).is_some_and(|rest| {
                                rest.starts_with(|c: char| c == ':' || c.is_whitespace())
                            })
                        })
                        .max_by_key(|id| id.len())
                        .map(str::to_owned)
                });
            if let Some(publisher) = publisher {
                part["app"] = Value::String(publisher);
            }
        }
    }
}

/// The records of `app` an open's result names as the windows it opened:
/// the desktop's command receipt lists them under `opened`, each with its
/// `app_id` and `document_id` (`tilo_desktop_model::commands::OpenedWindow`).
/// Empty when the result names none — an owner that does not report its
/// windows, or an open that is not the desktop's.
fn opened_records(value: Option<&Value>, app: &str) -> Vec<String> {
    value
        .and_then(|value| value.get("opened"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|window| window.get("app_id").and_then(Value::as_str) == Some(app))
        .filter_map(|window| window.get("document_id").and_then(Value::as_str))
        .filter(|record| !record.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Which operations are published and available, by id.
fn availability(catalogue: &surface::Catalogue) -> HashMap<String, bool> {
    catalogue
        .offers
        .iter()
        .map(|offer| {
            (
                offer.id(),
                matches!(
                    offer.descriptor.contract.availability,
                    tilo_actions::Availability::Available
                ),
            )
        })
        .collect()
}

/// The operations of `after` that are available now and were not before —
/// newly published, or newly available — revealed, as `"<id> — <label>"`.
fn newly_available(
    before: &HashMap<String, bool>,
    after: &surface::Catalogue,
    revealed: &mut Revealed,
    most: usize,
) -> Vec<String> {
    let mut out = Vec::new();
    // Document operations first: what a request most often needs next.
    let mut ordered: Vec<&surface::Offer> = after.offers.iter().collect();
    ordered.sort_by_key(|offer| match offer.descriptor.kind {
        tilo_actions::OperationKind::Document => 0,
        tilo_actions::OperationKind::View => 1,
        tilo_actions::OperationKind::Effect => 2,
    });
    for offer in ordered {
        let id = offer.id();
        let available = matches!(
            offer.descriptor.contract.availability,
            tilo_actions::Availability::Available
        );
        if !available || before.get(&id).copied().unwrap_or(false) {
            continue;
        }
        if out.len() >= most {
            break;
        }
        revealed.reveal(id.clone());
        out.push(tools::operation_line(offer));
    }
    out
}

#[path = "agent_loop_tests.rs"]
#[cfg(test)]
mod tests;

#[cfg(test)]
mod today_tests {
    /// The model is told the day and the week it is in, so "this week" needs no step.
    #[test]
    fn the_request_names_today_and_its_week() {
        let friday = ui::format::NaiveDate::from_ymd_opt(2026, 11, 13).unwrap();
        assert_eq!(
            super::today_line(friday),
            format!(
                "Today is {} where the person is (2026-11-13); this week runs from Monday \
                 2026-11-09 to Sunday 2026-11-15.",
                ui::format::day_heading(friday, true)
            )
        );
    }

    /// A turn's catalogue always carries the session API's programs: the listing an agent's
    /// connection holds never does, and without it every `tilo` call was answered "this session
    /// runs no programs". One the broker listed is kept as it came.
    #[test]
    fn a_turn_runs_its_programs_through_the_session_api() {
        use tilo_actions::remote::task_programs as wire;
        let api = super::task_programs_offer(&[]).expect("the API is named");
        assert_eq!(api.id(), "broker:tilo.task-programs:task#run-program");
        assert_eq!(api.app, wire::APP);
        assert!(
            api.descriptor
                .params
                .iter()
                .any(|param| param.name == "program" && param.required)
        );
        assert!(super::task_programs_offer(&[api]).is_none());
    }

    /// The model is told the person's time and how a moment they name is written in a program:
    /// an instant in UTC, shifted by the person's offset ("at 6pm" in UTC+01:00 is 17:00Z).
    #[test]
    fn the_request_names_the_time_and_how_a_moment_is_written() {
        let day = ui::format::NaiveDate::from_ymd_opt(2026, 11, 16).unwrap();
        assert_eq!(
            super::clock_line(
                day.and_hms_opt(22, 45, 0).unwrap(),
                day.and_hms_opt(21, 45, 0).unwrap()
            ),
            "It is 22:45 there now (UTC+01:00). A `Time` in a program is an instant in UTC with \
             a `Z`: 6 pm there today is `2026-11-16T17:00:00Z`. Keep every time they name."
        );
    }
}
