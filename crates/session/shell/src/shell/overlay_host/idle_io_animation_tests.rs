use super::*;

/// The pointer rests on a window bar's button: its tooltip's dwell is a
/// deadline that wakes the loop, and a due deadline alone paints nothing.
/// The tick it wakes must report the tooltip shown, or the frame that draws
/// it never comes while the pointer stays still.
#[test]
fn a_bar_tooltips_dwell_running_out_asks_for_its_frame() {
    let chrome = crate::shell::chrome_state::ChromeState::new(
        crate::shell::state::WorkspaceTree::bootstrap_demo(),
    );
    let mut host =
        ShellOverlayHost::new(Box::new(ui::widget::Container::new())).with_chrome(chrome);
    host.force_desk_settled(std::time::Instant::now());
    let doc = automerge::AutoCommit::new();
    // The first updates bring the desk's own state in; then it rests.
    assert!(
        (0..20).any(|_| !host.update(&doc, &automerge::ROOT)),
        "the host comes to rest"
    );
    let button = vello::kurbo::Rect::new(10.0, 10.0, 40.0, 40.0);
    host.bar_tooltip
        .on_pointer(Some((button, "Close".into())), ui::time::now());
    let reveal = host
        .bar_tooltip
        .reveal_at()
        .expect("the resting pointer's tooltip waits for its dwell");
    assert!(
        host.next_wake_deadline().is_some_and(|at| at <= reveal),
        "the dwell wakes the loop"
    );
    assert!(
        !host.update(&doc, &automerge::ROOT),
        "before the dwell runs out nothing asks for a frame"
    );
    let shown =
        ui::time::testing::with_monotonic_time(reveal, || host.update(&doc, &automerge::ROOT));
    assert!(
        shown,
        "the tick the dwell wakes asks for the frame that shows it"
    );
    assert_eq!(
        host.bar_tooltip.visible().map(|(label, _)| label),
        Some("Close")
    );
}

#[test]
fn blocked_quick_settings_read_does_not_turn_waiting_into_animation() {
    let mut host = ShellOverlayHost::new(Box::new(ui::widget::Container::new()));
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    host.qs_refresh.kick_fast_with(move || {
        let _ = release_rx.recv();
        crate::shell::quick_settings::refresh::QsReading::Fast(Box::new(
            crate::shell::quick_settings::refresh::FastReading::empty(),
        ))
    });
    assert!(host.qs_refresh.is_in_flight());
    assert!(
        !host.needs_animation_frame(),
        "waiting on an OS command is not visual motion and must not repaint the desktop"
    );
    release_tx.send(()).expect("release test worker");
}

#[test]
fn a_reading_with_no_audio_device_leaves_no_volume_slider() {
    let chrome = crate::shell::chrome_state::ChromeState::new(
        crate::shell::state::WorkspaceTree::bootstrap_demo(),
    );
    let mut host =
        ShellOverlayHost::new(Box::new(ui::widget::Container::new())).with_chrome(chrome.clone());
    host.qs_refresh.kick_fast_with(|| {
        crate::shell::quick_settings::refresh::QsReading::Fast(Box::new(
            crate::shell::quick_settings::refresh::FastReading::empty(),
        ))
    });
    let deadline = tilo_test_env::hang_deadline();
    while host.qs_refresh.is_in_flight() && std::time::Instant::now() < deadline {
        host.apply_quick_settings_readings();
        std::thread::yield_now();
    }
    host.apply_quick_settings_readings();
    let st = chrome.borrow();
    assert!(
        !st.quick_settings.volume_available,
        "no sink, no volume slider"
    );
    assert!(
        !st.quick_settings.brightness_available,
        "no backlight, no brightness slider"
    );
}

#[test]
fn ordinary_host_update_adopts_completed_wifi_scan_without_a_paint() {
    use os_actions::WifiNetworkInfo;
    let (wake_tx, wake_rx) = std::sync::mpsc::channel();
    let mut scope = ui::WakeScope::new();
    scope
        .bind(move |_| {
            let _ = wake_tx.send(());
        })
        .unwrap();
    let chrome = crate::shell::chrome_state::ChromeState::new(
        crate::shell::state::WorkspaceTree::bootstrap_demo(),
    );
    let mut host = {
        let _entered = scope.enter();
        ShellOverlayHost::new(Box::new(ui::widget::Container::new())).with_chrome(chrome.clone())
    };
    chrome.borrow_mut().quick_settings.wifi_scanning = true;
    host.qs_refresh.kick_wifi_networks_with(|| {
        Ok(vec![WifiNetworkInfo {
            ssid: "Fixture network".into(),
            signal: 3,
            secure: false,
            security: os_actions::WifiSecurity::Open,
            connected: false,
            known: false,
        }])
    });
    tilo_test_env::recv("a completed scan must wake its owning host", &wake_rx);
    let changed = host.update(&automerge::AutoCommit::new(), &automerge::ROOT);
    let state = chrome.borrow();
    assert!(
        !host.qs_refresh.is_in_flight(),
        "the normal update must drain the actual completed worker"
    );
    assert!(!state.quick_settings.wifi_scanning);
    assert_eq!(state.quick_settings.wifi_networks.len(), 1);
    assert_eq!(
        state.quick_settings.wifi_networks[0].ssid,
        "Fixture network"
    );
    assert!(
        changed,
        "adopting a network scan must schedule the visible frame"
    );
}

#[test]
fn native_host_wake_delivers_a_failed_scan_to_the_ordinary_update() {
    let (wake_tx, wake_rx) = std::sync::mpsc::channel();
    let chrome = crate::shell::chrome_state::ChromeState::new(
        crate::shell::state::WorkspaceTree::bootstrap_demo(),
    );
    let mut host =
        ShellOverlayHost::new(Box::new(ui::widget::Container::new())).with_chrome(chrome.clone());
    // The desk's widgets appear with the first update; that is not what
    // this measures.
    host.force_desk_settled(std::time::Instant::now());
    host.set_quick_settings_wake(std::sync::Arc::new(move || {
        let _ = wake_tx.send(());
    }));
    chrome.borrow_mut().quick_settings.wifi_scanning = true;
    host.qs_refresh
        .kick_wifi_networks_with(|| Err("fixture scan refused".into()));
    tilo_test_env::recv(
        "native completion must wake the detached owning session",
        &wake_rx,
    );
    assert!(host.update(&automerge::AutoCommit::new(), &automerge::ROOT));
    assert!(!host.qs_refresh.is_in_flight());
    let state = chrome.borrow();
    assert!(!state.quick_settings.wifi_scanning);
    assert_eq!(
        state.quick_settings.wifi_scan_error.as_deref(),
        Some("fixture scan refused")
    );
    assert!(
        !host.needs_animation_frame(),
        "receipt handling must not turn I/O waiting into animation: {:?} {:?}",
        crate::shell::animation_diagnostics::host_labels(
            host.animation_demand_bits(std::time::Instant::now())
        )
        .collect::<Vec<_>>(),
        host.content_animating_leaves()
    );
}

#[test]
fn wifi_radio_dispatch_does_not_wait_for_the_native_network_operation() {
    use crate::shell::quick_settings::radio::fixture::Fixture;
    let fixture = Fixture::new();
    let chrome = crate::shell::chrome_state::ChromeState::new(
        crate::shell::state::WorkspaceTree::bootstrap_demo(),
    );
    chrome.borrow_mut().quick_settings.wifi = false;
    let mut host = ShellOverlayHost::new(Box::new(ui::widget::Container::new()))
        .with_chrome(chrome.clone())
        .with_qs_actions(Box::new(fixture.dispatcher));
    let (wake_tx, wake_rx) = std::sync::mpsc::channel();
    host.set_quick_settings_wake(Arc::new(move || {
        let _ = wake_tx.send(());
    }));
    host.dispatch_quick_settings_toggle_actions(
        crate::shell::quick_settings::root::PendingRootActions {
            wifi: Some(true),
            ..Default::default()
        },
    );
    // The dispatch returned while the radio operation is still held.
    assert_eq!(
        tilo_test_env::recv("the native radio operation to start", &fixture.entered),
        os_actions::RadioChange::Wifi(true),
        "ordinary Quick Settings input dispatch must not wait for a native radio operation"
    );
    assert!(chrome.borrow().quick_settings.radio_change_pending);
    assert!(!chrome.borrow().quick_settings.wifi);
    assert!(!host.needs_animation_frame());
    fixture
        .release
        .send(Err("fixture radio refusal".into()))
        .unwrap();
    tilo_test_env::recv("radio completion to wake the native host", &wake_rx);
    let doc = automerge::AutoCommit::new();
    assert!(host.update(&doc, &automerge::ROOT));
    assert!(!chrome.borrow().quick_settings.radio_change_pending);
    assert!(
        !chrome.borrow().quick_settings.wifi,
        "a refused native write must not turn the switch on"
    );
    assert_eq!(chrome.borrow().notifications().len(), 1);
    host.update(&doc, &automerge::ROOT);
    assert_eq!(
        chrome.borrow().notifications().len(),
        1,
        "the same receipt cannot publish a second failure"
    );
}

#[test]
fn the_wifi_sections_turn_on_waits_for_its_owned_radio_receipt() {
    use crate::shell::quick_settings::radio::fixture::Fixture;
    let fixture = Fixture::new();
    let chrome = crate::shell::chrome_state::ChromeState::new(
        crate::shell::state::WorkspaceTree::bootstrap_demo(),
    );
    {
        let mut state = chrome.borrow_mut();
        state.overlays.quick_settings = true;
        state.quick_settings.wifi = false;
        state.quick_settings.radios.wifi_available = true;
        state.quick_settings.section = Some(crate::shell::quick_settings::QsSection::Wifi);
    }
    let mut host = ShellOverlayHost::new(Box::new(ui::widget::Container::new()))
        .with_chrome(chrome.clone())
        .with_qs_actions(Box::new(fixture.dispatcher));
    // A display for the panel to open on: on none it has no width to press.
    host.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
    let (wake_tx, wake_rx) = std::sync::mpsc::channel();
    host.set_quick_settings_wake(Arc::new(move || {
        let _ = wake_tx.send(());
    }));
    host.force_quick_settings_settled();
    // Lay the page out where a frame would, so its targets are where a
    // click lands.
    let panel = host.quick_settings_rect();
    let _ = host.qs_root.handle_click(
        Point::new(-10.0, -10.0),
        panel,
        &mut chrome.borrow_mut().quick_settings,
    );
    let target =
        |host: &ShellOverlayHost| host.quick_settings_target("quick_settings_section_turn_on");
    let turn_on = target(&host).expect("the section offers to turn Wi-Fi on");
    let mut handled = false;
    host.handle_shell_quick_settings_click(turn_on.center(), &mut handled);
    assert_eq!(
        tilo_test_env::recv("the native radio operation to start", &fixture.entered),
        os_actions::RadioChange::Wifi(true)
    );
    assert!(chrome.borrow().quick_settings.radio_change_pending);
    assert!(
        !chrome.borrow().quick_settings.wifi,
        "admission is not radio success"
    );
    assert!(!host.needs_animation_frame());
    assert!(
        target(&host).is_none(),
        "while it turns on there is nothing to press"
    );
    let doc = automerge::AutoCommit::new();
    // The radio is still held: an update that waited on it would never return.
    host.update(&doc, &automerge::ROOT);
    assert!(
        chrome.borrow().quick_settings.radio_change_pending,
        "unrelated updates cannot wait on radio work"
    );
    fixture.release.send(Ok(())).unwrap();
    tilo_test_env::recv("the Quick Settings wake", &wake_rx);
    assert!(host.update(&doc, &automerge::ROOT));
    let state = chrome.borrow();
    assert!(!state.quick_settings.radio_change_pending);
    assert!(state.quick_settings.wifi);
    assert!(state.quick_settings.radios.wifi_enabled);
    assert!(state.notifications().is_empty());
}

#[test]
fn retired_radio_receipt_stays_pending_until_its_actual_worker_finishes() {
    use crate::shell::quick_settings::radio::fixture::Fixture;
    let fixture = Fixture::new();
    let chrome = crate::shell::chrome_state::ChromeState::new(
        crate::shell::state::WorkspaceTree::bootstrap_demo(),
    );
    chrome.borrow_mut().quick_settings.wifi = false;
    let mut host = ShellOverlayHost::new(Box::new(ui::widget::Container::new()))
        .with_chrome(chrome.clone())
        .with_qs_actions(Box::new(fixture.dispatcher.clone()));
    let (wake_tx, wake_rx) = std::sync::mpsc::channel();
    host.set_quick_settings_wake(Arc::new(move || {
        let _ = wake_tx.send(());
    }));
    let job = host
        .qs_action_slot
        .start_radio(os_actions::RadioChange::Wifi(true), &chrome)
        .unwrap()
        .expect("a prepared operation owns a pending receipt");
    tilo_test_env::recv("the native radio operation to start", &fixture.entered);
    host.qs_action_slot.replace(
        std::rc::Rc::new(fixture.dispatcher),
        crate::shell::quick_settings::actions::QsActionAvailability::all(),
    );
    assert!(job.result().unwrap().is_err());
    assert!(!job.finished());
    job.apply(&chrome);
    let doc = automerge::AutoCommit::new();
    host.update(&doc, &automerge::ROOT);
    assert!(
        chrome.borrow().quick_settings.radio_change_pending,
        "caller retirement cannot clear a still-running native operation"
    );
    assert!(
        host.qs_action_slot.take_radio_completion().is_none(),
        "read-back must not happen before the old hardware operation finishes"
    );
    assert!(!chrome.borrow().quick_settings.wifi);
    fixture.release.send(Ok(())).unwrap();
    tilo_test_env::recv("the Quick Settings wake", &wake_rx);
    assert!(host.update(&doc, &automerge::ROOT));
    assert!(job.finished());
    assert!(
        job.result().unwrap().is_err(),
        "late success cannot overwrite retirement"
    );
    assert!(!chrome.borrow().quick_settings.radio_change_pending);
    assert!(
        !chrome.borrow().quick_settings.wifi,
        "retired work cannot claim the replacement's state"
    );
    assert_eq!(chrome.borrow().notifications().len(), 1);
    assert!(host.qs_action_slot.take_radio_completion().is_none());
}

#[test]
fn successful_radio_reading_refreshes_the_recovered_public_action_availability() {
    use crate::shell::quick_settings::radio::fixture::Fixture;
    use crate::shell::quick_settings::refresh::{FastReading, QsReading};
    use crate::shell::search::{SearchContext, SearchProvider};
    let fixture = Fixture::new();
    let chrome = crate::shell::chrome_state::ChromeState::new(
        crate::shell::state::WorkspaceTree::bootstrap_demo(),
    );
    let mut host =
        ShellOverlayHost::new(Box::new(ui::widget::Container::new())).with_chrome(chrome.clone());
    host.qs_action_slot.replace(
        std::rc::Rc::new(fixture.dispatcher),
        crate::shell::quick_settings::actions::QsActionAvailability::default(),
    );
    let provider =
        crate::shell::search::providers::actions::ActionsProvider::with_shared_qs_actions(
            host.qs_action_slot.clone(),
        );
    let doc = automerge::AutoCommit::new();
    let available = || {
        let rows = provider
            .query(SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle: "quick-settings.",
                wanted: &[crate::shell::search::ResourceKind::Action],
                chrome: &chrome.borrow(),
                doc: &doc,
                doc_root: &automerge::ROOT,
            })
            .into_iter()
            .filter(|row| {
                matches!(
                    row.id.as_str(),
                    "quick-settings.wifi.set-enabled" | "quick-settings.airplane-mode.set-enabled"
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            rows.len(),
            2,
            "both declared radio actions must be observable"
        );
        rows.into_iter().all(|row| {
            matches!(
                row.contract.unwrap().availability,
                tilo_actions::Availability::Available
            )
        })
    };
    assert!(
        !available(),
        "startup has not observed a working radio backend"
    );
    let (wake_tx, wake_rx) = std::sync::mpsc::channel();
    host.set_quick_settings_wake(Arc::new(move || {
        let _ = wake_tx.send(());
    }));
    host.qs_refresh.kick_fast_with(|| {
        QsReading::Fast(Box::new(FastReading {
            radios: Some(os_actions::RadioState {
                wifi_available: true,
                wifi_enabled: true,
                ..os_actions::RadioState::default()
            }),
            ..FastReading::empty()
        }))
    });
    tilo_test_env::recv("the Quick Settings wake", &wake_rx);
    assert!(host.update(&doc, &automerge::ROOT));
    assert!(chrome.borrow().quick_settings.radios.wifi_available);
    assert!(
        available(),
        "actual recovered radio metadata must refresh both public actions"
    );
}

/// The desk's widgets fading in are motion the compositor must paint: the
/// demand the frame pump reads names them while they move and lets go once
/// they rest. The shell's demand is one model, so a motion missing from its
/// named sources would get no frames at all.
#[test]
fn the_desk_appearing_is_named_demand_until_it_rests() {
    let chrome = crate::shell::chrome_state::ChromeState::new(
        crate::shell::state::WorkspaceTree::bootstrap_demo(),
    );
    let mut host =
        ShellOverlayHost::new(Box::new(ui::widget::Container::new())).with_chrome(chrome);
    let doc = automerge::AutoCommit::new();
    let now = ui::time::now();
    ui::time::testing::with_monotonic_time(now, || host.update(&doc, &automerge::ROOT));
    let named = |host: &ShellOverlayHost| {
        crate::shell::animation_diagnostics::host_labels(host.animation_demand_bits(now))
            .any(|label| label == "desk")
    };
    assert!(
        host.desk_animating(now),
        "the desk's widgets fade in with the first update"
    );
    assert!(named(&host), "the desk fading in is named demand");
    assert!(
        host.needs_animation_frame_at(now),
        "and the frame pump paints for it"
    );
    host.force_desk_settled(now);
    assert!(!named(&host), "a desk at rest asks for nothing");
}
