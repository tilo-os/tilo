//! What a run tells its person about how it decided.
//!
//! A run's words land on its issue's thread, beside the actions it took and
//! in its history, so they are the only place a person learns which model
//! planned the work, what that model cost, or why nothing planned it at all.
//! Each sentence here names the state, the outcome and the next step — never
//! the run's internals ("no api key", a rules-only survey) — and every one of
//! them is reported through [`super::Session::report_task`], so it is a
//! reported phase of the run rather than a line in a log nobody reads.

use agent_provider_config::protocol::Usage;

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
                  this room's apps publish."
                .to_owned(),
            Self::Words {
                reason: Words::NoModel,
            } => "No model is set up to plan this run, so its steps are matched to the actions \
                  this room's apps publish. Choose a model in Settings, under Agent models, to \
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

/// What a model-planned run cost, as the provider counted it: the model that
/// spent it and the tokens it reported. Never a price: no provider here has
/// told this agent what a token costs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Spend {
    /// Model turns answered in this run.
    pub(super) turns: u64,
    /// Tokens every turn spent on its input, cached reads included.
    pub(super) input: u64,
    /// The part of `input` the provider served from its prompt cache.
    pub(super) cached: u64,
    /// Tokens every turn produced.
    pub(super) output: u64,
}

impl Spend {
    /// Add one answered turn's usage. A provider that reports no counters
    /// leaves them zero, which [`Self::report`] says rather than guessing.
    pub(super) fn turn(&mut self, usage: Usage) {
        self.turns = self.turns.saturating_add(1);
        self.input = self.input.saturating_add(usage.input_tokens);
        self.cached = self.cached.saturating_add(usage.cached_input_tokens);
        self.output = self.output.saturating_add(usage.output_tokens);
    }

    /// The run's receipt, for the issue's thread: the model, the turns and
    /// the tokens, or that the provider counted none.
    pub(super) fn report(&self, plan: &Planner) -> String {
        let Planner::Model { provider, model } = plan else {
            return String::new();
        };
        // A run the model never answered spent nothing it counted: saying
        // "0 tokens" would read as a free run rather than as no receipt.
        if self.turns == 0 {
            return String::new();
        }
        let turns = match self.turns {
            1 => "1 model turn".to_owned(),
            turns => format!("{turns} model turns"),
        };
        if self.input == 0 && self.output == 0 {
            return format!("{model} from {provider} answered {turns} and reported no tokens.");
        }
        let count = ui::format::count;
        let cached = if self.cached == 0 {
            String::new()
        } else {
            format!(" ({} read from cache)", count(self.cached))
        };
        format!(
            "{model} from {provider} answered {turns}: {} tokens in{cached}, {} out.",
            count(self.input),
            count(self.output)
        )
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
/// so the outcome does not read as a model's work.
pub(super) fn finished(plan: &Planner, actions: usize) -> String {
    match plan {
        Planner::Model { .. } => format!("Completed {actions} action(s) the broker approved"),
        Planner::Words {
            reason: Words::NoModel,
        } => format!("Done: {actions} action(s) matched to the request's words, without a model."),
        Planner::Words {
            reason: Words::Asked,
        } => format!("Done: {actions} action(s) matched to the request's words."),
    }
}

/// Why a run that asked for a model has none, with the step that fixes it.
pub(super) fn no_model(reason: &crate::model::Route) -> String {
    use crate::model::{Fallback, Route};
    match reason {
        Route::Keywords(Fallback::NoKey) => {
            "No model is set up to plan this run. Choose one in Settings, under Agent models, \
             then run this again."
                .to_owned()
        }
        Route::Keywords(Fallback::Disabled) => {
            "This agent runs with models turned off, so nothing can plan this run. Start it \
             again with a model turned on."
                .to_owned()
        }
        Route::Keywords(Fallback::Failed(error)) => format!(
            "The model didn't answer, so this run couldn't be planned: {error}. Check the model \
             in Settings, under Agent models, then run this again."
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
    fn a_run_says_which_model_planned_it_and_what_it_spent() {
        let mut spend = Spend::default();
        spend.turn(Usage {
            input_tokens: 1_800,
            output_tokens: 240,
            cached_input_tokens: 1_000,
            ..Usage::default()
        });
        spend.turn(Usage {
            input_tokens: 200,
            output_tokens: 60,
            ..Usage::default()
        });
        assert_eq!(
            model().announcing(),
            "Planning with Claude Sonnet from Anthropic."
        );
        assert_eq!(
            spend.report(&model()),
            "Claude Sonnet from Anthropic answered 2 model turns: 2,000 tokens in (1,000 read \
             from cache), 300 out."
        );
    }

    #[test]
    fn a_provider_that_reports_no_counters_is_not_shown_as_free() {
        let mut spend = Spend::default();
        spend.turn(Usage::default());
        assert_eq!(
            spend.report(&model()),
            "Claude Sonnet from Anthropic answered 1 model turn and reported no tokens."
        );
    }

    #[test]
    fn a_run_without_a_model_says_so_and_names_the_door() {
        let plan = Planner::Words {
            reason: Words::NoModel,
        };
        assert!(plan.announcing().contains("No model is set up"));
        assert!(plan.announcing().contains("Agent models"));
        assert!(spend_report_is_empty(&plan), "no model, no receipt");
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

    fn spend_report_is_empty(plan: &Planner) -> bool {
        Spend::default().report(plan).is_empty()
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
        assert_eq!(
            finished(&model(), 2),
            "Completed 2 action(s) the broker approved"
        );
        assert_eq!(
            finished(
                &Planner::Words {
                    reason: Words::NoModel
                },
                1
            ),
            "Done: 1 action(s) matched to the request's words, without a model."
        );
    }
}
