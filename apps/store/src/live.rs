//! The Store's live application host.
//!
//! Keeping this wrapper in the library, not the binary shim, means every
//! launch path uses the same worker-backed lifecycle and gives the Tilo app
//! SDK a real `app_root()` contract to inspect and reuse.
//!
//! [`LiveStore`] is a [`Decorator`](ui::widget::Decorator) over [`StoreApp`]:
//! it adds the one thing the app does not own, the serialized worker that
//! fetches catalogues and runs package operations, and forwards everything
//! else (resources, search, undo, layout, painting) to the app itself.

use ui::framework::{ResourceSurface, SurfaceCx};
use ui::widget::{AppModel, ModelPath, Widget};

use crate::FlatpakRef;
use crate::app::{ModerationIntent, RemoteIntent, StoreApp};
use crate::resource::{Store, StoreState};
use crate::worker::{Request, StoreWorker};

/// Construct the standard worker-backed Store root used by normal app hosts.
#[must_use]
pub fn app_root() -> Box<dyn Widget> {
    Box::new(LiveStore::new(None, None))
}

/// Construct the live root with the launch context accepted by Store's
/// `.flatpakref` and `tilo-store:app/` routes.
#[must_use]
pub fn app_root_with_launch(
    opened_ref: Option<FlatpakRef>,
    store_target: Option<String>,
) -> Box<dyn Widget> {
    Box::new(LiveStore::new(opened_ref, store_target))
}

/// The real Store application root. The worker owns blocking catalogue and
/// package work; this retained widget drains only completed snapshots.
pub struct LiveStore {
    inner: StoreApp,
    /// `None` once a capture staged the app: a staged catalogue is fixture
    /// content, and no worker may replace it.
    worker: Option<StoreWorker>,
}

impl LiveStore {
    /// A Store window with its worker already fetching.
    #[must_use]
    pub fn new(opened_ref: Option<FlatpakRef>, store_target: Option<String>) -> Self {
        let search_index = crate::search_provider::StoreSearchIndex::default();
        let mut inner = StoreApp::new()
            .with_image_store(crate::images::image_store())
            .with_search_index(search_index.clone());
        if let Some(id) = store_target {
            inner.open_catalogue_item(id);
        }
        // First presented frame is the catalogue chrome ("Loading apps…"),
        // not a cleared swapchain. The worker is already in flight.
        inner.set_worker_busy(true);
        Self {
            inner,
            worker: Some(StoreWorker::spawn(opened_ref, search_index)),
        }
    }
}

impl ui::widget::Decorator for LiveStore {
    type Inner = StoreApp;

    fn inner(&self) -> &StoreApp {
        &self.inner
    }

    fn inner_mut(&mut self) -> &mut StoreApp {
        &mut self.inner
    }

    #[allow(
        clippy::too_many_lines,
        reason = "this is the single host sync point that drains worker snapshots and forwards every typed Store intent without optimistic state"
    )]
    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let Some(worker) = self.worker.as_ref() else {
            self.inner.sync(state, path);
            return;
        };
        let (state, path) = ui::doc::document(state, path);
        while let Some(snap) = worker.try_snapshot() {
            if let Some(invocation) = snap.invocation {
                self.inner
                    .complete_lifecycle_invocation(invocation, snap.operation_error.clone());
            }
            let applies_items = match snap.query.as_deref() {
                Some(query) => query == self.inner.query(),
                None => snap.detail.is_none(),
            };
            if applies_items {
                self.inner.set_items_from_worker(snap.items);
            }
            if let Some(detail) = snap.detail {
                self.inner.set_detail_from_worker(detail);
            }
            self.inner.set_remotes(snap.remotes);
            self.inner.set_status_line(snap.status_line);
            self.inner.set_operation_error(snap.operation_error);
            if let Some(case) = snap.moderation_case {
                self.inner.set_moderation_case(state, path, &case);
            }
        }
        self.inner.sync(state, path);
        for (id, action, invocation) in self.inner.take_lifecycle_actions() {
            if let Some(item) = self
                .inner
                .items()
                .iter()
                .find(|item| item.id == id)
                .cloned()
            {
                if !worker
                    .request_with_invocation(Request::Act(Box::new(item), action), Some(invocation))
                {
                    self.inner.complete_lifecycle_invocation(
                        invocation,
                        Some("store worker is unavailable".into()),
                    );
                }
            } else {
                self.inner.complete_lifecycle_invocation(
                    invocation,
                    Some("app is no longer present in the catalog".into()),
                );
            }
        }
        if let Some(invocation) = self.inner.take_lifecycle_refresh()
            && !worker.request_with_invocation(Request::Refresh, Some(invocation))
        {
            self.inner.complete_lifecycle_invocation(
                invocation,
                Some("store worker is unavailable".into()),
            );
        }
        if let Some(query) = self.inner.take_search_requested() {
            worker.request(Request::Search(query));
        }
        if let Some(id) = self.inner.take_detail_requested() {
            worker.request(Request::Detail(id));
        }
        for (intent, invocation) in self.inner.take_lifecycle_remote() {
            let request = match intent {
                RemoteIntent::Refresh => Request::RefreshRemotes,
                RemoteIntent::Add { name, url } => Request::RemoteAdd { name, url },
                RemoteIntent::Remove(name) => Request::RemoteRemove(name),
            };
            if !worker.request_with_invocation(request, Some(invocation)) {
                self.inner.complete_lifecycle_invocation(
                    invocation,
                    Some("store worker is unavailable".into()),
                );
            }
        }
        for (intent, invocation) in self.inner.take_moderation_requests() {
            let request = match intent {
                ModerationIntent::Submit {
                    registry_name,
                    envelope,
                } => Request::SubmitReport {
                    registry_name,
                    envelope,
                },
                ModerationIntent::Refresh {
                    registry_name,
                    case_id,
                } => Request::RefreshModeration {
                    registry_name,
                    case_id,
                },
                ModerationIntent::Appeal {
                    registry_name,
                    case_id,
                    reason,
                } => Request::AppealModeration {
                    registry_name,
                    case_id,
                    reason,
                },
            };
            if !worker.request_with_invocation(request, Some(invocation)) {
                self.inner.complete_lifecycle_invocation(
                    invocation,
                    Some("Store worker is unavailable; the report draft was not submitted".into()),
                );
            }
        }
        self.inner.set_worker_busy(worker.has_pending());
    }

    fn needs_animation_frame(&self) -> bool {
        self.worker.as_ref().is_some_and(StoreWorker::has_pending)
            || self.inner.needs_animation_frame()
    }
}

impl ResourceSurface<Store> for LiveStore {
    fn present(&mut self, state: &std::rc::Rc<StoreState>) {
        self.inner.present(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Store>) {
        self.inner.attach(cx);
    }

    fn stage(&mut self, scenario: Option<&str>) {
        // A capture shows the fixture catalogue and the scenario's state, so
        // nothing the worker fetched may replace them.
        self.worker = None;
        self.inner.stage(scenario);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The live root is the real worker-backed host with its launch contract, and reconnects after drop.
    #[test]
    fn live_root_is_the_worker_backed_host_and_reconnects() {
        // Case `app_root_is_the_real_worker_backed_store_host`.
        {
            let root = app_root();
            assert_eq!(root.name(), "StoreApp");
            assert!(
                root.local_resource_operation_handlers()
                    .contains(&"store.refresh")
            );
        }

        // Case `launch_context_keeps_the_real_host_contract`.
        {
            let root = app_root_with_launch(None, Some("so.tilo.notes".into()));
            assert!(root.needs_animation_frame());
        }

        // Case `a_fresh_root_reconnects_after_the_previous_host_is_dropped`.
        {
            // Dropping a Store root drops the worker request sender. The worker
            // exits after its current operation; a reopened app must always own a
            // fresh, independent host rather than retaining the old channel.
            drop(app_root());
            let reopened = app_root();
            assert!(reopened.needs_animation_frame());
        }
    }

    /// A staged capture drops the worker, so fixture content is never replaced.
    #[test]
    fn staging_a_capture_drops_the_worker_and_keeps_the_fixture() {
        let mut live = LiveStore::new(None, None);
        assert!(live.worker.is_some());
        live.stage(Some("detail"));
        assert!(live.worker.is_none());
        assert_eq!(
            live.inner.items().len(),
            crate::shots::fixture_catalog().len()
        );
    }
}
