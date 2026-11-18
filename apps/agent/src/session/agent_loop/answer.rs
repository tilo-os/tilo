//! The words the person reads at the end of a run: the model's own, asked for once when a turn
//! would end the run without them, never a line the run writes in their place. An empty or
//! stand-in reply ("…", "placeholder") is no answer ([`tools::says_nothing`]).

use super::{Failure, LoopEnd, LoopState, head};
use crate::model::Conversation;
use crate::tools;

/// What the model is told when it finished with work done and nothing written
/// for the person: its own words are the reply, never a line it never wrote.
pub(super) const SAY_WHAT_YOU_DID: &str = "You finished without telling the person what you did. Call `plan` again with your reply: \
     one or two sentences, in their words, saying what you did. Do not repeat any step.";
/// What the model is told after a turn that ended with nothing said to the person (an empty reply,
/// "…", "placeholder"), after the reason: the answer is its own words, asked for once.
const ANSWER_IN_WORDS: &str = "It tells the person nothing. Answer the request now: call `plan` \
     with your whole answer to the person in `reply`, in real words. Do not repeat any step.";

impl LoopState {
    /// A model turn that failed after every listed part of the request
    /// already has a successful operation ends the task as finished: the
    /// work is done, only the model's closing turn was lost (e.g. it ran
    /// into its token limit). The reply is the last words the model wrote for
    /// the person, on whatever call carried them and whether or not that call
    /// succeeded — never a line the run writes in its place. When the model
    /// wrote nothing, it is asked once for the words; still none, the run ends
    /// without a reply rather than with one nobody said.
    pub(super) fn finished_despite_failed_plan(
        &mut self,
        conversation: &mut Conversation,
        error: &str,
    ) -> Option<LoopEnd> {
        if self.completed == 0 || self.parts.is_empty() || !self.undone_parts().is_empty() {
            return None;
        }
        if self.reply.is_none() && !self.asked_for_words {
            self.asked_for_words = true;
            tracing::info!(
                error,
                completed = self.completed,
                "closing model turn failed after every part was done; asking for the words"
            );
            conversation.push_user(SAY_WHAT_YOU_DID);
            return None;
        }
        tracing::info!(
            error,
            completed = self.completed,
            said = self.reply.is_some(),
            "closing model turn failed after every part was done; finishing"
        );
        Some(LoopEnd::Finished(
            tools::Finish {
                reply: self.reply.clone(),
                done: true,
                view: None,
            },
            self.completed,
        ))
    }

    /// A turn that failed after the model was asked for the words, and left
    /// none: the run ends with the clean failure row, since the person gets no
    /// answer the model said. `None` when the turn failed for another reason
    /// (a model that never reached this point, a part still undone).
    pub(super) fn failed_after_asking_for_words(&self, error: &str) -> Option<LoopEnd> {
        if !self.asked_for_words || self.reply.is_some() {
            return None;
        }
        if self.completed == 0 || self.parts.is_empty() || !self.undone_parts().is_empty() {
            return None;
        }
        // Every part was done: the person reads that the steps ran and no words came, never
        // that nothing was done.
        Some(LoopEnd::Failed(Failure::spoken(
            &crate::i18n::t("agent-failure-steps-unsaid"),
            error,
        )))
    }

    /// A turn that would end the run with nothing said to the person: no reply, or words that say
    /// nothing (`said`: "…", "placeholder", nothing at all; [`tools::says_nothing`]). That is never
    /// an answer. The model is told why and asked once for its answer; one that still gives none
    /// ends the run with the readable failure, the words it gave kept as the resource. `None` while
    /// the model has its turn to answer.
    pub(super) fn unanswered(
        &mut self,
        conversation: &mut Conversation,
        said: &str,
    ) -> Option<LoopEnd> {
        let said = said.trim();
        let reason = if said.is_empty() {
            "the model's answer was empty".to_owned()
        } else {
            format!("the model's answer was only {:?}", head(said, 80))
        };
        if self.asked_for_words {
            tracing::info!(%reason, "the model gave no answer after being asked; failing");
            return Some(LoopEnd::Failed(Failure::spoken(
                &crate::i18n::t("agent-failure-empty-answer"),
                &reason,
            )));
        }
        self.asked_for_words = true;
        tracing::info!(%reason, "the model gave no answer; asking for it once");
        conversation.push_user(&format!("Not an answer: {reason}. {ANSWER_IN_WORDS}"));
        None
    }
}
