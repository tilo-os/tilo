//! What a model writes while it answers one ask, handed on as it comes.
//!
//! A model's ask blocks a thread of its own ([`super::Model::turn_streaming`]); what it writes
//! meanwhile reaches [`super::Doing::hear`] through a [`Hearing`], on the turn's own task, while
//! the ask goes on. What arrives while the one who hears it is busy with what came before waits
//! and is handed on together.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use agent_provider_config::protocol::{ModelRequest, ModelResponse, StreamEvent};
use tilo_lock::MutexRecover as _;
use tokio::sync::mpsc;

use super::{Doing, Model};

/// What the model writes while one ask goes on.
pub struct Hearing {
    came: mpsc::UnboundedReceiver<StreamEvent>,
}

impl Hearing {
    /// A hearing fed by what is sent into the sender it comes with.
    #[must_use]
    pub fn new() -> (mpsc::UnboundedSender<StreamEvent>, Self) {
        let (sender, came) = mpsc::unbounded_channel();
        (sender, Self { came })
    }

    /// Everything the model wrote since this was last asked, waiting for at least one piece;
    /// `None` once the ask ended and all it wrote was handed on.
    pub async fn next(&mut self) -> Option<Vec<StreamEvent>> {
        let first = self.came.recv().await?;
        let mut all = vec![first];
        while let Ok(more) = self.came.try_recv() {
            all.push(more);
        }
        Some(all)
    }
}

/// Ask `model` once, named `key`, by `deadline`, on a thread it may block, handing what it
/// writes meanwhile to `doing`: its answer, or why none came.
pub(super) async fn ask<T, M: Model + 'static>(
    model: &Arc<Mutex<M>>,
    request: ModelRequest,
    key: String,
    deadline: Instant,
    doing: &impl Doing<T>,
) -> Result<ModelResponse, String> {
    let (written, hearing) = Hearing::new();
    let carrier = Arc::clone(model);
    let asking = tokio::task::spawn_blocking(move || {
        carrier
            .lock_recover()
            .turn_streaming(&request, &key, deadline, &mut |event| {
                // Whoever hears may have stopped listening: the answer is what counts.
                let _ = written.send(event);
            })
    });
    let (answered, ()) = tokio::join!(asking, doing.hear(hearing));
    let answered = answered.unwrap_or_else(|error| Err(error.to_string()));
    if let Ok(response) = &answered {
        doing.heard(response).await;
    }
    answered
}
