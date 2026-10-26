//! The runtime's budgets (docs/designs/app-language/native-apps.md section 4.1), held by the
//! bench's own scenarios. A tilolang app that misses one is a runtime bug.
//!
//! The gate holds the work each scenario does, in counts that do not depend on the machine or
//! its load ([`Work`]), as docs/measurements/performance.md has pull requests gate: what a
//! window shows, and what a change re-renders, patches and reads, is the same in a room of 10k
//! items as in one of 1k. Opening lists each collection it shows once, which grows with its
//! items and nothing else.
//!
//! The budgets in time and memory depend on the machine and what else runs on it, so no test
//! asserts them: the bench checks and records them on the machine they are for,
//! `cargo run --release -p tilolang-tilo --example bench -- apps/tl/<app> --sizes 1000,10000
//! --budgets`.
//!
//! Its own test target: the allocation counts are those of this work alone.

#![allow(clippy::panic, reason = "test assertions")]

use std::path::PathBuf;
use std::time::Duration;

use tilolang_runtime::Translation;
use tilolang_tilo::bench::{BenchConfig, CountingAlloc, Env, Record, Work, run};

/// Counted, so the records hold the allocations each scenario made.
#[global_allocator]
static ALLOCATOR: CountingAlloc = CountingAlloc;

/// The sizes the gate compares: a scenario does at 10k what it does at 1k.
const SMALL: usize = 1000;
const LARGE: usize = 10_000;

/// The scenarios of a room that change it after it opened.
const CHANGES: &[&str] = &[
    "keystroke",
    "press",
    "peer_edit",
    "scroll",
    "thread_keystroke",
    "thread_post",
];

fn apps() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl")
}

fn bench(app: &str) -> Vec<Record> {
    let config = BenchConfig {
        package: apps().join(app),
        sizes: vec![SMALL, LARGE],
        // The first change apart (see `Work::first_of_record`), two steady ones.
        samples: 3,
        idle: Duration::ZERO,
        translation: Translation::default(),
        profile: false,
        memory: None,
        open_only: false,
        budgets: false,
        env: Env {
            commit: "test".into(),
            virtualization: "none".into(),
            runner: "test".into(),
        },
    };
    run(&config, &mut |_| {}).unwrap_or_else(|e| panic!("{app}: {e}"))
}

/// The record of `scenario` at the size `size` names (`10k`).
fn find<'a>(records: &'a [Record], scenario: &str, size: &str) -> Option<&'a Record> {
    let prefix = format!("tilolang.{scenario}.");
    let suffix = format!(".{size}");
    records
        .iter()
        .find(|r| r.kpi.starts_with(&prefix) && r.kpi.ends_with(&suffix))
}

/// The counts of what a scenario shows and changes: the boundaries it renders, the item
/// references it builds, the patches it sends, and what it reads of the store one by one.
fn shown(work: Work) -> [(&'static str, u64); 6] {
    [
        ("renders", work.renders),
        ("items", work.items),
        ("patches", work.patches),
        ("located", work.located),
        ("fields", work.fields),
        ("listings", work.listings),
    ]
}

/// What the store reads of a collection's items as a whole: its entries, and the fields of
/// every item a filter reads, and the items it evaluates.
fn loaded(work: Work) -> [(&'static str, u64); 3] {
    [
        ("entries", work.entries),
        ("scanned", work.scanned),
        ("evaluated", work.evaluated),
    ]
}

/// The work `of` reads off the record of `scenario`, at 1k and at 10k.
fn both(
    records: &[Record],
    scenario: &str,
    of: fn(&Record) -> Option<Work>,
) -> Option<(Work, Work)> {
    let work = |size: &str| {
        let record = find(records, scenario, size)?;
        Some(of(record).unwrap_or_else(|| panic!("{} counts no work", record.kpi)))
    };
    Some((work("1k")?, work("10k")?))
}

/// What `scenario` shows and changes is the same at 10k as at 1k, count for count.
fn same_shown(app: &str, label: &str, (small, large): (Work, Work), misses: &mut Vec<String>) {
    for ((name, at_large), (_, at_small)) in shown(large).into_iter().zip(shown(small)) {
        if at_large != at_small {
            misses.push(format!(
                "{app} {label}: {name} {at_large} at 10k, {at_small} at 1k"
            ));
        }
    }
}

fn gate(app: &str, records: &[Record], misses: &mut Vec<String>) {
    // Opening shows the same at 10k as at 1k, and loads each collection it lists once: every
    // count of the collection's items grows with them (ten times the items, at most ten times
    // the count), and its entries are read once per listing.
    for scenario in ["open", "thread_open"] {
        let Some((small, large)) = both(records, scenario, Work::of_record) else {
            continue;
        };
        same_shown(app, scenario, (small, large), misses);
        let growth = u64::try_from(LARGE / SMALL).unwrap_or(u64::MAX);
        for ((name, at_large), (_, at_small)) in loaded(large).into_iter().zip(loaded(small)) {
            if at_large > at_small.saturating_mul(growth) {
                misses.push(format!(
                    "{app} {scenario}: {name} {at_large} at 10k, {at_small} at 1k: more than once per item"
                ));
            }
        }
        let items = u64::try_from(LARGE).unwrap_or(u64::MAX);
        if large.entries > large.listings.saturating_mul(items) {
            misses.push(format!(
                "{app} {scenario}: {} entries read for {} listings of {LARGE}",
                large.entries, large.listings
            ));
        }
    }
    let nodes = |size: &str| find(records, "open", size).and_then(|r| r.meta.get("nodes"));
    if nodes("1k") != nodes("10k") {
        misses.push(format!(
            "{app} open: {:?} nodes at 10k, {:?} at 1k",
            nodes("10k"),
            nodes("1k")
        ));
    }

    for scenario in CHANGES {
        // A steady change: the same on screen and in the store, it loads nothing, and the
        // allocations it makes do not grow with the room.
        if let Some((small, large)) = both(records, scenario, Work::of_record) {
            same_shown(app, scenario, (small, large), misses);
            for ((name, at_large), (_, at_small)) in loaded(large).into_iter().zip(loaded(small)) {
                if at_large != at_small {
                    misses.push(format!(
                        "{app} {scenario}: {name} {at_large} at 10k, {at_small} at 1k: a change loads the room again"
                    ));
                }
            }
            // Automerge's columns are a few levels deeper in a larger record: a few percent
            // more, never in proportion to the items.
            let limit = small
                .allocations
                .saturating_add(small.allocations / 10)
                .saturating_add(64);
            if large.allocations > limit {
                misses.push(format!(
                    "{app} {scenario}: allocations {} at 10k, {} at 1k",
                    large.allocations, small.allocations
                ));
            }
        }
        // The first change, which brings a writer new to the record: the same on screen and in
        // what the runtime reads. What Automerge does to take a new actor in grows with the
        // record (its allocations are not held here).
        if *scenario == "scroll" {
            // Scrolling writes nothing: it has no first change.
            continue;
        }
        let first = |size: &str| find(records, scenario, size).map(Work::first_of_record);
        match (first("1k"), first("10k")) {
            (Some(Some(small)), Some(Some(large))) => {
                same_shown(app, &format!("first {scenario}"), (small, large), misses);
            }
            (None, None) => {}
            _ => misses.push(format!("{app} {scenario}: the first change counts no work")),
        }
    }
}

#[test]
fn the_runtime_does_the_work_of_what_it_shows() {
    let mut misses = Vec::new();
    for app in ["notes", "chat", "todo", "board"] {
        gate(app, &bench(app), &mut misses);
    }
    assert!(
        misses.is_empty(),
        "more work in a larger room:\n{}",
        misses.join("\n")
    );
}
