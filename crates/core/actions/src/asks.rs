//! The questions a program outside Tilo asks the person at the device, as it hands them to the
//! session.
//!
//! A program that needs a person's answer (which file to open, whether an app may record the
//! screen, an administrator's password, a network's secret) asks it on an [`AskSender`] and
//! waits ([`AskSender::ask`]). The sender keeps nothing: each question goes to the session's
//! [`AskReceiver`], which raises it as an asked record the person answers like any other, and
//! hands their answer back through [`AskReceiver::answer`]: what they typed reaches the asking
//! program in memory and nowhere else.
//!
//! A question is gone once it is answered, declined, or withdrawn by its program (the app
//! closed the request, polkit cancelled it): the future [`Asked`] withdraws its question when
//! dropped, so a program never leaves one behind, and a receiver that is gone answers every
//! question no.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tilo_lock::MutexRecover as _;
use tokio::sync::{mpsc, oneshot};
use zeroize::Zeroizing;

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

struct Pending {
    /// Tells this asking apart from another under the same handle.
    id: u64,
    question: Question,
    /// The workspace it is raised in: its sender's ([`AskSender::in_workspace`]).
    workspace: Option<String>,
    reply: oneshot::Sender<Answer>,
}

/// What a sender hands its receiver.
enum Sent {
    Ask(Pending),
    /// What pending question `handle` shows changed to these details.
    Update {
        handle: String,
        details: Value,
    },
    /// The program no longer asks `handle`: the asking `id` alone, or whatever asks it.
    Withdraw {
        handle: String,
        id: Option<u64>,
    },
}

type Wake = Arc<dyn Fn() + Send + Sync>;

/// Who is woken when a question arrives or goes: the receiver names it once.
type Waking = Arc<Mutex<Option<Wake>>>;

/// The asking that tells one question from another under the same handle.
static ASKED: AtomicU64 = AtomicU64::new(1);

/// A sender and the receiver its questions reach.
#[must_use]
pub fn channel() -> (AskSender, AskReceiver) {
    let (sent, received) = mpsc::unbounded_channel();
    let waking = Waking::default();
    (
        AskSender {
            sent,
            waking: Arc::clone(&waking),
            workspace: None,
        },
        AskReceiver {
            received: Arc::new(Mutex::new(Received {
                sent: received,
                pending: Vec::new(),
            })),
            waking,
        },
    )
}

/// Where a program asks the person: every question goes to the [`AskReceiver`] of its
/// [`channel`], and the sender keeps none of them.
#[derive(Clone)]
pub struct AskSender {
    sent: mpsc::UnboundedSender<Sent>,
    waking: Waking,
    /// The workspace of the program that asks through it: where each of its questions is raised.
    /// `None` for a program no workspace runs (polkit, a program outside Tilo), whose questions
    /// are raised in the workspace the person has in front.
    workspace: Option<String>,
}

impl std::fmt::Debug for AskSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AskSender")
            .field("received", &!self.sent.is_closed())
            .field("workspace", &self.workspace)
            .finish_non_exhaustive()
    }
}

impl AskSender {
    fn send(sent: &mpsc::UnboundedSender<Sent>, waking: &Waking, what: Sent) -> bool {
        let reached = sent.send(what).is_ok();
        let wake = waking.lock_recover().clone();
        if reached && let Some(wake) = wake {
            wake();
        }
        reached
    }

    /// This sender for a program `workspace` runs: each question asked through it is raised in
    /// that workspace and no other, whatever the program says of it.
    #[must_use]
    pub fn in_workspace(&self, workspace: impl Into<String>) -> Self {
        Self {
            workspace: Some(workspace.into()),
            ..self.clone()
        }
    }

    /// Ask `question`; the answer is `None` when it was declined or replaced, or nobody receives
    /// questions any more. Asking again under the same handle replaces the question.
    pub fn ask(&self, question: Question) -> Asked {
        let (reply, answer) = oneshot::channel();
        let handle = question.handle.clone();
        let id = ASKED.fetch_add(1, Ordering::Relaxed);
        Self::send(
            &self.sent,
            &self.waking,
            Sent::Ask(Pending {
                id,
                question,
                workspace: self.workspace.clone(),
                reply,
            }),
        );
        Asked {
            id,
            handle,
            answer,
            sent: self.sent.clone(),
            waking: Arc::clone(&self.waking),
        }
    }

    /// Change what pending question `handle` shows to `details` (an app offering other choices
    /// while it waits), keeping its asker waiting.
    pub fn update(&self, handle: &str, details: Value) {
        Self::send(
            &self.sent,
            &self.waking,
            Sent::Update {
                handle: handle.to_owned(),
                details,
            },
        );
    }

    /// The program no longer asks question `handle`, whoever asked it under that handle.
    pub fn withdraw(&self, handle: &str) {
        Self::send(
            &self.sent,
            &self.waking,
            Sent::Withdraw {
                handle: handle.to_owned(),
                id: None,
            },
        );
    }
}

struct Received {
    sent: mpsc::UnboundedReceiver<Sent>,
    pending: Vec<Pending>,
}

impl Received {
    /// Take in what the senders handed over since.
    fn take_in(&mut self) {
        while let Ok(sent) = self.sent.try_recv() {
            match sent {
                Sent::Ask(asked) => {
                    // Dropping the replaced question's sender tells its asker.
                    self.pending
                        .retain(|pending| pending.question.handle != asked.question.handle);
                    self.pending.push(asked);
                }
                Sent::Update { handle, details } => {
                    if let Some(pending) = self
                        .pending
                        .iter_mut()
                        .find(|pending| pending.question.handle == handle)
                    {
                        pending.question.details = details;
                    }
                }
                Sent::Withdraw { handle, id } => self.pending.retain(|pending| {
                    pending.question.handle != handle || id.is_some_and(|id| id != pending.id)
                }),
            }
        }
    }

    fn take(&mut self, handle: &str) -> Option<Pending> {
        self.take_in();
        let index = self
            .pending
            .iter()
            .position(|pending| pending.question.handle == handle)?;
        Some(self.pending.remove(index))
    }
}

/// The session's side of a [`channel`]: the questions programs ask now, which it puts to the
/// person, and where their answers go back.
#[derive(Clone)]
pub struct AskReceiver {
    received: Arc<Mutex<Received>>,
    waking: Waking,
}

impl std::fmt::Debug for AskReceiver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AskReceiver")
            .field("pending", &self.pending())
            .finish()
    }
}

impl AskReceiver {
    /// Call `wake` whenever a question arrives or goes, from whichever thread asked it.
    pub fn on_change(&self, wake: impl Fn() + Send + Sync + 'static) {
        *self.waking.lock_recover() = Some(Arc::new(wake));
    }

    /// The questions pending now, oldest first.
    #[must_use]
    pub fn pending(&self) -> Vec<Question> {
        let mut received = self.received.lock_recover();
        received.take_in();
        received
            .pending
            .iter()
            .map(|pending| pending.question.clone())
            .collect()
    }

    /// Pending question `handle`.
    #[must_use]
    pub fn question(&self, handle: &str) -> Option<Question> {
        self.pending()
            .into_iter()
            .find(|question| question.handle == handle)
    }

    /// The workspace pending question `handle` is raised in: its program's, `None` for a
    /// program no workspace runs.
    #[must_use]
    pub fn workspace_of(&self, handle: &str) -> Option<String> {
        let mut received = self.received.lock_recover();
        received.take_in();
        received
            .pending
            .iter()
            .find(|pending| pending.question.handle == handle)
            .and_then(|pending| pending.workspace.clone())
    }

    /// Answer question `handle`; `false` when it is no longer pending, or its program stopped
    /// waiting for it.
    pub fn answer(&self, handle: &str, answer: Answer) -> bool {
        self.received
            .lock_recover()
            .take(handle)
            .is_some_and(|pending| pending.reply.send(answer).is_ok())
    }

    /// Decline question `handle`; `false` when it is no longer pending.
    pub fn decline(&self, handle: &str) -> bool {
        self.received.lock_recover().take(handle).is_some()
    }
}

/// A question asked and not yet answered: resolves to the answer, `None`
/// when it was declined or replaced. Dropping it withdraws the question.
pub struct Asked {
    id: u64,
    handle: String,
    answer: oneshot::Receiver<Answer>,
    sent: mpsc::UnboundedSender<Sent>,
    waking: Waking,
}

impl Asked {
    /// The question's handle.
    #[must_use]
    pub fn handle(&self) -> &str {
        &self.handle
    }

    /// Wait on this thread for the person's answer, at most until
    /// `deadline` (`None`: until the person answers or the question goes):
    /// `Some` of the answer, which is `None` when the question was declined
    /// or replaced, or `None` when the question is still up at the deadline,
    /// in which case the caller keeps it pending and asks again later.
    ///
    /// The session answers on its own loop, so a thread waiting here must
    /// not block the loop: this parks on a waker the answer wakes, and never
    /// on a timer that polls the loop.
    pub fn until(&mut self, deadline: Option<tilo_clock::time::Instant>) -> Option<Option<Answer>> {
        struct Park(std::thread::Thread);
        impl std::task::Wake for Park {
            fn wake(self: std::sync::Arc<Self>) {
                self.0.unpark();
            }
            fn wake_by_ref(self: &std::sync::Arc<Self>) {
                self.0.unpark();
            }
        }
        let waker = std::task::Waker::from(std::sync::Arc::new(Park(std::thread::current())));
        let mut cx = Context::from_waker(&waker);
        loop {
            if let Poll::Ready(answer) = Pin::new(&mut *self).poll(&mut cx) {
                return Some(answer);
            }
            match deadline {
                Some(deadline) => {
                    let now = tilo_clock::now();
                    if now >= deadline {
                        // The deadline has come: the question is still up.
                        return None;
                    }
                    std::thread::park_timeout(deadline.saturating_duration_since(now));
                }
                None => std::thread::park(),
            }
        }
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
        // Only this asker's own question: a replacement under the same handle belongs to
        // whoever asked it. One answered already is gone, and the receiver finds nothing.
        if !matches!(
            self.answer.try_recv(),
            Ok(_) | Err(oneshot::error::TryRecvError::Closed)
        ) {
            AskSender::send(
                &self.sent,
                &self.waking,
                Sent::Withdraw {
                    handle: std::mem::take(&mut self.handle),
                    id: Some(self.id),
                },
            );
        }
    }
}

#[cfg(test)]
mod tests;
