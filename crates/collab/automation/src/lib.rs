//! # Shortcuts: typed pipelines over the action door
//!
//! A [`Shortcut`] is a small program whose every effect is an ordinary
//! published action: the same `(app, resource, action)` addresses a person
//! reaches through the palette, an agent through the control socket and a
//! peer through [`ActionHost`](tilo_actions::remote::ActionHost). Nothing in
//! here touches an application directly, so a shortcut can do exactly what
//! its author could do by hand and no more.
//!
//! Three pieces, all pure:
//!
//! - [`shortcut`] — the serialisable program: inputs, [`Step`]s, value
//!   [`Binding`]s between them, the [`grant`](Shortcut::grant) that bounds its
//!   authority and the [`TriggerSpec`]s that may start it.
//! - [`check`] — the save-time type checker. It reads the descriptors a host
//!   returned from `query` (parameters, contracts, result specs) and refuses a
//!   binding whose type cannot satisfy the parameter it feeds, a step output
//!   pointer that the declared result schema does not have, an action outside
//!   the grant, and a shortcut that could run itself.
//! - [`run`] — the interpreter. It invokes each step as the person who
//!   started the run, only inside the shortcut's grant, with invocation ids
//!   derived from the run so a retried run observes what already happened
//!   instead of doing it twice, and it hands confirmation and questions to
//!   the caller instead of answering them itself.
//!
//! AI steps are not a second execution path: an [`Ai`](StepKind::Ai) step is
//! an invocation of the agent's transform actions ([`AGENT_APP_ID`],
//! [`AGENT_TRANSFORM_KIND`]), granted, confirmed and recorded like any other.

pub mod catalog;
pub mod check;
pub mod dispatch;
pub mod grant;
pub mod path;
pub mod program;
pub mod requirements;
pub mod run;
pub mod shortcut;
mod template;
mod types;

#[cfg(test)]
mod tests;

pub use catalog::{
    ActionCatalog, ResourceTarget, kind_matches, quick_action_arguments, quick_action_input,
};
pub use check::{
    BindingOption, CheckError, CheckErrorKind, NoShortcuts, ShortcutResolver, binding_options,
    check, check_bot, values_at,
};
pub use grant::{StepGrant, action_rule, grant_answer, step_node};
pub use path::StepPath;
pub use run::{
    AskRequest, ConfirmationRequest, RunHooks, RunId, RunMode, RunOutcome, RunReport, RunRequest,
    RunTrigger, STEP_FACET, StepFailure, StepRecord, StepStatus, invocation_id, run,
};
pub use shortcut::{
    ActorFilter, AiTransform, Binding, CatchUp, Condition, EVENT_INPUT, FOCUS_DO_NOT_DISTURB,
    FOCUS_SCHEDULE, FocusTransition, Schedule, ScheduleClaim, Selector, Shortcut, Step, StepKind,
    TimeOfDay, TriggerSpec, Weekday,
};

/// Application that publishes shortcuts as runnable actions.
pub const SHORTCUTS_APP_ID: &str = "so.tilo.shortcuts";
/// Resource kind of one saved shortcut; its id is
/// `shortcuts.shortcut:<shortcut id>` (see [`shortcut_resource_id`]).
pub const SHORTCUT_RESOURCE_KIND: &str = "shortcuts.shortcut";
/// Action on a shortcut resource that runs it.
pub const SHORTCUT_RUN_ACTION: &str = "run";

/// Application that publishes the model-backed transform actions an
/// [`Ai`](StepKind::Ai) step lowers to.
pub const AGENT_APP_ID: &str = "so.tilo.agent";
/// Resource kind carrying the transform actions.
pub const AGENT_TRANSFORM_KIND: &str = "agent.transform";
/// The one resource the agent publishes its transforms on.
pub const AGENT_TRANSFORM_RESOURCE: &str = "agent.transform:default";
/// Condense the input into a short text.
pub const AGENT_SUMMARISE_ACTION: &str = "summarise";
/// Pull a JSON value of a given schema out of the input.
pub const AGENT_EXTRACT_ACTION: &str = "extract";
/// Pick exactly one of a fixed set of labels for the input.
pub const AGENT_CLASSIFY_ACTION: &str = "classify";
/// Answer a free-form prompt about the input.
pub const AGENT_FREEFORM_ACTION: &str = "freeform";
/// Take a person's request to their agent: answer it when it asks for
/// information, or name the work when it asks for something to be done.
pub const AGENT_RESPOND_ACTION: &str = "respond";

/// The stable resource id under which shortcut `id` is published.
#[must_use]
pub fn shortcut_resource_id(id: &str) -> String {
    format!("{SHORTCUT_RESOURCE_KIND}:{id}")
}

/// The shortcut id a resource id names, when it names one.
#[must_use]
pub fn shortcut_id_of(resource_id: &str) -> Option<&str> {
    resource_id
        .strip_prefix(SHORTCUT_RESOURCE_KIND)?
        .strip_prefix(':')
        .filter(|id| !id.is_empty())
}
