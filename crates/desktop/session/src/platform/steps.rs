//! What the session's own actions ask of the session that holds them.
//!
//! The session's own actions — the layout, its undo, the windows, the notices — are decided
//! by the broker like any other step and carried out on the session's pass, where the layout
//! lives on the thread that shows it. A role that offers them names this service; the host's own
//! APIs answer it (the native session's API queue, the page's own).

use tilo_actions::ActionAddress;
use tilo_actions::remote::{ActionCallError, Completer};

/// A step of the session's own actions, answered on its pass.
pub(crate) trait SessionSteps: Send + Sync {
    /// Run `address` as `actor` with `slot` (its arguments in the slot form the broker carries),
    /// serving the invocation `serving` names, a step of the run it names ([`serving`]):
    /// completed on the session's pass, which is where the layout and the windows are. The
    /// answer is `None` for a step the session accepted and carries on with.
    ///
    /// `beside` is the place the ask is in, by the resource that place shows — the conversation
    /// of the turn an agent asks in — or empty for the place in front. A step that opens a place
    /// stands it there (`docs/designs/app-language/one-app-model.md` §3).
    fn invoke_step(
        &self,
        actor: tilo_actions::Actor,
        address: ActionAddress,
        slot: Option<String>,
        beside: String,
        serving: tilo_actions::ActionContext,
        done: Completer<Result<Option<tilo_actions::ActionOutput>, ActionCallError>>,
    );
}

/// What a step of the session's own actions serves: the invocation `request` is, and the run it
/// is a step of. What it changes in the layout names both, so taking the invocation back takes
/// the step back with it.
pub(crate) fn serving(
    request: &tilo_actions::remote::InvokeRequest,
) -> tilo_actions::ActionContext {
    tilo_actions::ActionContext {
        group: request
            .context
            .as_ref()
            .and_then(|context| context.group.clone()),
        source_invocation: Some(request.invocation),
        ..tilo_actions::ActionContext::default()
    }
}
