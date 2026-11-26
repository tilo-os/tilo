use super::*;
use std::cell::RefCell;
use std::rc::Rc;
use tilo_ui_base::{
    collaboration::CollabEvent, collaboration::CollabRequest, collaboration::PeerTransport,
    collaboration::RouteSpec,
};

struct AcknowledgingPeer {
    connected: bool,
    acknowledged: Option<RouteSpec>,
    requests: Rc<RefCell<Vec<CollabRequest>>>,
}

impl PeerTransport for AcknowledgingPeer {
    fn is_connected(&self) -> bool {
        self.connected
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
}

fn app_for_acknowledged_route(route: RouteSpec) -> (App, Rc<RefCell<Vec<CollabRequest>>>) {
    let requests = Rc::new(RefCell::new(Vec::new()));
    let mut manager = CollaborationManager::with_peer_transport(AcknowledgingPeer {
        connected: true,
        acknowledged: None,
        requests: requests.clone(),
    });
    assert!(manager.send(CollabRequest::Hello {
        app_id: route.app_id,
        cluster_id: route.cluster_id,
        record: route.document,
        resource_type: None,
        handle: None,
    }));
    manager.poll();
    requests.borrow_mut().clear();
    let app = App::with_collaboration_manager(
        State::new(),
        Box::new(crate::widget::Text::new("continuity route fixture")),
        manager,
    );
    (app, requests)
}

#[test]
#[serial_test::serial(process_env)]
fn each_acknowledged_continuation_route_reports_once() {
    let routes = [
        RouteSpec::document("dev.tilo.notes", uuid::Uuid::new_v4(), "continuation:7"),
        RouteSpec::document("dev.tilo.browser", uuid::Uuid::new_v4(), "continuation:91"),
    ];
    for route in routes {
        let (_app, requests) = app_for_acknowledged_route(route);
        let reports = requests
            .borrow()
            .iter()
            .filter(|request| {
                matches!(
                    request,
                    CollabRequest::ReportContinuation { .. } | CollabRequest::ClearContinuation
                )
            })
            .count();
        assert_eq!(reports, 1);
    }
}

#[test]
#[serial_test::serial(process_env)]
fn authenticated_document_route_emits_no_private_continuity_requests() {
    let route = RouteSpec::document("so.tilo.calculator", uuid::Uuid::new_v4(), "tab:0013");
    let (_app, requests) = app_for_acknowledged_route(route);
    assert!(requests.borrow().iter().all(|request| !matches!(
        request,
        CollabRequest::ReportContinuation { .. } | CollabRequest::ClearContinuation
    )));
}

#[test]
#[serial_test::serial(process_env)]
fn route_acknowledgement_does_not_lose_the_first_unchanged_projection() {
    let requests = Rc::new(RefCell::new(Vec::new()));
    let manager = CollaborationManager::with_peer_transport(AcknowledgingPeer {
        connected: true,
        acknowledged: None,
        requests: requests.clone(),
    });
    let mut app = App::with_collaboration_manager(
        State::new(),
        Box::new(crate::widget::Text::new("delayed route fixture")),
        manager,
    );
    assert!(app.continuation.last_published.is_none());

    let route = RouteSpec::document("dev.tilo.notes", uuid::Uuid::new_v4(), "continuation:42");
    assert!(app.collaboration.manager.send(CollabRequest::Hello {
        app_id: route.app_id.clone(),
        cluster_id: route.cluster_id,
        record: route.document.clone(),
        resource_type: None,
        handle: None,
    }));
    app.collaboration.manager.poll();
    requests.borrow_mut().clear();
    app.publish_continuation_metadata();

    assert!(
        requests
            .borrow()
            .iter()
            .any(|request| matches!(request, CollabRequest::ReportContinuation { .. }))
    );
}

#[test]
#[serial_test::serial(process_env)]
fn a_new_acknowledgement_epoch_republishes_unchanged_continuity() {
    let route = RouteSpec::document("dev.tilo.notes", uuid::Uuid::new_v4(), "continuation:42");
    let (mut app, requests) = app_for_acknowledged_route(route.clone());
    requests.borrow_mut().clear();

    assert!(app.collaboration.manager.send(CollabRequest::Hello {
        app_id: route.app_id.clone(),
        cluster_id: route.cluster_id,
        record: route.document.clone(),
        resource_type: None,
        handle: None,
    }));
    app.collaboration.manager.poll();
    requests.borrow_mut().clear();
    app.publish_continuation_metadata();

    assert!(
        requests
            .borrow()
            .iter()
            .any(|request| matches!(request, CollabRequest::ReportContinuation { .. }))
    );
}
