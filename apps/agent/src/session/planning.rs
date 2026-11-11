//! What a run tells its person about how it decided.
//!
//! A turn's words land in the conversation that asked, in its reply beside
//! the actions it took, so they are the only place a person learns which
//! model planned the work, or why nothing planned it at all.
//! Each sentence here names the state, the outcome and the next step — never
//! the run's internals ("no api key", a rules-only survey) — and every one of
//! them is reported through [`super::Session::report_task`], so it is a
//! reported phase of the turn rather than a line in a log nobody reads.

/// Where a run's steps came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Planner {
    /// A model chose them, named as the person knows it.
    Model { provider: String, model: String },
    /// The published actions answered the request's own words: the run was
    /// set up without a model, or none could be reached.
    Words { reason: Words },
}

/// Why a run's steps were matched to the request's words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Words {
    /// The run itself is planned without a model (its issue asks for that).
    Asked,
    /// The run asked for a model and there is none to ask.
    NoModel,
}

impl Planner {
    /// The note a person reads when the run starts deciding.
    pub(super) fn announcing(&self) -> String {
        match self {
            Self::Model { provider, model } => {
                format!("Planning with {model} from {provider}.")
            }
            Self::Words {
                reason: Words::Asked,
            } => "This run is planned without a model, so its steps are matched to the actions \
                  this workspace's apps publish."
                .to_owned(),
            Self::Words {
                reason: Words::NoModel,
            } => "No model is set up to plan this run, so its steps are matched to the actions \
                  this workspace's apps publish. Choose a model in Settings, under Agent models, to \
                  have one plan instead."
                .to_owned(),
        }
    }

    /// Whether a model chose this run's steps, so the words-only planner's
    /// refusals are not this run's to give.
    pub(super) fn is_model(&self) -> bool {
        matches!(self, Self::Model { .. })
    }
}

/// What to say when a run's steps could not be chosen: why no single action
/// answered the request, and — when no model planned it — the step that
/// would let one.
pub(super) fn unmatched(plan: &Planner, why: &str) -> String {
    match plan {
        Planner::Model { .. } => why.to_owned(),
        Planner::Words {
            reason: Words::NoModel,
        } => format!(
            "{why} No model is set up to plan one, so choose a model in Settings, under Agent \
             models, or name the app to work in."
        ),
        Planner::Words {
            reason: Words::Asked,
        } => format!("{why} Name the app to work in, or say what to do in it."),
    }
}

/// How a run that the published actions decided ended, said as one sentence
/// so the outcome does not read as a model's work. `None` for a
/// model-planned run: its result is the model's own reply, and the agent
/// writes no line in its place.
pub(super) fn finished(plan: &Planner, actions: usize) -> Option<String> {
    match plan {
        Planner::Model { .. } => None,
        Planner::Words {
            reason: Words::NoModel,
        } => Some(format!(
            "Done: {actions} action(s) matched to the request's words, without a model."
        )),
        Planner::Words {
            reason: Words::Asked,
        } => Some(format!(
            "Done: {actions} action(s) matched to the request's words."
        )),
    }
}

/// Why a run that asked for a model has none, with the step that fixes it.
///
/// A run whose model call failed says the model didn't answer, in the words a
/// person reads; the transport's own error is the record kept beside it.
pub(super) fn no_model(reason: &crate::model::Route) -> crate::session::Failure {
    use crate::model::{Fallback, Route};
    match reason {
        Route::Keywords(Fallback::NoKey) => crate::session::Failure::said(
            "No model is set up to plan this run. Choose one in Settings, under Agent models, \
             then run this again.",
        ),
        Route::Keywords(Fallback::Disabled) => crate::session::Failure::said(
            "This agent runs with models turned off, so nothing can plan this run. Start it \
             again with a model turned on.",
        ),
        Route::Keywords(Fallback::Failed(error)) => crate::session::Failure::spoken(
            "The model didn't answer, so this run couldn't be planned. Try again in a moment.",
            error,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> Planner {
        Planner::Model {
            provider: "Anthropic".into(),
            model: "Claude Sonnet".into(),
        }
    }

    #[test]
    fn a_run_without_a_model_says_so_and_names_the_door() {
        let plan = Planner::Words {
            reason: Words::NoModel,
        };
        assert!(plan.announcing().contains("No model is set up"));
        assert!(plan.announcing().contains("Agent models"));
        let unmatched = unmatched(
            &plan,
            "No published action carries the words of the request.",
        );
        assert!(unmatched.contains("No model is set up"));
        assert!(
            !unmatched.contains("permission"),
            "a missing model never reads as a missing permission: {unmatched}"
        );
    }

    #[test]
    fn a_run_asked_to_plan_without_a_model_does_not_blame_a_missing_one() {
        let plan = Planner::Words {
            reason: Words::Asked,
        };
        assert!(!plan.announcing().contains("No model is set up"));
        assert!(
            !unmatched(
                &plan,
                "No published action carries the words of the request."
            )
            .contains("Agent models")
        );
    }

    #[test]
    fn an_outcome_says_whether_a_model_produced_it() {
        // A model-planned run's result is the model's own reply: the agent
        // writes no line in its place.
        assert_eq!(finished(&model(), 2), None);
        assert_eq!(
            finished(
                &Planner::Words {
                    reason: Words::NoModel
                },
                1
            )
            .as_deref(),
            Some("Done: 1 action(s) matched to the request's words, without a model.")
        );
    }
}
