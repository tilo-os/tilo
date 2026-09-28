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
    fn checkpoint_app_document(&mut self) -> Result<(), String> {
        Ok(())
    }

    /// Answer one handoff request from the shell.
    pub(super) fn apply_handoff(&mut self, request: HandoffRequest) {
        let report = match request {
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
        let viewer = |current: &crate::permissions::PermissionContext| {
            crate::permissions::PermissionContext::from_capabilities(
                crate::permissions::Actor::person(
                    Some(crate::permissions::Role::Viewer),
                    current.user().0.clone(),
                ),
                Vec::new(),
            )
        };
        if let Some(sync) = self.actions.bound_record_sync.as_ref() {
            let read_only = viewer(&sync.binding.permissions.borrow());
            *sync.binding.permissions.borrow_mut() = read_only;
        }
        let read_only = viewer(&self.permission_ctx);
        self.set_permission_ctx(read_only);
    }

    fn report_handoff(&mut self, report: &HandoffReport) {
        let Some(route) = self.private_continuity_route() else {
            tracing::warn!("handoff: no instance route to answer on");
            return;
        };
        let _ = self
            .collaboration
            .manager
            .send(CollabRequest::ReportHandoff {
                app_id: route.app_id,
                message: report.encode(),
            });
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
    }

    fn instance() -> (App, Rc<RefCell<Vec<CollabRequest>>>) {
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
