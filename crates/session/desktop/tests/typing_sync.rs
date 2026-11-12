//! What a keystroke costs a desk with six apps open.
//!
//! Five of those windows have nothing of their own going on while a person
//! types in the sixth. The frame the key asks for reaches every one of them,
//! and each proves from what its last sync read that nothing it reads moved
//! ([`ui::component_inputs`]): only the window typed into syncs. A window
//! something did reach — a change to its record from elsewhere — syncs again
//! on the frame it arrives.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::cast_precision_loss,
    reason = "test fixtures: a failed lookup is the assertion, and the figures \
              are said in the units a person reads"
)]

mod typing_desktop;

use typing_desktop::DemoDesk;

/// Keys typed before anything is counted: the first ones find the caches a
/// typist's second key does, and a window's first frames are its arrival.
const WARMUP: usize = 8;

/// Keys measured.
const KEYS: usize = 20;

#[test]
fn a_key_on_a_desk_of_six_builds_what_each_window_binds_once() {
    let mut desk = DemoDesk::open();
    assert_eq!(desk.apps_open(), 6, "the demo's six apps are open");
    desk.rest();
    desk.focus_the_note();
    let before = desk.note_heads();
    for _ in 0..WARMUP {
        desk.type_key("l");
    }
    let costs: Vec<typing_desktop::KeyCost> = (0..KEYS)
        .map(|index| desk.measure_key(if index % 10 == 9 { " " } else { "l" }))
        .collect();
    assert_ne!(
        desk.note_heads(),
        before,
        "the keys went into the note: it holds what was typed"
    );
    let total = |pick: fn(&typing_desktop::KeyCost) -> usize| costs.iter().map(pick).sum::<usize>();
    let passes = total(|cost| cost.passes);
    let declarations = total(|cost| cost.declarations);
    let syncs = total(|cost| cost.syncs);
    let mut cpu: Vec<u64> = costs.iter().map(|cost| cost.cpu_us).collect();
    cpu.sort_unstable();
    // Said on every run: what a key costs the thread that runs it is the
    // number a person feels.
    println!(
        "typing into the note, six apps open: cpu per key p50 {:.2} ms, max {:.2} ms, \
         {:.2} window syncs, {:.2} action passes and {:.2} declaration builds per key",
        cpu[cpu.len() / 2] as f64 / 1000.0,
        cpu.last().copied().unwrap_or(0) as f64 / 1000.0,
        syncs as f64 / costs.len() as f64,
        passes as f64 / costs.len() as f64,
        declarations as f64 / costs.len() as f64,
    );
    // Each pass builds them once, and the note's publication of what the
    // key changed once more; a window of the other five would add its own.
    assert!(
        declarations <= passes + costs.len(),
        "{declarations} declaration builds over {passes} passes and {} keys: a pass \
         that reads its bound actions reads them once, however many rows the window \
         shows",
        costs.len()
    );
    // The note syncs for the key; a window of the other five syncing would
    // be a second.
    assert!(
        syncs <= costs.len(),
        "{syncs} window syncs over {} keys: only the window typed into syncs",
        costs.len()
    );
}

#[test]
fn an_idle_desk_syncs_no_window_and_a_change_from_elsewhere_syncs_the_one_it_reached() {
    let mut desk = DemoDesk::open();
    desk.rest();
    assert_eq!(
        desk.syncs_during(DemoDesk::idle),
        0,
        "nothing reached any window: none syncs"
    );
    // Another participant's edit reaches the calendar's record.
    desk.change_elsewhere("calendar");
    let reached = desk.syncs_during(|desk| {
        desk.idle();
        desk.idle();
    });
    assert!(
        (1..=2).contains(&reached),
        "the calendar takes up the change to its record, and only the calendar \
         ({reached} syncs)"
    );
    assert_eq!(
        desk.syncs_during(DemoDesk::idle),
        0,
        "once it showed the change, the calendar rests again"
    );
}
