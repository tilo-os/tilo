use super::*;
use crate::application::{AppComponent, LaunchTarget, RestoreInput};
use crate::application_test_support::*;
use crate::widget::TileHost;
use automerge::ReadDoc;
use winit::keyboard::{Key, NamedKey};

fn editable_component(host: &ProbeHost, instance: u64) -> AppComponent {
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
    // Case `prepared_native_services_keep_explicit_presentation_and_watch_system_updates`.
    {
        fn services(
            inputs: crate::application::ComponentRunnerInputs,
            binding: &crate::bound_dispatch::CallbackBinding,
        ) -> AppConstructionInputs {
            let mut services = AppConstructionInputs::controlled();
            services.debug_mode = || true;
            let mut presence = PresenceCtx::empty();
            presence.set_own_identity(["announced-handle"]);
            services.presence_ctx = ConstructionValue::Ready(presence);
            AppConstructionInputs::component_with_services(inputs, binding, services)
        }
        let host = ProbeHost::new();
        let component = editable_component(&host, 11);
        let mut app =
            App::from_component_using(component, presentation_inputs(2.0), services).unwrap();
        assert_eq!(app.local_author(), &author());
        assert!(app.presence_ctx.is_own_identity(&author().0));
        assert!(app.presence_ctx.is_own_identity("announced-handle"));
        assert!(app.debug_mode);
        assert!(app.watch_system_settings);
        assert_presentation(&app.theme, 1.5, 1.25);
        let due = app.last_theme_store_check + Duration::from_millis(750);
        assert!(app.reconcile_configured_theme_with(
            due,
            || Some(settings_store::SettingsStamp::Modified(
                std::time::SystemTime::UNIX_EPOCH
            )),
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
        assert!(app.component_close_reports()[0].record_error.is_none());
    }

    // Case `prepared_native_due_theme_poll_preserves_explicit_inputs_without_discovery`.
    {
        let host = ProbeHost::new();
        let component = editable_component(&host, 11);
        let mut app = App::from_component(component, presentation_inputs(2.0)).unwrap();
        let observed = observe_presentation();
        let checks = std::cell::Cell::new(0);
        let applies = std::cell::Cell::new(0);
        let due = app.last_theme_store_check + Duration::from_millis(750);
        let changed = app.reconcile_configured_theme_with(
            due,
            || {
                checks.set(checks.get() + 1);
                Some(settings_store::SettingsStamp::Modified(
                    std::time::SystemTime::UNIX_EPOCH,
                ))
            },
            |app| {
                applies.set(applies.get() + 1);
                app.accessibility.base_theme = Theme::light();
                // This controlled stand-in never creates a narrator or reads settings.
                app.accessibility.a11y.screen_reader = true;
            },
        );
        draw_prepared_component_test_frame(&mut app);
        assert_eq!(
            checks.get(),
            0,
            "prepared polling must not consult its source"
        );
        assert_eq!(applies.get(), 0);
        assert!(!changed);
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

    // Case `legacy_theme_poll_keeps_due_stamp_and_reload_behavior_with_controlled_sources`.
    {
        let mut app = App::from_factory_with_controlled_inputs(State::new(), || {
            Box::new(crate::widget::Column::new())
        });
        let start = app.last_theme_store_check;
        let checks = std::cell::Cell::new(0);
        let applies = std::cell::Cell::new(0);
        let stamp = Some(settings_store::SettingsStamp::Modified(
            std::time::SystemTime::UNIX_EPOCH,
        ));
        for (millis, source_stamp, expected_checks, expected_applies, expected_changed) in [
            (749, stamp, 0, 0, false),
            (750, stamp, 1, 1, true),
            (1500, stamp, 2, 1, false),
            (2250, None, 3, 2, true),
        ] {
            app.layout.layout_dirty = false;
            app.layout.structure_dirty = false;
            let changed = app.reconcile_configured_theme_with(
                start + Duration::from_millis(millis),
                || {
                    checks.set(checks.get() + 1);
                    source_stamp
                },
                |app| {
                    applies.set(applies.get() + 1);
                    app.accessibility.base_theme = Theme::light();
                },
            );
            assert_eq!(changed, expected_changed);
            assert_eq!(checks.get(), expected_checks);
            assert_eq!(applies.get(), expected_applies);
            assert_eq!(app.layout.layout_dirty, expected_changed);
            assert_eq!(app.layout.structure_dirty, expected_changed);
            assert!(app.accessibility.screen_reader.is_none());
        }
        assert!(app.theme_store_stamp.is_none());
        assert_eq!(app.theme.name, Theme::light().name);
    }
}

#[test]
#[serial_test::serial(process_env)]
fn configured_theme_file_change_rethemes_an_open_app() {
    let config = tempfile::tempdir().expect("isolated appearance store");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("XDG_CONFIG_HOME", config.path());

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
    let app_xdg = tempfile::tempdir().expect("sandboxed xdg");
    let session_file = session.path().join("settings.json");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("XDG_CONFIG_HOME", app_xdg.path())
        .set(settings_store::SESSION_SETTINGS_ENV, &session_file);

    let write = |theme: &str| {
        let mut values = serde_json::Map::new();
        values.insert("mode".into(), serde_json::Value::String(theme.into()));
        values.insert("high_contrast".into(), serde_json::Value::Bool(false));
        settings_store::update_section("appearance", values).expect("write appearance");
        assert_eq!(
            settings_store::settings_path().as_deref(),
            Some(session_file.as_path()),
            "the write must land on the session document, not the remapped XDG copy"
        );
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
    assert!(
        !app_xdg.path().join("tilo/settings.json").exists(),
        "the remapped XDG copy must stay unused"
    );
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
    let app_xdg = tempfile::tempdir().expect("sandboxed xdg");
    let session_file = session.path().join("settings.json");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.remove("XDG_CURRENT_DESKTOP")
        .set("XDG_CONFIG_HOME", app_xdg.path())
        .set(settings_store::SESSION_SETTINGS_ENV, &session_file);

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
            let host = ProbeHost::new();
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

fn wait_for_saved_record(host: &ProbeHost, binding: &crate::bound_dispatch::CallbackBinding) {
    assert!(host.records.same_owner(&binding.records));
    host.records.await_submitted(&binding.checkout.id).unwrap();
    let status = host.records.save_status(&binding.checkout.id).unwrap();
    assert_eq!(
        status,
        crate::record::RecordSaveStatus::Saved,
        "original writer did not acknowledge"
    );
}

fn assert_saved_body(
    host: &ProbeHost,
    binding: &crate::bound_dispatch::CallbackBinding,
    expected: &str,
) {
    wait_for_saved_record(host, binding);
    let path = app_state::DocStore::room_record_dir_at(
        &host.root(),
        APP,
        ROOM,
        &binding.checkout.id.storage_key(),
    )
    .unwrap()
    .join(app_state::DOCUMENT_FILE);
    let doc = automerge::AutoCommit::load_with_options(
        &std::fs::read(path).unwrap(),
        automerge::LoadOptions::new().on_partial_load(automerge::OnPartialLoad::Error),
    )
    .unwrap();
    assert_eq!(
        doc.get(automerge::ROOT, "body")
            .unwrap()
            .unwrap()
            .0
            .as_str(),
        Some(expected)
    );
}

fn edit_bound_record(app: &mut App, binding: &crate::bound_dispatch::CallbackBinding) {
    let target = crate::bound_dispatch::BoundActionTarget {
        binding: binding.clone(),
        address: crate::bound_dispatch::BoundActionAddress {
            instance_id: binding.instance_id,
            address: tilo_actions::ActionAddress::new("probe:shared", "edit"),
        },
    };
    app.invoke_bound_target(
        &target,
        &ResourceInvocation::new(&target.address.address, "probe-author"),
    )
    .unwrap();
    assert_eq!(body(&binding.records, &binding.checkout), "edited");
    assert_eq!(
        binding.records.save_status(&binding.checkout.id).unwrap(),
        crate::record::RecordSaveStatus::Saving
    );
}

#[test]
fn prepared_autosave_and_close_retain_original_owners_for_retry() {
    // Case `prepared_native_autosave_tick_acknowledges_original_writer_before_close`.
    {
        let host = ProbeHost::new();
        let component = editable_component(&host, 11);
        let binding = component.callback_binding();
        let mut app = App::from_component(component, runner_inputs()).unwrap();
        assert!(app.persistence.autosave.is_none() && app.persistence.records.is_none());
        assert_saved_body(&host, &binding, "initialized");
        edit_bound_record(&mut app, &binding);
        app.autosave_tick().unwrap();
        assert_saved_body(&host, &binding, "edited");
        assert!(binding.lifetime.is_active());
        assert!(app.component_close_reports().is_empty());
        app.close_components();
    }

    // Case `prepared_native_autosave_tick_reaches_distinct_embedded_owners_before_close`.
    {
        let first_host = ProbeHost::new();
        let first = editable_component(&first_host, 11);
        let first_binding = first.callback_binding();
        let mut app = App::from_component(first, runner_inputs()).unwrap();
        let second_host = ProbeHost::new();
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
        edit_bound_record(&mut app, &first_binding);
        edit_bound_record(&mut app, &second_binding);
        app.autosave_tick().unwrap();
        assert_saved_body(&first_host, &first_binding, "edited");
        assert_saved_body(&second_host, &second_binding, "edited");
        assert!(first_binding.lifetime.is_active() && second_binding.lifetime.is_active());
        assert!(app.component_close_reports().is_empty());
        app.close_components();
    }
}

#[test]
fn prepared_close_retains_the_original_store_and_rejects_stale_results() {
    // Case `prepared_three_runner_close_retains_original_view_store_for_retry`.
    {
        for mode in 0..3 {
            let host = ProbeHost::new();
            let component = runner_probe_definition()
                .instantiate(
                    host.prepare(runner_probe_definition(), 11, &LaunchTarget::New)
                        .unwrap(),
                    LaunchTarget::New,
                    RestoreInput::None,
                )
                .unwrap();
            let path = host.view_path(11);
            std::fs::create_dir_all(&path).unwrap();
            let mut runner = Runner::new(mode, component);
            drop(host.views);
            let mut reports = runner.close();
            assert_eq!(reports.len(), 1);
            let report = &mut reports[0];
            assert!(report.view_error.is_some());
            assert!(report.view_retry.is_some());
            assert_eq!(report.recovery.as_ref().unwrap().app_id, APP);
            drop(runner);
            std::fs::remove_dir(&path).unwrap();
            report.retry_view().unwrap();
            assert!(report.view_error.is_none());
            assert!(report.view_retry.is_none());
            let saved: crate::application_view::SavedInstanceView =
                postcard::from_bytes(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(saved.instance_id, 11);
            assert_eq!(saved.device_id, DEVICE);
            assert_eq!(saved.workspace_id, ROOM);
        }
    }
    // Case `prepared_native_close_retains_original_recovery_and_is_idempotent`.
    {
        let host = ProbeHost::new();
        let component = runner_probe_definition()
            .instantiate(
                host.prepare(runner_probe_definition(), 11, &LaunchTarget::New)
                    .unwrap(),
                LaunchTarget::New,
                RestoreInput::None,
            )
            .unwrap();
        let binding = component.callback_binding();
        let path = host.view_path(11);
        std::fs::create_dir_all(&path).unwrap();
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
        std::fs::remove_dir(&path).unwrap();
        reports[0].retry_view().unwrap();
        assert!(path.is_file());
        assert!(app.take_component_close_reports().is_empty());
    }
    // Case `prepared_three_runner_close_cancels_local_operations_and_rejects_stale_results`.
    {
        for mode in 0..3 {
            let host = ProbeHost::new();
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
                        presence.set_own_identity([author.0.clone()]);
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
    // Case `prepared_three_runner_factory_record_view_action_and_denial_parity`.
    {
        let seed = ProbeHost::new();
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
            let host = ProbeHost::new();
            let path = app_state::DocStore::room_record_dir_at(
                &host.root(),
                APP,
                ROOM,
                &checkout.id.storage_key(),
            )
            .unwrap()
            .join(app_state::DOCUMENT_FILE);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &bytes).unwrap();
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
                    // It is independent of the authenticated participant identity.
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
                .read_history(&checkout.id, |_, history| {
                    assert!(history.next_undo_in(&author(), 11).is_some());
                })
                .unwrap();
            *permissions.borrow_mut() = crate::permissions::PermissionContext::from_capabilities(
                crate::permissions::Actor::person(None, "probe-author"),
                vec![],
            )
            .at_unix_time(100);
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
                    assert!(history.next_undo_in(&author(), 11).is_none());
                })
                .unwrap();
            assert_eq!(body(&host.records, &checkout), "initialized");
        }
        assert_eq!(results, vec![b"\"edited\"".to_vec(); 3]);
    }

    // Case `component_palette_uses_host_text_through_prompt_confirmation_and_reopen`.
    {
        let host = ProbeHost::new();
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
        let catalog = crate::font_catalog::FontCatalog::controlled_fixture(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts"),
            "de-DE",
        );
        let mut renderer = TextRenderer::with_font_catalog(&catalog).unwrap();
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
            .as_mut()
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
            .as_mut()
            .unwrap()
            .set_query(r#"{"value":"typed","enabled":true}"#);
        palette_key(&mut app, Key::Named(NamedKey::Enter), None);
        assert_eq!(body(&host.records, &checkout), "typed");
        app.open_action_palette();
        activate_palette_action(&mut app, "confirm");
        palette_frame(&mut app);
        app.actions
            .action_palette
            .as_mut()
            .unwrap()
            .set_query("wrong");
        palette_key(&mut app, Key::Named(NamedKey::Enter), None);
        assert_eq!(body(&host.records, &checkout), "typed");
        app.actions.action_palette.as_mut().unwrap().set_query("JA");
        palette_key(&mut app, Key::Named(NamedKey::Enter), None);
        assert_eq!(body(&host.records, &checkout), "confirmed");
        app.open_action_palette();
        let before = app.text_renderer.cache_stats();
        app.text_renderer.bump_generation();
        palette_frame(&mut app);
        assert!(app.text_renderer.cache_stats().1 > before.1);
        host.records
            .read_history(&checkout.id, |_, history| {
                assert_eq!(
                    history.next_undo_in(&author(), 11).unwrap().author.as_ref(),
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
            .as_mut()
            .unwrap()
            .activate_bound(&target)
    );
}
