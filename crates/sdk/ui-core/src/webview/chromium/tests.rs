use super::*;

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn renderer_state_events_decode_bounded_and_coalesce() {
    // phase: fullscreen_protocol_decodes_valid_edges_and_rejects_malformed_known_events
    {
        assert_eq!(
            decode_event(&json!({"event":"fullscreen","fullscreen":true})),
            Some(WebViewEvent::FullscreenChanged(true))
        );
        assert_eq!(
            decode_event(&json!({"event":"fullscreen","fullscreen":false})),
            Some(WebViewEvent::FullscreenChanged(false))
        );
        assert_eq!(
            decode_event(&json!({"event":"renderer_terminated"})),
            Some(WebViewEvent::RendererTerminated)
        );
        assert!(validate_event(&json!({"event":"fullscreen","fullscreen":true})).is_ok());
        assert!(validate_event(&json!({"event":"renderer_terminated"})).is_ok());
        for malformed in [
            json!({"event":"fullscreen"}),
            json!({"event":"fullscreen","fullscreen":"true"}),
            json!({"event":"renderer_terminated","status":"crashed"}),
        ] {
            assert!(validate_event(&malformed).is_err(), "{malformed}");
        }
        assert_eq!(
            encode_command(WebViewCommand::ExitFullscreen),
            Some(json!({"op":"exit_fullscreen"}))
        );
    }
    // phase: fullscreen_state_events_coalesce_without_dropping_renderer_termination
    {
        let shared = Shared::default();
        shared.event(WebViewEvent::FullscreenChanged(true));
        shared.event(WebViewEvent::FullscreenChanged(false));
        shared.event(WebViewEvent::RendererTerminated);
        for progress in 0..100 {
            shared.event(WebViewEvent::LoadProgress(f64::from(progress) / 100.0));
        }
        assert_eq!(
            shared
                .events
                .lock()
                .unwrap()
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
            vec![
                WebViewEvent::FullscreenChanged(false),
                WebViewEvent::RendererTerminated,
                WebViewEvent::LoadProgress(0.99),
            ]
        );
    }
    // phase: bounded_identity_events_decode_and_reject_oversize_favicons
    {
        let event = decode_event(&serde_json::json!({"event":"favicon","page_url":"https://fixture.test/","width":1,"height":1,"png":"iVBORw=="})).unwrap();
        assert!(matches!(
            event,
            WebViewEvent::FaviconChanged {
                width: 1,
                height: 1,
                ..
            }
        ));
        assert!(decode_event(&serde_json::json!({"event":"favicon","page_url":"https://fixture.test/","width":65,"height":1,"png":"AA=="})).is_none());
        assert_eq!(
            decode_event(
                &serde_json::json!({"event":"media_state","page_url":"https://fixture.test/","document_generation":7,"playing":true})
            ),
            Some(WebViewEvent::MediaPlayingChanged {
                page_url: "https://fixture.test/".into(),
                document_generation: 7,
                playing: true
            })
        );
        assert_eq!(
            encode_command(WebViewCommand::SetAudioMuted(true)).unwrap()["op"],
            "audio_muted"
        );
    }
    // phase: oversized_favicon_dimensions_cannot_wrap_into_valid_sizes
    {
        for key in ["width", "height"] {
            let mut event = json!({"event":"favicon", "width":1, "height":1,
            "png":"AA==", "page_url":"https://example.test"});
            event[key] = json!(u64::from(u32::MAX) + 2);
            assert!(
                decode_event(&event).is_none(),
                "oversized {key} was truncated"
            );
        }
    }
    // phase: favicon_base64_rejects_trailing_data_and_invalid_padding
    {
        for invalid in ["AA==junk", "=AAA", "A", "AA=", "AB=="] {
            assert!(
                decode_base64_bounded(invalid, 16).is_none(),
                "accepted {invalid}"
            );
        }
        assert_eq!(decode_base64_bounded("AA==", 1), Some(vec![0]));
        assert_eq!(decode_base64_bounded("AQID", 3), Some(vec![1, 2, 3]));
        assert!(decode_base64_bounded("AQID", 2).is_none());
        assert_eq!(decode_base64_bounded("", 0), Some(Vec::new()));
    }
}

#[test]
fn navigation_outcomes_are_mapped_and_preserved_under_bursts() {
    // phase: maps_navigation_and_load_errors_without_simulating_pages
    {
        assert_eq!(
            decode_event(&json!({"event":"uri","url":"https://tilo.so"})),
            Some(WebViewEvent::UriChanged("https://tilo.so".into()))
        );
        assert_eq!(
            decode_event(
                &json!({"event":"error","url":"https://tilo.so","message":"certificate error"})
            ),
            Some(WebViewEvent::LoadFailed {
                uri: "https://tilo.so".into(),
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
    // phase: completed_navigation_preserves_page_identity
    {
        let event =
            decode_event(&serde_json::json!({"event":"loaded","url":"about:blank","status":200}));
        assert_eq!(
            event,
            Some(WebViewEvent::LoadFinishedAt {
                uri: "about:blank".into()
            })
        );
    }
    // phase: progress_bursts_preserve_load_outcome_and_waker_can_replace_itself
    {
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
    // phase: media_response_saturation_is_bounded_and_preserves_navigation_outcome
    {
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
}

#[test]
fn an_unavailable_engine_fails_startup_without_page_pixels() {
    // phase: missing_helper_is_a_startup_error
    {
        let root = tempfile::tempdir().unwrap();
        let result = ChromiumEngine::with_host(
            &root.path().join("missing-host"),
            &root.path().join("profile"),
        );
        assert!(result.is_err());
    }
    // phase: unavailable_engine_reports_failure_without_page_pixels
    {
        let engine = UnavailableEngine::new("Runtime missing".into());
        engine.send(WebViewCommand::Navigate("https://tilo.so".into()));
        assert!(engine.frames().lock().unwrap().is_none());
        assert!(engine.drain_events().iter().any(|event| matches!(event, WebViewEvent::LoadFailed {uri,error} if uri == "https://tilo.so" && error == "Runtime missing")));
    }
}

#[test]
#[cfg(unix)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn private_engines_own_and_remove_their_disposable_roots() {
    // phase: private_helper_gets_incognito_flag_and_owned_root_is_removed_after_exit
    {
        use std::os::unix::fs::PermissionsExt;
        let fixture = tempfile::tempdir().unwrap();
        let host = fixture.path().join("private-helper");
        let arguments = fixture.path().join("arguments");
        let exited = fixture.path().join("exited");
        let mut bytes = Vec::new();
        protocol::write_json(
            &mut bytes,
            &json!({"event":"ready","cef_version":"152","chromium_version":"152"}),
        )
        .unwrap();
        let encoded: String = bytes.iter().map(|byte| format!("\\{:03o}", byte)).collect();
        std::fs::write(
        &host,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nprintf '{encoded}'\ncat >/dev/null\nprintf done > '{}'\n",
            arguments.display(),
            exited.display()
        ),
    )
    .unwrap();
        std::fs::set_permissions(&host, std::fs::Permissions::from_mode(0o755)).unwrap();
        let root = private_root();

        let engine = ChromiumEngine::with_private_host(&host, &root).unwrap();
        let launched = std::fs::read_to_string(&arguments).unwrap();
        let has_private = launched.lines().any(|arg| arg == "--private");
        let has_profile = launched
            .lines()
            .any(|arg| arg == format!("--profile={}/chromium", root.display()));
        assert!(root.exists(), "root must outlive the running helper");
        drop(engine);
        assert!(has_private, "launched arguments: {launched:?}");
        assert!(has_profile, "launched arguments: {launched:?}");
        assert!(
            exited.exists(),
            "engine drop must wait for the helper to exit"
        );
        assert!(
            !root.exists(),
            "engine drop must wait for complete owned-root cleanup"
        );
    }
    // phase: private_startup_failures_remove_owned_roots_after_reaping
    {
        use std::os::unix::fs::PermissionsExt;

        let missing_root = private_root();
        assert!(
            ChromiumEngine::with_private_host(Path::new("/missing/tilo-host"), &missing_root)
                .is_err()
        );
        assert!(!missing_root.exists());

        let fixture = tempfile::tempdir().unwrap();
        let host = fixture.path().join("not-ready");
        std::fs::write(&host, "#!/bin/sh\nexit 7\n").unwrap();
        std::fs::set_permissions(&host, std::fs::Permissions::from_mode(0o755)).unwrap();
        let readiness_root = private_root();
        assert!(ChromiumEngine::with_private_host(&host, &readiness_root).is_err());
        assert!(!readiness_root.exists());
    }
    // phase: private_packaged_helper_discovery_failure_removes_root
    {
        let fixture = tempfile::tempdir().unwrap();
        let isolated_executable = fixture.path().join("isolated/bin/browser-test");
        std::fs::create_dir_all(isolated_executable.parent().unwrap()).unwrap();
        let root = private_root();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "webview::chromium::tests::private_packaged_helper_discovery_failure_child",
                "--ignored",
            ])
            .env_remove("TILO_CHROMIUM_HOST")
            .env("TILO_TEST_CHROMIUM_DISCOVERY_EXE", isolated_executable)
            .env("TILO_TEST_PRIVATE_ROOT", &root)
            .status()
            .unwrap();
        assert!(result.success());
        assert!(!root.exists());
    }
    // phase: private_helper_crash_removes_root_while_engine_handle_remains
    {
        let (_fixture, host) = helper_fixture("exit 9");
        let root = private_root();
        let engine = ChromiumEngine::with_private_host(&host, &root).unwrap();
        wait_until_removed(&root);
        assert!(
            !root.exists(),
            "supervisor must clean after the crashed helper exits"
        );
        assert!(engine.shared.failed.load(Ordering::Acquire));
    }
    // phase: private_process_exit_waits_for_helper_and_root_cleanup
    {
        let (_fixture, host) = helper_fixture("exec cat >/dev/null");
        let root = private_root();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "webview::chromium::tests::private_process_lifecycle_child",
                "--ignored",
            ])
            .env("TILO_TEST_PRIVATE_HOST", &host)
            .env("TILO_TEST_PRIVATE_ROOT", &root)
            .status()
            .unwrap();
        assert!(result.success());
        assert!(!root.exists(), "root must be absent when the child exits");
    }
    // phase: unavailable_private_engine_owns_its_disposable_root
    {
        let root = private_root();
        let engine =
            UnavailableEngine::new_with_private_root("Private backend unavailable".into(), &root)
                .unwrap();
        assert!(root.exists());
        drop(engine);
        assert!(!root.exists());
    }
}

#[cfg(unix)]
fn private_root() -> PathBuf {
    let root = std::env::temp_dir().join(format!("tilo-browser-private-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    std::fs::write(
        root.join(".tilo-private-root-v1"),
        "tilo-browser-owned-private-root-v1\n",
    )
    .unwrap();
    std::fs::write(root.join(".tilo-private-root.lock"), "private-root-lock\n").unwrap();
    root.canonicalize().unwrap()
}

#[cfg(unix)]
fn wait_until_removed(root: &Path) {
    tilo_test_env::wait_until("the profile root to be removed", || !root.exists());
}

#[test]
#[ignore = "child entry point for isolated packaged-helper discovery"]
#[cfg(unix)]
fn private_packaged_helper_discovery_failure_child() {
    let root = PathBuf::from(std::env::var_os("TILO_TEST_PRIVATE_ROOT").unwrap());
    let result = ChromiumEngine::new_private(&root);
    assert!(
        result.is_err(),
        "isolated discovery unexpectedly found a helper"
    );
    assert!(
        !root.exists(),
        "discovery failure must release the allocated private root"
    );
}

#[test]
#[ignore = "child entry point for the real process-lifecycle regression"]
#[cfg(unix)]
fn private_process_lifecycle_child() {
    let host = std::env::var_os("TILO_TEST_PRIVATE_HOST").unwrap();
    let root = PathBuf::from(std::env::var_os("TILO_TEST_PRIVATE_ROOT").unwrap());
    let engine = ChromiumEngine::with_private_host(Path::new(&host), &root).unwrap();
    assert!(root.exists());
    drop(engine);
}

#[test]
fn input_is_forwarded_exactly_and_coalesced_per_tab() {
    // phase: forwards_modifier_and_physical_input_values
    {
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
    // phase: pointer_bursts_coalesce_and_keep_navigation_order
    {
        let queue = CommandQueue::default();
        for x in 0..10000 {
            queue.push(json!({"op":"mouse_move","x":x,"y":0})).unwrap();
        }
        queue
            .push(json!({"op":"navigate","url":"https://tilo.so"}))
            .unwrap();
        queue.close();
        assert_eq!(queue.pop().unwrap()["x"], 9999);
        assert_eq!(queue.pop().unwrap()["op"], "navigate");
        assert!(queue.pop().is_none());
    }
    // phase: pointer_coalescing_never_erases_another_tabs_input
    {
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
}

#[test]
fn protocol_frames_reject_malformed_and_oversized_input() {
    // phase: malformed_known_events_are_errors_and_unknown_events_are_ignored
    {
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
    // phase: large_commands_are_rejected_before_encoding
    {
        assert!(!command_within_limits(&WebViewCommand::Navigate(
            "x".repeat(65537)
        )));
        assert!(!command_within_limits(&WebViewCommand::SetUserScripts(
            Arc::new(vec!["x".repeat(4 * 1024 * 1024 + 1)])
        )));
    }
    // phase: routed_frame_packet_is_accepted_and_malformed_tab_rejected
    {
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
    // phase: find_and_zoom_protocol_rejects_invalid_bounds
    {
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
            validate_event(
                &json!({"event":"find_result","id":1,"count":3,"active":1,"final":true})
            )
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
fn helper_failures_are_reported_once_and_reaped() {
    // phase: helper_eof_is_reported_once_without_engine_drop
    {
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
    // phase: malformed_output_terminates_and_reaps_helper
    {
        let (dir, host) = helper_fixture("printf '\\001\\000\\000\\000\\011'\nexec cat >/dev/null");
        let engine = ChromiumEngine::with_host(&host, &dir.path().join("profile")).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert!(engine.shared.failed.load(Ordering::Acquire));
        assert!(engine.tx.state.lock().unwrap().2);
        assert!(engine.drain_events().iter().any(|event| matches!(event, WebViewEvent::LoadFailed {error, ..} if error.contains("Invalid Chromium output"))));
    }
    // phase: rejected_navigation_is_visible_and_does_not_change_current_uri
    {
        let (dir, host) = helper_fixture("exec cat >/dev/null");
        let engine = ChromiumEngine::with_host(&host, &dir.path().join("profile")).unwrap();
        *engine.shared.uri.lock().unwrap() = "https://old.example".into();
        // Model the helper reporting failure before this new navigation attempt.
        engine.shared.failed.store(true, Ordering::Release);
        engine.tx.close();
        engine.send(WebViewCommand::Navigate("https://new.example".into()));
        assert_eq!(*engine.shared.uri.lock().unwrap(), "https://old.example");
        assert!(engine.drain_events().iter().any(|event| matches!(event, WebViewEvent::LoadFailed {uri,error} if uri == "https://new.example" && error.contains("not queued"))));
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn capability_history_and_download_payloads_round_trip_bounded() {
    // phase: capability_commands_and_events_round_trip
    {
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
        let update = json!({
            "event":"download","id":9,"state":"failed","path":"","name":"archive.zip",
            "url":"https://downloads.test/archive.zip","retryable":true,"received":128.0,"total":1024.0,
            "error":"connection reset"
        });
        assert!(validate_event(&update).is_ok());
        assert_eq!(
            decode_event(&update),
            Some(WebViewEvent::DownloadUpdated {
                id: 9,
                state: "failed".into(),
                path: String::new(),
                name: "archive.zip".into(),
                url: "https://downloads.test/archive.zip".into(),
                retryable: true,
                received: 128,
                total: 1024,
                error: "connection reset".into(),
            })
        );
        assert_eq!(
            encode_command(WebViewCommand::RetryDownload { id: 9 }),
            Some(json!({"op":"download_retry","id":9}))
        );
        assert_eq!(
            encode_command(WebViewCommand::CancelAllDownloads),
            Some(json!({"op":"download_cancel_all"}))
        );
    }
    // phase: session_history_payloads_are_bounded_and_keep_snapshot_identity
    {
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
    // phase: runtime_download_queue_coalesces_progress_and_is_bounded
    {
        let queue = Mutex::new(VecDeque::new());
        let update = |id, state: &str, received| WebViewEvent::DownloadUpdated {
            id,
            state: state.into(),
            path: "/tmp/file".into(),
            name: "file".into(),
            url: "https://downloads.test/file".into(),
            retryable: true,
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
        assert_eq!(
            decode_event(&json!({"event":"tab_detached","tab":2})),
            Some(WebViewEvent::TabDetached)
        );
        assert_eq!(
            decode_event(
                &json!({"event":"download_runtime","admitted":2,"detached":1,"accepting":true,"error":""})
            ),
            Some(WebViewEvent::DownloadRuntime {
                admitted: 2,
                detached: 1,
                accepting: true,
                error: String::new(),
            })
        );
    }
    // phase: download_byte_counts_reject_fractional_and_out_of_range_values
    {
        let base = json!({"event":"download", "id":1, "state":"progress",
        "path":"", "name":"fixture", "url":"https://example.test/file",
        "retryable":true, "received":12.0, "total":100.0, "error":""});
        assert!(validate_event(&base).is_ok());
        assert!(matches!(
            decode_event(&base),
            Some(WebViewEvent::DownloadUpdated {
                received: 12,
                total: 100,
                ..
            })
        ));
        for key in ["received", "total"] {
            for invalid in [-1.0, 0.5, 18_446_744_073_709_551_616.0, 1e30] {
                let mut event = base.clone();
                event[key] = json!(invalid);
                assert!(validate_event(&event).is_err(), "accepted {key}={invalid}");
                assert!(decode_event(&event).is_none(), "decoded {key}={invalid}");
            }
        }
    }
}

#[test]
#[cfg(unix)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn tab_endpoints_keep_capacity_and_frames_until_acknowledged() {
    // phase: tab_endpoints_keep_runtime_and_waker_after_first_endpoint_closes
    {
        let (dir, host) = helper_fixture("exec 3>&1; exec cat >/dev/null");
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
    // phase: native_capacity_is_reserved_until_closed_acknowledgment
    {
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
        tilo_test_env::wait_until("the closed event to release the native reservation", || {
            !first.runtime.routes.lock().unwrap().contains_key(&32)
        });
        first
            .create_tab()
            .expect("capacity released after closed acknowledgment");
    }
    // phase: hiding_retains_last_frame_for_recomposed_tab_until_next_paint
    {
        let (dir, host) = helper_fixture("exec cat >/dev/null");
        let engine = ChromiumEngine::with_host(&host, &dir.path().join("profile")).unwrap();
        *engine.frames().lock().unwrap() = Some(super::super::Frame {
            trace_id: None,
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
    // phase: detached_acknowledgment_releases_visible_route_before_physical_close
    {
        let mut packet = Vec::new();
        protocol::write_json(&mut packet, &json!({"event":"tab_detached","tab":2})).unwrap();
        let encoded: String = packet
            .iter()
            .map(|byte| format!("\\{:03o}", byte))
            .collect();
        // Detach frees the route before Drop. Create the replacement while the
        // owner Arc still lives so a Drop reservation cannot hide a leak.
        let (dir, host) = helper_fixture(&format!(
            "profile=${{1#--profile=}}\nwhile [ ! -f \"$profile/detach-ready\" ]; do sleep 0.01; done\nprintf '{encoded}'\nexec cat >/dev/null"
        ));
        let first = ChromiumEngine::with_host(&host, &dir.path().join("profile")).unwrap();
        let detached = first.create_tab().unwrap();
        let routes = first.runtime.routes.clone();
        std::fs::write(dir.path().join("profile/detach-ready"), b"ready").unwrap();
        let deadline = tilo_test_env::hang_deadline();
        let mut saw_detach = false;
        while !saw_detach {
            saw_detach = detached.drain_events().contains(&WebViewEvent::TabDetached);
            assert!(crate::time::now() < deadline, "detach acknowledgment");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            !routes.lock().unwrap().contains_key(&2),
            "route is retired before detach is observable"
        );
        // Capacity is released by the detach acknowledgment, while the owner
        // Arc is still alive — Drop must not be what frees the visible slot.
        assert_eq!(routes.lock().unwrap().len(), 1);
        first
            .create_tab()
            .expect("detached transfer owner does not use a visible slot");
        drop(detached);
        assert_eq!(
            routes.lock().unwrap().len(),
            2,
            "dropping a detached owner must not reserve capacity again"
        );
    }
    // phase: admitted_close_does_not_reinsert_capacity_when_endpoint_drops
    {
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
            tilo_test_env::wait_until("the native close acknowledgment", || {
                tab.drain_events().contains(&WebViewEvent::TabClosed)
            });
            assert!(
                !first.runtime.routes.lock().unwrap().contains_key(&id),
                "route is retired before close acknowledgment is observable"
            );
            drop(tab);
            let count = first.runtime.routes.lock().unwrap().len();
            assert_eq!(
                count, 1,
                "dropping an admitted closed tab must not reserve its capacity again"
            );
        }
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn site_and_credential_protocols_are_bounded_and_carry_no_secrets() {
    // phase: site_protocol_roundtrips_bounded_status_and_rejects_malformed_fields
    {
        let status = serde_json::json!({"event":"site_status","request_id":1,"origin":"https://example.test","generation":2.0,"blocked":4.0,"global_enabled":true,"excepted":false,"effective":true,"security":"secure","certificate_status":0,"tls_version":6,"content_status":0,"subject":"example.test","issuer":"fixture","expires":0.0,"permissions":{"camera":"ask","microphone":"ask","location":"ask","notifications":"blocked"}});
        assert!(validate_event(&status).is_ok());
        assert!(
            matches!(decode_event(&status),Some(WebViewEvent::SiteStatus(s)) if s.blocked==4 && s.generation==2 && s.permissions.len()==4)
        );
        for (key, bad) in [
            ("origin", serde_json::json!("x".repeat(8193))),
            ("generation", serde_json::json!(-1)),
            ("blocked", serde_json::json!(1.5)),
            ("issuer", serde_json::json!("x".repeat(513))),
            ("security", serde_json::json!("maybe")),
            (
                "permissions",
                serde_json::json!({"notifications":"anything"}),
            ),
        ] {
            let mut invalid = status.clone();
            invalid[key] = bad;
            assert!(validate_event(&invalid).is_err(), "{key}");
        }
        assert!(!command_within_limits(&WebViewCommand::SetSiteExceptions(
            vec!["https://a.test".into(); 1025]
        )));
        assert!(!command_within_limits(&WebViewCommand::SiteAction {
            request_id: 1,
            origin: "https://a.test".into(),
            generation: 0,
            action: "clear".into()
        }));
        let command = WebViewCommand::SiteAction {
            request_id: 7,
            origin: "https://a.test".into(),
            generation: 2,
            action: "clear".into(),
        };
        assert!(command_within_limits(&command));
        assert_eq!(
            encode_command(command).unwrap(),
            serde_json::json!({"op":"site_action","request_id":7,"origin":"https://a.test","generation":2,"action":"clear"})
        );
    }
    // phase: scoped_site_reload_roundtrips_with_document_bounds
    {
        let command = WebViewCommand::SiteAction {
            request_id: 7,
            origin: "https://a.test".into(),
            generation: 2,
            action: "reload".into(),
        };
        assert!(command_within_limits(&command));
        assert_eq!(
            encode_command(command).unwrap(),
            json!({"op":"site_action","request_id":7,"origin":"https://a.test","generation":2,"action":"reload"})
        );
        let reply = json!({"event":"site_result","request_id":7,"origin":"https://a.test","generation":2,"action":"reload","success":false,"message":"Page changed"});
        assert!(validate_event(&reply).is_ok());
        assert!(
            matches!(decode_event(&reply), Some(WebViewEvent::SiteResult { generation: 2, success: false, action, .. }) if action == "reload")
        );
        for generation in [0, 9_007_199_254_740_992] {
            assert!(!command_within_limits(&WebViewCommand::SiteAction {
                request_id: 7,
                origin: "https://a.test".into(),
                generation,
                action: "reload".into(),
            }));
        }
    }
    // phase: credential_protocol_encodes_actions_without_secret_fields
    {
        use crate::webview::{CredentialAction, CredentialRequest};
        let request = CredentialRequest {
            request_id: 7,
            origin: "https://example.test".into(),
            document: "2".into(),
            action: CredentialAction::Save {
                capture: "3".into(),
            },
        };
        let command = WebViewCommand::Credentials(request.clone());
        assert!(command_within_limits(&command));
        assert_eq!(
            encode_command(command).unwrap(),
            json!({"op":"credential_action","request_id":7,
        "origin":"https://example.test","document":"2","action":"save","capture":"3"})
        );
        let mut stale = request.clone();
        stale.document = "0".into();
        assert!(!command_within_limits(&WebViewCommand::Credentials(stale)));
        let mut bad = request;
        bad.action = CredentialAction::Fill {
            form: "01".into(),
            account: "account".into(),
        };
        assert!(!command_within_limits(&WebViewCommand::Credentials(bad)));
    }
    // phase: credential_protocol_rejects_secret_or_unbounded_reply_fields
    {
        let base = json!({"event":"credential_result","tab":1,"request_id":7,"action":"capture",
        "origin":"https://example.test","document":"2","success":true,"message":"Choose Save",
        "capture":"3","username":"synthetic-user"});
        assert!(validate_event(&base).is_ok());
        assert!(
            matches!(decode_event(&base),Some(WebViewEvent::CredentialResult(reply)) if reply.capture.as_deref()==Some("3"))
        );
        for (key, value) in [
            ("password", json!("must-not-cross")),
            ("document", json!("9007199254740992")),
            ("username", json!("x".repeat(1025))),
            ("success", json!("yes")),
        ] {
            let mut bad = base.clone();
            bad[key] = value;
            assert!(validate_event(&bad).is_err(), "{key}");
        }
        let mut list = json!({"event":"credential_result","request_id":1,"action":"list","origin":"https://example.test",
        "document":"2","success":true,"message":"Done","accounts":[{"id":"one","username":"user"}]});
        assert!(validate_event(&list).is_ok());
        list["accounts"][0]["password"] = json!("must-not-cross");
        assert!(validate_event(&list).is_err());
        list["accounts"] = json!(vec![json!({"id":"one","username":"user"}); 65]);
        assert!(validate_event(&list).is_err());
    }
}

#[test]
fn poisoned_queues_and_routes_fail_closed_without_panicking() {
    // phase: poisoned_command_queue_fails_closed_without_panicking
    {
        let queue = CommandQueue::default();
        let poison = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = queue.state.lock().unwrap();
            panic!("synthetic queue failure");
        }));
        assert!(poison.is_err());
        assert_eq!(
            queue.push(json!({"op":"navigate"})),
            Err("command channel poisoned")
        );
        assert!(queue.pop().is_none());
        queue.close();
    }
    // phase: poisoned_routes_still_notify_live_tabs_of_helper_failure
    {
        let shared = Arc::new(Shared::default());
        let routes: Routes = Arc::new(Mutex::new(HashMap::from([(1, Arc::downgrade(&shared))])));
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = routes.lock().unwrap();
            panic!("synthetic route failure");
        }));
        fail_routes(&routes, "fixture helper disconnected");
        assert!(shared.failed.load(Ordering::Acquire));
        assert!(shared.events.lock().unwrap().iter().any(|event| matches!(
            event, WebViewEvent::LoadFailed { error, .. } if error == "fixture helper disconnected"
        )));
    }
    // phase: poisoned_native_event_routes_return_error_instead_of_panicking
    {
        for event in [
            json!({"event":"closed","tab":1}),
            json!({"event":"download_runtime", "admitted":0,"detached":0,"accepting":true,"error":""}),
        ] {
            let routes: Routes = Arc::new(Mutex::new(HashMap::new()));
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _guard = routes.lock().unwrap();
                panic!("synthetic event route failure");
            }));
            let downloads = Mutex::new(VecDeque::new());
            let (ready, _receiver) = sync_channel(1);
            assert!(route_native_event(&event, &routes, &downloads, &ready).is_err());
        }
    }
}

#[test]
#[cfg(unix)]
fn poisoned_tab_state_never_panics_or_poisons_routes() {
    // phase: poisoned_tab_routes_do_not_panic_during_engine_drop
    {
        let (dir, host) = helper_fixture("exec cat >/dev/null");
        let engine = ChromiumEngine::with_host(&host, &dir.path().join("profile")).unwrap();
        let routes = engine.runtime.routes.clone();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = routes.lock().unwrap();
            panic!("synthetic route failure");
        }));
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(engine))).is_ok());
    }
    // phase: poisoned_tab_waker_returns_creation_error_without_poisoning_routes
    {
        let (dir, host) = helper_fixture("exec cat >/dev/null");
        let engine = ChromiumEngine::with_host(&host, &dir.path().join("profile")).unwrap();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = engine.shared.waker.lock().unwrap();
            panic!("synthetic waker failure");
        }));
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| engine.create_tab()));
        let routes_poisoned = engine.runtime.routes.is_poisoned();
        let dropped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(engine)));
        assert!(outcome.is_ok(), "tab creation panicked");
        assert!(outcome.unwrap().is_err());
        assert!(!routes_poisoned);
        assert!(dropped.is_ok());
    }
    // phase: poisoned_supervisor_slot_can_still_reap_its_child
    {
        let child = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        let slot = Mutex::new(Some(child));
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = slot.lock().unwrap();
            panic!("synthetic supervisor slot failure");
        }));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            take_supervisor_child(&slot)
        }));
        if result.is_err() {
            if let Some(mut child) = slot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            {
                let _ = child.wait();
            }
        }
        let mut child = result
            .expect("taking supervisor child panicked")
            .expect("child was lost");
        assert!(child.wait().unwrap().success());
        assert!(take_supervisor_child(&slot).is_none());
    }
    // phase: poisoned_new_tab_queue_preserves_already_admitted_requests
    {
        let (dir, host) = helper_fixture("exec cat >/dev/null");
        let engine = ChromiumEngine::with_host(&host, &dir.path().join("profile")).unwrap();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut requests = engine.shared.open_tabs.lock().unwrap();
            requests.push_back(("https://example.test".into(), true));
            panic!("synthetic new-tab queue failure");
        }));
        assert_eq!(
            engine.take_new_tab_requests(),
            vec![("https://example.test".into(), true)]
        );
        assert!(engine.take_new_tab_requests().is_empty());
    }
}

#[test]
fn page_requests_are_encoded_bounded_and_always_answered() {
    use crate::webview::{PageFailure, PageKey, PageReply, PageRequest};
    // phase: page_requests_encode_to_the_host_page_op
    {
        let command = WebViewCommand::Page {
            request_id: 4,
            request: PageRequest::Fill {
                element: "d2e7".into(),
                text: "ada@example.test".into(),
            },
        };
        assert!(command_within_limits(&command));
        assert_eq!(
            encode_command(command),
            Some(
                json!({"op":"page","request_id":4,"action":"fill","element":"d2e7","text":"ada@example.test"})
            )
        );
        for invalid in [
            WebViewCommand::Page {
                request_id: 0,
                request: PageRequest::Outline,
            },
            WebViewCommand::Page {
                request_id: 5,
                request: PageRequest::Click {
                    element: "button[type=submit]".into(),
                },
            },
            WebViewCommand::Page {
                request_id: 6,
                request: PageRequest::Press {
                    element: None,
                    key: PageKey {
                        key: "NotAKey".into(),
                        modifiers: crate::webview::KeyModifiers::default(),
                    },
                },
            },
        ] {
            assert!(!command_within_limits(&invalid), "{invalid:?}");
        }
    }
    // phase: page_results_validate_and_decode
    {
        let event = json!({"event":"page_result","tab":2,"request_id":9,"ok":true,
            "url":"https://shop.test/cart","message":"Clicked","outline":null,"navigated":true,"loading":true});
        assert!(validate_event(&event).is_ok());
        assert_eq!(
            decode_event(&event),
            Some(WebViewEvent::PageResult {
                request_id: 9,
                result: Ok(PageReply {
                    url: "https://shop.test/cart".into(),
                    message: "Clicked".into(),
                    outline: None,
                    navigated: true,
                    loading: true,
                }),
            })
        );
        let malformed = json!({"event":"page_result","request_id":9,"ok":true,"message":"x"});
        assert!(validate_event(&malformed).is_err());
    }
    // phase: engines_that_cannot_run_page_requests_answer_them
    {
        let unavailable = UnavailableEngine::new("missing runtime".into());
        unavailable.send(WebViewCommand::Page {
            request_id: 3,
            request: PageRequest::Outline,
        });
        assert_eq!(
            unavailable.drain_events(),
            vec![WebViewEvent::PageResult {
                request_id: 3,
                result: Err(PageFailure::new("unavailable", "missing runtime")),
            }]
        );
        let (dir, host) = helper_fixture("exec cat >/dev/null");
        let engine = ChromiumEngine::with_host(&host, &dir.path().join("profile")).unwrap();
        engine.send(WebViewCommand::Page {
            request_id: 8,
            request: PageRequest::Submit {
                element: "not a ref".into(),
            },
        });
        let answered = engine.drain_events().into_iter().any(|event| {
            matches!(
                event,
                WebViewEvent::PageResult { request_id: 8, result: Err(PageFailure { code, .. }) }
                    if code == "invalid_request"
            )
        });
        assert!(answered, "a refused page request must still be answered");
    }
}
