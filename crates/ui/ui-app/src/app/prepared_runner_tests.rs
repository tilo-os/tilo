use super::*;
use crate::application::{AppComponent, LaunchTarget, RestoreInput};
use crate::application_test_support::*;
use crate::widget::TileHost;
use automerge::ReadDoc;
use std::rc::Rc;
use winit::keyboard::{Key, NamedKey};

fn editable_component(host: &ProbeHarness, instance: u64) -> AppComponent {
    runner_probe_definition()
        .instantiate(
            host.prepare_with_permissions(
                runner_probe_definition(),
                instance,
                &LaunchTarget::New,
                runner_permissions(),
            )
            .unwrap(),
            LaunchTarget::New,
            RestoreInput::None,
        )
        .unwrap()
}

#[test]
fn prepared_services_and_theme_polls_keep_explicit_presentation_inputs() {
    {
        fn services(
            inputs: crate::application::ComponentRunnerInputs,
            binding: &crate::bound_dispatch::CallbackBinding,
        ) -> AppConstructionInputs {
            let mut services = AppConstructionInputs::controlled();
            services.debug_mode = || true;
            let mut presence = PresenceCtx::empty();
            presence.set_own_identity([tilo_ui_base::permissions::MemberId::person(
                "announced-handle",
            )]);
            services.presence_ctx = ConstructionValue::Ready(presence);
            AppConstructionInputs::component_with_services(inputs, binding, services)
        }
        let host = ProbeHarness::new();
        let component = editable_component(&host, 11);
        let mut app =
            App::from_component_using(component, presentation_inputs(2.0), services).unwrap();
        assert_eq!(app.local_author(), &author());
        assert!(app.presence_ctx.is_own_identity(&author()));
        assert!(
            app.presence_ctx
                .is_own_identity(&tilo_ui_base::permissions::MemberId::person(
                    "announced-handle"
                ))
        );
        assert!(app.debug_mode);
        assert!(app.watch_system_settings);
        assert_presentation(&app.theme, 1.5, 1.25);
        let due = app.last_theme_store_check + Duration::from_millis(750);
        assert!(app.reconcile_configured_theme_with(
            due,
            || Some(settings_store::SettingsStamp(0)),
            |app| {
                app.accessibility.base_theme = Theme::light();
            },
        ));
        assert_eq!(app.theme.name, Theme::light().name);
        assert_eq!(app.theme.text_scale, 1.5);
        assert_eq!(app.theme.ui_scale, 1.25);
        assert!(app.theme.reduce_motion);
        draw_prepared_component_test_frame(&mut app);
        app.close_components();
        assert_eq!(app.component_close_reports().len(), 1);
        assert!(app.component_close_reports()[0].resource_error.is_none());
    }

    {
        let host = ProbeHarness::new();
        let component = editable_component(&host, 11);
        let mut app = App::from_component(component, presentation_inputs(2.0)).unwrap();
        let observed = observe_presentation();
        let due = app.last_theme_store_check + Duration::from_millis(750);
        let changed = app.reconcile_configured_theme_with(
            due,
            || Some(settings_store::SettingsStamp(0)),
            |app| {
                app.accessibility.base_theme = Theme::light();
                // This controlled stand-in never creates a narrator or reads settings.
                app.accessibility.a11y.screen_reader = true;
            },
        );
        draw_prepared_component_test_frame(&mut app);
        assert!(
            !changed,
            "a prepared app keeps the presentation it was given"
        );
        assert!(app.theme_store_stamp.is_none());
        assert!(app.accessibility.screen_reader.is_none());
        assert!(!app.accessibility.a11y.screen_reader);
        assert_presentation(&app.accessibility.base_theme, 2.0, 1.25);
        let observation = observed.borrow();
        let (theme, renderer_scale) = observation.draws.last().unwrap();
        assert_eq!(*renderer_scale, 1.5);
        assert_presentation(theme, 1.5, 1.25);
        app.close_components();
    }
}

/// A write to the session's appearance settings rethemes an open app on its next tick, and the
/// surfaces a person sees change with it. The Settings sandbox clears the environment, so
/// `XDG_CURRENT_DESKTOP` is unset and no portal is granted; the runner still re-applies that unset
/// host reading after every settings change, and the session document must win over it.
#[test]
#[serial_test::serial(process_env)]
fn a_settings_write_rethemes_an_open_app_over_an_unset_host_scheme() {
    let session = tempfile::tempdir().expect("session settings");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.remove("XDG_CURRENT_DESKTOP");
    let _session = settings_store::scoped_source(Some(settings_store::profile(session.path())));

    let write = |theme: &str| {
        let mut values = serde_json::Map::new();
        values.insert("mode".into(), serde_json::Value::String(theme.into()));
        values.insert("high_contrast".into(), serde_json::Value::Bool(false));
        settings_store::update_section("appearance", values).expect("write appearance");
    };

    write("dark");
    let mut app = App::from_factory_with_inputs(
        State::new(),
        || Box::new(crate::widget::Column::new()),
        || {
            let mut inputs = AppConstructionInputs::controlled();
            inputs.configured_theme = ConstructionValue::Lazy(Theme::configured);
            inputs.theme_store_stamp = settings_store::stamp;
            inputs
        },
    );
    app.apply_configured_theme();
    app.rebuild_theme();
    let dark = app.theme.tokens.colors.surfaces.clone();
    let dark_name = app.theme.name.clone();

    write("light");
    if let Some(due) = crate::time::now().checked_sub(std::time::Duration::from_millis(750)) {
        app.last_theme_store_check = due;
    }
    assert!(
        app.reconcile_configured_theme(),
        "a settings write must retheme the next tick"
    );
    let content = app.theme.tokens.colors.surfaces.content;
    assert_ne!(
        (content, app.theme.name.as_str()),
        (dark.content, dark_name.as_str()),
        "sampled app chrome must not stay on the old theme after a settings write"
    );
    let (light_y, dark_y) = (
        tilo_ui_base::a11y::relative_luminance(content),
        tilo_ui_base::a11y::relative_luminance(dark.content),
    );
    assert!(
        (light_y - dark_y).abs() > 0.15,
        "light and dark sampled surfaces must not share luminance (light {light_y:.3}, dark {dark_y:.3})"
    );
    assert_eq!(app.theme.name, Theme::light().name);

    // Production re-applies the host reading on that same tick. With no
    // portal, the reading is the unset scheme, which is painted as dark.
    let _ = app.apply_host_appearance(tilo_ui_base::host_appearance::HostAppearance::default());
    let raised = app.theme.tokens.colors.surfaces.raised;
    let (light_y, dark_y) = (
        tilo_ui_base::a11y::relative_luminance(raised),
        tilo_ui_base::a11y::relative_luminance(dark.raised),
    );
    assert_ne!(
        raised, dark.raised,
        "Settings must not keep the previous raised surface after the session file changes"
    );
    assert!(
        (light_y - dark_y).abs() > 0.15,
        "the sampled Settings surface must change luminance (light {light_y:.3}, dark {dark_y:.3})"
    );
    assert_eq!(app.theme.name, Theme::light().name);
    assert!(
        tilo_ui_base::host_appearance::Source::start().is_none(),
        "the session document owns appearance; the unset host portal must not start"
    );
}

#[test]
fn prepared_presentation_replaces_effective_factors_across_runners_and_setters() {
    for theme_scale in [0.5, 1.0, 1.5, 2.0] {
        for native in [true, false] {
            let host = ProbeHarness::new();
            let component = runner_probe_definition()
                .instantiate(
                    host.prepare(runner_probe_definition(), 11, &LaunchTarget::New)
                        .unwrap(),
                    LaunchTarget::New,
                    RestoreInput::None,
                )
                .unwrap();
            let inputs = presentation_inputs(theme_scale);
            let observed = observe_presentation();
            if native {
                let mut app = App::from_component(component, inputs).unwrap();
                for scale in [1.5, 0.5, 4.0, 4.0, 0.75] {
                    app.set_text_scale(scale);
                    draw_prepared_component_test_frame(&mut app);
                    let observation = observed.borrow();
                    let (theme, renderer_scale) = observation.draws.last().unwrap();
                    assert_eq!(*renderer_scale, scale);
                    assert_presentation(theme, scale, 1.25);
                    assert_presentation(observation.layouts.last().unwrap(), scale, 1.25);
                }
                app.set_ui_scale(1.5);
                app.set_ui_scale(1.5);
                app.set_high_contrast(true);
                assert_eq!(app.theme.name, "High contrast");
                assert_eq!(app.theme.text_scale, 0.75);
                assert_eq!(app.theme.ui_scale, 1.5);
                app.set_high_contrast(false);
                draw_prepared_component_test_frame(&mut app);
                let observation = observed.borrow();
                let (theme, renderer_scale) = observation.draws.last().unwrap();
                assert_eq!(*renderer_scale, 0.75);
                assert_presentation(theme, 0.75, 1.5);
                assert!(!observation.motion.is_empty());
                assert!(observation.motion.iter().all(|value| *value));
                app.close_components();
            } else {
                let mut driver =
                    crate::headless::Driver::from_component(component, (800, 600), inputs).unwrap();
                let observation = observed.borrow();
                let (theme, renderer_scale) = observation.draws.last().unwrap();
                assert_eq!(*renderer_scale, 1.5);
                assert_presentation(theme, 1.5, 1.25);
                assert_presentation(observation.layouts.last().unwrap(), 1.5, 1.25);
                assert!(!observation.motion.is_empty());
                assert!(observation.motion.iter().all(|value| *value));
                driver.close_components();
            }
        }
    }
}

fn wait_for_saved_resource(host: &ProbeHarness, binding: &crate::bound_dispatch::CallbackBinding) {
    assert!(host.records.same_owner(&binding.records));
    host.records.await_submitted(&binding.checkout.id).unwrap();
    let status = host.records.save_status(&binding.checkout.id).unwrap();
    assert_eq!(
        status,
        crate::resource::ResourceSaveStatus::Saved,
        "original writer did not acknowledge"
    );
}

fn assert_saved_body(
    host: &ProbeHarness,
    binding: &crate::bound_dispatch::CallbackBinding,
    expected: &str,
) {
    wait_for_saved_resource(host, binding);
    assert_eq!(
        body(&binding.records, &binding.checkout),
        expected,
        "the record holds what the window saved, and its writer says so"
    );
}

fn edit_bound_resource(app: &mut App, binding: &crate::bound_dispatch::CallbackBinding) {
    let target = crate::bound_dispatch::BoundActionTarget {
        input: tilo_ui_base::widget::input_provenance::Input::Discrete,
        binding: binding.clone(),
        address: crate::bound_dispatch::BoundActionAddress {
            instance_id: binding.instance_id,
            address: tilo_actions::ActionAddress::new("probe:shared", "edit"),
        },
    };
    app.invoke_bound_target(
        &target,
        &ResourceInvocation::new(
            &target.address.address,
            &tilo_actions::Actor::Member(tilo_actions::MemberId::person("probe-author")),
        ),
    )
    .unwrap();
    assert_eq!(body(&binding.records, &binding.checkout), "edited");
    assert_eq!(
        binding.records.save_status(&binding.checkout.id).unwrap(),
        crate::resource::ResourceSaveStatus::Saving
    );
}

tilo_test_env::scenario! {
    fn prepared_close_and_autosave_keep_their_original_owners;

    phase native_idle_model_persists_a_bound_edit_before_any_close;
    {
        let host = ProbeHarness::new();
        let component = editable_component(&host, 11);
        let binding = component.callback_binding();
        let mut app = App::from_component(component, runner_inputs()).unwrap();
        assert_saved_body(&host, &binding, "initialized");

        edit_bound_resource(&mut app, &binding);
        app.service_native_idle_model();

        // Read the original writer's complete on-disk graph before close can flush it.
        assert_saved_body(&host, &binding, "edited");
        assert!(binding.lifetime.is_active());
        assert!(app.component_close_reports().is_empty());
        app.close_components();
    }

    phase prepared_autosave_and_close_retain_original_owners_for_retry;
    {
        {
            let host = ProbeHarness::new();
            let component = editable_component(&host, 11);
            let binding = component.callback_binding();
            let mut app = App::from_component(component, runner_inputs()).unwrap();
            assert_saved_body(&host, &binding, "initialized");
            edit_bound_resource(&mut app, &binding);
            app.autosave_tick().unwrap();
            assert_saved_body(&host, &binding, "edited");
            assert!(binding.lifetime.is_active());
            assert!(app.component_close_reports().is_empty());
            app.close_components();
        }

        {
            let first_host = ProbeHarness::new();
            let first = editable_component(&first_host, 11);
            let first_binding = first.callback_binding();
            let mut app = App::from_component(first, runner_inputs()).unwrap();
            let second_host = ProbeHarness::new();
            let mut second = editable_component(&second_host, 22);
            let second_binding = second.callback_binding();
            assert!(!first_binding.records.same_owner(&second_binding.records));
            let handle = app.wake_scope.as_ref().unwrap().handle();
            second.mount(move |_| handle.wake()).unwrap();
            let first_root =
                std::mem::replace(&mut app.root_widget, Box::new(crate::widget::Column::new()));
            app.root_widget = Box::new(
                crate::widget::Column::new()
                    .push(first_root)
                    .push(Box::new(TileHost::from_component(second))),
            );
            assert_saved_body(&first_host, &first_binding, "initialized");
            assert_saved_body(&second_host, &second_binding, "initialized");
            edit_bound_resource(&mut app, &first_binding);
            edit_bound_resource(&mut app, &second_binding);
            app.autosave_tick().unwrap();
            assert_saved_body(&first_host, &first_binding, "edited");
            assert_saved_body(&second_host, &second_binding, "edited");
            assert!(first_binding.lifetime.is_active() && second_binding.lifetime.is_active());
            assert!(app.component_close_reports().is_empty());
            app.close_components();
        }
    }

    phase prepared_close_retains_the_original_store_and_rejects_stale_results;
    {
        {
            for mode in 0..3 {
                let host = ProbeHarness::new();
                let component = runner_probe_definition()
                    .instantiate(
                        host.prepare(runner_probe_definition(), 11, &LaunchTarget::New)
                            .unwrap(),
                        LaunchTarget::New,
                        RestoreInput::None,
                    )
                    .unwrap();
                // The workspace refuses the window's view, as one that cannot take it.
                host.workspace.refuse.set(true);
                let workspace = Rc::clone(&host.workspace);
                let mut runner = Runner::new(mode, component);
                drop(host.views);
                let mut reports = runner.close();
                assert_eq!(reports.len(), 1);
                let report = &mut reports[0];
                assert!(report.view_error.is_some());
                assert!(report.view_retry.is_some());
                assert_eq!(report.recovery.as_ref().unwrap().app_id, APP);
                drop(runner);
                workspace.refuse.set(false);
                report.retry_view().unwrap();
                assert!(report.view_error.is_none());
                assert!(report.view_retry.is_none());
                let saved = crate::application_view::InstanceViewStore::new(
                    Some(workspace),
                    APP,
                    DEVICE,
                    WORKSPACE,
                )
                .unwrap()
                .load(11)
                .unwrap()
                .unwrap();
                assert_eq!(saved.instance_id, 11);
                assert_eq!(saved.device_id, DEVICE);
                assert_eq!(saved.workspace_id, WORKSPACE);
            }
        }
        {
            let host = ProbeHarness::new();
            let component = runner_probe_definition()
                .instantiate(
                    host.prepare(runner_probe_definition(), 11, &LaunchTarget::New)
                        .unwrap(),
                    LaunchTarget::New,
                    RestoreInput::None,
                )
                .unwrap();
            let binding = component.callback_binding();
            host.workspace.refuse.set(true);
            let workspace = Rc::clone(&host.workspace);
            let mut app = App::from_component(component, runner_inputs()).unwrap();
            drop(host.views);
            app.close_wake_scope();
            let _closed = app.enter_wake_scope();
            app.close_native_content();
            assert!(!binding.lifetime.is_active());
            assert_eq!(app.component_close_reports().len(), 1);
            assert!(app.component_close_reports()[0].view_error.is_some());
            app.close_native_content();
            let mut reports = app.take_component_close_reports();
            assert_eq!(reports.len(), 1);
            assert!(binding.records.read(&binding.checkout.id, |_| ()).is_some());
            assert_eq!(reports[0].recovery.as_ref().unwrap().instance_id, 11);
            workspace.refuse.set(false);
            reports[0].retry_view().unwrap();
            assert!(
                crate::application_view::InstanceViewStore::new(
                    Some(Rc::clone(&workspace) as Rc<dyn crate::resource::DocumentPort>),
                    APP,
                    DEVICE,
                    WORKSPACE,
                )
                .unwrap()
                .load(11)
                .unwrap()
                .is_some(),
                "the view the window reopened is in the workspace"
            );
            assert!(app.take_component_close_reports().is_empty());
        }
        {
            for mode in 0..3 {
                let host = ProbeHarness::new();
                let component = runner_probe_definition()
                    .instantiate(
                        host.prepare_with_permissions(
                            runner_probe_definition(),
                            11,
                            &LaunchTarget::New,
                            runner_permissions(),
                        )
                        .unwrap(),
                        LaunchTarget::New,
                        RestoreInput::None,
                    )
                    .unwrap();
                let checkout = component.record().clone();
                let writer = local_state().writer();
                let observed = operation_observations()[0].clone();
                let mut runner = Runner::new(mode, component);
                runner.invoke("pending").unwrap();
                assert_eq!(*observed.borrow(), vec!["pending"]);
                let reports = runner.close();
                assert_eq!(*observed.borrow(), vec!["pending", "cancelled"]);
                assert!(!writer.try_set("stale after close".into()));
                let target = existing(&checkout);
                let successor = runner_probe_definition()
                    .instantiate(
                        host.prepare_with_permissions(
                            runner_probe_definition(),
                            11,
                            &target,
                            runner_permissions(),
                        )
                        .unwrap(),
                        target,
                        RestoreInput::Local(reports[0].recovery.clone().unwrap()),
                    )
                    .unwrap();
                let successor_local = local_state();
                let mut successor = Runner::new(mode, successor);
                assert!(!writer.try_set("stale after reopen".into()));
                assert_eq!(successor_local.with(Clone::clone), "construction wake");
                successor.invoke("view").unwrap();
                successor.close();
                assert_eq!(body(&host.records, &checkout), "initialized");
                assert_eq!(*observed.borrow(), vec!["pending", "cancelled"]);
            }
        }
    }
}

enum Runner {
    App(Box<App>),
    Driver(Box<crate::headless::Driver>),
}
impl Runner {
    fn new(mode: u8, mut component: AppComponent) -> Self {
        match mode {
            0 => Self::App(Box::new(
                App::from_component(component, runner_inputs()).unwrap(),
            )),
            1 => {
                let scope = crate::WakeScope::new();
                let handle = scope.handle();
                component.mount(move |_| handle.wake()).unwrap();
                let author = component.callback_binding().author;
                Self::App(Box::new(App::from_scoped_parts_with_inputs(
                    scope,
                    State::new(),
                    Box::new(TileHost::from_component(component)),
                    || {
                        let mut inputs = AppConstructionInputs::controlled();
                        let mut presence = PresenceCtx::empty();
                        presence.set_own_identity([author.clone()]);
                        inputs.local_author = ConstructionValue::Ready(author);
                        inputs.presence_ctx = ConstructionValue::Ready(presence);
                        inputs
                    },
                )))
            }
            _ => Self::Driver(Box::new(
                crate::headless::Driver::from_component(component, (320, 180), runner_inputs())
                    .unwrap(),
            )),
        }
    }
    fn invoke(&mut self, action: &str) -> Result<(), String> {
        match self {
            Self::App(app) => {
                app.init_layout();
                app.refresh_semantic_tree();
                let id = app
                    .semantic_tree()
                    .iter()
                    .find(|node| node.name.as_deref() == Some("Shared probe"))
                    .ok_or_else(|| "resource absent after authority projection".to_owned())?
                    .id
                    .clone();
                if app.invoke_semantic_action(&id, action, None)? {
                    Ok(())
                } else {
                    Err("action absent".into())
                }
            }
            Self::Driver(driver) => driver
                .invoke_bound_action(
                    &crate::bound_dispatch::BoundActionAddress {
                        instance_id: 11,
                        address: tilo_actions::ActionAddress::new("probe:shared", action),
                    },
                    &serde_json::Map::new(),
                )
                .map(|_| ())
                .map_err(|error| error.to_string()),
        }
    }
    fn close(&mut self) -> Vec<crate::application::ComponentCloseReport> {
        match self {
            Self::App(app) => {
                app.close_components();
                app.take_component_close_reports()
            }
            Self::Driver(driver) => {
                driver.close_components();
                driver.take_component_close_reports()
            }
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one source record and View are compared across three real runner paths"
)]
#[test]
fn prepared_runners_share_factory_view_action_and_palette_behaviour() {
    {
        let seed = ProbeHarness::new();
        let mut component = runner_probe_definition()
            .instantiate(
                seed.prepare(runner_probe_definition(), 11, &LaunchTarget::New)
                    .unwrap(),
                LaunchTarget::New,
                RestoreInput::None,
            )
            .unwrap();
        let checkout = component.record().clone();
        component
            .restore_local_view(&crate::view::View {
                scroll: vec![37.0],
                focus: Some(crate::view::FocusView {
                    ordinal: 0,
                    ephemeral: "caret=9".into(),
                }),
                ..crate::view::View::default()
            })
            .unwrap();
        let saved = component.close().recovery.unwrap();
        let bytes = seed
            .records
            .with(&checkout.id, automerge::AutoCommit::save)
            .unwrap();
        let mut results = Vec::new();
        for mode in 0..3 {
            // A workspace that holds the record it was left on.
            let workspace = Rc::new(crate::resource::MemoryWorkspace::default());
            let mut written = crate::resource::DocumentPort::write(
                &*workspace,
                APP,
                WORKSPACE,
                checkout.id.route(),
                checkout.kind.as_str(),
                bytes.clone(),
            );
            let _ = written(std::time::Duration::ZERO);
            let host = ProbeHarness::over(workspace);
            let target = existing(&checkout);
            let permissions = runner_permissions();
            let context = host
                .prepare_with_permissions(
                    runner_probe_definition(),
                    11,
                    &target,
                    permissions.clone(),
                )
                .unwrap();
            let component = runner_probe_definition()
                .instantiate(context, target, RestoreInput::Local(saved.clone()))
                .unwrap();
            assert_eq!(calls(), 1);
            assert_eq!(initializations(), 0);
            assert_eq!(body(&host.records, &checkout), "initialized");
            assert_eq!(component.capture_local_view().unwrap(), saved.view);
            host.records
                .with(&checkout.id, |doc| {
                    use automerge::transaction::Transactable;
                    assert_eq!(doc.pending_ops(), 0);
                    // A fresh CRDT actor sorts before the copied historical actors.
                    // It is independent of the authenticated member identity.
                    doc.set_actor(automerge::ActorId::from([0_u8].as_slice()));
                    assert_eq!(doc.pending_ops(), 0);
                })
                .unwrap();
            let mut runner = Runner::new(mode, component);
            runner.invoke("view").unwrap();
            runner.invoke("effect").unwrap();
            assert_eq!(
                *operation_observations()[0].borrow(),
                vec!["View", "Effect"]
            );
            runner.invoke("edit").unwrap();
            assert_eq!(body(&host.records, &checkout), "edited");
            host.records
                .read_history(&checkout.id, |doc, history| {
                    assert!(history.next_undo_in(doc, &author(), 11).is_some());
                })
                .unwrap();
            permissions.set(
                tilo_permissions::PermissionContext::stranger(
                    &tilo_ui_base::permissions::MemberId::person("probe-author"),
                )
                .at_unix_time(100),
            );
            assert!(runner.invoke("edit").is_err());
            assert_eq!(body(&host.records, &checkout), "edited");
            assert_eq!(
                *operation_observations()[0].borrow(),
                vec!["View", "Effect"]
            );
            let reports = runner.close();
            assert_eq!(reports.len(), 1);
            assert_eq!(reports[0].recovery.as_ref().unwrap().view, saved.view);
            assert!(reports[0].view_error.is_none());
            results.push(
                serde_json::to_vec(
                    &host
                        .records
                        .read(&checkout.id, |doc| {
                            doc.get(automerge::ROOT, "body")
                                .unwrap()
                                .unwrap()
                                .0
                                .as_str()
                                .unwrap()
                                .to_owned()
                        })
                        .unwrap(),
                )
                .unwrap(),
            );
            host.records
                .with_history(&checkout.id, |doc, history| {
                    assert!(history.undo_in(doc, &author(), 11, None, None).is_some());
                    assert!(history.next_undo_in(doc, &author(), 11).is_none());
                })
                .unwrap();
            assert_eq!(body(&host.records, &checkout), "initialized");
        }
        assert_eq!(results, vec![b"\"edited\"".to_vec(); 3]);
    }

    {
        let host = ProbeHarness::new();
        let component = runner_probe_definition()
            .instantiate(
                host.prepare_with_permissions(
                    runner_probe_definition(),
                    11,
                    &LaunchTarget::New,
                    runner_permissions(),
                )
                .unwrap(),
                LaunchTarget::New,
                RestoreInput::None,
            )
            .unwrap();
        let checkout = component.record().clone();
        let mut renderer = TextRenderer::with_font_catalog(
            &tilo_ui_base::font_catalog::fixture_catalog_in("de-DE"),
        )
        .expect("prepared canonical renderer fixture");
        renderer.set_text_scale(1.5);
        renderer.bump_generation();
        let mut app = App::from_component(
            component,
            crate::application::ComponentRunnerInputs {
                theme: Theme::dark(),
                text_renderer: renderer,
            },
        )
        .unwrap();
        assert_eq!(app.local_author(), &author());
        app.init_layout();
        app.open_action_palette();
        let before = app.text_renderer.cache_stats();
        palette_frame(&mut app);
        assert!(
            app.text_renderer.cache_stats().1 > before.1,
            "measurement uses the host cache"
        );
        palette_key(&mut app, Key::Character("P".into()), Some("P"));
        assert_eq!(app.action_palette().unwrap().query(), "P");
        palette_frame(&mut app);
        activate_palette_action(&mut app, "prompt");
        assert!(app.action_palette().unwrap().in_argument_mode());
        palette_frame(&mut app);
        palette_key(&mut app, Key::Named(NamedKey::Escape), None);
        assert!(!app.action_palette().unwrap().in_argument_mode());
        palette_frame(&mut app);
        activate_palette_action(&mut app, "prompt");
        app.actions
            .action_palette
            .as_deref_mut()
            .unwrap()
            .set_query("prompted");
        palette_key(&mut app, Key::Named(NamedKey::Enter), None);
        assert_eq!(body(&host.records, &checkout), "prompted");
        app.open_action_palette();
        palette_frame(&mut app);
        activate_palette_action(&mut app, "typed");
        palette_frame(&mut app);
        app.actions
            .action_palette
            .as_deref_mut()
            .unwrap()
            .set_query(r#"{"value":"typed","enabled":true}"#);
        palette_key(&mut app, Key::Named(NamedKey::Enter), None);
        assert_eq!(body(&host.records, &checkout), "typed");
        app.open_action_palette();
        activate_palette_action(&mut app, "confirm");
        // Confirming queues the action and closes the palette, so this frame draws what the
        // close left and drains the queued dispatch; there is no palette left to lay out.
        frame(&mut app);
        assert_eq!(
            body(&host.records, &checkout),
            "typed",
            "a confirmed action waits for the answer"
        );
        palette_key(&mut app, Key::Named(NamedKey::Enter), None);
        assert_eq!(body(&host.records, &checkout), "confirmed");
        app.open_action_palette();
        let before = app.text_renderer.cache_stats();
        app.text_renderer.bump_generation();
        palette_frame(&mut app);
        assert!(app.text_renderer.cache_stats().1 > before.1);
        host.records
            .read_history(&checkout.id, |doc, history| {
                assert_eq!(
                    history
                        .next_undo_in(doc, &author(), 11)
                        .unwrap()
                        .author
                        .as_ref(),
                    Some(&author())
                );
            })
            .unwrap();
        app.close_components();
    }
}

fn palette_key(app: &mut App, key: Key, text: Option<&str>) {
    let mut event = action_palette_tests::synth_enter();
    if let WindowEvent::KeyboardInput { event, .. } = &mut event {
        event.logical_key = key;
        event.text = text.map(Into::into);
    }
    assert!(app.action_palette_handle_key(&event));
}

fn frame(app: &mut App) {
    let mut scene = Scene::new();
    draw_frame_content(app, &mut scene, 800, 600, 1.0, None);
}

fn palette_frame(app: &mut App) {
    let mut scene = Scene::new();
    draw_frame_content(app, &mut scene, 800, 600, 1.0, None);
    let palette = app.action_palette().unwrap();
    assert!(palette.rect().width() > 0.0 && palette.rect().height() > 0.0);
    assert_eq!(
        crate::action_palette::observed_field_language(palette),
        "de-DE"
    );
    assert_eq!(app.text_renderer.text_scale(), 1.5);
}
fn activate_palette_action(app: &mut App, action: &str) {
    // This font/lifecycle fixture chooses the same mounted owner directly;
    // an open modal deliberately does not publish background actions.
    let resources = crate::bound_dispatch::discover_bound_resources(
        app.root_widget.as_ref(),
        &app.state.doc,
        None,
    )
    .unwrap();
    let target = crate::bound_dispatch::select_bound_target(
        &resources,
        &tilo_actions::ActionAddress::new("probe:shared", action),
        Some(11),
    )
    .unwrap()
    .unwrap();
    assert!(
        app.actions
            .action_palette
            .as_deref_mut()
            .unwrap()
            .activate_bound(&target)
    );
}

/// The component a window shows asking for another record of its app, as an
/// app's own Open does: built through the same checked construction as the
/// first, on the same store.
fn request_another_resource(component: &AppComponent) -> crate::resource::DocumentId {
    let reopener = component.reopener();
    let record = crate::resource::DocumentId::minted();
    let (context, target) = reopener
        .prepare(runner_probe_definition(), record.clone())
        .unwrap();
    let replacement = runner_probe_definition()
        .instantiate(context, target, RestoreInput::None)
        .unwrap();
    reopener.replace(replacement, vec!["another".into()]);
    record
}

/// An app the session runs with no window has no frame to mount the component its window asked
/// for: its wake mounts it, so it serves and publishes over the record it asked to show (the
/// record a window opened before it arrived).
#[test]
fn an_app_with_no_window_shows_the_record_its_component_asked_for_on_a_wake() {
    let host = ProbeHarness::new();
    let component = editable_component(&host, 35);
    let wanted = request_another_resource(&component);
    let mut app = App::from_component(component, runner_inputs()).unwrap();
    app.wake_unshown();
    let shown = crate::bound_dispatch::callback_binding(&*app.root_widget).unwrap();
    assert_eq!(
        shown.checkout.id, wanted,
        "the wake mounted the asked-for record"
    );
    app.close_components();
}

#[test]
fn a_window_shows_the_record_its_component_asked_for_in_its_place() {
    {
        let host = ProbeHarness::new();
        let component = editable_component(&host, 31);
        let first = component.callback_binding();
        let wanted = request_another_resource(&component);
        let mut app = App::from_component(component, runner_inputs()).unwrap();
        edit_bound_resource(&mut app, &first);
        assert!(
            app.swap_requested_component(),
            "the window shows another record"
        );
        assert!(!app.swap_requested_component(), "once");
        let shown = crate::bound_dispatch::callback_binding(&*app.root_widget).unwrap();
        assert_eq!(shown.checkout.id, wanted);
        assert!(
            !first.lifetime.is_active(),
            "the outgoing component is closed"
        );
        assert_saved_body(&host, &first, "edited");
        assert_eq!(body(&shown.records, &shown.checkout), "initialized");
        edit_bound_resource(&mut app, &shown);
        app.close_components();
    }

    {
        let host = ProbeHarness::new();
        let component = editable_component(&host, 33);
        let first = component.callback_binding();
        let reopener = component.reopener();
        let (context, target) = reopener
            .prepare(
                runner_probe_definition(),
                crate::resource::DocumentId::minted(),
            )
            .unwrap();
        let replacement = runner_probe_definition()
            .instantiate(context, target, RestoreInput::None)
            .unwrap();
        let outcome = reopener.replace(replacement, vec![]);
        let mut app = App::from_component(component, runner_inputs()).unwrap();
        assert!(outcome.withdraw(), "withdrawn before the host took it");
        assert!(
            !app.swap_requested_component(),
            "the window keeps its record"
        );
        assert_eq!(
            crate::bound_dispatch::callback_binding(&*app.root_widget)
                .unwrap()
                .checkout
                .id,
            first.checkout.id
        );
        assert!(first.lifetime.is_active());
        assert!(matches!(outcome.get(), Some(Err(_))));
        assert!(!outcome.withdraw(), "an answered request stays answered");
        app.close_components();
    }

    {
        let host = ProbeHarness::new();
        let component = editable_component(&host, 32);
        let first = component.callback_binding();
        let wanted = request_another_resource(&component);
        let mut tile = TileHost::from_component(component);
        let mut state = crate::State::new();
        // An operation the outgoing component admitted is answered when the
        // tile swaps, never inherited by the component that takes its place.
        let pending = tilo_actions::InvocationId::new();
        let started = tilo_ui_base::widget::ResourceOperation::local(
            pending,
            tilo_actions::OperationKind::Effect,
            "probe.pending",
            &[],
        )
        .start(
            &mut tile,
            &mut tilo_ui_base::widget::Edits::lend(&mut state.doc),
            &state.root,
        )
        .unwrap();
        assert!(matches!(
            started,
            Some(tilo_ui_base::widget::ResourceOperationStart::Accepted)
        ));
        tile.sync(&state.doc, &state.root);
        let shown = crate::bound_dispatch::callback_binding(&tile).unwrap();
        assert_eq!(shown.checkout.id, wanted);
        assert!(!first.lifetime.is_active());
        let answer = tilo_ui_base::widget::poll_resource_operation(
            &mut tile,
            pending,
            false,
            &state.doc,
            &state.root,
        )
        .expect("the outgoing component's operation is answered");
        assert!(answer.is_terminal());
        crate::component_lifecycle::close_components(&mut tile);
    }
}

#[cfg(feature = "native")]
#[test]
fn native_idle_model_wake_adopts_requested_component_before_draining() {
    let host = ProbeHarness::new();
    let component = editable_component(&host, 91);
    let first = component.callback_binding();
    let retained_reopener = component.reopener();
    let wanted = request_another_resource(&component);
    let mut app = App::from_component(component, runner_inputs()).unwrap();
    edit_bound_resource(&mut app, &first);
    let (tx, rx) = std::sync::mpsc::channel();
    app.wake_scope
        .as_mut()
        .unwrap()
        .bind(move |event| {
            tx.send(event).unwrap();
        })
        .unwrap();
    app.wake_scope.as_ref().unwrap().handle().wake();
    assert!(app.accept_wake(&rx.recv().unwrap()));
    app.service_native_idle_model();
    let shown = crate::bound_dispatch::callback_binding(&*app.root_widget).unwrap();
    assert_eq!(
        shown.checkout.id, wanted,
        "idle must adopt the checked replacement before syncing its owner"
    );
    assert!(
        !first.lifetime.is_active(),
        "old callback ownership must retire"
    );
    assert_saved_body(&host, &first, "edited");
    assert_eq!(body(&shown.records, &shown.checkout), "initialized");
    let retained_id = crate::resource::DocumentId::minted();
    let (retained, target) = retained_reopener
        .prepare(runner_probe_definition(), retained_id.clone())
        .expect("retirement preserves the authorized same-store preparation capability");
    assert_eq!(retained.record().id, retained_id);
    assert!(matches!(target, LaunchTarget::New));
    drop(retained);
    // A Reopener deliberately retains same-store capability; stale callback
    // admission is its closed scoped wake, not permission to prepare a record.
    app.update_native_model();
    app.sync_model(false);
    for _ in rx.try_iter() {}
    first.lifetime.wake();
    assert!(
        matches!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)),
        "the outgoing callback must not wake the current owner"
    );
    assert!(!app.service_native_idle_model().changed);
    assert_eq!(
        crate::bound_dispatch::callback_binding(&*app.root_widget)
            .unwrap()
            .checkout
            .id,
        wanted
    );
    app.close_components();
}

#[test]
#[serial_test::serial(process_env)]
fn a_running_app_takes_contrast_text_size_and_preset_on_the_tick_they_change() {
    let mut env = tilo_test_env::ScopedEnv::new();
    for name in [
        "TILO_A11Y_HIGH_CONTRAST",
        "TILO_HIGH_CONTRAST",
        "TILO_A11Y_TEXT_SCALE",
        "TILO_TEXT_SCALE",
        "TILO_UI_SCALE",
        "TILO_A11Y_UI_SCALE",
    ] {
        env.remove(name);
    }
    let serde_json::Value::Object(initial) =
        serde_json::json!({"appearance": {"mode": "dark", "high_contrast": false}})
    else {
        unreachable!("a document is an object")
    };
    let _document = settings_store::scoped_source(Some(std::sync::Arc::new(
        settings_store::HostDocument::new(initial, |_| Ok(())),
    )));
    let mut app = App::from_factory_with_inputs(
        State::new(),
        || Box::new(crate::widget::Column::new()),
        || {
            let mut inputs = AppConstructionInputs::controlled();
            inputs.configured_theme = ConstructionValue::Lazy(Theme::configured);
            inputs.theme_store_stamp = settings_store::stamp;
            inputs
        },
    );
    app.apply_configured_theme();
    app.rebuild_theme();
    let before = app.theme.tokens.colors.surfaces.content;
    assert!(!app.theme.document.palette.high_contrast);

    let change = |values: serde_json::Value| {
        let serde_json::Value::Object(values) = values else {
            unreachable!("a section is an object")
        };
        settings_store::update_section("appearance", values).expect("the host takes the write");
    };
    let tick = |app: &mut App| {
        // The app looked a moment ago: the change must not wait out the
        // file debounce or a later input.
        app.last_theme_store_check = crate::time::now();
        app.layout.layout_dirty = false;
        app.reconcile_configured_theme()
    };

    change(serde_json::json!({"high_contrast": true, "text_scale": 1.3}));
    assert!(tick(&mut app), "the change lands on the tick it woke");
    assert!(
        app.theme.document.palette.high_contrast,
        "the running app shows high contrast"
    );
    assert_ne!(app.theme.tokens.colors.surfaces.content, before);
    assert!(
        (app.theme.text_scale - 1.3).abs() < 1e-3,
        "the running app takes the text size it was not launched with ({})",
        app.theme.text_scale
    );
    assert!(app.layout.layout_dirty, "and lays itself out again in it");

    change(serde_json::json!({"high_contrast": false, "mode": "light", "text_scale": 1.0}));
    assert!(tick(&mut app));
    assert!(!app.theme.document.palette.high_contrast);
    assert_eq!(
        app.theme.name,
        Theme::light().name,
        "the preset changes live too"
    );
    assert!((app.theme.text_scale - 1.0).abs() < 1e-3);
    assert!(app.layout.layout_dirty);
}

/// A window opened in a served window's place (the window a record opens in
/// once it is had, another record opened here) shows in the same host: the
/// image stores it builds come from that host too, never the toolkit's own.
#[test]
fn a_window_opened_in_a_served_windows_place_is_served_by_its_host() {
    let host = ProbeHarness::new();
    let component = editable_component(&host, 37);
    let served = std::rc::Rc::new(std::cell::Cell::new(0_usize));
    let counted = std::rc::Rc::clone(&served);
    tilo_ui_base::ImageStore::serve_window(
        &component.callback_binding().lifetime,
        std::rc::Rc::new(
            move |max_dimension, _: &tilo_ui_base::image_store::ServedWindow| {
                counted.set(counted.get() + 1);
                tilo_ui_base::ImageStore::host_driven_with_max_dimension(max_dimension).0
            },
        ),
    );
    let (context, _) = component
        .reopener()
        .prepare(
            runner_probe_definition(),
            crate::resource::DocumentId::minted(),
        )
        .unwrap();
    let successor = context.wake_handle();
    let unused: tilo_ui_base::image_store::Fetcher =
        std::sync::Arc::new(|_| panic!("a served window never fetches its pictures itself"));
    {
        let _entry = successor.enter_if_open().unwrap();
        drop(tilo_ui_base::ImageStore::new(&unused));
    }
    assert_eq!(served.get(), 1, "the window in its place is served too");
    drop(context);
    drop(component);
}

/// A picture a window opened in a served window's place shows, whose bytes
/// the host lacks, is asked for on that window's own content requests, also
/// once the window it replaced closed, and loads when the bytes arrive there.
#[test]
fn a_successor_window_asks_for_its_pictures_on_its_own_content_requests() {
    use ::collaboration::document_content::ContentLink;
    use ::collaboration::document_content::DocumentContent;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;
    use tilo_ui_base::{image_store::HostImageLoader, image_store::ServedWindow};

    let host = ProbeHarness::new();
    // The host's loaders, each with the window it draws for, as a page keeps them.
    let loaders: Rc<RefCell<Vec<(HostImageLoader, ServedWindow)>>> = Rc::default();
    let page = Rc::clone(&loaders);
    let mut first = host
        .prepare_with_permissions(
            runner_probe_definition(),
            39,
            &LaunchTarget::New,
            runner_permissions(),
        )
        .unwrap();
    let first_link = Arc::new(ContentLink::default());
    first.provide_content(DocumentContent::store(
        tilo_content::ContentStore::memory(),
        Arc::clone(&first_link),
    ));
    tilo_ui_base::ImageStore::serve_window(
        &first.wake_handle(),
        Rc::new(move |max_dimension, drawn_for: &ServedWindow| {
            let (store, loader) =
                tilo_ui_base::ImageStore::host_driven_with_max_dimension(max_dimension);
            page.borrow_mut().push((loader, drawn_for.clone()));
            store
        }),
    );
    let mut component = runner_probe_definition()
        .instantiate(first, LaunchTarget::New, RestoreInput::None)
        .unwrap();

    // The window opens another record in its place, on a content link of its own.
    let (mut next, target) = component
        .reopener()
        .prepare(
            runner_probe_definition(),
            crate::resource::DocumentId::minted(),
        )
        .unwrap();
    let next_link = Arc::new(ContentLink::default());
    let next_local = tilo_content::ContentStore::memory();
    next.provide_content(DocumentContent::store(
        next_local.clone(),
        Arc::clone(&next_link),
    ));
    let successor = next.wake_handle();
    let replacement = runner_probe_definition()
        .instantiate(next, target, RestoreInput::None)
        .unwrap();
    component.close();
    drop(component);

    let png = crate::png::encode_rgba8(8, 8, &[220, 60, 60, 255].repeat(64)).unwrap();
    let hash = tilo_content::ContentStore::memory().put(&png).unwrap();
    let url = format!("tilo-blob://{hash}");
    let unused: tilo_ui_base::image_store::Fetcher =
        Arc::new(|_| panic!("a served window never fetches its pictures itself"));
    let store = {
        let _entry = successor.enter_if_open().unwrap();
        tilo_ui_base::ImageStore::new(&unused)
    };
    assert!(store.get(&url).is_none());

    // The host drains the request and reads the bytes where the window
    // reads its content: not here yet, so the window's own content link asks.
    let mut loaders = loaders.borrow_mut();
    let (loader, drawn_for) = loaders.last_mut().expect("the successor is served");
    assert_eq!(loader.next_request().as_deref(), Some(url.as_str()));
    let content = tilo_ui::served_content::content_of(drawn_for).expect("the successor's content");
    assert!(!content.has(&hash));
    assert!(content.read(&hash).is_none());
    assert_eq!(next_link.take_requests().wants, vec![hash.clone()]);
    assert!(
        first_link.take_requests().wants.is_empty(),
        "the closed window's content link asks for nothing"
    );

    // The bytes arrive on the successor's content link: the host hands them over.
    next_link.arrive(hash.clone(), png.clone());
    let bytes = content.read(&hash).expect("the bytes arrived");
    assert_eq!(bytes, png);
    let ticket = loader.reserve(&url).unwrap();
    assert!(loader.complete_decoded(
        &ticket,
        Some(tilo_image_codec::DecodedImage {
            rgba: [220, 60, 60, 255].repeat(64),
            width: 8,
            height: 8,
        }),
    ));
    let picture = store.get(&url).expect("the picture shows");
    assert_eq!((picture.width, picture.height), (8, 8));
    drop(loaders);
    drop(replacement);
}
