//! One workspace, one poker table, two members' devices: a seat either member takes or leaves on
//! their laptop is taken or freed in the window open on the other member's, while it stays open.
//!
//! Both devices are real sessions, joined to one workspace a site serves, each running the
//! installed Poker, a default app written in tilolang: the table is the workspace's record,
//! which each app process reads and writes through its device's store and keeps no copy of.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures: a failed step is the assertion, and the devices live until their \
              sessions end"
)]

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::common::people::{Device, POKER, show, two_people_at_a_site_carrying};

/// The seats around the table.
const SEATS: usize = 6;
/// What an open seat's button says to someone not seated yet.
const SIT_HERE: &str = "Sit here";

/// How many open seats `device`'s Poker window offers to sit at, as the window describes its
/// controls.
fn open_seats(device: &Device) -> usize {
    let answer = device.ask(&json!({"op": "list_targets"}));
    answer["targets"]
        .as_array()
        .unwrap_or_else(|| panic!("{} lists its controls: {answer}", device.label))
        .iter()
        .filter(|target| {
            target["name"]
                .as_str()
                .is_some_and(|name| name.contains(SIT_HERE))
        })
        .count()
}

/// Wait until `device`'s window offers exactly `wanted` open seats.
#[track_caller]
fn wait_for_open_seats(device: &Device, wanted: usize) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let open = open_seats(device);
        if open == wanted {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{}'s table never offered {wanted} open seats; it offers {open}\n{}",
            device.label,
            device.diagnostic()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn a_seat_taken_on_either_device_is_taken_in_the_others_open_window() {
    let (site, _ada, _bob, first, second) =
        two_people_at_a_site_carrying(&[], super::common::people::Device::install_poker);
    show(&first, &site);
    show(&second, &site);
    // Both devices read the workspace they joined as their person: a refused read is why the
    // table its app draws never fills in, and either device's window offers no seat.
    for device in [&first, &second] {
        device.wait_reading(&site.workspace);
    }
    for device in [&first, &second] {
        device.open(POKER);
        device.wait_for_window(POKER);
    }

    // Nobody sits at the table yet: both windows offer every seat.
    wait_for_open_seats(&first, SEATS);
    wait_for_open_seats(&second, SEATS);

    // The first member takes the first seat, through the action its Sit here button invokes.
    let sat: Value = first.app_act(POKER, "sit", &json!({"seat": 1}));
    assert!(
        sat.get("err").is_none(),
        "{} takes a seat: {sat}\n{}",
        first.label,
        first.diagnostic()
    );
    // Seated, they are offered no other seat.
    wait_for_open_seats(&first, 0);

    // The other's window, open all along, no longer offers that seat: the change reached their
    // device's store, and their app read it from there into what it shows.
    wait_for_open_seats(&second, SEATS - 1);

    // The first member gets up again: the seat is open in both windows.
    let left = first.app_act(POKER, "leave", &json!({}));
    assert!(
        left.get("err").is_none(),
        "{} leaves the table: {left}\n{}",
        first.label,
        first.diagnostic()
    );
    wait_for_open_seats(&first, SEATS);
    wait_for_open_seats(&second, SEATS);

    // And the other way: the second member sits, and the first's open window no longer offers
    // their seat.
    let sat = second.app_act(POKER, "sit", &json!({"seat": 2}));
    assert!(
        sat.get("err").is_none(),
        "{} takes a seat: {sat}\n{}",
        second.label,
        second.diagnostic()
    );
    wait_for_open_seats(&second, 0);
    wait_for_open_seats(&first, SEATS - 1);
}
