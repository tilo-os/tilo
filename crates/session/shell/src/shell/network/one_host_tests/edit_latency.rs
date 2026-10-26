//! A note edit reaches the other person's device through the room's host
//! within the interactive budget: under 50 ms at p95, keystroke by
//! keystroke, once the room is open on both devices.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tilo_user_identity::DeviceCapabilities;

use super::{Device, Person, Wakes, hosted_workspace, scratch, until};

/// Keystrokes measured, each sent once the one before it arrived.
const SAMPLES: usize = 100;

/// Wall-clock time depends on the machine, so this runs only when asked for
/// (`--ignored`), with the runner to itself (.config/nextest.toml).
#[test]
#[serial_test::serial]
#[ignore = "measurement; run with --ignored --nocapture"]
fn a_note_edit_reaches_the_other_device_within_the_interactive_budget() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let dir = scratch();
    let wakes = Arc::new(Wakes::default());
    let (ada, grace) = (Person::new(141), Person::new(142));
    let p = Device::start(
        &ada,
        "Ada's laptop",
        &dir.path().join("ada"),
        0x0A8D,
        DeviceCapabilities::primary(),
        &wakes,
    );
    let q = Device::start(
        &grace,
        "Grace's laptop",
        &dir.path().join("grace"),
        0x0B8E,
        DeviceCapabilities::primary(),
        &wakes,
    );
    let (local, room) = hosted_workspace(&p, &[&q], "Drafts");
    p.open_window(room, "Notes");
    p.open_notes_first(room);
    until(&[&p], "the note's record on Ada's device", || {
        p.holds_note(room).then_some(())
    });
    q.join(&p.invite(local, "Grace", Some(grace.user())))
        .unwrap();
    until(&[&p, &q], "Grace to hold the workspace", || {
        q.holds(room).then_some(())
    });

    // The first edit may open the sessions and fetch the record's keys:
    // interactive latency is the steady path after that, so it is left out
    // of the samples.
    let arrives = |text: &str| {
        until(&[&p, &q], "the edit on Grace's device", || {
            (q.note(room).as_deref() == Some(text)).then_some(())
        });
    };
    p.write_note(room, "warm");
    arrives("warm");

    let mut latencies: Vec<Duration> = (0..SAMPLES)
        .map(|keystroke| {
            let text = format!("edit {keystroke}");
            let started = Instant::now();
            p.write_note(room, &text);
            arrives(&text);
            started.elapsed()
        })
        .collect();
    latencies.sort_unstable();
    let p95 = latencies[latencies.len().saturating_mul(95) / 100];
    eprintln!("note edit, Ada's write -> Grace's read through Ada's host: p95 {p95:?}");
    assert!(
        p95 < Duration::from_millis(50),
        "a note edit must reach the other device in under 50 ms at p95, got {p95:?}; \
         samples={latencies:?}"
    );
    drop((p, q));
}
