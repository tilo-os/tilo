//! The shared loop behind the broker's own room services: one published
//! surface, answered invocation by invocation from the durable stream.
use std::future::Future;

use super::{Arc, CapnpBrokerState, InvocationEnvelope, PublishedSurface};

/// How one invocation of a broker room service ended.
pub(super) enum ServiceOutcome {
    Succeeded(serde_json::Value),
    Failed(tilo_actions::ActionFailure),
    /// A cancellable retained observation ended without an effect.
    Cancelled,
    /// Completion is retained by a detached owner operation.
    Pending,
}

/// A refusal carrying only a code and the message shown to the caller.
pub(super) fn refused(code: &str, message: String) -> ServiceOutcome {
    ServiceOutcome::Failed(tilo_actions::ActionFailure {
        code: code.into(),
        message,
        details: None,
        retryable: false,
    })
}

impl CapnpBrokerState {
    /// Publish `surface` for its service peer and answer every invocation
    /// addressed to it with `serve`. An update that cannot be recorded stops
    /// the service; it then unpublishes the surface
    /// and runs `stopped` with whether the surface was still published.
    pub(super) fn serve_room<S, F>(
        self: &Arc<Self>,
        surface: PublishedSurface,
        serve: S,
        stopped: impl FnOnce(&Arc<Self>, bool) + 'static,
    ) -> Result<(), String>
    where
        S: Fn(Arc<Self>, InvocationEnvelope) -> F + 'static,
        F: Future<Output = ServiceOutcome>,
    {
        let peer_id = surface.peer_id;
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        self.invocation_subscribers
            .lock()
            .map_err(|_| "invocation subscribers unavailable")?
            .insert(peer_id, sender);
        self.stable_invocation_subscribers
            .lock()
            .map_err(|_| "stable invocation registry unavailable")?
            .insert(peer_id);
        self.surfaces
            .lock()
            .map_err(|_| "action surfaces unavailable")?
            .insert(peer_id, surface);
        let state = self.clone();
        tokio::task::spawn_local(async move {
            while let Some(envelope) = receiver.recv().await {
                let (id, invocation) = (envelope.id, envelope.invocation);
                let update = match serve(state.clone(), envelope).await {
                    ServiceOutcome::Succeeded(value) => tilo_actions::InvocationUpdate::Succeeded {
                        invocation,
                        output: tilo_actions::ActionOutput {
                            value: Some(value),
                            resources: Vec::new(),
                        },
                    },
                    ServiceOutcome::Failed(error) => {
                        tilo_actions::InvocationUpdate::Failed { invocation, error }
                    }
                    ServiceOutcome::Cancelled => tilo_actions::InvocationUpdate::Cancelled {
                        invocation,
                        message: None,
                    },
                    ServiceOutcome::Pending => continue,
                };
                state.refresh_task_control();
                if let Err(error) = state
                    .apply_invocation_update(peer_id, id, update, None)
                    .await
                {
                    state.record_owner_commit_failure(error.to_string());
                    break;
                }
            }
            let removed = state
                .surfaces
                .lock()
                .is_ok_and(|mut surfaces| surfaces.remove(&peer_id).is_some());
            stopped(&state, removed);
        });
        Ok(())
    }

    pub(super) async fn finish_room_service_invocation(
        self: &Arc<Self>,
        peer: u64,
        id: u64,
        invocation: tilo_actions::InvocationId,
        outcome: ServiceOutcome,
    ) -> Result<(), String> {
        let update = match outcome {
            ServiceOutcome::Succeeded(value) => tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput {
                    value: Some(value),
                    resources: Vec::new(),
                },
            },
            ServiceOutcome::Failed(error) => {
                tilo_actions::InvocationUpdate::Failed { invocation, error }
            }
            ServiceOutcome::Cancelled => tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: None,
            },
            ServiceOutcome::Pending => return Ok(()),
        };
        self.refresh_task_control();
        self.apply_invocation_update(peer, id, update, None)
            .await
            .map_err(|error| {
                self.record_owner_commit_failure(error.to_string());
                error.to_string()
            })
    }

    fn record_owner_commit_failure(&self, reason: String) {
        if let Ok(mut failure) = self.owner_commit_failure.lock() {
            failure.get_or_insert(reason);
        }
    }
}
