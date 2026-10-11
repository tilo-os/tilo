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
//! What it deliberately does not cover: an XWayland client (it needs the
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
    let step = 56.0;
    let mut y = 0.0;
    let mut leftwards = false;
    while y < h {
        let mut x = if leftwards { w - 1.0 } else { 0.0 };
        loop {
            desktop.pointer_move(x.clamp(0.0, w - 1.0), y.clamp(0.0, h - 1.0));
            x += if leftwards { -step } else { step };
            if !(0.0..w).contains(&x) {
                break;
            }
        }
        leftwards = !leftwards;
        y += step;
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

fn window_count(desktop: &Desktop) -> usize {
    desktop.windows().len()
}

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
    for (client, rect) in clients.iter().zip(&rects) {
        let (cx, cy) = centre(*rect);
        desktop.click_at(cx, cy);
        // Keys go to whoever holds keyboard focus when they arrive: wait for
        // the click to have given it to this client.
        desktop.wait_for("the client to hold keyboard focus", || {
            client.count("EVENT:KEYBOARD ENTER") > client.count("EVENT:KEYBOARD LEAVE")
        });
        let before = client.count("EVENT:KEY PRESS");
        desktop.type_text("smoke");
        let wanted = before.saturating_add(5);
        desktop.wait_for_with(
            "the client to receive its keys",
            || format!("the client reported:\n{}", client.output()),
            || client.count("EVENT:KEY PRESS") >= wanted,
        );
    }
    // ... and the in-process apps take theirs.
    let (w, h) = (f64::from(SCREEN.0), f64::from(SCREEN.1));
    for x in [w * 0.1, w * 0.3, w * 0.5] {
        desktop.click_at(x, h * 0.5);
        desktop.type_text("hello");
        desktop.key("Enter");
    }

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
    desktop.invoke_action("actions", "workspace.next");
    sweep_edges(desktop);
    desktop.invoke_action("actions", "workspace.previous");

    // Lock, move the pointer over the lock screen, unlock.
    desktop.invoke_action("session", "lock");
    desktop.wait_for("the session to lock", || {
        desktop.chrome()["session_locked"].as_bool() == Some(true)
    });
    sweep_edges(desktop);
    desktop.type_text("not-the-password");
    desktop.key("Enter");
    desktop.type_text(UNLOCK_PASSWORD);
    desktop.key("Enter");
    desktop.wait_for("the session to unlock", || {
        desktop.chrome()["session_locked"].as_bool() == Some(false)
    });

    // The clients' windows are still there and the desktop still answers.
    sweep_edges(desktop);
    assert_eq!(painted_clients(desktop).len(), 2);
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
}

fn run(desktop: &Desktop) {
    scenario(desktop);
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
    run(&desktop);
}

#[test]
fn desktop_smoke_under_cpu_and_memory_pressure() {
    let desktop = Desktop::start(SCREEN, &smoke_tree());
    let _pressure = Pressure::for_this_machine();
    run(&desktop);
}
