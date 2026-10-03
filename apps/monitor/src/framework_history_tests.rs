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
    let definition = ui::history_mode::capability::document_history_definition(root.as_ref())
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
    ui::headless::Driver::from_managed_root(
        ui::State::new(),
        ManagedRoot::new(root, registration),
        (1_000, 750),
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

/// The production root declares Monitor History statically, constructing the sample
/// root reads no native provider, and the external policy has no preview/restore.
#[test]
fn monitor_history_declaration_is_static_inert_and_external_only() {
    // Phase `production_root_declares_monitor_history`.
    {
        let definition =
            ui::history_mode::capability::document_history_definition(crate::app_root().as_ref())
                .expect("production Monitor root declares History");
        assert_eq!(definition.app_id, "so.tilo.monitor");
        assert!(std::ptr::eq(
            definition,
            std::ptr::addr_of!(crate::framework_history::HISTORY)
        ));
    }

    // Phase `sample_root_construction_performs_no_native_provider_reads`.
    {
        crate::sampler::reset_provider_read_count();
        let _root = offline_root();
        assert_eq!(
            crate::sampler::provider_read_count(),
            0,
            "the deterministic sample root must not refresh native providers"
        );
    }

    // Phase `monitor_external_policy_has_no_preview_or_restore_schema`.
    {
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
}

/// The window presents History in its own header and stays inert, and a
/// kill from the inspector signals the exact process once.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn managed_history_round_trip_preserves_view_and_signals_once() {
    // Phase `managed_history_is_truthful_inert_and_returns_to_the_same_monitor_view`.
    {
        crate::sampler::reset_provider_read_count();
        let mut driver = managed_driver();
        let process_live = driver.inspection_json().unwrap();
        assert!(
            process_live.contains("tilo-fs"),
            "offline process row is visible"
        );
        let (x, y) = semantic_center(&process_live, "Resources");
        driver.click_at(x, y);
        driver.relayout();
        let live = driver.inspection_json().unwrap();
        assert!(
            live.contains("Host name"),
            "the Resources tab shows the system's facts"
        );
        let resources = resource_snapshot(&driver);
        let heads = driver.state_mut().doc.get_heads();
        let pending = driver.state_mut().doc.pending_ops();

        // The window's header presents History itself, so the framework
        // draws no History bar of its own; looking at the window leaves the
        // document and the published resources as they were.
        assert!(driver.targets().contains_key("history-button"));
        assert!(!driver.targets().contains_key("history-open"));
        driver.relayout();
        assert_eq!(resource_snapshot(&driver), resources);
        assert_eq!(driver.state_mut().doc.get_heads(), heads);
        assert_eq!(driver.state_mut().doc.pending_ops(), pending);
        assert_eq!(
            crate::sampler::provider_read_count(),
            0,
            "sample construction stays provider-free"
        );
    }

    // Phase `inspector_kill_signals_the_exact_process_once`.
    {
        let probe = crate::sampler::SignalProbe::default();
        let app = crate::MonitorApp::controlled_for_test(probe.clone());
        let mut driver = managed_driver_for(Box::new(app));
        driver.relayout();

        let kill = driver.targets()["monitor.kill"];
        driver.click_at(kill.center().x, kill.center().y);
        driver.relayout();
        assert!(
            probe.attempts().is_empty(),
            "a kill waits for its confirmation"
        );
        let target = published_target(&driver, "Confirmation");
        driver
            .perform_semantic_named(
                "action-palette",
                Some(&target),
                &ui::a11y::Action::SetValue("CONFIRM".into()),
            )
            .unwrap();
        let confirm = published_target(&driver, "Confirm");
        driver
            .perform_semantic_named(
                "action-palette",
                Some(&confirm),
                &ui::a11y::Action::Activate,
            )
            .unwrap();
        let attempts = probe.attempts();
        assert_eq!(attempts.len(), 1, "the confirmed kill signals once");
        assert_eq!(attempts[0].kind, crate::sampler::StopKind::Kill);

        driver.relayout();
        assert_eq!(probe.attempts().len(), 1, "settlement cannot replay");
    }
}

/// The action target the inspection publishes for the node named `label`.
fn published_target(driver: &ui::headless::Driver, label: &str) -> String {
    fn access(node: &ui::a11y::AccessNode, label: &str) -> Option<String> {
        if node.name.as_deref() == Some(label) {
            return node.action_target.clone();
        }
        node.children.iter().find_map(|n| access(n, label))
    }
    fn widget(node: &ui::headless::InspectionNode, label: &str) -> Option<String> {
        node.semantics
            .as_ref()
            .and_then(|n| access(n, label))
            .or_else(|| node.children.iter().find_map(|n| widget(n, label)))
    }
    widget(&driver.inspection().root, label)
        .unwrap_or_else(|| panic!("missing inspection target {label}"))
}
