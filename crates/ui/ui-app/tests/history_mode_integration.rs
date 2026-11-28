//! Managed components bind the framework History to their own record's owner, and a reviewed
//! restore rechecks authority and freshness before it writes.
use crate::support::application::{APP, ProbeHarness, runner_probe_definition};
use automerge::{ROOT, transaction::Transactable};
use tilo_ui_app::application::{LaunchTarget, RestoreInput};
use tilo_ui_app::history_mode::*;
use tilo_ui_app::{State, headless::Driver};

static BOUND: HistoryDefinition = HistoryDefinition {
    app_id: APP,
    scopes: &[HistoryScopeDefinition {
        key: "document",
        title: "Bound notebook",
        presentation: HistoryPresentation::Domain {
            create: tilo_ui_app::history_mode::testing::preview,
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
            .prepare_as(definition, 904, &LaunchTarget::New, tilo_ui_base::permissions::Role::Owner,)
            .unwrap();
        let component = definition
            .instantiate(context, LaunchTarget::New, RestoreInput::None)
            .unwrap();
        let binding = component.callback_binding_for_test();
        let selected = binding
            .records_for_test()
            .with_history(&binding.checkout_for_test().id, |doc, history| {
                doc.put(ROOT, "body", "bound first").unwrap();
                history.commit_action_authored(doc, None, None, Some(binding.author_for_test().clone()));
                let hash = doc.base_heads()[0];
                doc.put(ROOT, "body", "bound latest").unwrap();
                history.commit_action_authored(doc, None, None, Some(binding.author_for_test().clone()));
                format!(
                    "timeline-entry:{}",
                    tilo_ui_base::HistoryEntryId::hex(
                        &tilo_ui_app::history_source::entry_id_from_hash(&hash.to_string())
                    )
                )
            })
            .unwrap();
        let root = tilo_ui_app::widget::bound_app_widget(component);
        let source =
            HistorySourceRegistration::owned("document", APP, tilo_ui_app::bound_dispatch::history_owner(&*root).unwrap())
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
            tilo_ui_app::application::ComponentRunnerInputs {
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
        let root = tilo_ui_app::widget::TileHost::from_component(component);
        let mut driver = Driver::new(State::new(), Box::new(root), (900, 650));
        assert!(
            driver.targets().contains_key("history-open"),
            "a managed tile retains the component's History bundle"
        );
        driver.click_named("history-open").unwrap();
        assert!(driver.targets().contains_key("activity_timeline"));
    }
}

pub(crate) static RESTORABLE: HistoryDefinition = HistoryDefinition {
    app_id: "dev.tilo.restore-fixture",
    scopes: &[HistoryScopeDefinition {
        key: "document",
        title: "Notebook",
        presentation: HistoryPresentation::Domain {
            create: tilo_ui_app::history_mode::testing::preview,
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
                tilo_ui_app::DocumentId::for_tab(17),
            ),
        };
        let registration = HistoryRegistration::new(&RESTORABLE, vec![source]).unwrap();
        let mut driver = Driver::from_managed_root(
            state,
            ManagedRoot::new(Box::new(tilo_ui_app::widget::Column::new()), registration),
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
            tilo_ui_app::history_mode::testing::body(&driver.state_mut().doc),
            "old"
        );
        assert_eq!(driver.state_mut().doc.get_changes(&before).len(), 1);
        assert!(driver.state_mut().undo(None, None).is_some());
        assert_eq!(
            tilo_ui_app::history_mode::testing::body(&driver.state_mut().doc),
            "live"
        );
    }

    phase confirmation_rechecks_what_the_device_says_of_a_component_restore;
    {
        for revoked in [false, true] {
            let (_host, binding, mut driver) = bound_review_driver();
            // The device answers what restoring asks of the app: allowed while the person owns
            // the workspace.
            let restore = tilo_ui::shown_actions::ShownAction::app(
                APP,
                tilo_ui_base::permissions::ActionKind::Undoable,
            );
            assert!(!binding.permissions_for_test().borrow().permits(&restore), "not asked yet");
            crate::support::application::answer_read(
                binding.permissions_for_test(),
                &crate::support::application::device(tilo_ui_base::permissions::Role::Owner),
            );
            assert!(binding.permissions_for_test().borrow().permits(&restore));
            driver.click_named("history-restore").unwrap();
            if revoked {
                // The person's access is taken back while the review is open.
                crate::support::application::answer_read(
                    binding.permissions_for_test(),
                    &crate::support::application::stranger_device(),
                );
            }
            let before = bound_body(&binding);
            let confirm = driver
                .a11y_node_named("Restore version")
                .expect("real modal footer exports AT action");
            driver
                .perform_a11y_action(confirm, &tilo_ui_base::a11y::Action::Activate)
                .unwrap();
            if revoked {
                assert_eq!(bound_body(&binding), before, "a refused restore writes nothing");
                assert!(
                    driver
                        .inspection_json()
                        .unwrap()
                        .contains("no longer have permission")
                );
                assert!(driver.a11y_node_named("Refresh review").is_some());
            } else {
                assert_eq!(bound_body(&binding), "bound first");
            }
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
            tilo_ui_app::history_mode::testing::body(&driver.state_mut().doc),
            "old"
        );
        // The restore writes one change for each step since the version, and they are one
        // move: one undo takes all of it back.
        assert_eq!(driver.state_mut().doc.get_changes(&before).len(), 2);
        assert!(driver.state_mut().undo(None, None).is_some());
        assert_eq!(
            tilo_ui_app::history_mode::testing::body(&driver.state_mut().doc),
            "remote change"
        );
    }
}

/// A managed root over a component whose record has two versions, its history open on the older.
fn bound_review_driver() -> (
    ProbeHarness,
    tilo_ui_model::bound_dispatch::CallbackBinding,
    Driver,
) {
    let host = ProbeHarness::new();
    let definition = runner_probe_definition();
    let context = host
        .prepare_with_permissions(
            definition,
            905,
            &LaunchTarget::New,
            crate::support::application::permissions(),
        )
        .unwrap();
    let component = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .unwrap();
    let binding = component.callback_binding_for_test();
    let selected = binding
        .records_for_test()
        .with_history(&binding.checkout_for_test().id, |doc, history| {
            doc.put(ROOT, "body", "bound first").unwrap();
            history.commit_action_authored(
                doc,
                None,
                None,
                Some(binding.author_for_test().clone()),
            );
            let hash = doc.base_heads()[0];
            doc.put(ROOT, "body", "bound latest").unwrap();
            history.commit_action_authored(
                doc,
                None,
                None,
                Some(binding.author_for_test().clone()),
            );
            format!(
                "timeline-entry:{}",
                tilo_ui_base::HistoryEntryId::hex(
                    &tilo_ui_model::history_source::entry_id_from_hash(&hash.to_string())
                )
            )
        })
        .unwrap();
    let root = Box::new(tilo_ui_model::widget::BoundAppWidget::new(component));
    let source = HistorySourceRegistration::owned(
        "document",
        APP,
        tilo_ui_model::bound_dispatch::history_owner(&*root).unwrap(),
    )
    .unwrap();
    let registration = HistoryRegistration::new(&BOUND, vec![source]).unwrap();
    let mut driver = Driver::from_managed_root(
        State::new(),
        ManagedRoot::new(root, registration),
        (900, 650),
    )
    .unwrap();
    driver.click_named("history-open").unwrap();
    driver
        .perform_semantic_named(
            "activity_timeline",
            Some(&selected),
            &tilo_ui_base::a11y::Action::Activate,
        )
        .unwrap();
    (host, binding, driver)
}

/// What the component's record keeps at `body`.
fn bound_body(binding: &tilo_ui_model::bound_dispatch::CallbackBinding) -> String {
    binding
        .records_for_test()
        .read(&binding.checkout_for_test().id, |doc| {
            use automerge::ReadDoc as _;
            doc.get(ROOT, "body")
                .unwrap()
                .unwrap()
                .0
                .as_str()
                .unwrap()
                .to_owned()
        })
        .unwrap()
}

fn review_driver() -> Driver {
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
            tilo_ui_app::DocumentId::for_tab(18),
        ),
    };
    let registration = HistoryRegistration::new(&RESTORABLE, vec![source]).unwrap();
    let mut driver = Driver::from_managed_root(
        state,
        ManagedRoot::new(Box::new(tilo_ui_app::widget::Column::new()), registration),
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

/// A component whose History is the fixture's registered record, driven through a prepared root.
fn component_with_history() -> (ProbeHarness, tilo_ui_app::application::AppComponent) {
    let host = ProbeHarness::new();
    let definition = runner_probe_definition();
    let context = host
        .prepare_as(
            definition,
            950,
            &LaunchTarget::New,
            tilo_ui_base::permissions::Role::Owner,
        )
        .unwrap();
    let mut component = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .unwrap();
    component.register_document_history(&BOUND).unwrap();
    (host, component)
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
