//! What carries the model turns of the agents a host runs for the people they act for ([`PlanTurns`]),
//! holding its credential: its callers see requests and responses only.

use std::time::Instant;

use agent_provider_config::protocol::{ModelError, ModelRequest, ModelResponse, StreamEvent};

/// What answered a turn's calls: the provider as the person named it, and the
/// model's id. A turn keeps it on its reply, so a conversation says which
/// model wrote each answer even after the person switched models.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answered {
    pub provider: String,
    /// The model's id, as its provider names it.
    pub model: String,
    /// The model as a person reads it, where its provider labels it.
    pub label: String,
}

/// What carries a model's turns, holding the credential.
pub trait PlanTurns: Send {
    /// Answer one turn's `request`. `key` names the turn, the same for
    /// every attempt at it, so a metered endpoint bills it once; `retry`
    /// says this is the turn asked again after its first call was lost to
    /// the network, for the usage log.
    fn turn(
        &mut self,
        request: &ModelRequest,
        key: &str,
        deadline: Instant,
        retry: bool,
    ) -> Result<ModelResponse, ModelError>;

    /// [`Self::turn`], handing what the model writes to `written` while it writes: the
    /// answer's words as they arrive, for whoever shows the answer growing. Presentation
    /// only: the answer returned is what counts. What carries its turns without anything
    /// to hand on answers whole, and `written` hears nothing.
    fn turn_streaming(
        &mut self,
        request: &ModelRequest,
        key: &str,
        deadline: Instant,
        retry: bool,
        written: &mut (dyn FnMut(StreamEvent) + Send),
    ) -> Result<ModelResponse, ModelError> {
        let _ = written;
        self.turn(request, key, deadline, retry)
    }

    /// The model's name, for the usage log.
    fn model(&self) -> &str;

    /// What answered the last call: a person's own provider and its model,
    /// where the turns are theirs; `None` for a model the server provides itself,
    /// which has no provider of the person's to name.
    fn answered_by(&self) -> Option<Answered> {
        None
    }

    /// Whether the model is shown the pictures a tool answers with. One that
    /// is not reads that it was not shown them.
    fn sees(&self) -> bool {
        false
    }

    /// The most characters of conversation the model reads at once, by what
    /// its descriptor knows of its context: a turn sizes what it keeps to it
    /// rather than to a bound one model's window may not hold. `None` for a
    /// model that says nothing of its window, which then reads the default.
    fn context_chars(&self) -> Option<usize> {
        None
    }
}
