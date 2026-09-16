use super::*;

#[test]
fn maps_navigation_and_load_errors_without_simulating_pages() {
    assert_eq!(
        decode_event(&json!({"event":"uri","url":"https://podesta.ai"})),
        Some(WebViewEvent::UriChanged("https://podesta.ai".into()))
    );
    assert_eq!(
        decode_event(
            &json!({"event":"error","url":"https://podesta.ai","message":"certificate error"})
        ),
        Some(WebViewEvent::LoadFailed {
            uri: "https://podesta.ai".into(),
            error: "certificate error".into()
        })
    );
    assert_eq!(
        decode_event(&json!({"event":"history","back":true,"forward":false})),
        Some(WebViewEvent::HistoryChanged {
            can_go_back: true,
            can_go_forward: false
        })
    );
    assert!(decode_event(&json!({"event":"future-event"})).is_none());
    assert!(decode_event(&json!({"event":"uri"})).is_none());
}

#[test]
fn missing_helper_is_a_startup_error() {
    let root = tempfile::tempdir().unwrap();
    let result = ChromiumEngine::with_host(
        &root.path().join("missing-host"),
        &root.path().join("profile"),
    );
    assert!(result.is_err());
}

#[test]
fn unavailable_engine_reports_failure_without_page_pixels() {
    let engine = UnavailableEngine::new("Runtime missing".into());
    engine.send(WebViewCommand::Navigate("https://podesta.ai".into()));
    assert!(engine.frames().lock().unwrap().is_none());
    assert!(engine.drain_events().iter().any(|event| matches!(event, WebViewEvent::LoadFailed {uri,error} if uri == "https://podesta.ai" && error == "Runtime missing")));
}

#[test]
fn forwards_modifier_and_physical_input_values() {
    let cmd = encode_command(WebViewCommand::Key {
        key: super::super::KeyInput::Character("a".into()),
        down: true,
        modifiers: super::super::KeyModifiers {
            ctrl: true,
            shift: true,
            alt: false,
            meta: false,
        },
    })
    .unwrap();
    assert_eq!(
        cmd,
        json!({"op":"key","key":"a","down":true,"ctrl":true,"shift":true,"alt":false,"meta":false})
    );
    assert_eq!(
        encode_command(WebViewCommand::Resize {
            width: 1920,
            height: 1080,
            scale_factor: 2.0
        })
        .unwrap(),
        json!({"op":"resize","width":1920,"height":1080,"scale_factor":2.0})
    );
}

#[test]
fn malformed_known_events_are_errors_and_unknown_events_are_ignored() {
    for value in [
        json!({"event":"ready"}),
        json!({"event":"uri"}),
        json!({"event":"progress","progress":"half"}),
        json!({"event":"history","back":true}),
        json!({"event":"loaded","url":"https://example.org"}),
        json!({"event":"error","message":false}),
    ] {
        assert!(validate_event(&value).is_err(), "{value}");
    }
    assert!(validate_event(&json!({"event":"future-event"})).is_ok());
    assert!(
        validate_event(&json!({"event":"ready","cef_version":"152","chromium_version":"152"}))
            .is_ok()
    );
}

#[test]
fn large_commands_are_rejected_before_encoding() {
    assert!(!command_within_limits(&WebViewCommand::Navigate(
        "x".repeat(65537)
    )));
    assert!(!command_within_limits(&WebViewCommand::SetUserScripts(
        Arc::new(vec!["x".repeat(4 * 1024 * 1024 + 1)])
    )));
}

#[test]
fn pointer_bursts_coalesce_and_keep_navigation_order() {
    let queue = CommandQueue::default();
    for x in 0..10000 {
        queue.push(json!({"op":"mouse_move","x":x,"y":0})).unwrap();
    }
    queue
        .push(json!({"op":"navigate","url":"https://podesta.ai"}))
        .unwrap();
    queue.close();
    assert_eq!(queue.pop().unwrap()["x"], 9999);
    assert_eq!(queue.pop().unwrap()["op"], "navigate");
    assert!(queue.pop().is_none());
}

#[test]
fn progress_bursts_preserve_load_outcome_and_waker_can_replace_itself() {
    let engine = UnavailableEngine::new("error".into());
    let weak = Arc::downgrade(&engine);
    engine.set_waker(Box::new(move || {
        weak.upgrade().unwrap().set_waker(Box::new(|| {}));
    }));
    engine.shared.event(WebViewEvent::LoadFinished);
    for _ in 0..2000 {
        engine.shared.event(WebViewEvent::LoadProgress(0.5));
    }
    let events = engine.drain_events();
    assert_eq!(events.len(), 2);
    assert!(events.contains(&WebViewEvent::LoadFinished));
}

#[cfg(unix)]
fn helper_fixture(trailing: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("helper");
    let mut bytes = Vec::new();
    protocol::write_json(
        &mut bytes,
        &json!({"event":"ready","cef_version":"152","chromium_version":"152"}),
    )
    .unwrap();
    let encoded: String = bytes.iter().map(|byte| format!("\\{:03o}", byte)).collect();
    std::fs::write(
        &path,
        format!("#!/bin/sh\nprintf '{encoded}'\n{trailing}\n"),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    (dir, path)
}

#[test]
#[cfg(unix)]
fn helper_eof_is_reported_once_without_engine_drop() {
    let (dir, host) = helper_fixture("exit 0");
    let engine = ChromiumEngine::with_host(&host, &dir.path().join("profile")).unwrap();
    std::thread::sleep(Duration::from_millis(250));
    let events = engine.drain_events();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, WebViewEvent::LoadFailed { .. }))
            .count(),
        1
    );
    assert!(engine.tx.state.lock().unwrap().2);
}

#[test]
#[cfg(unix)]
fn malformed_output_terminates_and_reaps_helper() {
    let (dir, host) = helper_fixture("printf '\\001\\000\\000\\000\\011'\nexec cat >/dev/null");
    let engine = ChromiumEngine::with_host(&host, &dir.path().join("profile")).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert!(engine.shared.failed.load(Ordering::Acquire));
    assert!(engine.tx.state.lock().unwrap().2);
    assert!(engine.drain_events().iter().any(|event| matches!(event, WebViewEvent::LoadFailed {error, ..} if error.contains("Invalid Chromium output"))));
}

#[test]
fn media_response_saturation_is_bounded_and_preserves_navigation_outcome() {
    let engine = UnavailableEngine::new("error".into());
    engine.shared.event(WebViewEvent::LoadFinished);
    for request_id in 0..2000 {
        engine
            .shared
            .event(WebViewEvent::MediaElementUnavailable { request_id });
    }
    let events = engine.drain_events();
    assert_eq!(events.len(), 512);
    assert!(events.contains(&WebViewEvent::LoadFinished));
    assert!(!command_within_limits(&WebViewCommand::SetUserScripts(
        Arc::new(vec![String::new(); 1025])
    )));
}

#[test]
#[cfg(unix)]
fn rejected_navigation_is_visible_and_does_not_change_current_uri() {
    let (dir, host) = helper_fixture("exec cat >/dev/null");
    let engine = ChromiumEngine::with_host(&host, &dir.path().join("profile")).unwrap();
    *engine.shared.uri.lock().unwrap() = "https://old.example".into();
    engine.tx.close();
    engine.send(WebViewCommand::Navigate("https://new.example".into()));
    assert_eq!(*engine.shared.uri.lock().unwrap(), "https://old.example");
    assert!(engine.drain_events().iter().any(|event| matches!(event, WebViewEvent::LoadFailed {uri,error} if uri == "https://new.example" && error.contains("not queued"))));
}

#[test]
fn capability_commands_and_events_round_trip() {
    assert_eq!(
        encode_command(WebViewCommand::PermissionDecision {
            id: 7,
            allow: false
        })
        .unwrap(),
        json!({"op":"permission_decision","id":7,"allow":false})
    );
    assert_eq!(
        encode_command(WebViewCommand::SetClearOnExit(true)).unwrap(),
        json!({"op":"clear_on_exit","enabled":true})
    );
    assert_eq!(
        decode_event(
            &json!({"event":"permission_prompt","id":7,"origin":"https://example.org","permissions":"camera"})
        ),
        Some(WebViewEvent::PermissionRequested {
            id: 7,
            origin: "https://example.org".into(),
            permissions: "camera".into()
        })
    );
    assert!(
        validate_event(
            &json!({"event":"permission_prompt","id":-1,"origin":"x","permissions":"camera"})
        )
        .is_err()
    );
    assert!(validate_event(&json!({"event":"download","id":1,"state":"invented"})).is_err());
}

#[test]
fn session_history_payloads_are_bounded_and_keep_snapshot_identity() {
    let snapshot = json!({"event":"navigation_history","request_id":7,"direction":"forward","entries":[{"id":42,"url":"https://a.test","title":"A"}]});
    assert!(validate_event(&snapshot).is_ok());
    assert_eq!(
        decode_event(&snapshot),
        Some(WebViewEvent::NavigationHistory {
            request_id: 7,
            direction: super::super::NavigationDirection::Forward,
            entries: vec![super::super::NavigationEntry {
                id: 42,
                url: "https://a.test".into(),
                title: "A".into()
            }],
        })
    );
    assert_eq!(
        encode_command(WebViewCommand::RequestNavigationHistory {
            request_id: 7,
            direction: super::super::NavigationDirection::Forward,
        }),
        Some(json!({"op":"navigation_history","request_id":7,"direction":"forward"}))
    );
    assert_eq!(
        encode_command(WebViewCommand::NavigateToHistoryEntry {
            request_id: 7,
            id: 42
        }),
        Some(json!({"op":"history_entry","request_id":7,"id":42}))
    );
    let mut invalid = snapshot.clone();
    invalid["entries"][0]["id"] = json!(-1);
    assert!(validate_event(&invalid).is_err());
    invalid = snapshot.clone();
    invalid["entries"] = json!(vec![snapshot["entries"][0].clone(); 13]);
    assert!(validate_event(&invalid).is_err());
    assert!(!command_within_limits(
        &WebViewCommand::NavigateToHistoryEntry {
            request_id: 0,
            id: 42
        }
    ));
    let mut legacy = snapshot;
    legacy.as_object_mut().unwrap().remove("direction");
    assert_eq!(
        decode_event(&legacy),
        Some(WebViewEvent::NavigationHistory {
            request_id: 7,
            direction: super::super::NavigationDirection::Back,
            entries: vec![super::super::NavigationEntry {
                id: 42,
                url: "https://a.test".into(),
                title: "A".into()
            }],
        })
    );
}

#[test]
fn pointer_coalescing_never_erases_another_tabs_input() {
    let queue = CommandQueue::default();
    queue
        .push(json!({"op":"mouse_move","tab":1,"x":10}))
        .unwrap();
    queue
        .push(json!({"op":"mouse_move","tab":2,"x":20}))
        .unwrap();
    queue.close();
    assert_eq!(queue.pop().unwrap()["tab"], 1);
    assert_eq!(queue.pop().unwrap()["tab"], 2);
}

#[test]
fn routed_frame_packet_is_accepted_and_malformed_tab_rejected() {
    let mut bytes = 17u32.to_le_bytes().to_vec();
    bytes.push(3);
    bytes.extend_from_slice(&2u32.to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&[10, 20, 30, 255]);
    let Some(protocol::Packet::TabFrame(tab, frame)) =
        protocol::read_packet(&mut std::io::Cursor::new(&bytes)).unwrap()
    else {
        panic!("expected routed frame");
    };
    assert_eq!(tab, 2);
    assert_eq!(&*frame.rgba, &[30, 20, 10, 255]);
    bytes[5..9].copy_from_slice(&0u32.to_le_bytes());
    assert!(protocol::read_packet(&mut std::io::Cursor::new(&bytes)).is_err());
}

#[test]
#[cfg(unix)]
fn tab_endpoints_keep_runtime_and_waker_after_first_endpoint_closes() {
    let (dir, host) = helper_fixture("exec cat >/dev/null");
    let first = ChromiumEngine::with_host(&host, &dir.path().join("profile")).unwrap();
    let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = wakes.clone();
    first.set_waker(Box::new(move || {
        seen.fetch_add(1, Ordering::Relaxed);
    }));
    let second = first.create_tab().unwrap();
    let routes = first.runtime.routes.clone();
    let process = first.runtime.process.clone();
    drop(first);
    assert!(!process.stopping.load(Ordering::Acquire));
    assert!(route(&routes, 1).is_none());
    let before = wakes.load(Ordering::Relaxed);
    route(&routes, 2)
        .unwrap()
        .event(WebViewEvent::TitleChanged("Second".into()));
    assert!(wakes.load(Ordering::Relaxed) > before);
    assert_eq!(
        second.drain_events(),
        vec![WebViewEvent::TitleChanged("Second".into())]
    );
    fail_routes(&routes, "disconnected".into());
    assert!(
        second
            .drain_events()
            .iter()
            .any(|event| matches!(event, WebViewEvent::LoadFailed { .. }))
    );
    drop(second);
    assert!(process.stopping.load(Ordering::Acquire));
}

#[test]
#[cfg(unix)]
fn native_capacity_is_reserved_until_closed_acknowledgment() {
    let mut packet = Vec::new();
    protocol::write_json(&mut packet, &json!({"event":"closed","tab":32})).unwrap();
    let encoded: String = packet
        .iter()
        .map(|byte| format!("\\{:03o}", byte))
        .collect();
    let (dir, host) = helper_fixture(&format!(
        "profile=${{1#--profile=}}\nwhile [ ! -f \"$profile/ack-close\" ]; do sleep 0.01; done\nprintf '{encoded}'\nexec cat >/dev/null"
    ));
    let profile = dir.path().join("profile");
    let first = ChromiumEngine::with_host(&host, &profile).unwrap();
    let mut tabs: Vec<_> = (1..32).map(|_| first.create_tab().unwrap()).collect();
    drop(tabs.pop());
    assert!(
        first.create_tab().is_err(),
        "native browser is still closing"
    );
    assert_eq!(first.runtime.routes.lock().unwrap().len(), 32);
    std::fs::write(profile.join("ack-close"), b"ack").unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while first.runtime.routes.lock().unwrap().contains_key(&32) {
        assert!(
            std::time::Instant::now() < deadline,
            "closed event releases native reservation"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(first.create_tab().is_ok());
}

#[test]
#[cfg(unix)]
fn hiding_retains_last_frame_for_recomposed_tab_until_next_paint() {
    let (dir, host) = helper_fixture("exec cat >/dev/null");
    let engine = ChromiumEngine::with_host(&host, &dir.path().join("profile")).unwrap();
    *engine.frames().lock().unwrap() = Some(super::super::Frame {
        rgba: Arc::new(vec![255, 0, 0, 255]),
        width: 1,
        height: 1,
    });
    engine.set_visible(false);
    engine.set_visible(true);
    assert!(
        engine.frames().lock().unwrap().is_some(),
        "recomposing a retained tab needs pixels before asynchronous repaint"
    );
}

#[test]
fn find_and_zoom_protocol_rejects_invalid_bounds() {
    assert!(command_within_limits(&WebViewCommand::Find {
        id: 1,
        text: "needle".into(),
        forward: false,
        next: true
    }));
    assert!(!command_within_limits(&WebViewCommand::SetZoomLevel(
        f64::NAN
    )));
    assert!(!command_within_limits(&WebViewCommand::SetZoomLevel(10.0)));
    assert!(
        validate_event(&json!({"event":"find_result","id":1,"count":3,"active":1,"final":true}))
            .is_ok()
    );
    assert!(
        validate_event(
            &json!({"event":"find_result","id":1,"count":4294967296_u64,"active":1,"final":true})
        )
        .is_err()
    );
    assert!(validate_event(&json!({"event":"zoom","level":99.0})).is_err());
    assert_eq!(
        encode_command(WebViewCommand::SetZoomLevel(1.0)),
        Some(json!({"op":"zoom","level":1.0}))
    );
}

#[test]
fn runtime_download_queue_coalesces_progress_and_is_bounded() {
    let queue = Mutex::new(VecDeque::new());
    let update = |id, state: &str, received| WebViewEvent::DownloadUpdated {
        id,
        state: state.into(),
        path: "/tmp/file".into(),
        name: "file".into(),
        received,
        total: 1000,
        error: String::new(),
    };
    for received in 0..1000 {
        queue_download_event(&queue, update(1, "progress", received));
    }
    assert_eq!(queue.lock().unwrap().len(), 1);
    queue_download_event(&queue, update(1, "complete", 1000));
    assert!(
        matches!(queue.lock().unwrap().front(), Some(WebViewEvent::DownloadUpdated { state, .. }) if state == "complete")
    );
    for id in 2..500 {
        queue_download_event(&queue, update(id, "complete", 1000));
    }
    assert_eq!(queue.lock().unwrap().len(), 256);
    assert_eq!(
        decode_event(&json!({"event":"closed","tab":2})),
        Some(WebViewEvent::TabClosed)
    );
    assert_eq!(
        decode_event(&json!({"event":"close_blocked","tab":2})),
        Some(WebViewEvent::TabCloseBlocked)
    );
}

#[test]
#[cfg(unix)]
fn admitted_close_does_not_reinsert_capacity_when_endpoint_drops() {
    let mut script = String::from("profile=${1#--profile=}\n");
    for tab in 2..42 {
        let mut packet = Vec::new();
        protocol::write_json(&mut packet, &json!({"event":"closed","tab":tab})).unwrap();
        let encoded: String = packet
            .iter()
            .map(|byte| format!("\\{:03o}", byte))
            .collect();
        script.push_str(&format!(
            "while [ ! -f \"$profile/ack-{tab}\" ]; do sleep 0.01; done\nprintf '{encoded}'\n"
        ));
    }
    script.push_str("exec cat >/dev/null");
    let (dir, host) = helper_fixture(&script);
    let profile = dir.path().join("profile");
    let first = ChromiumEngine::with_host(&host, &profile).unwrap();
    for id in 2..42 {
        let tab = first.create_tab().unwrap();
        assert!(!tab.request_close());
        std::fs::write(profile.join(format!("ack-{id}")), b"ack").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while first.runtime.routes.lock().unwrap().contains_key(&id) {
            assert!(
                std::time::Instant::now() < deadline,
                "native close acknowledgment"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(tab.drain_events().contains(&WebViewEvent::TabClosed));
        drop(tab);
        let count = first.runtime.routes.lock().unwrap().len();
        assert_eq!(
            count, 1,
            "dropping an admitted closed tab must not reserve its capacity again"
        );
    }
}
