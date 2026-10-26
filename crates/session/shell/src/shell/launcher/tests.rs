use std::collections::BTreeMap;

use super::*;

mod host_windows;

fn launch_event(kind: LaunchEventKind) -> LaunchEvent {
    LaunchEvent {
        package_id: "org.example.Broken".into(),
        display_name: "Broken App".into(),
        managed_agent_id: None,
        managed_workspace_id: None,
        kind,
    }
}

fn held(package_id: &str, args: &[&str]) -> HeldLaunch {
    HeldLaunch {
        authority: None,
        #[cfg(target_os = "linux")]
        retained: None,
        package_id: package_id.into(),
        display_name: package_id.into(),
        command: LaunchCommand {
            program: PathBuf::from("/usr/libexec/tilo-launcher"),
            args: args.iter().map(ToString::to_string).collect(),
            env: Vec::new(),
            service_identity: None,
            portal_workspace_id: None,
            managed_agent_id: None,
            x11_workspace: None,
        },
    }
}

/// The signed record is verified once per installed manifest: a relaunch
/// of the same bytes reuses it, changed bytes are verified again, and a
/// failed verification is not remembered.
#[test]
fn creation_evidence_is_verified_once_per_installed_manifest() {
    let evidence = tilo_action_broker::services::ApplicationCreationEvidence {
        package_hash: "ab".repeat(32),
        declared_kinds: vec!["note".into()],
    };
    let key = |manifest: u8| {
        (
            "dev.tilo.creation-cache-probe".to_owned(),
            "ab".repeat(32),
            PathBuf::from("/store/creation-cache-probe/manifest.toml"),
            PathBuf::from("/store/creation-cache-probe/payload"),
            [manifest; 32],
        )
    };
    let verified = std::cell::Cell::new(0_u32);
    let counter = &verified;
    let verify =
        move |result: Option<tilo_action_broker::services::ApplicationCreationEvidence>| {
            move || {
                counter.set(counter.get().saturating_add(1));
                result
            }
        };
    assert_eq!(
        cached_creation_evidence(key(1), verify(Some(evidence.clone()))),
        Some(evidence.clone())
    );
    assert_eq!(
        cached_creation_evidence(key(1), verify(None)),
        Some(evidence.clone()),
        "the same installed bytes are not verified again"
    );
    assert_eq!(verified.get(), 1);
    assert_eq!(cached_creation_evidence(key(2), verify(None)), None);
    assert_eq!(
        cached_creation_evidence(key(2), verify(Some(evidence.clone()))),
        Some(evidence),
        "a refusal is not remembered"
    );
    assert_eq!(verified.get(), 3);
}

#[test]
fn launch_events_reach_the_session_that_asked_and_no_other() {
    let asked = |package_id: &str| LaunchEvent {
        package_id: package_id.into(),
        display_name: package_id.into(),
        managed_agent_id: None,
        managed_workspace_id: None,
        kind: LaunchEventKind::Exited,
    };
    let _ = drain_events();
    spawn_reporting(std::thread::Builder::new(), move || {
        push_event(asked("so.tilo.worker"));
    })
    .unwrap()
    .join()
    .unwrap();
    let other = std::thread::spawn(move || {
        push_event(asked("so.tilo.other-session"));
        drain_events()
    })
    .join()
    .unwrap();
    assert_eq!(
        drain_events(),
        vec![asked("so.tilo.worker")],
        "a worker reports to the session that started it"
    );
    assert_eq!(
        other,
        vec![asked("so.tilo.other-session")],
        "another session drains only its own launches"
    );
}

#[test]
fn a_held_launch_is_held_and_reported_in_its_own_session_only() {
    let command = LaunchCommand {
        program: PathBuf::from("/bin/true"),
        args: Vec::new(),
        env: Vec::new(),
        service_identity: None,
        portal_workspace_id: None,
        managed_agent_id: None,
        x11_workspace: None,
    };
    let _ = drain_events();
    let reported = std::thread::spawn(move || {
        hold_launches();
        spawn_launch("so.tilo.held".into(), "Held".into(), command).unwrap();
        fail_held_launches("peer sync could not start");
        drain_events()
    })
    .join()
    .unwrap();
    assert_eq!(reported.len(), 1);
    assert_eq!(reported[0].package_id, "so.tilo.held");
    assert!(
        matches!(reported[0].kind, LaunchEventKind::SpawnFailed { .. }),
        "{:?}",
        reported[0].kind
    );
    assert!(
        drain_events().is_empty(),
        "another session neither holds nor hears of the launch"
    );
    assert!(
        lock(&LaunchSession::current().gate).open().is_empty(),
        "holding launches in one session holds none in another"
    );
}

fn test_gate() -> Mutex<Gate> {
    Mutex::new(Gate::open_with_capacity(2))
}

fn admit_recording(
    gate: &Mutex<Gate>,
    launch: HeldLaunch,
    started: &mut Vec<String>,
    reported: &mut Vec<LaunchEvent>,
) -> Result<(), String> {
    admit_launch(
        gate,
        launch,
        |launch| {
            started.push(launch.package_id);
            Ok(())
        },
        |event| reported.push(event),
    )
}

#[test]
fn network_start_gates_waiting_launches_in_order() {
    fn launches_requested_while_the_network_starts_wait_then_run_in_order() {
        let gate = test_gate();
        let mut started = Vec::new();
        let mut reported = Vec::new();
        lock(&gate).hold();

        for app in ["so.tilo.files", "so.tilo.notes"] {
            admit_recording(&gate, held(app, &["run", app]), &mut started, &mut reported)
                .expect("held, not refused");
        }
        assert!(started.is_empty(), "nothing runs before the broker exists");
        assert!(reported.is_empty(), "nothing is dropped or failed either");

        release_from(&gate, |launch| started.push(launch.package_id));
        assert_eq!(started, ["so.tilo.files", "so.tilo.notes"]);

        // Once released, the next launch runs at once.
        admit_recording(
            &gate,
            held("so.tilo.music", &["run", "so.tilo.music"]),
            &mut started,
            &mut reported,
        )
        .unwrap();
        assert_eq!(started.last().map(String::as_str), Some("so.tilo.music"));
    }

    fn a_failed_network_start_fails_the_waiting_launches() {
        let gate = test_gate();
        let mut started = Vec::new();
        let mut reported = Vec::new();
        lock(&gate).hold();
        admit_recording(
            &gate,
            held("so.tilo.files", &["run", "so.tilo.files"]),
            &mut started,
            &mut reported,
        )
        .unwrap();

        fail_from(&gate, "peer sync could not start", |event| {
            reported.push(event);
        });
        assert!(started.is_empty());
        assert_eq!(reported.len(), 1);
        assert_eq!(reported[0].package_id, "so.tilo.files");
        assert_eq!(
            reported[0].kind,
            LaunchEventKind::SpawnFailed {
                reason: LaunchFailure::Other,
                detail: "peer sync could not start".into()
            }
        );
        assert!(!lock(&gate).is_holding(), "later launches are not held");
    }

    fn a_repeated_request_waits_once_and_an_overfull_wait_is_reported() {
        let gate = test_gate();
        let mut started = Vec::new();
        let mut reported = Vec::new();
        lock(&gate).hold();
        let files = || held("so.tilo.files", &["run", "so.tilo.files"]);
        admit_recording(&gate, files(), &mut started, &mut reported).unwrap();
        admit_recording(&gate, files(), &mut started, &mut reported).unwrap();
        // Same app, another document: a different request.
        admit_recording(
            &gate,
            held("so.tilo.files", &["run", "so.tilo.files", "--open", "/a"]),
            &mut started,
            &mut reported,
        )
        .unwrap();
        assert!(
            admit_recording(
                &gate,
                held("so.tilo.notes", &["run", "so.tilo.notes"]),
                &mut started,
                &mut reported,
            )
            .is_err()
        );
        assert_eq!(reported.len(), 1);
        assert_eq!(reported[0].package_id, "so.tilo.notes");

        let mut released = Vec::new();
        release_from(&gate, |launch| released.push(launch.command.args));
        assert_eq!(released.len(), 2);
        assert_eq!(released[1].last().map(String::as_str), Some("/a"));
    }

    launches_requested_while_the_network_starts_wait_then_run_in_order();
    a_failed_network_start_fails_the_waiting_launches();
    a_repeated_request_waits_once_and_an_overfull_wait_is_reported();
}

#[test]
#[allow(clippy::too_many_lines, reason = "one scenario read top to bottom")]
fn launch_failure_copy_and_logs_are_bounded_and_never_leak_detail() {
    fn failed_launch_banner_never_contains_raw_stderr() {
        let raw = "error: could not connect\nWAYLAND_DISPLAY=wayland-1\nbacktrace: 0xdeadbeef";
        let event = launch_event(LaunchEventKind::Failed {
            code: Some(1),
            stderr_tail: raw.into(),
        });
        let notice = event.failure_notice().expect("failure has a notice");
        assert_eq!(notice.title, "Couldn't open Broken App");
        assert_eq!(
            notice.body,
            "The app stopped before opening. Open Applications and try again."
        );
        assert!(!notice.body.contains(raw));
        assert!(!notice.body.contains('\n'));
        assert_eq!(LAUNCH_FAILURE_NOTICE_LIFETIME, Duration::from_secs(8));
    }

    fn spawn_failure_notice_names_the_typed_reason_not_a_missing_launcher() {
        let unavailable = crate::shell::i18n::t("launch-failed-launcher-unavailable");
        let cases = [
            (
                LaunchFailure::AuthorityChanged,
                "launch-failed-authority-changed",
            ),
            (
                LaunchFailure::AuthorityTimedOut,
                "launch-failed-authority-timeout",
            ),
            (
                LaunchFailure::SandboxSupervision,
                "launch-error-sandbox-supervision",
            ),
            (LaunchFailure::NotInstalled, "launch-error-not-installed"),
            (LaunchFailure::NoWorkspace, "launch-error-no-workspace"),
            (
                LaunchFailure::PreviousInstanceBusy,
                "launch-failed-previous-instance",
            ),
        ];
        for (reason, key) in cases {
            let notice = launch_event(LaunchEventKind::SpawnFailed {
                reason,
                detail: "raw detail stays in the log".into(),
            })
            .failure_notice()
            .unwrap();
            assert_eq!(notice.body, crate::shell::i18n::t(key));
            assert_ne!(notice.body, unavailable);
            assert!(!notice.body.contains("raw detail"));
        }
        let missing = launch_event(LaunchEventKind::SpawnFailed {
            reason: LaunchError::LauncherBinaryMissing { searched: vec![] }.failure(),
            detail: String::new(),
        })
        .failure_notice()
        .unwrap();
        assert_eq!(missing.body, unavailable);
        let refused = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "x");
        assert_eq!(
            LaunchFailure::of_authority_error(&refused),
            LaunchFailure::AuthorityChanged
        );
        assert_eq!(
            LaunchError::NotInstalled {
                package_id: "p".into()
            }
            .failure(),
            LaunchFailure::NotInstalled
        );
    }

    fn spawn_failure_and_non_failure_events_have_sensible_banner_copy() {
        let event = launch_event(LaunchEventKind::SpawnFailed {
            reason: LaunchFailure::LauncherUnavailable,
            detail: "No such file or directory (os error 2)".into(),
        });
        let notice = event.failure_notice().expect("failure has a notice");
        assert_eq!(notice.title, "Couldn't open Broken App");
        assert_eq!(
            notice.body,
            "The app launcher is unavailable. Open Applications and try again."
        );
        assert!(
            launch_event(LaunchEventKind::Started { pid: 42 })
                .failure_notice()
                .is_none()
        );
        assert!(
            launch_event(LaunchEventKind::Exited)
                .failure_notice()
                .is_none()
        );
    }

    fn launch_failure_title_normalises_and_bounds_untrusted_app_names() {
        let mut event = launch_event(LaunchEventKind::Failed {
            code: None,
            stderr_tail: String::new(),
        });
        event.display_name = "  An app\nwith a deliberately extremely long display name  ".into();
        let notice = event.failure_notice().unwrap();
        assert_eq!(
            notice.title,
            "Couldn't open An app with a deliberately extre…"
        );
        assert!(!notice.title.contains('\n'));
        assert_eq!(compact_notice_name(" \t "), "application");
    }

    fn preflight_notification_copy_excludes_paths_and_backend_detail() {
        let missing = LaunchError::LauncherBinaryMissing {
            searched: vec![
                PathBuf::from("/a/private/developer/build/tilo-launcher"),
                PathBuf::from("/another/very/long/path/tilo-launcher"),
            ],
        };
        let registry = LaunchError::RegistryUnreadable {
            detail: "permission denied while reading /var/lib/tilo/apps.json".into(),
        };
        assert_eq!(
            missing.notification_message(),
            "The app launcher is unavailable. Try again."
        );
        assert_eq!(
            registry.notification_message(),
            "The app list could not be read. Try again."
        );
        assert!(!missing.notification_message().contains('/'));
        assert!(!registry.notification_message().contains('/'));
        assert!(missing.user_message().contains("/a/private"));
        assert!(registry.user_message().contains("/var/lib"));
    }

    fn untrusted_app_log_lines_are_memory_bounded_and_stream_label_safe() {
        let input = vec![b'x'; APP_LOG_LINE_BYTES.saturating_mul(3)];
        let mut lines = Vec::new();
        relay_capped_lines(std::io::Cursor::new(input), |line| lines.push(line));
        assert_eq!(lines.len(), 1);
        assert!(lines[0].ends_with("… [truncated]"));
        assert!(
            lines[0].len() <= APP_LOG_LINE_BYTES.saturating_add(20),
            "capped line unexpectedly retained {} bytes",
            lines[0].len()
        );
    }

    failed_launch_banner_never_contains_raw_stderr();
    spawn_failure_and_non_failure_events_have_sensible_banner_copy();
    spawn_failure_notice_names_the_typed_reason_not_a_missing_launcher();
    launch_failure_title_normalises_and_bounds_untrusted_app_names();
    preflight_notification_copy_excludes_paths_and_backend_detail();
    untrusted_app_log_lines_are_memory_bounded_and_stream_label_safe();
}

#[test]
fn a_recognised_cause_is_named_without_quoting_the_process() {
    let cases = [
        (
            Some(127),
            "/usr/libexec/tilo/apps/terminal: error while loading shared libraries: \
             libvulkan.so.1: cannot open shared object file: No such file or directory",
            "The app needs a library this system doesn't have, so it stopped before opening.",
        ),
        (
            Some(127),
            "/var/tilo-apps/so.tilo.terminal/bin/terminal: error while loading shared \
             libraries: libstd-8f2c.so: cannot open shared object file: No such file or \
             directory",
            "This app was built for a different Tilo runtime. Update or reinstall it to get \
             the runtime it needs.",
        ),
        (
            Some(1),
            "tilo-launcher: `so.tilo.terminal` was built for a different Tilo runtime than \
             the one installed, so it cannot start: it needs the library \
             `libstd-55999f89fc36b51f.so`, which the installed runtime \
             `tilo-runtime/2026.10.0` does not provide (looked in \
             /usr/lib/tilo-runtime/2026.10.0/lib, /etc/ld.so.cache, /lib64). Update it with \
             `app update so.tilo.terminal` or reinstall it with `app install \
             so.tilo.terminal`, which also fetches the runtime it needs, or remove it with \
             `app remove so.tilo.terminal` to go back to the system build.",
            "This app was built for a different Tilo runtime. Update or reinstall it to get \
             the runtime it needs.",
        ),
        (
            Some(1),
            "bwrap: Can't make overlay mount on /newroot/usr with options \
             upperdir=/proc/self/fd/13,userxattr: Invalid argument",
            "The app's sandbox could not be set up, so it stopped before opening.",
        ),
        (
            Some(1),
            "error: service connection failed: joining collaboration route failed: \
             capnp error: Failed: remote exception: collaboration launch is not an \
             authenticated shell-managed process",
            "The app couldn't join this workspace's collaboration service, so it \
             stopped before opening. The desktop log says why.",
        ),
        (
            None,
            "",
            "The app crashed before opening. Open Applications and try again.",
        ),
    ];
    for (code, stderr, body) in cases {
        let notice = launch_event(LaunchEventKind::Failed {
            code,
            stderr_tail: stderr.into(),
        })
        .failure_notice()
        .expect("failure has a notice");
        assert_eq!(notice.body, body);
        assert!(!notice.body.contains('/'), "no path reaches the banner");
    }
}

/// A window that went away without being closed is a lost window: the
/// person is told, whether the process then exited cleanly (an app
/// whose drawing surface died used to) or with an error after its
/// window was up (which is otherwise an ordinary ending).
#[test]
fn memory_endings_name_the_app_or_its_job_and_why() {
    use ::launcher::memory_pressure::Cause;
    let event = |display_name: &str, end: MemoryEnd| LaunchEvent {
        package_id: "so.tilo.example".into(),
        display_name: display_name.into(),
        managed_agent_id: None,
        managed_workspace_id: None,
        kind: LaunchEventKind::OutOfMemory(end),
    };
    let app = |cause| MemoryEnd { cause, job: None };
    let job = |cause| MemoryEnd {
        cause,
        job: Some("rust-analyzer".into()),
    };

    let notice = event("Mail", app(Cause::Kernel)).failure_notice().unwrap();
    assert_eq!(notice.title, "Mail ran out of memory and was closed");
    assert_eq!(notice.body, "Reopen it to continue where you left off.");
    let notice = event("Mail", app(Cause::Runaway)).failure_notice().unwrap();
    assert_eq!(notice.title, "Mail ran out of memory and was closed");
    assert_eq!(
        notice.body,
        "It kept taking more memory until the computer ran low. \
         Reopen it to continue where you left off."
    );
    let notice = event("Code Editor", job(Cause::Runaway))
        .failure_notice()
        .unwrap();
    assert_eq!(
        notice.title,
        "rust-analyzer in Code Editor ran out of memory and was stopped"
    );
    assert_eq!(
        notice.body,
        "It kept taking more memory until the computer ran low. Code Editor is still open."
    );
    let notice = event(
        "Terminal",
        MemoryEnd {
            cause: Cause::Kernel,
            job: Some("bash".into()),
        },
    )
    .failure_notice()
    .unwrap();
    assert_eq!(
        notice.title,
        "bash in Terminal ran out of memory and was stopped"
    );
    assert_eq!(notice.body, "Terminal is still open.");
    // A lost window does not turn an ending for memory into another one.
    assert!(matches!(
        event("Mail", app(Cause::Kernel))
            .with_window_lost(true)
            .kind,
        LaunchEventKind::OutOfMemory(_)
    ));
}

#[cfg(unix)]
#[test]
fn memory_ends_a_launch_only_when_the_launch_did_not_end_on_its_own() {
    use ::launcher::memory_pressure::Cause;
    use std::os::unix::process::ExitStatusExt as _;
    let killed: std::io::Result<std::process::ExitStatus> =
        Ok(std::process::ExitStatus::from_raw(libc::SIGKILL));
    let clean: std::io::Result<std::process::ExitStatus> =
        Ok(std::process::ExitStatus::from_raw(0));
    assert_eq!(
        memory_end_of(&killed, Some(Cause::Runaway)),
        Some(MemoryEnd {
            cause: Cause::Runaway,
            job: None
        })
    );
    assert_eq!(memory_end_of(&killed, None), None);
    assert_eq!(
        memory_end_of(&clean, Some(Cause::Kernel)),
        None,
        "a helper the kernel ended earlier does not make a clean exit a crash"
    );
}

#[test]
fn a_window_lost_unasked_is_reported_however_the_process_exits() {
    clear_launched_for_test();
    attribute_surface_launch(7 << 32 | 3, "Broken App");
    note_surface_gone(7 << 32 | 3, false);
    assert!(take_launch_vanished("Broken App"));
    assert!(
        !take_launch_vanished("Broken App"),
        "the mark is taken once"
    );

    attribute_surface_launch(7 << 32 | 4, "Broken App");
    note_surface_gone(7 << 32 | 4, true);
    assert!(
        !take_launch_vanished("Broken App"),
        "a window someone closed is not lost"
    );

    let lost = launch_event(LaunchEventKind::Exited).with_window_lost(true);
    assert!(matches!(
        lost.kind,
        LaunchEventKind::WindowLost { code: Some(0), .. }
    ));
    let notice = lost.failure_notice().expect("a lost window is reported");
    assert_eq!(notice.title, "Broken App closed unexpectedly");
    assert!(
        launch_event(LaunchEventKind::Exited)
            .with_window_lost(false)
            .failure_notice()
            .is_none(),
        "a clean exit after a close stays quiet"
    );

    let stopped = launch_event(LaunchEventKind::Failed {
        code: Some(ui::app::WINDOW_LOST_EXIT_CODE),
        stderr_tail: format!(
            "error: {}: its drawing surface could not be resized: Validation Error",
            ui::app::WINDOW_LOST
        ),
    });
    let notice = stopped.failure_notice().expect("failure has a notice");
    assert_eq!(notice.title, "Broken App closed unexpectedly");
    assert_eq!(
        notice.body,
        "Its window stopped without being closed. Open it again from Applications; \
         the desktop log says why."
    );
}

#[test]
fn automatic_restore_gate_is_released_by_the_existing_expiry_paths() {
    let package = "dev.tilo.restore-expiry";
    set_pending_placement(package, crate::shell::tab_binding::Placement::IntoTab(55));
    assert!(has_unclaimed_launch(package));
    assert!(has_unclaimed_launch("RESTORE-EXPIRY"));
    assert!(!has_unclaimed_launch("dev.tilo.another-restore"));

    // A clean parent exit retains its destination for the existing
    // handoff grace. Expiry of that grace releases automatic siblings.
    let now = Instant::now();
    retire_pending_placement_at(package, now);
    assert!(has_unclaimed_launch(package));
    assert_eq!(
        expire_pending_placements_at(now),
        vec![package],
        "a windowless retired launch reports its failure as it releases the gate"
    );
    assert!(!has_unclaimed_launch(package));

    // A still-running but windowless parent also cannot pin restore.
    set_pending_placement(package, crate::shell::tab_binding::Placement::IntoTab(66));
    let deadline = lock(&LaunchSession::current().pending)[0].present_before;
    assert_eq!(expire_pending_placements_at(deadline), vec![package]);
    assert!(!has_unclaimed_launch(package));
    clear_pending_placements();
}

/// Restoring a host app's window notes the launch for its tab, finds
/// the app is a `.desktop` entry, and launches that: still one launch.
/// A second note stayed queued and would pull the next window of the
/// app, opened by the person, into the restored tab.
#[test]
fn a_restored_host_window_is_noted_once_under_the_host_apps_identities() {
    use crate::shell::tab_binding::Placement;
    clear_pending_placements();
    let noted = "dev.tilo.restored-host";
    set_pending_placement(noted, Placement::IntoTab(77));
    assert!(retarget_pending_placement(
        noted,
        "org.example.Terminal",
        &["Terminal".to_string(), "org.example.Terminal".to_string()],
    ));
    assert_eq!(
        lock(&LaunchSession::current().pending).len(),
        1,
        "one launch, one note"
    );
    assert_eq!(
        placement_for_toplevel("Terminal", 501),
        Some(Placement::IntoTab(77)),
        "the restored window lands in its tab, found by the host app's identity"
    );
    assert_eq!(
        placement_for_toplevel("Terminal", 502),
        None,
        "the next window of the app is the person's own, not the restored tab's"
    );
    assert!(
        !retarget_pending_placement("dev.tilo.never-noted", "x", &[]),
        "nothing noted: the caller notes the launch itself"
    );
    clear_pending_placements();
}

#[test]
fn saved_opening_status_only_tracks_live_exact_restore_destinations() {
    clear_pending_placements();
    let package = "dev.tilo.restore-status-lifetime";
    set_pending_placement(package, crate::shell::tab_binding::Placement::IntoTab(91));
    set_pending_placement(package, crate::shell::tab_binding::Placement::NewGroup);
    set_pending_placement(package, crate::shell::tab_binding::Placement::IntoTab(92));
    assert_eq!(
        pending_restore_tabs(),
        std::collections::HashSet::from([91, 92])
    );
    let deadline = lock(&LaunchSession::current().pending)
        .iter()
        .map(|note| note.present_before)
        .max()
        .unwrap();
    assert!(
        pending_restore_tabs_at(deadline).is_empty(),
        "expired tickets cannot keep a saved card opening before the event drain"
    );
    assert_eq!(
        lock(&LaunchSession::current().pending).len(),
        3,
        "status observation must not consume or reorder placements"
    );
    clear_pending_placements();
    set_pending_placement(package, crate::shell::tab_binding::Placement::IntoTab(93));
    retire_pending_placement_at(package, Instant::now());
    assert!(
        pending_restore_tabs().is_empty(),
        "retired destination is recoverable"
    );
    clear_pending_placements();
}

/// The startup metrics of `package_id`'s launches. The metrics registry
/// is process-wide, and tests in other sessions launch alongside.
fn launch_metrics_of(package_id: &str) -> Vec<(u64, String, bool, Option<u64>)> {
    tilo_startup_metrics::launch_snapshot()
        .into_iter()
        .filter(|metric| metric.1 == package_id)
        .collect()
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn pending_launch_placements_are_claimed_fifo_and_expire_at_their_deadline() {
    fn restored_launch_keeps_the_source_crdt_record_identity() {
        let room = RoomId::new();
        clear_pending_placements();
        set_pending_placement(
            "so.tilo.browser",
            crate::shell::tab_binding::Placement::IntoTab(77),
        );
        stage_continuation_record(room, 77, 0xdead_beef);
        assert_eq!(
            take_staged_resume_for_package("so.tilo.browser", room),
            Some((None, 0xdead_beef))
        );
        clear_pending_placements();
    }

    fn same_app_launch_placements_are_claimed_fifo() {
        clear_pending_placements();
        set_pending_placement(
            "so.tilo.same-app",
            crate::shell::tab_binding::Placement::IntoTab(11),
        );
        set_pending_placement(
            "so.tilo.same-app",
            crate::shell::tab_binding::Placement::IntoTab(22),
        );
        let command = LaunchCommand {
            program: PathBuf::new(),
            args: Vec::new(),
            env: Vec::new(),
            service_identity: None,
            portal_workspace_id: None,
            managed_agent_id: None,
            x11_workspace: None,
        };
        let first = diagnostic_attempt_for("so.tilo.same-app", &command).unwrap();
        let second = diagnostic_attempt_for("so.tilo.same-app", &command).unwrap();
        assert_ne!(
            first, second,
            "concurrent supervisors must own distinct diagnostic attempts before either spawns"
        );
        assert_eq!(diagnostic_attempt_for("so.tilo.same-app", &command), None);

        assert_eq!(
            placement_for_toplevel("same-app", 101),
            Some(crate::shell::tab_binding::Placement::IntoTab(11))
        );
        assert_eq!(
            placement_for_toplevel("same-app", 202),
            Some(crate::shell::tab_binding::Placement::IntoTab(22))
        );
        clear_pending_placements();
    }

    fn launch_latency_tracks_fifo_surface_until_first_frame_once() {
        clear_pending_placements();
        set_pending_placement(
            "dev.tilo.timed",
            crate::shell::tab_binding::Placement::NewGroup,
        );
        let selected = launch_metrics_of("dev.tilo.timed")[0].0;
        let attempt = mark_launch_spawned("dev.tilo.timed", 4242, Some(selected))
            .expect("a staged launch must expose its exact diagnostic attempt");
        assert_eq!(
            mark_launch_spawned("dev.tilo.timed", 4243, Some(selected)),
            None
        );
        assert_eq!(mark_launch_spawned("dev.tilo.unknown", 4244, None), None);
        assert_eq!(
            placement_for_toplevel("timed", 909),
            Some(crate::shell::tab_binding::Placement::NewGroup)
        );
        let metrics = launch_metrics_of("dev.tilo.timed");
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].0, attempt);
        assert!(metrics[0].2);
        assert_eq!(metrics[0].3, Some(909));
        record_first_present(909, 1);
        assert!(launch_metrics_of("dev.tilo.timed").is_empty());
        record_first_present(909, 2);
        assert!(launch_metrics_of("dev.tilo.timed").is_empty());
        clear_pending_placements();
    }

    fn failed_newest_same_app_launch_keeps_older_destination() {
        clear_pending_placements();
        set_pending_placement(
            "so.tilo.failed-newest",
            crate::shell::tab_binding::Placement::IntoTab(11),
        );
        set_pending_placement(
            "so.tilo.failed-newest",
            crate::shell::tab_binding::Placement::IntoTab(22),
        );
        let metrics = launch_metrics_of("so.tilo.failed-newest");
        assert_eq!(metrics.len(), 2);
        let older_metric = metrics[0].0;
        let newer_metric = metrics[1].0;
        clear_pending_placement("so.tilo.failed-newest");
        let metrics = launch_metrics_of("so.tilo.failed-newest");
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].0, older_metric);
        assert_ne!(metrics[0].0, newer_metric);

        assert_eq!(
            placement_for_toplevel("failed-newest", 101),
            Some(crate::shell::tab_binding::Placement::IntoTab(11)),
            "clearing the failed second launch must not erase the first launch's placement"
        );
        assert_eq!(placement_for_toplevel("failed-newest", 202), None);
        clear_pending_placements();
    }

    fn clean_parent_exit_keeps_delayed_window_placement_until_deadline() {
        clear_pending_placements();
        let package = "so.tilo.daemonised";
        let wanted = crate::shell::tab_binding::Placement::IntoTab(44);
        set_pending_placement(package, wanted);

        // A deadline no run reaches before the lookup below.
        retire_pending_placement_at(package, Instant::now() + Duration::from_secs(3600));
        assert_eq!(
            placement_for_toplevel("daemonised", 701),
            Some(wanted),
            "a GUI child mapping after its launcher parent exits keeps the user's destination"
        );
        clear_pending_placements();
    }

    fn windowless_clean_launch_expires_without_sleeping() {
        clear_pending_placements();
        let package = "so.tilo.windowless";
        set_pending_placement(package, crate::shell::tab_binding::Placement::IntoTab(55));

        let now = Instant::now();
        retire_pending_placement_at(package, now);
        assert_eq!(expire_pending_placements_at(now), vec![package.to_string()]);
        assert!(
            expire_pending_placements_at(now).is_empty(),
            "retirement reports once"
        );
        assert_eq!(placement_for_toplevel("windowless", 702), None);
        clear_pending_placements();
    }

    fn live_windowless_launch_reports_its_key_at_the_presentation_deadline() {
        clear_pending_placements();
        let package = "dev.tilo.never-mapped";
        set_pending_placement(package, crate::shell::tab_binding::Placement::IntoTab(57));
        let deadline = lock(&LaunchSession::current().pending)[0].present_before;

        assert_eq!(
            expire_pending_placements_at(deadline),
            vec![package.to_string()]
        );
        assert_eq!(placement_for_toplevel("never-mapped", 704), None);
        clear_pending_placements();
    }

    fn claimed_placement_expires_without_a_second_surface() {
        clear_pending_placements();
        let package = "so.tilo.claimed-once";
        let wanted = crate::shell::tab_binding::Placement::IntoTab(56);
        set_pending_placement(package, wanted);

        assert_eq!(placement_for_toplevel("claimed-once", 703), Some(wanted));
        let deadline = lock(&LaunchSession::current().pending)[0]
            .retire_after
            .expect("claim receives a bounded retirement deadline");
        assert!(
            expire_pending_placements_at(deadline).is_empty(),
            "a mapped window must not be reported as a windowless launch"
        );
        assert_eq!(placement_for_toplevel("claimed-once", 703), None);
        clear_pending_placements();
    }

    restored_launch_keeps_the_source_crdt_record_identity();
    same_app_launch_placements_are_claimed_fifo();
    launch_latency_tracks_fifo_surface_until_first_frame_once();
    failed_newest_same_app_launch_keeps_older_destination();
    clean_parent_exit_keeps_delayed_window_placement_until_deadline();
    windowless_clean_launch_expires_without_sleeping();
    live_windowless_launch_reports_its_key_at_the_presentation_deadline();
    claimed_placement_expires_without_a_second_surface();
}

#[cfg(target_os = "linux")]
#[test]
fn live_child_with_open_stderr_does_not_block_exit_supervision() {
    use std::os::unix::fs::PermissionsExt;

    const PACKAGE_ID: &str = "so.tilo.live-gui";
    let tmp = tempfile::tempdir().unwrap();
    let script = tmp.path().join("live-gui");
    std::fs::write(
        &script,
        "#!/bin/sh\nprintf 'starting without closing stderr\\n' >&2\nsleep 0.15\nexit 0\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&script).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&script, permissions).unwrap();

    let _ = drain_events();
    set_pending_placement(
        PACKAGE_ID,
        crate::shell::tab_binding::Placement::IntoTab(77),
    );
    spawn_launch(
        PACKAGE_ID.into(),
        "Live GUI".into(),
        LaunchCommand {
            program: script,
            args: Vec::new(),
            env: Vec::new(),
            service_identity: None,
            portal_workspace_id: None,
            managed_agent_id: None,
            x11_workspace: None,
        },
    )
    .unwrap();

    let deadline = tilo_test_env::hang_deadline();
    let mut saw_started = false;
    let mut saw_exit = false;
    while Instant::now() < deadline && !saw_exit {
        for event in drain_events() {
            if event.package_id != PACKAGE_ID {
                continue;
            }
            match event.kind {
                LaunchEventKind::Started { .. } => saw_started = true,
                LaunchEventKind::Exited => saw_exit = true,
                other => panic!("unexpected launch event: {other:?}"),
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(saw_started, "the child was reported started");
    assert!(
        saw_exit,
        "the supervisor must wait for a live GUI child without synchronously blocking on its open stderr pipe"
    );
    let history = lock(&LaunchSession::current().launch_history).value();
    let attempt = history["attempts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|attempt| attempt["package_id"] == PACKAGE_ID)
        .unwrap();
    assert_eq!(attempt["saved_tab"], 77);
    assert_eq!(attempt["exit"]["success"], true);
    assert_eq!(attempt["exit"]["stderr_redacted"], true);
    assert_eq!(attempt["exit"]["retained_stderr_lines"], 1);
    assert!(attempt["process"]["kernel_start_ticks"].as_u64().unwrap() > 0);
    assert!(
        attempt["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["phase"] == "exited")
    );
    assert!(
        !history
            .to_string()
            .contains("starting without closing stderr")
    );
    assert!(
        LaunchSession::default()
            .launch_history
            .lock()
            .unwrap()
            .value()["attempts"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    clear_pending_placements();
}
#[cfg(target_os = "linux")]
#[test]
fn failed_launch_retains_exact_record_process_and_exit_without_app_output() {
    use std::os::unix::fs::PermissionsExt as _;
    const PACKAGE: &str = "dev.tilo.failed-diagnostic";
    let directory = tempfile::tempdir().unwrap();
    let script = directory.path().join("failed-app");
    std::fs::write(
        &script,
        "#!/bin/sh\nprintf 'PRIVATE_OUTPUT_MUST_STAY_REDACTED\\n' >&2\nexit 17\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    set_pending_placement(PACKAGE, crate::shell::tab_binding::Placement::IntoTab(78));
    attach_continuity_instance(PACKAGE, 902);
    spawn_launch(
        PACKAGE.into(),
        "Fixture".into(),
        LaunchCommand {
            program: script,
            args: Vec::new(),
            env: vec![("TILO_CONTINUITY_INSTANCE".into(), "902".into())],
            service_identity: None,
            portal_workspace_id: None,
            managed_agent_id: None,
            x11_workspace: None,
        },
    )
    .unwrap();
    let deadline = tilo_test_env::hang_deadline();
    let mut finished = false;
    while Instant::now() < deadline && !finished {
        finished = drain_events().iter().any(|event| {
            event.package_id == PACKAGE
                && matches!(event.kind, LaunchEventKind::Failed { code: Some(17), .. })
        });
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(finished, "the real supervisor must observe the failure");
    let history = lock(&LaunchSession::current().launch_history).value();
    let attempt = &history["attempts"][0];
    assert_eq!(attempt["saved_tab"], 78);
    assert_eq!(attempt["record_instance"], 902);
    assert_eq!(attempt["exit"]["code"], 17);
    assert_eq!(attempt["exit"]["success"], false);
    assert_eq!(attempt["exit"]["retained_stderr_lines"], 1);
    assert!(attempt["process"]["pid"].as_u64().unwrap() > 0);
    assert!(attempt["process"]["kernel_start_ticks"].as_u64().unwrap() > 0);
    assert!(
        !history
            .to_string()
            .contains("PRIVATE_OUTPUT_MUST_STAY_REDACTED")
    );
    clear_pending_placements();
}

use std::io::Write;

#[test]
#[serial_test::serial(process_env)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn launcher_resolution_and_developer_overrides_follow_the_build_kind() {
    fn dev_build_lets_the_environment_name_the_launcher() {
        let got = launcher_bin_candidates_from(
            true,
            Some(std::ffi::OsString::from("/tmp/my/tilo-launcher")),
            Some(Path::new("/home/dev/target/debug/shell")),
        );
        assert_eq!(got[0], PathBuf::from("/tmp/my/tilo-launcher"));
        assert_eq!(
            got[1],
            PathBuf::from("/home/dev/target/debug/tilo-launcher")
        );
    }

    fn shipped_build_ignores_a_launcher_named_by_the_environment() {
        // A shipped session must not take the sandbox-building binary's
        // identity from its environment.
        let got = launcher_bin_candidates_from(
            false,
            Some(std::ffi::OsString::from("/tmp/evil/tilo-launcher")),
            Some(Path::new("/usr/bin/tilo-shell")),
        );
        assert!(
            !got.iter().any(|p| p.starts_with("/tmp")),
            "release build accepted an environment-named launcher: {got:?}"
        );
        assert_eq!(
            got,
            vec![
                PathBuf::from("/usr/bin/tilo-launcher"),
                PathBuf::from("/usr/libexec/tilo/tilo-launcher"),
                PathBuf::from("/usr/libexec/tilo-launcher"),
            ],
        );
    }

    fn the_install_paths_are_always_searched() {
        let got = launcher_bin_candidates_from(false, None, None);
        assert_eq!(
            got,
            vec![
                PathBuf::from("/usr/libexec/tilo/tilo-launcher"),
                PathBuf::from("/usr/libexec/tilo-launcher"),
            ],
        );
    }

    fn debug_launch_forwards_only_the_matched_developer_app_overlay() {
        let names = [
            "TILO_APPS_SYSTEM_DIR",
            "TILO_APPS_SYSTEM_PAYLOAD_DIR",
            "TILO_RUNTIME_BUNDLE_DIR",
            "TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE",
        ];
        let mut env = tilo_test_env::ScopedEnv::new();
        for (index, name) in names.iter().enumerate() {
            env.set(name, format!("/developer/overlay/{index}"));
        }
        let command = build_launch_command(
            &test_app("so.tilo.settings"),
            Path::new("/usr/libexec/tilo/tilo-launcher"),
            Path::new("/run/user/1000/wayland-3"),
            RoomId::new(),
            None,
            None,
            None,
            None,
        );
        if crate::compositor::dev_env::DEV_ENV_OVERRIDES {
            for (index, name) in names.iter().enumerate() {
                assert!(
                    command
                        .env
                        .contains(&((*name).to_string(), format!("/developer/overlay/{index}")))
                );
            }
        } else {
            assert!(
                command
                    .env
                    .iter()
                    .all(|(name, _)| !names.contains(&name.as_str()))
            );
        }
    }

    fn a_release_compositor_strips_developer_overrides_from_the_launcher() {
        let env = vec![("XDG_RUNTIME_DIR".to_string(), "/run/user/1000".to_string())];
        let mut command = std::process::Command::new("/usr/libexec/tilo/tilo-launcher");
        apply_launch_env_with(&mut command, &env, false);
        for name in launcher::dev_env::DEV_OVERRIDE_VARS {
            assert_eq!(
                launcher_env_entry(&command, name),
                LauncherEnv::Removed,
                "{name} must be removed from the launcher's environment",
            );
        }
        assert_eq!(
            launcher_env_entry(&command, "XDG_RUNTIME_DIR"),
            LauncherEnv::Set("/run/user/1000".to_string()),
        );
        // The overlay the developer session relies on is covered by the list.
        for name in [
            "TILO_APPS_SYSTEM_DIR",
            "TILO_APPS_SYSTEM_PAYLOAD_DIR",
            "TILO_RUNTIME_BUNDLE_DIR",
            "TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE",
        ] {
            assert!(
                launcher::dev_env::DEV_OVERRIDE_VARS.contains(&name),
                "{name}"
            );
        }
    }

    fn a_developer_compositor_leaves_the_launcher_environment_inherited() {
        let env = vec![(
            "TILO_RUNTIME_BUNDLE_DIR".to_string(),
            "/dev/runtime".to_string(),
        )];
        let mut command = std::process::Command::new("/usr/libexec/tilo/tilo-launcher");
        apply_launch_env_with(&mut command, &env, true);
        assert_eq!(
            launcher_env_entry(&command, "TILO_RUNTIME_BUNDLE_DIR"),
            LauncherEnv::Set("/dev/runtime".to_string()),
        );
        assert_eq!(
            launcher_env_entry(&command, "TILO_JIT_ENTITLED_FILE"),
            LauncherEnv::Inherited
        );
    }

    dev_build_lets_the_environment_name_the_launcher();
    shipped_build_ignores_a_launcher_named_by_the_environment();
    the_install_paths_are_always_searched();
    debug_launch_forwards_only_the_matched_developer_app_overlay();
    a_release_compositor_strips_developer_overrides_from_the_launcher();
    a_developer_compositor_leaves_the_launcher_environment_inherited();
}

// Tests that repoint process-global env vars are serialised on
// `process_env`, shared with other tests touching the same variables.

/// Write a registry + manifest pair into `dir` and point the
/// launcher's registry reader at it. Returns the app's payload root.
fn install_fixture(dir: &Path, package_id: &str, entrypoint_type: &str) -> PathBuf {
    use std::fmt::Write as _;
    let payload = dir.join("payloads").join(package_id);
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    let manifest_path = payload.join("manifest.toml");
    std::fs::write(
        &manifest_path,
        crate::shell::test_support::canonical_manifest(
            &format!("id = \"{package_id}\"\nversion = \"1.2.3\"\nkind = \"app\""),
            &format!("[entrypoint]\nbin = \"bin/app\"\ntype = \"{entrypoint_type}\"\n"),
        ),
    )
    .unwrap();
    let registry = dir.join("registry.toml");
    let mut existing = std::fs::read_to_string(&registry).unwrap_or_default();
    write!(
        existing,
        "[[entry]]\nid = '{package_id}'\npayload_root = {:?}\nmanifest_path = {:?}\n",
        payload.display().to_string(),
        manifest_path.display().to_string()
    )
    .unwrap();
    std::fs::write(&registry, existing).unwrap();
    payload
}

/// The compositor loop asks for installed apps while building tiles and
/// naming apps in Activity. It is served the last enumeration, stale or
/// not, and never reads the registry itself: a stale list is refreshed
/// on a worker and the refresh lands for a later ask.
#[test]
#[serial_test::serial(process_env)]
fn the_loop_is_served_the_last_app_list_while_a_worker_refreshes_it() {
    let tmp = tempfile::tempdir().unwrap();
    let payload = install_fixture(tmp.path(), "dev.tilo.calculator", "gui");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_APPS_DATA_DIR", tmp.path());
    invalidate_app_cache();
    let initial = installed_apps().unwrap();
    assert_eq!(initial[0].version, "1.2.3");

    let manifest = payload.join("manifest.toml");
    let changed_manifest = std::fs::read_to_string(&manifest)
        .unwrap()
        .replace("1.2.3", "1.2.4");
    std::fs::write(&manifest, changed_manifest).unwrap();
    lock(&APP_CACHE).as_mut().unwrap().1 = None;

    let served = installed_apps_without_waiting();
    assert_eq!(
        served, initial,
        "the stale list is served; the registry was not read here"
    );
    let refreshed = tilo_test_env::wait_for("the refreshed app list", || {
        let served = installed_apps_without_waiting();
        served
            .first()
            .is_some_and(|app| app.version == "1.2.4")
            .then_some(served)
    });
    env.remove("TILO_APPS_DATA_DIR");
    invalidate_app_cache();
    assert_eq!(refreshed.len(), 1);
}

#[test]
#[serial_test::serial(process_env)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn app_registry_enumeration_reads_signed_manifests_and_keeps_generations() {
    fn unchanged_app_cache_refresh_preserves_search_generation() {
        let tmp = tempfile::tempdir().unwrap();
        let payload = install_fixture(tmp.path(), "dev.tilo.calculator", "gui");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_APPS_DATA_DIR", tmp.path());
        invalidate_app_cache();
        let (snapshot_generation, initial_gui) = gui_apps_snapshot();
        let initial_generation = gui_apps_generation();
        assert_eq!(
            snapshot_generation, initial_generation,
            "populating a cold cache must not invalidate its own snapshot"
        );
        assert_eq!(initial_gui.len(), 1);
        let initial = installed_apps().unwrap();
        let expire = || {
            lock(&APP_CACHE).as_mut().unwrap().1 = None;
        };
        expire();
        let unchanged = installed_apps().unwrap();
        let unchanged_generation = gui_apps_generation();

        let manifest = payload.join("manifest.toml");
        let changed_manifest = std::fs::read_to_string(&manifest)
            .unwrap()
            .replace("1.2.3", "1.2.4");
        std::fs::write(&manifest, changed_manifest).unwrap();
        expire();
        let changed = installed_apps().unwrap();
        let changed_generation = gui_apps_generation();

        std::fs::write(tmp.path().join("registry.toml"), "").unwrap();
        expire();
        let removed = installed_apps().unwrap();
        let removed_generation = gui_apps_generation();
        env.remove("TILO_APPS_DATA_DIR");
        invalidate_app_cache();

        assert_eq!(unchanged, initial);
        assert_eq!(
            unchanged_generation, initial_generation,
            "an unchanged TTL refresh must not invalidate retained search results"
        );
        assert_eq!(changed[0].version, "1.2.4");
        assert_ne!(changed_generation, unchanged_generation);
        assert!(removed.is_empty());
        assert_ne!(removed_generation, changed_generation);
    }

    fn raced_app_cache_refresh_keeps_its_read_as_the_baseline() {
        let tmp = tempfile::tempdir().unwrap();
        let payload = install_fixture(tmp.path(), "dev.tilo.calculator", "gui");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_APPS_DATA_DIR", tmp.path());
        invalidate_app_cache();
        // Seed and re-read through `record_app_read` only. `installed_apps`
        // arms the real watch; earlier helpers (or a dropped tempdir) can
        // leave that watch firing `expire_watched_app_cache` mid-read and
        // falsely fail the uncontested memoisation assert below.
        record_app_read(
            APP_CACHE_EPOCH.load(Ordering::Acquire),
            &read_installed_apps().unwrap(),
        );
        let manifest = payload.join("manifest.toml");
        let changed_manifest = std::fs::read_to_string(&manifest)
            .unwrap()
            .replace("1.2.3", "1.2.4");
        std::fs::write(&manifest, changed_manifest).unwrap();
        let before_race = gui_apps_generation();
        // A watch event lands while this refresh is reading the registry.
        let read_epoch = APP_CACHE_EPOCH.load(Ordering::Acquire);
        let raced = read_installed_apps().unwrap();
        expire_watched_app_cache();
        let raced_snapshot_generation = record_app_read(read_epoch, &raced);
        assert_eq!(raced_snapshot_generation, 0, "a raced read stays stale");
        let raced_generation = gui_apps_generation();
        let stale = lock(&APP_CACHE).as_ref().unwrap().1.is_none();
        let reread_epoch = APP_CACHE_EPOCH.load(Ordering::Acquire);
        let reread = read_installed_apps().unwrap();
        record_app_read(reread_epoch, &reread);
        let reread_generation = gui_apps_generation();
        let fresh = lock(&APP_CACHE).as_ref().unwrap().1.is_some();
        env.remove("TILO_APPS_DATA_DIR");
        invalidate_app_cache();

        assert_eq!(raced[0].version, "1.2.4");
        assert_ne!(raced_generation, before_race, "the raced read saw a change");
        assert!(stale, "a raced read must not be memoised as fresh");
        assert_eq!(reread, raced);
        assert_eq!(
            reread_generation, raced_generation,
            "an identical re-read after a raced refresh keeps the generation"
        );
        assert!(fresh, "the uncontested re-read is memoised");
    }

    fn enumeration_reads_the_registry_and_flags_gui_apps() {
        let tmp = tempfile::tempdir().unwrap();
        install_fixture(tmp.path(), "so.tilo.hello", "gui");
        install_fixture(tmp.path(), "so.tilo.notes-cli", "cli");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_APPS_DATA_DIR", tmp.path());
        invalidate_app_cache();
        let apps = read_installed_apps().expect("registry readable");
        let offered = gui_apps(); // what the palette actually offers
        env.remove("TILO_APPS_DATA_DIR");
        invalidate_app_cache();

        assert_eq!(apps.len(), 2, "both packages enumerate: {apps:?}");
        let hello = apps
            .iter()
            .find(|a| a.package_id == "so.tilo.hello")
            .expect("hello present");
        assert!(hello.has_gui, "gui entrypoint must be flagged as GUI");
        assert_eq!(hello.display_name, "Hello");
        assert_eq!(hello.version, "1.2.3");
        let cli = apps
            .iter()
            .find(|a| a.package_id == "so.tilo.notes-cli")
            .expect("cli package present");
        assert!(!cli.has_gui, "a cli-only package is not a GUI app");

        // Enumerating it is fine; offering it to launch is not — a
        // headless package has no window to show.
        let offered_ids: Vec<&str> = offered.iter().map(|a| a.package_id.as_str()).collect();
        assert_eq!(
            offered_ids,
            vec!["so.tilo.hello"],
            "only the GUI package is launchable from the palette",
        );
    }

    fn installed_continuity_lookup_reads_signed_manifest_contract() {
        let tmp = tempfile::tempdir().unwrap();
        let payload = install_fixture(tmp.path(), "so.tilo.browser", "gui");
        let manifest_path = payload.join("manifest.toml");
        let mut manifest = std::fs::OpenOptions::new()
            .append(true)
            .open(&manifest_path)
            .unwrap();
        writeln!(
            manifest,
            "\n[continuity]\nrequired = [\"presentation\", \"semantic\"]\nsemantic-schema = \"web-location-v1\""
        )
        .unwrap();

        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_APPS_DATA_DIR", tmp.path());
        let required = installed_continuity_contract("so.tilo.browser");
        let legacy = installed_continuity_contract("so.tilo.not-installed");
        env.remove("TILO_APPS_DATA_DIR");

        assert_eq!(
            required,
            InstalledContinuityRequirements::Required(launcher::manifest::ContinuityContract {
                required: vec![
                    launcher::manifest::ContinuityFacet::Presentation,
                    launcher::manifest::ContinuityFacet::Semantic,
                ],
                semantic_schema: Some(launcher::manifest::ContinuitySemanticSchema::WebLocationV1,),
            })
        );
        assert_eq!(legacy, InstalledContinuityRequirements::NotInstalled);
    }

    fn display_name_title_cases_the_trailing_component() {
        assert_eq!(display_name_for_package_id("so.tilo.hello"), "Hello");
        assert_eq!(
            display_name_for_package_id("so.tilo.text-editor"),
            "Text Editor"
        );
        assert_eq!(display_name_for_package_id("Notes"), "Notes");
        // A degenerate id must not produce an empty (unclickable) label.
        assert!(!display_name_for_package_id("so.tilo.").is_empty());
    }

    fn managed_entry_registry_generation_changes_only_with_its_snapshot() {
        let first = set_windows_compat_entry_ids(vec!["windows-one".into()]);
        let unchanged = set_windows_compat_entry_ids(vec!["windows-one".into()]);
        let second = set_windows_compat_entry_ids(vec!["windows-two".into()]);
        assert_eq!(unchanged, first);
        assert!(second > first);
    }

    raced_app_cache_refresh_keeps_its_read_as_the_baseline();
    unchanged_app_cache_refresh_preserves_search_generation();
    enumeration_reads_the_registry_and_flags_gui_apps();
    installed_continuity_lookup_reads_signed_manifest_contract();
    display_name_title_cases_the_trailing_component();
    managed_entry_registry_generation_changes_only_with_its_snapshot();
}

#[test]
#[serial_test::serial(process_env)]
fn app_registry_watch_invalidates_without_waiting_for_ttl() {
    let tmp = tempfile::tempdir().unwrap();
    let payload = install_fixture(tmp.path(), "dev.tilo.calculator", "gui");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_APPS_DATA_DIR", tmp.path());
    invalidate_app_cache();
    let initial = installed_apps().unwrap();
    assert_eq!(initial[0].version, "1.2.3");
    let manifest = payload.join("manifest.toml");
    let changed = std::fs::read_to_string(&manifest)
        .unwrap()
        .replace("1.2.3", "1.2.4");
    std::fs::write(&manifest, changed).unwrap();
    tilo_test_env::wait_until("inotify invalidates the app cache", || {
        installed_apps().is_ok_and(|apps| apps.first().is_some_and(|app| app.version == "1.2.4"))
    });
    env.remove("TILO_APPS_DATA_DIR");
    invalidate_app_cache();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn launch_command_carries_exactly_the_resolved_payload() {
    fn launch_command_carries_manifest_payload_and_display() {
        let app = InstalledApp {
            package_id: "so.tilo.hello".into(),
            display_name: "Hello".into(),
            localized_names: BTreeMap::default(),
            keywords: Vec::new(),
            category: None,
            version: "0.1.0".into(),
            manifest_path: PathBuf::from("/apps/hello/manifest.toml"),
            payload_root: PathBuf::from("/apps/hello"),
            has_gui: true,
            opens_extensions: Vec::new(),
            opens_mime_types: Vec::new(),
            opens_uri_schemes: Vec::new(),
            opens_secondary: false,
            offers: Vec::new(),
        };
        let cmd = build_launch_command(
            &app,
            Path::new("/usr/libexec/tilo/tilo-launcher"),
            Path::new("/run/user/1000/wayland-3"),
            RoomId::new(),
            None,
            None,
            Some(Path::new("/dev/dri/renderD128")),
            None,
        );
        assert_eq!(cmd.program, Path::new("/usr/libexec/tilo/tilo-launcher"));
        assert_eq!(cmd.args[0], "run");
        let pos = |flag: &str| {
            cmd.args
                .iter()
                .position(|a| a == flag)
                .unwrap_or_else(|| panic!("{flag} missing from {:?}", cmd.args))
        };
        assert_eq!(cmd.args[pos("--manifest") + 1], "/apps/hello/manifest.toml");
        assert_eq!(cmd.args[pos("--payload-root") + 1], "/apps/hello");
        assert_eq!(
            cmd.args[pos("--wayland-socket") + 1],
            "/run/user/1000/wayland-3",
            "the app must be pointed at THIS compositor's socket"
        );
        assert_eq!(
            cmd.args[pos("--render-node") + 1],
            "/dev/dri/renderD128",
            "the app must be pointed at THIS compositor's GPU"
        );
    }

    /// No render node resolved means no `--render-node` flag at all — an
    /// empty or invented value would read to both loaders as "no drivers".
    fn launch_command_omits_the_render_node_when_none_resolved() {
        let app = InstalledApp {
            package_id: "so.tilo.hello".into(),
            display_name: "Hello".into(),
            localized_names: BTreeMap::default(),
            keywords: Vec::new(),
            category: None,
            version: "0.1.0".into(),
            manifest_path: PathBuf::from("/apps/hello/manifest.toml"),
            payload_root: PathBuf::from("/apps/hello"),
            has_gui: true,
            opens_extensions: Vec::new(),
            opens_mime_types: Vec::new(),
            opens_uri_schemes: Vec::new(),
            opens_secondary: false,
            offers: Vec::new(),
        };
        let cmd = build_launch_command(
            &app,
            Path::new("/usr/libexec/tilo/tilo-launcher"),
            Path::new("/run/user/1000/wayland-3"),
            RoomId::new(),
            None,
            None,
            None,
            None,
        );
        assert!(
            !cmd.args.iter().any(|a| a == "--render-node"),
            "no node resolved must mean no flag: {:?}",
            cmd.args
        );
    }

    fn launch_command_names_the_app_id_and_omits_capabilities_when_none_are_granted() {
        let cmd = build_launch_command(
            &test_app("so.tilo.books"),
            Path::new("/usr/libexec/tilo/tilo-launcher"),
            Path::new("/run/user/1000/wayland-3"),
            RoomId::new(),
            None,
            None,
            None,
            None,
        );
        assert!(
            cmd.args
                .windows(2)
                .any(|w| w[0] == "--app-id" && w[1] == "so.tilo.books"),
            "the launcher must be told which app id capabilities are addressed to: {:?}",
            cmd.args
        );
        assert!(
            !cmd.args.iter().any(|a| a == "--capabilities"),
            "an app granted nothing must not name a capability file: {:?}",
            cmd.args
        );
    }

    fn launch_command_names_the_capability_file_when_one_was_written() {
        let cmd = build_launch_command(
            &test_app("so.tilo.books"),
            Path::new("/usr/libexec/tilo/tilo-launcher"),
            Path::new("/run/user/1000/wayland-3"),
            RoomId::new(),
            Some(Path::new(
                "/run/user/1000/tilo/capabilities/so.tilo.books.json",
            )),
            None,
            None,
            None,
        );
        let pos = cmd
            .args
            .iter()
            .position(|a| a == "--capabilities")
            .expect("--capabilities flag");
        assert_eq!(
            cmd.args[pos + 1],
            "/run/user/1000/tilo/capabilities/so.tilo.books.json"
        );
    }

    fn launch_command_names_the_workspace_selected_identity() {
        let cmd = build_launch_command(
            &test_app("so.tilo.books"),
            Path::new("/usr/libexec/tilo/tilo-launcher"),
            Path::new("/run/user/1000/wayland-3"),
            RoomId::new(),
            None,
            Some(Path::new("/home/alice/.config/tilo/identity/alice.json")),
            None,
            None,
        );
        let position = cmd
            .args
            .iter()
            .position(|arg| arg == "--identity")
            .expect("--identity flag");
        assert_eq!(
            cmd.args[position + 1],
            "/home/alice/.config/tilo/identity/alice.json"
        );
    }

    fn workspace_agent_command_is_headless_fixed_package_service_argv() {
        let app = InstalledApp {
            package_id: WORKSPACE_AGENT_PACKAGE_ID.into(),
            display_name: "Agent".into(),
            localized_names: BTreeMap::default(),
            keywords: Vec::new(),
            category: None,
            version: "0.1.0".into(),
            manifest_path: PathBuf::from("/apps/agent/manifest.toml"),
            payload_root: PathBuf::from("/apps/agent"),
            has_gui: false,
            opens_extensions: Vec::new(),
            opens_mime_types: Vec::new(),
            opens_uri_schemes: Vec::new(),
            opens_secondary: false,
            offers: Vec::new(),
        };
        let room = RoomId::new();
        let command = build_workspace_agent_command(
            &app,
            Path::new("/usr/libexec/tilo/tilo-launcher"),
            room,
            Some(Path::new("/run/user/1000/tilo/identity.json")),
            "studio-agent",
        );

        assert!(!command.args.iter().any(|arg| arg == "--wayland-socket"));
        assert!(!command.args.iter().any(|arg| arg == "--capabilities"));
        assert_eq!(command.managed_agent_id.as_deref(), Some("studio-agent"));
        let separator = command
            .args
            .iter()
            .position(|arg| arg == "--")
            .expect("service argv has an explicit separator");
        assert_eq!(
            command
                .args
                .get(separator.wrapping_sub(1))
                .map(String::as_str),
            Some("--service")
        );
        assert_eq!(
            command.args.get(separator.saturating_add(1)..),
            Some(
                [
                    "--workspace".to_string(),
                    room.to_string(),
                    "--app".to_string(),
                    WORKSPACE_AGENT_PACKAGE_ID.to_string(),
                    "--name".to_string(),
                    "studio-agent".to_string(),
                    "--task-runs".to_string(),
                ]
                .as_slice()
            )
        );
        let room_string = room.to_string();
        assert_eq!(
            command
                .service_identity
                .as_ref()
                .map(|identity| (identity.app_id.as_str(), identity.workspace_id.as_str())),
            Some((WORKSPACE_AGENT_PACKAGE_ID, room_string.as_str()))
        );
    }

    fn agent_companion_command_is_a_headless_service_in_the_room() {
        let app = InstalledApp {
            package_id: "so.tilo.web-search".into(),
            display_name: "Web Search".into(),
            localized_names: BTreeMap::default(),
            keywords: Vec::new(),
            category: None,
            version: "0.1.0".into(),
            manifest_path: PathBuf::from("/apps/web-search/manifest.toml"),
            payload_root: PathBuf::from("/apps/web-search"),
            has_gui: false,
            opens_extensions: Vec::new(),
            opens_mime_types: Vec::new(),
            opens_uri_schemes: Vec::new(),
            opens_secondary: false,
            offers: Vec::new(),
        };
        assert!(AGENT_COMPANION_PACKAGE_IDS.contains(&app.package_id.as_str()));
        let room = RoomId::new();
        let command = build_agent_companion_command(
            &app,
            Path::new("/usr/libexec/tilo/tilo-launcher"),
            room,
            Some(Path::new("/run/user/1000/tilo/identity.json")),
        );
        assert!(!command.args.iter().any(|arg| arg == "--wayland-socket"));
        assert!(!command.args.iter().any(|arg| arg == "--capabilities"));
        // An ordinary app connection publishing under its own id, not a
        // second managed agent.
        assert_eq!(command.managed_agent_id, None);
        let separator = command
            .args
            .iter()
            .position(|arg| arg == "--")
            .expect("service argv has an explicit separator");
        assert_eq!(
            command
                .args
                .get(separator.wrapping_sub(1))
                .map(String::as_str),
            Some("--service")
        );
        assert_eq!(
            command.args.get(separator.saturating_add(1)..),
            Some(["--workspace".to_string(), room.to_string()].as_slice())
        );
        let room_string = room.to_string();
        assert_eq!(
            command
                .service_identity
                .as_ref()
                .map(|identity| (identity.app_id.as_str(), identity.workspace_id.as_str())),
            Some(("so.tilo.web-search", room_string.as_str()))
        );
    }

    launch_command_carries_manifest_payload_and_display();
    launch_command_omits_the_render_node_when_none_resolved();
    launch_command_names_the_app_id_and_omits_capabilities_when_none_are_granted();
    launch_command_names_the_capability_file_when_one_was_written();
    launch_command_names_the_workspace_selected_identity();
    workspace_agent_command_is_headless_fixed_package_service_argv();
    agent_companion_command_is_a_headless_service_in_the_room();
}

/// What the spawned launcher's environment will say about a variable.
#[derive(Debug, PartialEq, Eq)]
enum LauncherEnv {
    Inherited,
    Removed,
    Set(String),
}

fn launcher_env_entry(command: &std::process::Command, name: &str) -> LauncherEnv {
    match command
        .get_envs()
        .find(|(key, _)| *key == std::ffi::OsStr::new(name))
    {
        None => LauncherEnv::Inherited,
        Some((_, None)) => LauncherEnv::Removed,
        Some((_, Some(value))) => LauncherEnv::Set(value.to_string_lossy().into_owned()),
    }
}

#[test]
#[serial_test::serial(process_env)]
fn launch_refuses_unknown_cli_only_escaping_or_socketless_requests() {
    fn launch_refuses_an_unknown_package() {
        let tmp = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_APPS_DATA_DIR", tmp.path());
        invalidate_app_cache();
        let err = launch("so.tilo.nope", RoomId::new()).unwrap_err();
        env.remove("TILO_APPS_DATA_DIR");
        invalidate_app_cache();
        assert_eq!(
            err,
            LaunchError::NotInstalled {
                package_id: "so.tilo.nope".into()
            }
        );
        let message = err.user_message();
        assert!(
            message.contains("so.tilo.nope"),
            "the message must name the id it could not find: {message}"
        );
        // "installed" is not a word this desktop says to a user.
        assert!(
            !message.contains("install"),
            "user-facing text must not talk about installing: {message}"
        );
    }

    fn shell_owned_intent_never_falls_through_to_the_app_registry() {
        let intent = ToolIntent::WorkspaceSettings {
            route: os_actions::WorkspaceSettingsRoute::Members,
        };
        assert_eq!(
            launch_tool_intent(&intent, RoomId::new()).unwrap_err(),
            LaunchError::ShellOwnedIntent
        );
    }

    fn launch_refuses_a_cli_only_package() {
        let tmp = tempfile::tempdir().unwrap();
        install_fixture(tmp.path(), "so.tilo.notes-cli", "cli");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_APPS_DATA_DIR", tmp.path());
        invalidate_app_cache();
        let err = launch("so.tilo.notes-cli", RoomId::new()).unwrap_err();
        env.remove("TILO_APPS_DATA_DIR");
        invalidate_app_cache();
        assert_eq!(
            err,
            LaunchError::NotAGuiApp {
                package_id: "so.tilo.notes-cli".into()
            }
        );
    }

    fn launch_refuses_when_no_compositor_socket_is_published() {
        let tmp = tempfile::tempdir().unwrap();
        install_fixture(tmp.path(), "so.tilo.hello", "gui");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_APPS_DATA_DIR", tmp.path());
        invalidate_app_cache();
        set_wayland_socket_path(None);
        let err = launch("so.tilo.hello", RoomId::new()).unwrap_err();
        env.remove("TILO_APPS_DATA_DIR");
        invalidate_app_cache();
        assert_eq!(err, LaunchError::NoDisplay);
    }

    fn a_package_id_that_could_escape_the_directory_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_RUNTIME_DIR", tmp.path());
        let caps = vec![fs_grant(
            "../../etc/x",
            "/var/workspace/books",
            Access::Read,
        )];
        let escaped = write_capabilities_file("../../etc/x", &sandbox(caps.clone()));
        let slashed = write_capabilities_file("a/b", &sandbox(caps));
        env.remove("XDG_RUNTIME_DIR");
        assert!(escaped.is_err(), "`..` in a package id must be refused");
        assert!(slashed.is_err(), "`/` in a package id must be refused");
    }

    launch_refuses_an_unknown_package();
    shell_owned_intent_never_falls_through_to_the_app_registry();
    launch_refuses_a_cli_only_package();
    launch_refuses_when_no_compositor_socket_is_published();
    a_package_id_that_could_escape_the_directory_is_refused();
}

#[test]
#[serial_test::serial(process_env)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn launched_apps_exec_with_the_compositor_socket_and_match_back_by_app_id() {
    /// The production `launch` path end to end, with `tilo-launcher`
    /// replaced by a recorder that fails and is quoted back as a
    /// `LaunchEvent`.
    #[cfg(target_os = "linux")]
    fn launch_execs_the_launcher_binary_with_the_compositor_socket() {
        let tmp = tempfile::tempdir().unwrap();
        install_fixture(tmp.path(), "so.tilo.hello", "gui");
        let record = tmp.path().join("argv.txt");
        let fake = tmp.path().join("tilo-launcher");
        {
            let mut f = std::fs::File::create(&fake).unwrap();
            writeln!(f, "#!/bin/sh").unwrap();
            writeln!(f, "printf '%s\\n' \"$@\" > {}", record.display()).unwrap();
            writeln!(f, "echo \"launch refused by fixture\" >&2").unwrap();
            writeln!(f, "exit 3").unwrap();
        }
        let mut perms = std::fs::metadata(&fake).unwrap().permissions();
        {
            use std::os::unix::fs::PermissionsExt;
            perms.set_mode(0o755);
        }
        std::fs::set_permissions(&fake, perms).unwrap();

        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_APPS_DATA_DIR", tmp.path());
        env.set("TILO_LAUNCHER_BIN", &fake);
        invalidate_app_cache();
        set_wayland_socket_path(Some(PathBuf::from("/run/user/1000/wayland-7")));
        let _ = drain_events();

        let app = launch("so.tilo.hello", RoomId::new()).expect("launch accepted");
        assert_eq!(app.display_name, "Hello");

        // wait for the worker to report the child's exit
        let deadline = tilo_test_env::hang_deadline();
        let mut failed = None;
        let mut saw_started = false;
        while Instant::now() < deadline && failed.is_none() {
            for event in drain_events() {
                match event.kind {
                    LaunchEventKind::Started { pid } => {
                        assert!(pid > 0, "a real child pid is reported");
                        saw_started = true;
                    }
                    LaunchEventKind::Failed { .. } => failed = Some(event),
                    other => panic!("unexpected launch event: {other:?}"),
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        env.remove("TILO_APPS_DATA_DIR");
        env.remove("TILO_LAUNCHER_BIN");
        invalidate_app_cache();
        set_wayland_socket_path(None);

        assert!(saw_started, "the launch must report a started child");
        let failed = failed.expect("the failing child must surface a Failed event");
        match failed.kind {
            LaunchEventKind::Failed { code, stderr_tail } => {
                assert_eq!(code, Some(3), "the child's exit code is reported");
                assert!(
                    stderr_tail.contains("launch refused by fixture"),
                    "the child's stderr is quoted back: {stderr_tail:?}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }

        let argv = std::fs::read_to_string(&record).expect("the launcher binary really ran");
        let argv: Vec<&str> = argv.lines().collect();
        assert_eq!(argv[0], "run");
        assert!(
            argv.contains(&"--wayland-socket") && argv.contains(&"/run/user/1000/wayland-7"),
            "the spawned launcher is pointed at the compositor's socket: {argv:?}"
        );
        assert!(
            argv.iter()
                .any(|a| a.ends_with("payloads/so.tilo.hello/manifest.toml")),
            "the resolved manifest path is passed through: {argv:?}"
        );
        // The GPU choice must reach the launcher through the real
        // `launch()` path here, not just the pure builder.
        match crate::compositor::render_node::resolved() {
            Some(node) => {
                let pos = argv
                    .iter()
                    .position(|a| *a == "--render-node")
                    .unwrap_or_else(|| panic!("--render-node missing from {argv:?}"));
                assert_eq!(
                    argv[pos + 1],
                    node.display().to_string(),
                    "the app must be launched on the compositor's own node"
                );
            }
            None => assert!(
                !argv.contains(&"--render-node"),
                "no node resolved must mean no flag: {argv:?}"
            ),
        }
    }

    /// The launch history is process-global, so this test uses ids no
    /// other test registers rather than clearing shared state.
    fn launched_apps_are_matched_back_by_app_id() {
        remember_launch(&InstalledApp {
            package_id: "so.tilo.matchfixture".into(),
            display_name: "Match Fixture".into(),
            localized_names: BTreeMap::default(),
            keywords: Vec::new(),
            category: None,
            version: "0.1.0".into(),
            manifest_path: PathBuf::new(),
            payload_root: PathBuf::new(),
            has_gui: true,
            opens_extensions: Vec::new(),
            opens_mime_types: Vec::new(),
            opens_uri_schemes: Vec::new(),
            opens_secondary: false,
            offers: Vec::new(),
        });
        // full package id, trailing component, case-insensitivity
        assert_eq!(
            display_name_for_app_id("so.tilo.matchfixture").as_deref(),
            Some("Match Fixture")
        );
        assert_eq!(
            display_name_for_app_id("matchfixture").as_deref(),
            Some("Match Fixture")
        );
        assert_eq!(
            display_name_for_app_id("MATCHFIXTURE").as_deref(),
            Some("Match Fixture")
        );
        // an unrelated client keeps its own label
        assert_eq!(display_name_for_app_id("org.gnome.NeverLaunched"), None);
    }

    #[cfg(target_os = "linux")]
    launch_execs_the_launcher_binary_with_the_compositor_socket();
    launched_apps_are_matched_back_by_app_id();
}

use crate::shell::state::{Access, CapOverride, Scope, Subject};

fn test_app(package_id: &str) -> InstalledApp {
    InstalledApp {
        package_id: package_id.to_string(),
        display_name: display_name_for_package_id(package_id),
        localized_names: BTreeMap::default(),
        keywords: Vec::new(),
        category: None,
        version: "0.1.0".into(),
        manifest_path: PathBuf::from("/apps/x/manifest.toml"),
        payload_root: PathBuf::from("/apps/x"),
        has_gui: true,
        opens_extensions: Vec::new(),
        opens_mime_types: Vec::new(),
        opens_uri_schemes: Vec::new(),
        opens_secondary: false,
        offers: Vec::new(),
    }
}

fn fs_grant(app: &str, path: &str, verb: Access) -> CapOverride {
    CapOverride::grant(Subject::app(app), Scope::FsPath(path.to_string()), verb)
}

/// A launch asked for on the compositor thread must not do its
/// preflight there: the capability file is synced, the identity registry
/// and authority binding are files too, and a click waits on none of them.
/// The preflight here blocks until released, which happens only after the
/// call returns, so the call returning while the preflight is still held
/// proves it ran elsewhere. A refusal (no display is published) must
/// still unwind the caller's state on the worker and reach the session
/// as a launch event.
#[test]
#[serial_test::serial(process_env)]
fn queued_open_runs_its_preflight_off_the_calling_thread() {
    use std::sync::atomic::{AtomicBool, Ordering};
    set_wayland_socket_path(None);
    let _ = drain_events();
    let caller = std::thread::current().id();
    let (release, released) = std::sync::mpsc::channel::<()>();
    let (refusal, refused) = std::sync::mpsc::channel::<std::thread::ThreadId>();
    let preflight_done = std::sync::Arc::new(AtomicBool::new(false));
    let preflight_thread = std::sync::Arc::new(std::sync::Mutex::new(None));

    queue_prepared_launch(
        test_app("so.tilo.queued-open"),
        RoomId::new(),
        {
            let preflight_done = std::sync::Arc::clone(&preflight_done);
            let preflight_thread = std::sync::Arc::clone(&preflight_thread);
            move || {
                *preflight_thread.lock().unwrap() = Some(std::thread::current().id());
                let _ = released.recv_timeout(tilo_test_env::hang_bound());
                preflight_done.store(true, Ordering::SeqCst);
                Ok(LaunchGrants::default())
            }
        },
        QueuedOpen {
            document: Some(PathBuf::from("/var/workspace/books/novel.epub")),
            uri: None,
            refused: Some(Box::new(move || {
                let _ = refusal.send(std::thread::current().id());
            })),
            ..QueuedOpen::default()
        },
        None,
    )
    .expect("the preflight worker starts");
    assert!(
        !preflight_done.load(Ordering::SeqCst),
        "queueing a launch waited for its preflight: it ran on the calling thread"
    );
    assert!(
        refused.try_recv().is_err(),
        "the launch was refused before its preflight finished"
    );

    release.send(()).expect("the preflight is waiting");
    let worker = tilo_test_env::recv("the refused launch's unwind", &refused);
    assert_ne!(worker, caller, "the unwind ran on the calling thread");
    assert_ne!(
        *preflight_thread.lock().unwrap(),
        Some(caller),
        "the preflight ran on the calling thread"
    );
    tilo_test_env::wait_until("a SpawnFailed event", || {
        drain_events().into_iter().any(|event| {
            event.package_id == "so.tilo.queued-open"
                && matches!(event.kind, LaunchEventKind::SpawnFailed { .. })
        })
    });
}

fn sandbox(capabilities: Vec<CapOverride>) -> LaunchGrants {
    LaunchGrants {
        authors: Vec::new(),
        sandbox: capabilities,
    }
}

#[test]
#[serial_test::serial(process_env)]
fn published_capabilities_are_filtered_bound_read_only_and_revoked() {
    fn published_capabilities_are_filtered_to_the_launching_app() {
        set_workspace_capabilities(LaunchGrants {
            authors: vec!["so.tilo.books".into(), "so.tilo.other".into()],
            sandbox: vec![
                fs_grant("so.tilo.books", "/var/workspace/books", Access::Read),
                fs_grant("so.tilo.other", "/var/workspace/secrets", Access::Write),
                // A person's share is a capability in the same list but
                // not an app grant, and must not travel.
                CapOverride::grant(
                    Subject::person("alice"),
                    Scope::FsPath("/var/workspace/private".into()),
                    Access::Write,
                ),
            ],
        });
        let mine = capabilities_for_app("so.tilo.books");
        clear_workspace_capabilities();

        assert_eq!(
            mine.sandbox.len(),
            1,
            "only this app's grant travels: {mine:?}"
        );
        assert_eq!(
            mine.sandbox[0].scope,
            Scope::FsPath("/var/workspace/books".into())
        );
        assert_eq!(mine.sandbox[0].subject, Subject::app("so.tilo.books"));
        assert_eq!(
            mine.authors,
            vec!["so.tilo.books".to_owned()],
            "only this app's authorship travels"
        );
    }

    fn nothing_published_means_nothing_granted() {
        clear_workspace_capabilities();
        assert!(
            capabilities_for_app("so.tilo.books").is_empty(),
            "with no workspace published, an app is granted nothing"
        );
    }

    /// The whole handoff end to end: publish a capability, write the
    /// file a launch would write, then read it back with `tilo-launcher`'s
    /// **own** loader and planner and check the bind that comes out.
    fn a_granted_path_survives_the_handoff_and_becomes_a_read_only_bind() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_RUNTIME_DIR", tmp.path());
        set_workspace_capabilities(sandbox(vec![
            fs_grant("so.tilo.books", "/var/workspace/books", Access::Read),
            fs_grant("so.tilo.other", "/var/workspace/secrets", Access::Write),
        ]));

        let caps = capabilities_for_app("so.tilo.books");
        let path = write_capabilities_file("so.tilo.books", &caps)
            .expect("write capability file")
            .expect("a granted app gets a file");

        // readable only by the user: this file says what one app may reach
        let mode = std::fs::metadata(path.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "capability file must not be world-readable");

        let records = launcher::capabilities::load_capability_file(path.path());
        let plan = launcher::capabilities::CapabilityPlan::for_app(
            "so.tilo.books",
            &records.sandbox,
            Some(tilo_clock::unix_now()),
        );

        clear_workspace_capabilities();
        env.remove("XDG_RUNTIME_DIR");

        assert_eq!(
            plan.ro_paths,
            vec![PathBuf::from("/var/workspace/books")],
            "the granted path arrives at the launcher as a read-only bind: {plan:?}"
        );
        assert!(
            plan.rw_paths.is_empty(),
            "a Read grant must not cross as writable: {plan:?}"
        );
        assert!(
            !plan.allow_network,
            "no network capability was granted: {plan:?}"
        );
    }

    /// Every launch has its own record. A reduced-policy launch cannot
    /// reuse the previous file; the supervisor removes that file on exit.
    fn revoking_the_last_capability_removes_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_RUNTIME_DIR", tmp.path());

        let granted = sandbox(vec![fs_grant(
            "so.tilo.books",
            "/var/workspace/books",
            Access::Read,
        )]);
        let path = write_capabilities_file("so.tilo.books", &granted)
            .unwrap()
            .expect("file written while granted");
        let old_path = path.path().to_path_buf();
        assert!(old_path.exists());

        let after_revoke =
            write_capabilities_file("so.tilo.books", &LaunchGrants::default()).unwrap();
        assert!(
            after_revoke.is_none(),
            "revoked relaunch must have no capability file"
        );
        let other = write_capabilities_file(
            "so.tilo.books",
            &sandbox(vec![fs_grant(
                "so.tilo.books",
                "/another-room",
                Access::Write,
            )]),
        )
        .unwrap()
        .unwrap();
        assert_ne!(old_path, other.path());
        assert_eq!(
            launcher::capabilities::load_capability_file(&old_path),
            granted,
            "a concurrent same-package launch must not overwrite this snapshot"
        );
        assert_eq!(
            launcher::capabilities::load_capability_file(other.path()).sandbox[0].scope,
            Scope::FsPath("/another-room".into())
        );
        drop(path);
        assert!(
            !old_path.exists(),
            "the ended launch's snapshot must be removed"
        );
        let other_path = other.path().to_path_buf();
        assert!(other_path.exists(), "another launch's snapshot must remain");
        drop(other);
        assert!(
            !other_path.exists(),
            "each supervisor removes only its own snapshot"
        );
        env.remove("XDG_RUNTIME_DIR");
    }

    published_capabilities_are_filtered_to_the_launching_app();
    nothing_published_means_nothing_granted();
    a_granted_path_survives_the_handoff_and_becomes_a_read_only_bind();
    revoking_the_last_capability_removes_the_file();
}

#[test]
#[cfg(target_os = "linux")]
#[serial_test::serial(process_env)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn service_authority_is_bound_to_live_exact_pid_identity() {
    fn dbus_proxy_descendant_resolves_only_while_launcher_identity_is_live() {
        let launcher = LauncherStandIn::spawn();
        register_launched_process(launcher.pid(), "so.tilo.mail", Some("room-mail"), true);
        let descendant = launcher.descendant();
        assert_eq!(
            verified_package_for_descendant(descendant).as_deref(),
            Some("so.tilo.mail")
        );
        assert_eq!(
            verified_launch_for_descendant(descendant),
            Some(VerifiedLaunchIdentity {
                package_id: "so.tilo.mail".into(),
                workspace_id: Some("room-mail".into()),
                confined: true,
            })
        );
        let (_, custody) = verified_launch_custody_for_descendant(descendant)
            .expect("the live descendant retains exact launcher custody");
        assert!(launch_custody_is_current(
            custody,
            "so.tilo.mail",
            "room-mail"
        ));
        assert!(!launch_custody_is_current(
            custody,
            "so.tilo.chat",
            "room-mail"
        ));
        unregister_launched_process(launcher.pid());
        assert_eq!(verified_package_for_descendant(descendant), None);
        assert!(
            !launch_custody_is_current(custody, "so.tilo.mail", "room-mail"),
            "the persisted numeric pid/start pair loses authority when its live launch retires"
        );
    }

    fn an_unconfined_launch_is_portal_attributed_without_service_authority() {
        let _ = drain_events();
        let command = LaunchCommand {
            program: PathBuf::from("/bin/sh"),
            args: vec!["-c".into(), "sleep 1".into()],
            env: Vec::new(),
            service_identity: None,
            portal_workspace_id: Some("host-room".into()),
            managed_agent_id: None,
            x11_workspace: None,
        };
        spawn_launch("org.example.Flatpak".into(), "Example".into(), command)
            .expect("launch worker");

        let deadline = tilo_test_env::hang_deadline();
        let launcher_pid = loop {
            let started = drain_events().into_iter().find_map(|event| {
                (event.package_id == "org.example.Flatpak")
                    .then_some(event.kind)
                    .and_then(|kind| match kind {
                        LaunchEventKind::Started { pid } => Some(pid),
                        _ => None,
                    })
            });
            if let Some(pid) = started {
                break pid;
            }
            assert!(
                Instant::now() < deadline,
                "launch never became attributable"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(
            verified_workspaces_for_app("org.example.Flatpak"),
            ["host-room".to_string()]
        );
        assert!(
            consume_trusted_service_launch_for_pid(
                launcher_pid,
                "org.example.Flatpak",
                "host-room"
            )
            .is_none(),
            "portal attribution must not mint service authority"
        );
    }

    fn stale_start_time_is_never_accepted_as_pid_identity() {
        let mut launcher = LauncherStandIn::spawn();
        let pid = launcher.pid();
        LAUNCHED_PROCESSES.lock().unwrap().insert(
            pid,
            LaunchedProcess {
                package_id: "so.tilo.spoof".into(),
                workspace_id: Some("room-spoof".into()),
                start_time: u64::MAX,
                confined: true,
                retiring: false,
            },
        );
        assert_eq!(verified_package_for_descendant(pid), None);
        unregister_launched_process(pid);
        launcher.stop();
    }

    fn compatibility_service_authority_is_exact_and_consumed_once() {
        let launcher = LauncherStandIn::spawn();
        let pid = launcher.pid();
        register_trusted_service_launch(
            pid,
            ServiceLaunchIdentity {
                app_id: "so.tilo.windows".into(),
                workspace_id: "room-1".into(),
                allowed_compat_entry_ids: vec!["windows-reader".into()],
                compat_entry_generation: 7,
                application_creation: None,
            },
        );
        let (_, start_time) = proc_stat(pid).expect("launcher process stat");
        assert_eq!(
            consume_trusted_service_launch(pid, start_time, "so.tilo.agent", "room-1"),
            None,
            "another app id must deny without transferring the record"
        );
        assert_eq!(
            consume_trusted_service_launch(pid, start_time, "so.tilo.windows", "wrong-room"),
            None,
            "a mismatch must deny without transferring the record"
        );
        assert_eq!(
            consume_trusted_service_launch(pid, start_time, "so.tilo.windows", "room-1"),
            Some(vec!["windows-reader".into()])
        );
        assert_eq!(
            consume_trusted_service_launch(pid, start_time, "so.tilo.windows", "room-1"),
            None,
            "the authority record is single-use"
        );
    }

    fn direct_launcher_pid_has_no_service_authority_to_redeem() {
        let launcher = LauncherStandIn::spawn();
        assert_eq!(
            attest_trusted_service_launch_for_pid(launcher.pid(), "so.tilo.agent", "room-1"),
            None,
            "a direct tilo-launcher invocation must not mint the shell's one-shot authority"
        );
    }

    dbus_proxy_descendant_resolves_only_while_launcher_identity_is_live();
    an_unconfined_launch_is_portal_attributed_without_service_authority();
    stale_start_time_is_never_accepted_as_pid_identity();
    compatibility_service_authority_is_exact_and_consumed_once();
    direct_launcher_pid_has_no_service_authority_to_redeem();
}

#[test]
#[cfg(target_os = "linux")]
fn private_window_launch_binding_is_exact_live_and_host_retained() {
    let launcher = LauncherStandIn::spawn();
    let pid = launcher.pid();
    let identity = ServiceLaunchIdentity {
        app_id: "so.tilo.code".into(),
        workspace_id: "private-window-room".into(),
        allowed_compat_entry_ids: Vec::new(),
        compat_entry_generation: 0,
        application_creation: None,
    };
    register_trusted_window_launch(pid, identity, Some(42), Some(9));
    assert!(authenticated_window_launch(pid, "so.tilo.notes", "private-window-room").is_none());
    assert!(authenticated_window_launch(pid, "so.tilo.code", "other-room").is_none());
    assert!(
        authenticated_window_launch(launcher.descendant(), "so.tilo.code", "private-window-room")
            .is_none()
    );
    let bound = authenticated_window_launch(pid, "so.tilo.code", "private-window-room")
        .expect("original exact launcher record");
    assert_eq!(bound.continuation_instance, Some(42));
    assert_eq!(bound.launch_ticket, Some(9), "the launch's own ticket");
    // Kernel start time is part of the retained record, not app-reported identity.
    lock(&TRUSTED_SERVICE_LAUNCHES)
        .get_mut(&pid)
        .unwrap()
        .start_time = u64::MAX;
    assert!(authenticated_window_launch(pid, "so.tilo.code", "private-window-room").is_none());
    lock(&TRUSTED_SERVICE_LAUNCHES).remove(&pid);
    assert!(authenticated_window_launch(pid, "so.tilo.code", "private-window-room").is_none());
}

#[test]
#[cfg(target_os = "linux")]
fn in_process_private_window_binding_is_single_use_and_exact() {
    let app = format!("so.tilo.code.{}", uuid::Uuid::new_v4());
    let room = uuid::Uuid::new_v4().to_string();
    register_in_process_window(&app, &room, Some(42));
    assert!(authenticated_window_launch(std::process::id(), &app, "other-room").is_none());
    assert!(authenticated_window_launch(u32::MAX, &app, &room).is_none());
    let bound = authenticated_window_launch(std::process::id(), &app, &room).unwrap();
    assert_eq!(bound.continuation_instance, Some(42));
    assert!(authenticated_window_launch(std::process::id(), &app, &room).is_none());
    register_in_process_launch(&app, &room);
    assert_eq!(
        authenticated_window_launch(std::process::id(), &app, &room)
            .unwrap()
            .continuation_instance,
        None
    );
}

/// A live process standing in for a started `tilo-launcher`, with one
/// descendant of its own. The pid registries are keyed by process and
/// resolve a process through its ancestors, so each test registers a
/// process of its own rather than the test runner, which every test in
/// the process shares and every stand-in descends from.
#[cfg(target_os = "linux")]
struct LauncherStandIn {
    child: std::process::Child,
    descendant: u32,
}

#[cfg(target_os = "linux")]
impl LauncherStandIn {
    fn spawn() -> Self {
        use std::io::BufRead as _;

        // The descendant lives until the stand-in's stdin closes.
        let mut child = std::process::Command::new("sh")
            .args(["-c", "sleep 30 & echo $!; read -r _; kill $!"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn a launcher stand-in");
        let mut line = String::new();
        std::io::BufReader::new(child.stdout.take().expect("stand-in stdout"))
            .read_line(&mut line)
            .expect("read the descendant's pid");
        let descendant = line.trim().parse().expect("descendant pid");
        Self { child, descendant }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn descendant(&self) -> u32 {
        self.descendant
    }

    fn stop(&mut self) {
        drop(self.child.stdin.take());
        let _ = self.child.wait();
    }
}

#[cfg(target_os = "linux")]
impl Drop for LauncherStandIn {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(target_os = "linux")]
fn locked_canonical_writer(canonical: &Path) -> LauncherStandIn {
    use std::io::BufRead as _;
    let mut child = Command::new("flock")
        .arg("--exclusive")
        .arg(canonical)
        .args(["sh", "-c", "echo locked; read -r _"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    std::io::BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert_eq!(
        line.trim(),
        "locked",
        "the old process owns the real kernel gate"
    );
    LauncherStandIn {
        descendant: child.id(),
        child,
    }
}

#[test]
#[cfg(target_os = "linux")]
fn retiring_native_writer_delays_real_replacement_spawn() {
    let directory = tempfile::tempdir().unwrap();
    let canonical = directory.path().join("canonical.lock");
    let replacement = directory.path().join("replacement-opened");
    let package = format!("so.tilo.retirement.{}", uuid::Uuid::new_v4());
    let room = uuid::Uuid::new_v4().to_string();
    let mut writer = locked_canonical_writer(&canonical);
    register_launched_process(writer.pid(), &package, Some(&room), true);
    note_native_writer_retiring(writer.pid());
    let command = LaunchCommand {
        program: PathBuf::from("/bin/sh"),
        args: vec![
            "-c".into(),
            "exec flock --exclusive --nonblock \"$1\" sh -c 'touch \"$1\"' sh \"$2\"".into(),
            "sh".into(),
            canonical.to_str().unwrap().into(),
            replacement.to_str().unwrap().into(),
        ],
        env: Vec::new(),
        service_identity: Some(ServiceLaunchIdentity {
            app_id: package.clone(),
            workspace_id: room,
            allowed_compat_entry_ids: Vec::new(),
            compat_entry_generation: 0,
            application_creation: None,
        }),
        portal_workspace_id: None,
        managed_agent_id: None,
        x11_workspace: None,
    };
    let mut other_room = command.clone();
    other_room.service_identity.as_mut().unwrap().workspace_id = "other-room".into();
    assert!(wait_for_retiring_app(&package, &other_room, std::time::Duration::ZERO).is_ok());
    assert!(wait_for_retiring_app("another-app", &command, std::time::Duration::ZERO).is_ok());
    assert!(
        wait_for_retiring_app(&package, &command, std::time::Duration::from_millis(1)).is_err()
    );
    let session = LaunchSession::current();
    start_launch(
        package.clone(),
        package.clone(),
        command,
        &session,
        None,
        None,
    )
    .unwrap();
    tilo_test_env::wait_until("the replacement to wait on the retiring writer", || {
        lock(&RETIREMENT_WAITS).contains(&package)
    });
    assert!(
        !replacement.exists(),
        "the replacement cannot start while the old writer owns the gate"
    );
    assert!(
        !drain_events()
            .iter()
            .any(|event| event.package_id == package
                && matches!(
                    event.kind,
                    LaunchEventKind::Started { .. }
                        | LaunchEventKind::Exited
                        | LaunchEventKind::Failed { .. }
                        | LaunchEventKind::SpawnFailed { .. }
                ))
    );
    writer.stop();
    unregister_launched_process(writer.pid());
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let events = drain_events();
        assert!(
            !events.iter().any(|event| event.package_id == package
                && matches!(
                    event.kind,
                    LaunchEventKind::Failed { .. } | LaunchEventKind::SpawnFailed { .. }
                )),
            "replacement must not race the canonical gate: {events:?}"
        );
        if events.iter().any(|event| {
            event.package_id == package && matches!(event.kind, LaunchEventKind::Exited)
        }) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "replacement must settle after writer retirement"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(
        replacement.exists(),
        "the replacement acquired the unchanged canonical lock after retirement"
    );
}

/// A retiring launch whose launcher exited but was never unregistered
/// (a descendant still holds its stderr, so its launch worker never gets
/// there) must not hold the app's next open for the whole timeout, nor
/// refuse it, for the rest of the session.
#[test]
#[cfg(target_os = "linux")]
fn retiring_writer_whose_launcher_exited_holds_no_replacement() {
    let package = format!("so.tilo.retirement.{}", uuid::Uuid::new_v4());
    let room = uuid::Uuid::new_v4().to_string();
    let mut launcher = std::process::Command::new("/bin/sleep")
        .arg("infinity")
        .spawn()
        .unwrap();
    let pid = launcher.id();
    register_launched_process(pid, &package, Some(&room), true);
    note_native_writer_retiring(pid);
    let command = LaunchCommand {
        program: PathBuf::from("/bin/true"),
        args: Vec::new(),
        env: Vec::new(),
        service_identity: Some(ServiceLaunchIdentity {
            app_id: package.clone(),
            workspace_id: room,
            allowed_compat_entry_ids: Vec::new(),
            compat_entry_generation: 0,
            application_creation: None,
        }),
        portal_workspace_id: None,
        managed_agent_id: None,
        x11_workspace: None,
    };
    assert!(
        wait_for_retiring_app(&package, &command, std::time::Duration::from_millis(1)).is_err(),
        "a live retiring launcher still holds the replacement"
    );
    // The launcher exits while the replacement waits; nothing unregisters it.
    let reaper = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(100));
        launcher.kill().unwrap();
        launcher.wait().unwrap();
    });
    // Only the launcher's exit ends this wait: running out of time is
    // an error, which fails the assert.
    assert!(
        wait_for_retiring_app(&package, &command, tilo_test_env::hang_bound()).is_ok(),
        "the replacement went once the launcher had exited"
    );
    reaper.join().unwrap();
    assert!(
        !LAUNCHED_PROCESSES.lock().unwrap().contains_key(&pid),
        "the exited launcher's retiring record is gone"
    );
    assert!(
        wait_for_retiring_app(&package, &command, std::time::Duration::ZERO).is_ok(),
        "a later open is not held at all"
    );
}

#[test]
#[serial_test::serial(process_env)]
fn launch_projections_are_atomic_owner_only_and_staged() {
    fn governance_projection_is_atomic_bounded_and_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_RUNTIME_DIR", tmp.path());

        let room = RoomId::new();
        let path = write_trusted_identities_projection(
            room,
            &tilo_user_identity::TrustedIdentityProjectionV2::empty(),
        )
        .expect("stage projection");
        let bytes = std::fs::read(&path).unwrap();
        let decoded: tilo_user_identity::TrustedIdentityProjectionV2 =
            serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            decoded,
            tilo_user_identity::TrustedIdentityProjectionV2::empty()
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert!(
            std::fs::read_dir(path.parent().unwrap())
                .unwrap()
                .all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")),
            "an atomic publish leaves no staging inode"
        );

        // Replacing a complete snapshot keeps the stable path and remains a
        // complete decodable document.
        let again = write_trusted_identities_projection(
            room,
            &tilo_user_identity::TrustedIdentityProjectionV2::empty(),
        )
        .unwrap();
        assert_eq!(again, path);
        serde_json::from_slice::<tilo_user_identity::TrustedIdentityProjectionV2>(
            &std::fs::read(&again).unwrap(),
        )
        .unwrap();

        // A network/governance update publishes immediately; it is not merely
        // cached for the next application launch.
        std::fs::remove_file(&again).unwrap();
        set_trusted_identity_projection(
            room,
            &tilo_user_identity::TrustedIdentityProjectionV2::empty(),
        );
        assert!(again.exists());
    }

    fn files_launch_receives_only_the_staged_projection_path() {
        let tmp = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_RUNTIME_DIR", tmp.path());
        let room = RoomId::new();
        let files = test_app(FILES_PACKAGE_ID);
        let mut command = build_launch_command(
            &files,
            Path::new("/usr/libexec/tilo/tilo-launcher"),
            Path::new("/run/user/1000/wayland-3"),
            room,
            None,
            None,
            None,
            None,
        );
        attach_trusted_identities_projection(&files, room, &mut command).unwrap();
        let position = command
            .args
            .iter()
            .position(|arg| arg == "--trusted-identities")
            .unwrap();
        let projection = PathBuf::from(&command.args[position + 1]);
        assert_eq!(projection.file_name().unwrap(), TRUSTED_IDENTITIES_FILE);
        assert!(projection.starts_with(tmp.path()));

        let other = test_app("so.tilo.contacts");
        let mut other_command = command.clone();
        other_command.args.truncate(position);
        attach_trusted_identities_projection(&other, room, &mut other_command).unwrap();
        assert!(
            !other_command
                .args
                .iter()
                .any(|arg| arg == "--trusted-identities"),
            "the member directory is scoped to Files"
        );
    }

    /// Peer syncs publish the projection and synchronous launches stage
    /// the capability file from the compositor's loop. Both live in the
    /// runtime tmpfs and must never wait for the disk there.
    #[cfg(target_os = "linux")]
    fn runtime_files_never_wait_for_the_disk() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_RUNTIME_DIR", tmp.path());
        let room = RoomId::new();
        let grants = sandbox(vec![fs_grant(
            "so.tilo.hello",
            "/var/workspace/books",
            Access::Read,
        )]);
        let (projection, capabilities) =
            crate::shell::disk_sync_guard::on_a_thread_that_cannot_sync(|| {
                let projection = write_trusted_identities_projection(
                    room,
                    &tilo_user_identity::TrustedIdentityProjectionV2::empty(),
                )
                .expect("staging the projection needs no disk sync");
                let capabilities = write_capabilities_file("so.tilo.hello", &grants)
                    .expect("staging the capabilities needs no disk sync")
                    .expect("grants need a file");
                (projection, capabilities)
            });
        assert_eq!(
            serde_json::from_slice::<tilo_user_identity::TrustedIdentityProjectionV2>(
                &std::fs::read(&projection).unwrap(),
            )
            .unwrap(),
            tilo_user_identity::TrustedIdentityProjectionV2::empty()
        );
        assert_eq!(
            std::fs::metadata(&projection).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::read(capabilities.path()).unwrap(),
            serde_json::to_vec(&grants).unwrap()
        );
        assert_eq!(
            std::fs::metadata(capabilities.path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    governance_projection_is_atomic_bounded_and_owner_only();
    files_launch_receives_only_the_staged_projection_path();
    #[cfg(target_os = "linux")]
    runtime_files_never_wait_for_the_disk();
}
