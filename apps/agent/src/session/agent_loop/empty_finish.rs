//! A finish that did nothing: words for a request to do something, before any step was taken,
//! are held once ([`LoopState::hold_empty_finish`]).

use super::LoopState;
use crate::tools;

/// Whether a turn made any call other than its finish, and whether a finish that did nothing was
/// held once already: a request that only needed words (a greeting, a thank-you, an opinion) is
/// answered by the same reply sent again.
#[derive(Debug, Default)]
pub(super) struct EmptyFinish {
    pub(super) acted: bool,
    pub(super) held: bool,
}

impl EmptyFinish {
    /// The turn made the call `name`: anything but its finish is a step, a look or a program.
    pub(super) fn saw(&mut self, name: &str) {
        if name != tools::PLAN {
            self.acted = true;
        }
    }
}

impl LoopState {
    /// What the loop answers a finish sent as the model's first call, before any step, for a
    /// request to do something (not a question, and more than a few words): nothing was done, so
    /// do it now, or,
    /// when the request only needed words, send the same reply again. `None` lets it finish: a
    /// step was taken, the request is a question or a few words of greeting, the model named what
    /// it cannot do, or the finish was held once already.
    pub(super) fn hold_empty_finish(&mut self, request: &str, replied: bool) -> Option<String> {
        let words = request.split_whitespace().count();
        // A finish without words, and a request whose parts are known, have their own checks.
        if !replied
            || !self.parts.is_empty()
            || self.empty_finish.acted
            || self.turn > 0
            || self.completed > 0
            || self.progressed > 0
            || !self.done_apps.is_empty()
            || self.question
            || words < 4
            || !self.unable.is_empty()
            || std::mem::replace(&mut self.empty_finish.held, true)
        {
            return None;
        }
        Some(
            "Not finished: no step was taken, so nothing the request asks for has been done. Do \
             it now with an operation or a program. If the request only needed words (a greeting, \
             a thank-you, an opinion), call plan again with the same reply."
                .to_owned(),
        )
    }
}
