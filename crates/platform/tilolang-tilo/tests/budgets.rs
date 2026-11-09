//! The runtime's budgets (`tilolang_tilo::bench`), held by the
//! bench's own scenarios. A tilolang app that misses one is a runtime bug.
//!
//! The gate holds the work each scenario does, in counts that do not depend on the machine or
//! its load ([`Work`]), as docs/measurements/performance.md has pull requests gate: what a
//! window shows, and what a change re-renders, patches and reads, is the same in a workspace of 10k
//! items as in one of 1k. Opening over the index a workspace kept reads no more of it at 10k than at
//! 1k: the pages of the rows it shows, and nothing of the workspace's record.
//!
//! The budgets in time and memory depend on the machine and what else runs on it, so no test
//! asserts them: the bench checks and records them on the machine they are for,
//! `cargo run --release -p tilolang-tilo --example bench -- apps/tl/<app> --sizes 1000,10000
//! --budgets`.
//!
//! Its own test target: the allocation counts are those of this work alone (they are counted per
//! thread, so the apps' tests may run side by side). The runs do the work the gate counts and
//! nothing that is only timed ([`BenchConfig::timings`]).

#![allow(clippy::panic, reason = "test assertions")]

use std::path::PathBuf;
use std::time::Duration;

use tilolang_tilo::bench::{BenchConfig, CountingAlloc, EngineWorkloads, Env, Record, Work, run};

/// Counted, so the records hold the allocations each scenario made.
#[global_allocator]
static ALLOCATOR: CountingAlloc = CountingAlloc;

/// The sizes the gate compares: a scenario does at 10k what it does at 1k.
const SMALL: usize = 1000;
const LARGE: usize = 10_000;

/// The scenarios of a workspace that change it after it opened.
const CHANGES: &[&str] = &[
    "keystroke",
    "press",
    "peer_edit",
    "peer_post",
    "scroll",
    "thread_keystroke",
    "thread_post",
    "thread_send",
    "thread_scroll",
];

fn apps() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl")
}

fn bench(app: &str) -> Vec<Record> {
    bench_at(app, vec![SMALL, LARGE], 3)
}

/// The bench of `app` over workspaces of `sizes` items, `samples` changes each.
fn bench_at(app: &str, sizes: Vec<usize>, samples: usize) -> Vec<Record> {
    let config = BenchConfig {
        package: apps().join(app),
        sizes,
        // The first change apart (see `Work::first_of_record`), the rest steady ones.
        samples,
        idle: Duration::ZERO,
        // The engine the suite runs on (`TILOLANG_ENGINE`).
        engine: tilolang_runtime::configured(),
        workloads: EngineWorkloads::default(),
        profile: false,
        memory: None,
        open_only: false,
        budgets: false,
        timings: false,
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
/// references it builds, the patches it sends, what it reads of the record one by one, the
/// index's directories, pages and fields it reads, and the records publication looked at.
fn shown(work: Work) -> [(&'static str, u64); 10] {
    [
        ("renders", work.renders),
        ("items", work.items),
        ("patches", work.patches),
        ("located", work.located),
        ("fields", work.fields),
        ("listings", work.listings),
        ("dirs", work.dirs),
        ("pages", work.pages),
        ("index_fields", work.index_fields),
        ("published", work.published),
    ]
}

/// What would read a collection's items as a whole: its entries walked in the record, and the
/// items a filter no column answers is evaluated on.
fn loaded(work: Work) -> [(&'static str, u64); 2] {
    [("entries", work.entries), ("evaluated", work.evaluated)]
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

/// How many more, or fewer, of `name` a frame may read at one size than at the other: a page
/// holds hundreds of entries where a frame shows tens, so whether the first row it shows starts a
/// page decides whether its window spans one page or two, and two sizes put the same window at
/// different offsets in a page. Only pages read so: what a frame reads of the workspace grows with
/// its items, and every entry of it is still read once.
fn slack(name: &str) -> u64 {
    u64::from(name == "pages")
}

/// What `scenario` shows and changes is the same at 10k as at 1k, count for count.
fn same_shown(app: &str, label: &str, (small, large): (Work, Work), misses: &mut Vec<String>) {
    for ((name, at_large), (_, at_small)) in shown(large).into_iter().zip(shown(small)) {
        if at_large.abs_diff(at_small) > slack(name) {
            misses.push(format!(
                "{app} {label}: {name} {at_large} at 10k, {at_small} at 1k"
            ));
        }
    }
}

fn gate(app: &str, records: &[Record], misses: &mut Vec<String>) {
    // Opening over the workspace's index shows the same at 10k as at 1k and reads the same: of the
    // index, the directories and pages of what it shows; of the record, nothing that grows.
    for scenario in ["open", "thread_open"] {
        let Some((small, large)) = both(records, scenario, Work::of_record) else {
            continue;
        };
        same_shown(app, scenario, (small, large), misses);
        for ((name, at_large), (_, at_small)) in loaded(large).into_iter().zip(loaded(small)) {
            if at_large != at_small {
                misses.push(format!(
                    "{app} {scenario}: {name} {at_large} at 10k, {at_small} at 1k: opening reads the workspace"
                ));
            }
        }
    }
    // Choosing a filter: its query's first page reads the same at 10k as at 1k, from the
    // index's columns; no item is checked one by one.
    if let Some((small, large)) = both(records, "filter", Work::of_record) {
        same_shown(app, "filter", (small, large), misses);
        for ((name, at_large), (_, at_small)) in loaded(large).into_iter().zip(loaded(small)) {
            if at_large != at_small {
                misses.push(format!(
                    "{app} filter: {name} {at_large} at 10k, {at_small} at 1k: a filter reads the workspace"
                ));
            }
        }
    } else if matches!(app, "todo" | "notes") {
        misses.push(format!("{app}: no filter was chosen"));
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
        // allocations it makes do not grow with the workspace.
        if let Some((small, large)) = both(records, scenario, Work::of_record) {
            same_shown(app, scenario, (small, large), misses);
            for ((name, at_large), (_, at_small)) in loaded(large).into_iter().zip(loaded(small)) {
                if at_large != at_small {
                    misses.push(format!(
                        "{app} {scenario}: {name} {at_large} at 10k, {at_small} at 1k: a change loads the workspace again"
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
        if matches!(*scenario, "scroll" | "thread_scroll") {
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

/// The runtime does the work of what `app` shows, whatever the size of its workspace.
fn the_runtime_does_the_work_of_what_it_shows(app: &str) {
    let mut misses = Vec::new();
    gate(app, &bench(app), &mut misses);
    assert!(
        misses.is_empty(),
        "more work in a larger workspace:\n{}",
        misses.join("\n")
    );
}

#[test]
fn notes_does_the_work_of_what_it_shows() {
    the_runtime_does_the_work_of_what_it_shows("notes");
}

/// Typing in a notebook runs the views that read what was typed, and no others: a key costs
/// the same from the first note to ten thousand.
#[test]
fn a_key_reruns_the_same_views_whatever_the_notebook_holds() {
    let records = bench_at("notes", vec![1, 200, LARGE], 2);
    let work = |size: &str| {
        let record = find(&records, "keystroke", size)
            .unwrap_or_else(|| panic!("the keystroke at {size} counts work"));
        Work::of_record(record).unwrap_or_else(|| panic!("the keystroke at {size} counts work"))
    };
    let (one, some, many) = (work("1"), work("200"), work("10k"));
    let mut misses = Vec::new();
    let at = |work: Work| shown(work);
    for ((name, many), (_, some), (_, one)) in at(many)
        .into_iter()
        .zip(at(some))
        .zip(at(one))
        .map(|((a, b), c)| (a, b, c))
    {
        // The views' work, and what they read. Index reads are the store deriving a change
        // into its columns, held below from 200 up: an only note is a shape of its own.
        if name == "index_fields" {
            if some != many {
                misses.push(format!("{name}: {some} at 200 notes, {many} at 10k"));
            }
            continue;
        }
        // A list's row reads the item above it to know what it continues, and the only note
        // has no row above it: one item fewer at 1 note, and the same from 200 up.
        if name == "items" {
            if some != many || one > some {
                misses.push(format!(
                    "{name}: {one} at 1 note, {some} at 200, {many} at 10k"
                ));
            }
            continue;
        }
        if one != some || one != many {
            misses.push(format!(
                "{name}: {one} at 1 note, {some} at 200, {many} at 10k"
            ));
        }
    }
    assert!(
        misses.is_empty(),
        "a key does more work in a larger notebook:\n{}",
        misses.join("\n")
    );
}

#[test]
fn tasks_does_the_work_of_what_it_shows() {
    the_runtime_does_the_work_of_what_it_shows("tasks");
}

#[test]
fn today_does_the_work_of_what_it_shows() {
    the_runtime_does_the_work_of_what_it_shows("today");
}

#[test]
fn chat_does_the_work_of_what_it_shows() {
    the_runtime_does_the_work_of_what_it_shows("chat");
}

// Mail's root holds the workspace's accounts, and what people read is the conversations in
// one: the harness reaches them ([`Shape`]'s holders), so the gate holds them too.
#[test]
fn mail_does_the_work_of_what_it_shows() {
    the_runtime_does_the_work_of_what_it_shows("mail");
}
