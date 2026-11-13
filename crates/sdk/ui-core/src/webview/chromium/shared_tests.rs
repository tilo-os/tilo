//! One Chromium host per profile, shared by every engine and Browser process
//! that opens it.

use super::tests::{
    PROFILE_FROM_ARGUMENTS, assert_host_spawns, disconnected, helper_fixture_after, host_commands,
    host_spawns, shared_fixture, wait_for_command,
};
use super::*;
use tilo_test_env::SpawnUnderEnvLock as _;

/// A host that records its launch before it reports ready and keeps running after the
/// Browser process that started it is gone, as one flushing its profile does.
#[cfg(unix)]
fn lingering_fixture() -> (tempfile::TempDir, PathBuf) {
    helper_fixture_after(
        &format!(
            "{PROFILE_FROM_ARGUMENTS}\
             trap 'printf exited > \"$profile/exited\"' EXIT\n\
             trap '' PIPE\n\
             echo spawned >> \"$profile/spawns\"\n"
        ),
        "while [ ! -e \"$profile/release-host\" ]; do sleep 0.01; done",
    )
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
                "webview::chromium::shared_tests::shared_owner_child",
                "--ignored",
            ])
            .env("TILO_TEST_SHARED_HOST", &host)
            .env("TILO_TEST_SHARED_PROFILE", &profile)
            .spawn_locked()
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
        assert_eq!(
            host_spawns(&profile),
            2,
            "the dead owner's host recorded its launch, however soon it stopped"
        );
        let next = ChromiumEngine::with_host(&host, &profile).unwrap();
        assert!(next.runtime.link.is_none());
        assert_host_spawns(&profile, 3);
    }
}

/// The profile's lock belongs to its host, not to the Browser process that started it: a
/// process that dies leaves its host shutting down with the profile open, and the next
/// process to open that profile waits for that host to exit rather than starting a second
/// one on the same directory.
#[test]
#[cfg(unix)]
fn a_dead_owners_lingering_host_keeps_the_profile_until_it_exits() {
    let (dir, host) = lingering_fixture();
    let profile = dir.path().join("profile");
    std::fs::create_dir_all(&profile).unwrap();
    // phase: the_owner_process_dies_while_its_host_still_runs
    let mut owner = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "webview::chromium::shared_tests::shared_owner_child",
            "--ignored",
        ])
        .env("TILO_TEST_SHARED_HOST", &host)
        .env("TILO_TEST_SHARED_PROFILE", &profile)
        .spawn_locked()
        .unwrap();
    tilo_test_env::wait_until("the owner process", || profile.join("owner-ready").exists());
    assert_host_spawns(&profile, 1);
    owner.kill().unwrap();
    owner.wait().unwrap();
    // phase: the_next_process_waits_for_the_lingering_host
    let (opened, opener) = std::sync::mpsc::channel();
    let opening = {
        let host = host.clone();
        let profile = profile.clone();
        std::thread::spawn(move || {
            let engine = ChromiumEngine::with_host(&host, &profile).unwrap();
            opened.send(engine.runtime.link.is_none()).unwrap();
        })
    };
    assert!(
        opener.recv_timeout(Duration::from_millis(500)).is_err(),
        "the profile's host was started again while the dead owner's still ran"
    );
    assert!(!profile.join("exited").exists());
    assert_eq!(host_spawns(&profile), 1, "one host on one profile");
    // phase: the_host_exits_and_the_profile_passes_to_its_next_owner
    std::fs::write(profile.join("release-host"), b"").unwrap();
    assert!(
        opener
            .recv_timeout(tilo_test_env::hang_bound())
            .expect("the next process to take the profile")
    );
    opening.join().unwrap();
    assert_host_spawns(&profile, 2);
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
