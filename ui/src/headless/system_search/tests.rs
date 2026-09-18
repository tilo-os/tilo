use super::*;
use crate::search::system::SearchScope;

fn frame() -> FrameHarness {
    FrameHarness::new_with_controlled_inputs(
        State::new(),
        Box::new(
            crate::widget::TextInput::local()
                .with_id("document")
                .with_initial_value("unchanged"),
        ),
        (800, 640),
        Theme::dark(),
        crate::font_catalog::fixture_renderer(),
        crate::permissions::UserId("caller".into()),
    )
}
fn data() -> GlobalSearchSourceData {
    GlobalSearchSourceData {
        revision: "generation-1".into(),
        applications: vec![GlobalSearchApplication {
            app_id: "dev.qos.writer".into(),
            title: "Writer".into(),
            icon: None,
            action: qos_actions::ActionAddress::new("desktop:requests", "open-dev.qos.writer"),
        }],
        tabs: vec![GlobalSearchTab {
            target: "opaque-tab-11".into(),
            instance_id: 11,
            app_id: "dev.qos.notes".into(),
            record_id: "record-11".into(),
            title: "Trip notes".into(),
            in_active_workspace: true,
        }],
        active_instance_id: Some(11),
    }
}
fn actions() -> Vec<qos_actions::Resource> {
    vec![
        qos_actions::Resource::new("certified/revision", "document", "Writer").with_action(
            qos_actions::ActionSpec::typed(
                "insert",
                "Insert table",
                "",
                qos_actions::ExecutionLocus::ServerPreferred,
                vec![],
                |_| qos_actions::EncodedAction {
                    type_id: "never-execute".into(),
                    payload: vec![],
                },
            ),
        ),
    ]
}
fn enter(f: &mut FrameHarness) {
    f.send_host_event(&key_event(
        &Key::Named(NamedKey::Enter),
        ElementState::Pressed,
    ));
}
#[test]
fn global_search_owns_ime_and_restores_document_focus() {
    let mut f = frame();
    f.perform_semantic_named("document", None, &crate::a11y::Action::Focus)
        .unwrap();
    f.replace_global_search_data(data());
    f.set_caller_actions(actions());
    f.open_global_search_all("").unwrap();
    assert_eq!(f.global_search_scope(), Some(SearchScope::All));
    let json = f.inspection_json().unwrap();
    for name in [
        "Writer",
        "Trip notes",
        "Insert table",
        "Filter: Applications",
        "Everywhere",
    ] {
        assert!(json.contains(name), "missing {name}");
    }
    assert_eq!(
        f.inspection().keyboard_focus_owner.as_deref(),
        Some("system-search")
    );
    assert!(f.frame_output().ime.is_some());
    f.send_host_event(&WindowEvent::Ime(winit::event::Ime::Commit("Trip".into())));
    assert_eq!(f.global_search_query(), Some("Trip"));
    assert!(!f.inspection_json().unwrap().contains("Insert table"));
    f.send_host_event(&key_event(
        &Key::Named(NamedKey::Escape),
        ElementState::Pressed,
    ));
    assert_eq!(f.global_search_query(), None);
    assert!(f.inspection_json().unwrap().contains("unchanged"));
    assert!(
        f.inspection()
            .keyboard_focus_owner
            .unwrap()
            .contains("TextInput")
    );
}
#[test]
fn keyboard_semantic_and_scaled_pointer_produce_same_inert_target() {
    for mode in 0..3 {
        let mut f = frame();
        f.replace_global_search_data(data());
        f.open_global_search_all("Trip").unwrap();
        let before = f.state.doc.get_heads();
        if mode == 0 {
            enter(&mut f);
        } else if mode == 1 {
            let node = f.system_search_inspection().unwrap();
            let target = node.semantics.unwrap().children[1].children[0]
                .action_target
                .clone()
                .unwrap();
            f.perform_semantic_named(
                "system-search",
                Some(&target),
                &crate::a11y::Action::Activate,
            )
            .unwrap();
        } else {
            f.set_input_scale(2.0).unwrap();
            let rect = f
                .system_search
                .overlay
                .as_ref()
                .unwrap()
                .palette
                .row_rect(0)
                .unwrap();
            f.send_host_event(&WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(
                    rect.center().x * 2.0,
                    rect.center().y * 2.0,
                ),
            });
            f.send_host_event(&WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: ElementState::Pressed,
                button: MouseButton::Left,
            });
        }
        assert_eq!(
            f.take_global_search_intent(),
            Some(GlobalSearchIntent::SelectHostTarget {
                revision: "generation-1".into(),
                target: "opaque-tab-11".into()
            })
        );
        assert!(f.take_global_search_intent().is_none());
        assert_eq!(f.state.doc.get_heads(), before);
    }
}
#[test]
fn revocation_cannot_retarget_old_semantics_and_pending_intents_block_open() {
    let mut f = frame();
    f.replace_global_search_data(data());
    f.open_global_search_all("Trip").unwrap();
    let target = f
        .system_search_inspection()
        .unwrap()
        .semantics
        .unwrap()
        .children[1]
        .children[0]
        .action_target
        .clone()
        .unwrap();
    let mut replacement = data();
    replacement.revision = "generation-2".into();
    replacement.tabs[0].target = "other-tab".into();
    f.replace_global_search_data(replacement);
    assert!(
        f.perform_semantic_named(
            "system-search",
            Some(&target),
            &crate::a11y::Action::Activate
        )
        .is_err()
    );
    assert!(
        f.perform_semantic_named(
            "document",
            None,
            &crate::a11y::Action::SetValue("bad".into())
        )
        .is_err()
    );
    enter(&mut f);
    assert!(f.open_global_search_all("").is_err());
    f.take_global_search_intent();
    f.open_caller_search("").unwrap();
    assert!(f.open_global_search_all("").is_err());
}
#[test]
fn action_selection_hands_off_through_current_caller_authority() {
    let mut f = frame();
    f.set_caller_actions(actions());
    f.open_global_search_all("Insert").unwrap();
    enter(&mut f);
    let Some(GlobalSearchIntent::CallerAction(address)) = f.take_global_search_intent() else {
        panic!("inert action address")
    };
    assert!(f.take_caller_operation().is_none());
    f.handoff_caller_action(&address).unwrap();
    assert_eq!(f.take_caller_operation().unwrap().address, address);
    f.set_caller_actions(vec![]);
    assert!(f.handoff_caller_action(&address).is_err());
}
#[test]
fn accesskit_filters_and_query_share_the_current_native_controls() {
    let mut f = frame();
    let mut sources = data();
    let mut other = sources.tabs[0].clone();
    other.instance_id = 12;
    other.target = "opaque-12".into();
    other.title = "Other workspace".into();
    other.in_active_workspace = false;
    sources.tabs.push(other);
    f.replace_global_search_data(sources);
    f.set_caller_actions(actions());
    f.open_global_search_all("").unwrap();
    let active = f.a11y_node_named("Scope: Only Trip notes").unwrap();
    f.perform_a11y_action(active, &crate::a11y::Action::Activate)
        .unwrap();
    assert!(
        f.inspection_json().unwrap().contains("Insert table"),
        "native actions remain available in active-app scope"
    );
    assert!(
        f.a11y_focus()
            .unwrap()
            .1
            .label()
            .unwrap()
            .contains("Search apps")
    );
    let workspace = f
        .a11y_node_named("Scope: Workspace")
        .expect("native scope exposed to AT");
    f.perform_a11y_action(workspace, &crate::a11y::Action::Activate)
        .unwrap();
    assert!(!f.inspection_json().unwrap().contains("Other workspace"));
    let app_filter = f.a11y_node_named("Filter: Applications").unwrap();
    f.perform_a11y_action(app_filter, &crate::a11y::Action::Activate)
        .unwrap();
    assert_eq!(
        f.system_search
            .overlay
            .as_ref()
            .unwrap()
            .palette
            .visible_entries()
            .iter()
            .map(|entry| entry.label.as_str())
            .collect::<Vec<_>>(),
        vec!["Writer"]
    );
    f.perform_semantic_named(
        "system-search",
        Some("command-palette:query"),
        &crate::a11y::Action::SetValue("Writer".into()),
    )
    .unwrap();
    let app = f.a11y_node_named("Writer").unwrap();
    f.perform_a11y_action(app, &crate::a11y::Action::Activate)
        .unwrap();
    assert_eq!(
        f.take_global_search_intent(),
        Some(GlobalSearchIntent::CallerAction(
            qos_actions::ActionAddress::new("desktop:requests", "open-dev.qos.writer")
        ))
    );
}
#[test]
fn search_refuses_modal_trap_and_pending_caller_operations() {
    let mut f = frame();
    f.focus_trap_active = true;
    assert!(f.open_global_search_all("").is_err());
    f.focus_trap_active = false;
    f.root = Box::new(
        crate::widget::modal_dialog::ModalDialog::new(
            "Blocking",
            crate::widget::TextInput::local(),
        )
        .open(true),
    );
    f.relayout();
    assert!(f.open_global_search_all("").is_err());
    let mut f = frame();
    f.set_caller_actions(actions());
    f.handoff_caller_action(&qos_actions::ActionAddress::new(
        "certified/revision",
        "insert",
    ))
    .unwrap();
    assert!(f.open_global_search_all("").is_err());
}
#[test]
fn selected_action_collects_real_parameters_and_confirmation() {
    let mut f = frame();
    let mut resources = actions();
    resources[0].actions[0] = qos_actions::ActionSpec::typed(
        "insert",
        "Insert table",
        "",
        qos_actions::ExecutionLocus::ServerPreferred,
        vec![qos_actions::ActionParam::required(
            "rows",
            qos_actions::ParamType::Number,
            "Rows",
        )],
        |_| qos_actions::EncodedAction {
            type_id: "never-execute".into(),
            payload: vec![],
        },
    );
    resources[0].actions[0].contract.confirmation = qos_actions::ConfirmationPolicy::Phrase {
        phrase: "INSERT".into(),
    };
    f.set_caller_actions(resources);
    f.open_global_search_all("Insert").unwrap();
    enter(&mut f);
    let Some(GlobalSearchIntent::CallerAction(address)) = f.take_global_search_intent() else {
        panic!()
    };
    f.handoff_caller_action(&address).unwrap();
    assert_eq!(f.inspection().root.widget, "ActionPalette");
    assert!(f.take_caller_operation().is_none());
    f.send_host_event(&WindowEvent::Ime(winit::event::Ime::Commit(
        "{\"rows\":2}".into(),
    )));
    enter(&mut f);
    assert!(f.take_caller_operation().is_none());
    f.send_host_event(&WindowEvent::Ime(winit::event::Ime::Commit(
        "INSERT".into(),
    )));
    enter(&mut f);
    let operation = f.take_caller_operation().unwrap();
    assert_eq!(
        operation.wire_argument().as_deref(),
        Some("\u{1e}qos-args:{\"rows\":2}")
    );
    assert_eq!(
        operation.confirmation_evidence(),
        &serde_json::json!({"confirmation_phrase":"INSERT"})
    );
}
#[test]
fn mounted_provider_uses_exact_admitted_instance_and_current_permissions() {
    use crate::application::{LaunchTarget, RestoreInput};
    use crate::application_test_support::*;
    let host = ProbeHost::new();
    let observed = enable_provider_probe();
    let permissions = runner_permissions();
    let definition = runner_probe_definition();
    let context = host
        .prepare_with_permissions(definition, 11, &LaunchTarget::New, permissions.clone())
        .unwrap();
    let record = context.record().id.as_str().to_owned();
    let component = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .unwrap();
    let mut f = FrameHarness::from_component(component, (800, 640), runner_inputs()).unwrap();
    let mut sources = data();
    sources.tabs[0].app_id = APP.into();
    sources.tabs[0].record_id = record;
    f.replace_global_search_data(sources.clone());
    f.open_global_search_all("body-only-query").unwrap();
    assert!(
        observed
            .borrow()
            .methods
            .iter()
            .any(|(method, active)| *method == "page" && *active)
    );
    assert!(
        f.system_search
            .rows
            .values()
            .any(|row| row.title == "Probe"),
        "query-generated body hit, not static title"
    );
    enter(&mut f);
    assert_eq!(
        f.take_global_search_intent(),
        Some(GlobalSearchIntent::SelectHostTarget {
            revision: "generation-1".into(),
            target: "opaque-tab-11".into()
        })
    );
    // Same record identity with a mismatched instance is not an admitted correlation.
    sources.tabs[0].instance_id = 12;
    f.replace_global_search_data(sources.clone());
    f.open_global_search_all("body-only-query").unwrap();
    assert!(f.system_search.rows.is_empty());
    sources.tabs[0].instance_id = 11;
    f.replace_global_search_data(sources);
    assert!(!f.system_search.rows.is_empty());
    *permissions.borrow_mut() = crate::permissions::PermissionContext::from_capabilities(
        crate::permissions::Actor::person(None, "outsider"),
        vec![],
    )
    .at_unix_time(100);
    // The painted hit is revalidated against current component authority at activation.
    enter(&mut f);
    assert!(f.take_global_search_intent().is_none());
}

#[test]
fn direct_frame_pointer_helpers_cannot_bypass_the_modal_surface() {
    let mut f = frame();
    f.replace_global_search_data(data());
    f.open_global_search_all("Trip").unwrap();
    let rect = f
        .system_search
        .overlay
        .as_ref()
        .unwrap()
        .palette
        .row_rect(0)
        .unwrap();
    f.click_at(rect.center().x, rect.center().y);
    assert_eq!(
        f.take_global_search_intent(),
        Some(GlobalSearchIntent::SelectHostTarget {
            revision: "generation-1".into(),
            target: "opaque-tab-11".into()
        })
    );
    f.open_global_search_all("Trip").unwrap();
    enter(&mut f);
    assert!(
        f.open_caller_search("").is_err(),
        "an undrained system intent also blocks caller search"
    );
}
#[test]
fn layout_refreshes_content_without_retargeting_or_losing_keyboard_selection() {
    use crate::application::{LaunchTarget, RestoreInput};
    use crate::application_test_support::*;
    let host = ProbeHost::new();
    enable_provider_probe();
    let permissions = runner_permissions();
    let definition = runner_probe_definition();
    let context = host
        .prepare_with_permissions(definition, 11, &LaunchTarget::New, permissions.clone())
        .unwrap();
    let record = context.record().id.as_str().to_owned();
    let component = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .unwrap();
    let mut f = FrameHarness::from_component(component, (800, 640), runner_inputs()).unwrap();
    let mut sources = data();
    sources.tabs[0].app_id = APP.into();
    sources.tabs[0].record_id = record;
    f.replace_global_search_data(sources);
    f.open_global_search_all("").unwrap();
    f.key("ArrowDown");
    let selected = f
        .system_search
        .overlay
        .as_ref()
        .unwrap()
        .palette
        .selected_id()
        .unwrap()
        .to_owned();
    f.relayout();
    assert_eq!(
        f.system_search
            .overlay
            .as_ref()
            .unwrap()
            .palette
            .selected_id(),
        Some(selected.as_str())
    );
    f.open_global_search_all("body-only-query").unwrap();
    let old = f.a11y_node_named("Probe").unwrap();
    *permissions.borrow_mut() = crate::permissions::PermissionContext::from_capabilities(
        crate::permissions::Actor::person(None, "outsider"),
        vec![],
    )
    .at_unix_time(100);
    f.relayout();
    assert!(
        f.a11y_node_named("Probe").is_none(),
        "revocation refreshes without new query input"
    );
    assert!(
        f.perform_a11y_action(old, &crate::a11y::Action::Activate)
            .is_err()
    );
}
#[test]
fn touch_uses_the_same_inert_row_and_never_reaches_the_document() {
    let mut f = frame();
    f.replace_global_search_data(data());
    f.open_global_search_all("Trip").unwrap();
    let rect = f
        .system_search
        .overlay
        .as_ref()
        .unwrap()
        .palette
        .row_rect(0)
        .unwrap();
    f.tap_at(rect.center().x, rect.center().y);
    assert_eq!(
        f.take_global_search_intent(),
        Some(GlobalSearchIntent::SelectHostTarget {
            revision: "generation-1".into(),
            target: "opaque-tab-11".into()
        })
    );
    assert!(f.inspection_json().unwrap().contains("unchanged"));
}

#[test]
fn exact_application_name_outranks_actions_for_that_application() {
    let mut f = frame();
    f.replace_global_search_data(data());
    f.set_caller_actions(actions());
    f.open_global_search_all("Writer").unwrap();
    enter(&mut f);
    assert_eq!(
        f.take_global_search_intent(),
        Some(GlobalSearchIntent::CallerAction(
            qos_actions::ActionAddress::new("desktop:requests", "open-dev.qos.writer")
        ))
    );
}
