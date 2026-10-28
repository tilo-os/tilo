//! A search index keeps its incremental writer between commits. Its
//! threads must sleep until the next commit, not poll: the idle desktop's
//! wakeup budget is one a second for everything. Its own process, so the
//! only tantivy threads in it are this index's.
#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::time::Duration;

use tilo_search::{Document, FieldWeight, Page, SearchIndex};

/// The kernel keeps 15 bytes of a thread's name: tantivy's indexing thread
/// (`thrd-tantivy-index0`), its segment updater and its merge threads.
fn is_writer_thread(name: &str) -> bool {
    name.starts_with("thrd-tantivy")
        || name == "segment_updater"
        || name.starts_with("merge_thread")
}

/// Whether every writer thread is asleep (`S` in its `stat`), blocked on
/// whatever it waits for between commits.
fn writers_asleep() -> bool {
    writer_switches().keys().all(|task| {
        std::fs::read_to_string(format!("{task}/stat")).is_ok_and(|stat| {
            stat.rsplit_once(')')
                .and_then(|(_, rest)| rest.split_whitespace().next())
                == Some("S")
        })
    })
}

/// Context switches so far of every writer thread, by task path. An
/// unreadable `/proc` finds none, which fails the test's search for them.
fn writer_switches() -> BTreeMap<String, u64> {
    let mut switches = BTreeMap::new();
    let tasks = std::fs::read_dir("/proc/self/task").into_iter().flatten();
    for path in tasks.flatten().map(|task| task.path()) {
        let Ok(status) = std::fs::read_to_string(path.join("status")) else {
            continue; // the thread ended
        };
        let field = |name: &str| {
            status
                .lines()
                .find_map(|line| line.strip_prefix(name))
                .map(str::trim)
        };
        if !field("Name:").is_some_and(is_writer_thread) {
            continue;
        }
        let count = |name| {
            field(name)
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0)
        };
        let tid = path.to_string_lossy().into_owned();
        switches.insert(
            tid,
            count("voluntary_ctxt_switches:").saturating_add(count("nonvoluntary_ctxt_switches:")),
        );
    }
    switches
}

#[test]
fn a_kept_writer_sleeps_between_commits_and_leaves_with_its_index() {
    let mut index = SearchIndex::default();
    index.replace_all((0..200).map(|key| Document::new(key).field("row", FieldWeight::TITLE)));
    index.commit().unwrap();
    assert!(
        writer_switches().is_empty(),
        "a bulk build starts no writer"
    );
    for key in 200..205 {
        index.upsert(Document::new(key).field("new row", FieldWeight::TITLE));
        index.commit().unwrap();
    }
    assert_eq!(index.search("new", Page::first(10)).hits.len(), 5);

    // Let the indexing thread a commit respawns reach its channel.
    tilo_test_env::wait_until("the writer threads to go to sleep", writers_asleep);
    let before = writer_switches();
    assert!(
        before.len() >= 2,
        "the kept writer's threads were not found: {before:?}"
    );
    std::thread::sleep(Duration::from_secs(2));
    let after = writer_switches();
    let woke: Vec<_> = before
        .iter()
        .filter(|(tid, count)| after.get(*tid) != Some(count))
        .collect();
    assert!(
        woke.is_empty(),
        "idle writer threads woke: {woke:?} -> {after:?}"
    );

    // Dropping the index shuts the writer down: its threads end.
    drop(index);
    tilo_test_env::wait_until("the writer threads to end with their index", || {
        writer_switches().is_empty()
    });
}
