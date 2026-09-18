use automerge::transaction::Transactable;
use std::cell::RefCell;
use std::rc::Rc;
use tilo_actions::OperationKind;
use ui::history_mode::{
    HistoryPresentation, HistoryRegistration, HistorySourceRegistration, ManagedRoot,
};
use ui::widget::Widget;

fn offline_root() -> Box<dyn Widget> {
    let mut app = crate::LogsApp::new(crate::journal::sample_entries(), false);
    app.shots_filter_errors();
    app.shots_show_detail();
    Box::new(app)
}

fn managed_driver() -> ui::headless::Driver {
    managed_driver_for(offline_root())
}

fn managed_driver_for(root: Box<dyn Widget>) -> ui::headless::Driver {
    let definition = root
        .document_history_definition()
        .expect("Logs root must declare framework History");
    let scope = &definition.scopes[0];
    let HistoryPresentation::External { reason } = scope.presentation else {
        panic!("journal observations must be explicitly unversioned");
    };
    let registration = HistoryRegistration::new(
        definition,
        vec![HistorySourceRegistration::Unversioned {
            scope: scope.key,
            reason,
        }],
    )
    .unwrap();
    ui::headless::Driver::from_managed_root(
        ui::State::new(),
        ManagedRoot::new(root, registration),
        (390, 750),
    )
    .unwrap()
}

fn resource_snapshot(driver: &ui::headless::Driver) -> Vec<(String, String, Vec<String>)> {
    fn collect(
        resource: &ui::tilo_actions::Resource,
        out: &mut Vec<(String, String, Vec<String>)>,
    ) {
        out.push((
            resource.id.0.clone(),
            resource.label.clone(),
            resource
                .actions
                .iter()
                .map(|action| action.id.clone())
                .collect(),
        ));
        for child in &resource.children {
            collect(child, out);
        }
    }
    let mut snapshot = Vec::new();
    for resource in driver.resources() {
        collect(&resource, &mut snapshot);
    }
    snapshot
}

#[test]
fn production_root_type_declares_logs_history_without_querying_journal() {
    let definition = offline_root()
        .document_history_definition()
        .expect("production Logs root type declares History");
    assert_eq!(definition.app_id, "dev.tilo.logs");
    assert!(std::ptr::eq(
        definition,
        std::ptr::addr_of!(crate::framework_history::HISTORY)
    ));
}

#[test]
fn managed_history_is_truthful_inert_and_returns_to_the_same_logs_view() {
    let mut driver = managed_driver();
    let live = driver.inspection_json().unwrap();
    let resources = resource_snapshot(&driver);
    let heads = driver.state_mut().doc.get_heads();
    let pending = driver.state_mut().doc.pending_ops();
    assert!(
        live.contains("Back to entries"),
        "offline detail selection is visible"
    );
    assert!(live.contains("Error"), "offline error query is applied");
    assert!(
        live.contains("Failed to set privacy: Rejected"),
        "offline filtered journal row is visible"
    );

    driver.click_named("history-open").unwrap();
    let history = driver.inspection_json().unwrap();
    assert!(history.contains(crate::framework_history::LIMITATION));
    assert!(driver.targets().contains_key("history-open"));
    assert!(!driver.targets().contains_key("activity_timeline"));
    assert!(!driver.targets().contains_key("history-restore"));
    assert!(!driver.targets().contains_key("history-before"));
    assert!(!driver.targets().contains_key("history-after"));
    assert!(
        !history.contains("Failed to set privacy: Rejected"),
        "current journal rows stay out of History"
    );
    assert_eq!(driver.state_mut().doc.get_heads(), heads);
    assert_eq!(driver.state_mut().doc.pending_ops(), pending);

    driver.click_named("history-open").unwrap();
    let returned = driver.inspection_json().unwrap();
    assert!(returned.contains("Back to entries"));
    assert!(returned.contains("Error"));
    assert!(returned.contains("Failed to set privacy: Rejected"));
    assert_eq!(resource_snapshot(&driver), resources);
    assert_eq!(driver.state_mut().doc.get_heads(), heads);
    assert_eq!(driver.state_mut().doc.pending_ops(), pending);
    assert_ne!(returned, history);
}

fn invoke(app: &mut crate::LogsApp, operation: &crate::actions::LogsOperation) {
    let payload = serde_json::to_vec(&operation).unwrap();
    assert!(
        Widget::invoke_resource_operation(
            app,
            OperationKind::Effect,
            crate::actions::LOGS_OPERATION_TYPE,
            &payload,
            &automerge::AutoCommit::new(),
            &automerge::ROOT,
        )
        .unwrap()
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one causal export lifecycle is kept visible from request through single settlement"
)]
fn held_export_snapshot_survives_history_and_settles_once_for_the_exact_selection() {
    let entries = crate::journal::sample_entries();
    let selected = entries
        .iter()
        .find(|entry| entry.message.contains("Failed to set privacy: Rejected"))
        .unwrap()
        .clone();
    let expected = entries
        .iter()
        .filter(|entry| entry.priority.level() <= crate::Priority::Err.level())
        .cloned()
        .collect::<Vec<_>>();
    let held_sender = Rc::new(RefCell::new(None));
    let requests = Rc::new(RefCell::new(Vec::new()));
    let mut app = crate::LogsApp::new(entries, true);
    let sender_slot = held_sender.clone();
    let request_log = requests.clone();
    app.set_controlled_export_portal(move |request| {
        request_log.borrow_mut().push((
            request.suggested_name().map(str::to_owned),
            request
                .filter_names()
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>(),
        ));
        let (sender, receiver) = std::sync::mpsc::channel();
        *sender_slot.borrow_mut() = Some(sender);
        receiver
    });
    invoke(
        &mut app,
        &crate::actions::LogsOperation::SetPriority {
            value: crate::Priority::Err.level(),
        },
    );
    invoke(
        &mut app,
        &crate::actions::LogsOperation::SelectEntry {
            entry_id: crate::actions::entry_resource_id(&selected),
        },
    );
    app.shots_show_detail();
    invoke(&mut app, &crate::actions::LogsOperation::ExportFiltered);
    invoke(&mut app, &crate::actions::LogsOperation::ClearFilters);

    assert_eq!(
        requests.borrow().as_slice(),
        &[(
            Some("journal-filtered.jsonl".to_string()),
            vec!["JSON Lines".to_string()]
        )],
        "the real export operation issued one configured Save request"
    );
    let path = std::env::temp_dir().join(format!(
        "qos-logs-history-held-export-{}-{}.jsonl",
        std::process::id(),
        selected.realtime_us
    ));
    let _ = std::fs::remove_file(&path);
    let mut driver = managed_driver_for(Box::new(app));
    let live = driver.inspection_json().unwrap();
    assert!(
        live.contains(&selected.message),
        "exact selected row is live"
    );

    driver.click_named("history-open").unwrap();
    assert!(
        held_sender.borrow().is_some(),
        "portal request remains pending"
    );
    driver.click_named("history-open").unwrap();
    let returned = driver.inspection_json().unwrap();
    assert!(
        returned.contains(&selected.message),
        "the same selected journal row returns after History"
    );

    held_sender
        .borrow_mut()
        .take()
        .unwrap()
        .send(ui::portal::PortalOutcome::Opened(path.clone()))
        .unwrap();
    driver.relayout();
    let written = std::fs::read_to_string(&path).unwrap();
    let lines = written.lines().collect::<Vec<_>>();
    assert_eq!(
        lines.len(),
        expected.len(),
        "the frozen filtered snapshot writes"
    );
    for entry in &expected {
        assert!(
            lines.iter().any(|line| line.contains(&entry.message)),
            "snapshot contains {}",
            entry.message
        );
    }
    assert!(
        driver
            .inspection_json()
            .unwrap()
            .contains(&format!("Exported {} log entries.", expected.len()))
    );

    std::fs::write(&path, "settled-once\n").unwrap();
    driver.relayout();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "settled-once\n",
        "a drained portal outcome cannot write twice"
    );
    assert_eq!(requests.borrow().len(), 1, "the request is not replayed");
    std::fs::remove_file(path).unwrap();
}

#[test]
fn logs_external_policy_has_no_preview_or_restore_schema() {
    let scope = &crate::framework_history::HISTORY.scopes[0];
    assert_eq!(scope.key, "journal-observations");
    assert!(scope.content_schema.is_none());
    assert!(scope.presentation.preview_factory().is_none());
    let HistoryPresentation::External { reason } = scope.presentation else {
        panic!("external presentation");
    };
    assert_eq!(reason, crate::framework_history::LIMITATION);
}
