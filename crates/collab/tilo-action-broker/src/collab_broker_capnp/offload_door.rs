//! Offloadable invocations at the action door.
//!
//! An invocation of an action whose contract declares an offload is
//! admitted exactly like any other: who may invoke, confirmation, journal
//! and receipt are decided before this module sees it, the same for a
//! person, a script and an agent. Then, instead of handing it to the app at
//! once, the door asks the device's route
//! ([`HostServices::action_offload`](crate::services::HostServices)) where it
//! should run. When another device ran it, the door answers the invocation
//! with that output, as the app would have. Otherwise — for whatever reason
//! the route gives — the app gets the invocation as usual and its own
//! handler answers it, and the route hears how long that took. A worker
//! that fails or goes away therefore costs time, never a different answer.

use std::sync::Arc;
use std::time::Instant;

use tilo_actions::offload::{ActionOffload, OffloadRequest, Offloaded};
use tokio::sync::mpsc;

use super::{CapnpBrokerState, InvocationEnvelope, invocation_journal};

/// An admitted offloadable invocation, waiting for the broker turn.
pub(super) struct QueuedOffload {
    pub(super) id: u64,
    pub(super) peer_id: u64,
    pub(super) request: OffloadRequest,
    pub(super) route: Arc<dyn ActionOffload>,
    /// What the app is handed when it runs here.
    pub(super) envelope: InvocationEnvelope,
    pub(super) app: mpsc::UnboundedSender<InvocationEnvelope>,
}

/// Local runs awaited at once before the oldest are forgotten: an
/// invocation that expires never reports its run.
const MAX_OFFLOADED_HERE: usize = 256;

impl CapnpBrokerState {
    /// The route this broker's door asks where an admitted offloadable
    /// invocation runs, as its host installed it; `None` runs every one
    /// through its app's own handler.
    #[must_use]
    pub fn action_offload(&self) -> Option<&Arc<dyn ActionOffload>> {
        self.services.action_offload.as_ref()
    }

    /// The offload request for an admitted invocation of `action` in
    /// `room` with wire `argument`, when it declares an offload and this
    /// device routes offloads; `None` runs it here as any other.
    pub(super) fn offload_request(
        &self,
        (room, app_id): (&str, &str),
        action: &runtime_collab::ExportedAction,
        argument: Option<&str>,
    ) -> Option<(Arc<dyn ActionOffload>, OffloadRequest)> {
        let offload = action.contract.offload.clone()?;
        let route = self.services.action_offload.clone()?;
        let inputs = match tilo_actions::offload::argument_inputs(&action.params, argument) {
            Ok(inputs) => inputs,
            Err(reason) => {
                // The app's handler refuses such arguments itself.
                tracing::debug!(%reason, "offloadable invocation runs here");
                return None;
            }
        };
        Some((
            route,
            OffloadRequest {
                room: room.to_owned(),
                app_id: app_id.to_owned(),
                address: tilo_actions::ActionAddress::new(&action.resource, &action.action),
                offload,
                inputs,
            },
        ))
    }

    /// Ask the route of every queued offloadable invocation where it runs.
    /// Called on the broker `LocalSet`.
    pub(crate) fn drain_offloads(self: &Arc<Self>) {
        let queued = self
            .offloads
            .lock()
            .map(|mut queued| std::mem::take(&mut *queued))
            .unwrap_or_default();
        for offload in queued {
            let state = Arc::clone(self);
            tokio::task::spawn_local(async move { state.run_offload(offload).await });
        }
    }

    async fn run_offload(self: Arc<Self>, queued: QueuedOffload) {
        let QueuedOffload {
            id,
            peer_id,
            request,
            route,
            envelope,
            app,
        } = queued;
        match route.offload(request.clone()).await {
            Offloaded::Ran {
                output,
                device,
                elapsed,
            } => {
                tracing::info!(
                    id,
                    %device,
                    ?elapsed,
                    action = %request.address.action,
                    "offloadable invocation ran on another device"
                );
                let update = tilo_actions::InvocationUpdate::Succeeded {
                    invocation: envelope.invocation,
                    output,
                };
                // An invocation cancelled or expired meanwhile is no longer
                // pending, and keeps the answer it already has.
                if let Err(error) = self
                    .apply_invocation_update(peer_id, id, update, None)
                    .await
                {
                    tracing::info!(id, %error, "an offloaded answer arrived too late");
                }
            }
            Offloaded::RunHere { reason } => {
                tracing::debug!(id, %reason, "offloadable invocation runs here");
                self.hand_to_app(id, (request, envelope), &app);
            }
        }
    }

    /// Hand the still-pending invocation `id` to its app, remembering when.
    fn hand_to_app(
        &self,
        id: u64,
        (request, envelope): (OffloadRequest, InvocationEnvelope),
        app: &mpsc::UnboundedSender<InvocationEnvelope>,
    ) {
        let Ok(mut registry) = self.pending_invocations.lock() else {
            return;
        };
        if !registry.contains_key(&id) {
            return;
        }
        if let Ok(mut here) = self.offloaded_here.lock() {
            if here.len() >= MAX_OFFLOADED_HERE {
                here.clear();
            }
            here.insert(id, (request, Instant::now()));
        }
        let invocation = envelope.invocation;
        if app.send(envelope).is_err() {
            let removed = registry.remove(&id);
            drop(registry);
            self.invocation_changed();
            if let Some(pending) = removed {
                let _ = self.persist_invocation(
                    &pending,
                    invocation_journal::unknown(invocation),
                    true,
                );
            }
            if let Ok(mut here) = self.offloaded_here.lock() {
                here.remove(&id);
            }
            return;
        }
        drop(registry);
        self.local_turn.notify_one();
    }

    /// The app answered invocation `id` with `update`: when the route sent
    /// it back here and it succeeded, tell the route how long it took.
    pub(super) fn offload_answered(&self, id: u64, update: &tilo_actions::InvocationUpdate) {
        let Some((request, handed)) = self
            .offloaded_here
            .lock()
            .ok()
            .and_then(|mut here| here.remove(&id))
        else {
            return;
        };
        if matches!(update, tilo_actions::InvocationUpdate::Succeeded { .. })
            && let Some(route) = &self.services.action_offload
        {
            route.ran_here(&request, handed.elapsed());
        }
    }
}
