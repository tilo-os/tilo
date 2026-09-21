use automerge::{AutoCommit, transaction::Transactable};
use ui::history_mode::{
    HistoryPresentation, HistoryRegistration, HistorySourceRegistration, ManagedRoot,
};
use ui::widget::Widget;

fn offline_root() -> Box<dyn Widget> {
    Box::new(crate::MonitorApp::new())
}

fn managed_driver() -> ui::headless::Driver {
    managed_driver_for(offline_root())
}

fn managed_driver_for(root: Box<dyn Widget>) -> ui::headless::Driver {
    let definition = root
        .document_history_definition()
        .expect("Monitor root must declare framework History");
    let scope = &definition.scopes[0];
    let HistoryPresentation::External { reason } = scope.presentation else {
        panic!("Monitor observations must be explicitly unversioned");
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
        (1_000, 750),
        ui::Theme::dark(),
        renderer,
        ui::permissions::UserId("monitor-custody-author".into()),
    )
    .unwrap()
}

struct CustodyDeniedMonitorRoot {
    child: Box<dyn Widget>,
    denied: std::rc::Rc<std::cell::Cell<bool>>,
}

impl Widget for CustodyDeniedMonitorRoot {
    fn name(&self) -> &'static str {
        "CustodyDeniedMonitorRoot"
    }

    fn document_history_definition(&self) -> Option<&'static ui::history_mode::HistoryDefinition> {
        self.child.document_history_definition()
    }

    fn sync(&mut self, doc: &mut AutoCommit, path: &automerge::ObjId) {
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
        doc: &mut AutoCommit,
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

fn semantic_fragment<'a>(inspection: &'a str, name: &str) -> &'a str {
    let marker = format!("\"name\": \"{name}\"");
    let start = inspection.find(&marker).expect("named semantic node");
    &inspection[start..inspection.len().min(start + 800)]
}

fn semantic_center(inspection: &str, name: &str) -> (f64, f64) {
    fn find(value: &serde_json::Value, name: &str) -> Option<(f64, f64)> {
        if value.get("name").and_then(serde_json::Value::as_str) == Some(name) {
            let bounds = value.get("bounds")?;
            return Some((
                f64::midpoint(bounds.get("x0")?.as_f64()?, bounds.get("x1")?.as_f64()?),
                f64::midpoint(bounds.get("y0")?.as_f64()?, bounds.get("y1")?.as_f64()?),
            ));
        }
        match value {
            serde_json::Value::Array(values) => values.iter().find_map(|value| find(value, name)),
            serde_json::Value::Object(values) => {
                values.values().find_map(|value| find(value, name))
            }
            _ => None,
        }
    }
    find(&serde_json::from_str(inspection).unwrap(), name).expect("semantic bounds")
}

#[test]
fn controlled_inert_root_type_declares_monitor_history_without_provider_reads() {
    crate::sampler::reset_provider_read_count();
    let definition = offline_root()
        .document_history_definition()
        .expect("controlled Monitor root type declares History");
    assert_eq!(definition.app_id, "so.tilo.monitor");
    assert!(std::ptr::eq(
        definition,
        std::ptr::addr_of!(crate::framework_history::HISTORY)
    ));
    assert_eq!(
        crate::sampler::provider_read_count(),
        0,
        "controlled History declaration evidence must not read native providers"
    );
}

#[test]
fn sample_root_construction_performs_no_native_provider_reads() {
    crate::sampler::reset_provider_read_count();
    let _root = offline_root();
    assert_eq!(
        crate::sampler::provider_read_count(),
        0,
        "the deterministic sample root must not refresh native providers"
    );
}

#[test]
fn managed_history_is_truthful_inert_and_returns_to_the_same_monitor_view() {
    crate::sampler::reset_provider_read_count();
    let mut driver = managed_driver();
    let process_live = driver.inspection_json().unwrap();
    assert!(
        process_live.contains("shell — PID"),
        "offline process row is visible"
    );
    driver
        .perform_semantic_named(
            "monitor_app",
            Some("monitor.tab.resources"),
            &ui::a11y::Action::Activate,
        )
        .unwrap();
    driver
        .perform_semantic_named(
            "monitor_app",
            Some("monitor.auto-refresh"),
            &ui::a11y::Action::ToggleCheck,
        )
        .unwrap();
    let live = driver.inspection_json().unwrap();
    assert!(semantic_fragment(&live, "Resources").contains("\"selected\": true"));
    assert!(semantic_fragment(&live, "Automatic refresh").contains("\"checked\": false"));
    let resources = resource_snapshot(&driver);
    let heads = driver.state_mut().doc.get_heads();
    let pending = driver.state_mut().doc.pending_ops();

    driver.click_named("history-open").unwrap();
    let history = driver.inspection_json().unwrap();
    assert!(history.contains(crate::framework_history::LIMITATION));
    assert!(driver.targets().contains_key("history-open"));
    assert!(!driver.targets().contains_key("activity_timeline"));
    assert!(!driver.targets().contains_key("history-restore"));
    assert!(!driver.targets().contains_key("history-before"));
    assert!(!driver.targets().contains_key("history-after"));
    assert!(
        !history.contains("shell — PID"),
        "live sample rows stay out of History"
    );
    assert_eq!(driver.state_mut().doc.get_heads(), heads);
    assert_eq!(driver.state_mut().doc.pending_ops(), pending);

    driver.click_named("history-open").unwrap();
    let returned = driver.inspection_json().unwrap();
    assert!(semantic_fragment(&returned, "Resources").contains("\"selected\": true"));
    assert!(semantic_fragment(&returned, "Automatic refresh").contains("\"checked\": false"));
    assert_eq!(resource_snapshot(&driver), resources);
    assert_eq!(driver.state_mut().doc.get_heads(), heads);
    assert_eq!(driver.state_mut().doc.pending_ops(), pending);
    assert_ne!(returned, history);
    assert_ne!(live, history);
    assert_eq!(
        crate::sampler::provider_read_count(),
        0,
        "sample construction and History navigation stay provider-free"
    );
}

#[test]
fn pending_kill_confirmation_survives_history_and_signals_the_exact_process_once() {
    let probe = crate::sampler::SignalProbe::default();
    let (app, pid, started_at) = crate::MonitorApp::history_test_with_pending_kill(probe.clone());
    let mut driver = managed_driver_for(Box::new(app));

    assert!(
        probe.attempts().is_empty(),
        "confirmation has not signalled yet"
    );
    driver.click_named("history-open").unwrap();
    assert!(
        probe.attempts().is_empty(),
        "opening History does not dispatch"
    );
    driver.click_named("history-open").unwrap();
    assert!(
        probe.attempts().is_empty(),
        "closing History does not dispatch"
    );

    driver.relayout();
    let returned = driver.inspection_json().unwrap();
    let (x, y) = semantic_center(&returned, "Kill");
    driver.click_at(x, y);
    assert_eq!(
        probe.attempts(),
        vec![crate::sampler::SignalAttempt {
            pid,
            started_at,
            kind: crate::sampler::StopKind::Kill,
        }],
        "the original PID generation settles exactly once"
    );
    let returned = driver.inspection_json().unwrap();
    assert!(!returned.contains(&format!("PID {pid}")));

    driver.relayout();
    assert_eq!(probe.attempts().len(), 1, "settlement cannot replay");
}

#[test]
fn control_custody_monitor_unreachable_private_route_seals_e_suppresses_hidden_kill_and_refresh_and_runs_f()
 {
    crate::sampler::reset_provider_read_count();
    let probe = crate::sampler::SignalProbe::default();
    let (app, pid, started_at, arm, refresh_attempts) =
        crate::MonitorApp::history_test_controlled_root(probe.clone());
    let denied = std::rc::Rc::new(std::cell::Cell::new(false));
    let root = CustodyDeniedMonitorRoot {
        child: Box::new(app),
        denied: denied.clone(),
    };
    let mut driver = managed_driver_for(Box::new(root));

    arm.set(Some(crate::app::HistoryControlArm::Kill(pid)));
    driver.relayout();
    denied.set(true);
    driver.click_named("history-open").unwrap();
    let sealed = driver.pending_history_controls();
    assert_eq!(
        sealed.len(),
        1,
        "mounted custody seals generation-bound Kill E"
    );
    assert_eq!(
        sealed[0].invocation.address,
        ui::tilo_actions::ActionAddress::new(format!("process:{pid}:started:{started_at}"), "kill",)
    );
    assert!(sealed[0].invocation.arguments.is_empty());

    arm.set(Some(crate::app::HistoryControlArm::Kill(pid)));
    driver.relayout();
    arm.set(Some(crate::app::HistoryControlArm::Refresh));
    driver.relayout();
    assert_eq!(
        driver.pending_history_controls().len(),
        1,
        "hidden Kill and recovery Refresh H are absent from saved output"
    );
    assert!(probe.attempts().is_empty(), "E/H capture sends no signal");

    driver.click_named("history-open").unwrap();
    assert_eq!(
        driver.pending_history_controls().len(),
        1,
        "hidden H attempts do not reappear after returning live"
    );
    denied.set(false);
    driver.send(&winit::event::WindowEvent::Focused(true));
    driver
        .perform_semantic_named(
            "monitor_app",
            Some(&format!("monitor.process:{pid}:started:{started_at}:stop")),
            &ui::a11y::Action::Activate,
        )
        .unwrap();
    assert_eq!(
        probe.attempts(),
        vec![crate::sampler::SignalAttempt {
            pid,
            started_at,
            kind: crate::sampler::StopKind::Term,
        }],
        "fresh ordinary Stop F signals the exact generation once"
    );
    assert_eq!(
        refresh_attempts.get(),
        0,
        "E, hidden Kill/Refresh H, return, and fresh Stop perform no sampler refresh"
    );
    assert_eq!(crate::sampler::provider_read_count(), 0);
}

#[test]
fn monitor_external_policy_has_no_preview_or_restore_schema() {
    let scope = &crate::framework_history::HISTORY.scopes[0];
    assert_eq!(scope.key, "system-observations");
    assert!(scope.content_schema.is_none());
    assert!(scope.presentation.preview_factory().is_none());
    let HistoryPresentation::External { reason } = scope.presentation else {
        panic!("external presentation");
    };
    assert_eq!(reason, crate::framework_history::LIMITATION);

    let mut doc = AutoCommit::new();
    let heads = doc.get_heads();
    offline_root().sync(&mut doc, &automerge::ROOT);
    assert_eq!(doc.get_heads(), heads);
}
