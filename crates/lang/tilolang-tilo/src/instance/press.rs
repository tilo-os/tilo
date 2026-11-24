//! A person's press of an app action is a call of its own: the host that runs the instance names
//! it ([`PressCalls::begin`]) before the action runs, so what the action writes and the calls it
//! makes are that call's, and hears how it went ([`Pressed`]) once it ran.

use tilo_actions::{ActionAddress, InvocationId};

use crate::host::Host;
use crate::runtime::{ActionRef, InvokeOutcome, RuntimeError, Value};

/// What makes each press of the person's a call: the host running the instance, which records it
/// with the dispatch and runs it here.
pub trait PressCalls {
    /// The person pressed for the app's action published at `address`: the call it is, whose
    /// writes and child calls are its own until [`Self::end`]. `None` when it runs as no call (a
    /// window with no dispatch to record it with).
    fn begin(&self, address: &ActionAddress) -> Option<InvocationId>;
    /// The press [`Self::begin`] named ran as far as it runs now.
    fn end(&self);
}

/// A press that ran as the call `invocation`: the action at `address`, and how it came out.
#[derive(Debug)]
pub struct Pressed {
    pub invocation: InvocationId,
    pub address: ActionAddress,
    pub outcome: PressOutcome,
}

/// How a pressed action came out.
#[derive(Debug)]
pub enum PressOutcome {
    /// It ran, and answered this.
    Done(Value),
    /// It waits on the call `call` it made, as the run `run`.
    Waits {
        run: crate::runtime::ids::RunId,
        call: String,
    },
    /// It waits for the person's yes to the call it made of itself, which runs when they answer.
    Asked,
    /// It failed, for this reason.
    Failed(String),
}

impl PressOutcome {
    /// What `outcome`, a pressed action's run, came to.
    pub(super) fn of(outcome: &Result<InvokeOutcome, RuntimeError>) -> Self {
        match outcome {
            Ok(InvokeOutcome::Done(value)) => Self::Done(value.clone()),
            Ok(InvokeOutcome::Waits { run, call }) => Self::Waits {
                run: run.clone(),
                call: call.clone(),
            },
            Ok(InvokeOutcome::Asked(_)) => Self::Asked,
            Ok(InvokeOutcome::Denied) => Self::Failed("the person did not allow it".to_owned()),
            Err(error) => Self::Failed(error.to_string()),
        }
    }
}

/// Where `app` publishes the action `aref` runs: its name, on the resource it is run on (its first
/// argument), or on the app's package when it takes none. `None` for an action no package of
/// `app`'s declares (the platform's own, or another app's): no press of the app's.
pub(super) fn address(host: &Host, app: &str, aref: &ActionRef) -> Option<ActionAddress> {
    let name = host.find_in_packages(|package| {
        if package.meta.app != app {
            return None;
        }
        package
            .meta
            .actions
            .iter()
            .find(|action| action.id == aref.action)
            .map(|action| action.name.clone())
    })?;
    let resource = match aref.args.first() {
        Some(Value::Ref(on)) => crate::call::address_of(app, &on.target),
        _ => crate::publish::package_resource_id(app),
    };
    Some(ActionAddress::new(resource, name))
}

/// Begin the press of `aref` through `calls`, when it is a press of `app`'s action: the call it
/// runs as, and where it is published.
pub(super) fn begin(
    calls: Option<&dyn PressCalls>,
    host: &Host,
    app: &str,
    aref: &ActionRef,
) -> Option<(InvocationId, ActionAddress)> {
    let calls = calls?;
    let address = address(host, app, aref)?;
    calls
        .begin(&address)
        .map(|invocation| (invocation, address))
}
