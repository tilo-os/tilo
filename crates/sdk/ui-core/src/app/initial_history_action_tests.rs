// Socket-backed original history must precede public operation admission.
use automerge::ReadDoc as _;
struct InitialHistoryProbe(
    std::rc::Rc<std::cell::Cell<usize>>,
    std::rc::Rc<std::cell::Cell<bool>>,
);
impl crate::widget::Widget for InitialHistoryProbe {
    crate::provides!(
        dyn crate::resources::PublishesResources,
        dyn crate::widget::controls::ActionControls
    );
    crate::testing::inert_widget_methods!(unregistered_leaf_layout, name = "initial-history-probe");
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &["initial-history.probe"]
    }
    fn start_operation(
        &mut self,
        _: &crate::widget::ResourceOperation<'_>,
        _: &mut crate::widget::Edits<'_>,
        _: &automerge::ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        self.0.set(self.0.get().saturating_add(1));
        Ok(Some(crate::widget::ResourceOperationStart::Completed(
            tilo_actions::ActionOutput::default(),
        )))
    }
}
impl crate::widget::controls::ActionControls for InitialHistoryProbe {
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.1
            .replace(false)
            .then(|| tilo_actions::ControlInvocation {
                address: tilo_actions::ActionAddress::new("initial-probe", "inspect"),
                arguments: serde_json::Map::new(),
            })
    }
}
impl crate::resources::PublishesResources for InitialHistoryProbe {
    fn resources(&self, _: &automerge::AutoCommit) -> Vec<tilo_actions::Resource> {
        vec![
            tilo_actions::Resource::new("initial-probe", "probe", "Probe").with_action(
                tilo_actions::ActionSpec::operation(
                    "inspect",
                    "Inspect",
                    tilo_actions::OperationKind::View,
                    "initial-history.probe",
                    &true,
                )
                .unwrap()
                .with_contract(tilo_actions::ActionContract::view()),
            ),
        ]
    }
}
fn initial_history_query() -> runtime_collab::ActionDirectoryQuery {
    runtime_collab::ActionDirectoryQuery {
        peer_id: 0,
        revision: String::new(),
        cursor: vec![],
        needle: String::new(),
        parent_resource: String::new(),
        max_results: 16,
        max_bytes: 8192,
        max_work: 32,
        max_depth: 8,
        resource_name: String::new(),
    }
}
fn initial_history_app() -> (App, std::rc::Rc<std::cell::Cell<usize>>) {
    let calls = std::rc::Rc::new(std::cell::Cell::new(0));
    let mut history = automerge::AutoCommit::new();
    history
        .put(ROOT, "original-owner-history", "retained")
        .unwrap();
    history.commit();
    let (manager, _) = CollaborationManager::native_initial_fixture_for_test(
        "so.tilo.initial-history-probe",
        uuid::Uuid::new_v4(),
        history.save(),
    );
    let mut app = App::new_with_controlled_inputs(
        State::new(),
        Box::new(InitialHistoryProbe(
            calls.clone(),
            std::rc::Rc::new(std::cell::Cell::new(false)),
        )),
    );
    app.collaboration.local_author = crate::permissions::UserId("fixture-user".into());
    app.collaboration.manager = manager;
    (app, calls)
}
#[test]
fn initial_history_action_publication_waits_for_actual_owner_application() {
    let (mut app, calls) = initial_history_app();
    assert!(app.collaboration.manager.is_connected());
    assert!(!app.collaboration.manager.document_route_ready(None));
    assert!(!app.published_action_history_ready());
    let admission = app.native_admission_snapshot();
    assert!(admission.connection.manager_connected);
    assert!(!admission.history.action_history_ready);
    assert!(!admission.directory.directory_enqueued);
    app.publish_action_surface();
    assert!(app.actions.last_published_actions.is_none());
    assert!(
        app.published_action_directory_page(initial_history_query())
            .rows
            .is_empty()
    );
    assert!(matches!(
        app.published_action_directory_resolution("", "initial-probe", "inspect"),
        runtime_collab::ActionDirectoryResolution::Unavailable
    ));
    let deadline = tilo_test_env::hang_deadline();
    while !app.published_action_history_ready() {
        app.poll_and_apply_collab_events();
        assert!(
            crate::time::now() < deadline,
            "actual original history never applied"
        );
        std::thread::yield_now();
    }
    assert!(
        app.state
            .doc
            .get(ROOT, "original-owner-history")
            .unwrap()
            .is_some()
    );
    app.publish_action_surface();
    assert!(app.actions.last_published_actions.is_some());
    let admission = app.native_admission_snapshot();
    assert!(admission.history.action_history_ready);
    assert!(admission.history.primary_history_ready);
    assert!(admission.history.all_owner_histories_ready);
    assert!(admission.connection.custody_reconciled);
    assert!(admission.directory.directory_enqueued);
    let page = app.published_action_directory_page(initial_history_query());
    assert!(!page.rows.is_empty());
    assert!(matches!(
        app.published_action_directory_resolution(&page.revision, "initial-probe", "inspect"),
        runtime_collab::ActionDirectoryResolution::Found { .. }
    ));
    app.invoke_published_action(
        77,
        tilo_actions::InvocationId::new(),
        "initial-probe".into(),
        "inspect".into(),
        None,
        "fixture-user",
        None,
    );
    assert_eq!(
        calls.get(),
        1,
        "the actual public handler runs only after owner application"
    );
}
#[test]
fn initial_history_accepted_delivery_cannot_run_before_original_owner_application() {
    let (mut app, calls) = initial_history_app();
    app.invoke_published_action(
        78,
        tilo_actions::InvocationId::new(),
        "initial-probe".into(),
        "inspect".into(),
        None,
        "fixture-user",
        None,
    );
    assert_eq!(calls.get(), 0);
    assert!(!app.collaboration.manager.is_connected());
    assert!(
        app.action_publication_failure
            .as_deref()
            .unwrap()
            .contains("outcome unknown")
    );
}

#[test]
fn initial_history_retired_capture_refuses_previously_published_handler() {
    let (mut app, calls) = initial_history_app();
    let deadline = tilo_test_env::hang_deadline();
    while !app.published_action_history_ready() {
        app.poll_and_apply_collab_events();
        assert!(
            crate::time::now() < deadline,
            "actual original history never applied"
        );
        std::thread::yield_now();
    }
    app.publish_action_surface();
    assert!(app.actions.last_published_actions.is_some());
    let page = app.published_action_directory_page(initial_history_query());
    assert!(!page.rows.is_empty());
    let route = app
        .collaboration
        .manager
        .primary_route()
        .unwrap()
        .route
        .clone();
    let unrelated = crate::collaboration::RouteSpec::record(
        &route.app_id,
        route.cluster_id,
        "unrelated-record",
    );
    app.collaboration
        .manager
        .retire_document_capture_for_test(&unrelated);
    assert!(
        app.published_action_history_ready(),
        "an unrelated record is outside the actual live owner map"
    );
    app.collaboration
        .manager
        .retire_document_capture_for_test(&route);
    assert!(matches!(
        app.published_action_directory_resolution(&page.revision, "initial-probe", "inspect"),
        runtime_collab::ActionDirectoryResolution::Unavailable
    ));
    app.invoke_published_action(
        79,
        tilo_actions::InvocationId::new(),
        "initial-probe".into(),
        "inspect".into(),
        None,
        "fixture-user",
        None,
    );
    assert_eq!(
        calls.get(),
        0,
        "capture retirement cannot authorize an already offered handler"
    );
    assert!(!app.collaboration.manager.is_connected());
}

struct NoCommittedReadPeer {
    requests: std::rc::Rc<std::cell::RefCell<Vec<crate::collaboration::CollabRequest>>>,
    directories: std::rc::Rc<std::cell::Cell<usize>>,
}
impl crate::collaboration::PeerTransport for NoCommittedReadPeer {
    fn is_connected(&self) -> bool {
        true
    }
    fn send(&mut self, request: crate::collaboration::CollabRequest) -> Result<(), String> {
        self.requests.borrow_mut().push(request);
        Ok(())
    }
    fn poll(&mut self) -> Vec<crate::collaboration::CollabEvent> {
        vec![]
    }
    fn publish_action_directory(&mut self, _: String) -> Result<(), String> {
        self.directories
            .set(self.directories.get().saturating_add(1));
        Ok(())
    }
}
#[test]
fn initial_history_no_committed_read_peer_preserves_actual_public_dispatch() {
    let (mut app, calls) = initial_history_app();
    let requests = std::rc::Rc::new(std::cell::RefCell::new(vec![]));
    let directories = std::rc::Rc::new(std::cell::Cell::new(0));
    app.collaboration.manager = CollaborationManager::with_peer_transport(NoCommittedReadPeer {
        requests: requests.clone(),
        directories: directories.clone(),
    });
    assert!(app.collaboration.manager.primary_route().is_none());
    app.publish_action_surface();
    assert_eq!(directories.get(), 1);
    assert!(app.actions.last_published_actions.is_some());
    let page = app.published_action_directory_page(initial_history_query());
    assert!(!page.rows.is_empty());
    let invocation = tilo_actions::InvocationId::new();
    app.invoke_published_action(
        80,
        invocation,
        "initial-probe".into(),
        "inspect".into(),
        None,
        "fixture-user",
        None,
    );
    assert_eq!(calls.get(), 1);
    assert!(requests.borrow().iter().any(|request| matches!(request,
        crate::collaboration::CollabRequest::UpdateInvocation {
            update: tilo_actions::InvocationUpdate::Succeeded { invocation: actual, .. }, ..
        } if *actual == invocation)));
}

#[test]
fn initial_history_public_directory_requires_every_actual_live_record_owner() {
    let mut fixture = RoutedOwners::new();
    let manager = CollaborationManager::native_initial_named_fixture_for_test(
        fixture.bindings[0].broker_route().unwrap(),
        automerge::AutoCommit::new().save(),
    );
    fixture.app.collaboration.manager = manager;
    fixture.app.actions.bound_broker_route_for_test = None;
    assert!(
        fixture
            .app
            .published_action_directory_page(initial_history_query())
            .rows
            .is_empty()
    );
    let deadline = tilo_test_env::hang_deadline();
    while !fixture.app.published_action_history_ready() {
        fixture.app.poll_and_apply_collab_events();
        assert!(
            crate::time::now() < deadline,
            "the actual two record histories never applied"
        );
        std::thread::yield_now();
    }
    assert_eq!(fixture.app.document_routes.owners.len(), 2);
    fixture.app.publish_action_surface();
    assert!(fixture.app.actions.last_published_actions.is_some());
    let page = fixture
        .app
        .published_action_directory_page(initial_history_query());
    assert!(!page.rows.is_empty());
    // A root directory page lists resources; its action children are reached
    // by their parent. The eager surface supplies the actual published address.
    let action = fixture
        .app
        .actions
        .last_published_actions
        .as_ref()
        .and_then(|actions| actions.first())
        .expect("actual published record action")
        .clone();
    assert!(matches!(
        fixture.app.published_action_directory_resolution(
            &page.revision,
            &action.address.resource,
            &action.address.action
        ),
        runtime_collab::ActionDirectoryResolution::Found { .. }
    ));
    let second = fixture.app.document_routes.owners[1].route.clone();
    fixture
        .app
        .collaboration
        .manager
        .retire_document_capture_for_test(&second);
    assert!(!fixture.app.published_action_history_ready());
    assert!(
        fixture
            .app
            .published_action_directory_page(initial_history_query())
            .rows
            .is_empty()
    );
    assert!(matches!(
        fixture.app.published_action_directory_resolution(
            &page.revision,
            &action.address.resource,
            &action.address.action
        ),
        runtime_collab::ActionDirectoryResolution::Unavailable
    ));
}

#[test]
fn initial_history_worker_readiness_wakes_parked_owner_for_publication() {
    let mut scope = crate::wake::WakeScope::new();
    let (wake_tx, wakes) = std::sync::mpsc::channel();
    let wake_tx = std::sync::Mutex::new(wake_tx);
    scope
        .bind(move |_| {
            wake_tx.lock().unwrap().send(()).unwrap();
        })
        .unwrap();
    let mut history = automerge::AutoCommit::new();
    history
        .put(ROOT, "original-owner-history", "retained")
        .unwrap();
    history.commit();
    let (release, gate) = tokio::sync::oneshot::channel();
    let (manager, rechecks) = {
        let _entry = scope.enter();
        CollaborationManager::native_initial_parked_fixture_for_test(
            "so.tilo.initial-history-probe",
            uuid::Uuid::new_v4(),
            history.save(),
            gate,
        )
    };
    let mut app = App::new_with_controlled_inputs(
        State::new(),
        Box::new(InitialHistoryProbe(
            std::rc::Rc::new(std::cell::Cell::new(0)),
            std::rc::Rc::new(std::cell::Cell::new(false)),
        )),
    );
    app.collaboration.manager = manager;
    let deadline = tilo_test_env::hang_deadline();
    loop {
        app.pump_collab_presence();
        if app
            .state
            .doc
            .get(ROOT, "original-owner-history")
            .unwrap()
            .is_some()
        {
            break;
        }
        wakes
            .recv_timeout(deadline.saturating_duration_since(crate::time::now()))
            .expect("initial committed history must wake its actual owner");
    }
    // Readiness is deliberately withheld while the owner finishes its last pump.
    assert!(!app.published_action_history_ready());
    app.pump_collab_presence();
    while wakes.try_recv().is_ok() {}
    assert!(app.actions.last_published_actions.is_none());
    release.send(()).unwrap();
    // No owner polling, yield loop or synthetic event supplies this notification.
    wakes
        .recv_timeout(crate::time::Duration::from_secs(3))
        .expect("worker readiness transition must wake the parked owner");
    assert!(
        rechecks() > 0,
        "the socket worker must recheck its applied capture"
    );
    app.pump_collab_presence();
    assert!(app.published_action_history_ready());
    assert!(app.actions.last_published_actions.is_some());
    assert!(
        !app.published_action_directory_page(initial_history_query())
            .rows
            .is_empty()
    );
}

#[test]
fn initial_history_local_control_is_not_admitted_while_committed_capture_is_pending() {
    let calls = std::rc::Rc::new(std::cell::Cell::new(0));
    let queued = std::rc::Rc::new(std::cell::Cell::new(true));
    let mut history = automerge::AutoCommit::new();
    history
        .put(ROOT, "original-owner-history", "retained")
        .unwrap();
    history.commit();
    let (release, gate) = tokio::sync::oneshot::channel();
    let (manager, _) = CollaborationManager::native_initial_parked_fixture_for_test(
        "so.tilo.initial-history-probe",
        uuid::Uuid::new_v4(),
        history.save(),
        gate,
    );
    let mut app = App::new_with_controlled_inputs(
        State::new(),
        Box::new(InitialHistoryProbe(calls.clone(), queued.clone())),
    );
    app.collaboration.local_author = crate::permissions::UserId("fixture-user".into());
    app.collaboration.manager = manager;
    let deadline = tilo_test_env::hang_deadline();
    while app
        .state
        .doc
        .get(ROOT, "original-owner-history")
        .unwrap()
        .is_none()
    {
        app.pump_collab_presence();
        assert!(
            crate::time::now() < deadline,
            "actual original capture never reached its owner"
        );
        std::thread::yield_now();
    }
    assert!(!app.published_action_history_ready());
    assert_eq!(app.dispatch_control_invocations(), 0);
    assert!(
        queued.get(),
        "the original intent must remain with its producer, before admission"
    );
    assert_eq!(calls.get(), 0);
    assert!(app.collaboration.manager.is_connected());
    assert!(app.action_publication_failure.is_none());
    release.send(()).unwrap();
    while !app.published_action_history_ready() {
        app.pump_collab_presence();
        assert!(
            crate::time::now() < deadline,
            "original capture recheck never completed"
        );
        std::thread::yield_now();
    }
    assert_eq!(app.dispatch_control_invocations(), 1);
    assert!(!queued.get());
    assert_eq!(calls.get(), 1);
    assert_eq!(app.dispatch_control_invocations(), 0);
    assert_eq!(
        calls.get(),
        1,
        "the intent crosses the original door exactly once"
    );
    assert!(app.collaboration.manager.is_connected());
    assert!(app.action_publication_failure.is_none());
}

#[test]
fn initial_history_external_job10_waits_for_subsequent_capture_on_original_live_owner() {
    let calls = std::rc::Rc::new(std::cell::Cell::new(0));
    let mut history = automerge::AutoCommit::new();
    history
        .put(ROOT, "original-owner-history", "retained")
        .unwrap();
    history.commit();
    let fixture = CollaborationManager::native_history_invocation_fixture_for_test(history.save());
    let mut app = App::new_with_controlled_inputs(
        State::new(),
        Box::new(InitialHistoryProbe(
            calls.clone(),
            std::rc::Rc::new(std::cell::Cell::new(false)),
        )),
    );
    app.collaboration.local_author = crate::permissions::UserId("fixture-user".into());
    app.collaboration.manager = fixture.manager;
    let deadline = tilo_test_env::hang_deadline();
    while !app.published_action_history_ready() {
        app.pump_collab_presence();
        assert!(crate::time::now() < deadline);
        std::thread::yield_now();
    }
    let original_actor = app.state.doc.get_actor().clone();
    app.publish_action_surface();
    history
        .put(ROOT, "subsequent-history", "settlement")
        .unwrap();
    history.commit();
    let (release, gate) = tokio::sync::oneshot::channel();
    (fixture.refresh)(history.save(), gate);
    while app
        .state
        .doc
        .get(ROOT, "subsequent-history")
        .unwrap()
        .is_none()
    {
        app.pump_collab_presence();
        assert!(crate::time::now() < deadline);
        std::thread::yield_now();
    }
    assert!(!app.published_action_history_ready());
    (fixture.invoke)(10, uuid::Uuid::from_u128(10));
    while (fixture.received)() == 0 {
        app.pump_collab_presence();
        assert!(crate::time::now() < deadline);
        std::thread::yield_now();
    }
    // The exact stable job10 has crossed the original socket. Its capture
    // remains parked before the worker recheck, with the original owner live.
    let observe_until = crate::time::now() + crate::time::Duration::from_millis(60);
    while crate::time::now() < observe_until {
        app.pump_collab_presence();
        assert!(
            app.collaboration.manager.is_connected(),
            "subsequent capture must preserve original owner custody"
        );
        std::thread::yield_now();
    }
    assert_eq!(calls.get(), 0);
    assert!(
        app.collaboration.manager.is_connected(),
        "a subsequent capture is not lost owner custody"
    );
    assert!(app.action_publication_failure.is_none());
    assert_eq!(app.state.doc.get_actor(), &original_actor);
    release.send(()).unwrap();
    while calls.get() == 0 {
        app.pump_collab_presence();
        assert!(crate::time::now() < deadline);
        std::thread::yield_now();
    }
    assert_eq!(calls.get(), 1);
    assert_eq!((fixture.received)(), 1);
    assert!(app.action_publication_failure.is_none());
    assert!(app.collaboration.manager.is_connected());
    assert_eq!(app.state.doc.get_actor(), &original_actor);
    drop(app);
    fixture.thread.join().unwrap();
}

#[test]
fn initial_history_waited_external_job_rechecks_cancelled_or_refused_original_execution() {
    for refused in [false, true] {
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let mut history = automerge::AutoCommit::new();
        history
            .put(ROOT, "original-owner-history", "retained")
            .unwrap();
        history.commit();
        let fixture =
            CollaborationManager::native_history_invocation_fixture_for_test(history.save());
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(InitialHistoryProbe(
                calls.clone(),
                std::rc::Rc::new(std::cell::Cell::new(false)),
            )),
        );
        app.collaboration.local_author = crate::permissions::UserId("fixture-user".into());
        app.collaboration.manager = fixture.manager;
        let deadline = tilo_test_env::hang_deadline();
        while !app.published_action_history_ready() {
            app.pump_collab_presence();
            assert!(crate::time::now() < deadline);
            std::thread::yield_now();
        }
        history
            .put(ROOT, "subsequent-history", "settlement")
            .unwrap();
        history.commit();
        let (release, gate) = tokio::sync::oneshot::channel();
        (fixture.refresh)(history.save(), gate);
        while app
            .state
            .doc
            .get(ROOT, "subsequent-history")
            .unwrap()
            .is_none()
        {
            app.pump_collab_presence();
            assert!(crate::time::now() < deadline);
            std::thread::yield_now();
        }
        assert!(!app.published_action_history_ready());
        (fixture.invoke)(10, uuid::Uuid::from_u128(10));
        while (fixture.received)() == 0 {
            app.pump_collab_presence();
            assert!(crate::time::now() < deadline);
            std::thread::yield_now();
        }
        (fixture.execution)(if refused {
            Err("original host execution refused".into())
        } else {
            Ok(true)
        });
        release.send(()).unwrap();
        while app.collaboration.manager.is_connected() && (fixture.terminals)() == 0 {
            app.pump_collab_presence();
            assert!(crate::time::now() < deadline);
            std::thread::yield_now();
        }
        assert_eq!(
            calls.get(),
            0,
            "neither cancelled nor refused execution can enter a handler"
        );
        if refused {
            assert!(!app.collaboration.manager.is_connected());
            assert_eq!(
                (fixture.terminals)(),
                0,
                "lost authority cannot fabricate a terminal outcome"
            );
        } else {
            assert!(app.collaboration.manager.is_connected());
            assert_eq!(
                (fixture.terminals)(),
                1,
                "confirmed cancellation crosses the original host terminal door once"
            );
            assert_eq!(
                (fixture.updates)(),
                vec![(
                    10,
                    tilo_actions::InvocationUpdate::Cancelled {
                        invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(10)),
                        message: None,
                    }
                )],
                "the original wire must record the exact cancelled job, not a fabricated outcome"
            );
        }
        drop(app);
        fixture.thread.join().unwrap();
    }
}

#[test]
fn initial_history_job2_does_not_wait_for_unowned_retained_native_route() {
    let calls = std::rc::Rc::new(std::cell::Cell::new(0));
    let mut history = automerge::AutoCommit::new();
    history
        .put(ROOT, "original-owner-history", "retained")
        .unwrap();
    history.commit();
    let fixture = CollaborationManager::native_history_invocation_fixture_for_test(history.save());
    let mut app = App::new_with_controlled_inputs(
        State::new(),
        Box::new(InitialHistoryProbe(
            calls.clone(),
            std::rc::Rc::new(std::cell::Cell::new(false)),
        )),
    );
    app.collaboration.local_author = crate::permissions::UserId("fixture-user".into());
    app.collaboration.manager = fixture.manager;
    let deadline = tilo_test_env::hang_deadline();
    while !app.published_action_history_ready() {
        app.pump_collab_presence();
        assert!(crate::time::now() < deadline);
        std::thread::yield_now();
    }
    let original = app
        .collaboration
        .manager
        .primary_route()
        .unwrap()
        .route
        .clone();
    let unowned = crate::collaboration::RouteSpec::record(
        &original.app_id,
        original.cluster_id,
        "retained-unowned",
    );
    app.collaboration.manager.join_route(unowned.clone());
    while !app
        .committed_documents
        .iter()
        .any(|(route, _)| route == &unowned)
    {
        app.pump_collab_presence();
        assert!(crate::time::now() < deadline);
        std::thread::yield_now();
    }
    assert!(
        app.published_action_history_ready(),
        "the production live owner map is ready"
    );
    assert!(
        !app.collaboration
            .manager
            .document_route_ready(Some(&unowned))
    );
    let id = uuid::Uuid::parse_str("fb32a166-6569-490e-a585-6c7a8207ed06").unwrap();
    (fixture.invoke)(2, id);
    while calls.get() == 0 && app.collaboration.manager.is_connected() {
        app.pump_collab_presence();
        assert!(crate::time::now() < deadline);
        std::thread::yield_now();
    }
    assert!(
        app.collaboration.manager.is_connected(),
        "unowned retained capture cannot close the original live owner"
    );
    assert_eq!(calls.get(), 1);
    assert_eq!((fixture.received)(), 1);
    assert!(app.action_publication_failure.is_none());
    assert!(
        app.committed_documents
            .iter()
            .any(|(route, read)| route == &unowned && read.is_current())
    );
    assert!(
        !app.collaboration
            .manager
            .document_route_ready(Some(&unowned))
    );
    drop(app);
    fixture.thread.join().unwrap();
}

#[test]
fn ordinary_native_pump_exports_admission_before_any_action() {
    const CHILD: &str = "TILO_ADMISSION_CHECKPOINT_CHILD";
    if let Ok(case) = std::env::var(CHILD) {
        let diagnostics = &crate::host::environment().diagnostics;
        assert!(
            !diagnostics.presentation_profile,
            "this is not a synthetic scenario"
        );
        assert_eq!(diagnostics.presentation_timing, case == "enabled");
        tilo_startup_metrics::init();
        tilo_startup_metrics::mark("admission-probe-start");
        let (mut app, calls) = initial_history_app();
        let deadline = tilo_test_env::hang_deadline();
        // The directory RPC is answered on the transport thread; wait until it
        // is both published and answered, or a first publication by the final
        // pump can be answered between its export and the comparison.
        let directory_answered = |app: &App| {
            let (enqueued, counts) = app.collaboration.manager.action_directory_observation();
            enqueued && counts.is_none_or(|(successes, failures)| successes + failures > 0)
        };
        while !app.published_action_history_ready() || !directory_answered(&app) {
            app.pump_collab_presence();
            assert!(
                crate::time::now() < deadline,
                "original socket-backed owner history never applied"
            );
            std::thread::yield_now();
        }
        // Ordinary idle servicing exports the real current admission, without
        // injecting a published invocation or synthetic scenario.
        app.pump_collab_presence();
        assert_eq!(calls.get(), 0);
        let path = std::env::var_os("TILO_STARTUP_METRICS_PATH").unwrap();
        let checkpoint: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(checkpoint["events"][0]["name"], "admission-probe-start");
        if case == "enabled" {
            assert_eq!(
                checkpoint["native_admission"],
                serde_json::json!(app.native_admission_snapshot())
            );
            assert_eq!(checkpoint["native_admission"]["action_history_ready"], true);
        } else {
            assert!(
                checkpoint.get("native_admission").is_none(),
                "default diagnostics remain quiet"
            );
        }
        let previous_events = checkpoint["events"].as_array().unwrap();
        tilo_startup_metrics::mark("admission-probe-done");
        let later: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(later["native_admission"], checkpoint["native_admission"]);
        let current_events = later["events"].as_array().unwrap();
        assert_eq!(current_events.len(), previous_events.len() + 1);
        assert_eq!(
            current_events.get(..previous_events.len()).unwrap(),
            previous_events,
            "every existing startup event must remain unchanged"
        );
        assert_eq!(
            current_events.last().unwrap()["name"],
            "admission-probe-done"
        );
        return;
    }
    for case in ["enabled", "disabled"] {
        let temp = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "app::framework_history_tests::ordinary_native_pump_exports_admission_before_any_action", "--nocapture"])
            .env(CHILD, case)
            .env("TILO_STARTUP_METRICS_PATH", temp.path().join("startup.json"))
            .env("TILO_PROFILE_PRESENTATION", if case == "enabled" { "1" } else { "0" })
            .env_remove("TILO_NATIVE_SCENARIO")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{case}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("running 1 test"),
            "child must execute exactly its production pump case"
        );
        assert!(
            temp.path().join("startup.json").is_file(),
            "child must publish its actual startup checkpoint"
        );
    }
}
