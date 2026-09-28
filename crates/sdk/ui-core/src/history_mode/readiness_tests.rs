use super::*;
use crate::widget::{Column, IconDescriptor};
use std::{cell::Cell, rc::Rc};

struct DraftRoot {
    pending: Rc<Cell<bool>>,
    calls: Rc<Cell<usize>>,
    inner: Column,
}
impl DraftRoot {
    fn new(pending: Rc<Cell<bool>>, calls: Rc<Cell<usize>>) -> Self {
        Self {
            pending,
            calls,
            inner: Column::new(),
        }
    }
}
impl Widget for DraftRoot {
    crate::provides!(dyn crate::history_mode::capability::DocumentHistory);

    crate::testing::delegate_widget_methods!(
        inner: sync,
        build_layout,
        apply_layout,
        draw,
        get_rect
    );
    crate::testing::inert_widget_methods!(name = "draft-root");
}

impl crate::history_mode::capability::DocumentHistory for DraftRoot {
    fn history_restore_readiness(&self, scope: &str) -> Result<(), String> {
        assert_eq!(scope, "document");
        self.calls.set(self.calls.get() + 1);
        if self.pending.get() {
            Err("Return to live to finish pending preferences".into())
        } else {
            Ok(())
        }
    }
}
crate::testing::test_app_widget!(
    DraftRoot,
    "Draft root",
    icon = IconDescriptor::Named("draft".into())
);
fn global_driver(pending: Rc<Cell<bool>>, calls: Rc<Cell<usize>>) -> Driver {
    let mut state = State::new();
    state.doc.put(ROOT, "body", "old").unwrap();
    state.commit_action(None, None);
    let hash = state.doc.base_heads()[0];
    state.doc.put(ROOT, "body", "live").unwrap();
    state.commit_action(None, None);
    let registration = HistoryRegistration::new(
        &RESTORABLE,
        vec![HistorySourceRegistration::HostDocument {
            scope: "document",
            resource: HistoryResource::new(
                "draft",
                "lifetime",
                RESTORABLE.app_id,
                crate::RecordId::for_tab(180),
            ),
        }],
    )
    .unwrap();
    let mut driver = Driver::from_managed_root(
        state,
        ManagedRoot::new(Box::new(DraftRoot::new(pending, calls)), registration),
        (900, 650),
    )
    .unwrap();
    driver.click_named("history-open").unwrap();
    let target = format!(
        "timeline-entry:{}",
        crate::widget::history_group::hex16(&crate::history_source::entry_id_from_hash(
            &hash.to_string()
        ))
    );
    driver
        .perform_semantic_named(
            "activity_timeline",
            Some(&target),
            &crate::a11y::Action::Activate,
        )
        .unwrap();
    driver
}
#[test]
fn readiness_refuses_review_refresh_and_confirmation_without_head_change() {
    for introduced_after_review in [false, true] {
        let pending = Rc::new(Cell::new(!introduced_after_review));
        let calls = Rc::new(Cell::new(0));
        let mut driver = global_driver(pending.clone(), calls.clone());
        let heads = driver.state_mut().doc.base_heads();
        driver.click_named("history-restore").unwrap();
        if introduced_after_review {
            assert!(driver.a11y_node_named("Restore version").is_some());
            pending.set(true);
            driver.key("Enter");
        }
        assert_eq!(driver.state_mut().doc.base_heads(), heads);
        assert!(
            driver
                .inspection_json()
                .unwrap()
                .contains("Return to live to finish pending preferences")
        );
        assert!(driver.a11y_node_named("Restore version").is_none());
        driver.key("Enter");
        assert!(driver.a11y_node_named("Restore version").is_none());
        assert!(calls.get() >= 2);
        driver.key("Escape");
        assert!(driver.a11y_node_named("Refresh review").is_none());
        pending.set(false);
        driver.click_named("history-restore").unwrap();
        driver.key("Enter");
        assert_eq!(
            crate::state::owned_restore_tests::value(&driver.state_mut().doc),
            "old"
        );
    }
}

static COMPONENT_HISTORY: HistoryDefinition = HistoryDefinition {
    app_id: APP,
    scopes: &[HistoryScopeDefinition {
        key: "document",
        title: "Notebook",
        presentation: HistoryPresentation::Domain {
            create: super::super::host_tests::preview,
        },
        content_schema: Some(&crate::state::owned_restore_tests::SCHEMA),
    }],
};
fn component(
    pending: Rc<Cell<bool>>,
    calls: Rc<Cell<usize>>,
    id: u64,
) -> (ProbeHost, crate::application::AppComponent) {
    let host = ProbeHost::new();
    let definition = runner_probe_definition();
    let context = host
        .prepare_with_permissions(definition, id, &LaunchTarget::New, runner_permissions())
        .unwrap();
    let mut component = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .unwrap();
    component.root = Some(Box::new(DraftRoot::new(pending, calls)));
    component
        .register_document_history(&COMPONENT_HISTORY)
        .unwrap();
    (host, component)
}
#[test]
fn readiness_component_adapter_forwards_only_its_own_root_and_refuses_unavailable() {
    let pending = Rc::new(Cell::new(true));
    let calls = Rc::new(Cell::new(0));
    let (_host, owned) = component(pending.clone(), calls.clone(), 182);
    let mut adapter = BoundAppWidget { component: owned };
    assert!(
        crate::history_mode::capability::history_restore_readiness(&adapter, "document").is_err()
    );
    let other_calls = Rc::new(Cell::new(0));
    let (_other, other) = component(Rc::new(Cell::new(false)), other_calls.clone(), 183);
    let other = BoundAppWidget { component: other };
    assert!(crate::history_mode::capability::history_restore_readiness(&other, "document").is_ok());
    assert_eq!(calls.get(), 1);
    assert_eq!(other_calls.get(), 1);
    pending.set(false);
    assert!(
        crate::history_mode::capability::history_restore_readiness(&adapter, "document").is_ok()
    );
    adapter.component.root = None;
    assert!(
        crate::history_mode::capability::history_restore_readiness(&adapter, "document").is_err()
    );
    adapter.component.close();
    assert!(
        crate::history_mode::capability::history_restore_readiness(&adapter, "document").is_err()
    );
    assert_eq!(calls.get(), 2);
}
#[test]
fn readiness_component_tile_checks_retained_child_at_confirmation() {
    let pending = Rc::new(Cell::new(false));
    let calls = Rc::new(Cell::new(0));
    let (_host, component) = component(pending.clone(), calls.clone(), 184);
    let binding = component.callback_binding();
    let selected = binding
        .records
        .with_history(&binding.checkout.id, |doc, history| {
            doc.put(ROOT, "body", "old").unwrap();
            history.commit_action_authored(doc, None, None, Some(binding.author.clone()));
            let hash = doc.base_heads()[0];
            doc.put(ROOT, "body", "live").unwrap();
            history.commit_action_authored(doc, None, None, Some(binding.author.clone()));
            hash
        })
        .unwrap();
    let tile = crate::widget::TileHost::from_component(component);
    let mut driver = Driver::new(State::new(), Box::new(tile), (900, 650));
    driver.click_named("history-open").unwrap();
    let target = format!(
        "timeline-entry:{}",
        crate::widget::history_group::hex16(&crate::history_source::entry_id_from_hash(
            &selected.to_string()
        ))
    );
    driver
        .perform_semantic_named(
            "activity_timeline",
            Some(&target),
            &crate::a11y::Action::Activate,
        )
        .unwrap();
    driver.click_named("history-restore").unwrap();
    assert!(driver.a11y_node_named("Restore version").is_some());
    pending.set(true);
    driver.key("Enter");
    assert!(
        driver
            .inspection_json()
            .unwrap()
            .contains("Return to live to finish pending preferences")
    );
    assert!(driver.a11y_node_named("Restore version").is_none());
    assert_eq!(
        binding
            .records
            .read_checked(
                &binding.checkout.id,
                crate::state::owned_restore_tests::value
            )
            .unwrap(),
        "live"
    );
    pending.set(false);
    driver.key("Enter");
    driver.key("Enter");
    assert_eq!(
        binding
            .records
            .read_checked(
                &binding.checkout.id,
                crate::state::owned_restore_tests::value
            )
            .unwrap(),
        "old"
    );
    assert_eq!(calls.get(), 4);
}
