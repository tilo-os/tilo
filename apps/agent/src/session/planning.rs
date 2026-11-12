//! What a run tells its person about how it decided.
//!
//! A turn's words land in the conversation that asked, in its reply beside
//! the actions it took, so they are the only place a person learns which
//! model planned the work, or why nothing planned it at all.
//! Each sentence here names the state, the outcome and the next step — never
//! the run's internals ("no api key", a rules-only survey) — and every one of
//! them is reported through [`super::Session::report_task`], so it is a
//! reported phase of the turn rather than a line in a log nobody reads.
//!
//! The words are the catalogue's ([`crate::i18n`]), in the person's language.

use crate::i18n::{t, t_args};
use ui::i18n::ArgValue;

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
            Self::Model { provider, model } => t_args(
                "agent-plan-model",
                &[
                    ("model", ArgValue::from(model.clone())),
                    ("provider", ArgValue::from(provider.clone())),
                ],
            ),
            Self::Words {
                reason: Words::Asked,
            } => t("agent-plan-words-asked"),
            Self::Words {
                reason: Words::NoModel,
            } => t("agent-plan-words-no-model"),
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
///
/// A run that asked for a model and has none says exactly that, once: the
/// words of the request were not matched to any action, and listing the
/// actions they came nearest says nothing a person can act on. What they can
/// act on is the one thing that makes the request playable.
pub(super) fn unmatched(plan: &Planner, why: &str) -> String {
    match plan {
        Planner::Model { .. } => why.to_owned(),
        Planner::Words {
            reason: Words::NoModel,
        } => t("agent-plan-no-model"),
        Planner::Words {
            reason: Words::Asked,
        } => t_args("agent-plan-unmatched-asked", &[("why", why.into())]),
    }
}

/// How a run that the published actions decided ended, said as one sentence
/// so the outcome does not read as a model's work.
pub(super) fn finished(plan: &Planner, actions: usize) -> String {
    let actions = ArgValue::from(actions);
    match plan {
        Planner::Model { .. } => t_args("agent-done-model", &[("actions", actions)]),
        Planner::Words {
            reason: Words::NoModel,
        } => t_args("agent-done-words-no-model", &[("actions", actions)]),
        Planner::Words {
            reason: Words::Asked,
        } => t_args("agent-done-words-asked", &[("actions", actions)]),
    }
}

/// Why a run that asked for a model has none, with the step that fixes it.
pub(super) fn no_model(reason: &crate::model::Route) -> String {
    use crate::model::{Fallback, Route};
    match reason {
        Route::Keywords(Fallback::NoKey) => t("agent-plan-no-model"),
        Route::Keywords(Fallback::Disabled) => t("agent-plan-models-off"),
        Route::Keywords(Fallback::Failed(error)) => {
            t_args("agent-fail-model-call", &[("error", error.as_str().into())])
        }
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
    fn a_run_without_a_model_says_so_once_and_names_the_door() {
        let plan = Planner::Words {
            reason: Words::NoModel,
        };
        assert!(plan.announcing().contains("No model is set up"));
        assert!(plan.announcing().contains("Agent models"));
        let unmatched = unmatched(
            &plan,
            "That could mean reveal message or copy link. Which one?",
        );
        assert!(
            unmatched.contains("no model is set up"),
            "the missing model is said plainly: {unmatched}"
        );
        assert!(unmatched.contains("Agent models"));
        assert!(
            !unmatched.contains("That could mean"),
            "the reply does not list actions the words came nearest: {unmatched}"
        );
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
        assert_eq!(finished(&model(), 2), "Completed 2 actions.");
        assert_eq!(
            finished(
                &Planner::Words {
                    reason: Words::NoModel
                },
                1
            ),
            "Done: one action matched to the request's words, without a model."
        );
    }

    /// A run on a German desktop says all of it in German, and names the
    /// Settings page by the name that page has there.
    #[test]
    fn a_german_desktop_reads_the_run_in_german() {
        let _german = crate::i18n::chosen("de-DE");
        crate::i18n::follow();
        let plan = Planner::Words {
            reason: Words::NoModel,
        };
        let announcing = plan.announcing();
        assert!(announcing.contains("kein Modell"), "{announcing}");
        assert!(announcing.contains("Agentenmodelle"), "{announcing}");
        let unmatched = unmatched(&plan, "egal");
        assert!(
            unmatched.contains("Einstellungen") && unmatched.contains("Agentenmodelle"),
            "{unmatched}"
        );
        assert_eq!(finished(&model(), 1), "Eine Aktion ausgeführt.");
        assert_eq!(finished(&model(), 2), "2 Aktionen ausgeführt.");
    }
}
