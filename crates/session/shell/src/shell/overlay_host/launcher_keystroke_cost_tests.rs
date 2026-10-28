//! What one keystroke in search costs the compositor thread, end to end:
//! the key through the overlay, the query through every production search
//! provider (applications, actions, settings, windows, files over a home of
//! a few thousand names, people, answers), the rows into the palette, its
//! layout and its paint. Typing is every prefix of real words, as a person
//! types them; each keystroke's thread CPU time, the cheapest of a few
//! rounds, is the cost.
//!
//! The budget is the optimised build's; a build with debug assertions is
//! held to it times [`UNOPTIMISED_SLOWDOWN`]. CPU time depends on the
//! machine, so that measurement runs on request; every run checks the work
//! typing does instead.

use super::*;
use std::time::Duration;

/// How much slower the unoptimised test profile runs this path than the
/// optimised compositor.
const UNOPTIMISED_SLOWDOWN: u32 = 10;
/// Median and slow-tail (p95) keystroke budgets, optimised: well inside one
/// frame at 90 Hz (11 ms) and the 20 ms key-to-photon bar, which also has to
/// fit the GPU's render and the display's scanout.
const MEDIAN_BUDGET: Duration = Duration::from_millis(2);
const P95_BUDGET: Duration = Duration::from_millis(4);
/// Rounds of typing timed after the warm-up round.
const MEASURED_ROUNDS: usize = 3;

fn budget(optimised: Duration) -> Duration {
    if cfg!(debug_assertions) {
        optimised.saturating_mul(UNOPTIMISED_SLOWDOWN)
    } else {
        optimised
    }
}

fn thread_cpu() -> Duration {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `now` is a valid timespec for the call to fill in.
    let status = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &raw mut now) };
    assert_eq!(status, 0, "the thread CPU clock is readable");
    Duration::new(
        u64::try_from(now.tv_sec).unwrap_or(0),
        u32::try_from(now.tv_nsec).unwrap_or(0),
    )
}

/// A home with the names a person keeps: a few thousand files over the
/// user folders, and a source tree beside them.
fn seed_home(home: &std::path::Path) {
    for (folder, stems) in [
        (
            "Documents",
            ["report", "invoice", "notes", "plan", "letter"],
        ),
        (
            "Downloads",
            ["setup", "dataset", "paper", "slides", "photo"],
        ),
        (
            "Pictures",
            ["screenshot", "holiday", "scan", "portrait", "sketch"],
        ),
    ] {
        for (at, stem) in stems.iter().enumerate() {
            let dir = home.join(folder).join(format!("{stem}s-{at}"));
            std::fs::create_dir_all(&dir).unwrap();
            for item in 0..200 {
                std::fs::write(dir.join(format!("{stem}-{item:03}.txt")), b"x").unwrap();
            }
        }
    }
}

/// The words typed, one prefix at a time: an app, a setting, a German word,
/// a file, an action, a typo and a sum.
const WORDS: [&str; 8] = [
    "terminal",
    "bluetooth",
    "einstellungen",
    "invoice",
    "screenshot",
    "dark mode",
    "setings",
    "12*7",
];

/// A palette open and focused over a seeded home, with the file walk and
/// the retained indexes built, as a person finds it on a warm desktop.
fn warm_palette() -> (tilo_test_env::TestHome, AbsorbHarness) {
    let home = tilo_test_env::TestHome::new().unwrap();
    seed_home(home.home());
    let mut h = build_harness_sized(1600.0, 1000.0);
    h.host.set_palette_visible(true);
    h.host.set_palette_focus(true);
    h.host.force_palette_settled();
    // The palette opens on a warm desktop: the file walk and the retained
    // indexes are built before a person types, off this thread.
    tilo_test_env::wait_until("the home's names to be indexed", || {
        h.host.set_palette_query("invoice-1");
        h.host
            .refresh_search_results_for_shots(&mut automerge::AutoCommit::new(), &automerge::ROOT);
        h.host
            .search_results
            .iter()
            .any(|row| row.provider_id == "files")
    });
    h.host.set_palette_query("");
    settle(&mut h, 1600.0, 1000.0);
    (home, h)
}

/// Type every word one keystroke at a time, painting after each, and clear
/// it with Escape; `each` sees every keystroke's thread CPU time.
fn type_every_word(h: &mut AbsorbHarness, mut each: impl FnMut(Duration, &str)) {
    for word in WORDS {
        for scalar in word.chars() {
            let typed = scalar.to_string();
            let started = thread_cpu();
            pump(h, &ui::synthetic_input::char_key(&typed));
            paint(h);
            each(thread_cpu().saturating_sub(started), h.host.palette.query());
        }
        assert_eq!(h.host.palette.query(), word, "typing reaches the field");
        assert!(!h.host.search_results.is_empty(), "{word:?} found nothing");
        pump(h, &ui::synthetic_input::named_key(NamedKey::Escape));
        assert!(h.host.palette.query().is_empty(), "Escape clears the query");
    }
}

/// Typing in search touches neither the workspace's surfaces nor its apps:
/// a warm round of every word builds, resolves and mounts nothing.
#[test]
fn every_keystroke_through_search_layout_and_paint_leaves_the_workspace_alone() {
    let (_home, mut h) = warm_palette();
    // The first round warms what a session has warm.
    type_every_word(&mut h, |_, _| {});
    let before = crate::shell::work_accounting::snapshot();
    let mut keystrokes = 0_usize;
    type_every_word(&mut h, |_, _| keystrokes += 1);
    let after = crate::shell::work_accounting::snapshot();
    assert_eq!(
        keystrokes,
        WORDS.iter().map(|word| word.chars().count()).sum::<usize>()
    );
    assert_eq!(
        (
            after.surface_builds,
            after.tile_resolutions,
            after.app_mounts
        ),
        (
            before.surface_builds,
            before.tile_resolutions,
            before.app_mounts
        ),
        "typing in search rebuilt workspace surfaces or remounted apps"
    );
}

#[test]
#[ignore = "measurement; run with --ignored --nocapture"]
fn every_keystroke_through_search_layout_and_paint_costs_little() {
    let (_home, mut h) = warm_palette();

    // Each keystroke's cost is its cheapest of several rounds. Other work on
    // the machine (a loaded test runner, a busy hyperthread sibling, a
    // throttled clock) only ever adds CPU time to a keystroke, so the
    // minimum is the cost of the code itself, while a real regression
    // shows in every round and still fails the gate.
    let mut samples: Vec<(Duration, String)> = Vec::new();
    let mut round_medians: Vec<Duration> = Vec::new();
    for round in 0..=MEASURED_ROUNDS {
        let mut at = 0;
        let mut this_round: Vec<Duration> = Vec::new();
        type_every_word(&mut h, |spent, query| {
            // The first round warms what a session has warm.
            if round == 1 {
                samples.push((spent, query.to_owned()));
            } else if round > 1 {
                samples[at].0 = samples[at].0.min(spent);
            }
            if round > 0 {
                this_round.push(spent);
            }
            at += 1;
        });
        if round > 0 {
            this_round.sort();
            round_medians.push(this_round[this_round.len() / 2]);
        }
    }
    samples.sort_by_key(|(spent, _)| *spent);
    let median = samples[samples.len() / 2].0;
    let p95 = samples[samples.len() * 95 / 100].0;
    let (worst, worst_query) = samples.last().cloned().unwrap();
    println!(
        "PALETTE-KEYSTROKE samples={} median_us={} p95_us={} max_us={} max_query={worst_query:?} round_medians_us={:?}",
        samples.len(),
        median.as_micros(),
        p95.as_micros(),
        worst.as_micros(),
        round_medians
            .iter()
            .map(Duration::as_micros)
            .collect::<Vec<_>>(),
    );
    assert!(
        median < budget(MEDIAN_BUDGET),
        "a keystroke took {median:?} at the median; the budget is {:?}",
        budget(MEDIAN_BUDGET)
    );
    assert!(
        p95 < budget(P95_BUDGET),
        "a slow keystroke took {p95:?} (p95); the budget is {:?}",
        budget(P95_BUDGET)
    );
}
