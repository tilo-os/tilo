//! The workspace service's half of delivering an invocation to its app: the admission
//! [`deliver_or_launch`] passes it through, as the invocation's caller.

use tilo_broker_core::delivery::{DeliveryAsk, DeliveryFuture, DeliveryHost, deliver_or_launch};

use super::super::published_invocation::InvocationCaller;
use super::{
    ActionAddress, ActionCallError, AppBrokerState, Arc, Caller, Completer, InvokeRequest,
    LaunchDestination,
};

/// The workspace service's admission of one invocation while it waits for its app to take it:
/// every check [`deliver_or_launch`] passes it through is made here,
/// as the invocation's caller.
pub(in super::super) struct Delivering<'a> {
    state: &'a Arc<AppBrokerState>,
    request: &'a InvokeRequest,
}

/// Whom an invocation runs for, as it waits for its app: opaque outside the workspace
/// service, which alone reads it, so a host passing it along gains no authority by it.
pub struct LaunchOrigin<'a>(&'a InvocationCaller);

type Ask<'a, 'o> = DeliveryAsk<'a, LaunchOrigin<'o>>;

impl<'a> Delivering<'a> {
    /// The admission of `request` in `state`'s workspaces.
    pub(in super::super) const fn new(
        state: &'a Arc<AppBrokerState>,
        request: &'a InvokeRequest,
    ) -> Self {
        Self { state, request }
    }
}

impl<'o> DeliveryHost<LaunchOrigin<'o>> for Delivering<'_> {
    fn check_origin(&self, ask: &Ask<'_, 'o>) -> Result<(), ActionCallError> {
        (self.state).check_launch_origin(ask.caller, origin_of(ask), ask.workspace)
    }

    fn shows(&self, ask: &Ask<'_, 'o>) -> bool {
        (self.state).action_shows(ask.app_id, ask.workspace, ask.address)
    }

    fn deliver<'b>(
        &'b self,
        ask: &'b Ask<'_, 'o>,
        peer_id: u64,
        address: ActionAddress,
    ) -> DeliveryFuture<'b, Result<u64, ActionCallError>> {
        let (state, caller, origin, request) =
            (self.state, ask.caller, origin_of(ask), self.request);
        let Some((workspace, app_id)) = super::super::windowless_instances::started_app(request)
        else {
            let mut delivered = request.clone();
            delivered.target.provider = peer_id.to_string();
            delivered.target.address = address;
            return Box::pin(
                async move { state.start_job(caller, origin, &delivered, request).await },
            );
        };
        // Answering a workspace's `start` of its app with the app that runs now blocks on
        // the journal, so it answers on the delivery runtime's blocking pool.
        let state = Arc::clone(state);
        let (caller, origin, request) = (caller.clone(), origin.cloned(), request.clone());
        let reason = format!("{app_id} start worker stopped");
        let started = (app_id.to_owned(), workspace.to_owned());
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let started = (started.0.as_str(), started.1.as_str());
                state.answer_start(&caller, origin.as_ref(), &request, started, peer_id)
            })
            .await
            .map_err(|error| ActionCallError::Unavailable {
                reason: format!("{reason}: {error}"),
            })?
        })
    }

    fn launch(
        &self,
        ask: &Ask<'_, 'o>,
        permit: tokio::sync::OwnedSemaphorePermit,
    ) -> Result<tokio::task::JoinHandle<Result<(), ActionCallError>>, ActionCallError> {
        // Launch policy read again after the launch queue's wait (ownership was just rechecked),
        // and the launch it admits now, which must be the one asked for.
        let current = self.state.admit_launchable(ask.caller, self.request)?;
        if current.workspace.id().to_string() != ask.workspace
            || (current.app_id.as_str(), current.record.as_deref()) != (ask.app_id, ask.opened_on)
        {
            return Err(ActionCallError::NotPermitted);
        }
        let place = (self.state).launch_place(ask.caller, ask.workspace, None);
        let state = Arc::clone(self.state);
        let (caller, origin, parent) = (
            ask.caller.clone(),
            origin_of(ask).cloned(),
            self.request.clone(),
        );
        Ok(tokio::task::spawn_blocking(move || {
            let _permit = permit;
            state.launch_retained_child(&caller, origin.as_ref(), &parent, &current, &place)
        }))
    }
}

impl AppBrokerState {
    /// Launch `app_id` unless it already publishes the action, then deliver
    /// the invocation to the connection that does, through the same
    /// admission a live action takes
    /// ([`deliver_or_launch`]).
    pub(super) async fn relaunch_and_deliver<T: Send>(
        self: &Arc<Self>,
        completer: &Completer<T>,
        caller: &Caller,
        destination: &LaunchDestination,
        origin: Option<&InvocationCaller>,
        request: &InvokeRequest,
    ) -> Result<u64, ActionCallError> {
        let workspace = destination.workspace.id().to_string();
        let address = &request.target.address;
        let origin = origin.map(LaunchOrigin);
        let ask = DeliveryAsk {
            caller,
            origin: origin.as_ref(),
            app_id: &destination.app_id,
            workspace: &workspace,
            opened_on: destination.record.as_deref(),
            address,
            start: super::super::windowless_instances::started_app(request).is_some(),
            timeout: self.launchable.delivery_timeout(),
            launches: super::super::launchable::launch_permits(),
        };
        let host = Delivering::new(self, request);
        let launcher = self.services.launcher.as_deref();
        #[cfg(test)]
        let starts = self.launchable.windowless();
        #[cfg(test)]
        let launcher = starts.as_ref().map_or(launcher, |starts| Some(starts as _));
        deliver_or_launch(&self.publishers, launcher, &host, &ask, completer).await
    }
}

fn origin_of<'o>(ask: &Ask<'_, 'o>) -> Option<&'o InvocationCaller> {
    ask.origin.map(|origin| origin.0)
}
