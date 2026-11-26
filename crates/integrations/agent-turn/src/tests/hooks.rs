//! What a caller learns of a turn while it works: a turn with no limit of rounds goes on while its
//! model calls tools, what the model writes reaches the caller as it comes, each tool step knows
//! where it stands among its answer's calls, a model that gives no answer ends the turn in the
//! caller's words, and the log holds no value a call wrote nor anything a tool answered.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use agent_provider_config::protocol::{
    Content, Message, ModelError, ModelRequest, ModelResponse, Role, StopReason, StreamEvent,
    ToolCall, ToolChoice, ToolDefinition, Usage,
};
use serde_json::{Value, json};
use tilo_lock::MutexRecover as _;

use crate::{Answer, Asked, Doing, Hearing, Model, Outcome, Step, Tools, work};

/// A value no log line may hold: what a call wrote.
const WROTE: &str = "the-harbour-key-is-under-the-third-stone";
/// A value no log line may hold: what a tool answered.
const SHOWN: &str = "Ada's figures for the quarter";

/// Tools in the test itself: `tilo_ls` and `tilo_do` answer, and a call of `tilo_do` whose
/// path is [`WROTE`] is refused with a sentence that quotes it.
struct Stub {
    called: Mutex<Vec<String>>,
}

impl Stub {
    fn new() -> Self {
        Self {
            called: Mutex::default(),
        }
    }
}

impl Tools for Stub {
    async fn list(&self) -> Result<Vec<ToolDefinition>, String> {
        Ok(["tilo_ls", "tilo_do"]
            .into_iter()
            .map(|name| ToolDefinition {
                name: name.to_owned(),
                description: String::new(),
                input_schema: json!({"type": "object"}),
            })
            .collect())
    }

    async fn call(&self, name: &str, arguments: Value) -> Answer {
        self.called.lock_recover().push(name.to_owned());
        match arguments.get("path").and_then(Value::as_str) {
            Some(path) if path == WROTE => {
                Answer::failed(format!("{path} has no action open; close matches: read"))
            }
            _ => Answer {
                text: SHOWN.to_owned(),
                ..Answer::default()
            },
        }
    }
}

/// One answer of a scripted model: words, a list of calls, or a failure.
enum Said {
    Words(&'static str),
    Calls(Vec<(&'static str, Value)>),
    /// It writes these pieces as it answers, then answers with the words they make.
    Writing(Vec<StreamEvent>, &'static str),
    Fails(&'static str),
}

/// Answers each ask with the next of what it was given, keeping every request.
struct Scripted {
    said: VecDeque<Said>,
    asked: Arc<Mutex<Vec<ModelRequest>>>,
}

impl Scripted {
    fn new(said: Vec<Said>) -> (Arc<Mutex<Self>>, Arc<Mutex<Vec<ModelRequest>>>) {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let model = Self {
            said: said.into(),
            asked: Arc::clone(&asked),
        };
        (Arc::new(Mutex::new(model)), asked)
    }
}

fn answer(content: Vec<Content>, stop_reason: StopReason) -> ModelResponse {
    ModelResponse {
        message: Message {
            role: Role::Assistant,
            content,
        },
        stop_reason,
        usage: Usage::default(),
        cost_micros: 1,
    }
}

impl Model for Scripted {
    fn turn(
        &mut self,
        request: &ModelRequest,
        key: &str,
        deadline: Instant,
    ) -> Result<ModelResponse, String> {
        self.turn_streaming(request, key, deadline, &mut |_| {})
    }

    fn turn_streaming(
        &mut self,
        request: &ModelRequest,
        _: &str,
        _: Instant,
        heard: &mut (dyn FnMut(StreamEvent) + Send),
    ) -> Result<ModelResponse, String> {
        self.asked.lock_recover().push(request.clone());
        let at = self.asked.lock_recover().len();
        match self.said.pop_front().ok_or("script exhausted")? {
            Said::Words(words) => Ok(answer(
                vec![Content::Text {
                    text: words.to_owned(),
                }],
                StopReason::EndTurn,
            )),
            Said::Calls(calls) => Ok(answer(
                calls
                    .into_iter()
                    .enumerate()
                    .map(|(nth, (name, input))| {
                        Content::ToolCall(ToolCall {
                            id: format!("{at}-{nth}"),
                            name: name.to_owned(),
                            input,
                            extra_content: None,
                        })
                    })
                    .collect(),
                StopReason::ToolCalls,
            )),
            Said::Writing(pieces, words) => {
                for piece in pieces {
                    heard(piece);
                }
                Ok(answer(
                    vec![Content::Text {
                        text: words.to_owned(),
                    }],
                    StopReason::EndTurn,
                ))
            }
            Said::Fails(why) => Err(why.to_owned()),
        }
    }
}

/// What the test's caller was told, in order.
#[derive(Default)]
struct Kept {
    stop_after: Mutex<Option<String>>,
    stopped: AtomicBool,
    told: Mutex<Vec<String>>,
}

impl Kept {
    fn tell(&self, what: String) {
        self.told.lock_recover().push(what);
    }

    fn told(&self) -> Vec<String> {
        self.told.lock_recover().clone()
    }
}

fn place(step: Step) -> String {
    format!(
        "{}/{}{}",
        step.nth,
        step.of,
        if step.last() { " last" } else { "" }
    )
}

impl Doing<Stub> for Kept {
    fn stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    async fn calling(&self, call: &ToolCall, step: Step) {
        self.tell(format!("calling {} {}", call.name, place(step)));
    }

    async fn carry_out<W>(&self, call: &ToolCall, step: Step, work: W) -> Answer
    where
        W: Future<Output = Answer> + Send,
    {
        self.tell(format!("begins {} {}", call.name, place(step)));
        let answer = work.await;
        self.tell(format!("ends {} {}", call.name, place(step)));
        answer
    }

    fn called(&self, call: &ToolCall, step: Step, _: &Answer) {
        self.tell(format!("called {} {}", call.name, place(step)));
        if self.stop_after.lock_recover().as_deref() == Some(call.id.as_str()) {
            self.stopped.store(true, Ordering::SeqCst);
        }
    }

    async fn hear(&self, mut hearing: Hearing) {
        while let Some(pieces) = hearing.next().await {
            for piece in pieces {
                self.tell(match piece {
                    StreamEvent::TextDelta { text } => format!("wrote {text}"),
                    StreamEvent::ReasoningDelta { text } => format!("thought {text}"),
                    StreamEvent::Failed { .. } => "broke off".to_owned(),
                    _ => "something else".to_owned(),
                });
            }
        }
    }

    async fn heard(&self, response: &ModelResponse) {
        self.tell(format!("answered {}", crate::words_of(response)));
    }

    fn left_off(&self, why: String, messages: &[Message]) -> String {
        let calls = messages
            .iter()
            .flat_map(|message| &message.content)
            .filter(|content| matches!(content, Content::ToolCall(_)))
            .count();
        format!("{why}, after {calls} calls")
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// A turn with no limit of rounds asks its model for as long as it calls tools, well past the
/// bound other turns keep, and ends with its answer: nothing counts its asks to end it, and it
/// is never asked to stop calling tools.
#[test]
fn a_turn_without_a_limit_of_rounds_goes_on_while_its_model_calls_tools() {
    const ROUNDS: usize = 60;
    let mut said: Vec<Said> = (0..ROUNDS)
        .map(|_| Said::Calls(vec![("tilo_ls", json!({}))]))
        .collect();
    said.push(Said::Words("I looked sixty times."));
    let (model, asked) = Scripted::new(said);
    let tools = Stub::new();
    let turn = Asked {
        most_rounds: None,
        ..Asked::new("s", "r")
    };
    assert!(
        Asked::new("s", "r").most_rounds.is_some(),
        "other turns keep a bound"
    );
    let (outcome, cost) = runtime().block_on(work(&tools, model, &turn, &Kept::default()));
    assert_eq!(outcome, Outcome::Answered("I looked sixty times.".into()));
    assert_eq!(cost, u64::try_from(ROUNDS).unwrap() + 1);
    assert_eq!(tools.called.lock_recover().len(), ROUNDS);
    let asked = asked.lock_recover().clone();
    assert_eq!(asked.len(), ROUNDS + 1);
    assert!(
        asked
            .iter()
            .all(|request| request.tool_choice == ToolChoice::Auto),
        "no ask takes its tools away"
    );
}

/// What the model writes while it answers reaches the caller as it comes, in order, a break
/// where a call was asked again among it, and the answer after all of it.
#[test]
fn what_the_model_writes_reaches_the_caller_while_it_answers() {
    let (model, _) = Scripted::new(vec![Said::Writing(
        vec![
            StreamEvent::ReasoningDelta {
                text: "The list".into(),
            },
            StreamEvent::TextDelta {
                text: "Here".into(),
            },
            StreamEvent::Failed {
                error: ModelError::invalid("the call broke off"),
            },
            StreamEvent::TextDelta {
                text: "Here it is.".into(),
            },
        ],
        "Here it is.",
    )]);
    let kept = Kept::default();
    let (outcome, _) = runtime().block_on(work(&Stub::new(), model, &Asked::new("s", "r"), &kept));
    assert_eq!(outcome, Outcome::Answered("Here it is.".into()));
    assert_eq!(
        kept.told(),
        [
            "thought The list",
            "wrote Here",
            "broke off",
            "wrote Here it is.",
            "answered Here it is.",
        ]
    );
}

/// Each tool step knows which call of its answer it is and how many the answer holds: the
/// caller is told before it, carries it out itself, and is told after it, and the last step of
/// an answer says that its calls are done. A stop between steps leaves the rest untold.
#[test]
fn each_step_knows_where_it_stands_among_its_answers_calls() {
    let (model, _) = Scripted::new(vec![
        Said::Calls(vec![
            ("tilo_ls", json!({})),
            ("tilo_ls", json!({"path": "notes"})),
            ("tilo_do", json!({"path": "notes#create"})),
        ]),
        Said::Calls(vec![("tilo_do", json!({})), ("tilo_ls", json!({}))]),
    ]);
    let kept = Kept::default();
    // Its person stops it after the first call of the second answer.
    *kept.stop_after.lock_recover() = Some("2-0".into());
    let (outcome, _) = runtime().block_on(work(&Stub::new(), model, &Asked::new("s", "r"), &kept));
    assert_eq!(outcome, Outcome::Cancelled);
    let steps: Vec<String> = kept
        .told()
        .into_iter()
        .filter(|told| !told.starts_with("answered"))
        .collect();
    assert_eq!(
        steps,
        [
            "calling tilo_ls 0/3",
            "begins tilo_ls 0/3",
            "ends tilo_ls 0/3",
            "called tilo_ls 0/3",
            "calling tilo_ls 1/3",
            "begins tilo_ls 1/3",
            "ends tilo_ls 1/3",
            "called tilo_ls 1/3",
            "calling tilo_do 2/3 last",
            "begins tilo_do 2/3 last",
            "ends tilo_do 2/3 last",
            "called tilo_do 2/3 last",
            "calling tilo_do 0/2",
            "begins tilo_do 0/2",
            "ends tilo_do 0/2",
            "called tilo_do 0/2",
        ]
    );
}

/// A model that gives no answer ends the turn in the caller's words, which can say what the
/// turn had done by then from the conversation so far.
#[test]
fn a_model_that_gives_no_answer_ends_the_turn_in_the_callers_words() {
    let (model, _) = Scripted::new(vec![
        Said::Calls(vec![("tilo_ls", json!({})), ("tilo_ls", json!({}))]),
        Said::Fails("My model did not answer"),
    ]);
    let (outcome, _) = runtime().block_on(work(
        &Stub::new(),
        model,
        &Asked::new("s", "r"),
        &Kept::default(),
    ));
    assert_eq!(
        outcome,
        Outcome::Failed("My model did not answer, after 2 calls".into())
    );
}

/// Every log line the test's thread writes, at every level.
#[derive(Clone, Default)]
struct Lines(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Lines {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock_recover().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Lines {
    type Writer = Self;

    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}

/// A call's line names its tool and each argument by its name and the bytes of its value; a
/// refusal keeps its sentence without what the call wrote, and an answer its size. No value a
/// call wrote and nothing a tool answered stands in the log, at any level.
#[test]
fn the_log_names_a_calls_tool_and_arguments_and_holds_no_value() {
    let lines = Lines::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(lines.clone())
        .finish();
    let refused = json!({"path": WROTE, "args": {"title": WROTE}});
    let (model, _) = Scripted::new(vec![
        Said::Calls(vec![
            ("tilo_do", refused),
            ("tilo_read", json!({"path": format!("notes/{WROTE}")})),
            ("tilo_ls", json!({"path": "notes"})),
        ]),
        Said::Words("Done."),
    ]);
    let (outcome, _) = tracing::subscriber::with_default(subscriber, || {
        runtime().block_on(work(
            &Stub::new(),
            model,
            &Asked::new("s", "r"),
            &Kept::default(),
        ))
    });
    assert_eq!(outcome, Outcome::Answered("Done.".into()));
    let logged = String::from_utf8_lossy(&lines.0.lock_recover()).into_owned();
    assert!(
        !logged.contains(WROTE) && !logged.contains("harbour") && !logged.contains(SHOWN),
        "no value a call wrote and nothing a tool answered is logged:\n{logged}"
    );
    let line_of = |tool: &str| {
        logged
            .lines()
            .find(|line| line.contains(&format!(" tool={tool} ")))
            .unwrap_or_else(|| panic!("a line for {tool}:\n{logged}"))
            .to_owned()
    };
    let refusal = line_of("tilo_do");
    assert!(refusal.contains(" INFO "), "a refusal at info: {refusal}");
    assert!(
        refusal.contains("said=… has no action open") && refusal.contains("path:42"),
        "{refusal}"
    );
    assert!(refusal.contains("args:52"), "{refusal}");
    let used = line_of("tilo_ls");
    assert!(used.contains("DEBUG "), "an answer at debug: {used}");
    assert!(
        used.contains("arguments=path:7")
            && used.contains(&format!("answered_bytes={}", SHOWN.len())),
        "{used}"
    );
}
