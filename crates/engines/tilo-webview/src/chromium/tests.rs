use super::*;
use tilo_test_env::SpawnUnderEnvLock as _;

tilo_test_env::scenario! {
    fn the_engine_host_is_started_clean_or_its_absence_is_reported;

    phase missing_helper_is_a_startup_error;
    {
        let root = tempfile::tempdir().unwrap();
        let result = ChromiumEngine::with_host(
            &root.path().join("missing-host"),
            &root.path().join("profile"),
        );
        assert!(result.is_err());
    }

    phase unavailable_engine_reports_failure_without_page_pixels;
    {
        let engine = UnavailableEngine::new("Runtime missing".into());
        engine.send(WebViewCommand::Navigate("https://tilo.so".into()));
        assert!(engine.frames().lock().unwrap().is_none());
        assert!(engine.drain_events().iter().any(|event| matches!(event, WebViewEvent::LoadFailed {uri,error} if uri == "https://tilo.so" && error == "Runtime missing")));
    }

    phase the_engine_host_does_not_inherit_the_app_loader_environment;
    {
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

    phase a_governed_app_routes_page_audio_through_the_host_bridge;
    {
        // The sandbox withholds the audio sockets and devices, so the engine must not try to
        // open any; its sound reaches the app over the host protocol.
        assert_eq!(host_audio_switches(true), ["--tilo-audio-bridge"]);
        assert!(host_audio_switches(false).is_empty());
    }
}

tilo_test_env::scenario! {
    #[cfg(unix)]
    fn a_real_host_process_is_supervised_until_it_is_reaped;

    phase private_helper_gets_incognito_flag_and_owned_root_is_removed_after_exit;
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
        crate::test_support::write_executable(
            &host,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nprintf '{encoded}'\ncat >/dev/null\nprintf done > '{}'\n",
                arguments.display(),
                exited.display()
            ),
        );
        let root = private_root();

        let engine = ChromiumEngine::with_private_host(&host, &root).unwrap();
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

    phase private_startup_failures_remove_owned_roots_after_reaping;
    {
        let missing_root = private_root();
        assert!(
            ChromiumEngine::with_private_host(Path::new("/missing/tilo-host"), &missing_root)
                .is_err()
        );
        assert!(!missing_root.exists());

        let fixture = tempfile::tempdir().unwrap();
        let host = fixture.path().join("not-ready");
        crate::test_support::write_executable(&host, "#!/bin/sh\nexit 7\n");
        // A host that exits before it is ready fails the page once it is reaped; the
        // window never waited for it.
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

    phase private_packaged_helper_discovery_failure_removes_root;
    {
        let fixture = tempfile::tempdir().unwrap();
        let root = private_root();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "chromium::tests::private_packaged_helper_discovery_failure_child",
            ])
            .env_remove(runtime::HOST_OVERRIDE_ENV)
            .env(runtime::RUNTIME_DIR_ENV, fixture.path())
            .env("TILO_TEST_PRIVATE_ROOT", &root)
            .status_locked()
            .unwrap();
        assert!(result.success());
        assert!(!root.exists());
    }

    phase private_helper_crash_removes_root_while_engine_handle_remains;
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

    phase private_process_exit_waits_for_helper_and_root_cleanup;
    {
        let (_fixture, host) = helper_fixture("exec 3>&1; exec cat >/dev/null");
        let root = private_root();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "chromium::tests::private_process_lifecycle_child",
            ])
            .env("TILO_TEST_PRIVATE_HOST", &host)
            .env("TILO_TEST_PRIVATE_ROOT", &root)
            .status_locked()
            .unwrap();
        assert!(result.success());
        assert!(!root.exists(), "root must be absent when the child exits");
    }

    phase helper_eof_is_reported_once_without_engine_drop;
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

    phase malformed_output_terminates_and_reaps_helper;
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

    phase rejected_navigation_is_visible_and_does_not_change_current_uri;
    {
        let (dir, host) = helper_fixture("exec 3>&1; exec cat >/dev/null");
        let engine = ChromiumEngine::with_host(&host, &dir.path().join("profile")).unwrap();
        *engine.shared.uri.lock().unwrap() = "https://old.example".into();
        engine.shared.failed.store(true, Ordering::Release);
        engine.tx.close();
        engine.send(WebViewCommand::Navigate("https://new.example".into()));
        assert_eq!(*engine.shared.uri.lock().unwrap(), "https://old.example");
        assert!(engine.drain_events().iter().any(|event| matches!(event, WebViewEvent::LoadFailed {uri,error} if uri == "https://new.example" && error.contains("not queued"))));
    }

    phase an_engine_is_ready_to_use_before_its_host_is;
    // Opening a Browser window, the first of its profile, starts the engine host, which
    // takes a second or more to say it is ready. The window must not wait for it: the engine
    // is handed back while the host is still starting, and what the page asks for meanwhile
    // reaches the host once it reads.
    {
        let mut ready = Vec::new();
        protocol::write_json(
            &mut ready,
            &json!({"event":"ready","cef_version":"152","chromium_version":"152"}),
        )
        .unwrap();
        let encoded: String = ready.iter().map(|byte| format!("\\{:03o}", byte)).collect();
        let dir = tempfile::tempdir().unwrap();
        let host = dir.path().join("host");
        crate::test_support::write_executable(
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
        // Returns while the host waits: a constructor that waited for `ready` would give up
        // after its 20 s and fail here.
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

/// The isolated helper-discovery failure the test above runs again as its
/// own process; an ordinary run of the suite returns at once.
#[test]
#[cfg(unix)]
fn private_packaged_helper_discovery_failure_child() {
    let Some(root) = std::env::var_os("TILO_TEST_PRIVATE_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
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

/// The private process lifecycle the test above runs again as its own
/// process; an ordinary run of the suite returns at once.
#[test]
#[cfg(unix)]
fn private_process_lifecycle_child() {
    let Some(host) = std::env::var_os("TILO_TEST_PRIVATE_HOST") else {
        return;
    };
    let root = PathBuf::from(std::env::var_os("TILO_TEST_PRIVATE_ROOT").unwrap());
    let engine = ChromiumEngine::with_private_host(Path::new(&host), &root).unwrap();
    assert!(root.exists());
    drop(engine);
}

/// Sets `$profile` in a helper script from its `--profile=` argument, wherever the engine
/// put it: a governed process, as any test process is once one of its tests governs
/// audio, passes its audio switch first.
#[cfg(unix)]
pub(super) const PROFILE_FROM_ARGUMENTS: &str =
    "for argument; do case $argument in --profile=*) profile=${argument#--profile=};; esac; done\n";

#[cfg(unix)]
pub(super) fn helper_fixture(trailing: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    helper_fixture_after("", trailing)
}

/// [`helper_fixture`] for a host that has to do something before it says it is ready:
/// `prologue` runs first, so a host that dies before it reports ready still leaves its
/// mark.
#[cfg(unix)]
pub(super) fn helper_fixture_after(
    prologue: &str,
    trailing: &str,
) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("helper");
    let mut bytes = Vec::new();
    protocol::write_json(
        &mut bytes,
        &json!({"event":"ready","cef_version":"152","chromium_version":"152"}),
    )
    .unwrap();
    let encoded: String = bytes.iter().map(|byte| format!("\\{:03o}", byte)).collect();
    crate::test_support::write_executable(
        &path,
        &format!("#!/bin/sh\n{prologue}printf '{encoded}'\n{trailing}\n"),
    );
    (dir, path)
}

/// A Browser window left open costs the CPU nothing: the engine host's supervisor sleeps
/// until the host exits, fails or is asked to stop, rather than waking on a timer to
/// look.
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
        // This engine's own supervisor: in a test process shared with other engines, a
        // thread found by name may be another's, gone by now.
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
            engine.shared.failure_reported()
        });
        // The output is still held open: only the exit itself can have woken the
        // supervisor.
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

/// A host that records its launch as soon as it runs, records every command it receives
/// in the profile, writes `packets` once the test creates `emit` there, and records that
/// it stopped however it stops.
#[cfg(unix)]
pub(super) fn shared_fixture(packets: &[u8]) -> (tempfile::TempDir, PathBuf) {
    let encoded: String = packets
        .iter()
        .map(|byte| format!("\\{:03o}", byte))
        .collect();
    helper_fixture_after(
        &format!(
            "{PROFILE_FROM_ARGUMENTS}\
             trap 'printf exited > \"$profile/exited\"' EXIT\n\
             trap '' PIPE\n\
             echo spawned >> \"$profile/spawns\"\n"
        ),
        &format!(
            "exec 3<&0\n\
             cat <&3 > \"$profile/commands\" &\n\
             reader=$!\n\
             while [ ! -f \"$profile/emit\" ] && kill -0 $reader 2>/dev/null; do sleep 0.01; done\n\
             if [ -f \"$profile/emit\" ]; then printf '{encoded}'; fi\n\
             wait"
        ),
    )
}

#[cfg(unix)]
pub(super) fn host_spawns(profile: &Path) -> usize {
    std::fs::read_to_string(profile.join("spawns"))
        .unwrap_or_default()
        .lines()
        .count()
}

/// Wait until the hosts started for `profile` have all recorded their start, expecting
/// `expected` of them.
#[cfg(unix)]
pub(super) fn assert_host_spawns(profile: &Path, expected: usize) {
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
pub(super) fn host_commands(profile: &Path) -> Vec<Value> {
    let mut reader =
        std::io::Cursor::new(std::fs::read(profile.join("commands")).unwrap_or_default());
    let mut commands = Vec::new();
    while let Ok(Some(protocol::Packet::Json(command))) = protocol::read_packet(&mut reader) {
        commands.push(command);
    }
    commands
}

#[cfg(unix)]
pub(super) fn wait_for_command(profile: &Path, what: &str, matches: impl Fn(&Value) -> bool) {
    tilo_test_env::wait_until(what, || host_commands(profile).iter().any(&matches));
}

#[cfg(unix)]
pub(super) fn disconnected(events: &[WebViewEvent]) -> bool {
    events.iter().any(|event| {
        matches!(event, WebViewEvent::LoadFailed { error, .. } if error == "Chromium engine disconnected")
    })
}

tilo_test_env::scenario! {
    #[cfg(unix)]
    fn owned_navigation_keeps_its_owner_and_its_order_over_a_real_host;

    phase owned_navigation_real_transport_ignores_bootstrap_and_keeps_redirect_owner;
    {
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

    phase owned_navigation_admission_queue_loss_and_renderer_death_are_observable;
    {
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
        let (_fixture, host) = helper_fixture("exec 3>&1; exec cat >/dev/null");
        // A disposable root of this process's own, marked as one the browser may remove: the
        // owned host needs it, and the engine removes it only after its supervisor reaped the
        // helper.
        let root = std::env::temp_dir().join(format!("tilo-browser-private-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(
            root.join(".tilo-private-root-v1"),
            "tilo-browser-owned-private-root-v1\n",
        )
        .unwrap();
        std::fs::write(root.join(".tilo-private-root.lock"), "private-root-lock\n").unwrap();
        let root = root.canonicalize().unwrap();
        let engine = ChromiumEngine::with_private_host(&host, &root).unwrap();
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
        drop(engine);
        assert!(
            !root.exists(),
            "the refused navigation's helper must be reaped, not left running"
        );
    }

}

/// Turning an installed extension on loads it into the running engine, and off unloads it, over
/// the host's own DevTools pipe: no window restarts to make a change take effect.
#[test]
fn an_extension_turned_on_loads_and_off_unloads_without_a_restart() {
    let queue = Arc::new(CommandQueue::default());
    let shared = Arc::new(Shared::default());
    let root = tempfile::tempdir().expect("a root");
    let signer = crate::extensions::crx::fixtures::Signer::new();
    let id = crate::extensions::crx::extension_id(&signer.spki);
    let manifest: &[u8] =
        br#"{"manifest_version":3,"name":"Reader","version":"1.2.3","permissions":["storage"]}"#;
    let package = bounded_zip::write_zip(
        &[("manifest.json".to_owned(), manifest.to_vec())],
        &bounded_zip::ZipLimits::default(),
    )
    .expect("a package");
    let signed = crate::extensions::crx::fixtures::crx(&signer, &[&signer], &package);
    let extension = crate::extensions::install(&signed, root.path(), None).expect("installed");

    live_extension(&queue, &shared, 1, &extension, true);
    let unload = queue.pop().expect("the old version is unloaded first");
    assert_eq!(unload["op"], "devtools_call");
    assert_eq!(unload["method"], "Extensions.uninstall");
    assert_eq!(unload["params"]["id"], json!(id));
    let access = queue.pop().expect("what it may do is told before it runs");
    assert_eq!(access["op"], "extension_access");
    assert_eq!(access["id"], json!(id));
    assert_eq!(access["permissions"], json!(["storage"]));
    let load = queue.pop().expect("the extension is loaded");
    assert_eq!(load["op"], "devtools_call");
    assert_eq!(load["method"], "Extensions.loadUnpacked");
    assert_eq!(load["tab"], json!(1));
    assert_eq!(
        load["params"]["path"],
        json!(extension.path.to_string_lossy())
    );

    // Turning it off unloads it and loads nothing.
    live_extension(&queue, &shared, 1, &extension, false);
    let off = queue.pop().expect("it is unloaded");
    assert_eq!(off["method"], "Extensions.uninstall");
    assert_eq!(off["params"]["id"], json!(id));
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
