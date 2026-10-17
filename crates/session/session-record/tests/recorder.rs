//! The flight recorder program as the session runner starts it: a relay
//! between the compositor and its stderr that keeps private, bounded records.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tilo_session_record::recorder::LOG_LIMIT;

const RECORDER: &str = env!("CARGO_BIN_EXE_tilo-flight-recorder");
const LAUNCH: &[u8] = b"\x1b[2m2026-10-11T02:22:31.204118Z\x1b[0m \x1b[32m INFO\x1b[0m \x1b[2mtilo::launch_latency\x1b[0m\x1b[2m:\x1b[0m application launch requested package_id=com.onepassword.OnePassword\n";
const XWAYLAND: &[u8] = b"2026-10-11T02:22:31.981004Z  INFO tilo::xwayland: starting Xwayland bridge for its first client display=:1\n";
const PAINT_STATS: &[u8] = b"paint-stats: 3/14 painted | events=0 revision=0 anim=0 session=0 keepalive=0 | locked=false blanked=true | anim_sources host=0x0000 host_labels=\"\" | 60.000 Hz target\n";
const FRAME_LIFECYCLE: &[u8] =
    b"FRAME-LIFECYCLE serial=183875 event=render-retired queue_wait_us=9 render_us=36917\n";
const APP_STDERR: &[u8] =
    b"[app com.onepassword.OnePassword stderr] INFO  2026-10-11T02:22:33.1+00:00 started\n";

/// Waits for `path` to hold exactly `expected`; bounds only a hang.
fn wait_for(path: &Path, expected: &[u8]) {
    let deadline = tilo_test_env::hang_deadline();
    while Instant::now() < deadline {
        if fs::read(path).is_ok_and(|actual| actual == expected) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!(
        "live flight record was {:?}, expected {:?}",
        fs::read(path).map(|bytes| String::from_utf8_lossy(&bytes).into_owned()),
        String::from_utf8_lossy(expected)
    );
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).expect("metadata").permissions().mode() & 0o777
}

#[test]
fn the_record_persists_while_the_session_runs_and_keeps_the_last_bytes() {
    let directory = tempfile::tempdir().expect("directory");
    let log = directory.path().join("latest.log");
    let mut child = Command::new(RECORDER)
        .args([
            log.to_str().expect("path"),
            "now",
            "42",
            "/usr/bin/tilo-shell",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("recorder");
    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = child.stdout.take().expect("stdout");

    let header = b"tilo-flight-record-v1 start=now pid=42 shell=/usr/bin/tilo-shell\n";
    wait_for(&log, header);
    let first = b"startup failed before one buffered block\n";
    stdin.write_all(first).expect("write");
    stdin.flush().expect("flush");
    wait_for(&log, &[header.as_slice(), first].concat());
    assert_eq!(mode(&log), 0o600);
    assert!(
        child.try_wait().expect("status").is_none(),
        "persisted before end of input"
    );

    // Read the relay on its own thread while writing: a pipe may hold only
    // two pages, so writing first and reading after can deadlock.
    let second: Vec<u8> = (0..LOG_LIMIT + 257)
        .map(|i| u8::try_from(i * 7 % 251).unwrap())
        .collect();
    let reader = std::thread::spawn(move || {
        let mut relayed = Vec::new();
        stdout.read_to_end(&mut relayed).expect("relay");
        relayed
    });
    stdin.write_all(&second).expect("write");
    drop(stdin);
    let relayed = reader.join().expect("reader");
    assert!(child.wait().expect("exit").success());
    assert_eq!(relayed, [first.as_slice(), &second].concat());
    let whole = [header.as_slice(), first, &second].concat();
    assert_eq!(
        fs::read(&log).expect("record"),
        whole[whole.len() - LOG_LIMIT..]
    );
    assert_eq!(mode(&log), 0o600);
}

#[test]
fn the_relay_keeps_running_while_storage_refuses_every_publication() {
    // The compositor's stderr is this relay. If storage failures ended it,
    // the compositor's next log line would hit a closed pipe and abort it.
    let directory = tempfile::tempdir().expect("directory");
    let gone = directory.path().join("removed");
    fs::create_dir(&gone).expect("record directory");
    let log = gone.join("latest.log");
    let mut child = Command::new(RECORDER)
        .args([log.to_str().expect("path"), "now", "1", "/usr/bin/shell"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("recorder");
    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = child.stdout.take().expect("stdout");
    let deadline = tilo_test_env::hang_deadline();
    while !log.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    fs::remove_dir_all(&gone).expect("remove storage");
    let reader = std::thread::spawn(move || {
        let mut relayed = Vec::new();
        stdout.read_to_end(&mut relayed).expect("relay");
        relayed
    });
    let mut sent = Vec::new();
    for line in 0..200 {
        let chunk = format!("log line {line} after storage went away\n");
        stdin.write_all(chunk.as_bytes()).expect("write");
        stdin.flush().expect("flush");
        sent.extend_from_slice(chunk.as_bytes());
        std::thread::sleep(Duration::from_millis(2));
    }
    drop(stdin);
    assert_eq!(reader.join().expect("reader"), sent);
    assert!(child.wait().expect("exit").success());
}

fn run(args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut child = Command::new(RECORDER)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("recorder");
    let mut stdout = child.stdout.take().expect("stdout");
    let reader = std::thread::spawn(move || {
        let mut relayed = Vec::new();
        stdout.read_to_end(&mut relayed).expect("relay");
        relayed
    });
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(input)
        .expect("write");
    let relayed = reader.join().expect("reader");
    assert!(child.wait().expect("exit").success());
    relayed
}

#[test]
fn launch_and_xwayland_lines_outlive_hours_of_measurement_lines() {
    let directory = tempfile::tempdir().expect("directory");
    let log = directory.path().join("latest.log");
    let events = directory.path().join("events.log");
    let measurement = [PAINT_STATS, PAINT_STATS, FRAME_LIFECYCLE, APP_STDERR]
        .concat()
        .repeat(64);
    // More than the full record's bound after the events, as a session that
    // has been up for an afternoon writes.
    let filler = measurement.repeat(LOG_LIMIT / measurement.len() + 2);
    let stream = [LAUNCH, XWAYLAND, &filler].concat();
    let relayed = run(
        &[log.to_str().expect("path"), "now", "42", "/usr/bin/shell"],
        &stream,
    );
    assert_eq!(relayed, stream, "every byte is still relayed");
    let full = fs::read(&log).expect("record");
    assert_eq!(full.len(), LOG_LIMIT);
    assert!(
        !full.windows(14).any(|window| window == b"launch_latency"),
        "the full record lost it"
    );
    let header = b"tilo-flight-record-v1 start=now pid=42 shell=/usr/bin/shell\n";
    assert_eq!(
        fs::read(&events).expect("events"),
        [header.as_slice(), LAUNCH, XWAYLAND].concat()
    );
    assert_eq!(mode(&events), 0o600);
}

#[test]
fn the_event_record_keeps_its_own_bound_and_a_bad_invocation_is_refused() {
    let directory = tempfile::tempdir().expect("directory");
    let log = directory.path().join("latest.log");
    let events = directory.path().join("events.log");
    let count = LOG_LIMIT / XWAYLAND.len() + 100;
    let stream = [XWAYLAND, PAINT_STATS].concat().repeat(count);
    run(&[log.to_str().expect("path")], &stream);
    let kept = fs::read(&events).expect("events");
    assert_eq!(kept.len(), LOG_LIMIT);
    assert!(kept.ends_with(XWAYLAND));
    assert_eq!(fs::metadata(&log).expect("record").len(), LOG_LIMIT as u64);

    let status = Command::new(RECORDER)
        .args(["a", "b"])
        .stdin(Stdio::null())
        .status()
        .expect("recorder");
    assert_eq!(status.code(), Some(2));
}
