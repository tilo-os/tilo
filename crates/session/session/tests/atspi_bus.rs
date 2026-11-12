//! What a screen reader reads of a running session, over AT-SPI2.
//!
//! The desktop, the bar and every sheet and panel are painted by the session's
//! own loop, so no application window carries them; the session publishes each
//! one itself. This walks the tree the way Orca does — the accessibility bus's
//! registry, the session's connection on it, the root accessible, its children
//! and their names — finds the bar and the desk by the names their windows
//! carry, and presses the desk's "Open an app" button through
//! `org.a11y.atspi.Action`. The press must open the palette, and the assistive
//! bus must report the focus that follows.
//!
//! It needs a session bus with the AT-SPI registry and `at-spi2-core`'s
//! launcher, so it is ignored by default:
//!
//! ```sh
//! cargo test -p tilo-session --test it atspi_bus:: -- --ignored --nocapture
//! ```
//!
//! The test starts its own session bus, so it never reaches the caller's real
//! desktop; `DISPLAY` and `WAYLAND_DISPLAY` are never read.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              outlives every call made into it"
)]

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_session::{Options, Session};
use tilo_test_env::{PrivateBus, SpawnUnderEnvLock as _};

const REFRESH: Duration = Duration::from_millis(16);
/// The session's AT-SPI application root: every window it publishes hangs under it.
const ROOT_PATH: &str = "/org/a11y/atspi/accessible/root";
/// What the empty desk calls the way into the app palette, as the installed image's external AT-SPI
/// client looks for it.
const OPEN_APP: &str = "Open an app";
/// `AtspiRole::Frame` : a top level window with a title, which is what the accessibility stack
/// turns a window root into. AT-SPI's `window` role is the untitled variant of it.
const ROLE_FRAME: u32 = 23;

/// A child the test ends when it is done with it.
struct Guard(Child);

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Which bus a call is made on: the session's, or the accessibility bus the registry names.
enum Bus<'a> {
    Session,
    Address(&'a str),
}

/// One `gdbus call` , as the acceptance client of the installed image makes it.
fn call(
    bus: &Bus<'_>,
    dest: &str,
    path: &str,
    method: &str,
    args: &[&str],
) -> Result<String, String> {
    let mut command = Command::new("gdbus");
    command.arg("call");
    match bus {
        Bus::Session => {
            command.arg("--session");
        }
        Bus::Address(address) => {
            command.args(["--address", address]);
        }
    }
    command
        .args(["--dest", dest, "--object-path", path, "--method", method])
        .args(args);
    let output = command
        .output_locked()
        .map_err(|error| format!("gdbus {method}: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The `Name` property every accessible answers.
fn name(bus: &Bus<'_>, dest: &str, path: &str) -> Result<String, String> {
    call(
        bus,
        dest,
        path,
        "org.freedesktop.DBus.Properties.Get",
        &["org.a11y.atspi.Accessible", "Name"],
    )
}

/// The paths quoted in a D-Bus reply, as `GetChildren` spells them.
fn paths(reply: &str) -> Vec<String> {
    reply
        .split('\'')
        .skip(1)
        .step_by(2)
        .filter(|path| path.starts_with('/'))
        .map(str::to_owned)
        .collect()
}

/// The whole tree under the root, bounded as a screen reader bounds its walk: every path with the
/// name it says.
fn tree(bus: &Bus<'_>, dest: &str, limit: usize) -> Result<Vec<(String, String)>, String> {
    let mut pending = vec![ROOT_PATH.to_owned()];
    let mut seen = std::collections::HashSet::new();
    let mut found = Vec::new();
    while let Some(path) = pending.pop() {
        if seen.len() >= limit {
            break;
        }
        if !seen.insert(path.clone()) {
            continue;
        }
        let said = name(bus, dest, &path)?;
        let children = call(
            bus,
            dest,
            &path,
            "org.a11y.atspi.Accessible.GetChildren",
            &[],
        )?;
        pending.extend(paths(&children));
        found.push((path, said));
    }
    Ok(found)
}

/// The connection the process `pid` has on the accessibility bus.
fn connection_of(address: &str, pid: u32) -> Option<String> {
    let output = Command::new("busctl")
        .arg(format!("--address={address}"))
        .args(["list", "--no-pager"])
        .output_locked()
        .ok()?;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut fields = line.split_whitespace();
        let (Some(connection), Some(owner)) = (fields.next(), fields.next()) else {
            continue;
        };
        if connection.starts_with(':') && owner.parse::<u32>().ok() == Some(pid) {
            return Some(connection.to_owned());
        }
    }
    None
}

/// Turn assistive technology on, as the bus reports it: the adapter wakes on a change of this,
/// never on the value it starts at.
fn set_enabled(enabled: bool) -> Result<String, String> {
    call(
        &Bus::Session,
        "org.a11y.Bus",
        "/org/a11y/bus",
        "org.freedesktop.DBus.Properties.Set",
        &["org.a11y.Status", "IsEnabled", &format!("<{enabled}>")],
    )
}

/// The address of the accessibility bus the registry answers with.
fn atspi_address() -> Option<String> {
    let reply = call(
        &Bus::Session,
        "org.a11y.Bus",
        "/org/a11y/bus",
        "org.a11y.Bus.GetAddress",
        &[],
    )
    .ok()?;
    reply
        .split('\'')
        .nth(1)
        .filter(|address| !address.is_empty())
        .map(str::to_owned)
}

/// Step the session until `done` holds.
fn until(session: &mut Session, what: &str, mut done: impl FnMut(&mut Session) -> bool) {
    crate::common::wait_for(session, what, |session| done(session).then_some(()));
}

#[test]
#[ignore = "needs at-spi2-core and a session bus with the accessibility registry; see the module docs"]
#[expect(
    clippy::too_many_lines,
    reason = "the walk a screen reader makes, in the order it makes it"
)]
fn a_screen_reader_reads_and_drives_the_session_over_atspi() {
    let _settled = ui::motion::testing::settled();
    let session_bus = PrivateBus::start();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    home.env()
        .set("DBUS_SESSION_BUS_ADDRESS", session_bus.address());
    // The registry keeps its enabled flag in GSettings, and this bus serves no dconf: a memory
    // backend lets the flag take the value a screen reader sets.
    home.env().set("GSETTINGS_BACKEND", "memory");
    let _registry = Guard(
        Command::new("/usr/libexec/at-spi-bus-launcher")
            .arg("--launch-immediately")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn_locked()
            .expect("the accessibility bus needs at-spi2-core's launcher"),
    );
    let address = tilo_test_env::wait_for("the accessibility registry's address", atspi_address);

    let mut session = crate::common::headless(Options {
        collab: false,
        state: Some(home.state().to_path_buf()),
        ..Options::default()
    });
    session.step(REFRESH).expect("a turn of the session");
    // Off, then on: the session's adapters wait for assistive technology to arrive, and arrive they
    // must now that it has. The bus follows that property from the moment its registry knows of a
    // listener, so the session's first windows are made before the change is made.
    std::thread::sleep(Duration::from_millis(500));
    set_enabled(false).expect("the registry takes the change");
    std::thread::sleep(Duration::from_millis(300));
    set_enabled(true).expect("the registry takes the change");

    let bus = Bus::Address(&address);
    let pid = std::process::id();
    until(
        &mut session,
        "the session joins the accessibility bus",
        |_| connection_of(&address, pid).is_some(),
    );
    let dest = connection_of(&address, pid).expect("the session's connection");
    // A client that read a window makes the session build its tree on the loop; a reader waits that
    // long for a desktop, so the walk below may.
    for _ in 0..40 {
        session.step(REFRESH).expect("a turn of the session");
        std::thread::sleep(Duration::from_millis(20));
    }

    let mut tree_now = Vec::new();
    let mut walked = false;
    for _ in 0..4 {
        let nodes = tree(&bus, &dest, 1024).expect("the accessibility bus answers");
        let said = |needle: &str| nodes.iter().any(|(_, name)| name.contains(needle));
        if said("Top bar") && said("Desktop") && said(OPEN_APP) {
            tree_now = nodes;
            walked = true;
            break;
        }
        for _ in 0..20 {
            session.step(REFRESH).expect("a turn of the session");
        }
    }
    assert!(
        walked,
        "the bar, the desk and the desk's way into the palette reach AT-SPI"
    );
    let windowed = |label: &str| {
        let path = tree_now
            .iter()
            .find(|(_, name)| name.contains(&format!("'{label}'")))
            .map_or_else(
                || panic!("no accessible says {label:?}"),
                |(path, _)| path.clone(),
            );
        let role = call(&bus, &dest, &path, "org.a11y.atspi.Accessible.GetRole", &[])
            .expect("the role is answered");
        let role: u32 = role
            .split_whitespace()
            .find_map(|word| word.trim_end_matches([',', ')']).parse().ok())
            .expect("the role is a number");
        assert_eq!(role, ROLE_FRAME, "{label} is a titled window");
    };
    windowed("Top bar");
    windowed("Desktop");

    let button = tree_now
        .iter()
        .find(|(_, name)| name.contains(&format!("'{OPEN_APP}'")))
        .map(|(path, _)| path.clone())
        .expect("the desk says where to open an app");
    let actions = call(
        &bus,
        &dest,
        &button,
        "org.a11y.atspi.Action.GetActions",
        &[],
    )
    .expect("the actions are answered");
    assert!(
        actions.contains("'click'"),
        "the way into the palette a screen reader reads is one it can press: {actions}"
    );

    // What the assistive bus reports while the person presses it: the focus that follows the press
    // is an event, not just a tree the next reader would have to re-read.
    let mut monitor = Guard(
        Command::new("stdbuf")
            .args(["-oL", "dbus-monitor", "--address", &address])
            .arg("type='signal',interface='org.a11y.atspi.Event.Object'")
            .arg("type='signal',interface='org.a11y.atspi.Event.Window'")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn_locked()
            .expect("dbus-monitor runs"),
    );
    let events = Arc::new(Mutex::new(String::new()));
    let sink = Arc::clone(&events);
    let stdout = monitor.0.stdout.take().expect("the monitor's output");
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            sink.lock().expect("the event log").push_str(&line);
            sink.lock().expect("the event log").push('\n');
        }
    });
    // The monitor sees what is emitted after it joined the bus.
    std::thread::sleep(Duration::from_millis(500));

    let pressed = call(
        &bus,
        &dest,
        &button,
        "org.a11y.atspi.Action.DoAction",
        &["0"],
    )
    .expect("the action is answered");
    assert!(pressed.contains("true"), "the press is accepted: {pressed}");
    until(&mut session, "the palette opens", |session| {
        session.handle().search_open()
    });
    for _ in 0..10 {
        session.step(REFRESH).expect("a turn of the session");
    }
    drop(monitor);
    let _ = reader.join();
    let reported = events.lock().expect("the event log").clone();
    eprintln!("atspi: events after the press:\n{reported}");
    let focus_moved = (reported.contains("StateChanged") && reported.contains("focused"))
        || reported.contains("Activate");
    assert!(
        focus_moved,
        "the assistive bus reported the focus that followed the press:\n{reported}"
    );
}
