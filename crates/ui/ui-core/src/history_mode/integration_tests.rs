use super::*;
use crate::application::{LaunchTarget, RestoreInput};
use crate::application_test_support::{
    APP, ProbeHarness, runner_permissions, runner_probe_definition,
};
use crate::{State, headless::Driver, widget::BoundAppWidget};
use tilo_ui_base::widget::Widget;
use automerge::{ROOT, transaction::Transactable};

static BOUND: HistoryDefinition = HistoryDefinition {
    app_id: APP,
    scopes: &[HistoryScopeDefinition {
        key: "document",
        title: "Bound notebook",
        presentation: HistoryPresentation::Domain {
            create: super::host_tests::preview,
        },
    }],
};

tilo_test_env::scenario! {
    fn managed_components_bind_shared_history_to_their_real_owner;

    phase managed_bound_history_resolves_real_owner_instead_of_outer_state;
    {
        let host = ProbeHarness::new();
        let definition = runner_probe_definition();
        let context = host
            .prepare_with_permissions(definition, 904, &LaunchTarget::New, runner_permissions())
            .unwrap();
        let component = definition
            .instantiate(context, LaunchTarget::New, RestoreInput::None)
            .unwrap();
        let binding = component.callback_binding();
        let selected = binding
            .records
            .with_history(&binding.checkout.id, |doc, history| {
                doc.put(ROOT, "body", "bound first").unwrap();
                history.commit_action_authored(doc, None, None, Some(binding.author.clone()));
                let hash = doc.base_heads()[0];
                doc.put(ROOT, "body", "bound latest").unwrap();
                history.commit_action_authored(doc, None, None, Some(binding.author.clone()));
                format!(
                    "timeline-entry:{}",
                    tilo_ui_base::HistoryEntryId::hex(
                        &crate::history_source::entry_id_from_hash(&hash.to_string())
                    )
                )
            })
            .unwrap();
        let root = Box::new(BoundAppWidget::new(component));
        let source =
            HistorySourceRegistration::owned("document", APP, crate::bound_dispatch::history_owner(&*root).unwrap())
                .unwrap();
        let registration = HistoryRegistration::new(&BOUND, vec![source]).unwrap();
        let mut state = State::new();
        state.doc.put(ROOT, "body", "unrelated outer").unwrap();
        state.commit_action(None, None);
        let mut driver = Driver::from_managed_root(
            state,
            ManagedRoot::new(root, registration),
            (900, 650),
        )
        .expect("the registered actual bound owner must be admitted");
        driver.click_named("history-open").unwrap();
        driver
            .perform_semantic_named(
                "activity_timeline",
                Some(&selected),
                &tilo_ui_base::a11y::Action::Activate,
            )
            .unwrap();
        assert!(driver.targets().contains_key("history-preview"));
        let names = format!("{:?}", driver.a11y_nodes());
        assert!(
            names.contains("bound"),
            "preview reads bound content: {names}"
        );
        assert!(!names.contains("unrelated outer"));
    }

    phase managed_component_bundle_automatically_installs_shared_history_in_frame_runner;
    {
        let (_host, component) = component_with_history();
        let mut driver = Driver::from_component(
            component,
            (900, 650),
            crate::application::ComponentRunnerInputs {
                theme: tilo_ui_base::Theme::dark(),
                text_renderer: tilo_ui_base::font_catalog::fixture_renderer(),
            },
        )
        .unwrap();
        assert!(
            driver.targets().contains_key("history-open"),
            "managed component metadata follows its actual runner"
        );
        driver.click_named("history-open").unwrap();
        assert!(driver.targets().contains_key("activity_timeline"));
    }

    phase managed_component_tile_automatically_composes_shared_history;
    {
        let (_host, component) = component_with_history();
        let root = crate::widget::TileHost::from_component(component);
        let mut driver = Driver::new(State::new(), Box::new(root), (900, 650));
        assert!(
            driver.targets().contains_key("history-open"),
            "a managed tile retains the component's History bundle"
        );
        driver.click_named("history-open").unwrap();
        assert!(driver.targets().contains_key("activity_timeline"));
    }
}

/// The text a restore fixture's record holds at `body`.
pub(crate) fn body(doc: &automerge::AutoCommit) -> String {
    use automerge::ReadDoc as _;
    doc.get(ROOT, "body")
        .unwrap()
        .unwrap()
        .0
        .as_str()
        .unwrap()
        .to_owned()
}

pub(crate) static RESTORABLE: HistoryDefinition = HistoryDefinition {
    app_id: "dev.tilo.restore-fixture",
    scopes: &[HistoryScopeDefinition {
        key: "document",
        title: "Notebook",
        presentation: HistoryPresentation::Domain {
            create: super::host_tests::preview,
        },
    }],
};

tilo_test_env::scenario! {
    fn managed_review_confirmation_rechecks_authority_and_freshness;

    phase managed_review_confirm_restores_exactly_one_authored_change_and_can_cancel;
    {
        let mut state = State::new();
        state.doc.put(ROOT, "body", "old").unwrap();
        state.commit_action(None, None);
        let hash = state.doc.base_heads()[0];
        state.doc.put(ROOT, "body", "live").unwrap();
        state.commit_action(None, None);
        let before = state.doc.base_heads();
        let source = HistorySourceRegistration::HostDocument {
            scope: "document",
            resource: HistoryResource::new(
                "fixture",
                "restore-lifetime",
                RESTORABLE.app_id,
                crate::DocumentId::for_tab(17),
            ),
        };
        let registration = HistoryRegistration::new(&RESTORABLE, vec![source]).unwrap();
        let mut driver = Driver::from_managed_root(
            state,
            ManagedRoot::new(Box::new(crate::widget::Column::new()), registration),
            (900, 650),
        )
        .unwrap();
        driver.click_named("history-open").unwrap();
        let target = format!(
            "timeline-entry:{}",
            tilo_ui_base::HistoryEntryId::hex(&crate::history_source::entry_id_from_hash(
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
            .click_named("history-restore")
            .expect("registered content schema exposes reviewed restore");
        assert!(
            driver
                .inspection_json()
                .unwrap()
                .contains("Restore version")
        );
        assert_eq!(driver.state_mut().doc.base_heads(), before);
        driver.key("Escape");
        assert_eq!(driver.state_mut().doc.base_heads(), before);
        driver.click_named("history-restore").unwrap();
        driver
            .click_named("history-restore-review.primary")
            .unwrap();
        assert_eq!(
            crate::history_mode::integration_tests::body(&driver.state_mut().doc),
            "old"
        );
        assert_eq!(driver.state_mut().doc.get_changes(&before).len(), 1);
        assert!(driver.state_mut().undo(None, None).is_some());
        assert_eq!(
            crate::history_mode::integration_tests::body(&driver.state_mut().doc),
            "live"
        );
    }

    phase actual_confirmation_rechecks_unchanged_grant_expiry_replacement_and_actor_mapping;
    {
        use tilo_ui_base::permissions::*;
        for case in ["expiry", "replacement", "actor"] {
            let mut driver = review_driver();
            driver.set_permission_ctx(grant(LOCAL_USER, RESTORABLE.app_id, Some(150)));
            driver.click_named("history-restore").unwrap();
            let before = driver.state_mut().doc.base_heads();
            match case {
                "expiry" => NOW.set(200),
                "replacement" => driver.set_permission_ctx(
                    tilo_ui_base::permissions::PermissionContext::stranger(&tilo_ui_base::permissions::MemberId::person(LOCAL_USER)),
                ),
                _ => driver.set_permission_ctx(grant("another-person", RESTORABLE.app_id, None)),
            }
            let confirm = driver
                .a11y_node_named("Restore version")
                .expect("real modal footer exports AT action");
            driver
                .perform_a11y_action(confirm, &tilo_ui_base::a11y::Action::Activate)
                .unwrap();
            assert_eq!(driver.state_mut().doc.base_heads(), before, "{case}");
            assert!(
                driver
                    .inspection_json()
                    .unwrap()
                    .contains("no longer have permission"),
                "{case}"
            );
            assert!(driver.a11y_node_named("Refresh review").is_some());
        }
    }

    phase stale_review_requires_refresh_and_cancelled_footer_target_cannot_confirm;
    {
        let mut driver = review_driver();
        driver.click_named("history-restore").unwrap();
        let old_confirm = driver.a11y_node_named("Restore version").unwrap();
        driver.key("Escape");
        assert!(
            driver
                .perform_a11y_action(old_confirm, &tilo_ui_base::a11y::Action::Activate)
                .is_err()
        );
        driver.click_named("history-restore").unwrap();
        driver
            .state_mut()
            .doc
            .put(ROOT, "body", "remote change")
            .unwrap();
        driver.state_mut().commit_action(None, None);
        let before = driver.state_mut().doc.base_heads();
        driver
            .click_named("history-restore-review.primary")
            .unwrap();
        assert_eq!(driver.state_mut().doc.base_heads(), before);
        assert!(driver.a11y_node_named("Refresh review").is_some());
        driver
            .click_named("history-restore-review.primary")
            .unwrap();
        assert_eq!(
            driver.state_mut().doc.base_heads(),
            before,
            "refresh itself does not write"
        );
        driver
            .click_named("history-restore-review.primary")
            .unwrap();
        assert_eq!(
            crate::history_mode::integration_tests::body(&driver.state_mut().doc),
            "old"
        );
        // The restore writes one change for each step since the version, and they are one
        // move: one undo takes all of it back.
        assert_eq!(driver.state_mut().doc.get_changes(&before).len(), 2);
        assert!(driver.state_mut().undo(None, None).is_some());
        assert_eq!(
            crate::history_mode::integration_tests::body(&driver.state_mut().doc),
            "remote change"
        );
    }
}

thread_local! { static NOW: std::cell::Cell<u64> = const { std::cell::Cell::new(100) }; }
fn current_time() -> Option<u64> {
    Some(NOW.get())
}
fn review_driver() -> Driver {
    NOW.set(100);
    let mut state = State::new();
    state.doc.put(ROOT, "body", "old").unwrap();
    state.commit_action(None, None);
    let hash = state.doc.base_heads()[0];
    state.doc.put(ROOT, "body", "live").unwrap();
    state.commit_action(None, None);
    let source = HistorySourceRegistration::HostDocument {
        scope: "document",
        resource: HistoryResource::new(
            "fixture",
            "restore-lifetime",
            RESTORABLE.app_id,
            crate::DocumentId::for_tab(18),
        ),
    };
    let registration = HistoryRegistration::new(&RESTORABLE, vec![source]).unwrap();
    let mut driver = Driver::from_managed_root(
        state,
        ManagedRoot::new(Box::new(crate::widget::Column::new()), registration),
        (900, 650),
    )
    .unwrap();
    driver.set_history_clock(current_time);
    driver.click_named("history-open").unwrap();
    let target = format!(
        "timeline-entry:{}",
        tilo_ui_base::HistoryEntryId::hex(&crate::history_source::entry_id_from_hash(
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
fn managed_review_dialog_drains_and_returns_to_retained_preview() {
    // Dialogs appear from their origin (4480e9f9b6): this test checks that
    // the review dialog rests once drawn, so motion reads at its end.
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut driver = review_driver();
    assert!(driver.targets().contains_key("history-preview"));
    assert!(
        !driver.frame_output().needs_redraw,
        "selected preview stayed dirty after draw"
    );

    driver.click_named("history-restore").unwrap();
    assert!(
        driver
            .inspection_json()
            .unwrap()
            .contains("Restore version")
    );
    assert!(
        !driver.frame_output().needs_redraw,
        "review dialog stayed dirty after draw: {:?}",
        driver.frame_requesters()
    );
    assert!(!driver.poll_frame().unwrap());

    driver.key("Escape");
    assert!(driver.targets().contains_key("history-preview"));
    assert!(driver.inspection_json().unwrap().contains("Notebook: old"));
    assert!(
        !driver.frame_output().needs_redraw,
        "retained preview was not current after closing review"
    );
    assert!(!driver.poll_frame().unwrap());
}
fn grant(actor: &str, app: &str, until: Option<u64>) -> tilo_ui_base::permissions::PermissionContext {
    use tilo_ui_base::permissions::*;
    PermissionContext::granted(
        &tilo_ui_base::permissions::MemberId::person(actor),
        &TreePath::root().app(app),
        policy::KindSet::ALL,
        until,
    )
    .at_unix_time(100)
}

#[allow(
    clippy::too_many_lines,
    reason = "The assertion helper compares queued and refused bindings across one ownership transition."
)]
#[path = "readiness_tests.rs"]
mod readiness_tests;

pub(crate) fn component_with_history() -> (ProbeHarness, crate::application::AppComponent) {
    let host = ProbeHarness::new();
    let definition = runner_probe_definition();
    let context = host
        .prepare_with_permissions(definition, 950, &LaunchTarget::New, runner_permissions())
        .unwrap();
    let mut component = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .unwrap();
    component.register_document_history(&BOUND).unwrap();
    (host, component)
}
