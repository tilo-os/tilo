use crate::support::application::{APP, ProbeHarness, runner_permissions, runner_probe_definition};
use automerge::{ROOT, transaction::Transactable};
use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use std::{cell::Cell, rc::Rc};
use tilo_ui_app::app::driver::Driver;
use tilo_ui_app::application::{LaunchTarget, RestoreInput};
use tilo_ui_app::headless::*;
use tilo_ui_app::history_mode::testing::*;
use tilo_ui_app::history_mode::*;
use tilo_ui_app::kurbo::*;
use tilo_ui_app::retained_present::*;
use tilo_ui_app::testing::*;
use tilo_ui_app::vello::*;
use tilo_ui_app::widget::*;
use tilo_ui_app::widget::{Column, IconDescriptor};
use tilo_ui_app::{State, widget::BoundAppWidget};
use tilo_ui_base::a11y::*;
use tilo_ui_base::component::*;
use tilo_ui_base::i18n::*;
use tilo_ui_base::icons::*;
use tilo_ui_base::interaction::*;
use tilo_ui_base::paint::*;
use tilo_ui_base::presence::*;
use tilo_ui_base::synthetic_input::*;
use tilo_ui_base::text_selection::*;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::*;
use tilo_ui_base::widget::Widget;
use tilo_ui_base::widget::*;
use winit::event::*;
use winit::keyboard::*;

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
    tilo_ui_base::provides!(dyn tilo_ui_app::history_mode::capability::DocumentHistory);

    tilo_ui_app::delegate_widget_methods!(
        inner: sync,
        build_layout,
        apply_layout,
        draw,
        get_rect
    );
    tilo_ui_app::inert_widget_methods!(name = "draft-root");
}

impl tilo_ui_app::history_mode::capability::DocumentHistory for DraftRoot {
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
tilo_ui_app::test_app_widget!(
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
                tilo_ui_app::DocumentId::for_tab(180),
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
        tilo_ui_base::HistoryEntryId::hex(&tilo_ui_app::history_source::entry_id_from_hash(
            &hash.to_string()
        ))
    );
    driver
        .perform_semantic_named(
            "activity_timeline",
            Some(&target),
            &tilo_ui_base::a11y::Action::Activate,
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
            driver
                .click_named("history-restore-review.primary")
                .unwrap();
        }
        assert_eq!(driver.state_mut().doc.base_heads(), heads);
        assert!(
            driver
                .inspection_json()
                .unwrap()
                .contains("Return to live to finish pending preferences")
        );
        assert!(driver.a11y_node_named("Restore version").is_none());
        driver
            .click_named("history-restore-review.primary")
            .unwrap();
        assert!(driver.a11y_node_named("Restore version").is_none());
        assert!(calls.get() >= 2);
        driver.key("Escape");
        assert!(driver.a11y_node_named("Refresh review").is_none());
        pending.set(false);
        driver.click_named("history-restore").unwrap();
        driver
            .click_named("history-restore-review.primary")
            .unwrap();
        assert_eq!(
            tilo_ui_app::history_mode::testing::body(&driver.state_mut().doc),
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
            create: tilo_ui_app::history_mode::testing::preview,
        },
    }],
};
fn component(
    pending: Rc<Cell<bool>>,
    calls: Rc<Cell<usize>>,
    id: u64,
) -> (ProbeHarness, tilo_ui_app::application::AppComponent) {
    let host = ProbeHarness::new();
    let definition = runner_probe_definition();
    let context = host
        .prepare_with_permissions(definition, id, &LaunchTarget::New, runner_permissions())
        .unwrap();
    let mut component = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .unwrap();
    component.test_set_root(Some(Box::new(DraftRoot::new(pending, calls))));
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
    let mut adapter = BoundAppWidget::new(owned);
    assert!(
        tilo_ui_app::history_mode::capability::history_restore_readiness(&adapter, "document")
            .is_err()
    );
    let other_calls = Rc::new(Cell::new(0));
    let (_other, other) = component(Rc::new(Cell::new(false)), other_calls.clone(), 183);
    let other = BoundAppWidget::new(other);
    assert!(
        tilo_ui_app::history_mode::capability::history_restore_readiness(&other, "document")
            .is_ok()
    );
    assert_eq!(calls.get(), 1);
    assert_eq!(other_calls.get(), 1);
    pending.set(false);
    assert!(
        tilo_ui_app::history_mode::capability::history_restore_readiness(&adapter, "document")
            .is_ok()
    );
    adapter.test_component_mut().test_set_root(None);
    assert!(
        tilo_ui_app::history_mode::capability::history_restore_readiness(&adapter, "document")
            .is_err()
    );
    adapter.test_component_mut().close();
    assert!(
        tilo_ui_app::history_mode::capability::history_restore_readiness(&adapter, "document")
            .is_err()
    );
    assert_eq!(calls.get(), 2);
}
#[test]
fn readiness_component_tile_checks_retained_child_at_confirmation() {
    let pending = Rc::new(Cell::new(false));
    let calls = Rc::new(Cell::new(0));
    let (_host, component) = component(pending.clone(), calls.clone(), 184);
    let binding = component.callback_binding_for_test();
    let selected = binding
        .records_for_test()
        .with_history(&binding.checkout_for_test().id, |doc, history| {
            doc.put(ROOT, "body", "old").unwrap();
            history.commit_action_authored(
                doc,
                None,
                None,
                Some(binding.author_for_test().clone()),
            );
            let hash = doc.base_heads()[0];
            doc.put(ROOT, "body", "live").unwrap();
            history.commit_action_authored(
                doc,
                None,
                None,
                Some(binding.author_for_test().clone()),
            );
            hash
        })
        .unwrap();
    let tile = tilo_ui_app::widget::TileHost::from_component(component);
    let mut driver = Driver::new(State::new(), Box::new(tile), (900, 650));
    driver.click_named("history-open").unwrap();
    let target = format!(
        "timeline-entry:{}",
        tilo_ui_base::HistoryEntryId::hex(&tilo_ui_app::history_source::entry_id_from_hash(
            &selected.to_string()
        ))
    );
    driver
        .perform_semantic_named(
            "activity_timeline",
            Some(&target),
            &tilo_ui_base::a11y::Action::Activate,
        )
        .unwrap();
    driver.click_named("history-restore").unwrap();
    assert!(driver.a11y_node_named("Restore version").is_some());
    pending.set(true);
    driver
        .click_named("history-restore-review.primary")
        .unwrap();
    assert!(
        driver
            .inspection_json()
            .unwrap()
            .contains("Return to live to finish pending preferences")
    );
    assert!(driver.a11y_node_named("Restore version").is_none());
    assert_eq!(
        binding
            .records_for_test()
            .read_checked(
                &binding.checkout_for_test().id,
                tilo_ui_app::history_mode::testing::body
            )
            .unwrap(),
        "live"
    );
    pending.set(false);
    driver
        .click_named("history-restore-review.primary")
        .unwrap();
    driver
        .click_named("history-restore-review.primary")
        .unwrap();
    assert_eq!(
        binding
            .records_for_test()
            .read_checked(
                &binding.checkout_for_test().id,
                tilo_ui_app::history_mode::testing::body
            )
            .unwrap(),
        "old"
    );
    assert_eq!(calls.get(), 4);
}
