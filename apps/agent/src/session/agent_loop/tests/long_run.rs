//! Long runs through the real loop with a scripted model that reads what it
//! is sent: a run of many items over many invokes stays within its model's
//! context by compacting, re-reads a result it put aside, tells its progress
//! per item, survives a step that asks its person first and a restart while
//! one waited, and a model that cannot call tools is refused by name.

use super::super::compaction::Context;
use super::*;
use agent_provider_config::protocol::{
    Content, Message, ModelResponse, Role, StopReason, ToolCall, Usage,
};

/// What the scripted model calls on turn `n`, given the request it is sent.
type Decide = dyn Fn(usize, &Value) -> Vec<(&'static str, Value)> + Send + Sync;

/// A model that decides each turn from the request it is sent, like a real
/// one, and records every request.
struct Reading {
    decide: Box<Decide>,
    bodies: Mutex<Vec<Value>>,
}

impl TurnTransport for Reading {
    fn start(&self, request: TurnRequest) -> Receiver<TurnEvent> {
        let turn = {
            let mut bodies = self.bodies.lock().unwrap();
            bodies.push(request.body.clone());
            bodies.len() - 1
        };
        let calls = (self.decide)(turn, &request.body);
        let response = ModelResponse {
            message: Message {
                role: Role::Assistant,
                content: calls
                    .into_iter()
                    .enumerate()
                    .map(|(index, (name, input))| {
                        Content::ToolCall(ToolCall {
                            // Unique across the run, as a provider's are.
                            id: format!("t{turn}c{index}"),
                            name: name.into(),
                            input,
                            extra_content: None,
                        })
                    })
                    .collect(),
            },
            stop_reason: StopReason::ToolCalls,
            usage: Usage {
                input_tokens: 10,
                output_tokens: 10,
                total_tokens: 20,
                ..Usage::default()
            },
        };
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(TurnEvent::Finished(Ok(
            serde_json::to_string(&response).unwrap()
        )))
        .unwrap();
        rx
    }
}

/// The context the scripted model declares, in tokens.
const CONTEXT: u32 = 32_000;
/// Items of the request, and turns spent on each.
const ITEMS: usize = 8;
const TURNS_PER_ITEM: usize = 18;
/// What every paragraph of the document starts with, to find it again.
const MARKER: &str = "Candidate letter";

/// A mailbox of letters, each about a thousand tokens to read.
fn letters() -> World {
    World {
        letters: 200,
        ..World::default()
    }
}

/// A budget for a long chat task.
fn long_budget(turns: u32) -> Option<collab_types::task_budget::TaskExecutionBudget> {
    Some(collab_types::task_budget::TaskExecutionBudget {
        turn_ms: 20_000,
        total_ms: 600_000,
        max_turns: Some(turns),
    })
}

/// A fixture whose model is `decide`, declaring `model`.
fn reading_fixture(
    title: &str,
    world: World,
    budget: Option<collab_types::task_budget::TaskExecutionBudget>,
    model: agent_provider_config::ModelDescriptor,
    decide: Box<Decide>,
) -> (Fixture, Arc<Reading>) {
    let mut f = execution_fixture(title, world, Vec::new(), None, budget);
    let reading = Arc::new(Reading {
        decide,
        bodies: Mutex::new(Vec::new()),
    });
    f.session.chooser = Chooser::scripted_model(
        Duration::from_secs(5),
        model,
        Arc::clone(&reading) as Arc<dyn TurnTransport>,
    );
    (f, reading)
}

fn model_with_context() -> agent_provider_config::ModelDescriptor {
    agent_provider_config::ModelDescriptor {
        context_window: Some(CONTEXT),
        ..agent_provider_config::ModelDescriptor::new("scripted-long")
    }
}

/// Every string in `value` that is itself a JSON object: the tool results
/// a request carries.
fn embedded_objects(value: &Value, out: &mut Vec<serde_json::Map<String, Value>>) {
    match value {
        Value::String(text) => {
            if let Ok(Value::Object(object)) = serde_json::from_str::<Value>(text) {
                out.push(object);
            }
        }
        Value::Array(items) => items.iter().for_each(|item| embedded_objects(item, out)),
        Value::Object(object) => object.values().for_each(|item| embedded_objects(item, out)),
        _ => {}
    }
}

/// The first result handle a folded note in the request lists.
fn folded_handle(body: &Value) -> Option<String> {
    let text = body.to_string();
    let note = text.find("Earlier steps of this task")?;
    let rest = &text[note..];
    let at = rest.find(": result r")? + ": result ".len();
    Some(
        rest[at..]
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect(),
    )
}

/// The handles of the results a request carries put aside.
fn put_aside(body: &Value) -> Vec<String> {
    let mut objects = Vec::new();
    embedded_objects(body, &mut objects);
    objects
        .iter()
        .filter(|object| object.get("put_aside") == Some(&Value::Bool(true)))
        .filter_map(|object| object.get("result")?.as_str().map(str::to_owned))
        .collect()
}

/// An invoke reading letter `turn + 1`, saying it finished an item on that
/// item's last turn.
fn read_turn(turn: usize) -> Vec<(&'static str, Value)> {
    let mut input = invoke("read-letter", &serde_json::json!({"letter": turn + 1})).1;
    if (turn + 1) % TURNS_PER_ITEM == 0 {
        let done = (turn + 1) / TURNS_PER_ITEM;
        input["progress"] = serde_json::json!({
            "done": done, "of": ITEMS, "item": format!("reviewed candidate {done}")
        });
    }
    vec![(tools::INVOKE, input)]
}

fn assert_within_context(bodies: &[Value]) {
    for (turn, body) in bodies.iter().enumerate() {
        let tokens = Context::tokens_of(body);
        assert!(
            tokens <= usize::try_from(CONTEXT).unwrap(),
            "turn {turn} sends about {tokens} tokens, past the model's {CONTEXT}"
        );
    }
}

/// A run of 8 items over 150 model turns, each reading a paged document:
/// far more than the model's context holds. Every request stays within it,
/// because results read long ago are put aside as their handles; the model
/// reads one of them again whole; the person is told each item once.
#[test]
fn a_150_turn_run_of_8_items_stays_within_its_models_context() {
    const TURNS: usize = 150;
    let reread = Arc::new(Mutex::new(None::<String>));
    let seen = Arc::clone(&reread);
    let (mut f, reading) = reading_fixture(
        "Go through today's applications and review each",
        letters(),
        long_budget(200),
        model_with_context(),
        Box::new(move |turn, body| {
            match turn {
                // Read one result folded away long ago again, from the start.
                144 => {
                    let handle = folded_handle(body).expect("a folded result");
                    vec![(
                        crate::pages::READ_RESULT,
                        serde_json::json!({"result": handle, "from": 0}),
                    )]
                }
                145 => {
                    let mut objects = Vec::new();
                    embedded_objects(body, &mut objects);
                    *seen.lock().unwrap() = objects
                        .iter()
                        .rev()
                        .find_map(|object| object.get("text")?.as_str().map(str::to_owned));
                    read_turn(turn)
                }
                last if last + 1 == TURNS => vec![finish("Reviewed all 8 applications.", true)],
                _ => read_turn(turn),
            }
        }),
    );
    f.session.run_task(&f.run).unwrap();
    assert_eq!(
        terminal(&f.sent.borrow()),
        runtime_collab::TaskRunUpdate::Succeeded {
            message: "Reviewed all 8 applications.".into()
        }
    );
    let bodies = reading.bodies.lock().unwrap().clone();
    assert_eq!(
        bodies.len(),
        TURNS,
        "one request per model turn, past the old 30"
    );
    assert_within_context(&bodies);
    let read = f.world.borrow().steps.len();
    assert_eq!(read, TURNS - 2, "every read ran once");
    // Each letter alone is about a thousand tokens: 150 of them are far
    // past the model's context, so the run's oldest turns were folded and
    // the later ones put aside.
    let last = bodies.last().unwrap();
    assert!(folded_handle(last).is_some(), "the oldest turns are folded");
    assert!(
        bodies.iter().any(|body| !put_aside(body).is_empty()),
        "older results were put aside before turns were folded"
    );
    let reread = reread
        .lock()
        .unwrap()
        .clone()
        .expect("the re-read result came back");
    assert!(reread.contains(MARKER), "re-read whole: {reread}");
    let progress: Vec<String> = notes(&f.sent.borrow())
        .into_iter()
        .filter(|note| note.contains(" of 8: "))
        .collect();
    let expected: Vec<String> = (1..=ITEMS)
        .map(|item| format!("{item} of 8: reviewed candidate {item}"))
        .collect();
    assert_eq!(progress, expected, "one line per item, in order");
}

/// A step that asks its person first, deep in a long run: the run waits for
/// the answer, the step runs once with the arguments asked about, and the run
/// goes on to finish many turns later.
#[test]
fn a_confirmation_inside_a_long_run_waits_and_the_run_goes_on() {
    const TURNS: usize = 80;
    let (mut f, reading) = reading_fixture(
        "Go through today's applications and draft a closing line",
        World {
            awaiting: Some(("append-paragraph".into(), 1)),
            ..letters()
        },
        long_budget(120),
        model_with_context(),
        Box::new(|turn, _| match turn {
            60 => vec![invoke(
                "append-paragraph",
                &serde_json::json!({"text": "Kind regards, the hiring team"}),
            )],
            last if last + 1 == TURNS => vec![finish("Drafted the closing line.", true)],
            _ => read_turn(turn),
        }),
    );
    f.session.run_task(&f.run).unwrap();
    assert_eq!(
        terminal(&f.sent.borrow()),
        runtime_collab::TaskRunUpdate::Succeeded {
            message: "Drafted the closing line.".into()
        }
    );
    let world = f.world.borrow();
    let appended: Vec<_> = world
        .steps
        .iter()
        .filter(|(action, _)| action == "append-paragraph")
        .collect();
    assert_eq!(appended.len(), 1, "the confirmed step ran once");
    assert_eq!(
        world.blocks.last().unwrap().1,
        "Kind regards, the hiring team"
    );
    drop(world);
    let waits = f
        .sent
        .borrow()
        .iter()
        .filter(|request| {
            matches!(request, CollabRequest::InvokeTaskAction { action, wait: Some(_), .. }
                if action == "append-paragraph")
        })
        .count();
    assert!(waits >= 1, "the step was held until the person answered");
    let bodies = reading.bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), TURNS);
    assert_within_context(&bodies);
}

/// The agent restarts while a long run's step waits for its person: the run
/// is delivered again, takes up exactly that step, and goes on for many more
/// turns within its budget and its model's context.
#[test]
fn a_long_run_resumes_after_a_restart_and_goes_on_within_its_context() {
    const TURNS: usize = 120;
    let (mut f, reading) = reading_fixture(
        "Go through today's applications and draft a closing line",
        World {
            awaiting: Some(("append-paragraph".into(), 1)),
            ..letters()
        },
        long_budget(200),
        model_with_context(),
        Box::new(|turn, _| match turn {
            last if last + 1 == TURNS => vec![finish("Finished the applications.", true)],
            _ => read_turn(turn),
        }),
    );
    let arguments = serde_json::Map::from_iter([("text".to_owned(), Value::from("Kind regards"))]);
    let catalogue = f.session.task_catalogue();
    let descriptor = surface::find(&catalogue.offers, &op("append-paragraph"))
        .unwrap()
        .descriptor
        .clone();
    f.run.paused = Some(runtime_collab::TaskRunPausedStep {
        app: "so.tilo.word-processor".into(),
        resource: DOC.into(),
        action: "append-paragraph".into(),
        draft: tilo_actions::Draft::render(&descriptor, &arguments),
        arguments_digest: tilo_actions::draft::arguments_digest(&arguments),
    });
    f.session.run_task(&f.run).unwrap();
    assert_eq!(
        terminal(&f.sent.borrow()),
        runtime_collab::TaskRunUpdate::Succeeded {
            message: "Finished the applications.".into()
        }
    );
    let world = f.world.borrow();
    assert_eq!(
        world
            .steps
            .iter()
            .filter(|(action, _)| action == "append-paragraph")
            .count(),
        1,
        "the waiting step ran once, after the restart"
    );
    drop(world);
    let bodies = reading.bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), TURNS);
    assert!(
        bodies[0].to_string().contains("has resumed at that step"),
        "the model is told the run resumed"
    );
    assert_within_context(&bodies);
    assert!(
        bodies.iter().any(|body| !put_aside(body).is_empty()),
        "older results were put aside"
    );
}

/// A model that declares no tool calling never gets a turn: the run fails
/// at once and names the model and its provider.
#[test]
fn a_model_without_tool_calling_is_refused_by_name() {
    let (mut f, reading) = reading_fixture(
        "Go through today's applications",
        World::default(),
        long_budget(200),
        agent_provider_config::ModelDescriptor {
            tool_calling: false,
            ..agent_provider_config::ModelDescriptor::new("tiny-chat")
        },
        Box::new(|_, _| vec![finish("never", true)]),
    );
    f.session.run_task(&f.run).unwrap();
    assert!(reading.bodies.lock().unwrap().is_empty(), "no model turn");
    assert!(f.world.borrow().steps.is_empty());
    assert!(matches!(
        terminal(&f.sent.borrow()),
        runtime_collab::TaskRunUpdate::Failed { message }
            if message.contains("tiny-chat from Scripted cannot call tools")
    ));
}
