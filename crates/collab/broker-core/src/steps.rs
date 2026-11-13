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

/// One step the broker admitted for the host to run: action `address` for `actor` in `workspace`,
/// with `argument` in its slot form.
pub struct HostStepRequest<'a> {
    pub workspace: &'a str,
    pub actor: &'a crate::actor::BrokerActor,
    pub address: &'a ActionAddress,
    pub argument: Option<&'a str>,
    /// The task run the step is taken in, when it is one of a run's steps (a task's program):
    /// what the step does in turn goes through it, so it is admitted by the run too.
    pub task_run: Option<&'a dyn TaskRun>,
    /// The run the invocation names itself a step of (its context's group, as an agent's
    /// connection stamps every call): what a step that changes a record makes is that run's,
    /// which History groups and an undo of the run takes back, as an app's action is.
    pub run: Option<&'a tilo_actions::WorkGroup>,
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
