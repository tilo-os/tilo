//! The agent loop's tests: what a run says about itself, and what it does
//! when it cannot do what it was asked.

use super::*;
use crate::model::{Chooser, TurnRequest, TurnTransport};
use crate::session::Config;
use std::sync::{Arc, Mutex};
use std::{cell::RefCell, rc::Rc};
use tilo_actions::{ActionContract, ActionParam, Availability, ParamType};
use ui::collaboration::CollabRequest;
use ui::collaboration::{
    ActionCatalogue, BrokerActionSurface, CatalogueChange, ExportedAction, ExportedInput,
};

/// The catalogue a follower holds once it was sent `surfaces`.
pub(super) fn catalogue_of(surfaces: &[BrokerActionSurface]) -> ActionCatalogue {
    let mut catalogue = ActionCatalogue::default();
    catalogue
        .apply(&CatalogueChange::snapshot(surfaces))
        .expect("a snapshot applies to any catalogue");
    catalogue
}
use uuid::Uuid;

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

/// One gateway turn calling `calls` (`(name, arguments)`), streamed as
/// argument deltas of `piece` bytes and completed-call frames, then the
/// assembled provider-neutral response. Call ids are never reused, as a
/// provider's are not: the neutral conversation refuses a consumed id.
fn turn(calls: &[(&str, Value)], piece: usize) -> Vec<TurnEvent> {
    static NEXT_CALL: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let mut events = Vec::new();
    let mut content = Vec::new();
    for (index, (name, arguments)) in calls.iter().enumerate() {
        let id = format!(
            "call_{}",
            NEXT_CALL.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
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
                    "index": index, "id": &id, "name": name, "delta": &text[start..end]
                })
                .to_string(),
            });
            start = end;
        }
        events.push(TurnEvent::Frame {
            event: tilo_model_client::EVENT_TOOL_CALL.into(),
            output: serde_json::json!({"id": &id, "name": name, "arguments": text}).to_string(),
        });
        content.push(serde_json::json!({
            "type": "tool_call", "id": id, "name": name, "input": arguments
        }));
    }
    events.push(TurnEvent::Finished(Ok(serde_json::json!({
        "message": {"role": "assistant", "content": content},
        "stop_reason": "tool_calls"
    })
    .to_string())));
    events
}

/// One provider-neutral turn that answers in words and calls nothing, as a
/// chat-style model ends a turn: its text streams as deltas, then the
/// assembled response with no tool call and `end_turn`.
fn said_turn(text: &str) -> Vec<TurnEvent> {
    let response = serde_json::json!({
        "message": {"role": "assistant", "content": [{"type": "text", "text": text}]},
        "stop_reason": "end_turn"
    });
    vec![
        TurnEvent::Neutral(agent_provider_config::protocol::StreamEvent::TextDelta {
            text: text.into(),
        }),
        TurnEvent::Neutral(agent_provider_config::protocol::StreamEvent::Finished {
            response: serde_json::from_value(response.clone()).unwrap(),
        }),
        TurnEvent::Finished(Ok(response.to_string())),
    ]
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
const CHAT_RECORD_PEER: u64 = 29;
const DESKTOP_PEER: u64 = 9;
const SECOND_PEER: u64 = 10;
const BROWSER_PEER: u64 = 11;
/// The Browser tab the fixture's Browser publishes its page actions on.
const BROWSER_TAB: &str = "browser.tab:1";
const DOC: &str = "word-processor.document";

fn descriptor(
    action: &str,
    label: &str,
    kind: tilo_actions::OperationKind,
    params: Vec<ActionParam>,
    availability: Availability,
    tags: Vec<String>,
) -> ExportedAction {
    ExportedAction {
        did: None,
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
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent fact about the world a run meets, not a state machine"
)]
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
    /// The person stops the turn once the agent's link polled the host this many times (zero
    /// never).
    cancel_after_polls: usize,
    /// How many times the agent's link polled the host.
    polls: usize,
    /// The person stops the turn once this many steps have been applied.
    cancel_after_steps: Option<usize>,
    /// The host told the agent the turn stopped.
    turn_stopped: bool,
    /// How the desktop's open of Writer behaves.
    opens: Opens,
    /// The record of a second Writer window (its own peer and record,
    /// the same resource ids), once an open added one.
    second_window: Option<&'static str>,
    /// A republished catalogue reaches the agent this many link polls
    /// after the step's result instead of with it.
    republish_after_polls: usize,
    /// Chat publishes the conversation's message only this many link polls after the run started:
    /// the agent's first catalogue holds none of it.
    chat_late: Option<usize>,
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
    /// An action that asks its person first, and how many of the agent's link polls the person
    /// takes to allow it.
    awaiting: Option<(String, usize)>,
    /// A mailbox of this many letters published beside the document, each
    /// read whole by `read-letter` (a host-owned read, which no view
    /// settles after); zero publishes none.
    letters: usize,
    /// A Browser is published, and a capture of the page answers with this
    /// content address; `None` publishes neither.
    browser: Option<String>,
    /// The workspace also runs tilolang Board and Views, whose hosts take programs,
    /// and the conversation's reply lists the run's steps.
    tilolang: bool,
    /// What each `run-block` answers, by a word its source holds.
    tl_blocks: Vec<(&'static str, Value)>,
    /// What the API answers each task program, by a word the program holds.
    tl_programs: Vec<(&'static str, Value)>,
    /// A conversation whose message this is: its reply is where the run
    /// records its turn, so `None` records nothing.
    chat: Option<String>,
    /// Chat is closed: the workspace keeps the offers it last published, none of them
    /// a message it publishes now.
    chat_closed: bool,
    /// The turn's message also offers Chat's `keep-summary`, as a session that answers turns
    /// publishes it.
    summary: bool,
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
                document_id: "doc-1".into(),
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
                document_id: String::new(),
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
        if self.letters > 0 {
            surfaces[0].actions.push(descriptor(
                "read-letter",
                "Read a letter",
                tilo_actions::OperationKind::Effect,
                vec![ActionParam::required(
                    "letter",
                    ParamType::Integer,
                    "Which letter",
                )],
                Availability::Available,
                vec![],
            ));
        }
        if self.browser.is_some() {
            let tab = |action: &str, label: &str, params: Vec<ActionParam>| ExportedAction {
                resource: BROWSER_TAB.into(),
                resource_kind: "browser.tab".into(),
                resource_label: "Jane Doe".into(),
                ..descriptor(
                    action,
                    label,
                    tilo_actions::OperationKind::Effect,
                    params,
                    Availability::Available,
                    vec![],
                )
            };
            surfaces.push(BrokerActionSurface {
                peer_id: BROWSER_PEER,
                provider: None,
                app_id: "so.tilo.browser".into(),
                cluster_id: Uuid::from_u128(1).to_string(),
                document_id: String::new(),
                live: true,
                actions: vec![
                    tab(
                        "navigate",
                        "Go to an address",
                        vec![ActionParam::required("url", ParamType::Text, "The address")],
                    ),
                    tab("read-page", "Read the page", vec![]),
                    tab("capture-page", "Capture the page", vec![]),
                ],
            });
        }
        if self.tilolang {
            surfaces.extend(programs::surfaces());
        }
        // A conversation keeps every message it holds: one whose `record-work`
        // the catalogue already offers, beside the one this run answers. Only
        // the latter may be republished late, so the run's wait for it can be
        // told from a workspace whose Chat publishes nothing at all.
        // A closed Chat keeps the offers it last published, as the broker retains them
        // until the app runs again.
        let live = !self.chat_closed;
        let keeps_summary = self.summary;
        let conversation = |record: &'static str, message: &str| BrokerActionSurface {
            peer_id: CHAT_RECORD_PEER,
            provider: (!live).then(|| {
                format!(
                    "launchable-actions:{}:so.tilo.chat:{record}",
                    Uuid::from_u128(1)
                )
            }),
            app_id: "so.tilo.chat".into(),
            cluster_id: Uuid::from_u128(1).to_string(),
            document_id: record.into(),
            live,
            actions: vec![ExportedAction {
                state: tilo_actions::ActionState::default(),
                semantic_identity: None,
                resource: format!("chat.messages/messages/{message}"),
                resource_kind: "chat.messages".into(),
                resource_label: "The conversation".into(),
                ..descriptor(
                    "record-work",
                    "Record work",
                    tilo_actions::OperationKind::Document,
                    vec![
                        ActionParam::required("message", ParamType::Text, "The message"),
                        ActionParam::required("work", ParamType::Object, "How the work goes"),
                        ActionParam::optional("say", ParamType::Text, "The reply's words"),
                    ],
                    Availability::Available,
                    vec![],
                )
            }]
            .into_iter()
            .chain(keeps_summary.then(|| ExportedAction {
                state: tilo_actions::ActionState::default(),
                semantic_identity: None,
                resource: format!("chat.messages/messages/{message}"),
                resource_kind: "chat.messages".into(),
                resource_label: "The conversation".into(),
                ..descriptor(
                    "keep-summary",
                    "Keep a summary",
                    tilo_actions::OperationKind::Document,
                    vec![
                        ActionParam::required("through", ParamType::Text, "Its last message"),
                        ActionParam::required("through_ts", ParamType::Integer, "When"),
                        ActionParam::required("text", ParamType::Text, "The summary"),
                        ActionParam::required("conversation", ParamType::Text, "The conversation"),
                    ],
                    Availability::Available,
                    vec![],
                )
            }))
            .collect(),
        };
        if let Some(message) = &self.chat {
            surfaces.push(conversation("chat-1", message));
            if self.chat_late.is_some() {
                surfaces.push(conversation("chat-2", "earlier"));
            }
        }
        if let Some(record) = self.second_window {
            surfaces.push(BrokerActionSurface {
                peer_id: SECOND_PEER,
                document_id: record.into(),
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
                document_id: String::new(),
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
                    "document_id": record,
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
        // The conversation's reply takes the record; it is no step of the
        // document the run works in.
        if action == "record-work" || action == "keep-summary" {
            return (serde_json::json!({"succeeded": {}}), false);
        }
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
            "navigate" => (
                serde_json::json!({"succeeded": {"value": {"url": text("url"), "title": "Jane Doe"}}}),
                false,
            ),
            "read-page" => (
                serde_json::json!({"succeeded": {"value": {
                    "title": "Jane Doe",
                    "text": "Jane Doe. Projects: a CRDT text editor, a Rust tracing library."
                }}}),
                false,
            ),
            "capture-page" => (
                serde_json::json!({"succeeded": {"resources": [self.browser.clone()]}}),
                false,
            ),
            "fetch" => (
                serde_json::json!({"succeeded": {"value": {"text": "Jane Doe."}}}),
                false,
            ),
            "read-letter" => {
                let letter = arguments.get("letter").and_then(Value::as_u64).unwrap_or(0);
                (
                    serde_json::json!({"succeeded": {"value": {
                        "letter": letter,
                        "text": format!(
                            "Candidate letter {letter}: {}",
                            "I am applying for the role and attach my CV and a link to my \
                             work. "
                                .repeat(40)
                        )
                    }}}),
                    false,
                )
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
                    "type": "WrittenView", "id": "root/items:v1", "label": text("title")
                }}}),
                false,
            ),
            "record-work" | "place" | "position" | "close" | "activate" | "show-style" => {
                (serde_json::json!({"succeeded": {}}), false)
            }
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
    /// Asks about running steps the host holds: the step, what the agent had seen, and when the
    /// hold ends.
    holding: Vec<HeldReask>,
    /// A catalogue still on its way, and the polls until it arrives.
    late: Option<(usize, CollabEvent)>,
    /// Wakes the agent's link when this host has more to deliver, so the
    /// agent's waits sleep between deliveries instead of polling for them.
    wake: crate::wire::TestWake,
    /// When a wake is already on its way, so one due time is rung once.
    wake_at: Option<ui::time::Instant>,
}

/// An ask the fake host holds until its step moves on.
struct HeldReask {
    invocation: tilo_actions::InvocationId,
    seen: tilo_actions::remote::UpdateMark,
    until: ui::time::Instant,
}

/// The handle the fixture hands the agent its turn under.
const TURN: u64 = 7;

/// The host's message `message`, as the agent's connection reads it.
fn remote(message: tilo_actions::remote::RemoteActionMessage) -> CollabEvent {
    CollabEvent::RemoteAction { message }
}

/// The host's answer about a step: its state now.
fn step_update(update: tilo_actions::InvocationUpdate) -> CollabEvent {
    remote(tilo_actions::remote::RemoteActionMessage::Update {
        invocation: update.invocation(),
        state: Some(tilo_actions::remote::InvocationState {
            update,
            untrusted: false,
            left_at: None,
        }),
    })
}

/// The host's receipt for the step `invocation`.
fn step_receipt(
    invocation: tilo_actions::InvocationId,
    status: tilo_actions::remote::ReceiptStatus,
) -> CollabEvent {
    remote(tilo_actions::remote::RemoteActionMessage::Receipt(
        tilo_actions::remote::InvocationReceipt { invocation, status },
    ))
}

/// The receipt that says `update`.
fn status_of(update: tilo_actions::InvocationUpdate) -> tilo_actions::remote::ReceiptStatus {
    use tilo_actions::remote::ReceiptStatus;
    match update {
        tilo_actions::InvocationUpdate::Succeeded { output, .. } => {
            ReceiptStatus::Completed { output }
        }
        tilo_actions::InvocationUpdate::Failed { error, .. } => ReceiptStatus::Failed { error },
        tilo_actions::InvocationUpdate::Cancelled { message, .. } => {
            ReceiptStatus::Cancelled { message }
        }
        _ => ReceiptStatus::Accepted,
    }
}

/// The step a request starts, when it starts one: its invocation, action and arguments.
fn invoked(
    request: &CollabRequest,
) -> Option<(
    tilo_actions::InvocationId,
    &str,
    serde_json::Map<String, Value>,
    bool,
)> {
    let CollabRequest::RemoteAction {
        message: tilo_actions::remote::RemoteActionMessage::Invoke(invoke),
    } = request
    else {
        return None;
    };
    let arguments = match &invoke.argument {
        tilo_actions::remote::InvokeArgument::Json(arguments) => arguments.clone(),
        _ => serde_json::Map::new(),
    };
    Some((
        invoke.invocation,
        invoke.target.address.action.as_str(),
        arguments,
        invoke.confirmation.is_some(),
    ))
}

/// What the turn reported, as the agent's answer sent it, read back as the agent tells it.
fn reported(sent: &[CollabRequest]) -> Vec<TurnUpdate> {
    sent.iter()
        .filter_map(|request| match request {
            CollabRequest::UpdateInvocation { id, update } if *id == TURN => Some(update),
            _ => None,
        })
        .filter_map(|update| match update {
            tilo_actions::InvocationUpdate::Progress { message, .. } => {
                Some(TurnUpdate::Progress {
                    message: message.clone().unwrap_or_default(),
                })
            }
            tilo_actions::InvocationUpdate::Item { value, .. } => Some(TurnUpdate::Spent {
                cost_micros: value["spent"].as_u64().unwrap_or_default(),
            }),
            tilo_actions::InvocationUpdate::Succeeded { output, .. } => {
                Some(TurnUpdate::Succeeded {
                    message: output
                        .value
                        .as_ref()
                        .and_then(|value| value["reply"].as_str())
                        .unwrap_or_default()
                        .to_owned(),
                })
            }
            tilo_actions::InvocationUpdate::Failed { error, .. } => Some(TurnUpdate::Failed {
                message: error.message.clone(),
            }),
            tilo_actions::InvocationUpdate::Cancelled { message, .. } => {
                Some(TurnUpdate::Cancelled {
                    message: message.clone().unwrap_or_default(),
                })
            }
            tilo_actions::InvocationUpdate::Accepted { .. } => None,
        })
        .collect()
}

impl Broker {
    /// Deliver a host answer after `delay` and `polls` more link polls.
    fn answer_after(&mut self, delay: Duration, polls: usize, event: CollabEvent) {
        self.held.push((ui::time::now() + delay, polls, event));
    }

    /// Ask the agent to stop its turn once the world says the person stopped it, as the host does
    /// when the turn ends: once, ahead of anything the host answers after.
    fn stop_when_due(&mut self) {
        let mut world = self.world.borrow_mut();
        if world.turn_stopped {
            return;
        }
        world.polls += 1;
        let due = (world.cancel_after_polls != 0 && world.polls > world.cancel_after_polls)
            || world
                .cancel_after_steps
                .is_some_and(|steps| world.steps.len() >= steps);
        if due {
            world.turn_stopped = true;
            self.events
                .push(CollabEvent::InvocationCancellation { id: TURN });
        }
    }
}

impl ui::collaboration::PeerTransport for Broker {
    fn is_connected(&self) -> bool {
        true
    }
    fn poll(&mut self) -> Vec<CollabEvent> {
        self.stop_when_due();
        let now = ui::time::now();
        let (due, later) = std::mem::take(&mut self.held)
            .into_iter()
            .map(|(at, polls, event)| (at, polls.saturating_sub(1), event))
            .partition::<Vec<_>, _>(|(at, polls, _)| *at <= now && *polls == 0);
        self.held = later;
        self.events
            .extend(due.into_iter().map(|(_, _, event)| event));
        self.answer_held_reasks(now);
        self.wake_when_due(now);
        if let Some((polls, event)) = self.late.take() {
            if polls == 0 {
                self.events.push(event);
            } else {
                self.late = Some((polls - 1, event));
                // Another poll still has something to deliver: say so now,
                // so the link reads it at once rather than on a timer.
                self.wake.ring();
            }
        }
        std::mem::take(&mut self.events)
    }
    fn send(&mut self, request: CollabRequest) -> Result<(), String> {
        use tilo_actions::remote::RemoteActionMessage;
        match &request {
            CollabRequest::RemoteAction {
                message:
                    RemoteActionMessage::Observe {
                        invocation,
                        wait_ms,
                        seen,
                    },
            } => self.observe_child(*invocation, *wait_ms, *seen),
            CollabRequest::RemoteAction {
                message: RemoteActionMessage::Confirm { correlation, .. },
            } => {
                // The person decides on the sheet, in their own time: the answer arrives this many
                // polls later, allowing the step as asked.
                let polls = self
                    .world
                    .borrow()
                    .awaiting
                    .as_ref()
                    .map_or(0, |(_, polls)| *polls);
                self.answer_after(
                    Duration::ZERO,
                    polls,
                    remote(RemoteActionMessage::ConfirmResponse {
                        correlation: *correlation,
                        result: Ok(Box::new(
                            tilo_actions::remote::ConfirmedInvocation::unchanged(
                                tilo_actions::remote::Confirmation::Witnessed("allowed".into()),
                            ),
                        )),
                    }),
                );
            }
            CollabRequest::RemoteAction {
                message: RemoteActionMessage::Cancel { invocation },
            } => self.cancel_child(*invocation),
            CollabRequest::RemoteAction {
                message: RemoteActionMessage::Invoke(_),
            } => {
                let Some((invocation, action, arguments, confirmed)) = invoked(&request) else {
                    unreachable!("an invoke names its step");
                };
                let action = action.to_owned();
                let asks_first = self
                    .world
                    .borrow()
                    .awaiting
                    .as_ref()
                    .is_some_and(|(waiting, _)| *waiting == action);
                if self.world.borrow().turn_stopped {
                    // The person stopped the turn: the host serves none of its steps from then on.
                    self.events.push(step_receipt(
                        invocation,
                        tilo_actions::remote::ReceiptStatus::Rejected {
                            error: tilo_actions::remote::ActionHostError::Refused {
                                reason: "the turn this step answers is no longer going".into(),
                            },
                        },
                    ));
                } else if asks_first && !confirmed {
                    self.events.push(step_receipt(
                        invocation,
                        tilo_actions::remote::ReceiptStatus::Rejected {
                            error: tilo_actions::remote::ActionHostError::confirmation_required(),
                        },
                    ));
                } else if self.world.borrow().deny.contains(&action)
                    || self.world.borrow().settle_after.is_some()
                {
                    self.start_child(invocation, &action, &arguments);
                } else {
                    self.apply_at_once(invocation, &action, &arguments);
                }
            }
            CollabRequest::UpdateInvocation { .. } | CollabRequest::Presence { .. } => {}
            other => panic!("unexpected request {other:?}"),
        }
        self.sent.borrow_mut().push(request);
        Ok(())
    }
}

impl Broker {
    /// Run a step in its application at once and answer its outcome in the receipt.
    fn apply_at_once(
        &mut self,
        invocation: tilo_actions::InvocationId,
        action: &str,
        arguments: &serde_json::Map<String, Value>,
    ) {
        let (outcome, republish) = self.world.borrow_mut().apply(action, arguments);
        let update = if let Some(output) = outcome.get("succeeded") {
            tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: serde_json::from_value(output.clone()).unwrap_or_default(),
            }
        } else {
            tilo_actions::InvocationUpdate::Failed {
                invocation,
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
        self.events
            .push(step_receipt(invocation, status_of(update)));
        // Republished after the result, as a real host does: the
        // loop must take it up after a view step.
        if republish {
            let catalogue = CollabEvent::ActionCatalogue {
                change: CatalogueChange::snapshot(&self.world.borrow().surfaces()),
            };
            match self.world.borrow().republish_after_polls {
                0 => self.events.push(catalogue),
                polls => self.late = Some((polls, catalogue)),
            }
        }
    }

    /// Stop a step the agent asked to cancel, as its application does when it has not settled.
    fn cancel_child(&self, invocation: tilo_actions::InvocationId) {
        let now = ui::time::now();
        let mut world = self.world.borrow_mut();
        if let Some((due, terminal)) = world.settling.get_mut(&invocation)
            && *due > now
        {
            *due = now;
            *terminal = tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some("Cancelled".into()),
            };
        }
        world.cancelled.push(invocation);
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

    /// Ring the link when the first answer this host holds becomes due, as a
    /// real host's answer arrives on its own. An answer already due needs no
    /// wake: the next poll reads it.
    fn wake_when_due(&mut self, now: ui::time::Instant) {
        let due = self
            .held
            .iter()
            .map(|(at, _, _)| *at)
            .chain(self.holding.iter().map(|held| held.until))
            // A step of the turn moving on is news of its own.
            .chain(self.world.borrow().settling.values().map(|(due, _)| *due))
            .filter(|at| *at > now)
            .min();
        let Some(due) = due else {
            self.wake_at = None;
            return;
        };
        if self.wake_at == Some(due) {
            return;
        }
        self.wake_at = Some(due);
        let wake = self.wake.clone();
        let wait = due.saturating_duration_since(now);
        std::thread::spawn(move || {
            std::thread::sleep(wait);
            wake.ring();
        });
    }

    /// Answer an ask about a started step with its state: at once, or — when the ask asks to be
    /// held — once the step moves past what the agent saw or the hold ends.
    fn observe_child(
        &mut self,
        invocation: tilo_actions::InvocationId,
        wait_ms: Option<u32>,
        seen: Option<tilo_actions::remote::UpdateMark>,
    ) {
        if wait_ms.is_some() {
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
        let held = wait_ms.zip(seen).filter(|(_, seen)| {
            self.world.borrow().reask_hold != ReaskHold::AnswerAtOnce
                && !update.is_terminal()
                && tilo_actions::remote::UpdateMark::of(&update) == *seen
        });
        match held {
            Some((wait_ms, seen)) => self.holding.push(HeldReask {
                invocation,
                seen,
                until: ui::time::now()
                    + if self.world.borrow().reask_hold == ReaskHold::UntilNews {
                        tilo_test_env::hang_bound()
                    } else {
                        Duration::from_millis(u64::from(wait_ms))
                    },
            }),
            None => self.answer_reask(invocation, seen),
        }
    }

    /// Answer an ask about `invocation` with the step's state now.
    fn answer_reask(
        &mut self,
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
        self.events.push(step_update(update));
    }

    /// Answer every held ask whose step moved on or whose hold ended.
    fn answer_held_reasks(&mut self, now: ui::time::Instant) {
        for held in std::mem::take(&mut self.holding) {
            let update = self.child_state(held.invocation);
            if update.is_terminal()
                || tilo_actions::remote::UpdateMark::of(&update) != held.seen
                || now >= held.until
            {
                self.answer_reask(held.invocation, Some(held.seen));
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

    /// Start a step of the turn as the action broker does: refused
    /// before anything runs when it is not permitted, else accepted at
    /// once and settled in its application `settle_after` later, which
    /// an ask about it then answers.
    fn start_child(
        &mut self,
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
            self.events.push(step_receipt(
                invocation,
                tilo_actions::remote::ReceiptStatus::Rejected {
                    error: tilo_actions::remote::ActionHostError::Refused {
                        reason: format!("permission denied: the agent may not {action}"),
                    },
                },
            ));
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
        self.events.push(step_receipt(
            invocation,
            tilo_actions::remote::ReceiptStatus::Accepted,
        ));
    }
}

struct Fixture {
    session: Session,
    run: crate::session::Asked,
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
    execution_fixture(title, world, turns, pace)
}

/// The catalogue that publishes Chat's message late, when `world` publishes it so.
fn late_chat(world: &World) -> Option<(usize, CollabEvent)> {
    let polls = world.chat_late?;
    let change = CatalogueChange::snapshot(&world.surfaces());
    Some((polls, CollabEvent::ActionCatalogue { change }))
}

/// The surfaces the agent reads at its first turn. When Chat republishes this
/// run's message late, every other message it holds is published already: what
/// is missing is the reply this run records on.
fn first_catalogue(world: &World) -> Vec<BrokerActionSurface> {
    let mut surfaces = world.surfaces();
    let answered = world
        .chat
        .as_ref()
        .filter(|_| world.chat_late.is_some())
        .map(|message| format!("chat.messages/messages/{message}"));
    surfaces.retain(|surface| {
        answered.as_ref().is_none_or(|resource| {
            !surface
                .actions
                .iter()
                .any(|action| &action.resource == resource)
        })
    });
    surfaces
}

/// The link to the fake host, which wakes it when it has news to deliver as a
/// real host does, instead of being polled for it.
fn host_link(
    world: &Rc<RefCell<World>>,
    sent: &Rc<RefCell<Vec<CollabRequest>>>,
) -> crate::wire::Link {
    let (wake, rx) = crate::wire::test_wake();
    crate::wire::test_link(
        Broker {
            world: world.clone(),
            sent: sent.clone(),
            events: Vec::new(),
            held: Vec::new(),
            late: late_chat(&world.borrow()),
            holding: Vec::new(),
            wake,
            wake_at: None,
        },
        rx,
    )
}

fn execution_fixture(
    title: &str,
    world: World,
    turns: Vec<Vec<TurnEvent>>,
    pace: Option<Duration>,
) -> Fixture {
    let world = world;
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
    let link = host_link(&world, &sent);
    let run = asked(title);
    let surfaces = first_catalogue(&world.borrow());
    let session = Session {
        cfg: Config {
            agent_id: "agent-1".into(),
            endpoint: None,
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
            provider: None,
            identity: None,
        },
        link,
        chooser: Chooser::scripted(
            Duration::from_secs(5),
            agent_provider_config::ProviderProtocol::OpenAiCompatible,
            Arc::new(Scripted(script.clone())),
        ),
        children: HashMap::new(),
        reported: 0,
        broker_catalogue: catalogue_of(&surfaces),
        next_broker_request: 1,
        record: None,
        broker_revision: 0,
        catalogue_cache: None,
        declarations: std::collections::BTreeMap::new(),
        turn: None,
        failure_detail: None,
        spending: None,
        web: None,
        single_shot: Arc::new(crate::transform::GatewayModel::default()),
    };
    Fixture {
        session,
        run,
        world,
        sent,
        script,
    }
}

/// The turn `title` asks, answering the conversation's message as the host hands it.
fn asked(title: &str) -> crate::session::Asked {
    let asked = crate::session::Asked {
        id: TURN,
        invocation: tilo_actions::InvocationId(Uuid::from_u128(7)),
        asked: runtime_collab::TurnRequest {
            message: "chat.workspace/channels/loop/messages/asked".into(),
            title: title.into(),
            instruction: title.into(),
            agent_instruction: None,
            fold: None,
            subject: None,
            spend: None,
        },
        context: None,
    };
    super::super::conversation::validate(&asked).unwrap();
    asked
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

#[path = "agent_loop/tests/anthropic_dialect.rs"]
mod anthropic_dialect;
#[path = "agent_loop/tests/catalogue_waits.rs"]
mod catalogue_waits;
#[path = "agent_loop/tests/long_run.rs"]
mod long_run;
#[path = "agent_loop/tests/malformed_calls.rs"]
mod malformed_calls;
#[path = "agent_loop/tests/native_pipeline.rs"]
mod native_pipeline;
#[path = "agent_loop/tests/pictures.rs"]
mod pictures;
#[path = "agent_loop/tests/programs.rs"]
mod programs;
#[path = "agent_loop/tests/spending.rs"]
mod spending;
#[path = "agent_loop/tests/summary.rs"]
mod summary;
#[path = "agent_loop/tests/text_answers.rs"]
mod text_answers;
#[path = "agent_loop/tests/turn_records.rs"]
mod turn_records;
#[path = "agent_loop/tests/unusable_answers.rs"]
mod unusable_answers;
#[path = "agent_loop/tests/web.rs"]
mod web;
#[path = "agent_loop/tests/web_pages.rs"]
mod web_pages;

fn native_fixture() -> Fixture {
    fixture(
        "Prepare release notes",
        World::default(),
        vec![turn(&[finish("Reviewed instructions", true)], 24)],
    )
}

tilo_test_env::scenario! {
    fn a_native_task_reads_its_instruction_before_acting;

    phase native_model_receives_the_full_private_instruction_not_a_truncated_title;
    {
        let mut fixture = native_fixture();
        let instruction =
            "Inspect every release-note section and preserve the complete compatibility matrix. "
                .repeat(6);
        assert!(instruction.chars().count() > 240);
        fixture.run.asked.instruction.clone_from(&instruction);
        fixture.session.run_task(&fixture.run).unwrap();
        let bodies = fixture.script.bodies.lock().unwrap().clone();
        assert_eq!(bodies.len(), 1);
        assert!(bodies[0].to_string().contains(instruction.trim()));
    }

    phase one_issues_run_reaches_the_model_without_another_issues;
    {
        let mut fixture = native_fixture();
        let first_instruction = "first-native-private-instruction-secret";
        fixture.run.asked.instruction = first_instruction.into();
        *fixture.script.turns.lock().unwrap() = vec![turn(
            &[finish("first-native-private-result-secret", true)],
            24,
        )]
        .into();
        fixture.session.run_task(&fixture.run).unwrap();

        fixture.run.asked.message = "chat.workspace/channels/second/messages/asked".into();
        let second_instruction = "second-native-private-instruction-secret";
        fixture.run.asked.instruction = second_instruction.into();
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

    phase native_unfinished_answer_requests_review_instead_of_claiming_completion;
    {
        let mut fixture = native_fixture();
        *fixture.script.turns.lock().unwrap() =
            vec![turn(&[finish("I need the owner's decision", false)], 24)].into();
        fixture.session.run_task(&fixture.run).unwrap();
        assert_eq!(
            fixture.script.bodies.lock().unwrap().len(),
            1,
            "must exercise the actual model request"
        );
        let last = reported(&fixture.sent.borrow()).pop().unwrap();
        assert!(
            matches!(&last, TurnUpdate::Failed { message } if message == "I need the owner's decision"),
            "unfinished native work reported {last:?}"
        );
    }
}

/// Every public note the run reported, in order.
fn notes(sent: &[CollabRequest]) -> Vec<String> {
    reported(sent)
        .into_iter()
        .filter_map(|update| match update {
            TurnUpdate::Progress { message } => Some(message),
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
    let TurnUpdate::Failed { message } = terminal(&sent) else {
        panic!("a run with nothing to do fails: {:?}", terminal(&sent));
    };
    assert!(
        message.contains("no model is set up"),
        "the reply says the one thing that fixes it: {message}"
    );
    assert!(message.contains("Agent models"), "{message}");
    assert_eq!(
        message.matches("model").count(),
        2,
        "the missing model is said once, not twice: {message}"
    );
    assert!(
        !message.to_lowercase().contains("permission"),
        "a missing model never reads as a permission: {message}"
    );
}

/// The same run on a German desktop: the person reads the missing model, and
/// where to choose one, in German, and the Settings page is named as it is
/// named there.
#[test]
fn a_german_desktop_reads_the_missing_model_in_german() {
    let _german = crate::i18n::chosen("de-DE");
    let mut f = fixture("zzz", World::default(), Vec::new());
    f.session.chooser = Chooser::new(false, Duration::from_secs(5));
    f.session.run_task(&f.run).unwrap();
    let sent = f.sent.borrow();
    let notes = notes(&sent);
    assert!(
        notes
            .iter()
            .any(|note| note.contains("kein Modell") && note.contains("Agentenmodelle")),
        "the run announces the missing model in German: {notes:?}"
    );
    assert!(
        !notes.iter().any(|note| note.contains("No model")),
        "no note stays English on a German desktop: {notes:?}"
    );
    let TurnUpdate::Failed { message } = terminal(&sent) else {
        panic!("a run with nothing to do fails: {:?}", terminal(&sent));
    };
    assert!(
        message.contains("Es ist kein Modell eingerichtet"),
        "the reply says the one thing that fixes it, in German: {message}"
    );
    assert!(
        message.contains("Einstellungen") && message.contains("Agentenmodelle"),
        "the way to fix it names the German Settings page: {message}"
    );
}

/// What the conversation was told, one entry per record, in order.
fn recorded(sent: &[CollabRequest], action: &str) -> Vec<serde_json::Map<String, Value>> {
    sent.iter()
        .filter_map(invoked)
        .filter(|(_, invoked, _, _)| *invoked == action)
        .map(|(_, _, arguments, _)| arguments)
        .collect()
}

/// The terminal task report.
fn terminal(sent: &[CollabRequest]) -> TurnUpdate {
    reported(sent)
        .into_iter()
        .rev()
        .find(|update| {
            matches!(
                update,
                TurnUpdate::Succeeded { .. }
                    | TurnUpdate::Failed { .. }
                    | TurnUpdate::Cancelled { .. }
            )
        })
        .expect("a terminal report")
}

fn tool_names(body: &Value) -> Vec<String> {
    body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect()
}

/// The last tool result the model was shown in `body`.
fn last_observation(body: &Value) -> Value {
    let content = body["messages"].as_array().unwrap().last().unwrap()["content"][0]["content"]
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
        TurnUpdate::Succeeded { .. }
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

tilo_test_env::scenario! {
    fn a_task_stops_at_its_cancellation_deadline_and_budget;

    phase stop_during_the_first_silent_model_turn_does_not_wait_for_provider_output;
    {
        let mut f = paced_fixture(
            "write a paragraph",
            World {
                cancel_after_polls: 2,
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
            TurnUpdate::Cancelled { .. }
        ));
    }

    phase a_cancelled_task_still_stops_before_its_next_step;
    // The host tells the agent the turn stopped ahead of anything it answers after, and refuses
    // every step of it from then on: the next step never runs.
    {
        let mut f = fixture(
            "write two paragraphs",
            World {
                cancel_after_steps: Some(1),
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
            TurnUpdate::Cancelled { .. }
        ));
    }



    phase a_run_goes_on_past_thirty_turns_until_it_is_done;
    // A run has no turn cap of its own: it goes on past the 30 turns the
    // runner used to allow until it is done, as long as its turns get it
    // further (a run that only looks is told to finish: `stalled`).
    {
        let mut turns: Vec<Vec<TurnEvent>> = (0..45)
            .map(|item| {
                turn(
                    &[invoke(
                        "append-paragraph",
                        &serde_json::json!({"text": format!("Item {item}.")}),
                    )],
                    64,
                )
            })
            .collect();
        turns.push(turn(&[finish("Looked everywhere.", true)], 8));
        let mut f = fixture("look around", World::default(), turns);
        f.session.run_task(&f.run).unwrap();
        assert_eq!(f.script.bodies.lock().unwrap().len(), 46);
        assert!(matches!(
            terminal(&f.sent.borrow()),
            TurnUpdate::Succeeded { message } if message == "Looked everywhere."
        ));
    }
}

tilo_test_env::scenario! {
    fn a_streaming_turn_reports_output_and_dispatches_only_validated_calls;

    phase neutral_truncation_retains_output_without_dispatching_calls;
    {
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
            cost_micros: 0,
        };
        let mut f = fixture(
            "write hello",
            World::default(),
            // The model keeps running into its output limit: each turn is
            // continued from where it stopped, a bounded number of times, and
            // then the run fails with the clean sentence.
            (0..=MOST_CONTINUATIONS)
                .map(|_| {
                    vec![
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
                    ]
                })
                .collect(),
        );
        f.session.chooser = Chooser::scripted(
            Duration::from_secs(5),
            agent_provider_config::ProviderProtocol::OpenAiCompatible,
            Arc::new(Scripted(f.script.clone())),
        );
        f.session.run_task(&f.run).unwrap();
        assert!(f.world.borrow().steps.is_empty());
        // What the host hears is the sentence a person reads, and nothing of the
        // turn's record: no partial answer, no provider-exposed reasoning.
        assert!(matches!(terminal(&f.sent.borrow()),
            TurnUpdate::Failed { message }
                if !message.contains("Partial answer") && !message.contains("Provider summary")));
    }

    phase completed_stream_frames_cannot_dispatch_before_a_failed_final_response;
    {
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
                .any(|request| invoked(request).is_some())
        );
        // The transport's own words are the record, kept for debugging; the
        // sentence a person reads is what happened, in their terms.
        assert!(matches!(terminal(&f.sent.borrow()),
            TurnUpdate::Failed { message }
                if !message.contains("provider stream reset") && message.contains("The model")));
    }

    phase calls_wait_for_the_complete_turn_and_run_only_once;
    // The loop waits for the validated final batch, executes each call
    // exactly once and returns each result under the original call id.
    {
        let first = turn(
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
            vec![first, turn(&[finish("Wrote it.", true)], 8)],
            Some(Duration::from_millis(4)),
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
        assert_eq!(results["role"], "tool");
        let ids = results["content"]
            .as_array()
            .unwrap()
            .iter()
            .map(|block| {
                assert_eq!(block["type"], "tool_result");
                assert_ne!(block["content"], "Not handled.");
                block["call_id"].as_str().unwrap().to_owned()
            })
            .collect::<Vec<_>>();
        assert_eq!(ids, asked_ids(&bodies[1]), "each result under its call id");
        assert_eq!(
            terminal(&f.sent.borrow()),
            TurnUpdate::Succeeded {
                message: "Wrote it.".into()
            }
        );
    }
}

fn invoke_replying(action: &str, arguments: &Value, reply: &str) -> (&'static str, Value) {
    (
        tools::INVOKE,
        serde_json::json!({"operation": op(action), "parameters": arguments, "reply": reply}),
    )
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

tilo_test_env::scenario! {
    fn a_task_is_carried_out_step_by_step_through_published_operations;

    phase making_the_second_paragraph_bold_selects_it_then_toggles_the_mark;
    // Review test 7: "make the second paragraph bold" is carried out by
    // discovering, invoking and observing published operations — read the
    // document, select the paragraph (which makes toggle-mark available),
    // toggle the mark — with no application-specific tool anywhere.
    {
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
            .filter(|request| invoked(request).is_some())
            .count();
        assert_eq!(invocations, 3, "one invocation each");
        assert_eq!(
            terminal(&sent),
            TurnUpdate::Succeeded {
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
                    "place",
                    "read_result",
                    "search_web",
                    "read_page",
                    "tilo",
                    "plan"
                ],
                "only generic tools, never an application-specific one"
            );
        }
        let first: String = bodies[0]["messages"][1]["content"]
            .as_array()
            .unwrap()
            .iter()
            .map(|block| block["text"].as_str().unwrap())
            .collect();
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
        let observation = last_observation(&bodies[2]);
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

    phase a_late_republication_is_taken_while_the_model_is_still_busy;
    // A republication that reaches the agent after the view step's reply is
    // still taken up when the model has no next call for the loop to run,
    // so what the view unlocked is reported with the step. The step waits
    // for full-turn validation, not merely its streamed call preview.
    {
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

    phase opened_records_reads_the_desktop_receipt;
    {
        let receipt = serde_json::json!({
            "kind": "desktop.command",
            "command": "1:1",
            "generation": 3,
            "opened": [
                {"app_id": "so.tilo.notes", "document_id": "n-1"},
                {"app_id": "so.tilo.word-processor", "document_id": "doc-9"},
                {"app_id": "so.tilo.word-processor", "document_id": ""}
            ]
        });
        assert_eq!(
            opened_records(Some(&receipt), "so.tilo.word-processor"),
            ["doc-9"]
        );
        assert!(opened_records(Some(&serde_json::json!({})), "so.tilo.notes").is_empty());
        assert!(opened_records(None, "so.tilo.notes").is_empty());
    }

    phase a_single_turn_writes_and_finishes;
    // One turn does the whole job: the paragraph is written in one step
    // and the reply finishes the task — no second turn.
    {
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
            TurnUpdate::Succeeded {
                message: "Here you go: a paragraph about the ocean.".into()
            }
        );
    }

    phase a_failed_step_skips_the_rest_of_its_turn_and_is_observed;
    // A failed step stops the rest of its turn and keeps the turn's `plan`
    // from finishing; the model reads why and decides again.
    {
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
            TurnUpdate::Failed {
                message: "I could not write into the document.".into()
            }
        );
    }

    phase an_unshown_operation_is_refused_without_a_step;
    // An id no listing showed is answered as an error and never reaches the
    // broker, whatever it names.
    {
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
            TurnUpdate::Succeeded {
                message: "Nothing to do.".into()
            }
        );
    }

    phase a_fresh_workspace_opens_writer_then_writes_and_posts;
    // Lab regression (fresh workspace, Writer closed): the summary shows
    // the desktop's open for Writer; the model opens it (with a premature
    // reply, as gpt-oss does), the result lists Writer's operations with
    // their ids, and the model writes with an id copied without its
    // routing prefix and posts in Chat — all within three turns.
    {
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
            TurnUpdate::Succeeded {
                message: "Added the paragraph and posted hello.".into()
            }
        );
    }

    phase an_open_with_a_reply_does_not_finish_the_task;
    // Live regression (9997): the first turn opens Writer with a reply and
    // lists only "Open Writer" as a part. The reply on an open does not
    // finish; a following bare finish is refused, because the open part
    // and the request's own mention of Writer both need a Writer
    // operation; only after writing does the task finish.
    {
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
            TurnUpdate::Succeeded {
                message: "Wrote the essay.".into()
            }
        );
    }
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
        .flat_map(|message| message["content"].as_array().cloned().unwrap_or_default())
        .map(|block| {
            assert_eq!(block["type"], "tool_result", "{block}");
            (
                block["call_id"].as_str().unwrap().to_owned(),
                serde_json::from_str(block["content"].as_str().unwrap()).unwrap(),
            )
        })
        .collect()
}

/// The ids of the calls the model asked for in `body`, in call order.
fn asked_ids(body: &Value) -> Vec<String> {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["content"].as_array().cloned().unwrap_or_default())
        .filter(|block| block["type"] == "tool_call")
        .map(|block| block["id"].as_str().unwrap().to_owned())
        .collect()
}
#[path = "agent_loop_run_tests.rs"]
mod run;
