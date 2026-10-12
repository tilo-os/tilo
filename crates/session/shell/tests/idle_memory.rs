//! What an idle desktop's compositor holds in memory.
//!
//! The production compositor runs headless with a signed-in person's empty
//! desktop, painting as a watched session does, and is left alone until
//! startup work is behind it. Then the compositor reports its own memory by
//! kind of mapping (`process_memory`: `mincore` over its maps, the same
//! report vm-acceptance R01 records on Tilo's kernel, which has no smaps),
//! and this test reads `/proc/<pid>/smaps` beside it for RSS, PSS and dirty
//! pages per mapping. Both are printed; the gate is the compositor's
//! anonymous RSS.
//!
//! ```bash
//! cargo test --locked -p shell --test it idle_memory:: -- --nocapture
//! ```

#![allow(
    clippy::arithmetic_side_effects,
    reason = "sums of a process's KiB counters, far from overflow"
)]

use std::collections::BTreeMap;
use std::time::Instant;

use crate::idle_wakeups::{Desktop, SETTLE, running, wait_until_gone};

/// Most anonymous RSS the idle compositor may hold, in MiB: CI's x86-64
/// runner, headless, lavapipe, debug build.
///
/// Measured 159.7 MiB before startup's freed heap was trimmed (focused
/// run 94053: glibc held 53 MiB free) and 122.6–123.5 MiB after (runs 94099,
/// 94297, 94638). Of the 98 MiB glibc still has in use, ~70 MiB is
/// lavapipe's (`ralloc_size`: the NIR it keeps for every compute pipeline,
/// which a hardware driver does not hold), ~31 MiB the compositor's own
/// Rust heap and ~4 MiB LLVM's. The budget is that plus a margin.
const IDLE_ANON_BUDGET_MIB: u64 = 150;

/// One mapping of `/proc/<pid>/smaps`, the fields this report sums.
#[derive(Debug, Default, Clone)]
struct Smap {
    name: String,
    rss: u64,
    pss: u64,
    private_dirty: u64,
    shared_dirty: u64,
    anonymous: u64,
}

fn smaps(pid: u32) -> Vec<Smap> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/smaps")).unwrap_or_default();
    let mut maps = Vec::new();
    let mut current: Option<Smap> = None;
    for line in text.lines() {
        let mut fields = line.split_ascii_whitespace();
        let Some(first) = fields.next() else { continue };
        if first.contains('-') && !first.ends_with(':') {
            maps.extend(current.take());
            let name = fields.skip(4).collect::<Vec<_>>().join(" ");
            current = Some(Smap {
                name: if name.is_empty() {
                    "[anon]".into()
                } else {
                    name
                },
                ..Smap::default()
            });
            continue;
        }
        let (Some(map), Some(kib)) = (
            current.as_mut(),
            fields.next().and_then(|v| v.parse::<u64>().ok()),
        ) else {
            continue;
        };
        match first {
            "Rss:" => map.rss = kib,
            "Pss:" => map.pss = kib,
            "Private_Dirty:" => map.private_dirty = kib,
            "Shared_Dirty:" => map.shared_dirty = kib,
            "Anonymous:" => map.anonymous = kib,
            _ => {}
        }
    }
    maps.extend(current);
    maps
}

/// Print `/proc/<pid>/smaps` summed per name (the anonymous ones together),
/// largest RSS first.
fn print_smaps(pid: u32) {
    let mut by_name: BTreeMap<String, Smap> = BTreeMap::new();
    let mut total = Smap::default();
    for m in smaps(pid) {
        let entry = by_name.entry(m.name.clone()).or_default();
        for (sum, add) in [
            (&mut entry.rss, m.rss),
            (&mut entry.pss, m.pss),
            (&mut entry.private_dirty, m.private_dirty),
            (&mut entry.shared_dirty, m.shared_dirty),
            (&mut entry.anonymous, m.anonymous),
            (&mut total.rss, m.rss),
            (&mut total.pss, m.pss),
            (&mut total.private_dirty, m.private_dirty),
            (&mut total.shared_dirty, m.shared_dirty),
            (&mut total.anonymous, m.anonymous),
        ] {
            *sum += add;
        }
    }
    let mut rows: Vec<_> = by_name.into_iter().collect();
    rows.sort_by_key(|row| std::cmp::Reverse(row.1.rss));
    eprintln!(
        "smaps of {pid}: rss {} KiB, pss {} KiB, private dirty {} KiB, shared dirty {} KiB, anonymous {} KiB",
        total.rss, total.pss, total.private_dirty, total.shared_dirty, total.anonymous
    );
    eprintln!("  rss_kib  pss_kib  pdirty_kib  anon_kib  name");
    for (name, m) in rows.iter().take(40) {
        eprintln!(
            "  {:>7}  {:>7}  {:>10}  {:>8}  {name}",
            m.rss, m.pss, m.private_dirty, m.anonymous
        );
    }
}

/// Print the compositor's own report: mapping classes, glibc's heap, threads.
fn print_report(report: &serde_json::Value) {
    eprintln!("process_memory status: {}", report["status_kib"]);
    eprintln!("process_memory malloc: {}", report["malloc"]);
    let mut classes: Vec<(&String, &serde_json::Value)> = report["mappings"]["classes"]
        .as_object()
        .map(|c| c.iter().collect())
        .unwrap_or_default();
    classes.sort_by_key(|(_, c)| std::cmp::Reverse(c["resident_kib"].as_u64().unwrap_or(0)));
    eprintln!("  resident_kib  virtual_kib  mappings  class");
    for (name, c) in classes {
        eprintln!(
            "  {:>12}  {:>11}  {:>8}  {name}",
            c["resident_kib"], c["virtual_kib"], c["mappings"]
        );
    }
    eprintln!("top mappings:");
    for m in report["mappings"]["top_mappings"]
        .as_array()
        .into_iter()
        .flatten()
        .take(25)
    {
        eprintln!(
            "  {:>8} KiB  {:<14} {} {}",
            m["resident_kib"], m["class"], m["perms"], m["path"]
        );
    }
    eprintln!("top files: {}", report["mappings"]["top_files"]);
    eprintln!("threads: {}", report["threads"]);
}

#[test]
fn an_idle_empty_desktop_stays_within_its_memory_budget() {
    let started = Instant::now();
    let desktop = Desktop::start_with(&shell::shell::state::WorkspaceTree::bootstrap_default());
    // A person looks at their desktop: frames are rasterised, as on DRM.
    let watched = desktop.control(&serde_json::json!({"op": "watch", "watching": true}));
    assert!(watched.get("err").is_none(), "watch refused: {watched}");
    // GStreamer builds its plugin registry once, in a child, into the fresh
    // XDG cache; settle until a whole settle period passed without one.
    let deadline = tilo_test_env::hang_deadline();
    loop {
        wait_until_gone(&desktop, "gst-plugin-scan");
        std::thread::sleep(SETTLE * 2);
        if !running(&desktop, "gst-plugin-scan") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "gst-plugin-scan kept starting. Log:\n{}",
            desktop.log_tail()
        );
    }
    // The desktop a person leaves: past the trim that returns startup's
    // scratch (`heap_trim::STARTUP_SETTLED` after the compositor started).
    let steady = started + shell::shell::heap_trim::STARTUP_SETTLED + SETTLE;
    std::thread::sleep(steady.saturating_duration_since(Instant::now()));
    let reply = desktop.control(&serde_json::json!({"op": "process_memory"}));
    let report = reply
        .get("answer")
        .unwrap_or_else(|| panic!("process_memory gave no answer: {reply}"));
    print_report(report);
    print_smaps(desktop.pid());
    let anon_kib = report["status_kib"]["RssAnon"]
        .as_u64()
        .unwrap_or_else(|| panic!("the report named no RssAnon: {report}"));
    let resident: u64 = report["mappings"]["classes"].as_object().map_or(0, |c| {
        c.values().filter_map(|v| v["resident_kib"].as_u64()).sum()
    });
    assert!(
        resident > 0,
        "the report counted no resident memory, so the compositor was not observed: {report}"
    );
    assert!(
        anon_kib <= IDLE_ANON_BUDGET_MIB * 1024,
        "the idle compositor holds {} MiB of anonymous memory, budget {IDLE_ANON_BUDGET_MIB} MiB",
        anon_kib / 1024
    );
}
