//! An agent's run on an issue.
//!
//! A person who asks an agent, or gives it an issue, starts run *n*+1 then,
//! acting for them; work an agent hands on waits for a person's
//! `approve-run`. The run has no cap of its own: what bounds it is the
//! budget its person sets. The agent reports its phase
//! and its findings with the run verbs, which post into the issue's thread,
//! asks before an action whose answer is "ask first", and finishes. Each of
//! these is an action on the tracker, so who may approve, steer, answer or
//! report is decided by the same grants as any other action.

use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, Atom, Counter, JsonSchema, Model, Refusal};

/// The longest turn a budget admits: 15 minutes.
pub const MAX_TURN_MS: u64 = 900_000;
/// The longest run a budget admits: two hours.
pub const MAX_TOTAL_MS: u64 = 7_200_000;
/// The most model turns a budget admits for one run: the same the run's
/// envelope admits (`collab_types::task_budget::MAX_RUN_TURNS`, held equal
/// by the shell's tracker-runs test).
pub const MAX_TURNS: u64 = 10_000;
/// Most per-step receipts a run keeps: the rest of the run still counts
/// towards its totals and its budget, only their receipts are not listed.
pub const MAX_SPEND_STEPS: u64 = 64;
/// The longest model name a receipt keeps.
pub const MAX_MODEL_CHARS: usize = 96;
/// The longest device name a receipt keeps.
pub const MAX_DEVICE_CHARS: usize = 96;
/// The longest reason a pause keeps, in characters.
pub const MAX_PAUSE_REASON_CHARS: usize = 240;

/// Where a run is.
#[derive(Model, Default, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum RunPhase {
    /// A person approved it; its agent has not taken it up yet.
    #[default]
    Approved,
    /// Its agent took it up.
    Accepted,
    /// Its agent is deciding what to do.
    Planning,
    /// Its agent is acting.
    Executing,
    /// Its agent waits for a person to answer an action that asks first.
    Waiting,
    /// It finished what it was asked.
    Done,
    /// It could not finish.
    Failed,
    /// A person stopped it.
    Stopped,
}

impl RunPhase {
    /// Every phase, in the order a run moves through them, so a pause can
    /// name the one it holds as a number.
    pub const ALL: &'static [Self] = &[
        Self::Approved,
        Self::Accepted,
        Self::Planning,
        Self::Executing,
        Self::Waiting,
        Self::Done,
        Self::Failed,
        Self::Stopped,
    ];

    /// Whether the run is still going.
    #[must_use]
    pub const fn active(self) -> bool {
        matches!(
            self,
            Self::Approved | Self::Accepted | Self::Planning | Self::Executing | Self::Waiting
        )
    }

    /// Whether the run is held at a boundary a person owns, waiting for the
    /// answer to an action that asks first.
    #[must_use]
    pub const fn held(self) -> bool {
        matches!(self, Self::Waiting)
    }

    /// Whether an agent may report this phase while it works: the phases a
    /// run moves through on its own.
    #[must_use]
    pub const fn reportable(self) -> bool {
        matches!(self, Self::Accepted | Self::Planning | Self::Executing)
    }

    /// The Fluent id naming the phase.
    #[must_use]
    pub const fn label_key(self) -> &'static str {
        match self {
            Self::Approved => "tasks-run-approved",
            Self::Accepted => "tasks-run-accepted",
            Self::Planning => "tasks-run-planning",
            Self::Executing => "tasks-run-executing",
            Self::Waiting => "tasks-run-waiting",
            Self::Done => "tasks-run-done",
            Self::Failed => "tasks-run-failed",
            Self::Stopped => "tasks-run-stopped",
        }
    }
}

/// How a run ended, as its agent reports it.
#[derive(Model, Default, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum RunOutcome {
    /// It did what it was asked: the issue goes to review.
    #[default]
    Done,
    /// It could not: the issue goes back to Todo, to approve again.
    Failed,
    /// It stopped because a person asked it to.
    Stopped,
}

/// What an issue's runs may spend, and for how long they may work.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq)]
pub struct Budget {
    /// The longest one model turn may take, in milliseconds.
    pub turn_ms: u64,
    /// The longest the whole run may take, in milliseconds.
    pub total_ms: u64,
    /// The most model turns one run may take; None for a run bounded by
    /// its time alone.
    pub max_turns: Option<u64>,
    /// The most tokens the issue's runs may spend, input and output
    /// together, counting its sub-issues'. None for no limit.
    pub max_tokens: Option<u64>,
    /// The most calls priced per call, such as a web search.
    pub max_priced_calls: Option<u64>,
    /// The most money, in micro-units of `currency`.
    pub max_cost_micros: Option<u64>,
    /// The currency `max_cost_micros` is in; empty when no money is limited.
    pub currency: String,
}

impl Budget {
    /// Whether a turn ceiling is one a run may have.
    #[must_use]
    pub fn admits_turns(max_turns: Option<u64>) -> bool {
        max_turns.is_none_or(|turns| (1..=MAX_TURNS).contains(&turns))
    }

    /// Whether a budget of these ceilings is one a run may have: each within
    /// its range, a turn no longer than the run.
    #[must_use]
    pub fn admits(turn_ms: u64, total_ms: u64) -> bool {
        let turn = (1..=MAX_TURN_MS).contains(&turn_ms);
        let total = (1..=MAX_TOTAL_MS).contains(&total_ms);
        turn && total && turn_ms <= total_ms
    }

    /// Whether it limits nothing a run spends: no tokens, priced calls or
    /// money.
    #[must_use]
    pub fn spends_freely(&self) -> bool {
        self.max_tokens.is_none()
            && self.max_priced_calls.is_none()
            && self.max_cost_micros.is_none()
    }

    /// Whether a run that settled `spent` has reached this budget, and why
    /// the next step may not be taken. The reason names the limit.
    #[must_use]
    pub fn reached(&self, spent: &Spend) -> Option<String> {
        if let Some(max) = self.max_tokens
            && spent.tokens() >= max
        {
            return Some(format!(
                "This task's budget of {max} tokens is spent, so it waits for you to raise it"
            ));
        }
        if let Some(max) = self.max_priced_calls
            && spent.priced_calls >= max
        {
            return Some(format!(
                "This task's budget of {max} priced calls is spent, so it waits for you to raise it"
            ));
        }
        if let Some(max) = self.max_cost_micros
            && spent.cost() >= max
        {
            let named = self.currency.trim();
            return Some(
                format!(
                    "This task's budget of {} {named} is spent, so it waits for you to raise it",
                    money(max)
                )
                .replace("  ", " "),
            );
        }
        None
    }
}

/// Micro-units as a person reads money: whole units and hundredths.
#[must_use]
pub fn money(micros: u64) -> String {
    format!(
        "{}.{:02}",
        micros / 1_000_000,
        (micros % 1_000_000) / 10_000
    )
}

/// A model of one provider, chosen for an issue's runs in place of the one
/// chosen for its agent.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ModelChoice {
    /// The provider profile, by id.
    pub profile: String,
    /// The model, by its id at that provider.
    pub model: String,
}

/// What an issue's goal is: an ordinary resource of the workspace, above the
/// tasks written for it.
#[derive(Model, Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Area {
    /// The resource's address in the app that keeps it.
    pub resource: String,
    /// The app that keeps it (`so.tilo.notes`).
    pub app: String,
    /// The resource's kind (`notes.note`).
    pub kind: String,
    /// How it reads where it is named, such as its title.
    pub label: String,
}

impl Area {
    /// Whether this names a resource: an address and the app keeping it.
    #[must_use]
    pub fn names_a_resource(&self) -> bool {
        !self.resource.trim().is_empty() && !self.app.trim().is_empty()
    }
}

/// What a run asks its person about, as the door that paused it says.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Question {
    /// The question's id.
    pub id: String,
    /// The app whose action it is.
    pub app: String,
    /// The action's id.
    pub action: String,
    /// The resource it acts on.
    pub resource: String,
    /// The digest of the exact arguments asked about.
    pub arguments_digest: String,
    /// The action's draft: one field per parameter, secrets withheld.
    pub draft: serde_json::Value,
    /// The change the step makes, before and after, where it previews one:
    /// the person decides on the difference rather than on the end state
    /// alone. Absent for a step whose change is not a tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<serde_json::Value>,
}

/// Where a confirmation is.
#[derive(Model, Default, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ConfirmationState {
    /// Nobody has answered yet.
    #[default]
    Waiting,
    Confirmed,
    Declined,
}

/// What one step of a run costs: one model turn, as its provider counted it,
/// and the priced calls it made besides.
///
/// A step is charged when it starts, from what the agent estimates it will
/// use, and settled when it completes: the settled figure is what the run's
/// totals and its budget are measured against. A step whose provider
/// answered with an error settles at nothing, so a failed call never spends
/// a task's budget.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StepSpend {
    /// The turn's number in the run, from 1.
    pub step: u64,
    /// The model the turn used, as its provider names it. Empty for a run
    /// whose steps a person plans, or matched to the request's words.
    pub model: String,
    /// The device that paid: the one the agent ran on.
    pub device: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Calls priced per call, such as a web search.
    pub priced_calls: u64,
    /// Micro-units of `currency`: `0` for a run on a local model, which
    /// spends the device's time and tokens rather than money.
    pub cost_micros: u64,
    /// The currency `cost_micros` is in; empty when nothing was priced.
    pub currency: String,
}

impl StepSpend {
    /// The tokens the step spent, input and output together.
    #[must_use]
    pub fn tokens(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }
}

/// What a run spent, step by step.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Spend {
    /// The most recent steps, oldest first, at most [`MAX_SPEND_STEPS`].
    pub steps: Vec<StepSpend>,
    /// The run's steps, listed in `steps` or dropped past the bound.
    pub steps_taken: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub priced_calls: u64,
    pub cost_micros: u64,
    /// Never two currencies in one run: a step priced in another is refused.
    pub currency: String,
    /// What is held for the step in flight, before it settles.
    pub held_tokens: u64,
    pub held_cost_micros: u64,
}

impl Spend {
    /// The tokens the run spent, input and output together, settled steps
    /// and the held estimate.
    #[must_use]
    pub fn tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.output_tokens)
            .saturating_add(self.held_tokens)
    }

    /// The money the run spent, in micro-units, with the held estimate.
    #[must_use]
    pub fn cost(&self) -> u64 {
        self.cost_micros.saturating_add(self.held_cost_micros)
    }

    /// Whether nothing is held for a step: every step taken has settled.
    #[must_use]
    pub fn is_settled(&self) -> bool {
        self.held_tokens == 0 && self.held_cost_micros == 0
    }

    /// Hold `tokens` and `cost_micros` for the step that is about to start.
    /// A hold is the agent's estimate; the settled receipt replaces it.
    ///
    /// # Errors
    /// Refuses a second hold while one is out, which would let a run spend
    /// its budget twice over.
    pub fn hold(&mut self, tokens: u64, cost_micros: u64) -> Result<(), Refusal> {
        if !self.is_settled() {
            return Err(Refusal::new("The step before this one has not settled"));
        }
        self.held_tokens = tokens;
        self.held_cost_micros = cost_micros;
        Ok(())
    }

    /// Settle the held step with `step`, the receipt its provider gave.
    ///
    /// # Errors
    /// Refuses a receipt in another currency than the run's.
    pub fn settle(&mut self, step: &StepSpend) -> Result<(), Refusal> {
        if !self.currency.is_empty() && !step.currency.is_empty() && self.currency != step.currency
        {
            return Err(Refusal::new(
                "A run keeps its cost in one currency: this step is priced in another",
            ));
        }
        if self.currency.is_empty() {
            self.currency.clone_from(&step.currency);
        }
        self.held_tokens = 0;
        self.held_cost_micros = 0;
        self.steps_taken = self.steps_taken.saturating_add(1);
        self.input_tokens = self.input_tokens.saturating_add(step.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(step.output_tokens);
        self.priced_calls = self.priced_calls.saturating_add(step.priced_calls);
        self.cost_micros = self.cost_micros.saturating_add(step.cost_micros);
        self.steps.push(step.clone());
        if self.steps.len() > usize::try_from(MAX_SPEND_STEPS).unwrap_or(usize::MAX) {
            self.steps.remove(0);
        }
        Ok(())
    }
}

/// The most an issue's runs may spend, as `set-execution-budget` is given
/// it. Every limit is optional: the one given is the one written.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpendLimits {
    /// Tokens, input and output together, counting the sub-issues'.
    pub max_tokens: Option<u64>,
    /// Calls priced per call, such as a web search.
    pub max_priced_calls: Option<u64>,
    /// Money, in micro-units of `currency`.
    pub max_cost_micros: Option<u64>,
    /// The currency the money limit is in, such as `EUR`.
    pub currency: String,
}

impl SpendLimits {
    /// Whether it limits nothing.
    #[must_use]
    pub fn is_unlimited(&self) -> bool {
        self.max_tokens.is_none()
            && self.max_priced_calls.is_none()
            && self.max_cost_micros.is_none()
    }

    /// The limits, checked: a token limit above nothing, and a money limit
    /// naming its currency.
    ///
    /// # Errors
    /// A limit no run can work under, or money with no currency.
    pub fn checked(mut self) -> Result<Self, Refusal> {
        if self.max_tokens == Some(0) {
            return Err(Refusal::new("A token budget is more than nothing"));
        }
        if self.max_priced_calls == Some(0) {
            return Err(Refusal::new("A budget for priced calls is more than none"));
        }
        if self.max_cost_micros == Some(0) {
            return Err(Refusal::new("A money budget is more than nothing"));
        }
        self.currency = self.currency.trim().to_owned();
        if self.max_cost_micros.is_some() && self.currency.is_empty() {
            return Err(Refusal::new("Name the currency the money limit is in"));
        }
        if self.currency.chars().count() > MAX_CURRENCY_CHARS {
            return Err(Refusal::new("A currency is its code, such as EUR"));
        }
        Ok(self)
    }
}

/// The longest currency code a limit keeps.
pub const MAX_CURRENCY_CHARS: usize = 8;

/// An action a run asks its person before taking.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Confirmation {
    /// The question's id.
    pub id: String,
    /// The app whose action it is.
    pub app: String,
    /// The action's id.
    pub action: String,
    /// The resource it acts on.
    pub resource: String,
    /// Hex of the digest of the exact arguments asked about: a
    /// confirmation admits those arguments (with the person's edits) and no
    /// others.
    pub arguments_digest: String,
    /// What it would do, as a draft the person can edit: one field per
    /// parameter, secrets withheld (`tilo_actions::draft`). Shaped by the
    /// door that paused the run, never by this record.
    pub draft: serde_json::Value,
    /// The change it makes, before and after, where it previews one. Shaped
    /// by the door that paused the run like the draft, and carried so the
    /// host puts the same two sides to the person who answers.
    pub preview: Option<serde_json::Value>,
    pub state: ConfirmationState,
    /// The person's edits to the draft, when they confirmed with edits.
    pub edits: serde_json::Value,
    /// Why the person declined.
    pub reason: String,
    /// Who answered.
    pub answered_by: String,
}

/// One run of an agent on an issue.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Run {
    /// The run's number on its issue, from 1.
    pub epoch: u64,
    /// The agent working, as the door names it (`agent:<id>`).
    pub agent: Atom,
    /// The person who approved it: the agent acts for them.
    pub starter: String,
    /// The task delegation the starter's device signed for the task agent
    /// made for this run, hex: its authority, verified by the broker. `None`
    /// for a collaborator's run.
    pub delegation: Option<String>,
    /// Where it stopped, when a person paused it: the phase it goes on in
    /// and the message boundary it reached. A paused run takes no new step,
    /// and resuming it continues from there, so what it already did is never
    /// done again.
    pub paused: Option<PausedRun>,
    pub phase: RunPhase,
    /// The model the run plans with, as the issue chose it when the run
    /// started; the agent's own without.
    pub model: Option<ModelChoice>,
    /// The device it runs on (its name, as Devices lists it): the device the
    /// last person to hand it over ran it from. Absent until one says so,
    /// and on a run that never ran anywhere.
    pub device: Option<String>,
    pub started_ms: i64,
    /// When it last reported, or ended.
    pub updated_ms: i64,
    /// What it spent, step by step, and the step in flight.
    pub spend: Option<Spend>,
    /// The action it waits on, or the last one it asked about.
    pub confirmation: Option<Confirmation>,
}

impl Run {
    /// The step the run reads as now: the phase a person's pause holds it in
    /// where one does, so a paused run never reads as if it were working.
    #[must_use]
    pub fn shown_phase(&self) -> RunPhase {
        self.paused
            .as_ref()
            .and_then(PausedRun::phase)
            .unwrap_or(self.phase)
    }

    /// Whether the run waits for its person's answer to an action that asks
    /// first: the one state a run cannot leave on its own.
    #[must_use]
    pub fn asks_its_person(&self) -> bool {
        self.paused.is_none()
            && self.shown_phase() == RunPhase::Waiting
            && self
                .confirmation
                .as_ref()
                .is_some_and(|confirmation| confirmation.state == ConfirmationState::Waiting)
    }

    /// The Fluent id naming the run as the person reads it: the step it is
    /// in, or that a person paused it.
    #[must_use]
    pub fn state_key(&self) -> &'static str {
        if self.paused.is_some() {
            return "tasks-run-paused";
        }
        self.phase.label_key()
    }
}

/// Where a paused run stopped, as the person paused it.
///
/// Every field is written on its own, last writer wins: a pause is a person's
/// decision about the whole run, so nothing another replica reported about
/// the step it was in may merge into the record and move it. `held` carries
/// the step, which is what makes the pause a value of this record rather
/// than a step of the run.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PausedRun {
    /// The step it goes on in, as `RunPhase::ALL` numbers it: `0` is a pause
    /// nobody wrote.
    #[serde(default)]
    pub held: Counter,
    /// How many messages to the run existed then: it goes on after them.
    pub through: u64,
    /// When it was paused, in milliseconds since the epoch.
    pub at_ms: i64,
    /// Why it was paused: a person's decision, or the budget it reached.
    /// Empty when nobody named one.
    pub reason: String,
}

impl PausedRun {
    /// `run` as the step it was paused in, `through` messages into it.
    /// `None` for a step this record has no name for.
    #[must_use]
    pub fn of(phase: RunPhase, through: u64, at_ms: i64) -> Option<Self> {
        let held = RunPhase::ALL
            .iter()
            .position(|step| *step == phase)
            .and_then(|index| i64::try_from(index).ok())
            .map(|index| index.saturating_add(1))?;
        Some(Self {
            held: Counter::new(held),
            through,
            at_ms,
            reason: String::new(),
        })
    }

    /// The step the run goes on in, or `None` where there is no pause.
    #[must_use]
    pub fn phase(&self) -> Option<RunPhase> {
        let index = usize::try_from(self.held.get()).ok()?.checked_sub(1)?;
        RunPhase::ALL.get(index).copied()
    }

    /// This pause, saying `reason` when one is known.
    #[must_use]
    pub fn because(mut self, reason: &str) -> Self {
        let reason = reason.trim();
        self.reason = reason.chars().take(MAX_PAUSE_REASON_CHARS).collect();
        self
    }
}
