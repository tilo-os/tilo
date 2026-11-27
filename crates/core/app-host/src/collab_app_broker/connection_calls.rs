//! The calls this device sent on to the device that runs their app's connection
//! ([`AppBrokerState::invoke_application`]), followed there for whoever made them.
//!
//! The call runs, and may wait for its person's yes, on that device: there it is followed by
//! whoever asked it, or by this device, which brought it, on whatever link either asks. Here it
//! is followed only by its caller, found by its full key (its app, its workspace and its id),
//! never by the id alone; anyone else reads it as an id nothing here admitted.

use std::sync::Arc;

use tilo_actions::remote::{ActionCallError, ActionProvider, Caller, InvocationState, Pending};
use tilo_lock::MutexRecover as _;

use super::{AppBrokerState, JobKey};

/// The most calls sent on that are kept followable at once; past it the oldest are forgotten
/// together, and their callers read them as ended where they ran.
const FORWARDED_KEPT: usize = 1024;

/// A call sent on to the device that runs its app's connection.
pub(super) struct ForwardedCall {
    /// Who made it, as this device admitted them.
    caller: Caller,
    /// The actions of the device it runs on.
    there: Arc<dyn ActionProvider + Send + Sync>,
}

impl AppBrokerState {
    /// Keep `request`, which `caller` made and this device sent to `there`, followable by them
    /// ([`Self::forwarded_call`]).
    pub(super) fn sent_on(
        &self,
        caller: &Caller,
        request: &tilo_actions::remote::InvokeRequest,
        there: &Arc<dyn ActionProvider + Send + Sync>,
    ) {
        let Some((workspace, app)) =
            tilo_actions::remote::application_provider(&request.target.provider)
        else {
            return;
        };
        let key = JobKey {
            app: app.to_owned(),
            workspace: workspace.to_owned(),
            invocation: request.invocation,
        };
        let mut sent = self.forwarded_calls.lock_recover();
        if sent.len() >= FORWARDED_KEPT && !sent.contains_key(&key) {
            sent.clear();
        }
        sent.insert(
            key,
            ForwardedCall {
                caller: caller.clone(),
                there: Arc::clone(there),
            },
        );
    }

    /// The call this device sent on that `caller` made under `id`, in the workspace `caller`
    /// asks in, as its caller follows it there: the caller and the device it runs on. One call
    /// alone answers to the id; two apps' calls sharing it name neither.
    fn forwarded_call(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Option<(Caller, Arc<dyn ActionProvider + Send + Sync>)> {
        let named: Vec<(Caller, Arc<dyn ActionProvider + Send + Sync>)> = self
            .forwarded_calls
            .lock_recover()
            .iter()
            .filter(|(key, _)| {
                key.invocation == *id
                    && caller
                        .workspace()
                        .is_none_or(|workspace| workspace == key.workspace)
            })
            .map(|(_, call)| (call.caller.clone(), Arc::clone(&call.there)))
            .collect();
        let [(asked, there)] = <[_; 1]>::try_from(named).ok()?;
        caller.follows_by_identity(&asked).then_some((asked, there))
    }

    /// Where a call this device sent on stands, as its caller reads it on the device it runs
    /// on; `None` for one `caller` did not make.
    pub(super) fn observe_sent_on(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Option<InvocationState> {
        let (asked, there) = self.forwarded_call(caller, id)?;
        there.observe(&asked, id)
    }

    /// When a call this device sent on may have moved there; `None` for one `caller` did not
    /// make, and for one whose device cannot tell.
    pub(super) fn next_change_sent_on(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Option<Pending<()>> {
        let (asked, there) = self.forwarded_call(caller, id)?;
        there.next_change(&asked, id)
    }

    /// Stop a call this device sent on, on the device it runs on; `None` for one `caller` did
    /// not make.
    pub(super) fn cancel_sent_on(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Option<Pending<Result<(), ActionCallError>>> {
        let (asked, there) = self.forwarded_call(caller, id)?;
        Some(there.cancel(&asked, id))
    }
}
