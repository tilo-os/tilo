//! The Store's live application host.
//!
//! Keeping this wrapper in the library—not the binary shim—means every
//! launch path uses the same worker-backed lifecycle and gives the Tilo app
//! SDK a real `app_root()` contract to inspect and reuse.

use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, TaffyTree};
use ui::tilo_actions::{ControlInvocation, InvocationId, InvocationUpdate, Resource};
use ui::widget::{AppModel, ModelPath};
use ui::widget::{ResourceOperation, ResourceOperationStart, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use crate::FlatpakRef;
use crate::app::{ModerationIntent, RemoteIntent, StoreApp};
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
    worker: StoreWorker,
}

impl LiveStore {
    fn new(opened_ref: Option<FlatpakRef>, store_target: Option<String>) -> Self {
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
            worker: StoreWorker::spawn(opened_ref, search_index),
        }
    }
}

impl Widget for LiveStore {
    ui::provides!(
        dyn ui::widget::controls::ActionControls,
        dyn ui::resources::PublishesResources
    );

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        self.inner.local_resource_operation_handlers()
    }

    fn start_operation(
        &mut self,
        op: &ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ResourceOperationStart>, String> {
        self.inner.start_operation(op, doc, path)
    }

    fn poll_resource_operation(
        &mut self,
        invocation: InvocationId,
        cancellation_requested: bool,
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Option<InvocationUpdate> {
        self.inner
            .poll_resource_operation(invocation, cancellation_requested, doc, path)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "this is the single host sync point that drains worker snapshots and forwards every typed Store intent without optimistic state"
    )]
    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (state, path) = ui::doc::document(state, path);
        while let Some(snap) = self.worker.try_snapshot() {
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
                if !self
                    .worker
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
            && !self
                .worker
                .request_with_invocation(Request::Refresh, Some(invocation))
        {
            self.inner.complete_lifecycle_invocation(
                invocation,
                Some("store worker is unavailable".into()),
            );
        }
        if let Some(query) = self.inner.take_search_requested() {
            self.worker.request(Request::Search(query));
        }
        if let Some(id) = self.inner.take_detail_requested() {
            self.worker.request(Request::Detail(id));
        }
        for (intent, invocation) in self.inner.take_lifecycle_remote() {
            let request = match intent {
                RemoteIntent::Refresh => Request::RefreshRemotes,
                RemoteIntent::Add { name, url } => Request::RemoteAdd { name, url },
                RemoteIntent::Remove(name) => Request::RemoteRemove(name),
            };
            if !self
                .worker
                .request_with_invocation(request, Some(invocation))
            {
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
            if !self
                .worker
                .request_with_invocation(request, Some(invocation))
            {
                self.inner.complete_lifecycle_invocation(
                    invocation,
                    Some("Store worker is unavailable; the report draft was not submitted".into()),
                );
            }
        }
        self.inner.set_worker_busy(self.worker.has_pending());
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &ui::Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.inner.build_layout(taffy, theme, node_map)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.inner.apply_layout(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        self.inner
            .draw(scene, transform, text_renderer, theme, presence);
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
        permissions: &ui::permissions::PermissionContext,
    ) {
        self.inner.draw_with_presence(
            scene,
            transform,
            text_renderer,
            theme,
            presence,
            permissions,
        );
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        self.inner
            .draw_overlay(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        self.inner.handle_event(event, ctx);
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.inner.children()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.inner.children_mut()
    }

    fn name(&self) -> &'static str {
        "LiveStore"
    }

    fn get_rect(&self) -> Rect {
        self.inner.get_rect()
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.inner.accessibility()
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.inner.take_structure_dirty()
    }

    fn needs_animation_frame(&self) -> bool {
        self.worker.has_pending() || self.inner.needs_animation_frame()
    }
}

impl ui::resources::PublishesResources for LiveStore {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        self.inner.resources(doc)
    }
}

impl ui::widget::controls::ActionControls for LiveStore {
    fn take_control_invocation(&mut self) -> Option<ControlInvocation> {
        self.inner.take_control_invocation()
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
            assert_eq!(root.name(), "LiveStore");
            assert!(
                root.local_resource_operation_handlers()
                    .contains(&"store.refresh")
            );
        }

        // Case `launch_context_keeps_the_real_host_contract`.
        {
            let root = app_root_with_launch(None, Some("so.tilo.notes".into()));
            assert_eq!(root.name(), "LiveStore");
            assert!(root.needs_animation_frame());
        }

        // Case `a_fresh_root_reconnects_after_the_previous_host_is_dropped`.
        {
            // Dropping a Store root drops the worker request sender. The worker
            // exits after its current operation; a reopened app must always own a
            // fresh, independent host rather than retaining the old channel.
            drop(app_root());
            let reopened = app_root();
            assert_eq!(reopened.name(), "LiveStore");
            assert!(reopened.needs_animation_frame());
        }
    }
}
