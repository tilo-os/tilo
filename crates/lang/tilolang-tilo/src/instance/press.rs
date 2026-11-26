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
        _ => crate::naming::package_resource_id(app),
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

/// The call a press of `aref` is on the dispatch, when it is `app`'s own action on one of its own
/// roots (`root shell: Shell`) that changes nothing but the app's records: the action at the
/// address the root is published under, with every argument after the root by its parameter's
/// name, as an agent or another device calls it. `None` for any other action: one that reaches
/// another app's actions or the session, one that reads this device (`uses files`), a scenario,
/// or one on a resource the session keeps (`origin: session.layout`) or on a record that is not a
/// root. A host that keeps the app's records in the person's workspace runs these there, so the
/// press is the person's change of their own records wherever they look at them.
pub(super) fn own_record_call(
    host: &Host,
    app: &str,
    aref: &ActionRef,
) -> Option<(ActionAddress, serde_json::Map<String, serde_json::Value>)> {
    use crate::runtime::meta::{Ty, parse_ty};
    let package = host.package(app)?;
    let meta = &package.meta;
    let declared = meta
        .actions
        .iter()
        .find(|action| action.id == aref.action)?;
    if declared.scenario
        || !declared.uses.is_empty()
        || declared.reaches.iter().any(|reach| !reach.own)
    {
        return None;
    }
    let Ok(Ty::Record(key)) = parse_ty(&declared.params.first()?.ty, &meta.app) else {
        return None;
    };
    if key.app != meta.app {
        return None;
    }
    let ty = meta.type_named(&key.name)?;
    if ty.origin.is_some() {
        return None;
    }
    let root = meta
        .named_roots
        .iter()
        .find(|root| root.ty == ty.id && !root.device)?;
    let resource = root
        .published
        .clone()
        .unwrap_or_else(|| crate::naming::resource_id(app, &format!("root-{}", root.ty)));
    let owner = |other: &str| {
        host.package(other)
            .map(|package| package.meta)
            .ok_or_else(|| format!("no package of {other} is loaded"))
    };
    let mut arguments = serde_json::Map::new();
    for (param, value) in declared.params.iter().zip(&aref.args).skip(1) {
        let ty = parse_ty(&param.ty, &meta.app).ok()?;
        let value = crate::host::argument_json(&owner, meta, value, &ty).ok()?;
        arguments.insert(param.name.clone(), value);
    }
    Some((
        ActionAddress::new(resource, declared.name.clone()),
        arguments,
    ))
}
