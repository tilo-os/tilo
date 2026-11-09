//! "Ask <provider> …": a question typed into search, answered by the model the person's AI settings
//! choose, streamed into the row as the answer is written. The row the person is on never moves
//! while the text arrives: the rows are built again around the same selection. Choosing the row
//! copies the answer.
//! With no provider set up the one row opens the page that sets one up, so a question is never a
//! dead end and search never explains a setting by name.
//! The ask goes through the session's model gateway as Activity does ([`crate::activity_models`]),
//! on a thread of its own; nothing typed leaves the machine before the person asks, and nothing
//! about a failure reaches anything but the row's own line.

#![cfg_attr(
    target_arch = "wasm32",
    allow(
        dead_code,
        reason = "the page answers its own questions through this role next"
    )
)]

use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use agent_provider_config::ProviderConfig;
use agent_provider_config::protocol::{
    Content, Message, ModelRequest, Role, StreamEvent, ToolChoice,
};
use tilo_desktop_search::answers::question;
use tilo_desktop_search::model::{Hit, InvokeTarget, Source, Target};
use tilo_desktop_search::providers::ask_kind;
use ui::search::SearchPreview;
use ui::search::system::MAX_PROVIDER_SCORE;

/// The package a question asks as: what the person's AI settings can choose a model for, and what
/// the gateway authorizes for a plain completion. Activity describes the history as the same app.
pub(crate) const PACKAGE: &str = "so.tilo.activity";

/// The app that sets a provider up.
const SETTINGS_APP: &str = "so.tilo.settings";

/// The Settings page providers are set up on, as the row asks it to open.
const PROVIDERS_ANCHOR: &str = "settings.section:agent_models";

/// The question the answer is given as, so a model answers it as one.
const QUESTION: &str = "Answer the question in a few sentences, in plain text, \
     as the person would say it to a well-read colleague. The question is: ";

/// The most of an answer a row keeps: a search answer reads at a glance, and asking for more is
/// asking again.
const MAX_ANSWER: usize = 4000;

/// What the answering thread sends as it writes.
enum Chunk {
    Text(String),
    Failed(String),
}

/// The question asked, and the answer as it stands now.
#[derive(Default)]
pub(crate) struct Answering {
    /// The question being answered: a new one is a new ask.
    question: String,
    provider: String,
    model: String,
    text: String,
    failed: Option<String>,
    /// Whether the provider is still writing.
    streaming: bool,
    /// The answer's arrival, while it comes.
    deltas: Option<Receiver<Chunk>>,
}

/// What the session keeps of the ask between rows: the same answer is still there when search is
/// built again (a widened scope, a filled-in frame).
pub(crate) type Shared = Arc<Mutex<Answering>>;

/// The search's answer source, in one workspace.
pub(crate) struct Ask {
    state: Shared,
    /// The workspace the setup row opens Settings in.
    workspace: String,
    /// What wakes the session's loop as the answer is written, so the row shows it as it lands.
    wake: Option<tilo_service::Wake>,
    /// Where this host's questions are answered.
    models: Option<Arc<dyn crate::platform::Models>>,
}

impl Ask {
    pub(crate) fn new(
        state: Shared,
        workspace: &str,
        wake: Option<tilo_service::Wake>,
        models: Option<Arc<dyn crate::platform::Models>>,
    ) -> Self {
        Self {
            state,
            workspace: workspace.to_owned(),
            wake,
            models,
        }
    }

    fn answered(&self) -> MutexGuard<'_, Answering> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Ask `needle` of the chosen provider, unless it is already asked or answered.
    fn ask(&self, needle: &str, provider: &str, model: &str) {
        let mut state = self.answered();
        let answered = state.question == needle
            && (state.streaming || state.failed.is_some() || !state.text.is_empty());
        if answered {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        *state = Answering {
            question: needle.to_owned(),
            provider: provider.to_owned(),
            model: model.to_owned(),
            text: String::new(),
            failed: None,
            streaming: true,
            deltas: Some(receiver),
        };
        drop(state);
        let needle = needle.to_owned();
        let wake = self.wake.clone();
        let models = self.models.clone();
        std::thread::spawn(move || answer(needle, &sender, wake.as_ref(), models.as_deref()));
    }

    fn row(&self, needle: &str) -> Hit {
        let state = self.answered();
        let (subtitle, body) = match (&state.failed, state.text.is_empty()) {
            (Some(reason), _) => (
                Some(tilo_desktop_search::i18n::t_args(
                    "search-ask-failed",
                    &[
                        ("provider", state.provider.as_str().into()),
                        ("reason", reason.as_str().into()),
                    ],
                )),
                None,
            ),
            (None, true) => (
                None,
                Some(tilo_desktop_search::i18n::t("search-ask-answering")),
            ),
            (None, false) => (Some(state.model.clone()), Some(state.text.clone())),
        };
        let preview = body.map(|body| SearchPreview::default().body(body));
        Hit {
            row: None,
            preview,
            faces: Vec::new(),
            outcome: Some(tilo_desktop_search::i18n::t("search-ask-copy")),
            kind: ask_kind(),
            provider: self.id().into(),
            id: format!("ask:{needle}"),
            title: tilo_desktop_search::i18n::t_args(
                "search-ask-for",
                &[
                    ("provider", state.provider.as_str().into()),
                    ("query", needle.into()),
                ],
            ),
            subtitle: subtitle.filter(|subtitle| !subtitle.trim().is_empty()),
            facts: Vec::new(),
            score: MAX_PROVIDER_SCORE,
            icon: Some(ui::icons::Icon::Sparkles),
            target: Target::Copy {
                text: state.text.clone(),
            },
        }
    }

    /// The row that takes a person to where a provider is set up.
    fn setup_row(&self) -> Hit {
        Hit {
            row: None,
            preview: None,
            faces: Vec::new(),
            outcome: None,
            kind: ask_kind(),
            provider: self.id().into(),
            id: "setup".into(),
            title: tilo_desktop_search::i18n::t("search-ask-setup"),
            subtitle: Some(tilo_desktop_search::i18n::t("search-ask-setup-detail")),
            facts: Vec::new(),
            score: MAX_PROVIDER_SCORE,
            icon: Some(ui::icons::Icon::Sparkles),
            target: Target::Invoke(Box::new(InvokeTarget::open(
                SETTINGS_APP,
                self.workspace.clone(),
                PROVIDERS_ANCHOR,
            ))),
        }
    }
}

/// The provider and model the person's AI settings choose for a plain answer,
/// as they are called; `None` while no provider is set up.
fn chosen() -> Option<(String, String)> {
    let config = ProviderConfig::load();
    let (profile, model) = config.selection_for(PACKAGE)?;
    Some((profile.label.clone(), model.label.clone()))
}

/// Ask `question` through the session's model gateway, on this thread (the ask is started on one of
/// its own), sending what comes as it is written.
fn answer(
    question: String,
    deltas: &Sender<Chunk>,
    wake: Option<&tilo_service::Wake>,
    gateway: Option<&dyn crate::platform::Models>,
) {
    let Some(models) = gateway.and_then(crate::platform::Models::models) else {
        land(
            deltas,
            wake,
            Chunk::Failed(tilo_desktop_search::i18n::t("search-ask-no-gateway")),
        );
        return;
    };
    answer_with(&models, question, deltas, wake);
}

/// What the answering thread wrote back, waking the loop so the row shows it.
fn land(deltas: &Sender<Chunk>, wake: Option<&tilo_service::Wake>, chunk: Chunk) {
    if deltas.send(chunk).is_ok()
        && let Some(wake) = wake
    {
        wake();
    }
}

/// [`answer`] against a model capability, wherever it comes from.
fn answer_with(
    models: &ui::models::Models,
    question: String,
    deltas: &Sender<Chunk>,
    wake: Option<&tilo_service::Wake>,
) {
    let Ok(runtime) = tilo_rt::try_app() else {
        land(
            deltas,
            wake,
            Chunk::Failed(tilo_desktop_search::i18n::t("search-ask-no-gateway")),
        );
        return;
    };
    let request = ModelRequest {
        messages: vec![
            Message {
                role: Role::System,
                content: vec![Content::Text {
                    text: QUESTION.into(),
                }],
            },
            Message {
                role: Role::User,
                content: vec![Content::Text { text: question }],
            },
        ],
        tools: Vec::new(),
        tool_choice: ToolChoice::None,
        max_output_tokens: 700,
    };
    let writing = deltas.clone();
    let waking = wake.cloned();
    let outcome = runtime.block_on(async move {
        let sink: agent_provider_config::protocol::StreamSink<'_> = Box::new(move |event| {
            if let StreamEvent::TextDelta { text } = event {
                land(&writing, waking.as_ref(), Chunk::Text(text));
            }
        });
        models.stream(&request, sink).await
    });
    match outcome {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => land(deltas, wake, Chunk::Failed(error.message)),
        Err(error) => land(deltas, wake, Chunk::Failed(error.to_string())),
    }
}

impl Source for Ask {
    fn id(&self) -> &'static str {
        "ask"
    }

    fn name(&self) -> String {
        tilo_desktop_search::i18n::t("search-kind-ask")
    }

    /// Asked what was typed, word for word: a question is the question.
    fn verbatim(&self) -> bool {
        true
    }

    fn query(&self, needle: &str) -> Vec<Hit> {
        let needle = needle.trim();
        if needle.is_empty() {
            return Vec::new();
        }
        let Some((provider, model)) = chosen() else {
            return vec![self.setup_row()];
        };
        if !question::asks(needle) {
            return Vec::new();
        }
        self.ask(needle, &provider, &model);
        vec![self.row(needle)]
    }

    fn poll(&self) -> bool {
        let mut state = self.answered();
        let Some(deltas) = state.deltas.take() else {
            return false;
        };
        let mut changed = false;
        let mut ended = false;
        loop {
            match deltas.try_recv() {
                Ok(Chunk::Text(text)) => {
                    if state.text.len() < MAX_ANSWER {
                        state.text.push_str(&text);
                    }
                    changed = true;
                }
                Ok(Chunk::Failed(reason)) => {
                    state.failed = Some(reason);
                    state.streaming = false;
                    changed = true;
                    ended = true;
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    changed |= state.streaming;
                    state.streaming = false;
                    ended = true;
                    break;
                }
            }
        }
        if !ended {
            state.deltas = Some(deltas);
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(workspace: &str) -> Ask {
        Ask::new(Shared::default(), workspace, None, None)
    }

    /// A question with nothing set up leads to the place that sets it up: the row opens Settings at
    /// its providers, in the workspace the search was opened in.
    #[test]
    fn without_a_provider_the_one_row_opens_the_setup() {
        let ask = setup("workspace");
        for typed in ["what is a CRDT?", "notes", ""] {
            let rows = ask.query(typed);
            if typed.is_empty() {
                assert!(rows.is_empty(), "an empty field asks nothing");
                continue;
            }
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].title,
                tilo_desktop_search::i18n::t("search-ask-setup")
            );
            let Target::Invoke(open) = &rows[0].target else {
                panic!("the setup row opens the page that sets one up");
            };
            assert_eq!(open.app, SETTINGS_APP);
            assert_eq!(open.workspace, "workspace");
            assert_eq!(
                open.arguments.get("anchor").and_then(|a| a.as_str()),
                Some(PROVIDERS_ANCHOR)
            );
        }
    }

    /// The text arrives a delta at a time, the row says what is known so far, and the same question
    /// is not asked twice.
    #[test]
    fn an_answer_written_into_the_row_keeps_the_question_it_answers() {
        let ask = setup("workspace");
        {
            let mut state = ask.answered();
            *state = Answering {
                question: "what is a CRDT?".into(),
                provider: "Anthropic".into(),
                model: "Claude".into(),
                text: "A conflict-free replicated data type.".into(),
                failed: None,
                streaming: false,
                deltas: None,
            };
        }
        let row = ask.row("what is a CRDT?");
        assert_eq!(row.title, "Ask Anthropic: what is a CRDT?");
        assert_eq!(row.subtitle.as_deref(), Some("Claude"));
        assert_eq!(
            row.preview
                .as_ref()
                .and_then(|preview| preview.body.as_deref()),
            Some("A conflict-free replicated data type.")
        );
        let Target::Copy { text } = &row.target else {
            panic!("choosing the row copies the answer");
        };
        assert_eq!(text, "A conflict-free replicated data type.");
    }

    /// A provider that writes its answer a piece at a time, as a streaming gateway does.
    struct Writing(Vec<&'static str>);

    impl agent_provider_config::protocol::ModelBackend for Writing {
        fn complete<'a>(
            &'a self,
            _request: &'a ModelRequest,
        ) -> agent_provider_config::protocol::CompletionFuture<'a> {
            Box::pin(async {
                Err(agent_provider_config::protocol::ModelError::new(
                    agent_provider_config::protocol::ErrorKind::Unavailable,
                ))
            })
        }

        fn stream<'a>(
            &'a self,
            _request: &'a ModelRequest,
            mut sink: agent_provider_config::protocol::StreamSink<'a>,
        ) -> agent_provider_config::protocol::CompletionFuture<'a> {
            Box::pin(async move {
                for piece in &self.0 {
                    sink(StreamEvent::TextDelta {
                        text: (*piece).to_owned(),
                    });
                }
                Ok(agent_provider_config::protocol::ModelResponse {
                    message: Message::text(Role::Assistant, self.0.concat()),
                    stop_reason: agent_provider_config::protocol::StopReason::EndTurn,
                    usage: agent_provider_config::protocol::Usage::default(),
                    cost_micros: 0,
                })
            })
        }
    }

    /// The answer arrives a piece at a time and the row shows it as it is written, then stops
    /// asking.
    #[test]
    fn a_streamed_answer_writes_itself_into_the_row() {
        let ask = setup("workspace");
        let (sender, receiver) = mpsc::channel();
        {
            let mut state = ask.answered();
            *state = Answering {
                question: "what is a CRDT?".into(),
                provider: "Anthropic".into(),
                model: "Claude".into(),
                streaming: true,
                deltas: Some(receiver),
                ..Answering::default()
            };
        }
        let models = ui::models::Models::new(std::rc::Rc::new(Writing(vec![
            "A conflict-free ",
            "replicated data type.",
        ])));
        answer_with(&models, "what is a CRDT?".into(), &sender, None);
        // The gateway returns, so nothing more comes: the thread that would have written more is
        // gone with its end of the channel.
        drop(sender);
        let changed = ask.poll();
        assert!(changed, "what arrived is worth drawing");
        let row = ask.row("what is a CRDT?");
        assert_eq!(
            row.preview
                .as_ref()
                .and_then(|preview| preview.body.as_deref()),
            Some("A conflict-free replicated data type."),
            "the row says what is written so far, and keeps it"
        );
        assert!(!ask.poll(), "a finished answer changes nothing more");
        assert!(!ask.answered().streaming);
    }

    #[test]
    fn a_failed_ask_says_so_in_its_own_row() {
        let ask = setup("workspace");
        {
            let mut state = ask.answered();
            *state = Answering {
                question: "why?".into(),
                provider: "Anthropic".into(),
                model: "Claude".into(),
                failed: Some("the key was refused".into()),
                ..Answering::default()
            };
        }
        let row = ask.row("why?");
        assert_eq!(
            row.subtitle.as_deref(),
            Some("Anthropic could not answer: the key was refused")
        );
    }
}
