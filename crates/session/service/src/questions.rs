//! The questions services ask the person at the device.
//!
//! A service that needs a person's answer (which file to open, whether an
//! app may record the screen, an administrator's password, a network's
//! secret) asks it here and waits ([`Questions::ask`]). Each question is
//! published as a resource of its own under [`QUESTIONS_RESOURCE`], with
//! the actions that answer or decline it; the session's sheet presents the
//! pending questions ([`Questions::pending`]) and answers through those
//! actions, or, for what was typed into a secret prompt, straight through
//! [`Questions::answer`] so the secret never travels as an action's
//! arguments.
//!
//! A question is gone once it is answered, declined, or withdrawn by its
//! service (the app closed the request, polkit cancelled it): the future
//! [`Asked`] withdraws its question when dropped, so a service never
//! leaves one behind.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tilo_actions::{ActionOutput, DoorError, ResourcePresentation, ResourceSpec};
use tilo_lock::MutexRecover as _;
use tokio::sync::oneshot;
use zeroize::Zeroizing;

use tilo_resource::Effect;

use crate::{Service, Started, decode, offer};

/// The resource the pending questions are published under.
pub const QUESTIONS_RESOURCE: &str = "questions";
/// The kind of [`QUESTIONS_RESOURCE`].
pub const QUESTIONS_KIND: &str = "session.questions";
/// The kind of each question's resource.
pub const QUESTION_KIND: &str = "session.question";

/// What a question's resource id starts with, before its handle.
const QUESTION_PREFIX: &str = "question:";

/// The resource id question `handle` is published as.
#[must_use]
pub fn question_resource(handle: &str) -> String {
    format!("{QUESTION_PREFIX}{handle}")
}

/// One question a service asks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Question {
    /// Names the question while it is pending; asking again under the same
    /// handle replaces it. Services keep theirs apart by a prefix of their
    /// own.
    pub handle: String,
    /// What is asked, as the service names it (`portal.file-chooser`,
    /// `polkit.authenticate`): what the sheet reads [`Self::details`] as.
    pub kind: String,
    /// The app the question is asked for, when one asks.
    pub app_id: Option<String>,
    /// What the question is listed as: the asking app, the network, the
    /// administrator action. Data from the asker, never the service's own
    /// words, which the sheet supplies in the person's language.
    pub label: String,
    /// Everything else the sheet needs, in the shape of [`Self::kind`].
    pub details: serde_json::Value,
}

/// The person's answer to a question.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Answer {
    /// The choice they made, counted from zero, when the question offered
    /// choices.
    pub choice: Option<usize>,
    /// The files or the place they chose.
    pub uris: Vec<String>,
    /// Whether they asked for the answer to be remembered.
    pub remember: bool,
    /// What they typed, one value per field the question asked for.
    pub values: Vec<Zeroizing<String>>,
}

impl std::fmt::Debug for Answer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // What was typed is never written out.
        f.debug_struct("Answer")
            .field("choice", &self.choice)
            .field("uris", &self.uris)
            .field("remember", &self.remember)
            .field("values", &self.values.len())
            .finish()
    }
}

/// What a question's resource offers: answering it or declining it.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.session.question")]
pub enum QuestionEffect {
    /// Answer the question
    #[effect(own_device, attended)]
    Answer {
        #[param("The choice made, counted from zero")]
        choice: Option<u32>,
        #[param("The files or the place chosen, as URIs")]
        uris: Option<Vec<String>>,
        #[param("Whether the answer is remembered")]
        remember: Option<bool>,
    },
    /// Decline the question
    #[effect(own_device, idempotent)]
    Cancel,
}

struct Pending {
    /// Tells this asking apart from another under the same handle.
    id: u64,
    question: Question,
    reply: oneshot::Sender<Answer>,
}

type Wake = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct State {
    pending: Vec<Pending>,
    wake: Option<Wake>,
    asked: u64,
}

/// The questions pending now, shared by the services that ask them and the
/// session that publishes them.
#[derive(Clone, Default)]
pub struct Questions {
    state: Arc<Mutex<State>>,
}

impl std::fmt::Debug for Questions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Questions")
            .field("pending", &self.pending())
            .finish()
    }
}

impl Questions {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Call `wake` whenever a question arrives or goes, from whichever
    /// thread asked or answered it.
    pub fn on_change(&self, wake: impl Fn() + Send + Sync + 'static) {
        self.state.lock_recover().wake = Some(Arc::new(wake));
    }

    /// Ask `question`; the answer is `None` when it was declined or
    /// replaced.
    pub fn ask(&self, question: Question) -> Asked {
        let (reply, answer) = oneshot::channel();
        let handle = question.handle.clone();
        let (id, wake) = {
            let mut state = self.state.lock_recover();
            state.asked = state.asked.wrapping_add(1);
            let id = state.asked;
            // Dropping the replaced question's sender tells its asker.
            state
                .pending
                .retain(|pending| pending.question.handle != handle);
            state.pending.push(Pending {
                id,
                question,
                reply,
            });
            (id, state.wake.clone())
        };
        if let Some(wake) = wake {
            wake();
        }
        Asked {
            id,
            handle,
            answer,
            questions: self.clone(),
        }
    }

    /// The questions pending now, oldest first.
    #[must_use]
    pub fn pending(&self) -> Vec<Question> {
        self.state
            .lock_recover()
            .pending
            .iter()
            .map(|pending| pending.question.clone())
            .collect()
    }

    /// Pending question `handle`.
    #[must_use]
    pub fn question(&self, handle: &str) -> Option<Question> {
        self.state
            .lock_recover()
            .pending
            .iter()
            .find(|pending| pending.question.handle == handle)
            .map(|pending| pending.question.clone())
    }

    fn take(&self, handle: &str) -> Option<Pending> {
        let (taken, wake) = {
            let mut state = self.state.lock_recover();
            let index = state
                .pending
                .iter()
                .position(|pending| pending.question.handle == handle)?;
            (state.pending.remove(index), state.wake.clone())
        };
        if let Some(wake) = wake {
            wake();
        }
        Some(taken)
    }

    /// Answer question `handle`; `false` when it is no longer pending.
    pub fn answer(&self, handle: &str, answer: Answer) -> bool {
        self.take(handle)
            .is_some_and(|pending| pending.reply.send(answer).is_ok())
    }

    /// Decline question `handle`; `false` when it is no longer pending.
    pub fn decline(&self, handle: &str) -> bool {
        self.take(handle).is_some()
    }
}

fn question_spec(question: Question) -> ResourceSpec {
    offer::<QuestionEffect>(ResourceSpec::new(
        question_resource(&question.handle),
        QUESTION_KIND,
        question.label,
    ))
    .with_presentation(ResourcePresentation {
        detail: Some(question.kind),
        badge: None,
        preview: None,
    })
}

impl Service for Questions {
    fn resources(&self) -> Vec<ResourceSpec> {
        let children = self.pending().into_iter().map(question_spec);
        vec![
            ResourceSpec::new(QUESTIONS_RESOURCE, QUESTIONS_KIND, "Questions")
                .with_children(children),
        ]
    }

    /// A question's reading is the question itself: what the sheet shows.
    fn read(&self, resource: &str) -> Option<Value> {
        if resource == QUESTIONS_RESOURCE {
            return Some(crate::reading(&self.pending()));
        }
        let handle = resource.strip_prefix(QUESTION_PREFIX)?;
        self.question(handle)
            .map(|question| crate::reading(&question))
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, DoorError> {
        let effect: QuestionEffect = decode(&self.resources(), resource, action, arguments)?;
        let unknown = || DoorError::Unknown(tilo_actions::ActionAddress::new(resource, action));
        let handle = resource.strip_prefix(QUESTION_PREFIX).ok_or_else(unknown)?;
        let done = match effect {
            QuestionEffect::Answer {
                choice,
                uris,
                remember,
            } => self.answer(
                handle,
                Answer {
                    choice: choice.and_then(|choice| usize::try_from(choice).ok()),
                    uris: uris.unwrap_or_default(),
                    remember: remember.unwrap_or(false),
                    values: Vec::new(),
                },
            ),
            QuestionEffect::Cancel => self.decline(handle),
        };
        if done {
            Ok(Started::Done(ActionOutput::default()))
        } else {
            Err(unknown())
        }
    }
}

/// A question asked and not yet answered: resolves to the answer, `None`
/// when it was declined or replaced. Dropping it withdraws the question.
pub struct Asked {
    id: u64,
    handle: String,
    answer: oneshot::Receiver<Answer>,
    questions: Questions,
}

impl Asked {
    /// The question's handle.
    #[must_use]
    pub fn handle(&self) -> &str {
        &self.handle
    }
}

impl Future for Asked {
    type Output = Option<Answer>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.answer).poll(cx).map(Result::ok)
    }
}

impl Drop for Asked {
    fn drop(&mut self) {
        // Only this asker's own question: a replacement under the same
        // handle belongs to whoever asked it.
        let wake = {
            let mut state = self.questions.state.lock_recover();
            let before = state.pending.len();
            state.pending.retain(|pending| pending.id != self.id);
            (state.pending.len() != before)
                .then(|| state.wake.clone())
                .flatten()
        };
        if let Some(wake) = wake {
            wake();
        }
    }
}

#[cfg(test)]
mod tests;
