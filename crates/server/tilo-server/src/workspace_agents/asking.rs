//! A turn's model call that fails for a moment is asked again, and a turn that cannot go on says
//! what it had done.
//!
//! A turn asks its model once per step and carries out the tools each answer calls. A call that
//! failed used to end the turn with the failure as its only words, whatever the turn had done by
//! then: a provider that refused one request threw the steps before it away for its person.
//!
//! [`again`] stands between a turn and its model. A failure the protocol answers with fresh
//! inference from what is committed ([`Recovery::RetryInference`]: the provider failed or was
//! unavailable, the connection broke) or with a wait ([`Recovery::WaitAndRetry`]: its rate limit)
//! is asked again: the same request under the same key, after a pause that grows to a steady
//! one ([`pause`]), for as long as it takes. No count of asks ends the call: it goes on until
//! the model answers, a failure comes that asking again cannot cure, or the turn's person
//! stops the turn. Each ask is given as much time as its caller gave the first. Only the model
//! call is repeated. The request holds what every tool of the turn answered so far, so no tool
//! is carried out a second time. A failure asking again cannot cure (the key, the account's
//! credit, the request, the context) is the call's answer at once, and so is a stop by the
//! turn's person.
//!
//! One kind of call is still counted: one the turn's own clock ended ([`TIMED_OUT_ASKS`]).
//! Its provider worked on every ask of it and bills what it thought, so asking it without a
//! count would spend its person's money while nothing arrives. That count goes with the clock.
//!
//! What a model writes while it answers is handed on from every ask. Before a further ask
//! whoever listens hears that the call broke off, so a reply that grows puts what comes then
//! below what came.
//!
//! [`left_off`] is what a turn says when its model stays unanswered: why, what it had done until
//! then, and that it gave no answer.

use std::collections::{BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tilo_lock::MutexRecover as _;

use super::{Content, Message, ModelError, ModelRequest, ModelResponse, PlanTurns, StreamEvent};
use crate::workspaces::person_model::protocol::{ErrorKind, Recovery};

/// The pause before the first further ask of a call that failed. Short, so a refusal that
/// passes at once costs its person a second. Every pause after it is twice the one before, up
/// to [`STEADY`]. Beneath each ask the model gateway itself asks again what a provider provably
/// did not take, and waits what the provider's own hint says.
const FIRST_PAUSE: Duration = Duration::from_secs(1);

/// The pause between two asks once it has grown: a provider that stays down is asked twice a
/// minute. It is a pause between asks and ends nothing.
const STEADY: Duration = Duration::from_secs(30);

/// How many asks in all a call gets that the turn's own clock ended ([`ErrorKind::TimedOut`]):
/// what it got before. Its provider worked on each of them, so it is not asked without a count
/// while that clock stands.
const TIMED_OUT_ASKS: u32 = 4;

/// How often a stop by the turn's person is looked for during a wait.
const STOP_LOOK: Duration = Duration::from_millis(50);

/// The most actions a turn that could not go on names.
const MOST_NAMED: usize = 6;

/// The agent tools that read and change nothing.
const LOOKS: [&str; 7] = [
    "tilo_ls",
    "tilo_read",
    "tilo_find",
    "tilo_view",
    "tilo_link",
    "tilo_watch",
    "tilo_status",
];

/// A turn's model, asked again where a call fails for a moment.
struct Again {
    turns: Box<dyn PlanTurns>,
    /// Set when the turn's person stopped it: nothing is asked after that.
    stop: Arc<AtomicBool>,
    /// The pause before a call's further ask of that number, counted from one.
    pause: fn(u32) -> Duration,
}

/// `turns`, each call of which is asked again as this module says while `stop` is not set.
pub(super) fn again(turns: Box<dyn PlanTurns>, stop: Arc<AtomicBool>) -> Box<dyn PlanTurns> {
    Box::new(Again { turns, stop, pause })
}

/// The pause before a call's `nth` further ask, counted from one: [`FIRST_PAUSE`], then twice
/// the one before, up to [`STEADY`].
fn pause(nth: u32) -> Duration {
    let times = 1_u32.checked_shl(nth.saturating_sub(1)).unwrap_or(u32::MAX);
    FIRST_PAUSE.saturating_mul(times).min(STEADY)
}

/// What a reply's work says while its turn's model call is asked again: which ask of the call
/// this is, and why the one before it failed, in the provider's last word.
pub(super) fn asking_again(asked: u32, why: &str) -> String {
    format!("asking my model again (ask {asked}): {why}")
}

/// What a turn's reply is to say of its model's call while the call is under way. The thread
/// the model is asked on puts each sentence here, and whoever follows the answer takes them
/// one by one, so nothing waits in silence.
#[derive(Default)]
pub(super) struct Said {
    sentences: Mutex<VecDeque<String>>,
    more: tokio::sync::Notify,
}

impl Said {
    /// The reply is to say `sentence`.
    pub(super) fn say(&self, sentence: String) {
        self.sentences.lock_recover().push_back(sentence);
        self.more.notify_one();
    }

    /// The next sentence, once there is one.
    pub(super) async fn next(&self) -> String {
        loop {
            let next = self.sentences.lock_recover().pop_front();
            if let Some(sentence) = next {
                return sentence;
            }
            self.more.notified().await;
        }
    }
}

impl Again {
    /// Pause for `wait` before a further ask. False as soon as the turn's person stops the turn.
    fn waited(&self, wait: Duration) -> bool {
        let Some(until) = Instant::now().checked_add(wait) else {
            return false;
        };
        loop {
            if self.stop.load(Ordering::Relaxed) {
                return false;
            }
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return true;
            }
            std::thread::sleep(left.min(STOP_LOOK));
        }
    }
}

impl PlanTurns for Again {
    fn turn(
        &mut self,
        request: &ModelRequest,
        key: &str,
        deadline: Instant,
        retry: bool,
    ) -> Result<ModelResponse, ModelError> {
        self.turn_streaming(request, key, deadline, retry, &mut |_| {})
    }

    fn turn_streaming(
        &mut self,
        request: &ModelRequest,
        key: &str,
        deadline: Instant,
        retry: bool,
        written: &mut (dyn FnMut(StreamEvent) + Send),
    ) -> Result<ModelResponse, ModelError> {
        let began = Instant::now();
        // The time its caller gives one ask: every further ask is given as much.
        let given = deadline.saturating_duration_since(began);
        let mut error = match self
            .turns
            .turn_streaming(request, key, deadline, retry, written)
        {
            Err(error) if passing(&error) => error,
            answer => return answer,
        };
        let mut asked = 1_u32;
        let mut timed_out = u32::from(error.kind == ErrorKind::TimedOut);
        while timed_out < TIMED_OUT_ASKS && self.waited((self.pause)(asked)) {
            let Some(due) = Instant::now().checked_add(given) else {
                break;
            };
            asked = asked.saturating_add(1);
            tracing::info!(kind = ?error.kind, asked, "a turn's model call failed; it is asked again");
            // What the call that broke off wrote stays where it was read: what the next
            // ask writes stands below it.
            written(StreamEvent::Failed {
                error: error.clone(),
            });
            error = match self.turns.turn_streaming(request, key, due, true, written) {
                Err(error) if passing(&error) => error,
                answer => return answer,
            };
            if error.kind == ErrorKind::TimedOut {
                timed_out = timed_out.saturating_add(1);
            }
        }
        if asked > 1 {
            error.message = format!(
                "{} (asked {asked} times over {} s)",
                error.message,
                began.elapsed().as_secs()
            );
        }
        Err(error)
    }

    fn model(&self) -> &str {
        self.turns.model()
    }
}

/// Whether asking again may cure `error`, as the protocol says what follows each kind.
fn passing(error: &ModelError) -> bool {
    matches!(
        error.recovery(),
        Recovery::RetryInference | Recovery::WaitAndRetry
    )
}

/// What a turn says that could not go on for `why`: why, then what it had done until then, read
/// from its own `messages` (the tools its model called and how each ended), and that it gave no
/// answer. `why` alone for a turn that had called no tool.
pub(super) fn left_off(why: &str, messages: &[Message]) -> String {
    let said = || messages.iter().flat_map(|message| &message.content);
    let failed: BTreeSet<&str> = said()
        .filter_map(|content| match content {
            Content::ToolResult(result) if result.is_error => Some(result.call_id.as_str()),
            _ => None,
        })
        .collect();
    let mut looked = 0_usize;
    let mut acted = Vec::new();
    for content in said() {
        let Content::ToolCall(call) = content else {
            continue;
        };
        if LOOKS.contains(&call.name.as_str()) {
            looked = looked.saturating_add(1);
            continue;
        }
        let mut what = named(&call.name, &call.input);
        if failed.contains(call.id.as_str()) {
            what.push_str(" (failed)");
        }
        acted.push(what);
    }
    let mut did = Vec::new();
    match looked {
        0 => {}
        1 => did.push("looked something up once".to_owned()),
        times => did.push(format!("looked things up {times} times")),
    }
    let earlier = acted.len().saturating_sub(MOST_NAMED);
    let last = acted.split_off(earlier).join("; ");
    match (earlier, last.is_empty()) {
        (_, true) => {}
        (0, false) => did.push(format!("carried out: {last}")),
        (1, false) => did.push(format!("carried out one action and after it: {last}")),
        (earlier, false) => did.push(format!(
            "carried out {earlier} actions and after them: {last}"
        )),
    }
    if did.is_empty() {
        return why.to_owned();
    }
    let kept = if last.is_empty() {
        ""
    } else {
        " What that changed stays as it is."
    };
    format!(
        "{why}. Until then I had {}.{kept} I had not finished and gave no answer: ask me to go on.",
        did.join(", and ")
    )
}

/// What a call of tool `name` with `input` did, as its person reads it: the action a `tilo_do`
/// names (each of several, in order), the tool's own name for any other.
fn named(name: &str, input: &serde_json::Value) -> String {
    let path = |call: &serde_json::Value| call.get("path")?.as_str().map(str::to_owned);
    let paths: Vec<String> = match input.get("calls").and_then(serde_json::Value::as_array) {
        Some(calls) => calls.iter().filter_map(path).collect(),
        None => path(input).into_iter().collect(),
    };
    if paths.is_empty() {
        name.to_owned()
    } else {
        paths.join(", then ")
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use tilo_lock::MutexRecover as _;

    use super::*;
    use crate::workspaces::person_model::protocol::{
        ErrorKind, Role, StopReason, ToolCall, ToolChoice, ToolResult, Usage,
    };

    /// A model whose calls are written out; it keeps whether each ask said it was a further one.
    struct Written {
        answers: VecDeque<Result<ModelResponse, ModelError>>,
        asked: Arc<Mutex<Vec<bool>>>,
    }

    impl PlanTurns for Written {
        fn turn(
            &mut self,
            _: &ModelRequest,
            _: &str,
            _: Instant,
            retry: bool,
        ) -> Result<ModelResponse, ModelError> {
            self.asked.lock_recover().push(retry);
            self.answers
                .pop_front()
                .unwrap_or_else(|| Err(ModelError::invalid("nothing further is written")))
        }

        fn model(&self) -> &'static str {
            "written"
        }
    }

    /// A model that writes a word on every ask, `first` or `again`, before the ask ends as
    /// written out.
    struct Writing {
        answers: VecDeque<Result<ModelResponse, ModelError>>,
    }

    impl PlanTurns for Writing {
        fn turn(
            &mut self,
            request: &ModelRequest,
            key: &str,
            deadline: Instant,
            retry: bool,
        ) -> Result<ModelResponse, ModelError> {
            self.turn_streaming(request, key, deadline, retry, &mut |_| {})
        }

        fn turn_streaming(
            &mut self,
            _: &ModelRequest,
            _: &str,
            _: Instant,
            retry: bool,
            written: &mut (dyn FnMut(StreamEvent) + Send),
        ) -> Result<ModelResponse, ModelError> {
            written(StreamEvent::TextDelta {
                text: if retry { "again" } else { "first" }.to_owned(),
            });
            self.answers
                .pop_front()
                .unwrap_or_else(|| Err(ModelError::invalid("nothing further is written")))
        }

        fn model(&self) -> &'static str {
            "writing"
        }
    }

    fn words(text: &str) -> ModelResponse {
        ModelResponse {
            message: Message::text(Role::Assistant, text.to_owned()),
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            cost_micros: 0,
        }
    }

    /// `answers` asked once through [`Again`] with no time between its asks, the call's time
    /// ending `time` from now: its answer, and whether each ask said it was a further one.
    fn asked_of(
        answers: Vec<Result<ModelResponse, ModelError>>,
        stopped: bool,
        time: Duration,
    ) -> (Result<ModelResponse, ModelError>, Vec<bool>) {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let mut again = Again {
            turns: Box::new(Written {
                answers: answers.into(),
                asked: Arc::clone(&asked),
            }),
            stop: Arc::new(AtomicBool::new(stopped)),
            pause: |_| Duration::ZERO,
        };
        let request = ModelRequest {
            messages: Vec::new(),
            tools: Vec::new(),
            tool_choice: ToolChoice::Auto,
            max_output_tokens: 16,
        };
        let deadline = Instant::now().checked_add(time).unwrap();
        let answer = again.turn(&request, "key", deadline, false);
        let asked = asked.lock_recover().clone();
        (answer, asked)
    }

    const MINUTE: Duration = Duration::from_secs(60);

    #[test]
    fn a_call_that_fails_for_a_moment_is_asked_again_and_its_answer_is_the_calls() {
        for kind in [
            ErrorKind::Provider,
            ErrorKind::Transport,
            ErrorKind::TimedOut,
            ErrorKind::Unavailable,
            ErrorKind::RateLimited,
        ] {
            let (answer, asked) = asked_of(
                vec![Err(ModelError::new(kind)), Ok(words("done"))],
                false,
                MINUTE,
            );
            assert_eq!(answer, Ok(words("done")), "{kind:?}");
            assert_eq!(asked, [false, true], "{kind:?}: once, and once more");
        }
    }

    #[test]
    fn what_the_model_writes_is_handed_on_and_a_further_ask_is_said_before_it() {
        let mut again = Again {
            turns: Box::new(Writing {
                answers: vec![Err(ModelError::new(ErrorKind::Provider)), Ok(words("done"))].into(),
            }),
            stop: Arc::new(AtomicBool::new(false)),
            pause: |_| Duration::ZERO,
        };
        let request = ModelRequest {
            messages: Vec::new(),
            tools: Vec::new(),
            tool_choice: ToolChoice::Auto,
            max_output_tokens: 16,
        };
        let deadline = Instant::now().checked_add(MINUTE).unwrap();
        let mut heard = Vec::new();
        let answer = again.turn_streaming(&request, "key", deadline, false, &mut |event| {
            heard.push(match event {
                StreamEvent::TextDelta { text } => text,
                StreamEvent::Failed { .. } => "the call broke off".to_owned(),
                _ => "something else".to_owned(),
            });
        });
        assert_eq!(answer, Ok(words("done")));
        assert_eq!(heard, ["first", "the call broke off", "again"]);
    }

    #[test]
    fn what_asking_again_cannot_cure_is_asked_once() {
        for kind in [
            ErrorKind::Authentication,
            ErrorKind::PermissionDenied,
            ErrorKind::CreditsExhausted,
            ErrorKind::NoProvider,
            ErrorKind::InvalidRequest,
            ErrorKind::ContextLimit,
            ErrorKind::InvalidResponse,
            ErrorKind::Unsupported,
            ErrorKind::Conflict,
            ErrorKind::Cancelled,
            ErrorKind::Refused,
            ErrorKind::Truncated,
        ] {
            let (answer, asked) = asked_of(
                vec![Err(ModelError::new(kind)), Ok(words("done"))],
                false,
                MINUTE,
            );
            assert_eq!(answer, Err(ModelError::new(kind)), "{kind:?}");
            assert_eq!(asked, [false], "{kind:?} is the call's answer at once");
        }
    }

    #[test]
    fn a_provider_that_stays_down_is_asked_for_as_long_as_it_takes() {
        let mut answers: Vec<_> = (0..30)
            .map(|_| Err(ModelError::new(ErrorKind::Provider)))
            .collect();
        answers.push(Ok(words("back again")));
        let (answer, asked) = asked_of(answers, false, MINUTE);
        assert_eq!(
            answer,
            Ok(words("back again")),
            "no count of asks ends the call"
        );
        assert_eq!(asked.len(), 31);
    }

    #[test]
    fn a_call_the_turns_own_clock_ended_is_asked_four_times_and_the_failure_says_so() {
        let late = || Err(ModelError::new(ErrorKind::TimedOut));
        let (answer, asked) = asked_of(
            vec![late(), late(), late(), late(), Ok(words("too late"))],
            false,
            MINUTE,
        );
        let error = answer.unwrap_err();
        assert_eq!(error.kind, ErrorKind::TimedOut);
        assert_eq!(
            error.message,
            "Model request timed out (asked 4 times over 0 s)"
        );
        assert_eq!(asked, [false, true, true, true]);
    }

    #[test]
    fn the_pause_between_asks_grows_to_a_steady_one() {
        let pauses: Vec<u64> = (1..=8).map(|nth| pause(nth).as_secs()).collect();
        assert_eq!(pauses, [1, 2, 4, 8, 16, 30, 30, 30]);
        assert_eq!(pause(u32::MAX), STEADY);
    }

    /// A model that keeps how much time each ask was given, and takes `takes` over each ask
    /// that fails.
    struct Timed {
        given: Arc<Mutex<Vec<Duration>>>,
        takes: Duration,
        fails: u32,
    }

    impl PlanTurns for Timed {
        fn turn(
            &mut self,
            _: &ModelRequest,
            _: &str,
            deadline: Instant,
            _: bool,
        ) -> Result<ModelResponse, ModelError> {
            self.given
                .lock_recover()
                .push(deadline.saturating_duration_since(Instant::now()));
            if self.fails == 0 {
                return Ok(words("done"));
            }
            self.fails = self.fails.saturating_sub(1);
            std::thread::sleep(self.takes);
            Err(ModelError::new(ErrorKind::Provider))
        }

        fn model(&self) -> &'static str {
            "timed"
        }
    }

    #[test]
    fn every_further_ask_is_given_as_much_time_as_the_first() {
        let given = Arc::new(Mutex::new(Vec::new()));
        let mut again = Again {
            turns: Box::new(Timed {
                given: Arc::clone(&given),
                takes: Duration::from_millis(300),
                fails: 2,
            }),
            stop: Arc::new(AtomicBool::new(false)),
            pause: |_| Duration::ZERO,
        };
        let request = ModelRequest {
            messages: Vec::new(),
            tools: Vec::new(),
            tool_choice: ToolChoice::Auto,
            max_output_tokens: 16,
        };
        // Each failing ask takes longer than the whole time the caller gave one ask.
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(200))
            .unwrap();
        let answer = again.turn(&request, "key", deadline, false);
        assert_eq!(answer, Ok(words("done")));
        let given = given.lock_recover().clone();
        assert_eq!(given.len(), 3);
        for (nth, given) in given.iter().enumerate() {
            assert!(
                *given > Duration::from_millis(100),
                "ask {nth} was given {given:?} of the 200 ms an ask is given"
            );
        }
    }

    #[test]
    fn a_failure_that_follows_a_passing_one_is_the_calls_answer() {
        let (answer, asked) = asked_of(
            vec![
                Err(ModelError::new(ErrorKind::Provider)),
                Err(ModelError::new(ErrorKind::CreditsExhausted)),
                Ok(words("never")),
            ],
            false,
            MINUTE,
        );
        assert_eq!(answer, Err(ModelError::new(ErrorKind::CreditsExhausted)));
        assert_eq!(asked, [false, true]);
    }

    #[test]
    fn nothing_is_asked_again_after_a_stop() {
        let script = || vec![Err(ModelError::new(ErrorKind::Provider)), Ok(words("done"))];
        let (answer, asked) = asked_of(script(), true, MINUTE);
        assert_eq!(answer, Err(ModelError::new(ErrorKind::Provider)));
        assert_eq!(asked, [false], "its person stopped the turn");
    }

    fn called(id: &str, name: &str, input: serde_json::Value) -> Message {
        Message {
            role: Role::Assistant,
            content: vec![Content::ToolCall(ToolCall {
                id: id.to_owned(),
                name: name.to_owned(),
                input,
                extra_content: None,
            })],
        }
    }

    fn answered(id: &str, is_error: bool) -> Message {
        Message {
            role: Role::Tool,
            content: vec![Content::ToolResult(ToolResult {
                call_id: id.to_owned(),
                content: "ok".to_owned(),
                is_error,
            })],
        }
    }

    #[test]
    fn a_turn_that_could_not_go_on_says_what_it_had_done() {
        let why = "My model did not answer: Provider request failed (asked 4 times over 7 s)";
        let mut messages = vec![
            Message::text(Role::System, "be brief".to_owned()),
            Message::text(Role::User, "tidy my notes".to_owned()),
        ];
        assert_eq!(left_off(why, &messages), why, "it had done nothing");

        messages.push(called("1", "tilo_ls", serde_json::json!({})));
        messages.push(answered("1", false));
        assert_eq!(
            left_off(why, &messages),
            format!(
                "{why}. Until then I had looked something up once. I had not finished and gave \
                 no answer: ask me to go on."
            )
        );

        messages.push(called(
            "2",
            "tilo_read",
            serde_json::json!({"path": "notes/Todo"}),
        ));
        messages.push(answered("2", false));
        messages.push(called(
            "3",
            "tilo_do",
            serde_json::json!({"path": "notes/Todo#rename", "args": {"title": "Done"}}),
        ));
        messages.push(answered("3", false));
        messages.push(called(
            "4",
            "tilo_do",
            serde_json::json!({"calls": [{"path": "notes#create"}, {"path": "notes/New#pin"}]}),
        ));
        messages.push(answered("4", true));
        assert_eq!(
            left_off(why, &messages),
            format!(
                "{why}. Until then I had looked things up 2 times, and carried out: \
                 notes/Todo#rename; notes#create, then notes/New#pin (failed). What that changed \
                 stays as it is. I had not finished and gave no answer: ask me to go on."
            )
        );

        for at in 5..=9 {
            let id = at.to_string();
            messages.push(called(&id, "answer_with_view", serde_json::json!({})));
            messages.push(answered(&id, false));
        }
        assert_eq!(
            left_off(why, &messages),
            format!(
                "{why}. Until then I had looked things up 2 times, and carried out one action \
                 and after it: notes#create, then notes/New#pin (failed); answer_with_view; \
                 answer_with_view; answer_with_view; answer_with_view; answer_with_view. What \
                 that changed stays as it is. I had not finished and gave no answer: ask me to \
                 go on."
            )
        );
    }
}
