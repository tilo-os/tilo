//! What a keystroke costs a shell with three apps open.
//!
//! Two of those windows have nothing of their own going on while a person
//! types in the third. The frame the key asks for reaches every one of them,
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

use crate::typing_desktop::Demo;

/// Keys typed before anything is counted: the first ones find the caches a
/// typist's second key does, and a window's first frames are its arrival.
const WARMUP: usize = 8;

/// Keys measured.
const KEYS: usize = 20;

#[test]
fn a_key_on_a_shell_of_three_builds_what_each_window_binds_once() {
    let mut shell = Demo::open();
    assert_eq!(shell.apps_open(), 3, "the demo's three apps are open");
    shell.rest();
    shell.focus_the_editor();
    let before = shell.typed_heads();
    for _ in 0..WARMUP {
        shell.type_key("l");
    }
    let costs: Vec<crate::typing_desktop::KeyCost> = (0..KEYS)
        .map(|index| shell.measure_key(if index % 10 == 9 { " " } else { "l" }))
        .collect();
    assert_ne!(
        shell.typed_heads(),
        before,
        "the keys went into the document: it holds what was typed"
    );
    let total =
        |pick: fn(&crate::typing_desktop::KeyCost) -> usize| costs.iter().map(pick).sum::<usize>();
    let passes = total(|cost| cost.passes);
    let declarations = total(|cost| cost.declarations);
    let syncs = total(|cost| cost.syncs);
    let mut cpu: Vec<u64> = costs.iter().map(|cost| cost.cpu_us).collect();
    cpu.sort_unstable();
    let rebuilds = total(|cost| cost.rebuilds);
    // Said on every run: what a key costs the thread that runs it is the
    // number a person feels.
    println!(
        "typing into the document, three apps open: cpu per key p50 {:.2} ms, max {:.2} ms, \
         {:.2} window syncs, {:.2} action passes, {:.2} declaration builds and \
         {:.2} layout rebuilds per key",
        cpu[cpu.len() / 2] as f64 / 1000.0,
        cpu.last().copied().unwrap_or(0) as f64 / 1000.0,
        syncs as f64 / costs.len() as f64,
        passes as f64 / costs.len() as f64,
        declarations as f64 / costs.len() as f64,
        rebuilds as f64 / costs.len() as f64,
    );
    // Each pass builds them once, and the document's publication of what the
    // key changed once more; a window of the other two would add its own.
    assert!(
        declarations <= passes + costs.len(),
        "{declarations} declaration builds over {passes} passes and {} keys: a pass \
         that reads its bound actions reads them once, however many rows the window \
         shows",
        costs.len()
    );
    // The document syncs for the key; a window of the other two syncing would
    // be a second.
    assert!(
        syncs <= costs.len(),
        "{syncs} window syncs over {} keys: only the window typed into syncs",
        costs.len()
    );
    // The window writes the key into its record, and the shell changes what
    // shows it in place: no frame of a key rebuilds its tree.
    assert_eq!(
        rebuilds,
        0,
        "{rebuilds} layout tree rebuilds over {} keys",
        costs.len()
    );
}

#[test]
fn an_idle_shell_syncs_no_window_and_a_change_from_elsewhere_syncs_the_one_it_reached() {
    let mut shell = Demo::open();
    shell.rest();
    assert_eq!(
        shell.syncs_during(Demo::idle),
        0,
        "nothing reached any window: none syncs"
    );
    // Another member's edit reaches the sheets' record.
    shell.change_elsewhere("sheets");
    let reached = shell.syncs_during(|shell| {
        shell.idle();
        shell.idle();
    });
    assert!(
        (1..=2).contains(&reached),
        "the sheets take up the change to their record, and only the sheets \
         ({reached} syncs)"
    );
    assert_eq!(
        shell.syncs_during(Demo::idle),
        0,
        "once it showed the change, the sheets rest again"
    );
}
