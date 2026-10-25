use super::*;
use crate::dispatcher::testing::RecordingDispatcher;

/// The compositor's exact output is the shared control and automation target.
#[test]
fn peer_drag_exports_the_same_exact_request_without_editing_local_outputs() {
    let _guard = crate::prefs::test_env::ConfigGuard::new();
    let mut pane = laptop_with_monitor();
    let saved = pane.output_config.clone();
    pane.device_displays = vec![settings_store::device_displays::DeviceDisplay {
        device: "drag-peer".into(),
        device_label: "Laptop".into(),
        connector: "eDP-1".into(),
        output_id: None,
        label: "Panel".into(),
        online: true,
        local: false,
        x: 1920,
        y: 0,
        width: 1280,
        height: 720,
        scale_milli: 1250,
        revision: 9,
    }];
    let row = DisplayModel::device_output_row(&pane.device_displays[0]);
    assert_eq!(row.id(), "settings.device-display:drag-peer/eDP-1");
    assert!(
        pane.apply(&DisplayEffect::MoveDeviceDisplay {
            display: "drag-peer/eDP-1".into(),
            revision: 8,
            x: 0,
            y: 1080
        })
        .is_err(),
        "a drag bound before a new peer layout cannot overwrite it"
    );
    pane.apply(&DisplayEffect::MoveDeviceDisplay {
        display: "drag-peer/eDP-1".into(),
        revision: 9,
        x: 0,
        y: 1080,
    })
    .unwrap();
    let ui::widget::AppHostIntent::MoveDeviceDisplay { request, reply } =
        pane.take_host_intent().unwrap()
    else {
        panic!("exact typed host request");
    };
    assert_eq!(request.screen, pane.device_displays[0]);
    assert_eq!((request.x, request.y), (0, 1080));
    assert_eq!(pane.output_config, saved);
    assert!(pane.pending_candidate.is_none());
    assert!(pane.take_host_intent().is_none());
    reply.upgrade().unwrap().borrow_mut().result = Some(Err("Disconnected".into()));
    pane.poll_device_move();
    assert_eq!(pane.notice.as_deref(), Some("Disconnected"));
    pane.device_displays[0].online = false;
    assert!(
        pane.apply(&DisplayEffect::MoveDeviceDisplay {
            display: "drag-peer/eDP-1".into(),
            revision: 9,
            x: 0,
            y: 0
        })
        .is_err()
    );
}

#[test]
fn peer_drag_confirmation_reports_owner_updates_competing_layouts_and_offline() {
    let _guard = crate::prefs::test_env::ConfigGuard::new();
    for outcome in 0..3 {
        let mut pane = laptop_with_monitor();
        pane.device_displays = vec![settings_store::device_displays::DeviceDisplay {
            device: "confirmation-peer".into(),
            device_label: "Peer".into(),
            connector: "eDP-1".into(),
            output_id: None,
            label: "Panel".into(),
            online: true,
            local: false,
            x: 1920,
            y: 0,
            width: 1280,
            height: 720,
            scale_milli: 1250,
            revision: 9,
        }];
        pane.apply(&DisplayEffect::MoveDeviceDisplay {
            display: "confirmation-peer/eDP-1".into(),
            revision: 9,
            x: 0,
            y: 1080,
        })
        .unwrap();
        let ui::widget::AppHostIntent::MoveDeviceDisplay { reply, .. } =
            pane.take_host_intent().unwrap()
        else {
            panic!("placement host request");
        };
        reply.upgrade().unwrap().borrow_mut().result = Some(Ok(()));
        pane.poll_device_move();
        assert!(
            pane.awaiting_device_move.is_some(),
            "dispatch is never reported as applied"
        );
        let screen = &mut pane.device_displays[0];
        screen.revision = 10;
        let expected = match outcome {
            0 => {
                screen.x = 0;
                screen.y = 1080;
                "display-device-move-confirmed"
            }
            1 => {
                screen.y = 20;
                "display-device-move-changed"
            }
            _ => {
                screen.online = false;
                "display-device-move-unavailable"
            }
        };
        pane.poll_device_move();
        assert!(pane.awaiting_device_move.is_none());
        assert_eq!(pane.notice, Some(t(expected)));
    }
}

#[test]
fn peer_drag_timeout_cancels_an_unadmitted_host_gesture() {
    let _guard = crate::prefs::test_env::ConfigGuard::new();
    let mut pane = laptop_with_monitor();
    pane.device_displays = vec![settings_store::device_displays::DeviceDisplay {
        device: "timeout-peer".into(),
        device_label: "Peer".into(),
        connector: "eDP-1".into(),
        output_id: None,
        label: "Panel".into(),
        online: true,
        local: false,
        x: 1920,
        y: 0,
        width: 1280,
        height: 720,
        scale_milli: 1250,
        revision: 9,
    }];
    pane.apply(&DisplayEffect::MoveDeviceDisplay {
        display: "timeout-peer/eDP-1".into(),
        revision: 9,
        x: 0,
        y: 1080,
    })
    .unwrap();
    let ui::widget::AppHostIntent::MoveDeviceDisplay { reply, .. } =
        pane.take_host_intent().unwrap()
    else {
        panic!("placement host request");
    };
    pane.awaiting_device_move.as_mut().unwrap().1 = ui::time::now()
        .checked_sub(Duration::from_secs(11))
        .unwrap();
    pane.poll_device_move();
    assert!(
        reply.upgrade().is_none(),
        "an expired gesture leaves no live admission witness"
    );
    assert!(pane.pending_device_move.is_none() && pane.awaiting_device_move.is_none());
    assert_eq!(pane.notice, Some(t("display-device-move-unconfirmed")));
    assert!(pane.take_host_intent().is_none());
}

#[test]
fn peer_drag_uses_the_mounted_app_semantic_door_and_fixed_revision() {
    let mut app = ui::framework::testing::TestApp::<crate::SettingsShell>::new().unwrap();
    app.call(
        "open",
        serde_json::json!({
            "anchor": crate::effects::pane_row_id(crate::categories::Category::Displays)
        }),
    )
    .unwrap();
    app.surface::<SettingsApp, _>(|settings| {
        crate::panes::display::area(settings)
            .unwrap()
            .model()
            .borrow_mut()
            .device_displays = vec![settings_store::device_displays::DeviceDisplay {
            device: "semantic-peer".into(),
            device_label: "Peer laptop".into(),
            connector: "eDP-1".into(),
            output_id: None,
            label: "Panel".into(),
            online: true,
            local: false,
            x: 1920,
            y: 0,
            width: 1280,
            height: 720,
            scale_milli: 1250,
            revision: 9,
        }];
    })
    .unwrap();
    app.settle();
    let address = "settings.device-display:semantic-peer/eDP-1";
    let surface = app.agent_surface();
    let row = surface
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["resource"] == address && row["action"] == "move-device-display")
        .expect("discovered screen exports its move action");
    assert_eq!(
        row["params"].as_array().unwrap().len(),
        2,
        "only logical x/y are caller-controlled"
    );
    assert!(
        app.call_on(
            address,
            "move-device-display",
            serde_json::json!({"x":0,"y":1080,"revision":1})
        )
        .is_err()
    );
    assert!(
        app.call_on(
            address,
            "move-device-display",
            serde_json::json!({"x":0,"y":1080,"display":"other/eDP-1"})
        )
        .is_err()
    );
    app.call_on(
        address,
        "move-device-display",
        serde_json::json!({"x":0,"y":1080}),
    )
    .unwrap();
    let intent = app.surface::<SettingsApp, _>(<SettingsApp as ui::framework::ResourceSurface<Settings, SettingsEffect>>::take_host_intent).flatten().expect("the real mounted surface carries the typed host intent");
    let ui::widget::AppHostIntent::MoveDeviceDisplay { request, .. } = intent else {
        panic!("typed topology request");
    };
    assert_eq!(request.screen.device, "semantic-peer");
    assert_eq!(request.screen.revision, 9);
    assert_eq!((request.x, request.y), (0, 1080));
    assert!(app.harness().component_contract_violations().is_empty());
}

#[test]
fn connected_display_controls_publish_their_parameter_actions() {
    let mut app = ui::framework::testing::TestApp::<crate::SettingsShell>::new()
        .expect("settings opens headlessly");
    app.settle();
    let open = crate::categories::Category::Displays;
    app.effect_on(
        &crate::effects::pane_row_id(open),
        &crate::effects::open_pane(open),
    )
    .expect("Displays opens through its declared operation");
    app.settle();
    let surface = app.agent_surface();
    let rows = surface.as_array().expect("published actions");
    for (action, field) in [
        ("set-display-mode", "mode"),
        ("set-output-scale", "scale"),
        ("rotate-display", "transform"),
        ("use-display", "enabled"),
        ("set-primary-display", "primary"),
        ("mirror-display", "mirror"),
    ] {
        assert!(
            rows.iter()
                .any(|row| row["resource"] == "settings.display-output:eDP-1"
                    && row["action"] == action),
            "{action} is published on the connected display: {surface}"
        );
        let bindings = app
            .surface::<SettingsApp, _>(|settings| {
                ui::widget::controls::control_action_bindings(&*settings)
            })
            .expect("mounted Settings");
        assert!(bindings.iter().any(|binding| matches!(&binding.kind,
            ui::widget::ControlBindingKind::Action(address)
                if address.resource.as_str() == "settings.display-output:eDP-1" && address.action == action)
            && binding.control_id == format!("display.output.eDP-1.{field}")),
            "{field} shares the published display operation: {bindings:#?}");
    }
    assert!(
        rows.iter()
            .any(|row| row["resource"] == "settings.display-output:eDP-1"
                && row["action"] == "place-display"),
        "place-display stays an action: {surface}"
    );
    let bindings = app
        .surface::<SettingsApp, _>(|settings| {
            ui::widget::controls::control_action_bindings(&*settings)
        })
        .expect("mounted Settings");
    assert!(
        !bindings
            .iter()
            .any(|binding| binding.control_id.ends_with(".position")),
        "no Position control: {bindings:#?}"
    );
    assert!(app.harness().component_contract_violations().is_empty());
}

/// Only the asked value is caller-controlled; the row owns the output identity.
#[test]
fn display_inspection_and_staging_share_the_production_door() {
    let mut app = ui::framework::testing::TestApp::<crate::SettingsShell>::new()
        .expect("settings opens headlessly");
    app.settle();
    let before = app
        .call("inspect-display-configuration", serde_json::json!({}))
        .expect("display observation is declared")
        .expect("observation returned");
    assert_eq!(before["has_staged_changes"], false);
    let mode = before["observed_outputs"][0]["supported_modes"][0].clone();
    // Read the declared choices instead of inventing a refresh-rate spelling.
    let surface = app.agent_surface();
    let row = surface
        .as_array()
        .unwrap()
        .iter()
        .find(|row| {
            row["resource"] == "settings.display-output:eDP-1"
                && row["action"] == "set-display-mode"
        })
        .expect("the connected output publishes mode selection");
    let choice = row["params"][0]["choices"][0]
        .as_str()
        .expect("actual mode choice");
    app.call_on(
        "settings.display-output:eDP-1",
        "set-display-mode",
        serde_json::json!({"mode":choice}),
    )
    .expect("the bound row stages its own output");
    app.call_on(
        "settings.display-output:eDP-1",
        "set-output-scale",
        serde_json::json!({"scale":"200%"}),
    )
    .expect("a real scale change stages through the same output resource");
    app.settle();
    let staged = app
        .call("inspect-display-configuration", serde_json::json!({}))
        .unwrap()
        .unwrap();
    assert_eq!(staged["has_staged_changes"], true);
    assert_eq!(
        staged["staged_configuration"]["outputs"]["eDP-1"]["scale"],
        2.0
    );
    assert_eq!(
        staged["staged_configuration"]["outputs"]["eDP-1"]["mode"],
        mode
    );
    assert_eq!(staged["observed_outputs"], before["observed_outputs"]);
    assert_eq!(staged["saved_configuration"], before["saved_configuration"]);
    assert!(
        app.call_on(
            "settings.display-output:eDP-1",
            "set-display-mode",
            serde_json::json!({"mode":choice,"display":"another-output"})
        )
        .is_err(),
        "a caller cannot replace the row's output identity"
    );
    assert!(
        app.call_on(
            "settings.display-output:eDP-1",
            "set-display-mode",
            serde_json::json!({"mode":"unsupported refresh rate"})
        )
        .is_err(),
        "unsupported modes are rejected by the published contract"
    );
    let after = app
        .call("inspect-display-configuration", serde_json::json!({}))
        .unwrap()
        .unwrap();
    assert_eq!(
        after, staged,
        "inspection and refused invocations cannot edit display state"
    );
}

#[test]
fn display_observation_is_available_to_viewers_without_staging_rights() {
    let mut app =
        ui::framework::testing::TestApp::<crate::SettingsShell>::with_role(ui::Role::Viewer)
            .expect("read-only Settings opens");
    app.settle();
    let before = app
        .call("inspect-display-configuration", serde_json::json!({}))
        .expect("read-only observer is available")
        .expect("observation returned");
    assert!(
        app.call_on(
            "settings.display-output:eDP-1",
            "set-output-scale",
            serde_json::json!({"scale":"200%"})
        )
        .is_err(),
        "inspection does not grant device configuration rights"
    );
    let after = app
        .call("inspect-display-configuration", serde_json::json!({}))
        .unwrap()
        .unwrap();
    assert_eq!(before, after);
}

#[test]
fn readonly_display_surface_keeps_inspection_and_has_no_stale_admin_controls() {
    for role in [ui::Role::Viewer, ui::Role::Editor] {
        let mut app =
            ui::framework::testing::TestApp::<crate::SettingsShell>::with_role(role).unwrap();
        let open = crate::categories::Category::Displays;
        app.effect_on(
            &crate::effects::pane_row_id(open),
            &crate::effects::open_pane(open),
        )
        .unwrap();
        app.settle();
        let audit = app.harness().native_app_audit();
        assert!(
            audit
                .issues
                .iter()
                .all(|issue| issue.code != "stale-action-binding"),
            "role {role:?}: stale control in the actual application and host doors: {:?}",
            audit.issues
        );
        assert!(
            app.call("inspect-display-configuration", serde_json::json!({}))
                .unwrap()
                .is_some()
        );
        for action in [
            "identify-displays",
            "apply-display-changes",
            "keep-display-changes",
            "revert-display-changes",
        ] {
            assert!(
                app.call(action, serde_json::json!({})).is_err(),
                "{role:?} cannot invoke {action}"
            );
        }
    }
}

#[test]
fn readonly_host_reply_replaces_stale_observation_without_staging_or_disk_writes() {
    let mut model = model(vec![classify_connector("eDP-1", "connected")]);
    let mode = OutputMode {
        width: 2944,
        height: 1840,
        refresh_mhz: 89999,
    };
    let observed = DisplayHardwareSnapshot {
        outputs: vec![settings_store::hardware::DisplayOutputSnapshot {
            descriptor: OutputDescriptor {
                connector: "eDP-1".into(),
                connector_label: "eDP-1".into(),
                modes: vec![mode],
                hdr: false,
                vrr: false,
                color_profiles: vec![],
                monitor: None,
            },
            current_mode: mode,
            scale: 2.0,
            builtin: true,
            sink_hdr: false,
            hdr_unavailable_reason: None,
            sink_vrr: false,
            vrr_unavailable_reason: None,
            unavailable_reason: None,
            color_profile_labels: BTreeMap::new(),
        }],
        ..DisplayHardwareSnapshot::default()
    };
    model.accept_host_reply(settings_store::display_control::DisplayReply {
        identify: None,
        observed,
        transaction: None,
        committed: DisplayConfig::default(),
    });
    let inspection = model.inspection();
    assert_eq!(
        inspection["observed_outputs"][0]["current_mode"]["refresh_mhz"],
        89999
    );
    assert_eq!(inspection["observed_outputs"][0]["scale"], 2.0);
    assert_eq!(inspection["has_staged_changes"], false);
    assert!(inspection["transaction"].is_null());
    assert!(model.pending_candidate.is_none());
}

fn model(outputs: Vec<DisplayOutput>) -> DisplayModel {
    let profile = tempfile::tempdir().unwrap().keep();
    DisplayModel::detached(PaneCx::in_file(profile.join("settings.json"))).with_outputs(outputs)
}

/// An output named after the DRM connector `conn` (`card0-eDP-1`, with the
/// `cardN-` prefix dropped); eDP/LVDS/DSI panels are built in.
fn classify_connector(conn: &str, status: &str) -> DisplayOutput {
    // Strip the leading `cardN-` so we're left with the connector, e.g.
    // `eDP-1`, `HDMI-A-1`, `DP-2`.
    let name = conn.split_once('-').map_or(conn, |(_, rest)| rest);
    let upper = name.to_ascii_uppercase();
    let builtin = upper.starts_with("EDP") || upper.starts_with("LVDS") || upper.starts_with("DSI");
    let label = if builtin {
        "Built-in display".to_string()
    } else {
        name.to_string()
    };
    DisplayOutput {
        label,
        connected: status.trim() == "connected",
        builtin,
        connector: name.to_string(),
        connector_label: name.to_string(),
        modes: Vec::new(),
        current_mode: None,
        scale: 1.0,
        hdr: false,
        sink_hdr: false,
        hdr_unavailable_reason: None,
        vrr: false,
        sink_vrr: false,
        vrr_unavailable_reason: None,
        unavailable_reason: None,
        color_profiles: Vec::new(),
        color_profile_labels: BTreeMap::new(),
        monitor: None,
    }
}

fn recording(outputs: Vec<DisplayOutput>) -> (DisplayModel, Arc<RecordingDispatcher>) {
    let rec = Arc::new(RecordingDispatcher::default());
    let mut model = model(outputs);
    model.brightness_control = Arc::new(SystemBrightness(
        Arc::clone(&rec) as Arc<dyn Dispatcher + Send + Sync>
    ));
    (model, rec)
}

fn panel() -> DisplayOutput {
    display_with_modes(
        "eDP-1",
        "Built-in display",
        true,
        vec![OutputMode {
            width: 1920,
            height: 1080,
            refresh_mhz: 60_000,
        }],
    )
}

/// A shown pane reads the compositor's display snapshot when its watch
/// says it changed, never on a timer; a pane whose snapshots cannot all
/// be watched, or whose change awaits confirmation, looks each second.
#[test]
fn followed_displays_are_read_on_a_change_not_on_a_timer() {
    let _config = crate::prefs::test_env::ConfigGuard::new();
    let runtime = tempfile::tempdir().unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("XDG_RUNTIME_DIR", runtime.path());
    let snapshot = settings_store::hardware::display_snapshot_path().unwrap();
    std::fs::create_dir_all(snapshot.parent().unwrap()).unwrap();
    let long_ago = || ui::time::now().checked_sub(Duration::from_secs(5)).unwrap();
    let mut pane = model(vec![panel()]);
    pane.of_device = true;
    let watched = Rc::new(crate::watch::Watched::by_hand());
    pane.following = Rc::downgrade(&watched);

    std::fs::write(&snapshot, "{}").unwrap();
    pane.last_snapshot_check = long_ago();
    assert!(
        pane.poll(),
        "the first look reads the snapshot that appeared"
    );
    assert_eq!(pane.next_wake(), None, "an idle pane sets no deadline");

    let read = pane.snapshot_stamp;
    assert!(read.is_some());
    std::fs::write(&snapshot, "{\"moved\":1}").unwrap();
    // A write within the file system's timestamp granularity keeps its
    // stamp; this one is a second later however fast it came.
    std::fs::File::options()
        .write(true)
        .open(&snapshot)
        .unwrap()
        .set_modified(read.unwrap() + Duration::from_secs(1))
        .unwrap();
    pane.last_snapshot_check = long_ago();
    assert!(!pane.poll(), "nothing is read without the watch's word");
    assert_eq!(pane.snapshot_stamp, read);

    watched.touch();
    assert!(pane.poll(), "the change is read once the watch reports it");
    assert_ne!(pane.snapshot_stamp, read);
    assert!(!pane.poll(), "and only once");

    pane.settling = true;
    assert!(
        pane.next_wake().is_some(),
        "a change awaiting confirmation counts down each second"
    );
    pane.settling = false;

    let unwatchable = Rc::new(crate::watch::Watched::files(&[runtime
        .path()
        .join("absent")
        .join("display-state.json")]));
    pane.following = Rc::downgrade(&unwatchable);
    assert!(
        pane.next_wake().is_some(),
        "snapshots that cannot be watched are read on the old timer"
    );
    drop(unwatchable);
    assert!(
        pane.next_wake().is_some(),
        "a pane nobody shows keeps its timer for reads an effect asks for"
    );
}

/// A brightness that could not be read is shown as that failure, not as
/// a slider resting at a made-up level; a probe that lands asks for a
/// rebuild once.
#[test]
fn the_brightness_probe_lands_once_and_a_failure_is_shown() {
    let mut model = model(vec![panel()]);
    let (probe_tx, probe_rx) = crate::channel::channel();
    model.hardware_probe = Some(probe_rx);
    probe_tx
        .send((Err("no backlight device".into()), false))
        .expect("probe receiver is alive");
    assert!(model.poll(), "the landed probe requests a rebuild");
    assert!(!model.poll(), "and reports it once");
    let text = crate::widgets::announced_text(model.build_composition().as_ref());
    assert!(
        text.iter()
            .any(|line| line == "Can't read the brightness: no backlight device"),
        "{text:#?}"
    );

    let mut model = self::model(vec![panel()]);
    let (probe_tx, probe_rx) = crate::channel::channel();
    model.hardware_probe = Some(probe_rx);
    probe_tx.send((Ok(0.8), true)).unwrap();
    assert!(model.poll());
    assert!((model.brightness - 0.8).abs() < f32::EPSILON);
    assert!(model.ambient_sensor_available);
}

#[test]
fn sdr_brightness_slider_round_trips_whole_nits_and_reasons_have_copy() {
    use settings_store::hardware::{AdaptiveSyncUnavailable, HdrUnavailable};
    for nits in [80, 203, 300, 480] {
        assert_eq!(sdr_brightness_nits(sdr_brightness_fraction(nits)), nits);
    }
    assert!((sdr_brightness_fraction(80) - 0.0).abs() < f64::EPSILON);
    assert!((sdr_brightness_fraction(480) - 1.0).abs() < f64::EPSILON);
    assert_eq!(
        sdr_brightness_nits(f64::NAN),
        80,
        "garbage lands on the dim end"
    );
    assert_eq!(sdr_brightness_nits(7.0), 480);
    let keys = [
        hdr_unavailable_key(false, None),
        hdr_unavailable_key(true, None),
        hdr_unavailable_key(true, Some(HdrUnavailable::DriverUnsupported)),
        hdr_unavailable_key(true, Some(HdrUnavailable::FormatUnsupported)),
        hdr_unavailable_key(true, Some(HdrUnavailable::DriverRejected)),
        adaptive_sync_unavailable_key(false, None),
        adaptive_sync_unavailable_key(true, Some(AdaptiveSyncUnavailable::DriverUnsupported)),
        adaptive_sync_unavailable_key(true, Some(AdaptiveSyncUnavailable::DriverRejected)),
    ];
    for key in keys {
        let copy = crate::i18n::t(key);
        assert_ne!(copy, key, "{key} has no en-US copy");
    }
}

#[test]
fn relative_positions_use_the_neighbour_s_logical_extent() {
    let mode = |width, height| {
        Some(OutputMode {
            width,
            height,
            refresh_mhz: 60_000,
        })
    };
    let panel = DisplayOutput {
        current_mode: mode(2560, 1600),
        ..classify_connector("card0-eDP-1", "connected")
    };
    let monitor = DisplayOutput {
        current_mode: mode(1920, 1080),
        ..classify_connector("card0-HDMI-A-1", "connected")
    };
    let connected = [panel, monitor];
    let mut next = DisplayConfig::default();
    next.outputs.insert(
        "eDP-1".into(),
        OutputConfig {
            scale: 2.0,
            ..Default::default()
        },
    );
    let place = |label: &str, cfg: &mut OutputConfig| {
        place_beside("HDMI-A-1", label, cfg, &next, &connected)
    };

    // The 2x panel is 1280 logical pixels wide, so its right-hand
    // neighbour starts there — not at its 2560-pixel mode width.
    let mut cfg = OutputConfig::default();
    place("Right of Built-in display", &mut cfg).unwrap();
    assert_eq!((cfg.x, cfg.y), (1280, 0));
    place("Below of Built-in display", &mut cfg).unwrap();
    assert_eq!((cfg.x, cfg.y), (0, 800));

    // A monitor rotated to portrait is 1080 wide on the left.
    let mut portrait = OutputConfig {
        transform: OutputTransform::Rotate90,
        ..Default::default()
    };
    place("Left of Built-in display", &mut portrait).unwrap();
    assert_eq!((portrait.x, portrait.y), (-1080, 0));
    assert!(place("Left of Nowhere", &mut portrait).is_err());
}

#[test]
fn identical_monitors_are_named_apart_by_number_in_settings() {
    let first = OutputDescriptor {
        connector: "pci:0000:01:00.0/HDMI-A-1".into(),
        connector_label: "HDMI-A-1".into(),
        modes: vec![],
        hdr: false,
        vrr: false,
        color_profiles: vec![],
        monitor: Some(MonitorIdentity {
            make: "Dell".into(),
            model: "DELL U2720Q".into(),
            ..MonitorIdentity::default()
        }),
    };
    let second = OutputDescriptor {
        connector: "pci:0000:03:00.0/HDMI-A-1".into(),
        ..first.clone()
    };
    let observed =
        |descriptor: &OutputDescriptor| settings_store::hardware::DisplayOutputSnapshot {
            descriptor: descriptor.clone(),
            current_mode: OutputMode {
                width: 3840,
                height: 2160,
                refresh_mhz: 60_000,
            },
            scale: 1.0,
            builtin: false,
            sink_hdr: false,
            hdr_unavailable_reason: None,
            sink_vrr: false,
            vrr_unavailable_reason: None,
            unavailable_reason: None,
            color_profile_labels: BTreeMap::new(),
        };
    let snapshot = DisplayHardwareSnapshot {
        outputs: vec![observed(&first), observed(&second)],
        ..DisplayHardwareSnapshot::default()
    };
    let labels: Vec<_> = outputs_from_snapshot(&snapshot)
        .into_iter()
        .map(|output| output.label)
        .collect();
    assert_eq!(labels, ["Dell U2720Q (1)", "Dell U2720Q (2)"]);
    let alone = DisplayHardwareSnapshot {
        outputs: vec![observed(&first)],
        ..DisplayHardwareSnapshot::default()
    };
    assert_eq!(outputs_from_snapshot(&alone)[0].label, "Dell U2720Q");
}

/// Distinct duplicate labels, staging per connector and virtual
/// connector filtering.
#[test]
fn display_connectors_are_labelled_and_filtered() {
    // Case `qualified_duplicate_connectors_stage_apart`.
    {
        let first = OutputDescriptor {
            connector: "pci:0000:01:00.0/HDMI-A-1".into(),
            connector_label: "HDMI-A-1".into(),
            modes: vec![],
            hdr: false,
            vrr: false,
            color_profiles: vec![],
            monitor: None,
        };
        let display = DisplayOutput {
            label: "Dell U2720Q (1)".into(),
            connector: first.connector.clone(),
            connector_label: first.label().to_string(),
            ..classify_connector("card0-HDMI-A-1", "connected")
        };
        let staged = DisplayModel::output_descriptor(&display);
        assert_eq!(staged.connector, "pci:0000:01:00.0/HDMI-A-1");
        assert_eq!(
            staged.connector_label, "HDMI-A-1",
            "the card qualifier is UI-only, never folded into descriptor data"
        );
    }

    // Case `duplicate_connectors_are_staged_apart_and_keep_the_old_key`.
    {
        let first = "pci:0000:01:00.0/HDMI-A-1";
        let second = "pci:0000:03:00.0/HDMI-A-1";
        let mut pane = model(vec![
            DisplayOutput {
                label: "HDMI-A-1 on pci:0000:01:00.0".into(),
                connector: first.into(),
                ..classify_connector("card0-HDMI-A-1", "connected")
            },
            DisplayOutput {
                label: "HDMI-A-1 on pci:0000:03:00.0".into(),
                connector: second.into(),
                ..classify_connector("card1-HDMI-A-1", "connected")
            },
        ]);
        pane.output_config.outputs.insert(
            "HDMI-A-1".into(),
            OutputConfig {
                enabled: false,
                ..Default::default()
            },
        );
        pane.apply(&DisplayEffect::SetPrimaryDisplay {
            display: second.into(),
        })
        .unwrap();
        let staged = pane.pending_candidate.as_ref().unwrap();
        assert!(!staged.outputs["HDMI-A-1"].enabled, "the old key is kept");
        assert!(staged.outputs[second].primary);
        assert!(!staged.outputs[first].primary);
    }

    // Case `virtual_connectors_are_filtered`.
    // The compositor's writeback connector and other virtual connectors are
    // not displays a user plugs into, so they are filtered from the list.
    {
        assert!(is_virtual_connector("card0-Writeback-1"));
        assert!(is_virtual_connector("card0-Virtual-1"));
        assert!(!is_virtual_connector("card0-eDP-1"));
        assert!(!is_virtual_connector("card0-HDMI-A-1"));
        assert!(!is_virtual_connector("card0-DP-2"));
        assert!(!is_virtual_connector("card0-USB-1"));
    }
}

/// The panel's slider sets its brightness; an external monitor's sets
/// that monitor's, and only a monitor that has a control gets one.
#[test]
fn brightness_reaches_the_panel_and_each_monitor() {
    let (mut pane, rec) = recording(vec![panel()]);
    pane.apply(&DisplayEffect::SetBrightness { brightness: 0.75 })
        .unwrap();
    assert_eq!(rec.calls(), ["set_brightness(75.00)"]);
    assert!((pane.brightness - 0.75).abs() < f32::EPSILON);

    let level = |connector: &str, level: f32| os_actions::MonitorLevel {
        connector: connector.into(),
        level,
        kind: if connector == "eDP-1" {
            os_actions::BrightnessKind::Backlight
        } else {
            os_actions::BrightnessKind::Ddc
        },
    };
    let (mut pane, rec) = recording(vec![
        display_with_modes("eDP-1", "Built-in display", true, Vec::new()),
        display_with_modes("DP-2", "Dell U2720Q", false, Vec::new()),
        display_with_modes("DP-3", "LG 27GL850", false, Vec::new()),
    ]);
    pane.output_brightness =
        output_brightness_from(&pane.outputs, &[level("eDP-1", 0.5), level("DP-2", 0.3)]);
    assert_eq!(
        pane.output_brightness.keys().collect::<Vec<_>>(),
        ["DP-2"],
        "the panel keeps the Brightness card; a monitor without DDC/CI gets no slider"
    );
    let dimmed = dimmed_by_tilo_from(
        &pane.outputs,
        &[os_actions::MonitorLevel {
            connector: "DP-3".into(),
            level: 1.0,
            kind: os_actions::BrightnessKind::Software,
        }],
    );
    assert_eq!(
        dimmed.into_iter().collect::<Vec<_>>(),
        ["DP-3"],
        "a monitor the compositor dims says so beside its slider"
    );
    pane.apply(&DisplayEffect::SetMonitorBrightness {
        display: "DP-2".into(),
        brightness: 0.8,
    })
    .unwrap();
    assert!(
        rec.calls()
            .contains(&"set_display_brightness(DP-2, 80.00)".to_string()),
        "{:?}",
        rec.calls()
    );
    assert!(
        pane.apply(&DisplayEffect::SetMonitorBrightness {
            display: "DP-3".into(),
            brightness: 0.8,
        })
        .is_err()
    );
}

/// Mirroring picks a shared mode; extending again keeps the origin.
#[test]
fn mirroring_and_extending_are_staged() {
    let full_hd_60 = OutputMode {
        width: 1920,
        height: 1080,
        refresh_mhz: 60_000,
    };
    let full_hd_5994 = OutputMode {
        refresh_mhz: 59_940,
        ..full_hd_60
    };
    let panel = display_with_modes("eDP-1", "Built-in display", true, vec![full_hd_60]);
    let usb_c = display_with_modes("DP-1", "USB-C display", false, vec![full_hd_5994]);
    assert_eq!(
        best_mirror_modes(&usb_c, &panel),
        Some((full_hd_5994, full_hd_60)),
        "software mirroring needs a shared extent, not identical connector clocks"
    );

    let mut pane = model(vec![panel, usb_c]);
    pane.output_config.outputs.insert(
        "eDP-1".into(),
        OutputConfig {
            primary: true,
            mode: Some(full_hd_60),
            ..Default::default()
        },
    );
    pane.output_config.outputs.insert(
        "DP-1".into(),
        OutputConfig {
            x: 1920,
            mode: Some(full_hd_5994),
            ..Default::default()
        },
    );
    pane.apply(&DisplayEffect::MirrorDisplay {
        display: "DP-1".into(),
        mirror: "Mirror Built-in display".into(),
    })
    .unwrap();
    let mirrored = pane.pending_candidate.clone().expect("mirror plan staged");
    assert_eq!(mirrored.outputs["DP-1"].mirror_of.as_deref(), Some("eDP-1"));
    assert_eq!(mirrored.outputs["DP-1"].mode, Some(full_hd_5994));
    assert_eq!(mirrored.outputs["eDP-1"].mode, Some(full_hd_60));

    pane.output_config = mirrored;
    pane.pending_candidate = None;
    pane.apply(&DisplayEffect::MirrorDisplay {
        display: "DP-1".into(),
        mirror: "Extend workspace".into(),
    })
    .unwrap();
    let extended = pane.pending_candidate.as_ref().expect("extend plan staged");
    assert_eq!(extended.outputs["DP-1"].mirror_of, None);
    assert_eq!(
        (extended.outputs["DP-1"].x, extended.outputs["DP-1"].y),
        (1920, 0)
    );

    let shown = model(vec![
        self::panel(),
        DisplayOutput {
            connected: false,
            ..display_with_modes("DP-2", "DP-2", false, Vec::new())
        },
    ]);
    assert_eq!(
        shown.connected_outputs().len(),
        1,
        "idle ports are not listed"
    );
}

fn display_with_modes(
    connector: &str,
    label: &str,
    builtin: bool,
    modes: Vec<OutputMode>,
) -> DisplayOutput {
    DisplayOutput {
        label: label.into(),
        connected: true,
        builtin,
        connector: connector.into(),
        connector_label: connector.into(),
        current_mode: modes.first().copied(),
        modes,
        scale: 1.0,
        hdr: false,
        sink_hdr: false,
        hdr_unavailable_reason: None,
        vrr: false,
        sink_vrr: false,
        vrr_unavailable_reason: None,
        unavailable_reason: None,
        color_profiles: Vec::new(),
        color_profile_labels: BTreeMap::new(),
        monitor: None,
    }
}

#[test]
fn preview_and_keep_preserve_the_staged_refresh_rate() {
    let _guard = crate::prefs::test_env::ConfigGuard::new();
    let current = OutputMode {
        width: 2944,
        height: 1840,
        refresh_mhz: 59_999,
    };
    let requested = OutputMode {
        refresh_mhz: 89_999,
        ..current
    };
    let mut pane = model(vec![display_with_modes(
        "eDP-1",
        "Built-in display",
        true,
        vec![current, requested],
    )]);
    pane.apply(&DisplayEffect::SetDisplayMode {
        display: "eDP-1".into(),
        mode: requested.label(),
    })
    .unwrap();
    pane.apply(&DisplayEffect::ApplyDisplayChanges).unwrap();
    let mut transaction = DisplayTransaction::load().expect("Apply requests a preview");
    assert_eq!(transaction.state, DisplayTransactionState::Requested);
    assert_eq!(transaction.candidate.outputs["eDP-1"].mode, Some(requested));
    let preview_id = transaction.id;
    pane.settle(&transaction);
    assert_eq!(pane.staged().outputs["eDP-1"].mode, Some(requested));

    transaction.state = DisplayTransactionState::AwaitingConfirmation;
    transaction.deadline_unix_ms = Some(unix_now_ms() + 15_000);
    transaction.save().unwrap();
    pane.settle(&transaction);
    pane.apply(&DisplayEffect::KeepDisplayChanges).unwrap();
    let mut kept = DisplayTransaction::load().unwrap();
    assert_eq!(kept.id, preview_id, "Keep confirms the original preview");
    assert_eq!(kept.state, DisplayTransactionState::ConfirmRequested);
    assert_eq!(kept.candidate.outputs["eDP-1"].mode, Some(requested));
    pane.settle(&kept);
    assert_eq!(pane.staged().outputs["eDP-1"].mode, Some(requested));

    // The compositor commits the kept arrangement before acknowledging it.
    kept.candidate.save_for(&kept.set).unwrap();
    kept.state = DisplayTransactionState::Confirmed;
    kept.deadline_unix_ms = None;
    kept.save().unwrap();
    pane.settle(&DisplayTransaction::load().unwrap());
    assert!(pane.pending_candidate.is_none());
    assert_eq!(pane.staged().outputs["eDP-1"].mode, Some(requested));
}

#[test]
fn display_scale_and_plans_persist_confirm_and_revert() {
    for (label, factor) in DISPLAY_SCALES {
        assert_eq!(scale_label(*factor), *label);
        assert_eq!(scale_factor_of(label), Some(*factor));
    }
    assert_eq!(scale_label(1.48), "150%");
    assert_eq!(scale_factor_of("bogus"), None);

    let _guard = crate::prefs::test_env::ConfigGuard::new();
    let mut pane = model(vec![panel()]);
    pane.cx
        .settings::<DisplayPrefs>()
        .update(|prefs| prefs.scale = 1.5)
        .unwrap();
    assert!((pane.cx.settings::<DisplayPrefs>().get().scale - 1.5).abs() < 1e-9);

    let mut committed = DisplayConfig::default();
    committed.outputs.insert(
        "eDP-1".into(),
        OutputConfig {
            primary: true,
            mode: pane.outputs[0].current_mode,
            ..Default::default()
        },
    );
    let set = pane.display_set();
    committed.save_for(&set).unwrap();
    pane.output_config = committed.clone();
    pane.apply(&DisplayEffect::RotateDisplay {
        display: "eDP-1".into(),
        rotation: "Portrait right".into(),
    })
    .unwrap();
    assert_eq!(
        pane.pending_candidate.as_ref().unwrap().outputs["eDP-1"].transform,
        OutputTransform::Rotate90
    );
    assert_eq!(
        DisplayConfig::load_for(&set),
        committed,
        "editing is only staged"
    );

    pane.apply(&DisplayEffect::ApplyDisplayChanges).unwrap();
    let mut transaction = DisplayTransaction::load().expect("Apply wrote a transaction");
    assert_eq!(transaction.state, DisplayTransactionState::Requested);
    assert_eq!(
        transaction.set, set,
        "the plan is for the monitors it was arranged on"
    );
    transaction.state = DisplayTransactionState::AwaitingConfirmation;
    transaction.deadline_unix_ms = Some(unix_now_ms() + 15_000);
    transaction.save().unwrap();
    pane.apply(&DisplayEffect::KeepDisplayChanges).unwrap();
    assert_eq!(
        DisplayTransaction::load().unwrap().state,
        DisplayTransactionState::ConfirmRequested
    );

    transaction.state = DisplayTransactionState::Reverted;
    transaction.deadline_unix_ms = None;
    transaction.save().unwrap();
    pane.settle(&DisplayTransaction::load().unwrap());
    assert!(pane.pending_candidate.is_none());
    assert_eq!(
        pane.staged().outputs["eDP-1"].transform,
        OutputTransform::Normal,
        "revert shows the last known-good arrangement, not the failed plan"
    );
}

fn laptop_with_monitor() -> DisplayModel {
    let full_hd = OutputMode {
        width: 1920,
        height: 1080,
        refresh_mhz: 60_000,
    };
    let qhd = OutputMode {
        width: 2560,
        height: 1440,
        refresh_mhz: 60_000,
    };
    let mut pane = model(vec![
        display_with_modes("HDMI-A-1", "Desk monitor", false, vec![qhd]),
        display_with_modes("eDP-1", "Built-in display", true, vec![full_hd]),
    ]);
    pane.output_config.outputs.insert(
        "eDP-1".into(),
        OutputConfig {
            primary: true,
            ..Default::default()
        },
    );
    pane.output_config.outputs.insert(
        "HDMI-A-1".into(),
        OutputConfig {
            x: 1920,
            ..Default::default()
        },
    );
    pane
}

#[test]
fn the_arrangement_numbers_and_sizes_displays_like_identify() {
    let pane = laptop_with_monitor();
    let connected = pane.connected_outputs();
    let displays = pane.arranged_displays(&connected);
    let panel = displays.iter().find(|d| d.connector == "eDP-1").unwrap();
    let monitor = displays.iter().find(|d| d.connector == "HDMI-A-1").unwrap();
    assert_eq!((panel.number, monitor.number), (1, 2));
    assert!(panel.primary && !monitor.primary);
    assert_eq!(
        monitor.placement,
        super::super::display_arrangement::placement::Placement::new(1920, 0, 2560, 1440)
    );
}

#[test]
fn the_arrangement_draws_mixed_dpi_displays_at_their_logical_size() {
    use super::super::display_arrangement::placement::{Placement, settle};
    // A 2x panel of 2560x1600 is 1280x800 on the desktop the pointer
    // moves through; a 1x monitor beside it starts at x = 1280.
    let panel_mode = OutputMode {
        width: 2560,
        height: 1600,
        refresh_mhz: 60_000,
    };
    let monitor_mode = OutputMode {
        width: 1920,
        height: 1080,
        refresh_mhz: 60_000,
    };
    let mut pane = model(vec![
        display_with_modes("eDP-1", "Built-in display", true, vec![panel_mode]),
        display_with_modes("HDMI-A-1", "Desk monitor", false, vec![monitor_mode]),
    ]);
    pane.output_config.outputs.insert(
        "eDP-1".into(),
        OutputConfig {
            primary: true,
            scale: 2.0,
            ..Default::default()
        },
    );
    pane.output_config.outputs.insert(
        "HDMI-A-1".into(),
        OutputConfig {
            x: 1280,
            ..Default::default()
        },
    );
    let connected = pane.connected_outputs();
    let displays = pane.arranged_displays(&connected);
    let panel = displays.iter().find(|d| d.connector == "eDP-1").unwrap();
    let monitor = displays.iter().find(|d| d.connector == "HDMI-A-1").unwrap();
    assert_eq!(panel.placement, Placement::new(0, 0, 1280, 800));
    assert_eq!(monitor.placement, Placement::new(1280, 0, 1920, 1080));
    assert!(
        panel.placement.touches(monitor.placement),
        "the canvas shows the edge the pointer crosses"
    );
    // Dropping the monitor near the panel's right edge snaps it onto
    // that logical edge, never onto the panel's mode width.
    let dropped = settle(monitor.placement.at(1300, 12), &[panel.placement], 24);
    assert_eq!((dropped.x, dropped.y), (1280, 0));
}

#[test]
fn an_unconfigured_display_is_drawn_and_staged_at_the_compositors_scale() {
    // Nothing saved: the compositor runs the HiDPI panel at the 2x it
    // chose itself, so the canvas draws it 1280 wide and staging a
    // change keeps that scale rather than writing 1x.
    let mut panel = display_with_modes(
        "eDP-1",
        "Built-in display",
        true,
        vec![OutputMode {
            width: 2560,
            height: 1600,
            refresh_mhz: 60_000,
        }],
    );
    panel.scale = 2.0;
    let mut pane = model(vec![panel]);
    let connected = pane.connected_outputs();
    let displays = pane.arranged_displays(&connected);
    assert_eq!(displays[0].placement.width, 1280);
    assert_eq!(displays[0].placement.height, 800);
    pane.apply(&DisplayEffect::RotateDisplay {
        display: "eDP-1".into(),
        rotation: "Landscape".into(),
    })
    .unwrap();
    assert!(
        (pane.staged().outputs["eDP-1"].scale - 2.0).abs() < f64::EPSILON,
        "staging keeps the scale the display runs at"
    );
}

#[test]
fn a_dropped_display_stages_its_exact_position() {
    let _guard = crate::prefs::test_env::ConfigGuard::new();
    let mut pane = laptop_with_monitor();
    pane.apply(&DisplayEffect::MoveDisplay {
        display: "HDMI-A-1".into(),
        x: -2560,
        y: -360,
    })
    .unwrap();
    let staged = pane.pending_candidate.as_ref().expect("the drop is staged");
    assert_eq!(
        (staged.outputs["HDMI-A-1"].x, staged.outputs["HDMI-A-1"].y),
        (-2560, -360)
    );
    assert!(
        DisplayTransaction::load().is_none(),
        "a drop is staged like every other edit, not applied"
    );
    pane.apply(&DisplayEffect::PlaceDisplay {
        display: "HDMI-A-1".into(),
        position: "Right of Built-in display".into(),
    })
    .unwrap();
    let staged = pane.pending_candidate.as_ref().unwrap();
    assert_eq!(
        (staged.outputs["HDMI-A-1"].x, staged.outputs["HDMI-A-1"].y),
        (1920, 0)
    );
}

#[test]
fn identify_asks_the_compositor_to_number_the_displays() {
    let _guard = crate::prefs::test_env::ConfigGuard::new();
    let mut pane = laptop_with_monitor();
    assert!(settings_store::display_identify::IdentifyRequest::load().is_none());
    let before = unix_now_ms();
    pane.apply(&DisplayEffect::IdentifyDisplays).unwrap();
    let after = unix_now_ms();
    let request = settings_store::display_identify::IdentifyRequest::load()
        .expect("Identify writes the request the compositor polls");
    assert!((before..=after).contains(&request.requested_unix_ms));
    assert_eq!(
        request.remaining_ms(request.requested_unix_ms),
        Some(settings_store::display_identify::IDENTIFY_MS)
    );
    let (shown, until) = pane
        .identify_window
        .expect("the canvas shows the numbers in the accent while they show");
    assert_eq!(
        until - shown,
        Duration::from_millis(settings_store::display_identify::IDENTIFY_MS)
    );
}

#[test]
fn a_trusted_display_tool_is_listed_and_can_be_asked_again() {
    use settings_store::display_tools::{DisplayTool, TrustedDisplayTools};
    let _guard = crate::prefs::test_env::ConfigGuard::new();
    let mut pane = laptop_with_monitor();
    pane.of_device = true;
    assert!(pane.build_trusted_tools_card().is_none());
    let kanshi = DisplayTool::new("/usr/bin/kanshi");
    TrustedDisplayTools::trust(kanshi.clone()).expect("trust");
    assert!(pane.build_trusted_tools_card().is_some());
    pane.apply(&DisplayEffect::ForgetDisplayTool {
        tool: kanshi.executable.clone(),
    })
    .unwrap();
    assert!(!TrustedDisplayTools::load().contains(&kanshi));
    assert!(pane.build_trusted_tools_card().is_none());
}
#[test]
fn peer_displays_are_inspectable_accessible_and_not_an_input_grant() {
    fn accessible_names(widget: &dyn Widget, names: &mut Vec<String>) {
        fn collect(node: &ui::a11y::AccessNode, names: &mut Vec<String>) {
            if let Some(name) = &node.name {
                names.push(name.clone());
            }
            for child in &node.children {
                collect(child, names);
            }
        }
        if let Some(node) = widget.accessibility() {
            collect(&node, names);
        }
        for child in widget.children() {
            accessible_names(child, names);
        }
    }
    let _guard = crate::prefs::test_env::ConfigGuard::new();
    let mut pane = laptop_with_monitor();
    assert!(pane.build_device_arrangement_card().is_none());
    pane.device_displays = vec![settings_store::device_displays::DeviceDisplay {
        device: "certified-peer".into(),
        device_label: "Peer laptop".into(),
        connector: "eDP-1".into(),
        output_id: None,
        label: "Panel".into(),
        online: false,
        local: false,
        x: 1920,
        y: 0,
        width: 1280,
        height: 720,
        scale_milli: 1250,
        revision: 1,
    }];
    let card = pane
        .build_device_arrangement_card()
        .expect("discovered peer has a real canvas");
    let mut names = Vec::new();
    let arrangement = crate::widgets::page(vec![card]);
    accessible_names(arrangement.as_ref(), &mut names);
    assert!(
        names
            .iter()
            .any(|name| name.contains("Peer laptop") && name.contains("Offline")),
        "assistive users receive actual peer and offline state"
    );
    let inspection = pane.inspection();
    assert_eq!(inspection["device_displays"][0]["device"], "certified-peer");
    assert_eq!(inspection["device_displays"][0]["scale_milli"], 1250);
    assert!(
        pane.apply(&DisplayEffect::MoveDisplay {
            display: "certified-peer/eDP-1".into(),
            x: 0,
            y: 0,
        })
        .is_err(),
        "read-only discovery cannot edit local outputs or grant input"
    );
}

fn test_ids(widget: &dyn Widget, ids: &mut Vec<String>) {
    if let Some(id) = widget.test_id() {
        ids.push(id.to_owned());
    }
    for child in widget.children() {
        test_ids(child, ids);
    }
}

fn page_ids(pane: &DisplayModel) -> Vec<String> {
    let mut ids = Vec::new();
    test_ids(pane.compose().as_ref(), &mut ids);
    ids
}

/// Where the tile named `name` is drawn on the mounted canvas.
fn tile(host: &ui::headless::FrameHarness, name: &str) -> ui::vello::kurbo::Rect {
    let node = host
        .root::<super::super::display_arrangement::DisplayArrangement>()
        .accessibility()
        .expect("the canvas describes its tiles");
    let child = node
        .children
        .iter()
        .find(|child| {
            child
                .name
                .as_deref()
                .is_some_and(|label| label.contains(name))
        })
        .unwrap_or_else(|| panic!("no tile named {name}"));
    let bounds = child.bounds.expect("a tile has bounds");
    ui::vello::kurbo::Rect::new(bounds.x0, bounds.y0, bounds.x1, bounds.y1)
}

#[test]
fn the_canvas_is_the_one_display_selection() {
    let _guard = crate::prefs::test_env::ConfigGuard::new();
    let mut pane = laptop_with_monitor();
    pane.normalize_selection();
    assert_eq!(
        pane.selected_output()
            .map(|output| output.connector.as_str()),
        Some("eDP-1"),
        "the main display is selected when the page opens"
    );
    // Chosen from the keyboard or by a screen reader: the canvas sets the
    // page's selection.
    pane.selected.set("HDMI-A-1".into());
    assert!(pane.poll(), "the page shows the newly chosen display");
    assert_eq!(
        pane.selected_output()
            .map(|output| output.connector.as_str()),
        Some("HDMI-A-1")
    );
    let connected = pane.connected_outputs();
    let canvas = pane.arrangement_canvas(&connected);
    let tiles = canvas.accessibility().unwrap().children;
    let chosen: Vec<_> = tiles
        .iter()
        .filter(|tile| tile.state.selected)
        .filter_map(|tile| tile.name.clone())
        .collect();
    assert_eq!(chosen.len(), 1, "one tile is chosen: {chosen:?}");
    assert!(chosen[0].contains("Desk monitor"), "{chosen:?}");
    drop(connected);

    // Chosen on the canvas by pointer.
    let connected = pane.connected_outputs();
    let canvas = pane.arrangement_canvas(&connected);
    drop(connected);
    let mut host = ui::headless::FrameHarness::for_root(
        ui::State::new(),
        canvas,
        (800, 400),
        ui::theme::Theme::dark(),
    );
    host.profile_frame();
    let panel = tile(&host, "Built-in display");
    host.send(&ui::synthetic_input::cursor_moved(
        panel.center().x,
        panel.center().y,
    ));
    host.send(&ui::synthetic_input::mouse_press());
    host.send(&ui::synthetic_input::mouse_release());
    assert!(
        pane.poll(),
        "a press on the canvas is a choice the page shows"
    );
    assert_eq!(
        pane.selected_output()
            .map(|output| output.connector.as_str()),
        Some("eDP-1")
    );
    let ids = page_ids(&pane);
    assert!(
        !ids.iter().any(|id| id == "display.picker"),
        "no second place to choose: {ids:?}"
    );
}

#[test]
fn only_the_selected_displays_settings_show() {
    let _guard = crate::prefs::test_env::ConfigGuard::new();
    let mut pane = laptop_with_monitor();
    pane.normalize_selection();
    let ids = page_ids(&pane);
    let of = |ids: &[String], display: &str| {
        ids.iter()
            .filter(|id| id.starts_with(&format!("display.output.{display}.")))
            .count()
    };
    assert!(of(&ids, "eDP-1") > 0, "the panel's settings: {ids:?}");
    assert_eq!(of(&ids, "HDMI-A-1"), 0, "never the monitor's beside them");
    assert_eq!(
        ids.iter().filter(|id| *id == "display.brightness").count(),
        1,
        "the panel's brightness, once, in its settings"
    );
    assert!(ids.iter().any(|id| id == "display.arrangement"));

    // The monitor has no brightness control: no slider at all.
    pane.selected.set("HDMI-A-1".into());
    pane.poll();
    let ids = page_ids(&pane);
    assert!(of(&ids, "HDMI-A-1") > 0);
    assert_eq!(of(&ids, "eDP-1"), 0);
    assert!(
        !ids.iter()
            .any(|id| id.ends_with("brightness") && !id.contains("sdr")),
        "no brightness for a display that cannot set it: {ids:?}"
    );

    // With a control it gets its own, still only in its settings.
    pane.output_brightness.insert("HDMI-A-1".into(), 0.4);
    let ids = page_ids(&pane);
    assert_eq!(
        ids.iter()
            .filter(|id| id.ends_with("brightness") && !id.contains("sdr"))
            .collect::<Vec<_>>(),
        ["display.output.HDMI-A-1.brightness"]
    );
}

#[test]
fn a_virtual_or_disconnected_connector_appears_nowhere() {
    let _guard = crate::prefs::test_env::ConfigGuard::new();
    let mut pane = laptop_with_monitor();
    pane.outputs
        .push(classify_connector("card0-HDMI-A-2", "disconnected"));
    // The brightness service also knows a development VM's virtual
    // connector; the compositor shows no such display.
    let level = |connector: &str| os_actions::MonitorLevel {
        connector: connector.into(),
        level: 0.5,
        kind: os_actions::BrightnessKind::Ddc,
    };
    pane.output_brightness =
        output_brightness_from(&pane.outputs, &[level("Virtual-1"), level("HDMI-A-2")]);
    assert!(pane.output_brightness.is_empty());
    pane.normalize_selection();
    let connected = pane.connected_outputs();
    let tiles: Vec<_> = pane
        .arranged_displays(&connected)
        .into_iter()
        .map(|display| display.connector)
        .collect();
    assert_eq!(tiles.len(), 2);
    assert!(
        tiles
            .iter()
            .all(|tile| tile == "eDP-1" || tile == "HDMI-A-1")
    );
    drop(connected);
    pane.selected.set("HDMI-A-2".into());
    pane.poll();
    assert_ne!(
        pane.selected_output()
            .map(|output| output.connector.as_str()),
        Some("HDMI-A-2"),
        "a disconnected port is never selected"
    );
    let ids = page_ids(&pane);
    assert!(
        !ids.iter()
            .any(|id| id.contains("HDMI-A-2") || id.contains("Virtual")),
        "{ids:?}"
    );
}

#[test]
fn pointing_at_or_choosing_a_display_flashes_its_number_on_it() {
    let _guard = crate::prefs::test_env::ConfigGuard::new();
    let mut pane = laptop_with_monitor();
    pane.normalize_selection();
    assert_eq!(
        pane.last_flash, None,
        "nothing flashes for the page opening"
    );
    pane.pointed.set(Some("HDMI-A-1".into()));
    pane.poll();
    assert_eq!(pane.last_flash.as_deref(), Some("HDMI-A-1"));
    pane.selected.set("eDP-1".into());
    pane.poll();
    assert_eq!(pane.last_flash.as_deref(), Some("eDP-1"));
}

#[test]
fn arrangement_scale_and_rotation_apply_at_once_a_mode_asks_first() {
    let _guard = crate::prefs::test_env::ConfigGuard::new();
    let mut pane = laptop_with_monitor();
    for effect in [
        DisplayEffect::MoveDisplay {
            display: "HDMI-A-1".into(),
            x: 1920,
            y: 120,
        },
        DisplayEffect::SetOutputScale {
            display: "HDMI-A-1".into(),
            scale: "125%".into(),
        },
        DisplayEffect::RotateDisplay {
            display: "HDMI-A-1".into(),
            rotation: "Portrait right".into(),
        },
    ] {
        pane.pending_candidate = None;
        pane.apply(&effect).unwrap();
        assert!(pane.staged_applies_at_once(), "{effect:?} applies at once");
    }
    pane.pending_candidate = None;
    let full_hd = OutputMode {
        width: 1920,
        height: 1080,
        refresh_mhz: 60_000,
    };
    pane.outputs[0].modes.push(full_hd);
    pane.apply(&DisplayEffect::SetDisplayMode {
        display: "HDMI-A-1".into(),
        mode: full_hd.label(),
    })
    .unwrap();
    assert!(!pane.staged_applies_at_once(), "a new mode is tried first");
}

#[test]
fn resolution_and_refresh_rate_each_choose_one_mode() {
    let mode = |width, height, refresh_mhz| OutputMode {
        width,
        height,
        refresh_mhz,
    };
    let modes = [
        mode(2560, 1440, 144_000),
        mode(2560, 1440, 59_940),
        mode(1920, 1080, 144_000),
        mode(1920, 1080, 60_000),
    ];
    assert_eq!(resolution_choices(&modes), ["2560 × 1440", "1920 × 1080"]);
    assert_eq!(refresh_choices(&modes, modes[0]), ["144 Hz", "59.94 Hz"]);
    assert_eq!(
        mode_for_resolution(&modes, modes[0], "1920 × 1080"),
        Some(modes[2]),
        "a new resolution keeps the refresh rate where it can"
    );
    assert_eq!(
        mode_for_resolution(&modes, modes[1], "1920 × 1080"),
        Some(modes[2]),
        "else it takes the fastest"
    );
    assert_eq!(
        mode_for_refresh(&modes, modes[0], "59.94 Hz"),
        Some(modes[1])
    );
    assert_eq!(mode_for_refresh(&modes, modes[2], "59.94 Hz"), None);
}

/// Settings reads the pending display change through its settings document,
/// the channel to the shell, never by watching the session's file: a change
/// kept or reverted elsewhere reaches the page.
#[test]
fn the_pending_display_change_is_read_through_the_settings_document() {
    let _guard = crate::prefs::test_env::ConfigGuard::new();
    let profile = tempfile::tempdir().unwrap().keep();
    let file = profile.join("settings.json");
    let mut pane = DisplayModel::detached(PaneCx::in_file(file.clone()))
        .with_outputs(laptop_with_monitor().outputs);
    pane.of_device = true;
    assert!(pane.transaction().is_none());
    let mut transaction =
        DisplayTransaction::prepare_request(pane.display_set(), DisplayConfig::default()).unwrap();
    transaction.state = DisplayTransactionState::AwaitingConfirmation;
    let section = serde_json::to_value(&transaction).unwrap();
    settings_store::update_section_at(
        &file,
        "display_transaction",
        section.as_object().unwrap().clone(),
    )
    .unwrap();
    assert!(pane.poll(), "the page follows the document");
    let shown = pane.transaction().expect("the pending change is shown");
    assert_eq!(shown.id, transaction.id);
    assert_eq!(shown.state, DisplayTransactionState::AwaitingConfirmation);
}

/// Every name, description and value the page's widgets say.
fn page_words(widget: &dyn Widget, words: &mut Vec<String>) {
    if let Some(node) = widget.accessibility() {
        words.extend(
            node.name
                .into_iter()
                .chain(node.description)
                .chain(node.value),
        );
    }
    for child in widget.children() {
        page_words(child, words);
    }
}

/// The compositor's diagnostic is for the inspection, in its own terms; the
/// page says what happened to the person's change in their words.
#[test]
fn a_refused_or_settled_display_change_is_said_in_the_persons_words() {
    let _guard = crate::prefs::test_env::ConfigGuard::new();
    let profile = tempfile::tempdir().unwrap().keep();
    let file = profile.join("settings.json");
    let mut pane = DisplayModel::detached(PaneCx::in_file(file.clone()))
        .with_outputs(laptop_with_monitor().outputs);
    pane.of_device = true;
    let diagnostic = "Rejected unsupported display plan: UnsupportedMode(\"DP-1\")";
    let mut transaction =
        DisplayTransaction::prepare_request(pane.display_set(), DisplayConfig::default()).unwrap();
    // How often the page says each outcome: a refusal is said by its own
    // card, so it has no notice, and the page says it no more often than it
    // says one notice.
    let mut times_said = Vec::new();
    for (state, said, notice) in [
        (
            DisplayTransactionState::Confirmed,
            "display-changes-kept",
            true,
        ),
        (
            DisplayTransactionState::Rejected,
            "display-rejected-caption",
            false,
        ),
        (
            DisplayTransactionState::Reverted,
            "display-changes-reverted",
            true,
        ),
    ] {
        transaction.id += 1;
        transaction.state = state;
        transaction.message = Some(diagnostic.into());
        let section = serde_json::to_value(&transaction).unwrap();
        settings_store::update_section_at(
            &file,
            "display_transaction",
            section.as_object().unwrap().clone(),
        )
        .unwrap();
        assert!(pane.poll(), "the page follows the document");
        assert_eq!(pane.notice, notice.then(|| t(said)), "{state:?}");
        let mut words = Vec::new();
        page_words(pane.compose().as_ref(), &mut words);
        assert!(
            words.iter().all(|word| !word.contains(diagnostic)),
            "{state:?}: the page repeats the compositor's diagnostic: {words:?}"
        );
        times_said.push(words.iter().filter(|word| **word == t(said)).count());
    }
    assert!(times_said[0] > 0, "the page says the change was kept");
    assert_eq!(
        times_said[1], times_said[0],
        "the page says a refusal once, as it says a kept change"
    );
    assert_eq!(
        pane.inspection()["transaction"]["message"],
        diagnostic,
        "the inspection still carries it"
    );
}
