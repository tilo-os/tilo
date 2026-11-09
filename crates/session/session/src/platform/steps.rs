//! What the session's own actions ask of the session that holds them.
//!
//! The session's own actions — the arrangement, its undo, the windows, the notices — are decided
//! by the broker like any other step and carried out on the session's pass, where the arrangement
//! lives on the thread that shows it. A role that offers them names this seam; the host's own
//! doors answer it (the native session's door queue, the page's own).

use tilo_actions::ActionAddress;
use tilo_actions::remote::Completer;

/// A step of the session's own actions, answered on its pass.
pub(crate) trait SessionSteps: Send + Sync {
    /// Run `address` as `actor` with `slot` (its arguments in the slot form the broker carries):
    /// completed on the session's pass, which is where the arrangement and the windows are. The
    /// answer is `None` for a step the session accepted and carries on with.
    ///
    /// `beside` is the place the ask is in, by the resource that place shows — the conversation
    /// of the turn an agent asks in — or empty for the place in front. A step that opens a place
    /// stands it there (`docs/designs/app-language/one-app-model.md` §3).
    fn invoke_step(
        &self,
        actor: String,
        address: ActionAddress,
        slot: Option<String>,
        beside: String,
        done: Completer<Result<Option<tilo_actions::ActionOutput>, String>>,
    );
}
