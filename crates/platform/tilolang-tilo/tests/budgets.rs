//! The runtime's budgets (docs/designs/app-language/native-apps.md section 4.1), held by the
//! bench's own scenarios in a release build. A tilolang app that misses one is a runtime bug.
//!
//! Its own test target, with one test: the timings and the process's memory are those of this
//! work alone, with no other test running beside it.

#![allow(clippy::panic, clippy::expect_used, reason = "test assertions")]

use std::path::PathBuf;
use std::time::Duration;

use tilolang_runtime::Translation;
use tilolang_tilo::bench::{BenchConfig, Env, Isolation, Record, memory_child, run};

/// Section 4.1, for the runtime's share of a frame. Opening is held to half the section's
/// 300 ms: every app opens 10k items well inside it (DECISIONS.md, "Where it stands").
const OPEN_P95_MS: f64 = 150.0;
const KEYSTROKE_MS: f64 = 2.0;
const PEER_MS: f64 = 2.0;
const SCROLL_P50_MS: f64 = 1.0;
const MEMORY_MIB: f64 = 60.0;

fn bench(package: PathBuf, sizes: Vec<usize>) -> Vec<Record> {
    let config = BenchConfig {
        package,
        sizes,
        samples: 20,
        idle: Duration::from_secs(2),
        translation: Translation::default(),
        profile: false,
        // This test, run again in a process of its own, measures the memory.
        memory: Some(Isolation {
            program: std::env::current_exe().expect("the test binary"),
            args: vec![
                "--exact".into(),
                "the_runtime_meets_its_budgets".into(),
                "--include-ignored".into(),
                "--nocapture".into(),
            ],
        }),
        env: Env {
            commit: "test".into(),
            virtualization: "none".into(),
            runner: "test".into(),
        },
    };
    run(&config, &mut |_| {}).unwrap_or_else(|e| panic!("{}: {e}", config.package.display()))
}

/// The record of `scenario` at the size `size` names (`10k`, `0`).
fn one<'a>(records: &'a [Record], scenario: &str, size: &str) -> &'a Record {
    let prefix = format!("tilolang.{scenario}.");
    let suffix = format!(".{size}");
    records
        .iter()
        .find(|r| r.kpi.starts_with(&prefix) && r.kpi.ends_with(&suffix))
        .unwrap_or_else(|| panic!("no {scenario} record at {size}"))
}

fn at_most(
    records: &[Record],
    (scenario, size): (&str, &str),
    percentile: usize,
    limit: f64,
    misses: &mut Vec<String>,
) {
    let record = one(records, scenario, size);
    let value = record.percentile(percentile);
    if value > limit {
        misses.push(format!(
            "{}: p{percentile} {value:.2} > {limit} {}",
            record.kpi, record.unit
        ));
    }
}

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "the budgets are for release builds: cargo test --release -p tilolang-tilo --test budgets"
)]
fn the_runtime_meets_its_budgets() {
    if let Some(measured) = memory_child() {
        measured.unwrap_or_else(|e| panic!("measuring memory: {e}"));
        return;
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut misses = Vec::new();

    let apps = manifest.join("../../../apps/tl");

    // The apps whose window is a list frame (section 3.2), each over 10k items: the frame
    // budgets at 10k; memory, an app's as the platform measures it, at 1k.
    for app in ["notes", "chat"] {
        let records = bench(apps.join(app), vec![1000, 10_000]);
        if app == "chat" {
            // One conversation of 10k messages, in its window's transcript.
            at_most(
                &records,
                ("thread_open", "10k"),
                95,
                OPEN_P95_MS,
                &mut misses,
            );
            at_most(
                &records,
                ("thread_keystroke", "10k"),
                95,
                KEYSTROKE_MS,
                &mut misses,
            );
            // A peer's message: one row more, whatever the length of the conversation.
            at_most(
                &records,
                ("thread_post", "10k"),
                95,
                PEER_MS,
                &mut misses,
            );
            at_most(
                &records,
                ("thread_scroll", "10k"),
                50,
                SCROLL_P50_MS,
                &mut misses,
            );
        }
        at_most(
            &records,
            ("memory_anon", "1k"),
            100,
            MEMORY_MIB,
            &mut misses,
        );
        at_most(&records, ("open", "10k"), 95, OPEN_P95_MS, &mut misses);
        at_most(
            &records,
            ("keystroke", "10k"),
            95,
            KEYSTROKE_MS,
            &mut misses,
        );
        at_most(&records, ("peer_edit", "10k"), 95, PEER_MS, &mut misses);
        at_most(&records, ("scroll", "10k"), 50, SCROLL_P50_MS, &mut misses);
        at_most(&records, ("idle_wakeups", "10k"), 100, 0.0, &mut misses);
    }

    // Todo, and the board over a notebook, a todo list and a chat of 10k items each.
    let todo = bench(apps.join("todo"), vec![1000, 10_000]);
    at_most(&todo, ("memory_anon", "1k"), 100, MEMORY_MIB, &mut misses);
    at_most(&todo, ("open", "10k"), 95, OPEN_P95_MS, &mut misses);
    at_most(&todo, ("keystroke", "10k"), 95, KEYSTROKE_MS, &mut misses);
    at_most(&todo, ("peer_edit", "10k"), 95, PEER_MS, &mut misses);
    at_most(&todo, ("scroll", "10k"), 50, SCROLL_P50_MS, &mut misses);
    at_most(&todo, ("idle_wakeups", "10k"), 100, 0.0, &mut misses);
    let board = bench(apps.join("board"), vec![1000, 10_000]);
    at_most(&board, ("memory_anon", "1k"), 100, MEMORY_MIB, &mut misses);
    at_most(&board, ("open", "10k"), 95, OPEN_P95_MS, &mut misses);
    at_most(&board, ("keystroke", "10k"), 95, KEYSTROKE_MS, &mut misses);
    at_most(&board, ("peer_edit", "10k"), 95, PEER_MS, &mut misses);
    at_most(&board, ("idle_wakeups", "10k"), 100, 0.0, &mut misses);

    // A counter: a root with no collection, pressed here and bumped by a peer.
    let counter = bench(manifest.join("../../../apps/tl/counter"), vec![0]);
    at_most(&counter, ("memory_anon", "0"), 100, MEMORY_MIB, &mut misses);
    at_most(&counter, ("open", "0"), 95, OPEN_P95_MS, &mut misses);
    at_most(&counter, ("press", "0"), 95, KEYSTROKE_MS, &mut misses);
    at_most(&counter, ("peer_edit", "0"), 95, PEER_MS, &mut misses);
    at_most(&counter, ("idle_wakeups", "0"), 100, 0.0, &mut misses);

    assert!(misses.is_empty(), "over budget:\n{}", misses.join("\n"));
}
