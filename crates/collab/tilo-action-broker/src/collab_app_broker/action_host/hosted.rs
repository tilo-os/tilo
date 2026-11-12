//! An action of a workspace another device hosts, run on that host.
//!
//! Where an action runs and where its view shows are separate. A record of a
//! workspace placed on a host is the host's to change: the host decides the
//! person's invocation with the workspace's policy and runs it in its own
//! worker, and the change reaches this device's window by sync like any
//! other. So the broker sends such an invocation to the host instead of
//! deciding it and delivering it to the app here, and answers the caller
//! with the host's receipt once the invocation settled there. The view stays
//! here: the app's window shows the record, and the broker notes it as where
//! the person works now.
//!
//! What only this device can do stays here: an action on the invoking
//! device or one its person attends, a view, an agent's or an app's call,
//! and every action of a workspace this device hosts itself.

use std::sync::Arc;

use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, Completer, InvocationReceipt, InvokeRequest,
    MAX_REMOTE_WAIT, Pending, RECORD_PROVIDER, ReceiptStatus, pending, settle_with,
};

use super::super::published_invocation::InvocationCaller;
use super::super::{
    ActionDirectoryBridgeReply, ActionDirectoryBridgeResponse, ActionDirectoryRoot, AppBrokerState,
    PublishedAction, published_directory_action,
};
use super::own_device::is_person_here;
use crate::collab_broker::CrdtRoute;

/// The host an invocation runs on, and the request it is sent as there.
pub(super) struct Forward {
    host: Arc<dyn ActionHost + Send + Sync>,
    request: InvokeRequest,
    workspace: String,
    app_id: String,
}

impl AppBrokerState {
    /// Where `caller`'s `request` for `action` of the record at `route` runs
    /// when the record's workspace is hosted on another device: that host,
    /// with the request naming the record as the host names it. `None` for
    /// everything decided and run here.
    pub(super) fn hosted_forward(
        &self,
        caller: &Caller,
        origin: Option<&InvocationCaller>,
        route: &CrdtRoute,
        action: &PublishedAction,
        request: &InvokeRequest,
    ) -> Option<Forward> {
        if route.record_id.is_empty()
            || action.operation_kind == tilo_actions::OperationKind::View
            || action.contract.runs_on_invokers_device()
            || !is_person_here(caller, origin)
        {
            return None;
        }
        let host = self
            .services
            .hosted_workspaces
            .as_ref()?
            .host_actions(&route.cluster_id)?;
        let mut request = request.clone();
        request.target.provider = format!("{RECORD_PROVIDER}:{}:{}", route.app_id, route.record_id);
        request.target.address = tilo_actions::ActionAddress::new(&action.resource, &action.action);
        // A revision names the action as this device's app published it; the
        // host decides on its own copy of the record.
        request.revision = None;
        Some(Forward {
            host,
            request,
            workspace: route.cluster_id.clone(),
            app_id: route.app_id.clone(),
        })
    }

    /// [`Self::hosted_forward`] for `request` naming an action a live app
    /// publishes by its connection.
    pub(super) fn eager_forward(
        &self,
        caller: &Caller,
        origin: Option<&InvocationCaller>,
        request: &InvokeRequest,
    ) -> Option<Forward> {
        let peer_id = request.target.provider.parse::<u64>().ok()?;
        let address = &request.target.address;
        let (route, action) = self.surfaces.lock().ok().and_then(|surfaces| {
            let surface = surfaces.get(&peer_id).filter(|surface| surface.live)?;
            let action = surface.actions.iter().find(|action| {
                action.resource == address.resource && action.action == address.action
            })?;
            Some((surface.route.clone(), action.clone()))
        })?;
        self.hosted_forward(caller, origin, &route, &action, request)
    }

    /// [`Self::hosted_forward`] for `request` naming an action of the
    /// directory `root`, as its app resolved it (`reply`).
    pub(super) fn directory_forward(
        &self,
        caller: &Caller,
        root: &ActionDirectoryRoot,
        request: &InvokeRequest,
        origin: Option<&InvocationCaller>,
        reply: Option<&ActionDirectoryBridgeReply>,
    ) -> Option<Forward> {
        let Some(Ok(ActionDirectoryBridgeResponse::Resolution(
            runtime_collab::ActionDirectoryResolution::Found { row, .. },
        ))) = reply
        else {
            return None;
        };
        let action = published_directory_action(row).ok()?;
        let route = CrdtRoute::record(&root.app_id, &root.cluster_id, &root.record_id);
        self.hosted_forward(caller, origin, &route, &action, request)
    }

    /// Send `forward` to its host for `caller`, answered with the host's
    /// receipt once the invocation settled there. The app's window is where
    /// the person works now, as it is for an invocation delivered to it.
    pub(super) fn send_to_host(
        &self,
        caller: &Caller,
        forward: Forward,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        let Forward {
            host,
            request,
            workspace,
            app_id,
        } = forward;
        let actor = self.caller_actor(caller, &workspace);
        self.launchable.note_view(&workspace, actor.id(), &app_id);
        let (done, answer) = pending();
        let caller = caller.clone();
        host.invoke(&caller, request)
            .then(move |receipt| match receipt {
                Some(Ok(receipt)) => follow(host, caller, receipt, done),
                Some(Err(error)) => done.complete(Err(error)),
                None => done.complete(Err(ActionHostError::Unavailable {
                    reason: "the workspace's host did not answer".into(),
                })),
            });
        answer
    }
}

/// Complete `done` with `receipt` once the host says the invocation settled:
/// an accepted one is followed there, a while at a time, for as long as it
/// runs. A host that stops telling leaves the admission as it stood.
fn follow(
    host: Arc<dyn ActionHost + Send + Sync>,
    caller: Caller,
    receipt: InvocationReceipt,
    done: Completer<Result<InvocationReceipt, ActionHostError>>,
) {
    if !matches!(receipt.status, ReceiptStatus::Accepted) || done.is_abandoned() {
        done.complete(Ok(receipt));
        return;
    }
    let now = tilo_clock::now();
    let until = now.checked_add(MAX_REMOTE_WAIT).unwrap_or(now);
    settle_with(&host, &caller, receipt.invocation, None, until).then(move |state| {
        match state.flatten() {
            // Still running at the end of a wait: follow it again. One that
            // answered sooner with nothing new is a host that cannot tell.
            Some(state) if !state.is_terminal() && tilo_clock::now() >= until => {
                follow(host, caller, state.receipt(), done);
            }
            Some(state) => done.complete(Ok(state.receipt())),
            None => done.complete(Ok(receipt)),
        }
    });
}
