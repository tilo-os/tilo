include!("../../../crates/sdk/ui-core/tests/support/history_adapter_conformance.rs");

use automerge::{ScalarValue, Value};
use ui::widget::Widget;

fn test_root() -> Box<dyn Widget> {
    Box::new(crate::history_test_root())
}

fn fixture() -> (AutoCommit, automerge::ChangeHash, String) {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "ret_daily", 7_i64).unwrap();
    let selected = doc.get_heads()[0];
    doc.put(ROOT, "ret_daily", 31_i64).unwrap();
    doc.put(ROOT, "ret_hourly", 48_i64).unwrap();
    doc.put(ROOT, "view_only", "current").unwrap();
    doc.get_heads();
    (doc, selected, "Daily: 7".into())
}

fn verify_restored(doc: &AutoCommit) {
    assert_eq!(
        crate::doc::BackupsDoc::stored_retention(doc, &ROOT, crate::doc::PolicyField::Daily),
        crate::doc::StoredRetention::Int(7)
    );
    assert_eq!(
        crate::doc::BackupsDoc::stored_retention(doc, &ROOT, crate::doc::PolicyField::Hourly),
        crate::doc::StoredRetention::Absent
    );
}

fn accessible_bounds(driver: &ui::headless::Driver, name: &str) -> [f64; 4] {
    let nodes = driver.a11y_nodes();
    let node = nodes
        .iter()
        .find(|(_, node)| node.label() == Some(name))
        .map_or_else(
            || {
                let labels: Vec<_> = nodes.iter().filter_map(|(_, node)| node.label()).collect();
                panic!("missing accessible target {name:?}; available: {labels:?}")
            },
            |(_, node)| node,
        );
    let bounds = node
        .bounds()
        .unwrap_or_else(|| panic!("accessible target {name:?} has no bounds"));
    assert!(
        bounds.x1 > bounds.x0 && bounds.y1 > bounds.y0,
        "accessible target {name:?} has empty bounds: {bounds:?}"
    );
    [bounds.x0, bounds.y0, bounds.x1, bounds.y1]
}

fn assert_bounds_eq(actual: [f64; 4], expected: [f64; 4]) {
    for (actual, expected) in actual.into_iter().zip(expected) {
        assert!(
            (actual - expected).abs() <= f64::EPSILON,
            "accessible bound changed: expected {expected}, got {actual}"
        );
    }
}

fn selected_accessible_bounds(driver: &ui::headless::Driver, name: &str) -> [f64; 4] {
    let (_, node) = driver
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| node.label() == Some(name))
        .unwrap_or_else(|| panic!("missing accessible target {name:?}"));
    assert_eq!(
        node.is_selected(),
        Some(true),
        "accessible target {name:?} is not selected"
    );
    let bounds = node
        .bounds()
        .unwrap_or_else(|| panic!("accessible target {name:?} has no bounds"));
    assert!(
        bounds.x1 > bounds.x0 && bounds.y1 > bounds.y0,
        "accessible target {name:?} has empty bounds: {bounds:?}"
    );
    [bounds.x0, bounds.y0, bounds.x1, bounds.y1]
}

fn checked_accessible_bounds(driver: &ui::headless::Driver, name: &str) -> [f64; 4] {
    let (_, node) = driver
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| node.label() == Some(name))
        .unwrap_or_else(|| panic!("missing accessible target {name:?}"));
    assert_eq!(
        node.toggled(),
        Some(true.into()),
        "accessible target {name:?} is not checked"
    );
    let bounds = node
        .bounds()
        .unwrap_or_else(|| panic!("accessible target {name:?} has no bounds"));
    assert!(
        bounds.x1 > bounds.x0 && bounds.y1 > bounds.y0,
        "accessible target {name:?} has empty bounds: {bounds:?}"
    );
    [bounds.x0, bounds.y0, bounds.x1, bounds.y1]
}

fn focused_accessible_bounds(driver: &ui::headless::Driver, name: &str) -> [f64; 4] {
    let (_, node) = driver
        .a11y_focus()
        .unwrap_or_else(|| panic!("no exported accessibility focus for {name:?}"));
    assert_eq!(node.label(), Some(name));
    let bounds = node
        .bounds()
        .unwrap_or_else(|| panic!("focused accessible target {name:?} has no bounds"));
    assert!(
        bounds.x1 > bounds.x0 && bounds.y1 > bounds.y0,
        "focused accessible target {name:?} has empty bounds: {bounds:?}"
    );
    [bounds.x0, bounds.y0, bounds.x1, bounds.y1]
}

fn activate_accessible(driver: &mut ui::headless::Driver, name: &str) {
    let target = driver
        .a11y_node_named(name)
        .unwrap_or_else(|| panic!("missing accessible action {name:?}"));
    driver
        .perform_a11y_action(target, &ui::a11y::Action::Activate)
        .unwrap();
}

fn malformed_fixture(doc: &mut AutoCommit) {
    doc.put(ROOT, "ret_weekly", "wrong type").unwrap();
}

#[test]
fn fresh_sync_and_reopen_preserve_absent_roots() {
    let mut state = ui::State::new();
    let heads = state.doc.get_heads();
    test_root().sync(&mut state.doc, &state.root);
    assert_eq!(state.doc.get_heads(), heads);
    assert_eq!(state.doc.pending_ops(), 0);
    let bytes = state.doc.save();
    let mut reopened = AutoCommit::load(&bytes).unwrap();
    let heads = reopened.get_heads();
    test_root().sync(&mut reopened, &ROOT);
    assert_eq!(reopened.get_heads(), heads);
    for field in crate::doc::PolicyField::ALL {
        assert!(reopened.get(ROOT, field.key()).unwrap().is_none());
    }
}

#[test]
fn exact_integer_projection_restores_negative_large_and_absent_values() {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "ret_hourly", -17_i64).unwrap();
    doc.put(ROOT, "ret_daily", 70_000_i64).unwrap();
    let selected = doc.get_heads();
    doc.put(ROOT, "ret_hourly", 2_i64).unwrap();
    doc.put(ROOT, "ret_daily", 3_i64).unwrap();
    doc.put(ROOT, "ret_weekly", 4_i64).unwrap();
    doc.put(ROOT, "unrelated", "keep me").unwrap();
    let current = doc.get_heads();
    let prepared = prepare_content_restore(
        &doc,
        crate::framework_history::HISTORY.scopes[0]
            .content_schema
            .unwrap(),
        &current,
        &selected,
    )
    .unwrap();
    apply_prepared_content_restore(&mut doc, &prepared).unwrap();
    assert_eq!(
        doc.get(ROOT, "ret_hourly").unwrap().unwrap().0.to_i64(),
        Some(-17)
    );
    assert_eq!(
        doc.get(ROOT, "ret_daily").unwrap().unwrap().0.to_i64(),
        Some(70_000)
    );
    assert!(doc.get(ROOT, "ret_weekly").unwrap().is_none());
    assert_eq!(
        doc.get(ROOT, "unrelated").unwrap().unwrap().0.to_str(),
        Some("keep me")
    );
}

#[test]
fn preview_reports_exact_values_defaults_and_device_local_catalogue_limit() {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "ret_hourly", -17_i64).unwrap();
    doc.put(ROOT, "ret_daily", 70_000_i64).unwrap();
    let selected = doc.get_heads()[0];
    let mut preview = preview(&doc, selected, ui::history_mode::PreviewSide::After);
    let description = preview.accessibility().description;
    assert!(description.contains("Hourly: -17"));
    assert!(description.contains("Daily: 70000"));
    assert!(description.contains("Weekly: 4 (default, not recorded)"));
    assert!(description.contains("Snapshot catalogue is device-local and unavailable in History."));
    assert_eq!(
        preview.accessibility().pages,
        vec![("retention".into(), "Retention policy".into())]
    );
    for width in [360.0, 1000.0] {
        preview
            .layout(
                vello::kurbo::Rect::new(0.0, 0.0, width, 500.0),
                &mut ui::TextRenderer::new(),
                &ui::Theme::dark(),
            )
            .unwrap();
        preview.draw(
            &mut vello::Scene::new(),
            vello::kurbo::Affine::IDENTITY,
            &mut ui::TextRenderer::new(),
            &ui::Theme::dark(),
        );
    }
    assert!(preview.navigate(ui::history_mode::PreviewNavigation::Scroll { dx: 0.0, dy: -80.0 }));
    assert!(
        preview.navigate(ui::history_mode::PreviewNavigation::SelectPage(
            "retention".into()
        ))
    );
    assert!(
        !preview.navigate(ui::history_mode::PreviewNavigation::SelectPage(
            "catalogue".into()
        ))
    );
}

#[test]
fn exact_reader_distinguishes_wrong_type_and_true_conflict() {
    let mut wrong = AutoCommit::new();
    wrong.put(ROOT, "ret_monthly", "12").unwrap();
    assert_eq!(
        crate::doc::BackupsDoc::stored_retention(&wrong, &ROOT, crate::doc::PolicyField::Monthly),
        crate::doc::StoredRetention::WrongType
    );

    let mut left = AutoCommit::new();
    left.put(ROOT, "ret_monthly", 2_i64).unwrap();
    let bytes = left.save();
    let mut right = AutoCommit::load(&bytes).unwrap();
    left.put(ROOT, "ret_monthly", 3_i64).unwrap();
    right.put(ROOT, "ret_monthly", 4_i64).unwrap();
    left.merge(&mut right).unwrap();
    assert_eq!(
        crate::doc::BackupsDoc::stored_retention(&left, &ROOT, crate::doc::PolicyField::Monthly),
        crate::doc::StoredRetention::Conflict
    );
    assert!(matches!(
        left.get_all(ROOT, "ret_monthly").unwrap().as_slice(),
        [
            (Value::Scalar(a), _),
            (Value::Scalar(b), _)
        ] if matches!(a.as_ref(), ScalarValue::Int(_)) && matches!(b.as_ref(), ScalarValue::Int(_))
    ));
}

#[test]
fn restore_refuses_wrong_type_and_current_conflict_without_writes() {
    let schema = crate::framework_history::HISTORY.scopes[0]
        .content_schema
        .unwrap();

    let mut wrong = AutoCommit::new();
    wrong.put(ROOT, "ret_daily", "seven").unwrap();
    let selected_wrong = wrong.get_heads();
    wrong.put(ROOT, "ret_daily", 9_i64).unwrap();
    let heads = wrong.get_heads();
    assert!(prepare_content_restore(&wrong, schema, &heads, &selected_wrong).is_err());
    assert_eq!(wrong.get_heads(), heads);
    assert_eq!(wrong.pending_ops(), 0);

    let mut left = AutoCommit::new();
    left.put(ROOT, "ret_daily", 7_i64).unwrap();
    let target = left.get_heads();
    let bytes = left.save();
    let mut right = AutoCommit::load(&bytes).unwrap();
    left.put(ROOT, "ret_daily", 8_i64).unwrap();
    right.put(ROOT, "ret_daily", 9_i64).unwrap();
    left.merge(&mut right).unwrap();
    let heads = left.get_heads();
    assert!(prepare_content_restore(&left, schema, &heads, &target).is_err());
    assert_eq!(left.get_heads(), heads);
    assert_eq!(left.pending_ops(), 0);
}

fn invoke_published_operation(
    app: &mut crate::BackupsApp,
    doc: &AutoCommit,
    operation: &crate::actions::BackupOperation,
) {
    let resources = Widget::resources(app, doc);
    let encoded = tilo_actions::flatten(&resources)
        .into_iter()
        .filter_map(|entry| match &entry.action.input {
            tilo_actions::ActionInput::Immediate(encoded) => Some(encoded),
            tilo_actions::ActionInput::Prompted { .. }
            | tilo_actions::ActionInput::Deferred { .. } => None,
        })
        .find(|encoded| {
            crate::actions::decode_operation(&encoded.type_id, &encoded.payload)
                .ok()
                .flatten()
                .as_ref()
                == Some(operation)
        })
        .expect("published operation payload")
        .clone();
    assert!(
        Widget::invoke_resource_operation(
            app,
            tilo_actions::OperationKind::Effect,
            &encoded.type_id,
            &encoded.payload,
            doc,
            &ROOT,
        )
        .unwrap()
    );
}

fn history_job_driver(app: crate::BackupsApp, doc: AutoCommit, tab: u64) -> ui::headless::Driver {
    let definition = &crate::framework_history::HISTORY;
    let registration = HistoryRegistration::new(
        definition,
        vec![HistorySourceRegistration::HostDocument {
            scope: definition.scopes[0].key,
            resource: HistoryResource::new(
                "temporary",
                "backups-job",
                definition.app_id,
                ui::RecordId::for_tab(tab),
            ),
        }],
    )
    .unwrap();
    ui::headless::Driver::from_managed_root(
        ui::State::from_doc(doc),
        ManagedRoot::new(Box::new(app), registration),
        (1000, 750),
    )
    .unwrap()
}

fn assert_same_live_job(
    observed: &std::rc::Rc<std::cell::RefCell<crate::app::HistoryJobSnapshot>>,
    receiver: &std::rc::Rc<()>,
    status: &str,
) {
    let snapshot = observed.borrow();
    let current = snapshot
        .receiver_identity
        .as_ref()
        .expect("accepted job retains its receiver identity");
    assert!(std::rc::Rc::ptr_eq(current, receiver));
    assert_eq!(snapshot.status.as_deref(), Some(status));
    assert_eq!(snapshot.terminal, None);
    assert_eq!(snapshot.terminal_toast_identity, None);
}

#[test]
fn create_job_reduces_progress_and_one_failure_through_history() {
    let (doc, selected, _) = fixture();
    let mut app = crate::history_test_job_root();
    let observed = app.observe_history_job();
    let (requests, captured) = std::sync::mpsc::channel();
    app.set_controlled_jobs(requests);
    invoke_published_operation(&mut app, &doc, &crate::actions::BackupOperation::BackupNow);
    let request = captured.try_recv().unwrap();
    assert_eq!(
        request.request,
        crate::app::ControlledJobRequest::Create {
            source: "/controlled/source".into(),
            destination: "/controlled/destination".into(),
        }
    );

    let mut driver = history_job_driver(app, doc, 77);
    let receiver = observed
        .borrow()
        .receiver_identity
        .clone()
        .expect("admitted create receiver");
    assert_same_live_job(&observed, &receiver, "Preparing files…");

    driver.click_named("history-open").unwrap();
    select_version(&mut driver, selected);
    driver.click_named("history-before").unwrap();
    driver.click_named("history-after").unwrap();
    request.progress(11, 8192).unwrap();
    driver.relayout();
    assert_same_live_job(&observed, &receiver, "11 files · 8.0 KiB");
    driver.click_named("history-restore").unwrap();
    driver.key("Enter");
    verify_restored(&driver.state_mut().doc);
    driver.click_named("history-back-live").unwrap();
    assert_same_live_job(&observed, &receiver, "11 files · 8.0 KiB");
    assert!(matches!(
        captured.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    request
        .fail("controlled create failure after restore")
        .unwrap();
    driver.relayout();
    let settled = observed.borrow().clone();
    assert!(settled.receiver_identity.is_none());
    assert_eq!(settled.status, None);
    assert_eq!(
        settled.terminal,
        Some(crate::app::HistoryJobTerminal::Error(
            "controlled create failure after restore".into()
        ))
    );
    assert!(settled.terminal_toast_identity.is_some());
    assert!(request.progress(12, 9000).is_err());
    verify_restored(&driver.state_mut().doc);
    driver.relayout();
    assert_eq!(*observed.borrow(), settled);
    assert!(matches!(
        captured.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
}

#[test]
fn verify_job_reduces_progress_and_one_success_through_history() {
    let (doc, selected, _) = fixture();
    let mut app = crate::history_test_job_root();
    let observed = app.observe_history_job();
    let (requests, captured) = std::sync::mpsc::channel();
    app.set_controlled_jobs(requests);
    let snapshot_id = "w-0-1700".to_string();
    invoke_published_operation(
        &mut app,
        &doc,
        &crate::actions::BackupOperation::Verify {
            snapshot_id: snapshot_id.clone(),
        },
    );
    let request = captured.try_recv().unwrap();
    assert_eq!(
        request.request,
        crate::app::ControlledJobRequest::Verify {
            destination: "/controlled/destination".into(),
            snapshot_id,
        }
    );

    let mut driver = history_job_driver(app, doc, 79);
    let receiver = observed
        .borrow()
        .receiver_identity
        .clone()
        .expect("admitted verify receiver");
    assert_same_live_job(&observed, &receiver, "Checking every file…");

    driver.click_named("history-open").unwrap();
    select_version(&mut driver, selected);
    driver.click_named("history-before").unwrap();
    driver.click_named("history-after").unwrap();
    request.progress(8, 4096).unwrap();
    driver.relayout();
    assert_same_live_job(&observed, &receiver, "8 files · 4.0 KiB");
    driver.click_named("history-restore").unwrap();
    driver.key("Enter");
    verify_restored(&driver.state_mut().doc);
    driver.click_named("history-back-live").unwrap();
    assert_same_live_job(&observed, &receiver, "8 files · 4.0 KiB");
    assert!(matches!(
        captured.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));

    request.verified(8, 4096).unwrap();
    driver.relayout();
    let settled = observed.borrow().clone();
    assert!(settled.receiver_identity.is_none());
    assert_eq!(settled.status, None);
    assert_eq!(
        settled.terminal,
        Some(crate::app::HistoryJobTerminal::Success(
            "Verified 8 files · 4.0 KiB".into()
        ))
    );
    assert!(settled.terminal_toast_identity.is_some());
    assert!(request.progress(9, 5000).is_err());
    driver.relayout();
    assert_eq!(*observed.borrow(), settled);
    assert!(matches!(
        captured.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
}

#[test]
fn managed_history_retains_public_retention_field_focus() {
    let (doc, selected, _) = fixture();
    let definition = &crate::framework_history::HISTORY;
    let registration = HistoryRegistration::new(
        definition,
        vec![HistorySourceRegistration::HostDocument {
            scope: definition.scopes[0].key,
            resource: HistoryResource::new(
                "temporary",
                "backups-focus",
                definition.app_id,
                ui::RecordId::for_tab(80),
            ),
        }],
    )
    .unwrap();
    let mut driver = ui::headless::Driver::from_managed_root(
        ui::State::from_doc(doc),
        ManagedRoot::new(Box::new(crate::app::BackupsApp::new_demo()), registration),
        (1000, 750),
    )
    .unwrap();

    activate_accessible(&mut driver, "Schedules view");
    let heads = driver.state_mut().doc.get_heads();
    let pending = driver.state_mut().doc.pending_ops();
    let weekly = driver
        .a11y_node_named("Weekly retention field")
        .expect("normal Schedules view exports the retention field focus target");
    driver
        .perform_a11y_action(weekly, &ui::a11y::Action::Focus)
        .unwrap();
    driver.relayout();
    let focus_bounds = focused_accessible_bounds(&driver, "Weekly retention field");
    assert_eq!(driver.state_mut().doc.get_heads(), heads);
    assert_eq!(driver.state_mut().doc.pending_ops(), pending);

    driver.click_named("history-open").unwrap();
    select_version(&mut driver, selected);
    driver.click_named("history-before").unwrap();
    driver.click_named("history-after").unwrap();
    driver.click_named("history-restore").unwrap();
    driver.key("Enter");
    driver.click_named("history-back-live").unwrap();

    assert_bounds_eq(
        focused_accessible_bounds(&driver, "Weekly retention field"),
        focus_bounds,
    );
    verify_restored(&driver.state_mut().doc);
}

#[test]
fn managed_history_retains_device_view_state_and_catalogue_across_restore() {
    let (doc, selected, _) = fixture();
    let (app, observed) = crate::history_test_staged_root();
    let definition = &crate::framework_history::HISTORY;
    let registration = HistoryRegistration::new(
        definition,
        vec![HistorySourceRegistration::HostDocument {
            scope: definition.scopes[0].key,
            resource: HistoryResource::new(
                "temporary",
                "backups-view",
                definition.app_id,
                ui::RecordId::for_tab(78),
            ),
        }],
    )
    .unwrap();
    let mut driver = ui::headless::Driver::from_managed_root(
        ui::State::from_doc(doc),
        ManagedRoot::new(Box::new(app), registration),
        (1000, 750),
    )
    .unwrap();
    let before = observed.borrow().clone();

    let initial_modal = accessible_bounds(&driver, "Choose an empty folder for recovery");
    activate_accessible(&mut driver, "Cancel recovery");
    let source = selected_accessible_bounds(&driver, "Select source home");
    let destination = selected_accessible_bounds(&driver, "Select destination pool");
    let snapshot = selected_accessible_bounds(&driver, "Open snapshot m-0-1422");
    let catalogue = accessible_bounds(&driver, "Open snapshot h-0-0300");
    let drawer = checked_accessible_bounds(&driver, "Show backup sources");

    activate_accessible(&mut driver, "Recover complete snapshot");
    assert_bounds_eq(
        accessible_bounds(&driver, "Choose an empty folder for recovery"),
        initial_modal,
    );

    driver.click_named("history-open").unwrap();
    select_version(&mut driver, selected);
    driver.click_named("history-before").unwrap();
    driver.click_named("history-after").unwrap();
    driver.click_named("history-restore").unwrap();
    driver.key("Enter");
    driver.click_named("history-back-live").unwrap();

    assert_bounds_eq(
        accessible_bounds(&driver, "Choose an empty folder for recovery"),
        initial_modal,
    );
    activate_accessible(&mut driver, "Cancel recovery");
    assert_bounds_eq(
        selected_accessible_bounds(&driver, "Select source home"),
        source,
    );
    assert_bounds_eq(
        selected_accessible_bounds(&driver, "Select destination pool"),
        destination,
    );
    assert_bounds_eq(
        selected_accessible_bounds(&driver, "Open snapshot m-0-1422"),
        snapshot,
    );
    assert_bounds_eq(
        accessible_bounds(&driver, "Open snapshot h-0-0300"),
        catalogue,
    );
    assert_bounds_eq(
        checked_accessible_bounds(&driver, "Show backup sources"),
        drawer,
    );
    let after = observed.borrow().clone();
    assert!((after.timeline_scroll - before.timeline_scroll).abs() <= f64::EPSILON);
    assert!((after.rail_scroll - before.rail_scroll).abs() <= f64::EPSILON);
    assert_eq!(after.source_path, before.source_path);
    assert_eq!(after.destination_path, before.destination_path);
    verify_restored(&driver.state_mut().doc);
}
