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
    let catalog = ui::font_catalog::FontCatalog::controlled_fixture(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui/assets/fonts"),
        "en-US",
    );
    let renderer = ui::TextRenderer::with_font_catalog(&catalog).unwrap();
    ui::headless::Driver::from_managed_root_with_controlled_inputs(
        ui::State::new(),
        ManagedRoot::new(root, registration),
        (390, 750),
        ui::Theme::dark(),
        renderer,
        ui::permissions::UserId("logs-custody-author".into()),
    )
    .unwrap()
}

struct CustodyDeniedLogsRoot {
    child: Box<dyn Widget>,
    denied: Rc<std::cell::Cell<bool>>,
}

impl Widget for CustodyDeniedLogsRoot {
    fn name(&self) -> &'static str {
        "CustodyDeniedLogsRoot"
    }
    fn document_history_definition(&self) -> Option<&'static ui::history_mode::HistoryDefinition> {
        self.child.document_history_definition()
    }
    fn sync(&mut self, doc: &mut automerge::AutoCommit, path: &automerge::ObjId) {
        self.child.sync(doc, path);
    }
    fn build_layout(
        &mut self,
        tree: &mut taffy::TaffyTree<()>,
        theme: &ui::Theme,
        nodes: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        self.child.build_layout(tree, theme, nodes)
    }
    fn apply_layout(
        &mut self,
        tree: &taffy::TaffyTree<()>,
        node: taffy::NodeId,
        origin: vello::kurbo::Point,
    ) {
        self.child.apply_layout(tree, node, origin);
    }
    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text: &mut ui::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        self.child.draw(scene, transform, text, theme, presence);
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        self.child.get_rect()
    }
    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        scale: f64,
        modifiers: &winit::event::Modifiers,
        doc: &mut automerge::AutoCommit,
        path: &automerge::ObjId,
        handled: &mut bool,
        focus: &mut Option<taffy::NodeId>,
        node: taffy::NodeId,
        tree: &taffy::TaffyTree<()>,
    ) {
        self.child.handle_event(
            event, scale, modifiers, doc, path, handled, focus, node, tree,
        );
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.child.as_ref()]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if self.denied.get() {
            Vec::new()
        } else {
            vec![self.child.as_mut()]
        }
    }
    fn take_control_invocation(&mut self) -> Option<ui::tilo_actions::ControlInvocation> {
        if self.denied.get() {
            None
        } else {
            self.child.take_control_invocation()
        }
    }
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

fn selected_log_entry_target(inspection: &str) -> String {
    fn find(value: &serde_json::Value) -> Option<String> {
        if value
            .get("action_target")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|target| target.starts_with("logs.entry:"))
            && value
                .get("state")
                .and_then(|state| state.get("selected"))
                .and_then(serde_json::Value::as_bool)
                == Some(true)
        {
            return value
                .get("action_target")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
        }
        match value {
            serde_json::Value::Array(values) => values.iter().find_map(find),
            serde_json::Value::Object(values) => values.values().find_map(find),
            _ => None,
        }
    }

    find(&serde_json::from_str(inspection).unwrap()).expect("selected journal entry target")
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

#[test]
fn control_custody_logs_unreachable_private_route_seals_e_suppresses_h_and_runs_f_once() {
    let (app, attempts, arm, exports) = crate::LogsApp::history_test_controlled_root();
    let denied = Rc::new(std::cell::Cell::new(false));
    let root = CustodyDeniedLogsRoot {
        child: Box::new(app),
        denied: denied.clone(),
    };
    let mut driver = managed_driver_for(Box::new(root));

    arm.set(Some("refresh"));
    driver.relayout();
    denied.set(true);
    driver.click_named("history-open").unwrap();
    let sealed = driver.pending_history_controls();
    assert_eq!(
        sealed.len(),
        1,
        "mounted custody seals Refresh E without traversal"
    );
    assert_eq!(
        sealed[0].invocation.address.resource,
        crate::actions::JOURNAL_RESOURCE_ID
    );
    assert_eq!(sealed[0].invocation.address.action, "refresh");
    assert!(sealed[0].invocation.arguments.is_empty());

    arm.set(Some("set-live-tail"));
    driver.relayout();
    driver.click_named("history-open").unwrap();
    assert!(
        denied.get(),
        "private traversal stays unavailable through return"
    );
    denied.set(false);
    driver.send(&winit::event::WindowEvent::Focused(true));
    assert!(attempts.borrow().is_empty(), "hidden live-tail never polls");
    assert_eq!(
        driver.pending_history_controls().len(),
        1,
        "H is neither saved nor live"
    );

    let ready = driver.pending_history_controls();
    assert_eq!(ready.len(), 1, "captured E remains exact and inert");
    assert!(attempts.borrow().is_empty(), "capture does not admit E");
    let token = driver
        .pending_history_control_token(ready[0].id)
        .expect("current exact continuation token");
    let refusal = "stale_pending_control: unsupported_continuation_identity: The action provider cannot prove the original target and handler identity";
    assert_eq!(
        driver.continue_pending_history_control(&token),
        Some(ui::history_mode::pending_controls::PendingControlStatus::Refused(refusal.into(),)),
        "Logs retains the exact sealed item while reporting its missing continuation identity",
    );
    let refused = driver.pending_history_controls();
    assert_eq!(refused.len(), 1, "refusal preserves sealed E for review");
    assert_eq!(
        refused[0].status,
        ui::history_mode::pending_controls::PendingControlStatus::Refused(refusal.into()),
    );
    assert_eq!(refused[0].invocation, sealed[0].invocation);
    assert!(attempts.borrow().is_empty(), "refused E remains inert");

    driver
        .perform_semantic_named(
            "logs_app",
            Some("logs.refresh"),
            &ui::a11y::Action::Activate,
        )
        .unwrap();
    assert_eq!(
        attempts.borrow().as_slice(),
        &[2000],
        "fresh semantic F performs one controlled journal load"
    );
    assert!(
        exports.borrow().is_empty(),
        "custody flow never opens export portal"
    );
    assert_eq!(driver.control_coverage().registered, 1);
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
    let default = entries
        .iter()
        .find(|entry| entry.priority.level() <= crate::Priority::Err.level())
        .unwrap();
    let selected = entries
        .iter()
        .find(|entry| entry.message.contains("renderer stalled 2 frames"))
        .unwrap()
        .clone();
    let selected_id = crate::actions::entry_resource_id(&selected);
    assert_ne!(
        selected_id,
        crate::actions::entry_resource_id(default),
        "fixture target must differ from Logs' automatic first-error selection"
    );
    let selected_target = format!("logs.entry:{selected_id}");
    let expected = entries
        .iter()
        .filter(|entry| entry.priority.level() <= crate::Priority::Err.level())
        .cloned()
        .collect::<Vec<_>>();
    let held_sender = Rc::new(RefCell::new(None));
    let requests = Rc::new(RefCell::new(Vec::new()));
    let mut app = crate::LogsApp::history_test_with_entries(entries, true);
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
            entry_id: selected_id,
        },
    );
    app.shots_show_detail();
    invoke(&mut app, &crate::actions::LogsOperation::ExportFiltered);
    invoke(
        &mut app,
        &crate::actions::LogsOperation::SetUnit {
            value: "shell.service".to_string(),
        },
    );

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
    assert_eq!(selected_log_entry_target(&live), selected_target);

    driver.click_named("history-open").unwrap();
    assert!(
        held_sender.borrow().is_some(),
        "portal request remains pending"
    );
    driver.click_named("history-open").unwrap();
    let returned = driver.inspection_json().unwrap();
    assert_eq!(selected_log_entry_target(&returned), selected_target);

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
    assert_eq!(
        selected_log_entry_target(&driver.inspection_json().unwrap()),
        selected_target,
        "portal settlement preserves the same explicit selected resource"
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
