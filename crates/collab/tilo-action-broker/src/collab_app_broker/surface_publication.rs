//! How a published action surface moves the broker's surfaces generation:
//! only when it changed. Every listing of actions and every open palette
//! reads that generation, so a move with nothing changed is churn, and
//! each move is traced with its cause.

#[cfg(any(test, feature = "test-support"))]
use super::WitnessPublicationKind;
use super::{AppBrokerState, PublishedSurface};
use std::sync::{OnceLock, Weak, mpsc};

/// Builds the catalogue subscribers follow, every published surface listed
/// for each audience, on a thread of its own: built on the connection
/// thread, each change of any app's surface held every app's document
/// changes behind it for tens of milliseconds. Changes that come while one
/// is built are taken by the next.
pub(super) struct CatalogueRefresh {
    state: Weak<AppBrokerState>,
    due: OnceLock<mpsc::Sender<()>>,
}

impl CatalogueRefresh {
    pub(super) const fn new(state: Weak<AppBrokerState>) -> Self {
        Self {
            state,
            due: OnceLock::new(),
        }
    }
}

impl AppBrokerState {
    /// What the broker offers changed: its catalogue subscribers are handed
    /// it again, built by [`CatalogueRefresh`]. The thread ends with the broker.
    pub(super) fn catalogue_changed(&self) {
        let refresh = &self.catalogue_refresh;
        let due = refresh.due.get_or_init(|| {
            let (due, changes) = mpsc::channel::<()>();
            let state = refresh.state.clone();
            let spawned = std::thread::Builder::new()
                .name("tilo-catalogue".into())
                .spawn(move || {
                    while changes.recv().is_ok() {
                        while changes.try_recv().is_ok() {}
                        let Some(state) = state.upgrade() else { return };
                        state.publish_action_catalogue();
                    }
                });
            if let Err(error) = spawned {
                tracing::error!(%error, "the action catalogue cannot be built");
            }
            due
        });
        let _ = due.send(());
    }

    /// Record that a published surface changed: the generation moves and
    /// every launch waiting for its app to publish wakes. Each move is
    /// traced with where it came from: every listing and open palette reads
    /// this generation, so a move with nothing changed is churn.
    #[track_caller]
    pub(super) fn surfaces_changed(&self) {
        self.surface_changes
            .send_modify(|generation| *generation = generation.wrapping_add(1));
        tracing::debug!(
            target: "tilo::publications",
            generation = *self.surface_changes.borrow(),
            cause = %std::panic::Location::caller(),
            "published surfaces changed"
        );
    }

    /// An app published `surface` over `previous`. The surfaces' generation
    /// moves, and catalogue subscribers hear of it, only when it differs: an
    /// app publishing what it already had changes no listing, and moving
    /// the generation for it made every listing and open palette read the
    /// whole surface again, and retired the app directory pages kept for it.
    pub(super) fn surface_republished(
        &self,
        peer: u64,
        surface: &PublishedSurface,
        previous: Option<&PublishedSurface>,
    ) {
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
        self.catalogue_changed();
    }

    /// [`crate::collab_broker::AppActionBroker::publish_surface_for_test`].
    #[cfg(any(test, feature = "test-support"))]
    pub fn publish_surface_for_test(&self, surface: PublishedSurface) {
        let peer = surface.peer_id;
        let Ok(mut published) = self.surfaces.lock() else {
            return;
        };
        let republished = surface.clone();
        let previous = published.insert(peer, surface);
        drop(published);
        self.advance_witness_publication(peer, WitnessPublicationKind::Surface);
        self.surface_republished(peer, &republished, previous.as_ref());
    }
}
