//! The agent loop's tests: what a run says about itself, and what it does
//! when it cannot do what it was asked.

use super::*;
use crate::model::{Chooser, TurnRequest, TurnTransport};
use crate::session::Config;
use std::sync::{Arc, Mutex};
use std::{cell::RefCell, rc::Rc};
use tilo_actions::{ActionContract, ActionParam, Availability, ParamType};
use ui::collaboration::CollabRequest;
use ui::collaboration::{BrokerActionSurface, ExportedAction, ExportedInput};
use uuid::Uuid;

#[test]
fn neutral_truncation_retains_output_without_dispatching_calls() {
    use agent_provider_config::protocol::{
        Content, Message, ModelResponse, Role, StopReason, StreamEvent, Usage,
    };
    let response = ModelResponse {
        message: Message {
            role: Role::Assistant,
            content: vec![Content::ToolCall(crate::model::ToolCall {
                id: "a".into(),
                name: tools::INVOKE.into(),
                input: invoke("type-text", &serde_json::json!({"text": "Do not write"})).1,
                extra_content: None,
            })],
        },
        stop_reason: StopReason::OutputLimit,
        usage: Usage::default(),
    };
    let mut f = fixture(
        "write hello",
        World::default(),
        vec![vec![
            TurnEvent::Neutral(StreamEvent::TextDelta {
                text: "Partial answer".into(),
            }),
            TurnEvent::Neutral(StreamEvent::ReasoningDelta {
                text: "Provider summary".into(),
            }),
            TurnEvent::Neutral(StreamEvent::Finished {
                response: response.clone(),
            }),
            TurnEvent::Finished(Ok(serde_json::to_string(&response).unwrap())),
        ]],
    );
    f.session.chooser =
        Chooser::scripted_neutral(Duration::from_secs(5), Arc::new(Scripted(f.script.clone())));
    f.session.run_task(&f.run).unwrap();
    assert!(f.world.borrow().steps.is_empty());
    assert!(matches!(terminal(&f.sent.borrow()),
        runtime_collab::TaskRunUpdate::Failed { message }
            if message.contains("Partial answer") && message.contains("Provider summary")));
    let sent = f.sent.borrow();
    let snapshots = sent
        .iter()
        .filter_map(|request| match request {
            CollabRequest::ReportTaskRun {
                run_id,
                lease,
                update:
                    runtime_collab::TaskRunUpdate::Progress {
                        presentation: Some(p),
                        ..
                    },
                ..
            } => {
                assert_eq!(*run_id, f.run.run_id);
                assert_eq!(lease, &f.run.lease);
                Some(p)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let final_snapshot = snapshots
        .last()
        .expect("production report carries typed output");
    assert_eq!(final_snapshot.text, "Partial answer");
    assert_eq!(final_snapshot.reasoning, "Provider summary");
    assert_eq!(
        final_snapshot.finish,
        Some(runtime_collab::TaskPresentationFinish::Truncated)
    );
    assert!(
        !serde_json::to_string(final_snapshot)
            .unwrap()
            .contains("Do not write")
    );
    assert!(
        snapshots
            .windows(2)
            .all(|pair| pair[0].run == pair[1].run && pair[0].sequence < pair[1].sequence)
    );
}

#[test]
fn presentation_reports_coalesce_bound_output_and_fall_back_for_old_hosts() {
    let mut f = fixture("say hello", World::default(), Vec::new());
    let (_tx, rx) = std::sync::mpsc::channel();
    let mut active = ActiveTurn::new(rx);
    active.presentation_run = "7:0".into();
    active.presentation.text = "hello".into();
    f.session.turn = Some(active);
    // A hundred rapid tokens do not make a hundred host round trips.
    for _ in 0..100 {
        f.session.publish_turn_presentation(&f.run).unwrap();
    }
    assert!(f.sent.borrow().is_empty());
    let active = f.session.turn.as_mut().unwrap();
    active.presentation.text = "é".repeat(40 * 1024);
    active.finished = Some(Err("stream lost".into()));
    f.session.publish_turn_presentation(&f.run).unwrap();
    f.session.publish_turn_presentation(&f.run).unwrap();
    let sent = f.sent.borrow();
    assert_eq!(sent.len(), 1);
    let CollabRequest::ReportTaskRun {
        update:
            runtime_collab::TaskRunUpdate::Progress {
                presentation: Some(p),
                ..
            },
        ..
    } = &sent[0]
    else {
        panic!("typed output report")
    };
    assert_eq!(
        p.text.len(),
        runtime_collab::TaskPresentation::MAX_TEXT_BYTES
    );
    assert_eq!(
        p.finish,
        Some(runtime_collab::TaskPresentationFinish::Truncated)
    );
    drop(sent);
    f.run.presentation_version = None;
    let (_tx, rx) = std::sync::mpsc::channel();
    let mut active = ActiveTurn::new(rx);
    active.presentation_run = "7:1".into();
    active.presentation.text = "Readable answer".into();
    active.finished = Some(Ok(String::new()));
    f.session.turn = Some(active);
    f.session.publish_turn_presentation(&f.run).unwrap();
    let sent = f.sent.borrow();
    let CollabRequest::ReportTaskRun { update, .. } = sent.last().unwrap() else {
        panic!("a progress report")
    };
    let json = serde_json::to_value(update).unwrap();
    assert_eq!(json["message"], "Readable answer");
    assert!(
        json.get("presentation").is_none(),
        "strict old host can decode this report"
    );
}

#[test]
fn completed_stream_frames_cannot_dispatch_before_a_failed_final_response() {
    let mut events = turn(
        &[invoke(
            "type-text",
            &serde_json::json!({"text": "Do not write"}),
        )],
        6,
    );
    events.pop();
    events.push(TurnEvent::Finished(Err("provider stream reset".into())));
    let mut f = fixture("write hello", World::default(), vec![events]);
    f.session.run_task(&f.run).unwrap();
    assert!(f.world.borrow().steps.is_empty());
    assert!(
        !f.sent
            .borrow()
            .iter()
            .any(|request| matches!(request, CollabRequest::InvokeTaskAction { .. }))
    );
    assert!(matches!(terminal(&f.sent.borrow()),
        runtime_collab::TaskRunUpdate::Failed { message } if message.contains("provider stream reset")));
}

#[test]
fn malformed_later_calls_prevent_the_entire_batch_from_dispatching() {
    let mut f = fixture(
        "write hello",
        World::default(),
        vec![turn(
            &[
                invoke("type-text", &serde_json::json!({"text": "Do not write"})),
                (
                    tools::LIST_OPERATIONS,
                    serde_json::json!({"application": false}),
                ),
            ],
            6,
        )],
    );
    f.session.run_task(&f.run).unwrap();
    assert!(f.world.borrow().steps.is_empty());
    assert!(
        !f.sent
            .borrow()
            .iter()
            .any(|request| matches!(request, CollabRequest::InvokeTaskAction { .. }))
    );
    assert!(matches!(
        terminal(&f.sent.borrow()),
        runtime_collab::TaskRunUpdate::Failed { .. }
    ));
}

/// Answers each turn with the next scripted event list and records the
/// request bodies, like the gateway but without a network.
#[derive(Default)]
struct Script {
    turns: Mutex<VecDeque<Vec<TurnEvent>>>,
    bodies: Mutex<Vec<Value>>,
    timeouts: Mutex<Vec<Duration>>,
    /// Delivered one event per `pace`, like a provider writing tokens.
    pace: Option<Duration>,
    /// Events delivered so far, across turns.
    emitted: Arc<std::sync::atomic::AtomicUsize>,
}

struct Scripted(Arc<Script>);

impl TurnTransport for Scripted {
    fn start(&self, request: TurnRequest) -> Receiver<TurnEvent> {
        self.0.timeouts.lock().unwrap().push(request.timeout);
        self.0.bodies.lock().unwrap().push(request.body);
        let events = self
            .0
            .turns
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| vec![TurnEvent::Finished(Err("script exhausted".into()))]);
        // Paced, each send waits until the loop takes the event, so
        // `emitted` never counts events still queued: it is a lower bound
        // on what the loop has received, and order assertions on it hold.
        let capacity = if self.0.pace.is_some() {
            0
        } else {
            events.len()
        };
        let (tx, rx) = std::sync::mpsc::sync_channel(capacity);
        let emitted = Arc::clone(&self.0.emitted);
        let send = move |events: Vec<TurnEvent>, pace: Option<Duration>| {
            for event in events {
                if let Some(pace) = pace {
                    std::thread::sleep(pace);
                }
                if tx.send(event).is_err() {
                    break;
                }
                emitted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        };
        match self.0.pace {
            Some(pace) => {
                std::thread::spawn(move || send(events, Some(pace)));
            }
            None => send(events, None),
        }
        rx
    }
}

/// One OpenAI-dialect turn calling `calls` (`(name, arguments)`), streamed
/// as argument deltas of `piece` bytes and completed-call frames, then the
/// assembled body.
fn turn(calls: &[(&str, Value)], piece: usize) -> Vec<TurnEvent> {
    let mut events = Vec::new();
    let mut tool_calls = Vec::new();
    for (index, (name, arguments)) in calls.iter().enumerate() {
        let id = format!("call_{index}");
        let text = arguments.to_string();
        let mut start = 0;
        while start < text.len() {
            let mut end = (start + piece).min(text.len());
            while !text.is_char_boundary(end) {
                end += 1;
            }
            events.push(TurnEvent::Frame {
                event: tilo_model_client::EVENT_TOOL_CALL_DELTA.into(),
                output: serde_json::json!({
                    "index": index, "id": id, "name": name, "delta": &text[start..end]
                })
                .to_string(),
            });
            start = end;
        }
        events.push(TurnEvent::Frame {
            event: tilo_model_client::EVENT_TOOL_CALL.into(),
            output: serde_json::json!({"id": id, "name": name, "arguments": text}).to_string(),
        });
        tool_calls.push(serde_json::json!({
            "id": id, "type": "function",
            "function": {"name": name, "arguments": text}
        }));
    }
    events.push(TurnEvent::Finished(Ok(serde_json::json!({
        "choices": [{"finish_reason": "tool_calls", "message": {
            "role": "assistant", "content": null, "tool_calls": tool_calls
        }}]
    })
    .to_string())));
    events
}

/// One Anthropic-dialect turn calling `calls`, as the gateway streams it:
/// a call's argument deltas (indexed among the turn's tool calls), its
/// completed-call frame at its `content_block_stop`, then the assembled
/// Messages body, whose text blocks the frames never carried.
fn anthropic_turn(calls: &[(&str, Value)], piece: usize) -> Vec<TurnEvent> {
    let mut events = Vec::new();
    let mut content = vec![serde_json::json!({"type": "text", "text": "On it."})];
    for (index, (name, arguments)) in calls.iter().enumerate() {
        let id = format!("toolu_0{index}");
        let text = arguments.to_string();
        let mut start = 0;
        while start < text.len() {
            let mut end = (start + piece).min(text.len());
            while !text.is_char_boundary(end) {
                end += 1;
            }
            events.push(TurnEvent::Frame {
                event: tilo_model_client::EVENT_TOOL_CALL_DELTA.into(),
                output: serde_json::json!({
                    "index": index, "id": id, "name": name, "delta": &text[start..end]
                })
                .to_string(),
            });
            start = end;
        }
        events.push(TurnEvent::Frame {
            event: tilo_model_client::EVENT_TOOL_CALL.into(),
            output: serde_json::json!({"id": id, "name": name, "arguments": text}).to_string(),
        });
        content.push(serde_json::json!({
            "type": "tool_use", "id": id, "name": name, "input": arguments
        }));
    }
    events.push(TurnEvent::Finished(Ok(serde_json::json!({
        "id": "msg_1", "type": "message", "role": "assistant", "model": "scripted",
        "content": content, "stop_reason": "tool_use", "stop_sequence": null,
        "usage": {"input_tokens": 10, "output_tokens": 20}
    })
    .to_string())));
    events
}

const PEER: u64 = 7;
const CHAT_PEER: u64 = 8;
const DESKTOP_PEER: u64 = 9;
const SECOND_PEER: u64 = 10;
const DOC: &str = "word-processor.document";

/// The first run waits for a companion until it has published actions:
/// a surface of the companion's app holding none (still starting) does
/// not count, and other apps' surfaces never stand in for it.
#[test]
fn a_companion_counts_once_it_has_published_actions() {
    let companions = vec!["so.tilo.web-search".to_owned()];
    let surface = |app: &str, actions: Vec<ExportedAction>| BrokerActionSurface {
        peer_id: PEER,
        provider: None,
        app_id: app.into(),
        cluster_id: Uuid::from_u128(1).to_string(),
        record_id: String::new(),
        live: true,
        actions,
    };
    let search = || {
        descriptor(
            "search",
            "Search the web",
            tilo_actions::OperationKind::Effect,
            vec![],
            Availability::Available,
            vec![],
        )
    };
    assert_eq!(missing_companions(&companions, &[]), companions);
    assert_eq!(
        missing_companions(&companions, &[surface("so.tilo.notes", vec![search()])]),
        companions
    );
    assert_eq!(
        missing_companions(&companions, &[surface("so.tilo.web-search", vec![])]),
        companions
    );
    assert!(
        missing_companions(
            &companions,
            &[surface("so.tilo.web-search", vec![search()])]
        )
        .is_empty()
    );
}

fn descriptor(
    action: &str,
    label: &str,
    kind: tilo_actions::OperationKind,
    params: Vec<ActionParam>,
    availability: Availability,
    tags: Vec<String>,
) -> ExportedAction {
    ExportedAction {
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        resource: DOC.into(),
        action: action.into(),
        resource_kind: "word-processor.document".into(),
        resource_label: "Brief".into(),
        action_label: label.into(),
        keywords: Vec::new(),
        icon: None,
        description: None,
        operation_kind: kind,
        params,
        contract: ActionContract {
            availability,
            tags,
            ..ActionContract::default()
        },
        view: None,
        input: ExportedInput::None,
    }
}

/// The document as the fake application holds it, plus the invoker's
/// own view (its selection).
#[derive(Default)]
struct World {
    blocks: Vec<(String, String)>,
    selection: Option<String>,
    bold: Vec<String>,
    /// `(action, arguments)` of every step, in order.
    steps: Vec<(String, serde_json::Map<String, Value>)>,
    refuse: Vec<String>,
    /// Refuse the n-th (0-based) step of this action.
    refuse_nth: Option<(String, usize)>,
    /// Model events delivered when each step arrived.
    steps_at: Vec<usize>,
    emitted: Arc<std::sync::atomic::AtomicUsize>,
    /// Messages posted in the Chat channel.
    posts: Vec<String>,
    /// A fresh workspace: Writer is closed (publishes nothing) until the
    /// desktop's open starts it.
    fresh: bool,
    writer_opened: bool,
    /// Cancel after this many successful status surveys (zero disables).
    cancel_after_checks: usize,
    cancellation_checks: usize,
    native_control: Option<runtime_collab::TaskRunControl>,
    native_applied_revision: u64,
    /// Cancel once this many steps have been applied.
    cancel_after_steps: Option<usize>,
    /// How long the host takes to answer a cancellation poll; steps are
    /// answered at once.
    cancel_delay: Duration,
    /// How many more of the agent's link polls a cancellation answer
    /// takes, on top of `cancel_delay`: a count the agent's waiting
    /// drives, where a delay is the machine's speed.
    cancel_delay_polls: usize,
    /// How the desktop's open of Writer behaves.
    opens: Opens,
    /// The record of a second Writer window (its own peer and record,
    /// the same resource ids), once an open added one.
    second_window: Option<&'static str>,
    /// A republished catalogue reaches the agent this many link polls
    /// after the step's result instead of with it.
    republish_after_polls: usize,
    /// A child of the run is accepted at once and settles in its
    /// application this long after it started: a re-ask answers its
    /// terminal update from then on.
    settle_after: Option<Duration>,
    /// The first this many invocations settle together, `settle_after`
    /// after the last of them started; until it has, each of them waits
    /// (at most the hang bound). A step that waited for an earlier one
    /// to settle never lets them.
    settle_gate: usize,
    /// The invocations the gate holds, in start order.
    gated: Vec<tilo_actions::InvocationId>,
    /// A native child settles when the host takes in this re-ask of it
    /// (1-based), instead of after `settle_after`.
    settles_at_reask: Option<usize>,
    /// When each re-ask of a native child reached the host.
    reasked_at: Vec<ui::time::Instant>,
    /// How long the host holds a re-ask that asks to be held.
    reask_hold: ReaskHold,
    /// Actions the broker refuses to start (an authorization refusal):
    /// nothing is applied.
    deny: Vec<String>,
    /// Every child started: its action, when it started and
    /// when it settles in its application.
    started: Vec<(String, ui::time::Instant, ui::time::Instant)>,
    /// Terminal updates of the children, with when they are due.
    settling:
        HashMap<tilo_actions::InvocationId, (ui::time::Instant, tilo_actions::InvocationUpdate)>,
    /// Invocations the agent asked the host to cancel.
    cancelled: Vec<tilo_actions::InvocationId>,
    /// Re-asks of a started native child (observations), and how many
    /// of them were answered with nothing new.
    observations: usize,
    idle_observations: usize,
    /// An action that asks its person first, and how many more asks the
    /// host answers with its draft waiting before the person confirms.
    awaiting: Option<(String, usize)>,
    /// The room also runs tilolang Board and Views, whose hosts take programs,
    /// and the Tasks tracker records the run's steps.
    tilolang: bool,
    /// What each `run-block` answers, by a word its source holds.
    tl_blocks: Vec<(&'static str, Value)>,
    /// What the door answers each task program, by a word the program holds.
    tl_programs: Vec<(&'static str, Value)>,
}

/// How the fixture host treats a re-ask that asks to be held.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ReaskHold {
    #[default]
    AsAsked,
    /// Until its child moves on, however long the re-ask asked.
    UntilNews,
    /// Not at all: answered at once, as a host from before held re-asks
    /// does.
    AnswerAtOnce,
}

/// How the desktop's open of Writer behaves in the fixture world.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Opens {
    /// A window per open; the result names none (an owner that does not
    /// report its windows).
    #[default]
    Unreported,
    /// A window per open; the receipt names it.
    Reported,
    /// An open of Writer while it is open lands on that window, and the
    /// receipt names it.
    OntoOpenWindow,
}

impl World {
    #[allow(
        clippy::too_many_lines,
        reason = "the fixture world declares every published operation in one place"
    )]
    fn surfaces(&self) -> Vec<BrokerActionSurface> {
        let text =
            |name: &'static str| ActionParam::required(name, ParamType::Text, "The text to write");
        let toggle = if self.selection.is_some() {
            Availability::Available
        } else {
            Availability::Unavailable {
                code: "selection_empty".into(),
                reason: "select something first".into(),
            }
        };
        let mut surfaces = vec![
            BrokerActionSurface {
                peer_id: PEER,
                provider: None,
                app_id: "so.tilo.word-processor".into(),
                cluster_id: Uuid::from_u128(1).to_string(),
                record_id: "doc-1".into(),
                live: true,
                actions: vec![
                    descriptor(
                        "read-text",
                        "Read the document",
                        tilo_actions::OperationKind::View,
                        vec![],
                        Availability::Available,
                        vec![],
                    ),
                    descriptor(
                        "select-block",
                        "Select a paragraph",
                        tilo_actions::OperationKind::View,
                        vec![ActionParam::required("block", ParamType::Text, "Block id")],
                        Availability::Available,
                        vec![],
                    ),
                    descriptor(
                        "toggle-mark",
                        "Toggle a mark on the selection",
                        tilo_actions::OperationKind::Document,
                        vec![
                            ActionParam::required("mark", ParamType::Text, "Which mark")
                                .with_choices(["strong", "em"]),
                        ],
                        toggle,
                        vec![],
                    ),
                    descriptor(
                        "append-paragraph",
                        "Append paragraph",
                        tilo_actions::OperationKind::Document,
                        vec![text("text")],
                        Availability::Available,
                        vec![],
                    ),
                    descriptor(
                        "type-text",
                        "Type text",
                        tilo_actions::OperationKind::Document,
                        vec![text("text")],
                        Availability::Available,
                        vec![],
                    ),
                    descriptor(
                        "erase-text",
                        "Erase text",
                        tilo_actions::OperationKind::Document,
                        vec![
                            ActionParam::required("direction", ParamType::Text, "backward|forward")
                                .with_choices(["backward", "forward"]),
                            ActionParam::required("count", ParamType::Integer, "Characters"),
                        ],
                        Availability::Available,
                        vec![],
                    ),
                ],
            },
            BrokerActionSurface {
                peer_id: CHAT_PEER,
                provider: None,
                app_id: "so.tilo.chat".into(),
                cluster_id: Uuid::from_u128(1).to_string(),
                record_id: String::new(),
                live: true,
                actions: vec![ExportedAction {
                    state: tilo_actions::ActionState::default(),
                    semantic_identity: None,
                    resource: "chat.channel:general".into(),
                    resource_kind: "chat.channel".into(),
                    resource_label: "# general".into(),
                    ..descriptor(
                        "post",
                        "Post message",
                        tilo_actions::OperationKind::Document,
                        vec![text("message")],
                        Availability::Available,
                        vec![],
                    )
                }],
            },
        ];
        if self.tilolang {
            surfaces.extend(programs::surfaces());
        }
        if let Some(record) = self.second_window {
            surfaces.push(BrokerActionSurface {
                peer_id: SECOND_PEER,
                record_id: record.into(),
                ..surfaces[0].clone()
            });
        }
        if self.fresh {
            if !self.writer_opened {
                surfaces.remove(0);
            }
            let open = |app: &str, title: &str| ExportedAction {
                state: tilo_actions::ActionState::default(),
                semantic_identity: None,
                resource: format!("{}{app}", tools::LAUNCHER_OPEN_PREFIX),
                resource_kind: "desktop.application".into(),
                resource_label: title.into(),
                ..descriptor(
                    "open",
                    &format!("Open {title}"),
                    tilo_actions::OperationKind::Effect,
                    vec![],
                    Availability::Available,
                    vec![],
                )
            };
            surfaces.push(BrokerActionSurface {
                peer_id: DESKTOP_PEER,
                provider: None,
                app_id: "so.tilo.desktop".into(),
                cluster_id: Uuid::from_u128(1).to_string(),
                record_id: String::new(),
                live: true,
                actions: vec![
                    open("so.tilo.word-processor", "Writer"),
                    open("so.tilo.notes", "Notes"),
                ],
            });
        }
        surfaces
    }

    /// The desktop's open of Writer: its first window, a second one, or
    /// — when opens land on it — the window already open. The committed
    /// outcome; its receipt names the window when the world says so.
    fn open_writer(&mut self) -> Value {
        let record = if !self.writer_opened || self.opens == Opens::OntoOpenWindow {
            "doc-1"
        } else {
            self.second_window = Some("doc-2");
            "doc-2"
        };
        self.writer_opened = true;
        let output = if self.opens == Opens::Unreported {
            serde_json::json!({})
        } else {
            serde_json::json!({"value": {
                "kind": "desktop.command",
                "command": "1:1",
                "generation": 1,
                "opened": [{
                    "app_id": "so.tilo.word-processor",
                    "record_id": record,
                    "strip": "1",
                    "group": "1",
                    "tab": "1"
                }]
            }})
        };
        serde_json::json!({ "succeeded": output })
    }

    /// Apply one step; the committed result, and whether the
    /// catalogue changed.
    #[allow(
        clippy::too_many_lines,
        reason = "the fixture world answers every published operation in one place"
    )]
    fn apply(&mut self, action: &str, arguments: &serde_json::Map<String, Value>) -> (Value, bool) {
        let nth = self.steps.iter().filter(|(seen, _)| seen == action).count();
        self.steps.push((action.into(), arguments.clone()));
        self.steps_at
            .push(self.emitted.load(std::sync::atomic::Ordering::SeqCst));
        if self
            .refuse_nth
            .as_ref()
            .is_some_and(|(refused, n)| refused == action && *n == nth)
        {
            return (serde_json::json!({"failed": "app.refused"}), false);
        }
        if self.refuse.iter().any(|refused| refused == action) {
            return (serde_json::json!({"failed": "app.refused"}), false);
        }
        let text = |name: &str| {
            arguments
                .get(name)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned()
        };
        match action {
            "read-text" => (
                serde_json::json!({"succeeded": {"value": {"blocks": self
                    .blocks
                    .iter()
                    .map(|(id, text)| serde_json::json!({"id": id, "text": text}))
                    .collect::<Vec<_>>()}}}),
                false,
            ),
            "select-block" => {
                self.selection = Some(text("block"));
                (
                    serde_json::json!({"succeeded": {"value": {"selection": {"block": text("block")}}}}),
                    true,
                )
            }
            "toggle-mark" => match &self.selection {
                Some(block) => {
                    self.bold.push(block.clone());
                    (serde_json::json!({"succeeded": {}}), false)
                }
                None => (serde_json::json!({"failed": "selection_empty"}), false),
            },
            "append-paragraph" => {
                self.blocks
                    .push((format!("p{}", self.blocks.len() + 1), text("text")));
                (serde_json::json!({"succeeded": {}}), false)
            }
            "erase-text" => {
                // Backward from the caret at the document end: a
                // paragraph break counts as one character.
                let mut left = arguments.get("count").and_then(Value::as_u64).unwrap_or(0);
                while left > 0 {
                    let Some(last) = self.blocks.last_mut() else {
                        break;
                    };
                    if last.1.is_empty() {
                        self.blocks.pop();
                        left -= 1;
                        continue;
                    }
                    last.1.pop();
                    left -= 1;
                }
                (serde_json::json!({"succeeded": {}}), false)
            }
            "post" => {
                self.posts.push(text("message"));
                (serde_json::json!({"succeeded": {}}), false)
            }
            "open" => (self.open_writer(), true),
            "run-block" => {
                let source = text("source");
                let answer = self
                    .tl_blocks
                    .iter()
                    .find(|(word, _)| source.contains(word))
                    .map(|(_, answer)| answer.clone());
                match answer {
                    Some(Value::String(error)) => {
                        (serde_json::json!({ "failed_with": error }), false)
                    }
                    Some(answer) => (serde_json::json!({"succeeded": {"value": answer}}), false),
                    None => (serde_json::json!({"failed": "no block"}), false),
                }
            }
            "run-program" => {
                let program = text("program");
                let answer = self
                    .tl_programs
                    .iter()
                    .find(|(word, _)| program.contains(word))
                    .map(|(_, answer)| answer.clone());
                match answer {
                    Some(answer) => (serde_json::json!({"succeeded": {"value": answer}}), false),
                    None => (serde_json::json!({"failed": "no program"}), false),
                }
            }
            "catalogue" => (
                serde_json::json!({"succeeded": {"value": programs::catalogue()}}),
                false,
            ),
            "answer" => (
                serde_json::json!({"succeeded": {"value": {
                    "type": "Answer", "id": "root/items:v1", "label": text("title")
                }}}),
                false,
            ),
            "record-step" | "record-answer" => (serde_json::json!({"succeeded": {}}), false),
            "type-text" => {
                if self.blocks.is_empty() {
                    self.blocks.push(("p1".into(), String::new()));
                }
                if let Some(last) = self.blocks.last_mut() {
                    last.1.push_str(&text("text"));
                }
                (serde_json::json!({"succeeded": {}}), false)
            }
            _ => (serde_json::json!({"failed": "unknown"}), false),
        }
    }
}

struct Broker {
    world: Rc<RefCell<World>>,
    sent: Rc<RefCell<Vec<CollabRequest>>>,
    events: Vec<CollabEvent>,
    /// Host answers not delivered yet, with when they are due and how
    /// many more link polls they take.
    held: Vec<(ui::time::Instant, usize, CollabEvent)>,
    /// Re-asks the host holds: request id, child, what the agent had
    /// seen, and when the hold ends.
    holding: Vec<HeldReask>,
    /// A catalogue still on its way, and the polls until it arrives.
    late: Option<(usize, CollabEvent)>,
}

/// A native re-ask the fake host holds until its child moves on.
struct HeldReask {
    request_id: u64,
    invocation: tilo_actions::InvocationId,
    seen: tilo_actions::remote::UpdateMark,
    until: ui::time::Instant,
}

/// The host reading a native run as cancelled cancels every child of it
/// still running (`cancel_native_children`).
fn cancel_native_children(world: &mut World) {
    let now = ui::time::now();
    let mut stopped = Vec::new();
    for (invocation, (due, terminal)) in &mut world.settling {
        if *due > now {
            *due = now;
            *terminal = tilo_actions::InvocationUpdate::Cancelled {
                invocation: *invocation,
                message: Some("Cancelled from Task Center".into()),
            };
            stopped.push(*invocation);
        }
    }
    world.cancelled.extend(stopped);
}

impl Broker {
    /// Deliver a host answer after `delay` and `polls` more link polls.
    fn answer_after(&mut self, delay: Duration, polls: usize, event: CollabEvent) {
        self.held.push((ui::time::now() + delay, polls, event));
    }

    fn answer_cancellation(&mut self, request_id: u64, lease: &str) {
        let (cancelled, delay, control) = {
            let mut world = self.world.borrow_mut();
            world.cancellation_checks += 1;
            let cancelled = (world.cancel_after_checks != 0
                && world.cancellation_checks > world.cancel_after_checks)
                || world
                    .cancel_after_steps
                    .is_some_and(|steps| world.steps.len() >= steps);
            let control = world.native_control.clone().map(|mut control| {
                let (_, revision) = runtime_collab::native_task_revision(lease).unwrap();
                control.from_revision = revision;
                control.applied_revision = world.native_applied_revision;
                control
                    .directions
                    .retain(|direction| direction.revision > revision);
                control
            });
            if cancelled {
                cancel_native_children(&mut world);
            }
            (
                cancelled,
                (world.cancel_delay, world.cancel_delay_polls),
                control,
            )
        };
        if let Some(control) = control {
            self.events.push(CollabEvent::TaskRunControl {
                request_id,
                control,
            });
        }
        self.answer_after(
            delay.0,
            delay.1,
            CollabEvent::TaskRunCancellation {
                request_id,
                result: Ok(cancelled),
            },
        );
    }
}

impl ui::collaboration::PeerTransport for Broker {
    fn is_connected(&self) -> bool {
        true
    }
    fn poll(&mut self) -> Vec<CollabEvent> {
        let now = ui::time::now();
        let (due, later) = std::mem::take(&mut self.held)
            .into_iter()
            .map(|(at, polls, event)| (at, polls.saturating_sub(1), event))
            .partition::<Vec<_>, _>(|(at, polls, _)| *at <= now && *polls == 0);
        self.held = later;
        self.events
            .extend(due.into_iter().map(|(_, _, event)| event));
        self.answer_held_reasks(now);
        if let Some((polls, event)) = self.late.take() {
            if polls == 0 {
                self.events.push(event);
            } else {
                self.late = Some((polls - 1, event));
            }
        }
        std::mem::take(&mut self.events)
    }
    fn send(&mut self, request: CollabRequest) -> Result<(), String> {
        if self.observe_child(&request) {
            self.sent.borrow_mut().push(request);
            return Ok(());
        }
        if self.answer_awaiting(&request) {
            self.sent.borrow_mut().push(request);
            return Ok(());
        }
        match &request {
            CollabRequest::InvokeTaskAction {
                request_id,
                invocation,
                action,
                arguments,
                ..
            } if self.world.borrow().deny.contains(action)
                || self.world.borrow().settle_after.is_some() =>
            {
                self.start_child(*request_id, *invocation, action, arguments);
            }
            CollabRequest::InvokeTaskAction {
                request_id,
                invocation,
                action,
                arguments,
                ..
            } => {
                let (outcome, republish) = self.world.borrow_mut().apply(action, arguments);
                let update = if let Some(output) = outcome.get("succeeded") {
                    tilo_actions::InvocationUpdate::Succeeded {
                        invocation: *invocation,
                        output: serde_json::from_value(output.clone()).unwrap_or_default(),
                    }
                } else {
                    tilo_actions::InvocationUpdate::Failed {
                        invocation: *invocation,
                        error: tilo_actions::ActionFailure {
                            code: "app.refused".into(),
                            // The host's own words, when it gave them.
                            message: outcome
                                .get("failed_with")
                                .and_then(Value::as_str)
                                .map_or_else(|| format!("{action} was refused"), str::to_owned),
                            details: None,
                            retryable: false,
                        },
                    }
                };
                self.events.push(CollabEvent::TaskActionInvocationResult {
                    request_id: *request_id,
                    result: Ok((0, update)),
                });
                // Republished after the result, as a real host does: the
                // loop must take it up after a view step.
                if republish {
                    let catalogue = CollabEvent::ActionCatalogue {
                        surfaces: self.world.borrow().surfaces(),
                    };
                    match self.world.borrow().republish_after_polls {
                        0 => self.events.push(catalogue),
                        polls => self.late = Some((polls, catalogue)),
                    }
                }
            }
            CollabRequest::TaskRunCancellation {
                request_id, lease, ..
            } => {
                self.answer_cancellation(*request_id, lease);
            }
            CollabRequest::ReportTaskRun {
                request_id, update, ..
            } => {
                if let runtime_collab::TaskRunUpdate::DirectionsApplied { revision } = update {
                    self.world.borrow_mut().native_applied_revision = *revision;
                }
                // A native report awaits its receipt; a Task Center
                // run's report is sent without one.
                if let Some(request_id) = request_id {
                    self.events.push(CollabEvent::TaskRunReportResult {
                        request_id: *request_id,
                        result: Ok(()),
                    });
                }
            }
            CollabRequest::Presence { .. } => {}
            other => panic!("unexpected request {other:?}"),
        }
        self.sent.borrow_mut().push(request);
        Ok(())
    }
}

impl Broker {
    /// Answer an ask of the action that waits for its person with its
    /// draft still waiting, as the host's confirmation gate does once a
    /// held ask's wait ends;
    /// `false` once the person confirmed it, or for anything else.
    fn answer_awaiting(&mut self, request: &CollabRequest) -> bool {
        let CollabRequest::InvokeTaskAction {
            request_id, action, ..
        } = request
        else {
            return false;
        };
        let mut world = self.world.borrow_mut();
        let Some((_, left)) = world
            .awaiting
            .as_mut()
            .filter(|(waiting, left)| waiting == action && *left > 0)
        else {
            return false;
        };
        *left -= 1;
        drop(world);
        self.events.push(CollabEvent::TaskActionInvocationResult {
            request_id: *request_id,
            result: Err(format!(
                "Failed: {}: {action} waits for the person",
                tilo_actions::draft::AWAITING_CONFIRMATION
            )),
        });
        true
    }

    /// The state of a started child as the host holds it now.
    fn child_state(
        &self,
        invocation: tilo_actions::InvocationId,
    ) -> tilo_actions::InvocationUpdate {
        let world = self.world.borrow();
        match world.settling.get(&invocation) {
            Some((due, terminal)) if *due <= ui::time::now() => terminal.clone(),
            _ => tilo_actions::InvocationUpdate::Accepted { invocation },
        }
    }

    /// Answer a native task's re-ask of a started child (by its stable
    /// id) with the child's state, as `native_task_observation` does: at
    /// once, or — when the re-ask asks to be held — once the child moves
    /// past what the agent saw, the run is cancelled, or the hold ends.
    /// `false` for any other request.
    fn observe_child(&mut self, request: &CollabRequest) -> bool {
        let &CollabRequest::InvokeTaskAction {
            request_id,
            invocation,
            wait,
            ..
        } = request
        else {
            return false;
        };
        if !self.world.borrow().settling.contains_key(&invocation) {
            return false;
        }
        if wait.is_some() {
            let mut world = self.world.borrow_mut();
            let now = ui::time::now();
            world.reasked_at.push(now);
            if world.settles_at_reask == Some(world.reasked_at.len())
                && let Some((due, _)) = world.settling.get_mut(&invocation)
            {
                *due = now;
            }
        }
        let update = self.child_state(invocation);
        let held = wait.filter(|wait| {
            self.world.borrow().reask_hold != ReaskHold::AnswerAtOnce
                && !update.is_terminal()
                && tilo_actions::remote::UpdateMark::of(&update) == wait.seen
        });
        match held {
            Some(wait) => self.holding.push(HeldReask {
                request_id,
                invocation,
                seen: wait.seen,
                until: ui::time::now()
                    + if self.world.borrow().reask_hold == ReaskHold::UntilNews {
                        tilo_test_env::hang_bound()
                    } else {
                        Duration::from_millis(u64::from(wait.wait_ms))
                    },
            }),
            None => self.answer_reask(request_id, invocation, wait.map(|wait| wait.seen)),
        }
        true
    }

    /// Answer the re-ask `request_id` with the child's state now.
    fn answer_reask(
        &mut self,
        request_id: u64,
        invocation: tilo_actions::InvocationId,
        seen: Option<tilo_actions::remote::UpdateMark>,
    ) {
        let update = self.child_state(invocation);
        {
            let mut world = self.world.borrow_mut();
            world.observations += 1;
            let news = update.is_terminal()
                || seen.is_some_and(|seen| tilo_actions::remote::UpdateMark::of(&update) != seen);
            if !news {
                world.idle_observations += 1;
            }
        }
        self.events.push(CollabEvent::TaskActionInvocationResult {
            request_id,
            result: Ok((0, update)),
        });
    }

    /// Answer every held re-ask whose child moved on or whose hold
    /// ended; first sample the run's cancellation, as the host does
    /// while it holds re-asks.
    fn answer_held_reasks(&mut self, now: ui::time::Instant) {
        if self.holding.is_empty() {
            return;
        }
        {
            let mut world = self.world.borrow_mut();
            if world
                .cancel_after_steps
                .is_some_and(|steps| world.steps.len() >= steps)
            {
                cancel_native_children(&mut world);
            }
        }
        for held in std::mem::take(&mut self.holding) {
            let update = self.child_state(held.invocation);
            if update.is_terminal()
                || tilo_actions::remote::UpdateMark::of(&update) != held.seen
                || now >= held.until
            {
                self.answer_reask(held.request_id, held.invocation, Some(held.seen));
            } else {
                self.holding.push(held);
            }
        }
    }

    /// Once the last invocation the gate holds has started, settle
    /// every one of them at `at`.
    fn open_the_gate(&self, at: ui::time::Instant) {
        let mut world = self.world.borrow_mut();
        if world.gated.len() < world.settle_gate {
            return;
        }
        let gated = world.gated.clone();
        // The gate holds the first invocations started.
        let World {
            started,
            settling,
            settle_gate,
            ..
        } = &mut *world;
        for (_, _, settles) in started.iter_mut().take(*settle_gate) {
            *settles = at;
        }
        for invocation in &gated {
            if let Some((due, _)) = settling.get_mut(invocation) {
                *due = at;
            }
        }
    }

    /// Start a child of the run as the action broker does: refused
    /// before anything runs when it is not permitted, else accepted at
    /// once and settled in its application `settle_after` later, which
    /// a re-ask of it then answers.
    fn start_child(
        &mut self,
        request_id: u64,
        invocation: tilo_actions::InvocationId,
        action: &str,
        arguments: &serde_json::Map<String, Value>,
    ) {
        if self
            .world
            .borrow()
            .deny
            .iter()
            .any(|denied| denied == action)
        {
            self.events.push(CollabEvent::TaskActionInvocationResult {
                request_id,
                result: Err(format!("permission denied: the agent may not {action}")),
            });
            return;
        }
        let (outcome, _) = self.world.borrow_mut().apply(action, arguments);
        let terminal = if outcome.get("succeeded").is_some() {
            tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput::default(),
            }
        } else {
            tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: "app.refused".into(),
                    message: format!("{action} was refused"),
                    details: None,
                    retryable: false,
                },
            }
        };
        let now = ui::time::now();
        let settle_after = self.world.borrow().settle_after.unwrap_or_default();
        let gated = {
            let world = self.world.borrow();
            world.gated.len() < world.settle_gate
        };
        let due = now
            + if gated {
                tilo_test_env::hang_bound()
            } else {
                settle_after
            };
        {
            let mut world = self.world.borrow_mut();
            world.started.push((action.to_owned(), now, due));
            world.settling.insert(invocation, (due, terminal));
        }
        if gated {
            self.world.borrow_mut().gated.push(invocation);
            self.open_the_gate(now + settle_after);
        }
        self.events.push(CollabEvent::TaskActionInvocationResult {
            request_id,
            result: Ok((0, tilo_actions::InvocationUpdate::Accepted { invocation })),
        });
    }
}

struct Fixture {
    session: Session,
    run: runtime_collab::TaskRunEnvelope,
    world: Rc<RefCell<World>>,
    sent: Rc<RefCell<Vec<CollabRequest>>>,
    script: Arc<Script>,
}

fn fixture(title: &str, world: World, turns: Vec<Vec<TurnEvent>>) -> Fixture {
    paced_fixture(title, world, turns, None)
}

fn paced_fixture(
    title: &str,
    world: World,
    turns: Vec<Vec<TurnEvent>>,
    pace: Option<Duration>,
) -> Fixture {
    execution_fixture(title, world, turns, pace, None)
}

fn execution_fixture(
    title: &str,
    world: World,
    turns: Vec<Vec<TurnEvent>>,
    pace: Option<Duration>,
    budget: Option<collab_types::task_budget::TaskExecutionBudget>,
) -> Fixture {
    let mut world = world;
    // The host answers every cancellation poll with the run's control
    // frontier, as the broker does for an issue's run.
    world
        .native_control
        .get_or_insert_with(|| runtime_collab::TaskRunControl {
            version: 1,
            task_id: "tasks.tracker/issues/loop".into(),
            run_epoch: 1,
            from_revision: 0,
            applied_revision: 0,
            latest_revision: 0,
            directions: Vec::new(),
            has_more: false,
        });
    let emitted = Arc::clone(&world.emitted);
    let world = Rc::new(RefCell::new(world));
    let sent = Rc::new(RefCell::new(Vec::new()));
    let script = Arc::new(Script {
        turns: Mutex::new(turns.into()),
        bodies: Mutex::new(Vec::new()),
        timeouts: Mutex::new(Vec::new()),
        pace,
        emitted,
    });
    let link = crate::wire::test_link(Broker {
        world: world.clone(),
        sent: sent.clone(),
        events: Vec::new(),
        held: Vec::new(),
        late: None,
        holding: Vec::new(),
    });
    let mut run = runtime_collab::TaskRunEnvelope {
        paused: None,
        model: None,
        version: 1,
        task_id: "tasks.tracker/issues/loop".into(),
        title: title.into(),
        instruction: None,
        subject: None,
        run_id: 7,
        lease: "native:00000000000000000000000000000000:0".into(),
        planning: runtime_collab::TaskRunPlanning::Model,
        presentation_version: Some(1),
        execution_budget: budget,
        action_context: serde_json::Value::Null,
    };
    bind_context(&mut run);
    let surfaces = world.borrow().surfaces();
    let session = Session {
        cfg: Config {
            agent_id: "agent-1".into(),
            endpoint: None,
            endpoint_identity: None,
            app_id: "so.tilo.agent".into(),
            cluster: Uuid::from_u128(1),
            record: None,
            name: "agent-1".into(),
            model: true,
            model_timeout: Duration::from_secs(5),
            permissions: ui::permissions::PermissionContext::agent_granted_at(
                "agent-1",
                &[],
                ui::permissions::KindSet::READ_UNDOABLE,
            ),
            content: ui::content::ContentStore::memory(),
            companions: Vec::new(),
            provider: None,
        },
        link,
        chooser: Chooser::scripted(
            Duration::from_secs(5),
            agent_provider_config::ProviderProtocol::OpenAiCompatible,
            Arc::new(Scripted(script.clone())),
        ),
        managed: false,
        task_deadline: None,
        native_task: None,
        broker_surfaces: surfaces,
        next_broker_request: 1,
        awaiting_companions: Vec::new(),
        held_turn: None,
        broker_revision: 0,
        catalogue_cache: None,
        turn: None,
        outstanding: crate::session::outstanding::Outstanding::default(),
        spend: crate::session::planning::Spend::default(),
    };
    Fixture {
        session,
        run,
        world,
        sent,
        script,
    }
}

/// Bind `run`'s action context to its title and id, as the broker does.
fn bind_context(run: &mut runtime_collab::TaskRunEnvelope) {
    run.action_context = serde_json::to_value(
        tilo_actions::ActionContext::default()
            .with_purpose(run.title.as_str())
            .with_group(run.group(), run.title.as_str(), "task"),
    )
    .unwrap();
    super::super::validate_task_run(run).unwrap();
}

fn op(action: &str) -> String {
    format!("broker:{PEER}:{DOC}#{action}")
}

fn invoke(action: &str, arguments: &Value) -> (&'static str, Value) {
    (
        tools::INVOKE,
        serde_json::json!({"operation": op(action), "parameters": arguments}),
    )
}

fn finish(reply: &str, done: bool) -> (&'static str, Value) {
    (
        tools::PLAN,
        serde_json::json!({"reply": reply, "done": done}),
    )
}

#[path = "agent_loop/tests/native_pipeline.rs"]
mod native_pipeline;
#[path = "agent_loop/tests/pictures.rs"]
mod pictures;
#[path = "agent_loop/tests/programs.rs"]
mod programs;

fn native_fixture(latest: u64) -> Fixture {
    let control = runtime_collab::TaskRunControl {
        version: 1,
        task_id: "tasks.tracker/issues/loop".into(),
        run_epoch: 1,
        from_revision: 0,
        applied_revision: 0,
        latest_revision: latest,
        directions: (1..=latest.min(8))
            .map(|revision| runtime_collab::TaskDirection {
                revision,
                author: "person:reviewer".into(),
                text: "Keep compatibility examples".into(),
                issued_at: 42,
            })
            .collect(),
        has_more: latest > 8,
    };
    fixture(
        "Prepare release notes",
        World {
            native_control: Some(control),
            ..Default::default()
        },
        vec![turn(&[finish("Reviewed instructions", true)], 24)],
    )
}

#[test]
fn native_directions_enter_the_actual_model_request_before_actions() {
    let mut fixture = native_fixture(1);
    fixture.session.run_task(&fixture.run).unwrap();
    let bodies = fixture.script.bodies.lock().unwrap();
    assert_eq!(bodies.len(), 1);
    let request = bodies[0].to_string();
    drop(bodies);
    assert!(request.contains("Keep compatibility examples"));
    assert!(request.contains("person:reviewer"));
    assert!(request.contains("issued_at"));
    assert_eq!(fixture.world.borrow().native_applied_revision, 1);
    assert!(fixture.sent.borrow().iter().any(|request| matches!(
        request,
        CollabRequest::ReportTaskRun {
            update: runtime_collab::TaskRunUpdate::Succeeded { .. },
            ..
        }
    )));
}

#[test]
fn native_model_receives_the_full_private_instruction_not_a_truncated_title() {
    let mut fixture = native_fixture(0);
    let instruction =
        "Inspect every release-note section and preserve the complete compatibility matrix. "
            .repeat(6);
    assert!(instruction.chars().count() > 240);
    fixture.run.instruction = Some(instruction.clone());
    fixture.session.run_task(&fixture.run).unwrap();
    let bodies = fixture.script.bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), 1);
    assert!(bodies[0].to_string().contains(instruction.trim()));
}

#[test]
fn one_issues_run_reaches_the_model_without_another_issues() {
    let mut fixture = native_fixture(0);
    let first_instruction = "first-native-private-instruction-secret";
    fixture.run.instruction = Some(first_instruction.into());
    *fixture.script.turns.lock().unwrap() = vec![turn(
        &[finish("first-native-private-result-secret", true)],
        24,
    )]
    .into();
    fixture.session.run_task(&fixture.run).unwrap();

    fixture.run.task_id = "tasks.tracker/issues/second-private-scope".into();
    fixture.run.run_id += 1;
    fixture.run.action_context = serde_json::to_value(
        tilo_actions::ActionContext::default()
            .with_purpose(fixture.run.title.as_str())
            .with_group(fixture.run.group(), fixture.run.title.as_str(), "task"),
    )
    .unwrap();
    fixture
        .world
        .borrow_mut()
        .native_control
        .as_mut()
        .unwrap()
        .task_id = fixture.run.task_id.clone();
    let second_instruction = "second-native-private-instruction-secret";
    fixture.run.instruction = Some(second_instruction.into());
    *fixture.script.turns.lock().unwrap() = vec![turn(
        &[finish("second-native-private-result-secret", true)],
        24,
    )]
    .into();
    fixture.session.run_task(&fixture.run).unwrap();

    let bodies = fixture.script.bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2, "both tasks must reach the model");
    let first = bodies[0].to_string();
    let second = bodies[1].to_string();
    drop(bodies);
    assert!(first.contains(first_instruction));
    assert!(second.contains(second_instruction));
    assert!(!second.contains(first_instruction));
    assert!(!second.contains("first-native-private-result-secret"));
}

#[test]
fn native_overflow_requires_review_without_acknowledging_any_direction() {
    let mut fixture = native_fixture(9);
    assert!(fixture.session.run_task(&fixture.run).is_err());
    assert_eq!(fixture.world.borrow().native_applied_revision, 0);
    assert!(fixture.script.bodies.lock().unwrap().is_empty());
    assert!(!fixture.sent.borrow().iter().any(|request| matches!(
        request,
        CollabRequest::ReportTaskRun {
            update: runtime_collab::TaskRunUpdate::DirectionsApplied { .. },
            ..
        }
    )));
}

#[test]
fn native_unfinished_answer_requests_review_instead_of_claiming_completion() {
    let mut fixture = native_fixture(0);
    *fixture.script.turns.lock().unwrap() =
        vec![turn(&[finish("I need the owner's decision", false)], 24)].into();
    fixture.session.run_task(&fixture.run).unwrap();
    assert_eq!(
        fixture.script.bodies.lock().unwrap().len(),
        1,
        "must exercise the actual model request"
    );
    let sent = fixture.sent.borrow();
    let last = sent
        .iter()
        .rev()
        .find_map(|request| match request {
            CollabRequest::ReportTaskRun { update, .. } => Some(update),
            _ => None,
        })
        .unwrap();
    assert!(
        matches!(last, runtime_collab::TaskRunUpdate::Failed { message } if message == "I need the owner's decision"),
        "unfinished native work reported {last:?}"
    );
}

/// Every public note the run reported, in order.
fn notes(sent: &[CollabRequest]) -> Vec<String> {
    sent.iter()
        .filter_map(|request| match request {
            CollabRequest::ReportTaskRun {
                update:
                    runtime_collab::TaskRunUpdate::Progress {
                        message,
                        presentation: None,
                    },
                ..
            } => Some(message.clone()),
            _ => None,
        })
        .collect()
}

/// A run whose issue asked for a model, with none set up: it says which
/// model is missing and where to choose one, and never lets that read as
/// a permission the person could go and grant.
#[test]
fn a_run_with_no_model_says_so_and_never_blames_permissions() {
    let mut f = fixture("zzz", World::default(), Vec::new());
    f.session.chooser = Chooser::new(false, Duration::from_secs(5));
    f.session.run_task(&f.run).unwrap();
    let sent = f.sent.borrow();
    let notes = notes(&sent);
    assert!(
        notes
            .iter()
            .any(|note| note.contains("No model is set up") && note.contains("Agent models")),
        "the run announces the missing model where it decides: {notes:?}"
    );
    let runtime_collab::TaskRunUpdate::Failed { message } = terminal(&sent) else {
        panic!("a run with nothing to do fails: {:?}", terminal(&sent));
    };
    assert!(message.contains("No model is set up"), "{message}");
    assert!(message.contains("Agent models"), "{message}");
    assert!(
        !message.to_lowercase().contains("permission"),
        "a missing model never reads as a permission: {message}"
    );
}

/// A run a model answered reports what it cost on the run's own thread:
/// the provider and model that spent it, and the tokens they counted.
#[test]
fn a_model_run_reports_what_it_spent_and_which_model_spent_it() {
    let mut f = fixture(
        "make it bold",
        World::default(),
        vec![turn(&[finish("Done.", true)], 32)],
    );
    f.session.run_task(&f.run).unwrap();
    let sent = f.sent.borrow();
    let notes = notes(&sent);
    assert!(
        notes
            .iter()
            .any(|note| note.starts_with("Planning with scripted from ")),
        "the run names the model that plans it: {notes:?}"
    );
    assert!(
        notes
            .iter()
            .any(|note| note.contains("answered 1 model turn")),
        "the run leaves what it spent in its record: {notes:?}"
    );
}

/// The terminal task report.
fn terminal(sent: &[CollabRequest]) -> runtime_collab::TaskRunUpdate {
    sent.iter()
        .rev()
        .find_map(|request| match request {
            CollabRequest::ReportTaskRun { update, .. }
                if !matches!(update, runtime_collab::TaskRunUpdate::Progress { .. }) =>
            {
                Some(update.clone())
            }
            _ => None,
        })
        .expect("a terminal report")
}

fn tool_names(body: &Value) -> Vec<String> {
    body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap().to_owned())
        .collect()
}

/// Review test 7: "make the second paragraph bold" is carried out by
/// discovering, invoking and observing published operations — read the
/// document, select the paragraph (which makes toggle-mark available),
/// toggle the mark — with no application-specific tool anywhere.
#[test]
fn making_the_second_paragraph_bold_selects_it_then_toggles_the_mark() {
    let world = World {
        blocks: vec![
            ("p1".into(), "Intro".into()),
            ("p2".into(), "Details".into()),
        ],
        ..World::default()
    };
    let mut f = fixture(
        "make the second paragraph bold",
        world,
        vec![
            turn(&[invoke("read-text", &serde_json::json!({}))], 16),
            turn(
                &[invoke("select-block", &serde_json::json!({"block": "p2"}))],
                16,
            ),
            turn(
                &[
                    invoke("toggle-mark", &serde_json::json!({"mark": "strong"})),
                    finish("Done: the second paragraph is bold now.", true),
                ],
                16,
            ),
        ],
    );
    f.session.run_task(&f.run).unwrap();

    let world = f.world.borrow();
    assert_eq!(world.bold, vec!["p2".to_string()]);
    assert_eq!(
        world
            .steps
            .iter()
            .map(|(action, _)| action.as_str())
            .collect::<Vec<_>>(),
        ["read-text", "select-block", "toggle-mark"]
    );
    let sent = f.sent.borrow();
    let invocations = sent
        .iter()
        .filter(|request| matches!(request, CollabRequest::InvokeTaskAction { .. }))
        .count();
    assert_eq!(invocations, 3, "one invocation each");
    assert_eq!(
        terminal(&sent),
        runtime_collab::TaskRunUpdate::Succeeded {
            message: "Done: the second paragraph is bold now.".into()
        }
    );

    let bodies = f.script.bodies.lock().unwrap().clone();
    assert_eq!(
        bodies.len(),
        3,
        "three turns: read, select, toggle + finish"
    );
    for body in &bodies {
        assert_eq!(
            tool_names(body),
            [
                "list_applications",
                "list_operations",
                "describe_operation",
                "invoke",
                "read_result",
                "tilo",
                "plan"
            ],
            "only generic tools, never an application-specific one"
        );
    }
    let first = bodies[0]["messages"][1]["content"].as_str().unwrap();
    assert!(first.contains("make the second paragraph bold"), "{first}");
    assert!(first.contains(&op("toggle-mark")), "{first}");
    assert!(
        first.contains("currently unavailable: select something first"),
        "{first}"
    );
    // The read-back reaches the next turn, and the selection's effect —
    // toggle-mark becoming available — is observed, not assumed.
    let second = bodies[1]["messages"].to_string();
    assert!(second.contains("Details"), "{second}");
    let third = bodies[2]["messages"].as_array().unwrap();
    let observation = third.last().unwrap()["content"].as_str().unwrap();
    let observation: Value = serde_json::from_str(observation).unwrap();
    assert_eq!(observation["result"], "succeeded");
    assert_eq!(observation["changed_view"]["selection"]["block"], "p2");
    assert!(
        observation["newly_available"]
            .as_array()
            .unwrap()
            .iter()
            .any(|line| line.as_str().unwrap().starts_with(&op("toggle-mark"))),
        "{observation}"
    );
}

/// The last tool result the model was shown in `body`.
fn last_observation(body: &Value) -> Value {
    let content = body["messages"].as_array().unwrap().last().unwrap()["content"]
        .as_str()
        .unwrap();
    serde_json::from_str(content).unwrap()
}

/// A view that changes no operation publishes nothing, and the loop
/// does not wait for it: three reads cost no settle time at all (each
/// used to wait the full [`VIEW_SETTLE`]).
#[test]
fn view_steps_that_change_no_operation_do_not_wait() {
    let world = World {
        blocks: vec![("p1".into(), "Intro".into())],
        ..World::default()
    };
    let mut f = fixture(
        "read the document three times",
        world,
        vec![turn(
            &[
                invoke("read-text", &serde_json::json!({})),
                invoke("read-text", &serde_json::json!({})),
                invoke("read-text", &serde_json::json!({})),
                finish("Read it.", true),
            ],
            64,
        )],
    );
    f.session.run_task(&f.run).unwrap();
    assert_eq!(f.world.borrow().steps.len(), 3);
    assert_eq!(
        SETTLES_RAN_OUT.get(),
        0,
        "no view step may wait for a republication"
    );
    assert!(matches!(
        terminal(&f.sent.borrow()),
        runtime_collab::TaskRunUpdate::Succeeded { .. }
    ));
}

/// Once the complete turn validates, a view step does not wait for a
/// republication when the next call is ready. Here the terminal plan
/// follows the read, so the step costs nothing beyond the stream itself.
#[test]
fn a_view_step_stops_listening_when_the_model_moves_on() {
    let world = World {
        blocks: vec![("p1".into(), "Intro".into())],
        ..World::default()
    };
    let events = turn(
        &[
            invoke("read-text", &serde_json::json!({})),
            finish("Read it.", true),
        ],
        16,
    );
    let mut f = paced_fixture(
        "read the document",
        world,
        vec![events],
        Some(Duration::from_millis(2)),
    );
    f.session.run_task(&f.run).unwrap();
    assert_eq!(f.world.borrow().steps.len(), 1);
    assert_eq!(
        SETTLES_RAN_OUT.get(),
        0,
        "the read may not hold the turn for a republication"
    );
}

/// A republication that reaches the agent after the view step's reply is
/// still taken up when the model has no next call for the loop to run,
/// so what the view unlocked is reported with the step. The step waits
/// for full-turn validation, not merely its streamed call preview.
#[test]
fn a_late_republication_is_taken_while_the_model_is_still_busy() {
    let world = World {
        blocks: vec![
            ("p1".into(), "Intro".into()),
            ("p2".into(), "Details".into()),
        ],
        republish_after_polls: 10,
        ..World::default()
    };
    let selection_turn = turn(
        &[invoke("select-block", &serde_json::json!({"block": "p2"}))],
        128,
    );
    let selection_finished_at = selection_turn.len() - 1;
    let mut f = paced_fixture(
        "make the second paragraph bold",
        world,
        vec![
            selection_turn,
            turn(
                &[
                    invoke("toggle-mark", &serde_json::json!({"mark": "strong"})),
                    finish("Done.", true),
                ],
                128,
            ),
        ],
        Some(Duration::from_millis(100)),
    );
    f.session.run_task(&f.run).unwrap();
    assert_eq!(f.world.borrow().bold, ["p2"]);
    assert!(
        f.world.borrow().steps_at[0] >= selection_finished_at,
        "the selection must not execute from a partial model turn"
    );
    let bodies = f.script.bodies.lock().unwrap().clone();
    let observation = last_observation(&bodies[1]);
    assert!(
        observation["newly_available"]
            .as_array()
            .unwrap()
            .iter()
            .any(|line| line.as_str().unwrap().starts_with(&op("toggle-mark"))),
        "{observation}"
    );
}

/// The open writing `type-text` after it, for the open tests.
fn open_then_type(world: World) -> Fixture {
    let open_writer = format!("broker:{DESKTOP_PEER}:desktop:app:so.tilo.word-processor#open");
    fixture(
        "type rain falls into the document",
        world,
        vec![
            turn(
                &[(
                    tools::INVOKE,
                    serde_json::json!({"operation": open_writer, "parameters": {}}),
                )],
                64,
            ),
            turn(
                &[
                    invoke("type-text", &serde_json::json!({"text": "Rain falls."})),
                    finish("Typed it.", true),
                ],
                64,
            ),
        ],
    )
}

/// An open whose receipt names a window that is already published — the
/// open landed on it — returns at once, instead of waiting the whole
/// [`OPEN_SETTLE`] for a resource id it has never seen.
#[test]
fn an_open_landing_on_a_published_window_returns_at_once() {
    let mut f = open_then_type(World {
        fresh: true,
        writer_opened: true,
        opens: Opens::OntoOpenWindow,
        ..World::default()
    });
    f.session.run_task(&f.run).unwrap();
    assert_eq!(
        SETTLES_RAN_OUT.get(),
        0,
        "the open of a published window waited out its settle"
    );
    let bodies = f.script.bodies.lock().unwrap().clone();
    let observation = last_observation(&bodies[1]);
    assert_eq!(observation["result"], "succeeded");
    assert!(
        observation["note"]
            .as_str()
            .unwrap()
            .contains("in the window it already had"),
        "{observation}"
    );
    assert_eq!(f.world.borrow().blocks[0].1, "Rain falls.");
}

/// A second window of an open application is its own peer and record,
/// even though it publishes the same resource ids as the first: the
/// open sees it on the catalogue that brings it, with no receipt.
#[test]
fn a_second_window_reusing_resource_ids_is_seen_as_opened() {
    let mut f = open_then_type(World {
        fresh: true,
        writer_opened: true,
        ..World::default()
    });
    f.session.run_task(&f.run).unwrap();
    assert_eq!(f.world.borrow().second_window, Some("doc-2"));
    assert_eq!(
        SETTLES_RAN_OUT.get(),
        0,
        "the second window's open waited out its settle"
    );
    let bodies = f.script.bodies.lock().unwrap().clone();
    let observation = last_observation(&bodies[1]);
    assert!(
        observation["note"]
            .as_str()
            .unwrap()
            .contains("is open in a new window"),
        "{observation}"
    );
    let second = format!("broker:{SECOND_PEER}:{DOC}#type-text");
    assert!(
        observation["newly_available"]
            .as_array()
            .unwrap()
            .iter()
            .any(|line| line.as_str().unwrap().starts_with(&second)),
        "{observation}"
    );
}

#[test]
fn a_fresh_open_ends_on_the_window_its_receipt_names() {
    let mut f = open_then_type(World {
        fresh: true,
        opens: Opens::Reported,
        ..World::default()
    });
    f.session.run_task(&f.run).unwrap();
    assert_eq!(SETTLES_RAN_OUT.get(), 0, "the open waited out its settle");
    let bodies = f.script.bodies.lock().unwrap().clone();
    let observation = last_observation(&bodies[1]);
    assert!(
        observation["note"]
            .as_str()
            .unwrap()
            .contains("is open in a new window"),
        "{observation}"
    );
    assert_eq!(f.world.borrow().blocks[0].1, "Rain falls.");
}

#[test]
fn opened_records_reads_the_desktop_receipt() {
    let receipt = serde_json::json!({
        "kind": "desktop.command",
        "command": "1:1",
        "generation": 3,
        "opened": [
            {"app_id": "so.tilo.notes", "record_id": "n-1"},
            {"app_id": "so.tilo.word-processor", "record_id": "doc-9"},
            {"app_id": "so.tilo.word-processor", "record_id": ""}
        ]
    });
    assert_eq!(
        opened_records(Some(&receipt), "so.tilo.word-processor"),
        ["doc-9"]
    );
    assert!(opened_records(Some(&serde_json::json!({})), "so.tilo.notes").is_empty());
    assert!(opened_records(None, "so.tilo.notes").is_empty());
}

#[test]
fn stop_during_the_first_silent_model_turn_does_not_wait_for_provider_output() {
    let mut f = paced_fixture(
        "write a paragraph",
        World {
            cancel_after_checks: 2,
            ..World::default()
        },
        vec![turn(
            &[
                invoke("append-paragraph", &serde_json::json!({"text": "late"})),
                finish("Done", true),
            ],
            24,
        )],
        // The provider stays silent for as long as the test may run, so
        // a run that ends ended without its first event.
        Some(tilo_test_env::hang_bound()),
    );
    f.session.run_task(&f.run).unwrap();
    assert_eq!(f.script.bodies.lock().unwrap().len(), 1);
    assert_eq!(
        f.script.emitted.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "Stop must not wait for the provider's first event"
    );
    assert!(f.world.borrow().steps.is_empty());
    assert!(f.session.turn.is_none());
    assert!(matches!(
        terminal(&f.sent.borrow()),
        runtime_collab::TaskRunUpdate::Cancelled { .. }
    ));
}

/// Not waiting for answers does not weaken cancellation: a poll already
/// out is answered ahead of the next step, so the step waits for it and
/// never reaches the host once the task is cancelled.
#[test]
fn a_cancelled_task_still_stops_before_its_next_step() {
    let mut f = fixture(
        "write two paragraphs",
        World {
            cancel_after_steps: Some(1),
            cancel_delay: Duration::from_millis(100),
            ..World::default()
        },
        vec![
            turn(
                &[invoke(
                    "append-paragraph",
                    &serde_json::json!({"text": "One"}),
                )],
                16,
            ),
            turn(
                &[
                    invoke("append-paragraph", &serde_json::json!({"text": "Two"})),
                    finish("Done", true),
                ],
                16,
            ),
        ],
    );
    f.session.run_task(&f.run).unwrap();

    let world = f.world.borrow();
    assert_eq!(
        world
            .steps
            .iter()
            .map(|(action, arguments)| (action.as_str(), arguments["text"].as_str()))
            .collect::<Vec<_>>(),
        [("append-paragraph", Some("One"))],
        "the step after the cancellation must not run"
    );
    assert!(matches!(
        terminal(&f.sent.borrow()),
        runtime_collab::TaskRunUpdate::Cancelled { .. }
    ));
}

/// One turn does the whole job: the paragraph is written in one step
/// and the reply finishes the task — no second turn.
#[test]
fn a_single_turn_writes_and_finishes() {
    let text = "The ocean covers more than seventy percent of our planet, holding \
                ninety-seven percent of its water and most of its life, from reefs \
                to trenches.";
    let mut f = fixture(
        "write a paragraph about the ocean",
        World::default(),
        vec![turn(
            &[
                invoke("append-paragraph", &serde_json::json!({"text": text})),
                finish("Here you go: a paragraph about the ocean.", true),
            ],
            24,
        )],
    );
    f.session.run_task(&f.run).unwrap();

    assert_eq!(f.script.bodies.lock().unwrap().len(), 1, "one model turn");
    let world = f.world.borrow();
    assert_eq!(world.blocks.len(), 1);
    assert_eq!(world.blocks[0].1, text, "the paragraph lands exactly");
    let actions: Vec<&str> = world
        .steps
        .iter()
        .map(|(action, _)| action.as_str())
        .collect();
    assert_eq!(
        actions,
        ["append-paragraph"],
        "a complete paragraph is one step"
    );
    let sent = f.sent.borrow();
    assert_eq!(
        terminal(&sent),
        runtime_collab::TaskRunUpdate::Succeeded {
            message: "Here you go: a paragraph about the ocean.".into()
        }
    );
}

/// A failed step stops the rest of its turn and keeps the turn's `plan`
/// from finishing; the model reads why and decides again.
#[test]
fn a_failed_step_skips_the_rest_of_its_turn_and_is_observed() {
    let world = World {
        refuse: vec!["append-paragraph".into()],
        ..World::default()
    };
    let mut f = fixture(
        "write hi",
        world,
        vec![
            turn(
                &[
                    invoke("append-paragraph", &serde_json::json!({"text": "hi"})),
                    invoke("read-text", &serde_json::json!({})),
                    finish("Written.", true),
                ],
                64,
            ),
            turn(&[finish("I could not write into the document.", false)], 64),
        ],
    );
    f.session.run_task(&f.run).unwrap();
    assert_eq!(
        f.world
            .borrow()
            .steps
            .iter()
            .map(|(action, _)| action.as_str())
            .collect::<Vec<_>>(),
        ["append-paragraph"],
        "the step after the failure did not run"
    );
    let bodies = f.script.bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), 2);
    let second = bodies[1]["messages"].to_string();
    assert!(second.contains("append-paragraph was refused"), "{second}");
    assert!(second.contains("not run"), "{second}");
    assert!(second.contains("Not finished"), "{second}");
    // Not finishing hands the issue back to its person, with the
    // model's words.
    assert_eq!(
        terminal(&f.sent.borrow()),
        runtime_collab::TaskRunUpdate::Failed {
            message: "I could not write into the document.".into()
        }
    );
}

/// An id no listing showed is answered as an error and never reaches the
/// broker, whatever it names.
#[test]
fn an_unshown_operation_is_refused_without_a_step() {
    let mut f = fixture(
        "do something",
        World::default(),
        vec![
            turn(
                &[(
                    tools::INVOKE,
                    serde_json::json!({"operation": "broker:9:ghost#wipe", "parameters": {}}),
                )],
                64,
            ),
            turn(&[finish("Nothing to do.", true)], 64),
        ],
    );
    f.session.run_task(&f.run).unwrap();
    assert!(f.world.borrow().steps.is_empty());
    let bodies = f.script.bodies.lock().unwrap().clone();
    assert!(
        bodies[1]["messages"]
            .to_string()
            .contains("is not an operation any listing"),
    );
    assert_eq!(
        terminal(&f.sent.borrow()),
        runtime_collab::TaskRunUpdate::Succeeded {
            message: "Nothing to do.".into()
        }
    );
}

#[test]
fn explicit_owner_task_budget_reaches_transport_and_keeps_the_default() {
    let turns = vec![turn(&[finish("Done", true)], 64)];
    let mut unbounded = fixture("finish", World::default(), turns.clone());
    unbounded.session.run_task(&unbounded.run).unwrap();
    assert_eq!(
        unbounded.script.timeouts.lock().unwrap()[0],
        Duration::from_secs(5)
    );
    let mut bounded = execution_fixture(
        "finish",
        World::default(),
        turns,
        None,
        Some(collab_types::task_budget::TaskExecutionBudget {
            turn_ms: 20_000,
            total_ms: 60_000,
        }),
    );
    bounded.session.run_task(&bounded.run).unwrap();
    assert_eq!(
        bounded.script.timeouts.lock().unwrap()[0],
        Duration::from_secs(20)
    );
    assert!(bounded.session.task_deadline.is_none());
}

#[test]
fn explicit_total_task_deadline_stops_silent_transport_before_any_step() {
    let mut f = execution_fixture(
        "write",
        World::default(),
        vec![turn(
            &[
                invoke("append-paragraph", &serde_json::json!({"text":"late"})),
                finish("Done", true),
            ],
            64,
        )],
        // A provider silent for as long as the test may run: only the
        // task's own deadline ends the run.
        Some(tilo_test_env::hang_bound()),
        Some(collab_types::task_budget::TaskExecutionBudget {
            turn_ms: 100,
            total_ms: 100,
        }),
    );
    f.session.run_task(&f.run).unwrap();
    assert_eq!(
        f.script.emitted.load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    assert!(f.world.borrow().steps.is_empty());
    assert!(f.session.turn.is_none());
    assert!(f.session.task_deadline.is_none());
    assert!(
        matches!(terminal(&f.sent.borrow()), runtime_collab::TaskRunUpdate::Failed { message }
        if message.contains("task time budget"))
    );
}

/// A model that never finishes is stopped at the turn bound.
#[test]
fn a_loop_that_never_finishes_stops_at_its_turn_bound() {
    let turns = (0..MAX_LOOP_TURNS + 2)
        .map(|_| turn(&[(tools::LIST_APPLICATIONS, serde_json::json!({}))], 64))
        .collect();
    let mut f = fixture("wander", World::default(), turns);
    f.session.run_task(&f.run).unwrap();
    assert_eq!(f.script.bodies.lock().unwrap().len(), MAX_LOOP_TURNS);
    assert!(matches!(
        terminal(&f.sent.borrow()),
        runtime_collab::TaskRunUpdate::Failed { message } if message.contains("did not finish")
    ));
}

/// Both dialects wait for the validated final batch, execute each call
/// exactly once and return each result under the original call id.
#[test]
fn anthropic_calls_wait_for_the_complete_turn_and_run_only_once() {
    let first = anthropic_turn(
        &[
            invoke("append-paragraph", &serde_json::json!({"text": "Hello"})),
            invoke("read-text", &serde_json::json!({})),
        ],
        8,
    );
    let turn_complete = first
        .iter()
        .position(|event| matches!(event, TurnEvent::Finished(_)))
        .unwrap();
    let mut f = paced_fixture(
        "write hello",
        World::default(),
        vec![first, anthropic_turn(&[finish("Wrote it.", true)], 8)],
        Some(Duration::from_millis(4)),
    );
    f.session.chooser = Chooser::scripted(
        Duration::from_secs(5),
        agent_provider_config::ProviderProtocol::Anthropic,
        Arc::new(Scripted(f.script.clone())),
    );
    f.session.run_task(&f.run).unwrap();

    let world = f.world.borrow();
    assert_eq!(world.blocks[0].1, "Hello");
    let actions = world
        .steps
        .iter()
        .map(|(action, _)| action.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        actions,
        ["append-paragraph", "read-text"],
        "each call ran once"
    );
    assert!(
        world.steps_at[0] >= turn_complete,
        "the first call ran at event {} before the final response ({turn_complete})",
        world.steps_at[0]
    );
    let bodies = f.script.bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), 2);
    assert!(bodies[0]["tools"][0].get("input_schema").is_some());
    let results = bodies[1]["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(results["role"], "user");
    let ids = results["content"]
        .as_array()
        .unwrap()
        .iter()
        .map(|block| {
            assert_eq!(block["type"], "tool_result");
            assert_ne!(block["content"], "Not handled.");
            block["tool_use_id"].as_str().unwrap().to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(ids, ["toolu_00", "toolu_01"]);
    assert_eq!(
        terminal(&f.sent.borrow()),
        runtime_collab::TaskRunUpdate::Succeeded {
            message: "Wrote it.".into()
        }
    );
}

fn invoke_replying(action: &str, arguments: &Value, reply: &str) -> (&'static str, Value) {
    (
        tools::INVOKE,
        serde_json::json!({"operation": op(action), "parameters": arguments, "reply": reply}),
    )
}

/// Anthropic caches only up to a breakpoint, so the catalogue closes a
/// text block of its own with one: the first request of two different
/// tasks is byte-identical through that block (system, tools,
/// catalogue), and the next task reads it back from the cache instead of
/// prefilling it. The request follows in its own block.
#[test]
fn anthropic_tasks_share_the_cached_catalogue_block() {
    let mut f = fixture(
        "ignored",
        World::default(),
        vec![
            anthropic_turn(
                &[invoke_replying(
                    "type-text",
                    &serde_json::json!({"text": "Hello chat."}),
                    "Posted hello.",
                )],
                64,
            ),
            anthropic_turn(&[(tools::LIST_APPLICATIONS, serde_json::json!({}))], 64),
            anthropic_turn(
                &[finish("I typed \"Hello chat.\" into the document.", true)],
                64,
            ),
        ],
    );
    f.session.chooser = Chooser::scripted(
        Duration::from_secs(5),
        agent_provider_config::ProviderProtocol::Anthropic,
        Arc::new(Scripted(f.script.clone())),
    );
    for title in ["post hello", "what did you just do?"] {
        let mut run = f.run.clone();
        run.title = title.into();
        bind_context(&mut run);
        f.session.run_task(&run).unwrap();
    }
    let bodies = f.script.bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), 3);
    // What Anthropic caches at the catalogue's breakpoint, in its render
    // order: tools, system, then the first user message through it.
    let prefix = |body: &Value| {
        serde_json::to_string(&(
            &body["tools"],
            &body["system"],
            &body["messages"][0]["content"][0],
        ))
        .unwrap()
    };
    for body in &bodies {
        assert_eq!(prefix(body), prefix(&bodies[0]));
        // The agent's one breakpoint; the gateway adds the rest.
        assert_eq!(body.to_string().matches("cache_control").count(), 1);
    }
    let first_turn = |body: &Value| body["messages"][0]["content"].as_array().unwrap().clone();
    let (first, second) = (first_turn(&bodies[0]), first_turn(&bodies[1]));
    assert_eq!(first.len(), 2);
    assert_eq!(second.len(), 2);
    let catalogue = &first[0];
    assert_eq!(catalogue["type"], "text");
    assert_eq!(
        catalogue["cache_control"],
        serde_json::json!({"type": "ephemeral"})
    );
    let catalogue = catalogue["text"].as_str().unwrap();
    assert!(
        catalogue.starts_with("Operations published right now:")
            && catalogue.contains(&op("type-text")),
        "{catalogue}"
    );
    assert!(!catalogue.contains("The request:"), "{catalogue}");
    assert!(first[1].get("cache_control").is_none());
    // What the task is and where it is come after the catalogue, in the block that changes.
    let task = first[1]["text"].as_str().unwrap();
    assert!(task.starts_with("Limits: at most"), "{task}");
    assert!(task.ends_with("The request:\n\npost hello"), "{task}");
    let task = second[1]["text"].as_str().unwrap();
    assert!(
        task.ends_with("The request:\n\nwhat did you just do?"),
        "{task}"
    );
    // Within a task, the first turn is replayed unchanged.
    assert_eq!(bodies[2]["messages"][0], bodies[1]["messages"][0]);
}

fn post(message: &str) -> (&'static str, Value) {
    (
        tools::INVOKE,
        serde_json::json!({
            "operation": format!("broker:{CHAT_PEER}:chat.channel:general#post"),
            "parameters": {"message": message}
        }),
    )
}

/// Lab regression (fresh workspace, Writer closed): the summary shows
/// the desktop's open for Writer; the model opens it (with a premature
/// reply, as gpt-oss does), the result lists Writer's operations with
/// their ids, and the model writes with an id copied without its
/// routing prefix and posts in Chat — all within three turns.
#[test]
fn a_fresh_workspace_opens_writer_then_writes_and_posts() {
    let open_writer = format!("broker:{DESKTOP_PEER}:desktop:app:so.tilo.word-processor#open");
    let paragraph = "Thunderstorms form when warm, moist air rises quickly.";
    let mut f = fixture(
        "Add a paragraph about thunderstorms to Writer, then post 'hello' in Chat.",
        World {
            fresh: true,
            ..World::default()
        },
        vec![
            turn(
                &[(
                    tools::INVOKE,
                    serde_json::json!({
                        "operation": open_writer,
                        "parameters": {},
                        "parts": [
                            {"part": "add a paragraph about thunderstorms",
                             "app": "so.tilo.word-processor"},
                            {"part": "post hello", "app": "so.tilo.chat"}
                        ],
                        "reply": "Writer opened."
                    }),
                )],
                64,
            ),
            turn(
                &[
                    (
                        tools::INVOKE,
                        serde_json::json!({
                            "operation": format!("{DOC}#append-paragraph"),
                            "parameters": {"text": paragraph}
                        }),
                    ),
                    post("hello"),
                    finish("Added the paragraph and posted hello.", true),
                ],
                64,
            ),
        ],
    );
    f.session.run_task(&f.run).unwrap();
    let bodies = f.script.bodies.lock().unwrap().clone();
    let first = bodies[0]["messages"].to_string();
    assert!(first.contains(&open_writer), "the open is in the summary");
    assert!(!first.contains("#append-paragraph"), "Writer is closed");
    let second = bodies[1]["messages"].to_string();
    assert!(
        second.contains(&op("append-paragraph")),
        "the open's result lists Writer's ids: {second}"
    );
    assert!(second.contains("is open in a new window"), "{second}");
    assert!(
        second.contains("Not finished"),
        "the premature reply is refused"
    );
    assert_eq!(bodies.len(), 2);
    drop(bodies);
    let world = f.world.borrow();
    assert_eq!(world.blocks.len(), 1);
    assert_eq!(world.blocks[0].1, paragraph);
    assert_eq!(world.posts, ["hello"]);
    drop(world);
    assert_eq!(
        terminal(&f.sent.borrow()),
        runtime_collab::TaskRunUpdate::Succeeded {
            message: "Added the paragraph and posted hello.".into()
        }
    );
}

/// Live regression (9997): the first turn opens Writer with a reply and
/// lists only "Open Writer" as a part. The reply on an open does not
/// finish; a following bare finish is refused, because the open part
/// and the request's own mention of Writer both need a Writer
/// operation; only after writing does the task finish.
#[test]
fn an_open_with_a_reply_does_not_finish_the_task() {
    let open_writer = format!("broker:{DESKTOP_PEER}:desktop:app:so.tilo.word-processor#open");
    let mut f = fixture(
        "Write a short essay about rain in Writer.",
        World {
            fresh: true,
            ..World::default()
        },
        vec![
            turn(
                &[(
                    tools::INVOKE,
                    serde_json::json!({
                        "operation": open_writer,
                        "parameters": {},
                        "parts": [{"part": "Open Writer", "app": "so.tilo.desktop: Writer"}],
                        "reply": "Opening Writer to write the essay."
                    }),
                )],
                64,
            ),
            turn(&[finish("Opened Writer.", true)], 64),
            turn(
                &[invoke_replying(
                    "type-text",
                    &serde_json::json!({"text": "Rain falls."}),
                    "Wrote the essay.",
                )],
                64,
            ),
        ],
    );
    f.session.run_task(&f.run).unwrap();
    let bodies = f.script.bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), 3);
    let second = bodies[1]["messages"].to_string();
    assert!(second.contains("does not finish a task"), "{second}");
    let third = bodies[2]["messages"].to_string();
    assert!(third.contains("Not finished: these parts"), "{third}");
    assert!(third.contains("so.tilo.word-processor"), "{third}");
    drop(bodies);
    assert_eq!(f.world.borrow().blocks[0].1, "Rain falls.");
    assert_eq!(
        terminal(&f.sent.borrow()),
        runtime_collab::TaskRunUpdate::Succeeded {
            message: "Wrote the essay.".into()
        }
    );
}

/// An `invoke` of `action` on `resource` in `peer`'s record; marked
/// independent of the steps before it when `independent`.
fn invoke_on(
    peer: u64,
    resource: &str,
    action: &str,
    arguments: &Value,
    independent: bool,
) -> (&'static str, Value) {
    let mut input = serde_json::json!({
        "operation": format!("broker:{peer}:{resource}#{action}"),
        "parameters": arguments,
    });
    if independent {
        input[tools::INVOKE_STANDALONE] = Value::Bool(true);
    }
    (tools::INVOKE, input)
}

/// Three steps on three different records: a Chat post and a paragraph
/// in each of two Writer windows.
fn three_records(independent: bool) -> Vec<(&'static str, Value)> {
    vec![
        invoke_on(
            CHAT_PEER,
            "chat.channel:general",
            "post",
            &serde_json::json!({"message": "Standup moved to 10."}),
            independent,
        ),
        invoke_on(
            PEER,
            DOC,
            "append-paragraph",
            &serde_json::json!({"text": "Agenda"}),
            independent,
        ),
        invoke_on(
            SECOND_PEER,
            DOC,
            "append-paragraph",
            &serde_json::json!({"text": "Minutes"}),
            independent,
        ),
    ]
}

/// The tool results a turn's answer carried back to the model, in the
/// order they were sent: `(call id, result)`.
fn tool_results(body: &Value) -> Vec<(String, Value)> {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "tool")
        .map(|message| {
            (
                message["tool_call_id"].as_str().unwrap().to_owned(),
                serde_json::from_str(message["content"].as_str().unwrap()).unwrap(),
            )
        })
        .collect()
}
/// A run's issue lists what it looked up, and only that: a read of a
/// resource the request did not name, listed once. Changing something is not
/// a lookup, the resource the request is about was given rather than found,
/// and reading again what is already listed adds nothing.
#[test]
fn only_a_first_read_of_a_resource_past_the_request_is_listed_as_a_lookup() {
    let descriptor = |resource: &str| tilo_actions::ActionDescriptor {
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        address: tilo_actions::ActionAddress::new(resource, "read"),
        resource_kind: "notes.page".into(),
        resource_label: "Release notes".into(),
        action_label: "Read".into(),
        keywords: Vec::new(),
        icon: None,
        description: None,
        locus: tilo_actions::ExecutionLocus::ClientLocal,
        kind: tilo_actions::OperationKind::View,
        contract: tilo_actions::ActionContract::default(),
        params: Vec::new(),
        input: tilo_actions::InputSpec::None,
        view: None,
    };
    let mut run = runtime_collab::TaskRunEnvelope {
        paused: None,
        model: None,
        version: 1,
        task_id: "tasks.tracker/issues/loop".into(),
        title: "Say what changed".into(),
        instruction: None,
        subject: Some(tilo_actions::SelectionSubject {
            app_id: "so.tilo.notes".into(),
            app_label: "Notes".into(),
            resource: tilo_actions::ActionResource {
                id: "notes.page:given".into(),
                kind: "notes.page".into(),
                label: "The page asked about".into(),
            },
            anchor: tilo_actions::selection::SelectionAnchor::default(),
        }),
        run_id: 7,
        lease: "native:00000000000000000000000000000000:0".into(),
        planning: runtime_collab::TaskRunPlanning::Model,
        presentation_version: Some(1),
        execution_budget: None,
        action_context: serde_json::Value::Null,
    };
    let empty: Vec<String> = Vec::new();
    assert!(
        looked_up(&run, &descriptor("notes.page:release"), true, &empty),
        "a first read past the request is a lookup"
    );
    assert!(
        !looked_up(&run, &descriptor("notes.page:release"), false, &empty),
        "an operation that is not a read looks nothing up"
    );
    assert!(
        !looked_up(&run, &descriptor("notes.page:given"), true, &empty),
        "what the person gave the run was not found"
    );
    let already = ["notes.page:release".to_owned()];
    assert!(
        !looked_up(&run, &descriptor("notes.page:release"), true, &already),
        "a resource it already listed is not listed again"
    );
    // An empty subject (a task asked in words alone) leaves every read a
    // lookup; there is no given resource to exclude.
    run.subject = None;
    assert!(looked_up(
        &run,
        &descriptor("notes.page:release"),
        true,
        &empty
    ));
}

#[path = "agent_loop_run_tests.rs"]
mod run;
