//! An instance's side of moving its window to another workspace
//! ([`::collaboration::handoff`]): making what it shows durable when
//! asked, so the record the workspace's store moves holds what the person
//! left in it.
//!
//! "What it shows" is the primary document: the record a component is bound
//! to, else the App's own document.

use super::App;
use ::collaboration::CollabRequest;
use ::collaboration::handoff::HandoffReport;
use ::collaboration::handoff::HandoffRequest;

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
        let HandoffRequest::Checkpoint { token } = request;
        let report = match self.checkpoint_primary_document() {
            Ok(()) => HandoffReport::Done { token },
            Err(reason) => HandoffReport::Failed { token, reason },
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
    use crate::state::State;
    use ::collaboration::CollabEvent;
    use ::collaboration::CollaborationManager;
    use ::collaboration::PeerTransport;
    use ::collaboration::RouteSpec;
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
        assert_eq!(reports(&requests), [HandoffReport::Done { token: 1 }]);
    }
}
