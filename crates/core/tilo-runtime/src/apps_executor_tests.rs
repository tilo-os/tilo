//! The device's one launcher: the daemon decides and records every launch, and its executor (the
//! daemon itself, or the seat's shell) only starts it.

use super::*;
use runtime_collab::wire::shell::{HostEntry, HostKind, LaunchAsk, LaunchOrder, OrderCommand};
use tilo_app_host::shell::ShellLauncher as _;

/// A pidfd pinning `child`, as the seat's shell hands the daemon one for a process it started.
fn pinned(child: &std::process::Child) -> runtime_portal::descriptors::Owned {
    use std::os::fd::FromRawFd as _;
    // SAFETY: pidfd_open takes a pid and flags and returns a new descriptor or -1; the child is
    // not reaped, so its pid still names it.
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, child.id(), 0) };
    let fd = i32::try_from(raw)
        .ok()
        .filter(|fd| *fd >= 0)
        .expect("pidfd_open");
    // SAFETY: pidfd_open returned a new descriptor nothing else owns.
    runtime_portal::descriptors::Owned::from(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) })
}

/// The orders waiting for the seat's shell now, without waiting for one.
fn orders_now(launcher: &DeviceLauncher) -> Vec<LaunchOrder> {
    tilo_rt::app().handle().block_on(async {
        tokio::time::timeout(Duration::from_millis(50), launcher.orders())
            .await
            .unwrap_or_default()
    })
}

fn desktop(id: &str) -> HostEntry {
    HostEntry::Desktop {
        id: id.to_owned(),
        files: Vec::new(),
        urls: Vec::new(),
        new_window: false,
    }
}

// The daemon decides and records every launch before a process of it exists, and a connection is
// admitted by the record alone. The person's own launches are started by their shell, by the
// order the daemon answers. One the daemon makes for an app or an agent that shows a window is
// ordered of the seat's shell where the daemon runs as the device's own account, and started by
// the daemon itself where it runs as the person. Either way the launch is the daemon's: a refused
// one starts nothing, a package is never started as the machine's own program, and only the
// process the shell reported holds the ticket.
tilo_test_env::scenario! {
    fn the_daemon_decides_every_launch_and_its_executor_only_starts_it;

    phase a_window_is_ordered_of_the_seat_and_admitted_once_its_process_runs;
    {
        let (_temp, root) = canonical_temp();
        let mut store = Store::new(&root);
        let installed =
            store.install(VIEWER, &manifest(VIEWER, "gui", ""), &[("probe", b"binary")]);
        let apps = Arc::new(
            apps(&store, &root, vec![root.join("missing/tilo-launcher")]).ordering_of_the_seat(),
        );
        let launcher = DeviceLauncher(Arc::clone(&apps));
        let mut ask = LaunchAsk::package(VIEWER, WORKSPACE);
        ask.window = Some(7);
        let ticket = apps.launch_asked(&ask).unwrap();
        assert_eq!(apps.tickets.kind(ticket), Some(executor::Kind::Package));
        assert!(
            apps.launched.0.lock().unwrap().is_empty(),
            "the daemon starts no window process itself"
        );
        assert_eq!(launcher.admitted(ticket), None, "no process holds the launch yet");
        let orders = orders_now(&launcher);
        let [order] = orders.as_slice() else {
            panic!("one order for the one launch: {orders:?}");
        };
        assert_eq!(
            (order.ticket, order.app.as_str(), order.window),
            (ticket, VIEWER, Some(7))
        );
        let OrderCommand::Package(command) = &order.command else {
            panic!("a package's order is a package's command: {order:?}");
        };
        assert_eq!(command.manifest, installed.manifest_path);
        assert_eq!(command.payload_root, installed.payload_root);
        assert_eq!(command.package_hash, installed.package_id_hex);

        let mut child = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        launcher.started(ticket, child.id(), pinned(&child)).unwrap();
        let admitted = launcher
            .admitted(ticket)
            .expect("the started launch is admitted");
        assert_eq!(
            (admitted.app.as_str(), admitted.workspace.as_str(), admitted.window),
            (VIEWER, WORKSPACE, Some(7))
        );
        let again = launcher
            .started(ticket, child.id(), pinned(&child))
            .unwrap_err();
        assert!(again.contains("started already"), "{again}");
        let unknown = launcher
            .started(ticket + 100, child.id(), pinned(&child))
            .unwrap_err();
        assert!(unknown.contains("was ordered"), "{unknown}");

        child.kill().unwrap();
        child.wait().unwrap();
        let ended = std::time::Instant::now();
        while launcher.admitted(ticket).is_some() {
            assert!(
                ended.elapsed() < Duration::from_secs(5),
                "an ended launch is forgotten"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    phase a_window_is_started_here_by_a_daemon_of_the_persons_own_account;
    {
        let (_temp, root) = canonical_temp();
        let mut store = Store::new(&root);
        store.install(VIEWER, &manifest(VIEWER, "gui", ""), &[("probe", b"binary")]);
        let display = root.join("wayland-0");
        std::fs::write(&display, b"").unwrap();
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set("WAYLAND_DISPLAY", &display);
        let stand_in = tilo_test_env::stand_in(root.join("tilo-launcher"), "sleep 30\n").unwrap();
        let apps = Arc::new(apps(&store, &root, vec![stand_in.path().to_owned()]));
        let launcher = DeviceLauncher(Arc::clone(&apps));
        let ticket = apps.launch_asked(&LaunchAsk::package(VIEWER, WORKSPACE)).unwrap();
        assert!(orders_now(&launcher).is_empty(), "nothing is ordered of a seat");
        let pids: Vec<u32> = apps.launched.0.lock().unwrap().keys().copied().collect();
        let [pid] = pids.as_slice() else {
            panic!("one process for the one launch: {pids:?}");
        };
        assert_eq!(apps.launched.ticket(*pid), Some(ticket));
        assert_eq!(
            launcher.admitted(ticket).map(|launch| launch.app),
            Some(VIEWER.to_owned())
        );
        let _ = std::process::Command::new("kill")
            .arg(pid.to_string())
            .status();
    }

    phase a_refused_launch_never_starts_a_process;
    {
        let (_temp, root) = canonical_temp();
        let mut store = Store::new(&root);
        store.install(VIEWER, &manifest(VIEWER, "gui", ""), &[("probe", b"binary")]);
        store.install(
            "so.tilo.tool",
            &manifest("so.tilo.tool", "cli", ""),
            &[("probe", b"binary")],
        );
        // A launcher that would leave a mark had anything started it.
        let started = root.join("started");
        let stand_in = tilo_test_env::stand_in(
            root.join("tilo-launcher"),
            &format!("touch {}\n", started.display()),
        )
        .unwrap();
        for seat in [false, true] {
            let apps = apps(&store, &root, vec![stand_in.path().to_owned()]);
            let apps = Arc::new(if seat { apps.ordering_of_the_seat() } else { apps });
            let launcher = DeviceLauncher(Arc::clone(&apps));
            for (ask, why) in [
                (LaunchAsk::package("so.tilo.unknown", WORKSPACE), "not installed"),
                (LaunchAsk::package("so.tilo.tool", WORKSPACE), "no window"),
                (
                    LaunchAsk::package(VIEWER, "4f1d0f7a-9d0e-4c43-8d3b-2f6f7b8f3a21"),
                    "not served",
                ),
            ] {
                let error = launcher.launch(ask.clone()).unwrap_err();
                assert!(error.contains(why), "{error}");
                let error = apps.launch_asked(&ask).unwrap_err();
                assert!(error.contains(why), "{error}");
            }
            assert!(
                orders_now(&launcher).is_empty(),
                "a refused launch is ordered of no one"
            );
            assert!(apps.launched.0.lock().unwrap().is_empty());
        }
        assert!(!started.exists(), "a refused launch starts no process");
    }

    phase a_package_is_never_launched_as_the_machines_own_program;
    {
        let (_temp, root) = canonical_temp();
        let mut store = Store::new(&root);
        store.install(VIEWER, &manifest(VIEWER, "gui", ""), &[("probe", b"binary")]);
        let apps = Arc::new(apps(&store, &root, Vec::new()).ordering_of_the_seat());
        let launcher = DeviceLauncher(Arc::clone(&apps));
        let error = launcher
            .launch(LaunchAsk::host(HostKind::System, desktop(VIEWER), WORKSPACE))
            .unwrap_err();
        assert!(error.contains("starts only in its sandbox"), "{error}");
        // Nor under another name than the entry it starts.
        let mut renamed = LaunchAsk::host(HostKind::System, desktop("org.gnome.Calculator"), "");
        renamed.app = VIEWER.to_owned();
        assert!(launcher.launch(renamed).is_err());
        // An agent starts no program that runs unconfined as the person.
        let mut agents = LaunchAsk::host(HostKind::System, desktop("org.gnome.Calculator"), "");
        agents.member = Some("agent".to_owned());
        let error = launcher.launch(agents).unwrap_err();
        assert!(error.contains("only they start it"), "{error}");
        assert!(orders_now(&launcher).is_empty());

        // The machine's own app the person asks for is answered as the entry their shell starts.
        let order = launcher
            .launch(LaunchAsk::host(
                HostKind::System,
                desktop("org.gnome.Calculator"),
                "",
            ))
            .unwrap();
        assert_eq!(
            apps.tickets.kind(order.ticket),
            Some(executor::Kind::Host(HostKind::System))
        );
        assert!(
            matches!(
                order.command,
                OrderCommand::Host {
                    kind: HostKind::System,
                    entry: HostEntry::Desktop { .. },
                }
            ),
            "{order:?}"
        );
        assert!(orders_now(&launcher).is_empty(), "the shell's own ask is answered, not queued");
    }

    phase the_persons_own_launch_is_started_by_their_shell_in_either_shape;
    {
        let (_temp, root) = canonical_temp();
        let mut store = Store::new(&root);
        let installed =
            store.install(VIEWER, &manifest(VIEWER, "gui", ""), &[("probe", b"binary")]);
        for seat in [false, true] {
            let apps = apps(&store, &root, vec![root.join("missing/tilo-launcher")]);
            let apps = Arc::new(if seat { apps.ordering_of_the_seat() } else { apps });
            let launcher = DeviceLauncher(Arc::clone(&apps));
            let order = launcher.launch(LaunchAsk::package(VIEWER, WORKSPACE)).unwrap();
            let OrderCommand::Package(command) = &order.command else {
                panic!("a package's order is a package's command: {order:?}");
            };
            assert_eq!(command.package_hash, installed.package_id_hex);
            assert!(apps.launched.0.lock().unwrap().is_empty(), "the daemon started nothing");
            assert_eq!(launcher.admitted(order.ticket), None);
        }
    }

    // Where the person's shell follows the orders, a window the daemon decides for another caller
    // is the shell's to start even on a daemon of the person's own account: the shell holds the
    // display and the person's grants, so the window starts with the same grants either way.
    phase a_window_goes_to_the_shell_that_follows_the_orders;
    {
        let (_temp, root) = canonical_temp();
        let mut store = Store::new(&root);
        store.install(VIEWER, &manifest(VIEWER, "gui", ""), &[("probe", b"binary")]);
        let apps = Arc::new(apps(&store, &root, vec![root.join("missing/tilo-launcher")]));
        let launcher = DeviceLauncher(Arc::clone(&apps));
        assert!(orders_now(&launcher).is_empty(), "the shell asks, and nothing waits yet");
        let ticket = apps.launch_asked(&LaunchAsk::package(VIEWER, WORKSPACE)).unwrap();
        assert!(apps.launched.0.lock().unwrap().is_empty(), "the daemon started nothing");
        let orders = orders_now(&launcher);
        assert_eq!(
            orders.iter().map(|order| order.ticket).collect::<Vec<_>>(),
            vec![ticket]
        );
    }

    // What the shell sees become of a launch it started is kept on the daemon's record of it, and
    // its end forgets the launch; a process the shell holds already (a kept app it took back, a
    // window it draws itself) is decided as a launch and bound to it once.
    phase a_shells_launch_is_followed_to_its_end_and_a_held_process_is_adopted;
    {
        use runtime_collab::wire::shell::{Adopt, LaunchEnd, LaunchEvent, LaunchKind, MemoryTier};
        let (_temp, root) = canonical_temp();
        let mut store = Store::new(&root);
        store.install(VIEWER, &manifest(VIEWER, "gui", ""), &[("probe", b"binary")]);
        let apps = Arc::new(
            apps(&store, &root, vec![root.join("missing/tilo-launcher")]).ordering_of_the_seat(),
        );
        let launcher = DeviceLauncher(Arc::clone(&apps));
        let mut child = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let adopt = Adopt {
            app: VIEWER.to_owned(),
            workspace: WORKSPACE.to_owned(),
            kind: LaunchKind::Package,
            window: Some(3),
            windowless: false,
            pid: child.id(),
            process: 0,
        };
        let ticket = launcher.adopt(adopt.clone(), pinned(&child)).unwrap();
        assert_eq!(
            launcher.admitted(ticket).map(|launch| launch.window),
            Some(Some(3)),
            "the adopted process holds its launch"
        );
        assert_eq!(
            launcher.adopt(adopt.clone(), pinned(&child)),
            Ok(ticket),
            "a process adopted again keeps its launch"
        );
        let mut unknown = adopt;
        unknown.app = "so.tilo.unknown".to_owned();
        assert!(launcher.adopt(unknown, pinned(&child)).is_err());

        let before = launcher
            .event(
                ticket + 100,
                LaunchEvent::Phase {
                    phase: "window".to_owned(),
                    after_ms: 1,
                },
            )
            .unwrap_err();
        assert!(before.contains("no launch"), "{before}");
        for event in [
            LaunchEvent::Phase {
                phase: "window".to_owned(),
                after_ms: 42,
            },
            LaunchEvent::Window { pid: 77 },
            LaunchEvent::Window { pid: 77 },
            LaunchEvent::Tier(MemoryTier::Background),
        ] {
            launcher.event(ticket, event).unwrap();
        }
        assert_eq!(
            apps.tickets.seen_of(ticket),
            Some(executor::Seen {
                phases: vec![("window".to_owned(), 42)],
                windows: vec![77],
                tier: Some(MemoryTier::Background),
            })
        );
        launcher
            .event(
                ticket,
                LaunchEvent::Ended(LaunchEnd {
                    code: Some(1),
                    signal: None,
                    memory: None,
                    stderr: vec!["it broke".to_owned()],
                    asked_to_end: false,
                }),
            )
            .unwrap();
        assert_eq!(launcher.admitted(ticket), None, "an ended launch is forgotten");
        child.kill().unwrap();
        child.wait().unwrap();

        // An order the shell never started ends all the same.
        let mut ask = LaunchAsk::package(VIEWER, WORKSPACE);
        ask.window = Some(4);
        let never = apps.launch_asked(&ask).unwrap();
        assert!(
            launcher
                .event(never, LaunchEvent::Window { pid: 1 })
                .is_err(),
            "nothing but its end is seen of an order not started"
        );
        launcher
            .event(
                never,
                LaunchEvent::Ended(LaunchEnd {
                    code: None,
                    signal: None,
                    memory: None,
                    stderr: vec!["its window showed already".to_owned()],
                    asked_to_end: false,
                }),
            )
            .unwrap();
        assert_eq!(apps.tickets.kind(never), None);
    }
}
