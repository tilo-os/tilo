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
    let snapshot = json!({"event":"navigation_history","request_id":7,"entries":[{"id":42,"url":"https://a.test","title":"A"}]});
    assert!(validate_event(&snapshot).is_ok());
    assert_eq!(
        decode_event(&snapshot),
        Some(WebViewEvent::NavigationHistory {
            request_id: 7,
            entries: vec![super::super::NavigationEntry {
                id: 42,
                url: "https://a.test".into(),
                title: "A".into()
            }],
        })
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
}
