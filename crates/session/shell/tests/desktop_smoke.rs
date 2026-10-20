//! Desktop smoke: the whole session, driven the way a person drives it, with
//! the compositor's liveness asserted throughout.
//!
//! A deadlock once shipped to a laptop and froze the desktop a few seconds
//! after login, the moment the pointer left a window. A test that caught it
//! existed, as one slow job among many, and its failure was a timeout like any
//! other. This is the gate that cannot be mistaken for flakiness: it is part of
//! the shell test target every change to the compositor already builds, it
//! takes seconds once built, and a main loop that stops turning fails it
//! within half a second, naming every thread and where it waits
//! (`support::desktop`).
//!
//! The scenario restores a session with several windows (in-process Tilo apps
//! and Wayland clients), then moves the pointer over every window, edge, corner
//! and the top bar, types into each window, resizes, maximizes, opens and closes
//! windows, searches in the palette, switches workspaces, and locks and unlocks.
//! The second test runs it again under CPU and memory pressure, where timing
//! bugs surface far more often.
//!
//! What it deliberately does not cover: an `XWayland` client (it needs the
//! host-desktop session and its satellite), real GPU output and real input
//! devices.

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use shell::shell::state::{DesktopTree, Tab, WindowGroup, WindowWidth, WorkspaceTree};

use crate::support::desktop::{Desktop, Pressure, UNLOCK_PASSWORD};

const SCREEN: (i32, i32) = (1280, 800);

/// The Wayland client that reports pointer enter/leave and key presses.
const CLIENT: &str = env!("CARGO_BIN_EXE_click_test_client");
const CLIENT_APP_ID: &str = "click-test";

/// The demo desktop, restored as three Tilo-app windows side by side (Notes,
/// Files, and Terminal with a Document tab) with two more workspaces to switch
/// to.
fn smoke_tree() -> WorkspaceTree {
    let mut tree = WorkspaceTree::bootstrap_demo();
    // An untouched demo is recognised as sample data and replaced on load; a
    // named workspace is the person's own.
    let workspace = &mut tree.desktop.workspaces[0];
    workspace.name = "Smoke desktop".into();
    let strip = &mut workspace.strips[0];
    let original = strip.window_groups.remove(0);
    let mut tabs = original.tabs.into_iter();
    let (notes, files) = (tabs.next().unwrap(), tabs.next().unwrap());
    let rest: Vec<Tab> = tabs.collect();
    let group = |id: u64, tabs: Vec<Tab>| WindowGroup {
        id,
        active_tab: tabs[0].id,
        tabs,
        width: WindowWidth::Fraction(300),
        maximized: false,
        focus_history: Vec::new(),
    };
    let (first, second, third) = (910_001, 910_002, 910_003);
    strip.window_groups = vec![
        group(first, vec![notes]),
        group(second, vec![files]),
        group(third, rest),
    ];
    let strip_id = strip.id;
    let DesktopTree { active_group, .. } = &mut tree.desktop;
    active_group.insert(strip_id, first);
    tree
}

fn rect_of(frame: &Value) -> Option<[f64; 4]> {
    let rect = frame.get("rect")?;
    Some([
        rect.get("x")?.as_f64()?,
        rect.get("y")?.as_f64()?,
        rect.get("w")?.as_f64()?,
        rect.get("h")?.as_f64()?,
    ])
}

/// Foreign windows that have painted, as screen rectangles.
fn painted_clients(desktop: &Desktop) -> Vec<[f64; 4]> {
    desktop
        .foreign_frames()
        .iter()
        .filter(|frame| frame.get("app_id").and_then(Value::as_str) == Some(CLIENT_APP_ID))
        .filter(|frame| frame.get("presented").and_then(Value::as_bool) == Some(true))
        .filter_map(rect_of)
        .collect()
}

fn centre(rect: [f64; 4]) -> (f64, f64) {
    (rect[0] + rect[2] / 2.0, rect[1] + rect[3] / 2.0)
}

/// Every point of a serpentine lattice over the screen, so the pointer crosses
/// every window, gap, edge and the top bar; then the four corners and the
/// midpoints of the four edges, which a lattice does not land on.
fn sweep(desktop: &Desktop) {
    let (w, h) = (f64::from(SCREEN.0), f64::from(SCREEN.1));
    let columns: Vec<f64> = (0..SCREEN.0).step_by(56).map(f64::from).collect();
    for (row, y) in (0..SCREEN.1).step_by(56).map(f64::from).enumerate() {
        let mut xs = columns.clone();
        if row % 2 == 1 {
            xs.reverse();
        }
        for x in xs {
            desktop.pointer_move(x.clamp(0.0, w - 1.0), y.clamp(0.0, h - 1.0));
        }
    }
    for (x, y) in [
        (0.0, 0.0),
        (w - 1.0, 0.0),
        (w - 1.0, h - 1.0),
        (0.0, h - 1.0),
        (w / 2.0, 0.0),
        (w - 1.0, h / 2.0),
        (w / 2.0, h - 1.0),
        (0.0, h / 2.0),
        // The top bar, end to end.
        (12.0, 14.0),
        (w / 2.0, 14.0),
        (w - 12.0, 14.0),
    ] {
        desktop.pointer_move(x, y);
    }
    close_overlays(desktop);
}

/// Into a window, across it and out of it onto the desktop, repeatedly: the
/// transition that froze the compositor is the pointer *leaving* a client.
fn cross(desktop: &Desktop, rect: [f64; 4]) {
    let [x, y, w, h] = rect;
    let (cx, cy) = centre(rect);
    for _ in 0..3 {
        desktop.pointer_move(x + 6.0, cy);
        desktop.pointer_move(cx, cy);
        desktop.pointer_move(x + w - 6.0, cy);
        desktop.pointer_move(x + w + 12.0, cy);
        desktop.pointer_move(cx, y - 8.0);
        desktop.pointer_move(cx, cy);
        desktop.pointer_move(cx, y + h + 8.0);
    }
}

fn clients_report(clients: &[crate::support::desktop::Client]) -> String {
    clients
        .iter()
        .enumerate()
        .map(|(n, c)| format!("client {n}:\n{}", c.output()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The pointer's visit to a corner or edge opens what lives there (the overview
/// at the top left, quick settings at the top right), and the next click lands
/// on it. A person moves on; so does the scenario, closing whatever the sweep
/// opened before it clicks anything.
#[track_caller]
fn close_overlays(desktop: &Desktop) {
    const OVERLAYS: [&str; 4] = ["overview", "quick_settings", "ws_settings", "collaborators"];
    let open = |desktop: &Desktop| {
        let chrome = desktop.chrome();
        OVERLAYS
            .iter()
            .any(|name| chrome["overlays"][name].as_bool() == Some(true))
    };
    for _ in 0..4 {
        if !open(desktop) {
            return;
        }
        eprintln!(
            "[desktop-smoke] closing overlays at {}: {}",
            std::panic::Location::caller(),
            desktop.chrome()["overlays"]
        );
        desktop.key("Escape");
        desktop.pause(Duration::from_millis(100));
    }
    desktop.wait_for("the corner overlays to close", || !open(desktop));
}

/// A click as a person makes it: on the desktop, not on whatever the pointer's
/// last visit left open.
#[track_caller]
fn click(desktop: &Desktop, x: f64, y: f64) {
    close_overlays(desktop);
    desktop.click_at(x, y);
}

fn window_count(desktop: &Desktop) -> usize {
    desktop.windows().len()
}

#[allow(
    clippy::too_many_lines,
    reason = "the smoke scenario is one ordered walk through the desktop on one compositor"
)]
fn scenario(desktop: &Desktop) {
    // Restored: the tree the compositor was started on is what it shows.
    let restored = desktop.windows();
    let titles: Vec<&str> = restored
        .iter()
        .filter_map(|row| row.get("title").and_then(Value::as_str))
        .collect();
    for expected in ["Notes", "Files", "Terminal", "Document"] {
        assert!(
            titles.contains(&expected),
            "the restored session lost {expected}: {titles:?}"
        );
    }

    // Two Wayland clients, each in a window group of its own.
    let clients: Vec<_> = (0..2)
        .map(|_| {
            desktop.ok(&json!({"op": "place_next_window", "app_id": CLIENT_APP_ID}));
            desktop.spawn_client(Path::new(CLIENT))
        })
        .collect();
    desktop.wait_for("both Wayland clients to paint", || {
        painted_clients(desktop).len() == 2
    });
    desktop.ok(&json!({"op": "set_group_width", "permille": 190}));
    desktop.pause(Duration::from_millis(300));

    // Pointer: the whole screen, then in and out of each client window.
    sweep(desktop);
    let rects = painted_clients(desktop);
    assert_eq!(
        rects.len(),
        2,
        "both clients are still on screen: {rects:?}"
    );
    for rect in &rects {
        cross(desktop, *rect);
    }
    for client in &clients {
        assert!(
            client.count("EVENT:ENTER") >= 2 && client.count("EVENT:LEAVE") >= 2,
            "the client should have seen the pointer enter and leave it; it reported:\n{}",
            client.output()
        );
    }

    // Typing into each window: the Wayland clients see their keys.
    for rect in &rects {
        let (cx, cy) = centre(*rect);
        // Which client is at this rectangle is not known up front: the
        // compositor lists its windows in no order of ours. The click says: the
        // client under the pointer is the one that reports it.
        let clicks: Vec<usize> = clients.iter().map(|c| c.count("EVENT:CLICK")).collect();
        click(desktop, cx, cy);
        let mut hit = None;
        desktop.wait_for_with(
            "a client to report the click",
            || {
                format!(
                    "clicked at {:?}; clients reported:\n{}",
                    (cx, cy),
                    clients_report(&clients)
                )
            },
            || {
                hit = clients
                    .iter()
                    .zip(&clicks)
                    .position(|(c, before)| c.count("EVENT:CLICK") > *before);
                hit.is_some()
            },
        );
        let client = &clients[hit.expect("a client reported the click")];
        // Keys go to whoever holds keyboard focus when they arrive: wait for
        // the client to have been told it holds it.
        desktop.wait_for_with(
            "the clicked client to be told it holds keyboard focus",
            || {
                format!(
                    "{}\nseat focus: {:?}\nclients reported:\n{}",
                    client.fate(),
                    desktop.chrome().get("seat_focus"),
                    clients_report(&clients)
                )
            },
            || client.count("EVENT:KEYBOARD ENTER") > client.count("EVENT:KEYBOARD LEAVE"),
        );
        let before = client.count("EVENT:KEY PRESS");
        desktop.type_text("smoke");
        let wanted = before.saturating_add(5);
        desktop.wait_for_with(
            "the client to receive its keys",
            || {
                format!(
                    "{}\nseat focus: {:?}\nclients reported:\n{}",
                    client.fate(),
                    desktop.chrome().get("seat_focus"),
                    clients_report(&clients)
                )
            },
            || client.count("EVENT:KEY PRESS") >= wanted,
        );
    }
    // ... and the in-process apps take theirs, wherever they are laid out.
    let apps: Vec<[f64; 4]> = desktop
        .targets()
        .into_iter()
        .filter(|(name, rect)| name.starts_with("so.tilo.") && rect[2] > 50.0 && rect[3] > 50.0)
        .map(|(_, rect)| rect)
        .collect();
    assert!(
        !apps.is_empty(),
        "the session has Tilo app windows on screen"
    );
    for rect in apps {
        let (cx, cy) = centre(rect);
        click(desktop, cx, cy);
        desktop.type_text("hello");
        desktop.key("Enter");
    }
    close_overlays(desktop);

    // Resize and maximize.
    for permille in [260, 340, 220] {
        desktop.ok(&json!({"op": "set_group_width", "permille": permille}));
        desktop.pause(Duration::from_millis(150));
        sweep_edges(desktop);
    }

    // Open and close windows.
    let before = window_count(desktop);
    desktop.invoke_action("actions", "window.new.terminal");
    desktop.wait_for("a new Terminal window", || window_count(desktop) > before);
    sweep_edges(desktop);

    // Palette: open, search, close.
    desktop.chord_ctrl("k");
    desktop.pause(Duration::from_millis(200));
    desktop.type_text("notes");
    desktop.pause(Duration::from_millis(400));
    sweep_edges(desktop);
    desktop.key("Escape");

    // Workspaces.
    // Away and back, as a person pressing the keys would.
    desktop.invoke_action("actions", "workspace.next");
    desktop.wait_for_with(
        "the next workspace to show",
        || format!("showing {:?}", desktop.active_workspace()),
        || desktop.active_workspace() != "Smoke desktop",
    );
    sweep_edges(desktop);
    desktop.invoke_action("actions", "workspace.previous");
    desktop.wait_for_with(
        "FINDING: workspace.previous to bring the first workspace back",
        || format!("showing {:?}", desktop.active_workspace()),
        || desktop.active_workspace() == "Smoke desktop",
    );

    // Lock, move the pointer over the lock screen, unlock.
    desktop.invoke_action("session", "lock");
    desktop.wait_for("the session to lock", || {
        desktop.chrome()["session_locked"].as_bool() == Some(true)
    });
    sweep_edges(desktop);
    // (A rejected password is the lock suite's: its verification is
    // asynchronous and has no signal to wait on here.)
    desktop.type_text(UNLOCK_PASSWORD);
    desktop.key("Enter");
    desktop.wait_for("the session to unlock", || {
        desktop.chrome()["session_locked"].as_bool() == Some(false)
    });

    // The clients' windows are still there and the desktop still answers. The
    // tiles are laid out again by the frame after the unlock, so ask for them
    // to be painted rather than reading them in the same instant.
    sweep_edges(desktop);
    // Only the active workspace's windows are listed, so on another workspace
    // the paint wait below could never be met.
    assert_eq!(
        desktop.active_workspace(),
        "Smoke desktop",
        "the session unlocked onto another workspace than the clients'"
    );
    desktop.wait_for_with(
        "both clients to be painted again after the unlock",
        || {
            format!(
                "foreign frames: {:?}\nlocked: {:?}\n{}",
                desktop.foreign_frames(),
                desktop.chrome().get("session_locked"),
                desktop.thread_dump()
            )
        },
        || painted_clients(desktop).len() == 2,
    );
}

/// Most of one core an untouched desktop may use over the idle window.
///
/// The window is read with the liveness monitor holding its pings. Each
/// heartbeat wakes the main loop, and every wake is a whole loop turn
/// whether or not anything changed, so with the monitor asking every 50 ms
/// the window read the probe instead of the desktop: 16.5% with it and
/// 0.0% without on the same CI run, the loop turning 39 times in 2 s for
/// heartbeats and painting nothing. Against the old 25% bound that probe
/// decided the result by how loaded the runner was (25-31% in 31 of the 36
/// runs it failed over two days, the desktop doing nothing).
///
/// Without the probe, nine 2 s windows over both variants on one CI run
/// read 0.0-3.0%, and 6.0% once under the pressure variant. Most of that was
/// the Files app's Nearby snapshot poll, every 2 s, each a loop turn whose
/// pump took 50-160 ms on that runner; Files now asks only when the shell
/// announces a Nearby change, and an idle desktop announces none.
///
/// What the bound must still catch is a loop that turns on its own with
/// nothing to do: the probe's 20 turns a second read 16-31%, so a loop
/// woken half as often for nothing crosses it. The window is 4 s so that
/// one slow turn, or the top bar's minute repaint (up to 135 ms on a loaded
/// runner), cannot carry it over on its own.
const IDLE_SHARE_BOUND: f64 = 0.10;

/// How long the idle window lasts.
const IDLE_WINDOW: Duration = Duration::from_secs(4);

/// Left alone, the desktop is idle. A widget that reports a wake deadline
/// nothing can satisfy makes the loop pump every app on every pass: the Files
/// app did, while its nearby-transfer snapshot went unanswered. Measured
/// after the backlog the scenario left has drained.
fn assert_idle(desktop: &Desktop) {
    desktop.pause(Duration::from_secs(2));
    let idle = desktop.without_heartbeats(|| desktop.idle(IDLE_WINDOW));
    // A loop that froze while the monitor held its pings fails at the first
    // heartbeat after them.
    desktop.pause(crate::support::desktop::HEARTBEAT_BUDGET.saturating_mul(2));
    // Every run logs its reading by thread, so a failure names what woke the
    // loop, and a drift toward the bound shows before a run crosses it.
    eprintln!(
        "[desktop-smoke] idle: {:.1}% of a core (bound {:.0}%)\n{}\n{}",
        idle.share * 100.0,
        IDLE_SHARE_BOUND * 100.0,
        idle.threads,
        idle_log(&idle)
    );
    assert!(
        idle.share < IDLE_SHARE_BOUND,
        "an untouched desktop used {:.1}% of a core (bound {:.0}%); something wakes its loop \
         with nothing to do, or it is spinning on a wake deadline that never clears (the \
         [desktop-smoke] idle reading above names the threads)",
        idle.share * 100.0,
        IDLE_SHARE_BOUND * 100.0,
    );
}

/// What the compositor said in an idle window: what woke its loop and what
/// it painted (`TILO_PAINT_STATS`), and anything it logged.
fn idle_log(idle: &crate::support::desktop::Idle) -> String {
    let lines: Vec<&str> = idle.log.iter().map(String::as_str).take(40).collect();
    format!("  compositor in the window:\n    {}", lines.join("\n    "))
}

/// A shorter pointer pass: the corners, the edges and the top bar.
fn sweep_edges(desktop: &Desktop) {
    let (w, h) = (f64::from(SCREEN.0), f64::from(SCREEN.1));
    for (x, y) in [
        (0.0, 0.0),
        (w / 2.0, 14.0),
        (w - 1.0, 0.0),
        (w - 1.0, h / 2.0),
        (w - 1.0, h - 1.0),
        (w / 2.0, h - 1.0),
        (0.0, h - 1.0),
        (0.0, h / 2.0),
        (w / 2.0, h / 2.0),
    ] {
        desktop.pointer_move(x, y);
    }
    close_overlays(desktop);
}

fn run(desktop: &Desktop, pressure: Option<Pressure>) {
    scenario(desktop);
    // The pressure ends with the scenario: idleness is judged without it.
    drop(pressure);
    assert_idle(desktop);
    let (beats, worst) = desktop.heartbeat_summary();
    eprintln!(
        "[desktop-smoke] {beats} heartbeats acknowledged; the slowest wake took {worst:?} \
         (a stall is anything over {:?})",
        crate::support::desktop::HEARTBEAT_BUDGET
    );
    for slow in desktop.jank() {
        eprintln!("[desktop-smoke] SLOW (not hung): {slow}");
    }
    assert!(beats > 0, "the heartbeat monitor never saw the loop turn");
}

#[test]
fn desktop_smoke_keeps_the_main_loop_turning() {
    let desktop = Desktop::start(SCREEN, &smoke_tree());
    run(&desktop, None);
}

#[test]
fn desktop_smoke_under_cpu_and_memory_pressure() {
    let desktop = Desktop::start(SCREEN, &smoke_tree());
    run(&desktop, Some(Pressure::for_this_machine()));
}

// The compositor serves each Wayland client a bounded number of requests per
// loop pass, so one chatty client cannot hold the loop. The remainder is
// decoded work that no file descriptor reports any more: the loop must come
// back for it by itself.
mod burst {
    use std::time::{Duration, Instant};

    use wayland_client::protocol::{
        wl_callback, wl_compositor::WlCompositor, wl_surface::WlSurface,
    };
    use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};

    use super::{SCREEN, smoke_tree};
    use crate::support::desktop::Desktop;
    use crate::support::wayland::{Globals as _, test_client};

    #[derive(Default)]
    struct Burst {
        globals: Vec<(u32, String, u32)>,
        synced: bool,
    }
    test_client!(Burst: registry);
    delegate_noop!(Burst: ignore WlCompositor);
    delegate_noop!(Burst: ignore WlSurface);

    impl Dispatch<wl_callback::WlCallback, ()> for Burst {
        fn event(
            state: &mut Self,
            _: &wl_callback::WlCallback,
            event: wl_callback::Event,
            (): &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if matches!(event, wl_callback::Event::Done { .. }) {
                state.synced = true;
            }
        }
    }

    /// More requests than one pass serves, in a single write, then silence:
    /// the answer to the `sync` at the end must still arrive.
    #[test]
    fn a_burst_past_the_per_client_bound_is_answered_without_further_input() {
        let mut desktop = Desktop::start(SCREEN, &smoke_tree());
        // Nothing may wake the loop but the burst itself.
        desktop.stop_monitor();
        let conn = Connection::from_socket(desktop.connect_wayland()).expect("connect");
        let mut queue = conn.new_event_queue::<Burst>();
        let qh = queue.handle();
        let registry = conn.display().get_registry(&qh, ());
        let mut client = Burst::default();
        queue.roundtrip(&mut client).expect("learn the globals");
        let (name, version) = client.global("wl_compositor", 4).expect("a compositor");
        let compositor: WlCompositor = registry.bind(name, version, &qh, ());
        let surface = compositor.create_surface(&qh, ());
        queue.roundtrip(&mut client).expect("create the surface");

        // Past the bound several times over; one write, so one socket read
        // decodes more than the bound.
        for n in 0..640_i32 {
            surface.damage(0, 0, 1_i32.saturating_add(n % 7), 1);
        }
        let _sync = conn.display().sync(&qh, ());
        conn.flush().expect("send the burst");

        let deadline = Instant::now()
            .checked_add(Duration::from_secs(10))
            .unwrap_or_else(Instant::now);
        while !client.synced {
            queue.dispatch_pending(&mut client).expect("dispatch");
            if client.synced {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the compositor never answered the sync behind a {}-request burst: it slept \
                 with decoded requests pending\n{}",
                640,
                desktop.thread_dump()
            );
            if let Some(guard) = queue.prepare_read() {
                let mut fd = libc::pollfd {
                    fd: std::os::fd::AsRawFd::as_raw_fd(&guard.connection_fd()),
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: one initialised pollfd, live for the call.
                unsafe { libc::poll(&raw mut fd, 1, 50) };
                let _ = guard.read();
            }
        }
    }
}

// Windows must come back after the session unlocks: a screen that unlocks onto
// blank windows is a desktop the person cannot use. One lock cycle failed to
// bring the clients back about one run in fifteen, so this repeats it.
mod lock_cycle {
    use std::path::Path;
    use std::time::Duration;

    use serde_json::json;

    use super::{CLIENT, CLIENT_APP_ID, SCREEN, UNLOCK_PASSWORD, painted_clients, smoke_tree};
    use crate::support::desktop::Desktop;

    #[test]
    fn clients_are_painted_again_after_every_unlock() {
        let desktop = Desktop::start(SCREEN, &smoke_tree());
        let _clients: Vec<_> = (0..2)
            .map(|_| {
                desktop.ok(&json!({"op": "place_next_window", "app_id": CLIENT_APP_ID}));
                desktop.spawn_client(Path::new(CLIENT))
            })
            .collect();
        desktop.wait_for("both Wayland clients to paint", || {
            painted_clients(&desktop).len() == 2
        });
        desktop.ok(&json!({"op": "set_group_width", "permille": 190}));
        for cycle in 0..25 {
            desktop.invoke_action("session", "lock");
            desktop.wait_for("the session to lock", || {
                desktop.chrome()["session_locked"].as_bool() == Some(true)
            });
            desktop.type_text(UNLOCK_PASSWORD);
            desktop.key("Enter");
            desktop.wait_for("the session to unlock", || {
                desktop.chrome()["session_locked"].as_bool() == Some(false)
            });
            desktop.wait_for_with(
                &format!("both clients to be painted again after unlock {cycle}"),
                || {
                    format!(
                        "foreign frames: {:?}\nstate: {:?}\n{}",
                        desktop.foreign_frames(),
                        desktop.chrome().get("session_locked"),
                        desktop.thread_dump()
                    )
                },
                || painted_clients(&desktop).len() == 2,
            );
            desktop.pause(Duration::from_millis(100));
        }
    }
}

// Switching workspaces away and back must always come back.
mod workspace_cycle {
    use super::{SCREEN, smoke_tree};
    use crate::support::desktop::Desktop;

    #[test]
    fn next_then_previous_returns_to_the_first_workspace_every_time() {
        let desktop = Desktop::start(SCREEN, &smoke_tree());
        // As the scenario does first: open the palette, search, close it.
        desktop.chord_ctrl("k");
        desktop.pause(std::time::Duration::from_millis(200));
        desktop.type_text("notes");
        desktop.pause(std::time::Duration::from_millis(400));
        desktop.key("Escape");
        for cycle in 0..40 {
            desktop.invoke_action("actions", "workspace.next");
            desktop.wait_for_with(
                &format!("the next workspace to show (cycle {cycle})"),
                || format!("showing {:?}", desktop.active_workspace()),
                || desktop.active_workspace() != "Smoke desktop",
            );
            desktop.invoke_action("actions", "workspace.previous");
            desktop.wait_for_with(
                &format!("the first workspace to show again (cycle {cycle})"),
                || format!("showing {:?}", desktop.active_workspace()),
                || desktop.active_workspace() == "Smoke desktop",
            );
        }
    }
}
