//! The commands a session ran, as its shell marks them (docs/app-model/ush.md, "Commands").
//!
//! The marks themselves and the runs they make are the engine's
//! ([`tilo_terminal::commands`]); this app keeps the queue of commands it was asked to run.

pub use tilo_terminal::commands::{
    Commands, FinishedCommand, HistoryAsk, MAX_OUTPUT_BYTES, Mark, Marks, Piece, new_key,
};

#[derive(Clone, Default)]
pub struct CommandQueue {
    inner: std::rc::Rc<std::cell::RefCell<CommandQueueState>>,
}

#[derive(Default)]
struct CommandQueueState {
    waiting: Vec<Asked>,
    pane: String,
}

pub struct Asked {
    pub request: ui::tilo_resource::Id,
    pub command: String,
    pub requested_by: Option<ui::tilo_resource::MemberId>,
    pub agent: bool,
    pub reply: ui::framework::EffectReply,
}

impl CommandQueue {
    pub fn ask(&self, asked: Asked) {
        self.inner.borrow_mut().waiting.push(asked);
    }

    pub fn take(&self) -> Vec<Asked> {
        std::mem::take(&mut self.inner.borrow_mut().waiting)
    }

    pub fn set_pane(&self, pane: &str) {
        pane.clone_into(&mut self.inner.borrow_mut().pane);
    }

    #[must_use]
    pub fn owns(&self, pane: &str) -> bool {
        let own = &self.inner.borrow().pane;
        !own.is_empty() && own == pane
    }
}
