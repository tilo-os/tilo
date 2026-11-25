//! An instance's side of moving its window to another workspace
//! ([`crate::collaboration::handoff`]): making what it shows durable when
//! asked, so the record the workspace's store moves holds what the person
//! left in it.
//!
//! "What it shows" is the primary document: the record a component is bound
//! to, else the App's own document.

use super::App;
use crate::collaboration::CollabRequest;
use crate::collaboration::handoff::{HandoffReport, HandoffRequest};

impl App {
    /// Make the document this window shows durable now, without closing
    /// anything.
    fn checkpoint_primary_document(&mut self) -> Result<(), String> {
        if self.prepare_primary_document()? {
            let sync = self
                .actions
                .bound_document_sync
                .as_ref()
                .ok_or("primary record is unavailable")?;
            let _scope = sync.binding.enter().map_err(|error| format!("{error:?}"))?;
            return sync
                .binding
                .records
                .try_flush_resource(&sync.binding.checkout.id);
        }
        Ok(())
    }

    /// Answer one handoff request from the shell.
    pub(super) fn apply_handoff(&mut self, request: HandoffRequest) {
        let report = match request {
            HandoffRequest::OwnerView { view } => {
                self.apply_owner_view(view);
                return;
            }
            HandoffRequest::Checkpoint { token } => match self.checkpoint_primary_document() {
                Ok(()) => HandoffReport::done(token, &[]),
                Err(reason) => HandoffReport::Failed { token, reason },
            },
        };
        self.report_handoff(&report);
    }

    pub(super) fn report_handoff(&mut self, report: &HandoffReport) -> bool {
        if self.private_continuity_route().is_none() {
            tracing::warn!("handoff: no instance route to answer on");
            return false;
        }
        self.collaboration
            .manager
            .send(CollabRequest::ReportHandoff {
                message: report.encode(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collaboration::{CollabEvent, CollaborationManager, PeerTransport, RouteSpec};
    use crate::state::State;
    use automerge::transaction::Transactable as _;
    use std::cell::RefCell;
    use std::rc::Rc;

    struct Instance {
        acknowledged: Option<RouteSpec>,
        requests: Rc<RefCell<Vec<CollabRequest>>>,
    }

    impl PeerTransport for Instance {
        fn is_connected(&self) -> bool {
            true
        }

        fn send(&mut self, request: CollabRequest) -> Result<(), String> {
            if let CollabRequest::Hello {
                app_id,
                cluster_id,
                record,
                ..
            } = &request
            {
                self.acknowledged = Some(RouteSpec {
                    app_id: app_id.clone(),
                    cluster_id: *cluster_id,
                    document: record.clone(),
                });
            }
            self.requests.borrow_mut().push(request);
            Ok(())
        }

        fn poll(&mut self) -> Vec<CollabEvent> {
            Vec::new()
        }

        fn take_primary_route_result(&mut self) -> Option<Result<RouteSpec, String>> {
            self.acknowledged.take().map(Ok)
        }
        fn publish_action_directory(&mut self, _: String) -> Result<(), String> {
            Ok(())
        }
    }

    pub(super) fn instance() -> (App, Rc<RefCell<Vec<CollabRequest>>>) {
        let requests = Rc::new(RefCell::new(Vec::new()));
        let mut manager = CollaborationManager::with_peer_transport(Instance {
            acknowledged: None,
            requests: requests.clone(),
        });
        assert!(manager.send(CollabRequest::Hello {
            app_id: "dev.tilo.handoff".into(),
            cluster_id: uuid::Uuid::new_v4(),
            record: Some("continuation:31".into()),
            resource_type: None,
            handle: None,
        }));
        manager.poll();
        let mut state = State::new();
        state.doc.put(automerge::ROOT, "body", "draft").unwrap();
        let app = App::with_collaboration_manager(
            state,
            Box::new(crate::widget::Text::new("handoff fixture")),
            manager,
        );
        requests.borrow_mut().clear();
        (app, requests)
    }

    fn reports(requests: &RefCell<Vec<CollabRequest>>) -> Vec<HandoffReport> {
        requests
            .borrow()
            .iter()
            .filter_map(|request| match request {
                CollabRequest::ReportHandoff { message } => HandoffReport::decode(message),
                _ => None,
            })
            .collect()
    }

    /// What the window shows is made durable when the shell asks, and the app says so on
    /// its own route.
    #[test]
    #[serial_test::serial(process_env)]
    fn an_app_checkpoints_what_it_shows_when_asked() {
        let (mut app, requests) = instance();
        app.apply_handoff(HandoffRequest::Checkpoint { token: 1 });
        assert_eq!(reports(&requests), [HandoffReport::done(1, &[])]);
    }
}

#[cfg(test)]
mod owner_view_tests {
    use super::*;
    use crate::collaboration::{
        CollabEvent,
        owner_view::{OwnerProjection, OwnerView},
    };
    use crate::widget::{Continuable, Widget};
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
    };

    struct Receiver {
        text: crate::widget::Text,
        ready: Rc<Cell<bool>>,
        applied: Rc<RefCell<Vec<Vec<u8>>>>,
    }
    impl Widget for Receiver {
        crate::provides!(dyn Continuable);
        fn build_layout(
            &mut self,
            t: &mut taffy::TaffyTree<()>,
            theme: &crate::Theme,
            map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
        ) -> taffy::NodeId {
            self.text.build_layout(t, theme, map)
        }
        fn apply_layout(
            &mut self,
            t: &taffy::TaffyTree<()>,
            n: taffy::NodeId,
            o: vello::kurbo::Point,
        ) {
            self.text.apply_layout(t, n, o);
        }
        fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
            #[allow(
                unused_variables,
                unused_mut,
                reason = "a draw binds only the parts it uses"
            )]
            let (s, transform, renderer, theme, mut env) = ctx.parts();
            self.text.draw(&mut crate::draw::DrawCtx::new(
                s,
                transform,
                renderer,
                theme,
                env.clone(),
            ));
        }
        fn name(&self) -> &'static str {
            "owner-receiver"
        }
        fn get_rect(&self) -> vello::kurbo::Rect {
            self.text.get_rect()
        }
    }
    impl Continuable for Receiver {
        fn continuation_metadata(
            &self,
            _: &automerge::AutoCommit,
            _: &automerge::ObjId,
        ) -> Option<crate::collaboration::ContinuationMetadata> {
            Some(crate::collaboration::ContinuationMetadata {
                schema_version: 1,
                bytes: b"local-position".to_vec(),
            })
        }
        fn restore_continuation_metadata(
            &mut self,
            _: &crate::collaboration::ContinuationMetadata,
            _: &mut crate::widget::Edits<'_>,
            _: &automerge::ObjId,
        ) -> bool {
            false
        }
        fn restore_live_continuation_metadata(
            &mut self,
            metadata: &crate::collaboration::ContinuationMetadata,
            _: &automerge::AutoCommit,
            _: &automerge::ObjId,
        ) -> bool {
            if !self.ready.get() {
                return false;
            }
            self.applied.borrow_mut().push(metadata.bytes.clone());
            true
        }
    }
    struct Surface;
    impl crate::app::AppSurface for Surface {
        fn scale_factor(&self) -> f64 {
            1.5
        }
        fn inner_size(&self) -> winit::dpi::PhysicalSize<u32> {
            winit::dpi::PhysicalSize::new(1200, 900)
        }
        fn request_redraw(&self) {}
        fn set_cursor(&self, _: winit::window::CursorIcon) {}
    }
    type Requests = Rc<RefCell<Vec<crate::collaboration::CollabRequest>>>;
    fn receiver() -> (App, Rc<Cell<bool>>, Rc<RefCell<Vec<Vec<u8>>>>) {
        let (app, ready, applied, _) = receiver_with_requests();
        (app, ready, applied)
    }
    fn receiver_with_requests() -> (App, Rc<Cell<bool>>, Rc<RefCell<Vec<Vec<u8>>>>, Requests) {
        let (mut app, requests) = super::tests::instance();
        let ready = Rc::new(Cell::new(true));
        let applied = Rc::new(RefCell::new(Vec::new()));
        app.root_widget = Box::new(Receiver {
            text: crate::widget::Text::new("fixture"),
            ready: ready.clone(),
            applied: applied.clone(),
        });
        assert!(
            app.collaboration
                .manager
                .send(crate::collaboration::CollabRequest::Hello {
                    app_id: "dev.tilo.handoff".into(),
                    cluster_id: uuid::Uuid::new_v4(),
                    record: Some("continuation:31".into()),
                    resource_type: None,
                    handle: None,
                })
        );
        app.collaboration.manager.poll();
        (app, ready, applied, requests)
    }
    fn view(app: &App, generation: u64, counter: u64) -> OwnerView {
        let route = &app.collaboration.manager.primary_route().unwrap().route;
        OwnerView {
            workspace: route.cluster_id.to_string(),
            app: route.app_id.clone(),
            instance: 31,
            generation,
            counter,
            source: "peerA".into(),
            local_claim: false,
            projection: Some(OwnerProjection {
                schema_version: 1,
                bytes: b"remote-position".to_vec(),
            }),
        }
    }
    fn deliver(app: &mut App, view: OwnerView) {
        app.apply_collab_event_for_test(CollabEvent::Handoff {
            request: HandoffRequest::OwnerView { view },
        });
    }
    fn reannounce(app: &mut App, record: &str) {
        let route = app
            .collaboration
            .manager
            .primary_route()
            .unwrap()
            .route
            .clone();
        assert!(
            app.collaboration
                .manager
                .send(crate::collaboration::CollabRequest::Hello {
                    app_id: route.app_id,
                    cluster_id: route.cluster_id,
                    record: Some(record.into()),
                    resource_type: None,
                    handle: None,
                })
        );
    }

    struct RefusingPublication {
        acknowledged: Option<crate::collaboration::RouteSpec>,
    }
    impl crate::collaboration::PeerTransport for RefusingPublication {
        fn is_connected(&self) -> bool {
            true
        }
        fn send(&mut self, request: crate::collaboration::CollabRequest) -> Result<(), String> {
            if let crate::collaboration::CollabRequest::Hello {
                app_id,
                cluster_id,
                record,
                ..
            } = request
            {
                self.acknowledged = Some(crate::collaboration::RouteSpec {
                    app_id,
                    cluster_id,
                    document: record,
                });
                Ok(())
            } else {
                Err("synthetic closed publication queue".into())
            }
        }
        fn poll(&mut self) -> Vec<CollabEvent> {
            Vec::new()
        }
        fn take_primary_route_result(
            &mut self,
        ) -> Option<Result<crate::collaboration::RouteSpec, String>> {
            self.acknowledged.take().map(Ok)
        }
    }

    tilo_test_env::scenario! {
        fn owner_view_handoff_defers_fences_and_protects;

        phase owner_context_defers_then_cancels_without_focus_or_document_changes;
        {
            let (mut app, ready, applied) = receiver();
            let heads = app.state.doc.get_heads();
            let focus = app.focused_node;
            ready.set(false);
            let first = view(&app, 1, 5);
            deliver(&mut app, first.clone());
            assert!(app.continuation.owner_view.pending.is_some());
            let mut cancel = first.clone();
            cancel.generation = 2;
            cancel.projection = None;
            deliver(&mut app, cancel);
            ready.set(true);
            app.publish_continuation_metadata();
            assert!(applied.borrow().is_empty());
            assert!(app.continuation.owner_view.pending.is_none());
            deliver(&mut app, first); // old queued incarnation is inert
            assert!(applied.borrow().is_empty());
            let reconnect = view(&app, 3, 5);
            deliver(&mut app, reconnect);
            assert_eq!(applied.borrow().len(), 1);
            assert_eq!(app.focused_node, focus);
            assert_eq!(app.state.doc.get_heads(), heads);
        }

        phase native_input_takes_over_and_old_remote_views_wait_for_the_newer_ack;
        {
            let (mut app, _, applied) = receiver();
            let first = view(&app, 1, 5);
            deliver(&mut app, first);
            app.init_layout();
            app.surface_input(&Surface, crate::synthetic_input::mouse_press(), false);
            assert!(app.continuation.owner_view.awaiting_claim);
            assert!(!app.continuation.owner_view.following);
            let old = view(&app, 2, 5);
            deliver(&mut app, old);
            assert_eq!(applied.borrow().len(), 1);
            let mut ack = view(&app, 3, 6);
            ack.local_claim = true;
            ack.source = "localB".into();
            ack.projection = None;
            deliver(&mut app, ack);
            assert!(!app.continuation.owner_view.awaiting_claim);
            let stale = view(&app, 4, 5);
            deliver(&mut app, stale);
            assert_eq!(applied.borrow().len(), 1);
            let mut new_owner = view(&app, 5, 6);
            new_owner.source = "peerC".into();
            deliver(&mut app, new_owner);
            assert_eq!(applied.borrow().len(), 2);
        }

        phase owner_context_refuses_foreign_routes_modal_and_secret_focus;
        {
            let (mut app, _, applied) = receiver();
            let mut foreign = view(&app, 1, 5);
            foreign.instance = 32;
            deliver(&mut app, foreign);
            assert!(applied.borrow().is_empty());
            app.accessibility.focus_trap_active = true;
            let modal = view(&app, 2, 5);
            deliver(&mut app, modal);
            assert!(applied.borrow().is_empty());
            assert!(app.continuation.owner_view.pending.is_none());
            app.accessibility.focus_trap_active = false;
            app.floating.capture_protected = true;
            let secret = view(&app, 3, 6);
            deliver(&mut app, secret);
            assert!(applied.borrow().is_empty());
            assert!(app.continuation.owner_view.pending.is_none());
        }

        phase reconnect_reannounces_the_same_resource_and_fences_old_owner_stamps;
        {
            let (mut app, ready, applied) = receiver();
            ready.set(false);
            let remote = view(&app, 8, 12);
            deliver(&mut app, remote.clone());
            let route = app
                .collaboration
                .manager
                .primary_route()
                .unwrap()
                .route
                .clone();
            let heads = app.state.doc.get_heads();
            assert!(
                app.collaboration
                    .manager
                    .send(crate::collaboration::CollabRequest::Hello {
                        app_id: route.app_id.clone(),
                        cluster_id: route.cluster_id,
                        record: route.document.clone(),
                        resource_type: None,
                        handle: None,
                    })
            );
            app.collaboration.manager.poll();
            assert_eq!(app.private_continuity_route(), Some(route));
            assert!(app.continuation.owner_view.pending.is_none());
            ready.set(true);
            let mut replay = remote;
            replay.generation = 1;
            deliver(&mut app, replay);
            assert_eq!(
                applied.borrow().len(),
                1,
                "a fresh host generation may replay the same durable stamp"
            );
            let stale = view(&app, 2, 11);
            deliver(&mut app, stale);
            assert_eq!(applied.borrow().len(), 1);
            assert_eq!(app.state.doc.get_heads(), heads);
        }

        phase deferred_owner_view_is_fenced_before_a_reconnected_body_becomes_ready;
        {
            let (mut app, ready, applied) = receiver();
            let heads = app.state.doc.get_heads();
            ready.set(false);
            let remote = view(&app, 8, 12);
            deliver(&mut app, remote);
            assert!(app.continuation.owner_view.pending.is_some());
            reannounce(&mut app, "continuation:31");
            app.collaboration.manager.poll();
            ready.set(true);
            // Drive the production publication turn, without separately invoking
            // the route helper that would otherwise hide this ordering defect.
            app.publish_continuation_metadata();
            assert!(applied.borrow().is_empty());
            assert!(app.continuation.owner_view.pending.is_none());
            assert!(app.continuation.owner_view.following);
            assert_eq!(app.state.doc.get_heads(), heads);
        }

        phase deferred_owner_view_cannot_cross_pending_or_acknowledged_document_routes;
        {
            for acknowledged in [false, true] {
                let (mut app, ready, applied) = receiver();
                let heads = app.state.doc.get_heads();
                let focus = app.focused_node;
                ready.set(false);
                let remote = view(&app, 1, 5);
                deliver(&mut app, remote);
                assert!(app.continuation.owner_view.pending.is_some());
                reannounce(&mut app, "tab:000000000000002a");
                if acknowledged {
                    app.collaboration.manager.poll();
                }
                ready.set(true);
                app.publish_continuation_metadata();
                assert!(
                    applied.borrow().is_empty(),
                    "document acknowledgement={acknowledged}"
                );
                assert!(app.continuation.owner_view.pending.is_none());
                assert_eq!(app.focused_node, focus);
                assert_eq!(app.state.doc.get_heads(), heads);
            }
        }

        phase protected_focus_clears_published_positions_and_defers_explicit_takeover;
        {
            for modal in [false, true] {
                let (mut app, _, _, requests) = receiver_with_requests();
                let heads = app.state.doc.get_heads();
                app.publish_continuation_metadata();
                assert!(app.continuation.last_published.is_some());
                requests.borrow_mut().clear();
                let remote = view(&app, 1, 5);
                deliver(&mut app, remote);
                app.take_over_owner_view();
                if modal {
                    app.accessibility.focus_trap_active = true;
                } else {
                    app.floating.capture_protected = true;
                }
                app.publish_continuation_metadata();
                assert!(app.continuation.last_published.is_none());
                assert!(app.continuation.owner_view.last_claim_at.is_none());
                assert!(app.continuation.owner_view.awaiting_claim);
                assert!(requests.borrow().iter().any(|request| matches!(
                    request,
                    crate::collaboration::CollabRequest::ClearContinuation
                )));
                assert!(requests.borrow().iter().all(|request| !matches!(
                    request,
                    crate::collaboration::CollabRequest::ReportContinuation { .. }
                        | crate::collaboration::CollabRequest::ReportHandoff { .. }
                )));
                app.accessibility.focus_trap_active = false;
                app.floating.capture_protected = false;
                app.publish_continuation_metadata();
                assert!(app.continuation.last_published.is_some());
                assert!(app.continuation.owner_view.last_claim_at.is_some());
                assert_eq!(app.state.doc.get_heads(), heads);
            }
        }

        phase rejected_publication_cannot_cache_a_position_or_delay_a_takeover_retry;
        {
            for takeover in [false, true] {
                let (mut app, _, _) = receiver();
                if takeover {
                    let remote = view(&app, 1, 5);
                    deliver(&mut app, remote);
                    app.take_over_owner_view();
                }
                let route = app
                    .collaboration
                    .manager
                    .primary_route()
                    .unwrap()
                    .route
                    .clone();
                app.collaboration.manager =
                    crate::collaboration::CollaborationManager::with_peer_transport(
                        RefusingPublication { acknowledged: None },
                    );
                assert!(
                    app.collaboration
                        .manager
                        .send(crate::collaboration::CollabRequest::Hello {
                            app_id: route.app_id,
                            cluster_id: route.cluster_id,
                            record: route.document,
                            resource_type: None,
                            handle: None
                        })
                );
                app.collaboration.manager.poll();
                app.publish_continuation_metadata();
                assert!(app.continuation.last_published.is_none());
                assert!(app.continuation.owner_view.last_claim_at.is_none());
                assert!(matches!(
                    app.collaboration.manager.state(),
                    crate::collaboration::CollaborationState::Offline(_)
                ));
            }
        }

        phase protected_reconnect_clears_unknown_prior_projection_once_per_acknowledged_epoch;
        {
            let (mut app, _, _, requests) = receiver_with_requests();
            let heads = app.state.doc.get_heads();
            app.floating.capture_protected = true;
            requests.borrow_mut().clear();
            for epoch in 1..=2 {
                if epoch == 2 {
                    reannounce(&mut app, "continuation:31");
                    app.collaboration.manager.poll();
                }
                app.publish_continuation_metadata();
                app.publish_continuation_metadata();
                assert_eq!(
                    requests
                        .borrow()
                        .iter()
                        .filter(|request| matches!(
                            request,
                            crate::collaboration::CollabRequest::ClearContinuation
                        ))
                        .count(),
                    epoch
                );
                assert!(requests.borrow().iter().all(|request| !matches!(
                    request,
                    crate::collaboration::CollabRequest::ReportContinuation { .. }
                        | crate::collaboration::CollabRequest::ReportHandoff { .. }
                )));
                assert!(app.continuation.publication_initialized);
                assert!(app.continuation.last_published.is_none());
            }
            assert_eq!(app.state.doc.get_heads(), heads);
        }
    }
}
