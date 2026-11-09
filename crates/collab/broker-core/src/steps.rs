//! What the broker's host serves the session, on every host: the steps a host offers beside its
//! apps' actions, who takes one, and how it is read and answered.
//!
//! A host serves two kinds of action: the ones its apps publish, and the ones the host itself
//! runs for the person ([`HostStepServices`]) — pairing a device, administering identities, the
//! session's own arrangement. Both are decided by the same admission, and a caller reads both
//! through one [`ActionHost`](tilo_actions::remote::ActionHost): what differs between a Linux
//! desktop and the page is only where the steps are carried out, which is the host's own
//! business, not the role's.

use std::sync::Arc;

use tilo_actions::remote::{ActionHost, Pending};
use tilo_actions::{ActionAddress, ActionDescriptor};
use tilo_actions::{ActionFailure, ActionOutput};

/// Who a host step is taken by, as the broker admitted it.
///
/// The broker decides what an actor may do from its grants, its delegation and the turn it acts
/// for ([`crate::actor`]'s native half); a step is handed only the facts it answers to the person
/// who asked and the agent that acted.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct StepActor {
    id: String,
    agent: bool,
    /// The person an agent acts for, when one delegated it.
    for_person: Option<String>,
    /// The issue whose run the agent works in, by its address.
    issue: Option<String>,
    /// The turn the agent acts for: the person's message it answers.
    turn: Option<tilo_action_policy::turn::Turn>,
    /// Whether it is the person at their own device: whose own gesture answers a grant that asks
    /// first, and the only actor a host lets govern a workspace.
    direct: bool,
}

impl StepActor {
    /// The actor `id` taking steps as `agent` or as a person.
    #[must_use]
    pub fn new(id: impl Into<String>, agent: bool) -> Self {
        Self {
            id: id.into(),
            agent,
            for_person: None,
            issue: None,
            turn: None,
            direct: !agent,
        }
    }

    /// The same actor, acting for `person`.
    #[must_use]
    pub fn for_person(mut self, person: Option<String>) -> Self {
        self.for_person = person;
        self
    }

    /// The same actor, working the issue at `issue`.
    #[must_use]
    pub fn on_issue(mut self, issue: Option<String>) -> Self {
        self.issue = issue;
        self
    }

    /// The same actor, acting inside `turn`.
    #[must_use]
    pub fn on_turn(mut self, turn: Option<tilo_action_policy::turn::Turn>) -> Self {
        self.turn = turn;
        self
    }

    /// The same actor, held to its own device (`direct`) or not.
    #[must_use]
    pub fn direct(mut self, direct: bool) -> Self {
        self.direct = direct;
        self
    }

    /// The policy principal id (`agent:`-prefixed for an agent).
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub fn is_agent(&self) -> bool {
        self.agent
    }

    /// The person this agent acts for, when one delegated it.
    #[must_use]
    pub fn acting_for(&self) -> Option<&str> {
        self.for_person.as_deref()
    }

    /// The issue whose run this actor works in, by its address.
    #[must_use]
    pub fn issue(&self) -> Option<&str> {
        self.issue.as_deref()
    }

    /// The turn this actor acts for, when it is an agent's.
    #[must_use]
    pub fn turn(&self) -> Option<&tilo_action_policy::turn::Turn> {
        self.turn.as_ref()
    }

    /// The person this actor is, when they act from their own device; `None` for an agent, a
    /// capped device, or an invocation that continues another one.
    #[must_use]
    pub fn direct_person(&self) -> Option<&str> {
        (self.direct && !self.agent).then_some(self.id.as_str())
    }
}

/// One step the broker admitted for the host to run: action `address` for `actor` in `workspace`,
/// with `argument` in its slot form.
pub struct HostStepRequest<'a> {
    pub workspace: &'a str,
    pub actor: &'a StepActor,
    pub address: &'a ActionAddress,
    pub argument: Option<&'a str>,
    /// The task run the step is taken in, when it is one of a run's steps (a task's program):
    /// what the step does in turn goes through it, so it is admitted by the run too.
    pub task_run: Option<&'a dyn TaskRun>,
}

/// How a host step ended.
pub type HostStepOutcome = Result<ActionOutput, Box<ActionFailure>>;

/// A task's run, as one of its steps names it: the host those steps run against, so every call a
/// run's program makes is a call the run admits.
pub trait TaskRun: Send + Sync {
    /// The door the run's programs call through.
    fn door(&self) -> Arc<dyn ActionHost + Send + Sync>;
}

/// Steps the host itself runs for the person at this device: pairing a nearby device,
/// administering the person's identities and the devices trusted under them, changing the
/// person's settings whether or not the app that shows them is open. The broker answers each
/// service under its [`Self::provider`] and decides each invocation before the host runs it.
pub trait HostStepServices: Send + Sync {
    fn provider(&self) -> &'static str;
    /// The application whose controls reach the steps. It reaches them as the person using it,
    /// as it does its own actions.
    fn app_id(&self) -> &str;
    /// Every step, described.
    fn descriptors(&self) -> Vec<ActionDescriptor>;
    /// Every step as the row it stands in: the resource of the level above it, and whether its
    /// own resource holds rows below it. By default every step is a top-level row.
    fn rows(&self) -> Vec<tilo_actions::door::PlacedAction> {
        self.descriptors()
            .into_iter()
            .map(|action| tilo_actions::door::PlacedAction {
                parent: String::new(),
                has_children: false,
                action,
            })
            .collect()
    }
    /// Whether its steps are a turn's (the session's own actions: the arrangement, its undo, the
    /// windows and the notices). Such a step is offered in an agent's catalogue while it works a
    /// turn and admitted by that turn, exactly as the same agent's app action in it is.
    fn for_turns(&self) -> bool {
        false
    }
    /// Whether its steps are steps a task agent takes in its run (a task's programs). No other
    /// host step is a task agent's.
    fn for_tasks(&self) -> bool {
        false
    }
    /// The step at `address` as one invocation with `argument` (its slot form) is: by default as
    /// described, or, for a setting, as risky as the value it carries makes it.
    fn invocation(
        &self,
        address: &ActionAddress,
        argument: Option<&str>,
    ) -> Option<ActionDescriptor> {
        let _ = argument;
        self.descriptors()
            .into_iter()
            .find(|descriptor| &descriptor.address == address)
    }
    /// Start one admitted step. Its outcome arrives on the returned answer: at once, or once the
    /// step ends (a LAN window, an administrator's authentication, a step that reaches another
    /// device), completed from another thread.
    fn start(&self, request: &HostStepRequest<'_>) -> Result<Pending<HostStepOutcome>, String>;
    /// Follow one admitted step whose contract is
    /// [`ExecutionMode::Live`](tilo_actions::ExecutionMode::Live): how to read its result as it
    /// is now, and when that may have changed.
    fn watch(&self, request: &HostStepRequest<'_>) -> Result<LiveView, String> {
        let _ = request;
        Err("this step has no live result".into())
    }
}

/// A live result a host step serves ([`HostStepServices::watch`]).
pub struct LiveView {
    /// The whole result as it is now. A host that reads it on a thread of its own answers when
    /// it has; one that has it at hand answers at once
    /// ([`Pending::ready`](tilo_actions::remote::Pending::ready)).
    pub read: Box<dyn FnMut() -> Pending<LiveRead> + Send>,
    /// Moves whenever the result may have changed.
    pub changes: tokio::sync::watch::Receiver<u64>,
}

/// One read of a [`LiveView`]: the result now, or why it failed for good.
pub type LiveRead = Result<serde_json::Value, Box<ActionFailure>>;

/// What a window's app reports about resuming where it was, in the workspace it runs in. The
/// session that runs the window keeps it; the broker carries it.
pub trait ContinuationServices: Send + Sync {
    /// Reports one application instance's continuation metadata.
    fn report(
        &self,
        workspace: tilo_desktop_model::WorkspaceId,
        app: String,
        instance: u64,
        schema: u16,
        metadata: Vec<u8>,
    ) -> Result<(), String>;
    /// Clears one application instance's continuation metadata.
    fn clear(
        &self,
        workspace: tilo_desktop_model::WorkspaceId,
        app: &str,
        instance: u64,
    ) -> Result<(), String>;
    /// One application instance answering a handoff request, or reporting that the document it
    /// shows arrived retired. `message` is an encoded `collab_types::handoff::HandoffReport`.
    fn handoff(
        &self,
        workspace: tilo_desktop_model::WorkspaceId,
        app: String,
        instance: u64,
        message: Vec<u8>,
    ) -> Result<(), String>;
}
