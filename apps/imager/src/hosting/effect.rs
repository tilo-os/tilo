//! What an effect answers, and the workers it starts.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use ui::tilo_actions::ActionOutput;

use super::{Port, Window};

/// What an effect, or the worker it started, answers.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Effected {
    /// Finished: the output goes back to whoever invoked the effect.
    Done(ActionOutput),
    /// Still running: the worker started through the [`EffectCx`] answers later.
    Pending,
}

impl Effected {
    /// Finished, with nothing to report.
    pub(crate) fn done() -> Self {
        Self::Done(ActionOutput::default())
    }

    /// Finished, answering with `value`.
    pub(crate) fn value(value: impl serde::Serialize) -> Self {
        Self::Done(ActionOutput {
            value: Some(serde_json::to_value(value).unwrap_or(serde_json::Value::Null)),
            resources: Vec::new(),
        })
    }
}

/// An effect's answer, or why it did not run.
pub(crate) type EffectAnswer = Result<Effected, String>;

/// What a running worker last reported.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct JobProgress {
    pub phase: String,
    pub completed: Option<u64>,
    pub total: Option<u64>,
}

/// A worker's end of its progress: each report wakes the window.
#[derive(Clone)]
pub(crate) struct Progress {
    state: Arc<Mutex<JobProgress>>,
    wake: ui::WakeHandle,
}

impl Progress {
    pub(super) fn new(wake: ui::WakeHandle) -> Self {
        Self {
            state: Arc::default(),
            wake,
        }
    }

    pub(crate) fn report(&self, phase: &str, completed: Option<u64>, total: Option<u64>) {
        *self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = JobProgress {
            phase: phase.to_owned(),
            completed,
            total,
        };
        self.wake.wake();
    }

    pub(super) fn read(&self) -> JobProgress {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// Where an invocation's answer goes.
pub(super) enum Answer {
    /// A control's press: a failure is said over the window.
    Pressed,
    #[cfg(unix)]
    Console(super::console::Waiting),
    #[cfg(test)]
    Test(Rc<RefCell<Option<Result<ActionOutput, String>>>>),
}

struct Waiting {
    answer: Option<Answer>,
}

/// An invocation that answers later: what a window keeps of the caller of an effect it
/// finishes itself (the write, which ends when the writer stops).
#[derive(Clone)]
pub(crate) struct Reply(Rc<RefCell<Waiting>>);

impl Reply {
    pub(super) fn new(answer: Answer) -> Self {
        Self(Rc::new(RefCell::new(Waiting {
            answer: Some(answer),
        })))
    }

    /// Answer the caller, once.
    pub(crate) fn finish(&self, answer: Result<ActionOutput, String>) {
        let Some(waiting) = self.0.borrow_mut().answer.take() else {
            return;
        };
        match waiting {
            Answer::Pressed => {
                if let Err(reason) = answer {
                    ui::control_notice::say_in_window(&reason);
                }
            }
            #[cfg(unix)]
            Answer::Console(waiting) => waiting.finish(answer),
            #[cfg(test)]
            Answer::Test(slot) => *slot.borrow_mut() = Some(answer),
        }
    }
}

/// What an effect is answered through: the caller waiting for it, and the workers it starts.
pub(crate) struct EffectCx<'a, W: Window> {
    port: &'a Port<W>,
    reply: Option<Reply>,
}

impl<'a, W: Window> EffectCx<'a, W> {
    pub(super) fn new(port: &'a Port<W>, reply: Option<Reply>) -> Self {
        Self { port, reply }
    }

    pub(super) fn take_reply(&mut self) -> Option<Reply> {
        self.reply.take()
    }

    /// The caller of this effect, to answer later: the effect answers [`Effected::Pending`].
    pub(crate) fn reply(&mut self) -> Reply {
        self.reply
            .take()
            .unwrap_or_else(|| Reply::new(Answer::Pressed))
    }

    /// Run `work` on a worker and `then` on the window with what it returned; `then` answers
    /// the caller.
    pub(crate) fn thread<T: Send + 'static>(
        &mut self,
        work: impl FnOnce(&Progress) -> T + Send + 'static,
        then: impl FnOnce(&mut W, T, &mut EffectCx<'_, W>) -> EffectAnswer + Send + 'static,
    ) -> Effected {
        let reply = self.reply();
        let progress = Progress::new(self.port.wake());
        let ticket = self.port.spawn(None, progress, work, then);
        self.port.0.waiting.borrow_mut().insert(ticket, reply);
        Effected::Pending
    }

    /// Ask the person for a file with the platform's chooser, then run `then` on the window with
    /// what they chose: `None` when they cancelled.
    pub(crate) fn choose_path(
        &mut self,
        request: ui::portal::PickRequest,
        then: impl FnOnce(&mut W, Option<std::path::PathBuf>, &mut EffectCx<'_, W>) -> EffectAnswer
        + Send
        + 'static,
    ) -> Effected {
        let chosen = request.send();
        self.thread(
            move |_| chosen.recv().ok(),
            move |window, outcome, cx| match outcome {
                Some(ui::portal::PortalOutcome::Opened(path)) => then(window, Some(path), cx),
                Some(ui::portal::PortalOutcome::Cancelled) => then(window, None, cx),
                Some(ui::portal::PortalOutcome::Failed(reason)) => Err(reason),
                None => Err("The file chooser closed without an answer.".into()),
            },
        )
    }
}
