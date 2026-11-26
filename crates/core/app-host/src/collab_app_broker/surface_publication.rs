//! How a published action surface moves the broker's surfaces generation:
//! only when it changed. Every listing of actions and every open palette
//! reads that generation, so a move with nothing changed is churn, and
//! each move is traced with its cause.

use super::{AppBrokerState, PublishedSurface};
use std::sync::Arc;

impl AppBrokerState {
    /// Record that a published surface changed: the generation moves and
    /// every launch waiting for its app to publish wakes. Each move is
    /// traced with where it came from: every listing and open palette reads
    /// this generation, so a move with nothing changed is churn.
    #[track_caller]
    pub(super) fn surfaces_changed(&self) {
        self.publishers
            .changes
            .send_modify(|generation| *generation = generation.wrapping_add(1));
        tracing::debug!(
            target: "tilo::publications",
            generation = *self.publishers.changes.borrow(),
            cause = %std::panic::Location::caller(),
            "published surfaces changed"
        );
    }

    /// An app published `surface` over `previous`. The surfaces' generation
    /// moves only when it differs: an
    /// app publishing what it already had changes no listing, and moving
    /// the generation for it made every listing and open palette read the
    /// whole surface again, and retired the app directory pages kept for it.
    pub(super) fn surface_republished(
        &self,
        peer: u64,
        surface: &PublishedSurface,
        previous: Option<&PublishedSurface>,
    ) {
        self.register_app_calls(surface);
        if previous == Some(surface) {
            tracing::debug!(
                target: "tilo::publications",
                peer,
                app = %surface.app_id,
                "an unchanged action surface republished; nothing moves"
            );
            return;
        }
        tracing::debug!(
            target: "tilo::publications",
            peer,
            app = %surface.app_id,
            actions = surface.actions.len(),
            "action surface changed"
        );
        self.surfaces_changed();
    }

    /// The app's calls reach this broker through the device's one registry, under its workspace's
    /// application token: an action of the app's on one of its records, named anywhere, reaches
    /// the process holding that record, and starts one with no window when none does. The
    /// registration belongs to the broker rather than to the connection that published it, so it
    /// outlives every process it starts; a stopped broker answers that it is gone.
    fn register_app_calls(&self, surface: &PublishedSurface) {
        self.answer_app_calls(&surface.route.cluster_id, &surface.app_id);
    }

    /// Answer `app`'s calls in `workspace` here, from this device and every other one of the
    /// workspace ([`Self::register_app_calls`]), unless something answers them already.
    pub(crate) fn answer_app_calls(&self, workspace: &str, app: &str) {
        let Some(providers) = self.services.providers.as_ref() else {
            return;
        };
        providers.keep_invocations(self.itself.clone());
        let provider =
            tilo_workspace_service::workspaces::providers::Providers::app_provider(workspace, app);
        if providers.get(&provider).is_some() {
            return;
        }
        providers.provide_app(
            workspace,
            app,
            tilo_workspace_service::workspaces::providers::Reach::Devices,
            Arc::new(super::action_provider::AppCalls::new(self.itself.clone())),
        );
    }

    /// [`crate::collab_broker::AppActionBroker::publish_surface_for_test`].
    #[cfg(any(test, feature = "test-support"))]
    pub fn publish_surface_for_test(&self, surface: PublishedSurface) {
        let peer = surface.peer_id;
        let Ok(mut published) = self.publishers.surfaces.lock() else {
            return;
        };
        let republished = surface.clone();
        let previous = published.insert(peer, surface);
        drop(published);
        self.surface_republished(peer, &republished, previous.as_ref());
    }
}
