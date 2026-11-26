use super::*;
use crate::application::{AppComponent, LaunchTarget, RestoreInput};
use crate::application_test_support::*;
use crate::widget::TileHost;
use std::rc::Rc;

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
#[serial_test::serial(process_env)]
fn configured_theme_file_change_rethemes_an_open_app() {
    let config = tempfile::tempdir().expect("isolated appearance store");
    let _profile = settings_store::scoped_source(Some(settings_store::profile(config.path())));

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
    let dark_bg = app.theme.tokens.colors.surfaces.content;
    let dark_name = app.theme.name.clone();

    write("light");
    if let Some(due) = crate::time::now().checked_sub(std::time::Duration::from_millis(750)) {
        app.last_theme_store_check = due;
    }
    assert!(
        app.reconcile_configured_theme(),
        "an appearance file write must retheme the next tick"
    );
    let light_bg = app.theme.tokens.colors.surfaces.content;
    assert_ne!(
        (light_bg, app.theme.name.as_str()),
        (dark_bg, dark_name.as_str()),
        "light and dark app chrome must not share colours"
    );
    assert_eq!(app.theme.name, Theme::light().name);
}

#[test]
#[serial_test::serial(process_env)]
fn settings_file_write_rethemes_the_sampled_app_surface() {
    // U02 samples the open Settings window after writing the session
    // settings file. Sandbox XDG is a private tree; the session document
    // is the one the compositor watches. A write that leaves this surface
    // on the old theme is the failure.
    let session = tempfile::tempdir().expect("session settings");
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
    let dark_bg = app.theme.tokens.colors.surfaces.content;
    let dark_y = crate::a11y::relative_luminance(dark_bg);

    write("light");
    if let Some(due) = crate::time::now().checked_sub(std::time::Duration::from_millis(750)) {
        app.last_theme_store_check = due;
    }
    assert!(
        app.reconcile_configured_theme(),
        "a session-file theme write must retheme the next tick"
    );
    let light_bg = app.theme.tokens.colors.surfaces.content;
    let light_y = crate::a11y::relative_luminance(light_bg);
    assert_ne!(
        (light_bg, app.theme.name.as_str()),
        (dark_bg, Theme::dark().name.as_str()),
        "sampled app chrome must not stay on the old theme after a settings-file write"
    );
    assert!(
        (light_y - dark_y).abs() > 0.15,
        "light and dark sampled surfaces must not share luminance (light {light_y:.3}, dark {dark_y:.3})"
    );
    assert_eq!(app.theme.name, Theme::light().name);
}

#[test]
#[serial_test::serial(process_env)]
fn settings_keeps_the_session_theme_when_the_host_scheme_is_unset() {
    // The Settings sandbox clears the environment, so `XDG_CURRENT_DESKTOP`
    // is unset and the Settings portal is not granted. The runner still
    // re-applies that unset host reading after every session-file change.
    // U02 samples the centre of that window: it stays on the dark raised
    // surface unless the session document wins.
    let session = tempfile::tempdir().expect("session settings");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.remove("XDG_CURRENT_DESKTOP");
    // The session's settings, as the app's channel to the shell gives them.
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
    let dark_raised = app.theme.tokens.colors.surfaces.raised;
    let dark_y = crate::a11y::relative_luminance(dark_raised);

    write("light");
    if let Some(due) = crate::time::now().checked_sub(std::time::Duration::from_millis(750)) {
        app.last_theme_store_check = due;
    }
    assert!(
        app.reconcile_configured_theme(),
        "the session file change must be noticed"
    );
    // Production re-applies the host reading on that same tick. With no
    // portal, the reading is the unset scheme, which is painted as dark.
    let _ = app.apply_host_appearance(crate::host_appearance::HostAppearance::default());

    let raised = app.theme.tokens.colors.surfaces.raised;
    let light_y = crate::a11y::relative_luminance(raised);
    assert_ne!(
        raised, dark_raised,
        "Settings must not keep the previous raised surface after the session file changes"
    );
    assert!(
        (light_y - dark_y).abs() > 0.15,
        "the sampled Settings surface must change luminance (light {light_y:.3}, dark {dark_y:.3})"
    );
    assert_eq!(app.theme.name, Theme::light().name);
    assert!(
        crate::host_appearance::Source::start().is_none(),
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
        input: crate::widget::input_provenance::Input::Discrete,
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
        let started = crate::widget::ResourceOperation::local(
            pending,
            tilo_actions::OperationKind::Effect,
            "probe.pending",
            &[],
        )
        .start(
            &mut tile,
            &mut crate::widget::Edits::lend(&mut state.doc),
            &state.root,
        )
        .unwrap();
        assert!(matches!(
            started,
            Some(crate::widget::ResourceOperationStart::Accepted)
        ));
        tile.sync(&state.doc, &state.root);
        let shown = crate::bound_dispatch::callback_binding(&tile).unwrap();
        assert_eq!(shown.checkout.id, wanted);
        assert!(!first.lifetime.is_active());
        let answer = crate::widget::poll_resource_operation(
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
    crate::ImageStore::serve_window(
        &component.callback_binding().lifetime,
        std::rc::Rc::new(move |max_dimension, _: &crate::image_store::ServedWindow| {
            counted.set(counted.get() + 1);
            crate::ImageStore::host_driven_with_max_dimension(max_dimension).0
        }),
    );
    let (context, _) = component
        .reopener()
        .prepare(
            runner_probe_definition(),
            crate::resource::DocumentId::minted(),
        )
        .unwrap();
    let successor = context.wake_handle();
    let unused: crate::image_store::Fetcher =
        std::sync::Arc::new(|_| panic!("a served window never fetches its pictures itself"));
    {
        let _entry = successor.enter_if_open().unwrap();
        drop(crate::ImageStore::new(&unused));
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
    use crate::collaboration::document_content::{ContentLink, DocumentContent};
    use crate::image_store::{HostImageLoader, ServedWindow};
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;

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
    crate::ImageStore::serve_window(
        &first.wake_handle(),
        Rc::new(move |max_dimension, drawn_for: &ServedWindow| {
            let (store, loader) = crate::ImageStore::host_driven_with_max_dimension(max_dimension);
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
    let unused: crate::image_store::Fetcher =
        Arc::new(|_| panic!("a served window never fetches its pictures itself"));
    let store = {
        let _entry = successor.enter_if_open().unwrap();
        crate::ImageStore::new(&unused)
    };
    assert!(store.get(&url).is_none());

    // The host drains the request and reads the bytes where the window
    // reads its content: not here yet, so the window's own content link asks.
    let mut loaders = loaders.borrow_mut();
    let (loader, drawn_for) = loaders.last_mut().expect("the successor is served");
    assert_eq!(loader.next_request().as_deref(), Some(url.as_str()));
    let content = drawn_for.content().expect("the successor's content");
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
