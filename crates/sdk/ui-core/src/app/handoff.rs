//! An instance's side of moving its window to another workspace
//! ([`crate::collaboration::handoff`]): making what it shows durable when
//! asked, stamping it retired when the window has reopened elsewhere, and
//! noticing when a document it shows arrives retired from someone else.
//!
//! "What it shows" is the primary document: the record a component is bound
//! to, else the App's own document. A retirement stamped here is an ordinary
//! change to that document, published over its normal route, so everyone
//! holding the record learns it moved.

use super::App;
use crate::collaboration::CollabRequest;
use crate::collaboration::handoff::{HandoffReport, HandoffRequest, Successor};
use crate::record::transfer::{self, Retirement};

impl App {
    /// Run `f` on the document this window shows.
    fn with_primary_document<R>(
        &mut self,
        f: impl FnOnce(&mut automerge::AutoCommit) -> R,
    ) -> Result<R, String> {
        if !self.prepare_primary_document()? {
            return Ok(f(&mut self.state.doc));
        }
        let sync = self
            .actions
            .bound_record_sync
            .as_mut()
            .ok_or("primary record is unavailable")?;
        let _scope = sync.binding.enter().map_err(|error| format!("{error:?}"))?;
        sync.binding
            .records
            .with_action_state(&sync.binding.checkout.id, |doc, _, actions| {
                actions.commit_for_save(doc);
                f(doc)
            })
            .map_err(|error| error.to_string())
    }

    /// Make the document this window shows durable now, without closing
    /// anything.
    fn checkpoint_primary_document(&mut self) -> Result<(), String> {
        if self.prepare_primary_document()? {
            let sync = self
                .actions
                .bound_record_sync
                .as_ref()
                .ok_or("primary record is unavailable")?;
            let _scope = sync.binding.enter().map_err(|error| format!("{error:?}"))?;
            return sync
                .binding
                .records
                .try_flush_record(&sync.binding.checkout.id);
        }
        self.checkpoint_app_document()
    }

    #[cfg(feature = "native")]
    fn checkpoint_app_document(&mut self) -> Result<(), String> {
        if let Some(set) = self.persistence.records.as_mut() {
            set.try_flush(&mut self.state.doc)?;
            if let (Some(saver), Some(room)) = (
                self.persistence.autosave.as_mut(),
                self.persistence.room_index.as_mut(),
            ) {
                saver.request(room.document_mut());
                saver.flush();
            }
        } else if let Some(saver) = self.persistence.autosave.as_mut() {
            saver.request(&mut self.state.doc);
            saver.flush();
        }
        match self
            .persistence
            .autosave
            .as_ref()
            .and_then(|saver| saver.last_error())
        {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    #[cfg(not(feature = "native"))]
    #[allow(
        clippy::unnecessary_wraps,
        clippy::unused_self,
        reason = "a host without native persistence has nothing further to make durable"
    )]
    fn checkpoint_app_document(&self) -> Result<(), String> {
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
            HandoffRequest::Retire { token, successor } => {
                self.retire_primary_document(token, successor)
            }
        };
        self.report_handoff(&report);
    }

    fn retire_primary_document(&mut self, token: u64, successor: Successor) -> HandoffReport {
        let why = Retirement::Moved(Some(successor));
        let stamped = self.with_primary_document(|doc| {
            let before = doc.get_heads();
            transfer::stamp_retired(doc, &why).map(|()| before)
        });
        let before = match stamped {
            Ok(Ok(before)) => before,
            Ok(Err(error)) => {
                return HandoffReport::Failed {
                    token,
                    reason: error.to_string(),
                };
            }
            Err(reason) => return HandoffReport::Failed { token, reason },
        };
        // This window moved on; it neither edits nor re-reports its own stamp.
        self.continuation.retired = true;
        self.stop_editing_retired_document();
        self.broadcast_doc_delta();
        match self.checkpoint_primary_document() {
            Ok(()) => HandoffReport::done(token, &before),
            Err(reason) => HandoffReport::Failed { token, reason },
        }
    }

    /// After a document change arrived: if what this window shows was moved
    /// to another workspace by someone, stop editing it and tell the desktop,
    /// which knows whether the person can see where it went.
    pub(super) fn observe_primary_retirement(&mut self) {
        if self.continuation.retired {
            return;
        }
        let Ok(Some(retirement)) = self.with_primary_document(|doc| transfer::retirement(doc))
        else {
            return;
        };
        self.continuation.retired = true;
        self.stop_editing_retired_document();
        if let Retirement::Moved(Some(successor)) = retirement {
            self.report_handoff(&HandoffReport::Retired { successor });
        }
    }

    /// A retired document is history: whoever holds it may still read it,
    /// nobody edits it here any more.
    fn stop_editing_retired_document(&mut self) {
        let viewer = crate::permissions::PermissionContext::read_only;
        if let Some(sync) = self.actions.bound_record_sync.as_ref() {
            let read_only = viewer(&sync.binding.permissions.borrow());
            *sync.binding.permissions.borrow_mut() = read_only;
        }
        let read_only = viewer(&self.permission_ctx);
        self.set_permission_ctx(read_only);
    }

    pub(super) fn report_handoff(&mut self, report: &HandoffReport) -> bool {
        let Some(route) = self.private_continuity_route() else {
            tracing::warn!("handoff: no instance route to answer on");
            return false;
        };
        self.collaboration
            .manager
            .send(CollabRequest::ReportHandoff {
                app_id: route.app_id,
                message: report.encode(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collaboration::{CollabEvent, CollaborationManager, PeerTransport, RouteSpec};
    use crate::state::State;
    use automerge::ReadDoc as _;
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
                    record: record.clone(),
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
            record_kind: None,
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
                CollabRequest::ReportHandoff { app_id, message } => {
                    assert_eq!(app_id, "dev.tilo.handoff");
                    HandoffReport::decode(message)
                }
                _ => None,
            })
            .collect()
    }

    fn successor() -> Successor {
        Successor {
            room: "3f2b1c00-0000-4000-8000-0000000000f1".into(),
            record: "uuid:arrived".into(),
            by: "person:ada".into(),
        }
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn an_instance_checkpoints_then_stamps_its_document_retired_over_its_route() {
        let (mut app, requests) = instance();
        app.apply_handoff(HandoffRequest::Checkpoint { token: 1 });
        assert_eq!(reports(&requests), [HandoffReport::done(1, &[])]);
        requests.borrow_mut().clear();

        let before = app.state.doc.get_heads();
        app.apply_handoff(HandoffRequest::Retire {
            token: 2,
            successor: successor(),
        });
        assert_eq!(reports(&requests), [HandoffReport::done(2, &before)]);
        assert_eq!(
            transfer::retirement(&app.state.doc),
            Some(Retirement::Moved(Some(successor())))
        );
        assert!(
            requests
                .borrow()
                .iter()
                .any(|request| matches!(request, CollabRequest::Document { .. })),
            "the stamp goes out over the document's own route"
        );
        assert_eq!(
            app.permission_ctx.role(),
            Some(crate::permissions::Role::Viewer),
            "the window that moved on no longer edits"
        );
        app.observe_primary_retirement();
        assert!(
            !reports(&requests)
                .iter()
                .any(|report| matches!(report, HandoffReport::Retired { .. })),
            "its own stamp is not news to it"
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn a_retirement_arriving_from_elsewhere_stops_editing_and_is_reported_once() {
        let (mut app, requests) = instance();
        let mut moved = app.state.doc.fork();
        transfer::stamp_retired(&mut moved, &Retirement::Moved(Some(successor()))).unwrap();
        app.state
            .load_incremental(&moved.save_incremental())
            .unwrap();
        assert_eq!(
            app.state
                .doc
                .get(automerge::ROOT, "body")
                .unwrap()
                .map(|(v, _)| v.to_string()),
            Some("\"draft\"".into()),
            "the content stays readable"
        );
        app.observe_primary_retirement();
        app.observe_primary_retirement();
        assert_eq!(
            reports(&requests),
            [HandoffReport::Retired {
                successor: successor()
            }]
        );
        assert_eq!(
            app.permission_ctx.role(),
            Some(crate::permissions::Role::Viewer)
        );
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
        fn draw(
            &mut self,
            s: &mut vello::Scene,
            transform: vello::kurbo::Affine,
            renderer: &mut crate::TextRenderer,
            theme: &crate::Theme,
            presence: &crate::PresenceCtx,
        ) {
            self.text.draw(s, transform, renderer, theme, presence);
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
                    record_kind: None,
                    handle: None,
                })
        );
        app.collaboration.manager.poll();
        (app, ready, applied, requests)
    }
    fn view(app: &App, generation: u64, counter: u64) -> OwnerView {
        let route = &app.collaboration.manager.primary_route().unwrap().route;
        OwnerView {
            room: route.cluster_id.to_string(),
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
    #[test]
    fn owner_context_defers_then_cancels_without_focus_or_document_changes() {
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
    #[test]
    fn native_input_takes_over_and_old_remote_views_wait_for_the_newer_ack() {
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
    #[test]
    fn owner_context_refuses_foreign_routes_modal_and_secret_focus() {
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
    #[test]
    fn reconnect_reannounces_the_same_resource_and_fences_old_owner_stamps() {
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
                    record: route.record.clone(),
                    record_kind: None,
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
                    record_kind: None,
                    handle: None,
                })
        );
    }

    #[test]
    fn deferred_owner_view_is_fenced_before_a_reconnected_body_becomes_ready() {
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

    #[test]
    fn deferred_owner_view_cannot_cross_pending_or_acknowledged_document_routes() {
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

    #[test]
    fn protected_focus_clears_published_positions_and_defers_explicit_takeover() {
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
                crate::collaboration::CollabRequest::ClearContinuation { .. }
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
                    record,
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

    #[test]
    fn rejected_publication_cannot_cache_a_position_or_delay_a_takeover_retry() {
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
                        record: route.record,
                        record_kind: None,
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
    #[test]
    fn protected_reconnect_clears_unknown_prior_projection_once_per_acknowledged_epoch() {
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
                        crate::collaboration::CollabRequest::ClearContinuation { .. }
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
