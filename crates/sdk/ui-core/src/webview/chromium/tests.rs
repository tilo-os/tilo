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
        crate::testing::write_executable(
            &host,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nprintf '{encoded}'\ncat >/dev/null\nprintf done > '{}'\n",
                arguments.display(),
                exited.display()
            ),
        );
        let root = private_root();

        let engine = ChromiumEngine::with_private_host(&host, &root).unwrap();
        // The engine is handed back while its host starts.
        let launched = tilo_test_env::wait_for("the helper's arguments", || {
            std::fs::read_to_string(&arguments)
                .ok()
                .filter(|text| text.contains("--profile="))
        });
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
        let missing_root = private_root();
        assert!(
            ChromiumEngine::with_private_host(Path::new("/missing/tilo-host"), &missing_root)
                .is_err()
        );
        assert!(!missing_root.exists());

        let fixture = tempfile::tempdir().unwrap();
        let host = fixture.path().join("not-ready");
        crate::testing::write_executable(&host, "#!/bin/sh\nexit 7\n");
        // A host that exits before it is ready fails the page once it is
        // reaped; the window never waited for it.
        let readiness_root = private_root();
        let engine = ChromiumEngine::with_private_host(&host, &readiness_root).unwrap();
        wait_until_removed(&readiness_root);
        assert!(!readiness_root.exists());
        assert!(engine.shared.failed.load(Ordering::Acquire));
        assert!(
            engine
                .drain_events()
                .iter()
                .any(|event| matches!(event, WebViewEvent::LoadFailed { .. }))
        );
    }
    // phase: private_packaged_helper_discovery_failure_removes_root
    {
        let fixture = tempfile::tempdir().unwrap();
        let root = private_root();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "webview::chromium::tests::private_packaged_helper_discovery_failure_child",
                "--ignored",
            ])
            .env_remove(runtime::HOST_OVERRIDE_ENV)
            .env(runtime::RUNTIME_DIR_ENV, fixture.path())
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
        // Keep the protocol output open while stdin is consumed. Redirecting
        // stdout alone sends EOF to the production frame reader, which can
        // reap this supposedly-live helper before the child checks its root.
        let (_fixture, host) = helper_fixture("exec 3>&1; exec cat >/dev/null");
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
    // phase: every_event_the_host_sends_is_well_formed
    // A malformed known event ends the engine and every tab with it, so what
    // the host really sends must pass: a page can cause each of these.
    {
        // The request filter's own trouble names no page.
        let filter =
            json!({"event":"error","message":"Native request matcher failed to check a resource"});
        assert!(validate_event(&filter).is_ok());
        assert_eq!(
            decode_event(&filter),
            None,
            "a diagnostic, not a failed page"
        );
        // A page's export: its whole file is the download's address.
        let export = json!({"event":"download", "id":1, "state":"progress",
        "path":"", "name":"export.csv", "url":format!("data:text/csv,{}", "a".repeat(20_000)),
        "retryable":true, "received":0.0, "total":20_000.0, "error":""});
        assert!(validate_event(&export).is_ok());
        assert!(matches!(
            decode_event(&export),
            Some(WebViewEvent::DownloadUpdated { url, retryable: false, .. }) if url.is_empty()
        ));
        let long = format!("https://example.test/?{}", "a".repeat(70_000));
        let outcome = json!({"event":"navigation_outcome","request_id":1,"url":long});
        assert!(validate_event(&outcome).is_ok());
        assert!(matches!(
            decode_event(&outcome),
            Some(WebViewEvent::NavigationOutcome { uri, .. }) if uri == long
        ));
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
        assert_eq!(&*frame.into_frame(tab).rgba, &[30, 20, 10, 255]);
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

/// Sets `$profile` in a helper script from its `--profile=` argument,
/// wherever the engine put it: a governed process, as any test process is
/// once one of its tests governs audio, passes its audio switch first.
#[cfg(unix)]
const PROFILE_FROM_ARGUMENTS: &str =
    "for argument; do case $argument in --profile=*) profile=${argument#--profile=};; esac; done\n";

#[cfg(unix)]
pub(super) fn helper_fixture(trailing: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("helper");
    let mut bytes = Vec::new();
    protocol::write_json(
        &mut bytes,
        &json!({"event":"ready","cef_version":"152","chromium_version":"152"}),
    )
    .unwrap();
    let encoded: String = bytes.iter().map(|byte| format!("\\{:03o}", byte)).collect();
    crate::testing::write_executable(
        &path,
        &format!("#!/bin/sh\nprintf '{encoded}'\n{trailing}\n"),
    );
    (dir, path)
}

#[test]
#[cfg(unix)]
fn helper_failures_are_reported_once_and_reaped() {
    // phase: helper_eof_is_reported_once_without_engine_drop
    {
        let (dir, host) = helper_fixture("exit 0");
        let engine = ChromiumEngine::with_host(&host, &dir.path().join("profile")).unwrap();
        tilo_test_env::wait_until("the helper's exit to be reaped", || {
            engine.shared.failed.load(Ordering::Acquire) && engine.tx.state.lock().unwrap().2
        });
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
        let (dir, host) =
            helper_fixture("printf '\\001\\000\\000\\000\\011'\nexec 3>&1; exec cat >/dev/null");
        let engine = ChromiumEngine::with_host(&host, &dir.path().join("profile")).unwrap();
        tilo_test_env::wait_until("the malformed helper to be reaped", || {
            engine.shared.failed.load(Ordering::Acquire) && engine.tx.state.lock().unwrap().2
        });
        assert!(engine.shared.failed.load(Ordering::Acquire));
        assert!(engine.tx.state.lock().unwrap().2);
        assert!(engine.drain_events().iter().any(|event| matches!(event, WebViewEvent::LoadFailed {error, ..} if error.contains("Invalid Chromium output"))));
    }
    // phase: rejected_navigation_is_visible_and_does_not_change_current_uri
    {
        let (dir, host) = helper_fixture("exec 3>&1; exec cat >/dev/null");
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

/// A Browser window left open costs the CPU nothing: the engine host's
/// supervisor sleeps until the host exits, fails or is asked to stop,
/// rather than waking on a timer to look. Its exit is still seen at once,
/// even when a process the host started keeps its output open.
#[test]
#[cfg(target_os = "linux")]
fn an_idle_host_never_wakes_its_supervisor_and_its_exit_is_seen_at_once() {
    fn checks(engine: &ChromiumEngine) -> u32 {
        engine.shared.supervisor_checks.load(Ordering::Relaxed)
    }
    fn wait_for(what: &str, done: impl Fn() -> bool) {
        let began = std::time::Instant::now();
        while !done() {
            assert!(began.elapsed() < tilo_test_env::hang_bound(), "{what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn untimed_waits(engine: &ChromiumEngine) -> u32 {
        engine
            .shared
            .supervisor_untimed_waits
            .load(Ordering::Acquire)
    }
    // phase: an_idle_host_is_looked_at_once
    {
        // Keeps its output open (fd 3) while it idles on its input.
        let (dir, host) = helper_fixture("exec 3>&1\nexec cat >/dev/null");
        let engine = ChromiumEngine::with_host(&host, &dir.path().join("profile")).unwrap();
        wait_for("the supervisor to wait on the ready host", || {
            untimed_waits(&engine) >= 1
        });
        // This engine's own supervisor: in a test process shared with other
        // engines, a thread found by name may be another's, gone by now.
        let supervisor = engine
            .shared
            .supervisor_thread
            .get()
            .expect("the supervisor names its thread before it waits")
            .to_string();
        tilo_test_env::idle::assert_stays_asleep(&supervisor, "the idle host's supervisor");
        assert_eq!(checks(&engine), 1, "an idle host woke its supervisor");
        assert_eq!(untimed_waits(&engine), 1);
        assert!(!engine.shared.failed.load(Ordering::Acquire));
    }
    // phase: an_exit_wakes_the_supervisor_though_the_output_stays_open
    {
        // A process the host started holds its output until `released`,
        // and the host exits on `go`.
        let (dir, host) = helper_fixture(&format!(
            "{PROFILE_FROM_ARGUMENTS}\
             (while [ -d \"$profile\" ] && [ ! -e \"$profile/released\" ]; do sleep 0.05; done) &\n\
             while [ ! -e \"$profile/go\" ]; do sleep 0.01; done\n\
             exit 0",
        ));
        let profile = dir.path().join("profile");
        let engine = ChromiumEngine::with_host(&host, &profile).unwrap();
        wait_for("the supervisor to wait on the ready host", || {
            untimed_waits(&engine) >= 1
        });
        std::fs::write(profile.join("go"), b"").unwrap();
        wait_for("the host's exit is reported", || {
            engine.shared.failed.load(Ordering::Acquire)
        });
        // The output is still held open: only the exit itself can have
        // woken the supervisor.
        assert!(!profile.join("released").exists());
        assert_eq!(
            checks(&engine),
            2,
            "the supervisor looked other than at its exit"
        );
        assert!(engine.drain_events().iter().any(
            |event| matches!(event, WebViewEvent::LoadFailed { error, .. } if error == "Chromium engine exited")
        ));
        std::fs::write(profile.join("released"), b"").unwrap();
    }
}

/// Opening a Browser window, the first of its profile, starts the engine
/// host, which takes a second or more to say it is ready. The window must not
/// wait for it: the engine is handed back while the host is still starting,
/// and what the page asks for meanwhile reaches the host once it reads.
#[test]
#[cfg(unix)]
fn an_engine_is_ready_to_use_before_its_host_is() {
    let mut ready = Vec::new();
    protocol::write_json(
        &mut ready,
        &json!({"event":"ready","cef_version":"152","chromium_version":"152"}),
    )
    .unwrap();
    let encoded: String = ready.iter().map(|byte| format!("\\{:03o}", byte)).collect();
    let dir = tempfile::tempdir().unwrap();
    let host = dir.path().join("host");
    // Reads nothing and says nothing until the test lets it start.
    crate::testing::write_executable(
        &host,
        &format!(
            "#!/bin/sh\n\
             {PROFILE_FROM_ARGUMENTS}\
             while [ ! -f \"$profile/start\" ]; do sleep 0.01; done\n\
             printf '{encoded}'\n\
             exec 3>&1\n\
             exec cat > \"$profile/commands\"\n"
        ),
    );
    let profile = dir.path().join("profile");
    // Returns while the host waits: a constructor that waited for `ready`
    // would give up after its 20 s and fail here.
    let engine = ChromiumEngine::with_host(&host, &profile).unwrap();
    let profile = profile.canonicalize().unwrap();
    assert!(engine.supports_text_input());
    engine.send(WebViewCommand::Navigate("https://first.test/".into()));
    std::fs::write(profile.join("start"), b"").unwrap();
    wait_for_command(
        &profile,
        "the navigation sent before the host was ready",
        |command| command["op"] == "navigate" && command["url"] == "https://first.test/",
    );
    assert!(!engine.shared.failed.load(Ordering::Acquire));
    assert!(
        !engine
            .drain_events()
            .iter()
            .any(|event| matches!(event, WebViewEvent::LoadFailed { .. }))
    );
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
            encode_command(WebViewCommand::StartDownload {
                uri: "https://downloads.test/archive.zip".into(),
            }),
            Some(json!({"op":"download_start","url":"https://downloads.test/archive.zip"}))
        );
        for uri in ["file:///etc/passwd", "https://", "javascript:alert(1)"] {
            assert!(!command_within_limits(&WebViewCommand::StartDownload {
                uri: uri.into(),
            }));
        }
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
            "{PROFILE_FROM_ARGUMENTS}while [ ! -f \"$profile/ack-close\" ]; do sleep 0.01; done\nprintf '{encoded}'\nexec 3>&1\nexec cat >/dev/null"
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
        let (dir, host) = helper_fixture("exec 3>&1; exec cat >/dev/null");
        let engine = ChromiumEngine::with_host(&host, &dir.path().join("profile")).unwrap();
        *engine.frames().lock().unwrap() = Some(super::super::Frame {
            trace_id: None,
            damage: None,
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
            "{PROFILE_FROM_ARGUMENTS}while [ ! -f \"$profile/detach-ready\" ]; do sleep 0.01; done\nprintf '{encoded}'\nexec 3>&1; exec cat >/dev/null"
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
        let mut script = String::from(PROFILE_FROM_ARGUMENTS);
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
        script.push_str("exec 3>&1; exec cat >/dev/null");
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
        let (dir, host) = helper_fixture("exec 3>&1; exec cat >/dev/null");
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
        let (dir, host) = helper_fixture("exec 3>&1; exec cat >/dev/null");
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
            if let Some(mut child) = slot.lock_recover().take() {
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
        let (dir, host) = helper_fixture("exec 3>&1; exec cat >/dev/null");
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
    use crate::webview::{PageFailure, PageKey, PageNavigation, PageReply, PageRequest};
    // phase: page_requests_encode_to_the_host_page_op
    {
        let command = WebViewCommand::Page {
            request_id: 4,
            request: PageRequest::Fill {
                element: "d2e7".into(),
                text: "ada@example.test".into(),
                secret: false,
            },
        };
        assert!(command_within_limits(&command));
        assert_eq!(
            encode_command(command),
            Some(
                json!({"op":"page","request_id":4,"action":"fill","element":"d2e7","text":"ada@example.test","secret":false})
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
                    data: None,
                    navigation: PageNavigation::Started,
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
        let (dir, host) = helper_fixture("exec 3>&1; exec cat >/dev/null");
        let engine = ChromiumEngine::with_host(&host, &dir.path().join("profile")).unwrap();
        engine.send(WebViewCommand::Page {
            request_id: 8,
            request: PageRequest::Submit {
                element: "not a ref".into(),
                cross_origin: false,
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

/// A host that records its launches and every command it receives in the
/// profile, and writes `packets` once the test creates `emit` there.
#[cfg(unix)]
fn shared_fixture(packets: &[u8]) -> (tempfile::TempDir, PathBuf) {
    let encoded: String = packets
        .iter()
        .map(|byte| format!("\\{:03o}", byte))
        .collect();
    helper_fixture(&format!(
        "{PROFILE_FROM_ARGUMENTS}\
         echo spawned >> \"$profile/spawns\"\n\
         exec 3<&0\n\
         cat <&3 > \"$profile/commands\" &\n\
         reader=$!\n\
         while [ ! -f \"$profile/emit\" ] && kill -0 $reader 2>/dev/null; do sleep 0.01; done\n\
         if [ -f \"$profile/emit\" ]; then printf '{encoded}'; fi\n\
         wait\n\
         printf exited > \"$profile/exited\""
    ))
}

#[cfg(unix)]
fn host_spawns(profile: &Path) -> usize {
    std::fs::read_to_string(profile.join("spawns"))
        .unwrap_or_default()
        .lines()
        .count()
}

/// Wait until the hosts started for `profile` have all recorded their start,
/// expecting `expected` of them. A spawned host records itself once it runs,
/// which can be after the engine that started it has returned.
#[cfg(unix)]
fn assert_host_spawns(profile: &Path, expected: usize) {
    tilo_test_env::wait_until(format!("{expected} host start(s)"), || {
        let spawns = host_spawns(profile);
        assert!(
            spawns <= expected,
            "{spawns} hosts started, expected {expected}"
        );
        spawns == expected
    });
}

#[cfg(unix)]
fn host_commands(profile: &Path) -> Vec<Value> {
    let mut reader =
        std::io::Cursor::new(std::fs::read(profile.join("commands")).unwrap_or_default());
    let mut commands = Vec::new();
    while let Ok(Some(protocol::Packet::Json(command))) = protocol::read_packet(&mut reader) {
        commands.push(command);
    }
    commands
}

#[cfg(unix)]
fn wait_for_command(profile: &Path, what: &str, matches: impl Fn(&Value) -> bool) {
    tilo_test_env::wait_until(what, || host_commands(profile).iter().any(&matches));
}

#[cfg(unix)]
fn disconnected(events: &[WebViewEvent]) -> bool {
    events.iter().any(|event| {
        matches!(event, WebViewEvent::LoadFailed { error, .. } if error == "Chromium engine disconnected")
    })
}

#[test]
#[cfg(unix)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn engines_on_one_profile_share_one_host() {
    let mut packets = Vec::new();
    protocol::write_json(
        &mut packets,
        &json!({"event":"title","tab":2,"title":"Client page"}),
    )
    .unwrap();
    protocol::write_tab_frame(
        &mut packets,
        2,
        &protocol::WireFrame {
            bgra: vec![3, 2, 1, 255],
            width: 1,
            height: 1,
        },
    )
    .unwrap();
    protocol::write_json(
        &mut packets,
        &json!({"event":"title","tab":1,"title":"Owner page"}),
    )
    .unwrap();
    protocol::write_json(
        &mut packets,
        &json!({"event":"download_runtime","admitted":1,"detached":0,"accepting":true,"error":""}),
    )
    .unwrap();
    let (dir, host) = shared_fixture(&packets);
    let profile = dir.path().join("profile");
    let owner = ChromiumEngine::with_host(&host, &profile).unwrap();
    let client = ChromiumEngine::with_host(&host, &profile).unwrap();
    // phase: the_second_engine_joins_the_running_host
    {
        assert!(owner.runtime.link.is_none());
        assert!(client.runtime.link.is_some());
        assert_host_spawns(&profile, 1);
        assert_eq!(
            (owner.tab, client.tab),
            (1, 2),
            "a client's first endpoint is a new tab"
        );
    }
    // phase: client_commands_reach_the_host_under_the_owner_allocated_tab
    {
        client.send(WebViewCommand::Navigate("https://client.test/".into()));
        wait_for_command(&profile, "the client's tab to be created", |command| {
            command["op"] == "create_tab" && command["tab"] == 2
        });
        wait_for_command(&profile, "the client's navigation", |command| {
            command["op"] == "navigate"
                && command["tab"] == 2
                && command["url"] == "https://client.test/"
        });
    }
    // phase: host_events_and_frames_reach_only_their_tabs_process
    {
        std::fs::write(profile.join("emit"), b"go").unwrap();
        let client_title = WebViewEvent::TitleChanged("Client page".into());
        let owner_title = WebViewEvent::TitleChanged("Owner page".into());
        let mut client_events = Vec::new();
        tilo_test_env::wait_until("the client's title and frame", || {
            client_events.extend(client.drain_events());
            client_events.contains(&client_title) && client.frames().lock().unwrap().is_some()
        });
        let rgba = client
            .frames()
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .rgba
            .clone();
        assert_eq!(&*rgba, &[1, 2, 3, 255]);
        let mut owner_events = Vec::new();
        tilo_test_env::wait_until("the owner's title", || {
            owner_events.extend(owner.drain_events());
            owner_events.contains(&owner_title)
        });
        assert!(!owner_events.contains(&client_title));
        assert!(!client_events.contains(&owner_title));
        assert!(owner.frames().lock().unwrap().is_none());
    }
    // phase: download_events_reach_every_process
    {
        let runtime = |event: &WebViewEvent| {
            matches!(
                event,
                WebViewEvent::DownloadRuntime {
                    admitted: 1,
                    accepting: true,
                    ..
                }
            )
        };
        tilo_test_env::wait_until("the client's download runtime", || {
            client.drain_download_events().iter().any(runtime)
        });
        tilo_test_env::wait_until("the owner's download runtime", || {
            owner.drain_download_events().iter().any(runtime)
        });
    }
    // phase: every_process_takes_tabs_from_the_one_counter
    {
        let _client_tab = client.create_tab().unwrap();
        let _owner_tab = owner.create_tab().unwrap();
        wait_for_command(&profile, "the client's second tab", |command| {
            command["op"] == "create_tab" && command["tab"] == 3
        });
        wait_for_command(&profile, "the owner's second tab", |command| {
            command["op"] == "create_tab" && command["tab"] == 4
        });
    }
    // phase: a_client_cannot_address_another_processs_tab
    {
        client
            .tx
            .push(json!({"op":"navigate","tab":1,"url":"https://intruder.test/"}))
            .unwrap();
        client.tx.push(json!({"op":"shutdown","tab":2})).unwrap();
        client.send(WebViewCommand::Navigate("https://barrier.test/".into()));
        wait_for_command(
            &profile,
            "the navigation after the refused ones",
            |command| command["url"] == "https://barrier.test/",
        );
        let commands = host_commands(&profile);
        assert!(
            !commands
                .iter()
                .any(|command| command["url"] == "https://intruder.test/")
        );
        assert!(!commands.iter().any(|command| command["op"] == "shutdown"));
    }
}

#[test]
#[cfg(unix)]
fn the_tab_bound_spans_processes_and_a_lost_client_closes_its_tabs() {
    let (dir, host) = shared_fixture(&[]);
    let profile = dir.path().join("profile");
    let owner = ChromiumEngine::with_host(&host, &profile).unwrap();
    let client = ChromiumEngine::with_host(&host, &profile).unwrap();
    let client_tabs: Vec<_> = (0..15).map(|_| client.create_tab().unwrap()).collect();
    // phase: the_32_tab_bound_counts_every_process
    {
        let _owner_tabs: Vec<_> = (0..15).map(|_| owner.create_tab().unwrap()).collect();
        let error = client.create_tab().err().unwrap();
        assert!(error.contains("At most 32"), "{error}");
        assert!(owner.create_tab().is_err());
    }
    // phase: a_lost_client_has_its_tabs_closed_by_the_owner
    {
        let remote: Vec<u32> = owner
            .runtime
            .routes
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, route)| {
                route
                    .upgrade()
                    .is_some_and(|shared| shared.remote.is_some())
            })
            .map(|(tab, _)| *tab)
            .collect();
        assert_eq!(remote.len(), 16);
        client.runtime.link.as_ref().unwrap().disconnect();
        tilo_test_env::wait_until("the client to learn it is disconnected", || {
            disconnected(&client.drain_events())
        });
        assert!(disconnected(&client_tabs[0].drain_events()));
        tilo_test_env::wait_until("a close for every client tab", || {
            let commands = host_commands(&profile);
            remote.iter().all(|tab| {
                commands
                    .iter()
                    .any(|command| command["op"] == "close_tab" && command["tab"] == *tab)
            })
        });
        let routes = owner.runtime.routes.lock().unwrap();
        assert!(
            remote.iter().all(|tab| routes
                .get(tab)
                .is_some_and(|route| route.upgrade().is_none())),
            "closing tabs keep their capacity until the host acknowledges"
        );
    }
}

#[test]
#[cfg(unix)]
fn owners_serve_until_idle_and_replace_dead_owners_sockets() {
    let (dir, host) = shared_fixture(&[]);
    let profile = dir.path().join("profile");
    std::fs::create_dir_all(&profile).unwrap();
    let socket = profile.canonicalize().unwrap().with_extension("sock");
    // phase: a_dead_owners_socket_and_lock_are_taken_over
    let owner = {
        drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());
        std::fs::write(socket.with_extension("lock"), b"").unwrap();
        let owner = ChromiumEngine::with_host(&host, &profile).unwrap();
        assert!(owner.runtime.link.is_none());
        assert_host_spawns(&profile, 1);
        owner
    };
    // phase: the_owner_keeps_the_host_while_a_client_uses_it
    {
        let client = ChromiumEngine::with_host(&host, &profile).unwrap();
        let server = owner.runtime.server.get().unwrap().clone();
        let process = owner.runtime.process.clone();
        drop(owner);
        assert!(!process.stopping.load(Ordering::Acquire));
        let served = Arc::new(AtomicBool::new(false));
        let done = served.clone();
        let serving = std::thread::spawn(move || {
            server.serve_until_idle();
            done.store(true, Ordering::Release);
        });
        client.send(WebViewCommand::Navigate("https://after-owner.test/".into()));
        wait_for_command(&profile, "the client's navigation", |command| {
            command["url"] == "https://after-owner.test/"
        });
        assert!(
            !served.load(Ordering::Acquire),
            "a client is still connected"
        );
        drop(client);
        tilo_test_env::wait_until("serving to end with the last client", || {
            served.load(Ordering::Acquire)
        });
        serving.join().unwrap();
        assert!(
            profile.join("exited").exists(),
            "the idle host was shut down"
        );
        assert!(!socket.exists());
    }
    // phase: the_next_engine_starts_a_fresh_host
    {
        let next = ChromiumEngine::with_host(&host, &profile).unwrap();
        assert!(next.runtime.link.is_none());
        assert_host_spawns(&profile, 2);
    }
    // phase: a_profile_deeper_than_a_socket_address_is_owned_and_joined
    {
        let deep = dir.path().join("x".repeat(120)).join("profile");
        std::fs::create_dir_all(&deep).unwrap();
        let socket = deep.canonicalize().unwrap().with_extension("sock");
        assert!(std::os::unix::net::SocketAddr::from_pathname(&socket).is_err());
        let owner = ChromiumEngine::with_host(&host, &deep).unwrap();
        assert!(owner.runtime.link.is_none());
        assert_host_spawns(&deep, 1);
        // The starter serves once the host is spawned, not before.
        tilo_test_env::wait_until("the owner to serve beside the deep profile", || {
            socket.exists()
        });
        let client = ChromiumEngine::with_host(&host, &deep).unwrap();
        assert!(
            client.runtime.link.is_some(),
            "the second engine joins the owner"
        );
        client.send(WebViewCommand::Navigate(
            "https://deep-profile.test/".into(),
        ));
        wait_for_command(&deep, "the client's navigation", |command| {
            command["url"] == "https://deep-profile.test/"
        });
        assert_eq!(host_spawns(&deep), 1, "no second host for one profile");
    }
    // phase: a_socket_name_too_long_to_bind_is_an_error
    {
        let long = dir.path().join("y".repeat(120));
        let error = ChromiumEngine::with_host(&host, &long).err().unwrap();
        assert!(error.contains("too long"), "{error}");
        assert_eq!(host_spawns(&long), 0);
    }
}

#[test]
#[cfg(unix)]
fn a_second_process_browses_through_the_first_processs_host() {
    let (dir, host) = shared_fixture(&[]);
    let profile = dir.path().join("profile");
    std::fs::create_dir_all(&profile).unwrap();
    let spawn_owner = || {
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "webview::chromium::tests::shared_owner_child",
                "--ignored",
            ])
            .env("TILO_TEST_SHARED_HOST", &host)
            .env("TILO_TEST_SHARED_PROFILE", &profile)
            .spawn()
            .unwrap()
    };
    // phase: the_owner_process_outlives_its_clients
    {
        let mut owner = spawn_owner();
        tilo_test_env::wait_until("the owner process", || profile.join("owner-ready").exists());
        let client = ChromiumEngine::with_host(&host, &profile).unwrap();
        assert!(client.runtime.link.is_some());
        assert_host_spawns(&profile, 1);
        std::fs::write(profile.join("release"), b"").unwrap();
        client.send(WebViewCommand::Navigate(
            "https://still-served.test/".into(),
        ));
        wait_for_command(&profile, "the client's navigation", |command| {
            command["url"] == "https://still-served.test/" && command["tab"] == 2
        });
        assert!(
            owner.try_wait().unwrap().is_none(),
            "the owner waits for its clients"
        );
        assert!(!profile.join("served").exists());
        drop(client);
        let status =
            tilo_test_env::wait_for("the owner process to exit", || owner.try_wait().unwrap());
        assert!(status.success());
        assert!(profile.join("served").exists());
        assert!(
            profile.join("exited").exists(),
            "the host stopped with its owner"
        );
    }
    // phase: a_dead_owner_fails_its_clients_and_the_next_engine_takes_over
    {
        for file in ["owner-ready", "release", "served", "exited"] {
            let _ = std::fs::remove_file(profile.join(file));
        }
        let mut owner = spawn_owner();
        tilo_test_env::wait_until("the owner process", || profile.join("owner-ready").exists());
        let client = ChromiumEngine::with_host(&host, &profile).unwrap();
        assert!(client.runtime.link.is_some());
        owner.kill().unwrap();
        owner.wait().unwrap();
        tilo_test_env::wait_until("the client to learn its owner died", || {
            disconnected(&client.drain_events())
        });
        tilo_test_env::wait_until("the dead owner's host to exit", || {
            profile.join("exited").exists()
        });
        let next = ChromiumEngine::with_host(&host, &profile).unwrap();
        assert!(next.runtime.link.is_none());
        assert_host_spawns(&profile, 3);
    }
}

#[test]
#[ignore = "child entry point for the cross-process shared engine test"]
#[cfg(unix)]
fn shared_owner_child() {
    let host = PathBuf::from(std::env::var_os("TILO_TEST_SHARED_HOST").unwrap());
    let profile = PathBuf::from(std::env::var_os("TILO_TEST_SHARED_PROFILE").unwrap());
    let owner = ChromiumEngine::with_host(&host, &profile).unwrap();
    assert!(owner.runtime.link.is_none());
    std::fs::write(profile.join("owner-ready"), b"").unwrap();
    tilo_test_env::wait_until("the parent's release", || profile.join("release").exists());
    drop(owner);
    serve_shared_until_idle();
    std::fs::write(profile.join("served"), b"").unwrap();
}

#[test]
fn the_engine_host_does_not_inherit_the_app_loader_environment() {
    let command = host_command(Path::new(
        "/run/tilo-runtime/x/lib/chromium/tilo-chromium-host",
    ))
    .unwrap();
    for name in ["LD_LIBRARY_PATH", "LD_PRELOAD"] {
        assert!(
            command
                .get_envs()
                .any(|(key, value)| key == name && value.is_none()),
            "the host must start without the app's {name}"
        );
    }
}

#[test]
fn owned_navigation_protocol_preserves_identity_and_bounds() {
    for (command, fields) in [
        (
            NavigationCommand::Navigate {
                url: "about:blank".into(),
            },
            json!({"command":"navigate","url":"about:blank"}),
        ),
        (NavigationCommand::Back, json!({"command":"back"})),
        (NavigationCommand::Forward, json!({"command":"forward"})),
        (NavigationCommand::Reload, json!({"command":"reload"})),
        (
            NavigationCommand::HistoryEntry {
                history_request_id: 8,
                id: 2,
            },
            json!({"command":"history_entry","history_request_id":8,"id":2}),
        ),
    ] {
        let command = WebViewCommand::ScopedNavigation {
            request_id: 7,
            command,
        };
        assert!(command_within_limits(&command));
        let mut expected = fields;
        expected["op"] = json!("scoped_navigation");
        expected["request_id"] = json!(7);
        assert_eq!(encode_command(command), Some(expected));
    }
    assert_eq!(
        encode_command(WebViewCommand::CancelScopedNavigation { request_id: 7 }),
        Some(json!({"op":"cancel_navigation","request_id":7}))
    );
    let valid = json!({"event":"navigation_outcome","request_id":7,"url":"https://redirect.test/"});
    assert_eq!(
        decode_event(&valid),
        Some(WebViewEvent::NavigationOutcome {
            request_id: 7,
            uri: "https://redirect.test/".into(),
            error: None
        })
    );
    for (key, bad) in [
        ("request_id", json!(0)),
        ("request_id", json!(u32::MAX)),
        ("request_id", json!(-1)),
        ("request_id", json!("7")),
        ("url", json!(false)),
        ("url", json!("x".repeat(MAX_PAGE_URL + 1))),
        ("error", json!(null)),
        ("error", json!("")),
        ("error", json!("x".repeat(4097))),
    ] {
        let mut invalid = valid.clone();
        invalid[key] = bad;
        assert!(validate_event(&invalid).is_err(), "{invalid}");
        assert!(decode_event(&invalid).is_none(), "{invalid}");
    }
    for request_id in [0, u32::MAX] {
        assert!(!command_within_limits(&WebViewCommand::ScopedNavigation {
            request_id,
            command: NavigationCommand::Back
        }));
        assert!(!command_within_limits(
            &WebViewCommand::CancelScopedNavigation { request_id }
        ));
    }
    for command in [
        NavigationCommand::Navigate {
            url: "x".repeat(8193),
        },
        NavigationCommand::Navigate { url: String::new() },
        NavigationCommand::HistoryEntry {
            history_request_id: 0,
            id: 2,
        },
    ] {
        assert!(!command_within_limits(&WebViewCommand::ScopedNavigation {
            request_id: 1,
            command
        }));
    }
}

#[test]
fn owned_navigation_first_terminal_survives_unknown_duplicate_and_state_floods() {
    let shared = Arc::new(Shared::default());
    let routes = Arc::new(Mutex::new(HashMap::from([(1, Arc::downgrade(&shared))])));
    let downloads = Mutex::new(VecDeque::new());
    let (ready, _receiver) = sync_channel(1);
    for id in 1..=32 {
        shared.admit_navigation(id, "requested".into()).unwrap();
    }
    // Real decoder/router: failure is first and a late success cannot overwrite it.
    route_native_event(&json!({"event":"navigation_outcome","request_id":1,"url":"failed","error":"fixture failure"}), &routes, &downloads, &ready).unwrap();
    for _ in 0..2000 {
        route_native_event(
            &json!({"event":"navigation_outcome","request_id":1,"url":"stale-success"}),
            &routes,
            &downloads,
            &ready,
        )
        .unwrap();
        route_native_event(
            &json!({"event":"navigation_outcome","request_id":999,"url":"unowned"}),
            &routes,
            &downloads,
            &ready,
        )
        .unwrap();
    }
    for id in 0..2000 {
        shared.event(WebViewEvent::MediaElementUnavailable { request_id: id });
    }
    shared.event(WebViewEvent::LoadFinishedAt {
        uri: "about:blank".into(),
    });
    for id in 2..=32 {
        route_native_event(
            &json!({"event":"navigation_outcome","request_id":id,"url":format!("page-{id}")}),
            &routes,
            &downloads,
            &ready,
        )
        .unwrap();
    }
    let events = shared.events.lock().unwrap();
    assert_eq!(events.len(), 512);
    let outcomes: Vec<_> = events
        .iter()
        .filter(|event| matches!(event, WebViewEvent::NavigationOutcome { .. }))
        .collect();
    assert_eq!(outcomes.len(), 32);
    assert!(
        matches!(outcomes[0], WebViewEvent::NavigationOutcome { request_id: 1, error: Some(error), uri } if error == "fixture failure" && uri == "failed")
    );
    assert!(events.iter().any(
        |event| matches!(event, WebViewEvent::LoadFinishedAt { uri } if uri == "about:blank")
    ));
    drop(events);
    assert!(shared.admit_navigation(33, "blocked".into()).is_err());
    shared.events.lock().unwrap().clear();
    shared.admit_navigation(33, "fresh".into()).unwrap();
    shared.cancel_navigation(33);
    route_native_event(
        &json!({"event":"navigation_outcome","request_id":33,"url":"cancelled"}),
        &routes,
        &downloads,
        &ready,
    )
    .unwrap();
    assert!(shared.events.lock().unwrap().is_empty());
    assert!(shared.admit_navigation(33, "reused".into()).is_err());
}

#[test]
#[cfg(unix)]
fn owned_navigation_real_transport_ignores_bootstrap_and_keeps_redirect_owner() {
    let mut packets = Vec::new();
    for event in [
        json!({"event":"uri","url":"about:blank"}),
        json!({"event":"loaded","url":"about:blank","status":200}),
        json!({"event":"uri","url":"https://redirect.test/final"}),
        json!({"event":"navigation_outcome","request_id":1,"url":"https://redirect.test/final"}),
        json!({"event":"navigation_outcome","request_id":1,"url":"about:blank","error":"stale duplicate"}),
    ] {
        protocol::write_json(&mut packets, &event).unwrap();
    }
    let (dir, host) = shared_fixture(&packets);
    let profile = dir.path().join("profile");
    let engine = ChromiumEngine::with_host(&host, &profile).unwrap();
    engine
        .navigate_owned(
            1,
            NavigationCommand::Navigate {
                url: "https://requested.test/start".into(),
            },
        )
        .unwrap();
    assert!(
        !engine
            .drain_events()
            .iter()
            .any(|event| matches!(event, WebViewEvent::NavigationOutcome { .. }))
    );
    // The helper's input recorder and event emitter run independently.
    // Observe the real command before permitting its canned terminal events.
    wait_for_command(
        &profile,
        "owned navigation recorded on helper wire",
        |command| command["op"] == "scoped_navigation" && command["request_id"] == 1,
    );
    std::fs::write(profile.join("emit"), "").unwrap();
    let mut events = Vec::new();
    tilo_test_env::wait_until("owned navigation outcome", || {
        events.extend(engine.drain_events());
        events
            .iter()
            .any(|event| matches!(event, WebViewEvent::NavigationOutcome { request_id: 1, .. }))
    });
    let outcomes: Vec<_> = events
        .iter()
        .filter(|event| matches!(event, WebViewEvent::NavigationOutcome { .. }))
        .collect();
    assert_eq!(outcomes.len(), 1);
    assert!(
        matches!(outcomes[0], WebViewEvent::NavigationOutcome { request_id: 1, uri, error: None } if uri == "https://redirect.test/final")
    );
    assert!(events.iter().any(
        |event| matches!(event, WebViewEvent::LoadFinishedAt { uri } if uri == "about:blank")
    ));
    // Event output and the command reader run independently in the host
    // fixture. Observe a complete command before opening its recording.
    wait_for_command(&profile, "owned navigation command", |command| {
        command["op"] == "scoped_navigation"
    });
    let mut commands = std::fs::File::open(profile.join("commands")).unwrap();
    let mut navigation = None;
    while let Some(protocol::Packet::Json(value)) = protocol::read_packet(&mut commands).unwrap() {
        if value["op"] == "scoped_navigation" {
            navigation = Some(value);
            break;
        }
    }
    assert_eq!(
        navigation,
        Some(
            json!({"op":"scoped_navigation","tab":1,"command":"navigate","request_id":1,"url":"https://requested.test/start"})
        )
    );
}

#[test]
#[cfg(unix)]
fn owned_navigation_admission_queue_loss_and_renderer_death_are_observable() {
    let (dir, host) = helper_fixture("exec 3>&1; exec cat >/dev/null");
    let engine = ChromiumEngine::with_host(&host, &dir.path().join("profile")).unwrap();
    assert!(engine.navigate_owned(0, NavigationCommand::Back).is_err());
    assert!(
        engine
            .navigate_owned(u32::MAX, NavigationCommand::Back)
            .is_err()
    );
    assert!(
        engine
            .navigate_owned(
                1,
                NavigationCommand::Navigate {
                    url: "x".repeat(8193)
                }
            )
            .is_err()
    );
    assert!(engine.shared.navigations.lock().unwrap().pending.is_empty());
    let (ready, _receiver) = sync_channel(1);
    engine.navigate_owned(7, NavigationCommand::Reload).unwrap();
    route_native_event(&json!({"event":"navigation_outcome","request_id":7,"url":"crashed","error":"renderer crashed"}), &engine.runtime.routes, &engine.runtime.download_events, &ready).unwrap();
    assert!(engine.drain_events().iter().any(|event| matches!(event, WebViewEvent::NavigationOutcome { request_id: 7, error: Some(error), .. } if error == "renderer crashed")));
    engine.navigate_owned(8, NavigationCommand::Reload).unwrap();
    route_native_event(
        &json!({"event":"renderer_terminated"}),
        &engine.runtime.routes,
        &engine.runtime.download_events,
        &ready,
    )
    .unwrap();
    let events = engine.drain_events();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, WebViewEvent::RendererTerminated))
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, WebViewEvent::NavigationOutcome { request_id: 8, .. }))
    );
    assert!(
        engine
            .shared
            .navigations
            .lock()
            .unwrap()
            .pending
            .contains_key(&8)
    );
    route_native_event(
        &json!({"event":"navigation_outcome","request_id":8,"url":"recovered"}),
        &engine.runtime.routes,
        &engine.runtime.download_events,
        &ready,
    )
    .unwrap();
    assert!(engine.drain_events().iter().any(|event| matches!(event, WebViewEvent::NavigationOutcome { request_id: 8, error: None, uri } if uri == "recovered")));
    engine.navigate_owned(9, NavigationCommand::Reload).unwrap();
    engine.shared.fail("fixture helper disconnected".into());
    assert!(engine.drain_events().iter().any(|event| matches!(event, WebViewEvent::NavigationOutcome { request_id: 9, error: Some(error), .. } if error == "fixture helper disconnected")));
    assert!(engine.navigate_owned(10, NavigationCommand::Back).is_err());
    // Closing the queue also drops the helper's stdin; keep the helper alive
    // past EOF so the reader's disconnect cannot win the race and turn the
    // refusal into "Chromium is unavailable".
    let (dir, host) = helper_fixture("exec 3>&1; cat >/dev/null; exec sleep 30");
    let engine = ChromiumEngine::with_host(&host, &dir.path().join("profile")).unwrap();
    engine.tx.close();
    assert!(
        engine
            .navigate_owned(1, NavigationCommand::Reload)
            .unwrap_err()
            .contains("not queued")
    );
    assert!(engine.shared.navigations.lock().unwrap().pending.is_empty());
    let unavailable = UnavailableEngine::new("runtime unavailable".into());
    assert_eq!(
        unavailable.navigate_owned(1, NavigationCommand::Back),
        Err("runtime unavailable".into())
    );
}

#[test]
#[cfg(unix)]
fn owned_navigation_concurrent_cancel_cannot_overtake_admitted_command() {
    let (dir, host) = shared_fixture(&[]);
    let profile = dir.path().join("profile");
    let engine = ChromiumEngine::with_host(&host, &profile).unwrap();
    // Stop the production enqueue at its mutex, after owner registration.
    let queue_barrier = engine.tx.state.lock().unwrap();
    let navigating = engine.clone();
    let navigation =
        std::thread::spawn(move || navigating.navigate_owned(1, NavigationCommand::Reload));
    tilo_test_env::wait_until("navigation reserved before blocked enqueue", || {
        engine
            .shared
            .navigations
            .lock()
            .unwrap()
            .pending
            .contains_key(&1)
    });
    assert!(engine.shared.navigation_commands.try_lock().is_err());
    let cancelling = engine.clone();
    let (ready, entered) = sync_channel(1);
    let cancel = std::thread::spawn(move || {
        ready.send(()).unwrap();
        cancelling.send(WebViewCommand::CancelScopedNavigation { request_id: 1 });
    });
    entered.recv().unwrap();
    // The held queue barrier makes both scheduling orders deterministic: the
    // cancel either blocks now or begins after the admitted command was queued.
    drop(queue_barrier);
    navigation.join().unwrap().unwrap();
    cancel.join().unwrap();
    assert!(engine.shared.navigations.lock().unwrap().pending.is_empty());
    let mut observed = Vec::new();
    tilo_test_env::wait_until("owned navigation then cancellation on helper wire", || {
        let Ok(bytes) = std::fs::read(profile.join("commands")) else {
            return false;
        };
        let mut reader = std::io::Cursor::new(bytes);
        let mut commands = Vec::new();
        while let Ok(Some(protocol::Packet::Json(value))) = protocol::read_packet(&mut reader) {
            if matches!(
                value["op"].as_str(),
                Some("scoped_navigation" | "cancel_navigation")
            ) {
                commands.push(value);
            }
        }
        if commands.len() == 2 {
            observed = commands;
            true
        } else {
            false
        }
    });
    assert_eq!(
        observed,
        vec![
            json!({"op":"scoped_navigation","tab":1,"request_id":1,"command":"reload"}),
            json!({"op":"cancel_navigation","tab":1,"request_id":1}),
        ]
    );
}

#[test]
#[cfg(unix)]
fn owned_navigation_cancel_refusal_wakes_without_holding_command_order() {
    // Closing the command queue hangs up helper stdin. Keep stdout owned by
    // the supervised helper until cancellation fails it; otherwise an unrelated
    // EOF can win the first-error latch before the refusal under test.
    let (_fixture, host) = helper_fixture("exec 3>&1; cat >/dev/null; exec sleep 30");
    // This route joins the supervisor on drop, so a fast test process cannot
    // exit before its deliberately live helper has been killed and reaped.
    let root = private_root();
    let engine = ChromiumEngine::with_private_host(&host, &root).unwrap();
    let enabled = Arc::new(AtomicBool::new(false));
    let reentered = Arc::new(AtomicBool::new(false));
    let weak = Arc::downgrade(&engine);
    let ready = enabled.clone();
    let observed = reentered.clone();
    engine.set_waker(Box::new(move || {
        if ready.swap(false, Ordering::AcqRel) {
            weak.upgrade()
                .unwrap()
                .send(WebViewCommand::CancelScopedNavigation { request_id: 2 });
            observed.store(true, Ordering::Release);
        }
    }));
    engine.tx.close();
    enabled.store(true, Ordering::Release);
    let cancelling = engine.clone();
    let (finished, received) = sync_channel(1);
    let worker = std::thread::spawn(move || {
        cancelling.send(WebViewCommand::CancelScopedNavigation { request_id: 1 });
        finished.send(()).unwrap();
    });
    tilo_test_env::recv(
        "cancel refusal must allow its waker to reenter the command path",
        &received,
    );
    worker.join().unwrap();
    assert!(reentered.load(Ordering::Acquire));
    assert!(engine.drain_events().iter().any(|event| matches!(event, WebViewEvent::LoadFailed { error, .. } if error.contains("cancellation was not queued"))));
    drop(engine);
    assert!(
        !root.exists(),
        "cancellation failure must reap its owned helper"
    );
}

#[test]
fn private_text_input_receipts_route_only_to_the_original_tab_without_public_events() {
    let first = Arc::new(Shared::default());
    let second = Arc::new(Shared::default());
    let (one, a) = first
        .text_input
        .lock()
        .unwrap()
        .admit(super::super::text_input::Request::Prepare)
        .unwrap();
    let (two, b) = second
        .text_input
        .lock()
        .unwrap()
        .admit(super::super::text_input::Request::Prepare)
        .unwrap();
    assert_eq!(
        a["request_id"], b["request_id"],
        "tab owners have independent request spaces"
    );
    let routes = Arc::new(Mutex::new(HashMap::from([
        (1, Arc::downgrade(&first)),
        (2, Arc::downgrade(&second)),
    ])));
    let downloads = Mutex::default();
    let (ready, _) = sync_channel(1);
    let reply = json!({"event":"text_input_result","tab":2,"request_id":b["request_id"],"ok":true,"navigated":false,"url":"http://fixture.invalid/","message":"Prepared","data":{"lease":"0123456789abcdef0123456789abcdef"}});
    route_native_event(&reply, &routes, &downloads, &ready).unwrap();
    assert!(
        one.poll().is_none(),
        "another tab cannot acknowledge the original request"
    );
    assert!(matches!(two.poll(), Some(Ok(Some(_)))));
    assert!(first.events.lock().unwrap().is_empty());
    assert!(second.events.lock().unwrap().is_empty());
    let mut stale = reply.clone();
    stale["ok"] = json!(false);
    stale["code"] = json!("text_input_changed");
    route_native_event(&stale, &routes, &downloads, &ready).unwrap();
    assert!(
        matches!(two.poll(), Some(Ok(Some(_)))),
        "replayed failures cannot alter a consumed receipt"
    );
    first.text_input.lock().unwrap().unavailable();
    assert!(matches!(
        one.poll(),
        Some(Err(super::super::text_input::Failure::Unknown))
    ));
}

#[test]
fn native_history_capability_is_optional_strict_and_per_tab() {
    let first = Arc::new(Shared::default());
    let second = Arc::new(Shared::default());
    let routes = Arc::new(Mutex::new(HashMap::from([
        (1, Arc::downgrade(&first)),
        (2, Arc::downgrade(&second)),
    ])));
    let downloads = Mutex::default();
    let (ready, _receiver) = sync_channel(1);
    assert!(!first.native_edit_history.load(Ordering::Acquire));
    assert!(!first.native_caret_text_input.load(Ordering::Acquire));
    route_native_event(&json!({"event":"ready","cef_version":"152","chromium_version":"152","native_edit_history":true,"guarded_edit_history":true,"native_caret_text_input":true}), &routes, &downloads, &ready).unwrap();
    assert!(first.native_edit_history.load(Ordering::Acquire));
    assert!(first.guarded_edit_history.load(Ordering::Acquire));
    assert!(first.native_caret_text_input.load(Ordering::Acquire));
    route_native_event(
        &json!({"event":"tab_ready","tab":2}),
        &routes,
        &downloads,
        &ready,
    )
    .unwrap();
    assert!(!second.native_edit_history.load(Ordering::Acquire));
    assert!(!second.guarded_edit_history.load(Ordering::Acquire));
    assert!(!second.native_caret_text_input.load(Ordering::Acquire));
    assert!(first.native_edit_history.load(Ordering::Acquire));
    route_native_event(
        &json!({"event":"tab_ready","tab":2,"native_edit_history":true,"guarded_edit_history":true,"native_caret_text_input":true}),
        &routes,
        &downloads,
        &ready,
    )
    .unwrap();
    assert!(second.native_edit_history.load(Ordering::Acquire));
    assert!(second.guarded_edit_history.load(Ordering::Acquire));
    assert!(second.native_caret_text_input.load(Ordering::Acquire));
    assert!(
        route_native_event(
            &json!({"event":"tab_ready","tab":2,"native_edit_history":true,"guarded_edit_history":"true"}),
            &routes,
            &downloads,
            &ready
        )
        .is_err()
    );
    assert!(
        route_native_event(
            &json!({"event":"tab_ready","tab":2,"native_caret_text_input":"true"}),
            &routes,
            &downloads,
            &ready,
        )
        .is_err()
    );
    route_native_event(
        &json!({"event":"ready","cef_version":"152","chromium_version":"152"}),
        &routes,
        &downloads,
        &ready,
    )
    .unwrap();
    assert!(!first.native_edit_history.load(Ordering::Acquire));
    assert!(!first.native_caret_text_input.load(Ordering::Acquire));
    assert!(!first.guarded_edit_history.load(Ordering::Acquire));
    assert!(second.native_edit_history.load(Ordering::Acquire));
}

#[test]
fn native_caret_prepare_opt_in_uses_the_original_tab_capability() {
    let tx = Arc::new(CommandQueue::default());
    let first = Arc::new(Shared::default());
    let second = Arc::new(Shared::default());
    let routes = Arc::new(Mutex::new(HashMap::from([
        (1, Arc::downgrade(&first)),
        (2, Arc::downgrade(&second)),
    ])));
    let runtime = Arc::new(Runtime {
        tx: tx.clone(),
        process: first.clone(),
        supervisor: Mutex::default(),
        routes,
        next_tab: AtomicU32::new(3),
        download_events: Arc::default(),
        server: Arc::default(),
        link: None,
        service_session: None,
        exited: Arc::new(AtomicBool::new(false)),
    });
    let one = ChromiumEngine {
        tx: tx.clone(),
        shared: first,
        runtime: runtime.clone(),
        tab: 1,
    };
    let two = ChromiumEngine {
        tx: tx.clone(),
        shared: second,
        runtime: runtime.clone(),
        tab: 2,
    };
    let original = one
        .text_input(super::super::text_input::Request::Prepare)
        .unwrap();
    let legacy = tx.pop().unwrap();
    assert!(legacy.get("native_caret").is_none());
    let (ready, _receiver) = sync_channel(1);
    route_native_event(
        &json!({"event":"ready","cef_version":"152","chromium_version":"152","native_caret_text_input":true}),
        &runtime.routes, &runtime.download_events, &ready,
    ).unwrap();
    one.text_input(super::super::text_input::Request::Prepare)
        .unwrap();
    let native = tx.pop().unwrap();
    assert_eq!(native["tab"], 1);
    assert_eq!(native["native_caret"], true);
    two.text_input(super::super::text_input::Request::Prepare)
        .unwrap();
    let other = tx.pop().unwrap();
    assert_eq!(other["tab"], 2);
    assert!(other.get("native_caret").is_none());
    route_native_event(
        &json!({"event":"text_input_result","tab":1,"request_id":legacy["request_id"],"ok":true,"navigated":false,"url":"http://fixture.invalid/","message":"Prepared","data":{"lease":"0123456789abcdef0123456789abcdef"}}),
        &runtime.routes, &runtime.download_events, &ready,
    ).unwrap();
    let lease = original.poll().unwrap().unwrap().unwrap();
    one.text_input(super::super::text_input::Request::Check(lease))
        .unwrap();
    let check = tx.pop().unwrap();
    assert_eq!(check["action"], "check_text_input");
    assert!(check.get("native_caret").is_none());
}

#[test]
fn document_size_reports_reach_only_a_host_that_measures() {
    let tx = Arc::new(CommandQueue::default());
    let shared = Arc::new(Shared::default());
    let routes = Arc::new(Mutex::new(HashMap::from([(1, Arc::downgrade(&shared))])));
    let runtime = Arc::new(Runtime {
        tx: tx.clone(),
        process: shared.clone(),
        supervisor: Mutex::default(),
        routes,
        next_tab: AtomicU32::new(2),
        download_events: Arc::default(),
        server: Arc::default(),
        link: None,
        service_session: None,
        exited: Arc::new(AtomicBool::new(false)),
    });
    let engine = ChromiumEngine {
        tx: tx.clone(),
        shared,
        runtime: runtime.clone(),
        tab: 1,
    };
    engine.send(WebViewCommand::ReportDocumentSize(true));
    assert!(engine.drain_events().is_empty());
    let queued = || tx.state.lock().unwrap().0.len();
    assert_eq!(queued(), 0, "an older host never sees the command");

    let (ready, _receiver) = sync_channel(1);
    route_native_event(
        &json!({"event":"ready","cef_version":"152","chromium_version":"152","document_size":true}),
        &runtime.routes,
        &runtime.download_events,
        &ready,
    )
    .unwrap();
    engine.drain_events();
    let asked = tx.pop().expect("told once the host can measure");
    assert_eq!(asked["op"], "document_size");
    assert_eq!(asked["tab"], 1);
    assert_eq!(asked["enabled"], true);
    engine.drain_events();
    assert_eq!(queued(), 0, "told once");

    route_native_event(
        &json!({"event":"document_size","tab":1,"width":640.0,"height":1830.5,"scroll_y":0.0}),
        &runtime.routes,
        &runtime.download_events,
        &ready,
    )
    .unwrap();
    assert_eq!(
        engine.drain_events(),
        [WebViewEvent::DocumentSize {
            width: 640.0,
            height: 1830.5,
            scroll_y: 0.0
        }]
    );
    assert!(
        route_native_event(
            &json!({"event":"document_size","tab":1,"width":640.0,"height":-1.0,"scroll_y":0.0}),
            &runtime.routes,
            &runtime.download_events,
            &ready,
        )
        .is_err(),
        "a size no document has is refused"
    );

    engine.send(WebViewCommand::ReportDocumentSize(false));
    let stopped = tx.pop().expect("stopping is told at once");
    assert_eq!(stopped["enabled"], false);
}

pub(super) fn selected_service_request() -> super::super::ServiceSessionRequest {
    use browser_continuity_core::sessions::{Cookie, SameSite, Service, SessionCookies};
    super::super::ServiceSessionRequest {
        request_id: 7,
        verify_only: false,
        session: SessionCookies {
            schema: 1,
            session_id: "selected-slack-session".into(),
            service: Service::Slack,
            identity_public_key: [9; 32],
            setup_binding: "setup-operation".into(),
            source_snapshot_digest: [4; 32],
            origin_storage: Vec::new(),
            cookies: vec![Cookie {
                host: "app.slack.com".into(),
                domain: "app.slack.com".into(),
                host_only: true,
                name: "__Host-session".into(),
                value: "private-cookie-marker".into(),
                path: "/".into(),
                expires_unix_seconds: None,
                secure: true,
                http_only: true,
                same_site: SameSite::Lax,
            }],
        },
    }
}

#[test]
fn selected_service_wire_preserves_cookie_semantics_redacts_and_refuses_expired_credentials() {
    let request = selected_service_request();
    let command = WebViewCommand::ImportServiceSession(request.clone());
    assert!(!format!("{command:?}").contains("private-cookie-marker"));
    assert!(command_within_limits(&command));
    let mut wire = encode_command(command).unwrap();
    assert_eq!(wire["cookies"][0]["host"], "app.slack.com");
    assert_eq!(wire["cookies"][0]["domain"], "app.slack.com");
    assert_eq!(wire["cookies"][0]["host_only"], true);
    assert_eq!(wire["cookies"][0]["expires_unix_seconds"], Value::Null);
    assert_eq!(wire["cookies"][0]["http_only"], true);
    assert_eq!(wire["cookies"][0]["same_site"], "lax");
    assert!(wire.get("identity_public_key").is_none());
    assert!(wire.get("source_snapshot_digest").is_none());
    wipe_service_wire(&mut wire);
    assert_eq!(wire["cookies"][0]["value"], "");
    let mut expired = request.clone();
    expired.session.cookies[0].expires_unix_seconds = Some(1);
    assert!(!command_within_limits(
        &WebViewCommand::ImportServiceSession(expired)
    ));
    let mut foreign = request.clone();
    foreign.session.cookies[0].host = "foreign.example".into();
    assert!(encode_command(WebViewCommand::ImportServiceSession(foreign)).is_none());
    let mut weakened = request;
    weakened.session.cookies[0].secure = false;
    assert!(encode_command(WebViewCommand::ImportServiceSession(weakened)).is_none());
}

#[test]
fn selected_service_completion_is_correlated_storage_evidence_and_test_engine_refuses_import() {
    let event = json!({"event":"service_session_result", "request_id":7,
        "session_id":"selected-slack-session", "service":"slack", "imported":true, "cookie_count":1});
    assert!(validate_event(&event).is_ok());
    let Some(WebViewEvent::ServiceSessionResult(reply)) = decode_event(&event) else {
        panic!("missing storage result");
    };
    assert_eq!(reply.request_id, 7);
    assert_eq!(reply.cookie_count, 1);
    assert!(reply.imported);
    for (key, value) in [
        ("request_id", json!(0)),
        ("cookie_count", json!(1025)),
        ("session_id", json!("../escape")),
        ("service", json!("unknown")),
    ] {
        let mut malformed = event.clone();
        malformed[key] = value;
        assert!(validate_event(&malformed).is_err());
    }
    for field in [
        "cookies",
        "value",
        "password",
        "token",
        "identity_public_key",
        "source_snapshot_digest",
    ] {
        let mut leaked = event.clone();
        leaked[field] = json!("private-credential-must-not-be-a-receipt");
        assert!(
            validate_event(&leaked).is_err(),
            "private field admitted to public result"
        );
        assert!(decode_event(&leaked).is_none());
    }
    let engine = super::super::test_engine::TestEngine::new();
    engine.send(WebViewCommand::ImportServiceSession(
        selected_service_request(),
    ));
    let events = engine.drain_events();
    assert!(events.iter().any(
        |event| matches!(event, WebViewEvent::ServiceSessionResult(reply)
        if reply.request_id == 7 && !reply.imported && reply.cookie_count == 0)
    ));
}

#[test]
fn selected_discord_origin_wire_preserves_raw_value_refuses_other_origins_and_redacts_receipts() {
    use browser_continuity_core::sessions::{OriginStorage, Service};
    let mut request = selected_service_request();
    request.session.schema = 2;
    request.session.service = Service::Discord;
    request.session.cookies.clear();
    request.session.origin_storage = vec![OriginStorage {
        origin: "https://discord.com".into(),
        key: "token".into(),
        value: "\"synthetic-raw-dom-value\"".into(),
        source_snapshot_digest: [6; 32],
    }];
    assert!(command_within_limits(
        &WebViewCommand::ImportServiceSession(request.clone())
    ));
    assert!(!format!("{request:?}").contains("synthetic-raw-dom-value"));
    request.verify_only = true;
    let mut wire = encode_command(WebViewCommand::ImportServiceSession(request.clone())).unwrap();
    assert_eq!(wire["verify_only"], true);
    assert_eq!(
        wire["origin_storage"][0]["value"],
        "\"synthetic-raw-dom-value\""
    );
    assert!(
        wire["origin_storage"][0]
            .get("source_snapshot_digest")
            .is_none()
    );
    wipe_service_wire(&mut wire);
    assert_eq!(wire["origin_storage"][0]["value"], "");
    request.session.origin_storage[0].origin = "https://discord.com.evil.example".into();
    assert!(encode_command(WebViewCommand::ImportServiceSession(request)).is_none());
    let event = json!({"event":"service_session_result", "request_id":2,
        "session_id":"selected-discord-session", "service":"discord", "imported":true,
        "cookie_count":0,"origin_storage_count":1});
    assert!(
        matches!(decode_event(&event),Some(WebViewEvent::ServiceSessionResult(reply))
        if reply.origin_storage_count == 1 && reply.cookie_count == 0)
    );
    let mut wrong_service = event.clone();
    wrong_service["service"] = json!("slack");
    assert!(decode_event(&wrong_service).is_none());
    let mut leak = event;
    leak["origin_storage"] = json!([{"value":"synthetic-raw-dom-value"}]);
    assert!(validate_event(&leak).is_err());
    assert!(decode_event(&leak).is_none());
}

#[test]
fn devtools_extension_calls_round_trip_bounded_and_validated() {
    // phase: command_encodes_with_its_session_and_is_bounded
    let load = WebViewCommand::DevTools {
        request_id: 7,
        method: "Extensions.loadUnpacked".into(),
        params: serde_json::json!({"path":"/profile/extensions/a"}),
        session: None,
    };
    assert!(command_within_limits(&load));
    let encoded = encode_command(load).unwrap();
    assert_eq!(encoded["op"], "devtools_call");
    assert_eq!(encoded["request_id"], 7);
    assert_eq!(encoded["params"]["path"], "/profile/extensions/a");
    assert!(encoded.get("session").is_none());
    let attached = encode_command(WebViewCommand::DevTools {
        request_id: 8,
        method: "Runtime.evaluate".into(),
        params: serde_json::json!({}),
        session: Some("S1".into()),
    })
    .unwrap();
    assert_eq!(attached["session"], "S1");
    for bad in [
        WebViewCommand::DevTools {
            request_id: 0,
            method: "Extensions.uninstall".into(),
            params: serde_json::json!({}),
            session: None,
        },
        WebViewCommand::DevTools {
            request_id: 1,
            method: "Extensions.uninstall".into(),
            params: serde_json::json!("not an object"),
            session: None,
        },
        WebViewCommand::DevTools {
            request_id: 1,
            method: "x".repeat(129),
            params: serde_json::json!({}),
            session: None,
        },
        WebViewCommand::DevTools {
            request_id: 1,
            method: "Runtime.evaluate".into(),
            params: serde_json::json!({"expression":"x".repeat(70_000)}),
            session: Some("S".into()),
        },
    ] {
        assert!(!command_within_limits(&bad));
    }

    // phase: reply_is_validated_and_decoded
    let reply = serde_json::json!({"event":"devtools_result","tab":1,"request_id":7,"ok":true,"body":"{\"id\":\"abc\"}"});
    assert!(validate_event(&reply).is_ok());
    assert!(matches!(
        decode_event(&reply),
        Some(WebViewEvent::DevToolsResult { request_id: 7, ok: true, body }) if body.contains("abc")
    ));
    for broken in [
        serde_json::json!({"event":"devtools_result","tab":1,"request_id":0,"ok":true,"body":""}),
        serde_json::json!({"event":"devtools_result","tab":1,"request_id":1,"ok":"yes","body":""}),
        serde_json::json!({"event":"devtools_result","tab":1,"request_id":1,"ok":true}),
    ] {
        assert!(validate_event(&broken).is_err());
    }
}

#[test]
fn a_governed_app_routes_page_audio_through_the_host_bridge() {
    // The sandbox withholds the audio sockets and devices, so the engine must
    // not try to open any; its sound reaches the app over the host protocol.
    assert_eq!(host_audio_switches(true), ["--tilo-audio-bridge"]);
    assert!(host_audio_switches(false).is_empty());
}

#[test]
fn the_pointer_takes_the_shape_the_page_asks_for() {
    let shared = Arc::new(Shared::default());
    let routes: Routes = Arc::new(Mutex::new(HashMap::from([(1, Arc::downgrade(&shared))])));
    let downloads = Mutex::new(VecDeque::new());
    let (ready, _receiver) = sync_channel(1);
    let cursor = |name: &str| {
        route_native_event(
            &json!({"event":"cursor","cursor":name}),
            &routes,
            &downloads,
            &ready,
        )
        .unwrap();
        *shared.cursor.lock().unwrap()
    };
    assert_eq!(cursor("pointer"), Some(winit::window::CursorIcon::Pointer));
    assert_eq!(cursor("text"), Some(winit::window::CursorIcon::Text));
    assert_eq!(cursor("default"), None, "the arrow is the toolkit's own");
    assert_eq!(cursor("a-shape-from-later"), None);
    assert!(
        shared.events.lock().unwrap().is_empty(),
        "a shape is not a page event"
    );
    assert!(validate_event(&json!({"event":"cursor","cursor":""})).is_err());
    assert!(validate_event(&json!({"event":"cursor","cursor":"x".repeat(65)})).is_err());
}

#[test]
fn page_questions_reach_only_a_tab_that_answers_them() {
    let tx = Arc::new(CommandQueue::default());
    let shared = Arc::new(Shared::default());
    let routes = Arc::new(Mutex::new(HashMap::from([(1, Arc::downgrade(&shared))])));
    let runtime = Arc::new(Runtime {
        tx: tx.clone(),
        process: shared.clone(),
        supervisor: Mutex::default(),
        routes,
        next_tab: AtomicU32::new(2),
        download_events: Arc::default(),
        server: Arc::default(),
        link: None,
        service_session: None,
        exited: Arc::new(AtomicBool::new(false)),
    });
    let engine = ChromiumEngine {
        tx: tx.clone(),
        shared,
        runtime: runtime.clone(),
        tab: 1,
    };
    engine.accept_page_dialogs();
    assert!(engine.drain_events().is_empty());
    let queued = || tx.state.lock().unwrap().0.len();
    assert_eq!(queued(), 0, "an older host never sees the command");

    let (ready, _receiver) = sync_channel(1);
    let route = |event: Value| {
        route_native_event(&event, &runtime.routes, &runtime.download_events, &ready)
    };
    route(json!({"event":"ready","cef_version":"152","chromium_version":"152","js_dialogs":true}))
        .unwrap();
    engine.drain_events();
    let asked = tx.pop().expect("told once the host sends questions");
    assert_eq!(asked, json!({"op":"js_dialogs","tab":1,"enabled":true}));
    engine.drain_events();
    assert_eq!(queued(), 0, "told once");

    route(
        json!({"event":"js_dialog","tab":1,"id":4,"kind":"prompt","origin":"https://example.org",
        "message":"Your name?","default_text":"Ada"}),
    )
    .unwrap();
    route(
        json!({"event":"js_dialog","tab":1,"id":5,"kind":"beforeunload","reload":true,
        "origin":"https://example.org","message":"","default_text":""}),
    )
    .unwrap();
    route(json!({"event":"js_dialog_closed","tab":1,"id":4})).unwrap();
    assert_eq!(
        engine.drain_events(),
        [
            WebViewEvent::PageDialog(crate::webview::PageDialog {
                id: 4,
                kind: crate::webview::PageDialogKind::Prompt,
                origin: "https://example.org".into(),
                message: "Your name?".into(),
                default_text: "Ada".into(),
            }),
            WebViewEvent::PageDialog(crate::webview::PageDialog {
                id: 5,
                kind: crate::webview::PageDialogKind::Leave { reload: true },
                origin: "https://example.org".into(),
                message: String::new(),
                default_text: String::new(),
            }),
            WebViewEvent::PageDialogClosed { id: 4 },
        ]
    );
    for malformed in [
        json!({"event":"js_dialog","tab":1,"id":6,"kind":"print","origin":"","message":"","default_text":""}),
        json!({"event":"js_dialog","tab":1,"id":0,"kind":"alert","origin":"","message":"","default_text":""}),
        json!({"event":"js_dialog","tab":1,"id":6,"kind":"beforeunload","origin":"","message":"","default_text":""}),
        json!({"event":"js_dialog","tab":1,"id":6,"kind":"alert","origin":"","message":"x".repeat(16 * 1024 + 1),"default_text":""}),
        json!({"event":"js_dialog_closed","tab":1,"id":-1}),
    ] {
        assert!(route(malformed.clone()).is_err(), "{malformed}");
    }

    engine.send(WebViewCommand::AnswerPageDialog {
        id: 4,
        accept: true,
        text: Some("Grace".into()),
    });
    assert_eq!(
        tx.pop().unwrap(),
        json!({"op":"js_dialog_reply","tab":1,"id":4,"accept":true,"text":"Grace"})
    );
    engine.send(WebViewCommand::AnswerPageDialog {
        id: 5,
        accept: false,
        text: None,
    });
    assert_eq!(
        tx.pop().unwrap(),
        json!({"op":"js_dialog_reply","tab":1,"id":5,"accept":false})
    );
    engine.send(WebViewCommand::AnswerPageDialog {
        id: 6,
        accept: true,
        text: Some("x".repeat(crate::webview::MAX_PAGE_DIALOG_ANSWER + 1)),
    });
    assert_eq!(queued(), 0, "an answer past the host's bound is never sent");
}
