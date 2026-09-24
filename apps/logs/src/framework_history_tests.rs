use automerge::transaction::Transactable;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
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
    assert_eq!(definition.app_id, "so.tilo.logs");
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

/// A host chooser that records every Save request and holds its answer until
/// the test settles it, so the request stays pending across History.
#[derive(Default)]
struct HeldSavePicker {
    requests: Mutex<Vec<ui::portal::PickRequest>>,
    held: Mutex<Option<Sender<ui::portal::PortalOutcome>>>,
}

impl HeldSavePicker {
    fn requests(&self) -> Vec<ui::portal::PickRequest> {
        self.requests.lock().unwrap().clone()
    }

    fn is_pending(&self) -> bool {
        self.held.lock().unwrap().is_some()
    }

    fn settle(&self, outcome: ui::portal::PortalOutcome) {
        let sender = self.held.lock().unwrap().take();
        sender.unwrap().send(outcome).unwrap();
    }
}

/// A channel whose sender is already gone: the request disconnects at once.
fn disconnected<T>() -> Receiver<T> {
    std::sync::mpsc::channel().1
}

impl ui::host::FilePicker for HeldSavePicker {
    fn pick(&self, request: ui::portal::PickRequest) -> ui::portal::PortalChannel {
        self.requests.lock().unwrap().push(request);
        let (sender, receiver) = std::sync::mpsc::channel();
        *self.held.lock().unwrap() = Some(sender);
        receiver
    }

    fn pick_multiple(&self, _request: ui::portal::PickRequest) -> ui::portal::MultiChannel {
        disconnected()
    }

    fn open_file(&self, _request: ui::portal::PickRequest) -> ui::portal::OpenFileChannel {
        disconnected()
    }

    fn launch(&self, _request: ui::portal::LaunchRequest) -> ui::portal::LaunchChannel {
        disconnected()
    }
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
    let picker = Arc::new(HeldSavePicker::default());
    let (mut services, _recorder) = ui::host::HostServices::recording();
    services.files = picker.clone();
    let _host = ui::host::enter(&services);
    let mut app = crate::LogsApp::new(entries, true);
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

    let requests = picker
        .requests()
        .iter()
        .map(|request| {
            (
                request.suggested_name().map(str::to_owned),
                request
                    .filter_names()
                    .into_iter()
                    .map(str::to_owned)
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        requests.as_slice(),
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
    assert!(picker.is_pending(), "portal request remains pending");
    driver.click_named("history-open").unwrap();
    let returned = driver.inspection_json().unwrap();
    assert!(
        returned.contains(&selected.message),
        "the same selected journal row returns after History"
    );

    picker.settle(ui::portal::PortalOutcome::Opened(path.clone()));
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
    assert_eq!(picker.requests().len(), 1, "the request is not replayed");
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
