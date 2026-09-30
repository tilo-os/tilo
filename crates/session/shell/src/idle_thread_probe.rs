//! Observe whether a service thread sleeps while it has nothing to do.
//!
//! Linux-only: it reads the thread's context switches from `/proc/self/task`.

use std::collections::BTreeSet;
use std::time::Duration;

fn thread_ids() -> BTreeSet<String> {
    std::fs::read_dir("/proc/self/task")
        .expect("/proc/self/task")
        .map(|entry| entry.expect("task").file_name().into_string().expect("tid"))
        .collect()
}

fn context_switches(tid: &str) -> u64 {
    std::fs::read_to_string(format!("/proc/self/task/{tid}/status"))
        .expect("thread status")
        .lines()
        .filter(|line| line.contains("ctxt_switches"))
        .filter_map(|line| line.split_whitespace().nth(1)?.parse::<u64>().ok())
        .sum()
}

/// Run `start` and return its result with the id of the thread it spawned
/// under `comm` (the kernel's 15-byte truncation of the thread name).
pub(crate) fn spawned_thread<T>(comm: &str, start: impl FnOnce() -> T) -> (T, String) {
    let before = thread_ids();
    let started = start();
    let tid = thread_ids()
        .difference(&before)
        .find(|tid| {
            std::fs::read_to_string(format!("/proc/self/task/{tid}/comm"))
                .is_ok_and(|name| name.trim() == comm)
        })
        .cloned()
        .unwrap_or_else(|| panic!("no new thread named {comm}"));
    (started, tid)
}

/// Fail if thread `tid` is scheduled at all during an idle 400 ms.
pub(crate) fn assert_stays_asleep(tid: &str, what: &str) {
    std::thread::sleep(Duration::from_millis(50));
    let settled = context_switches(tid);
    std::thread::sleep(Duration::from_millis(400));
    let woke = context_switches(tid).saturating_sub(settled);
    assert_eq!(woke, 0, "idle {what} woke {woke} times in 400 ms");
}
