//! A program whose `main` serves its own workers starts them with the worker
//! argument alone: libtest's arguments belong to a test binary's workers, and
//! a person reading a program's process list should not see them.

use std::process::Command;

use tilo_test_env::SpawnUnderEnvLock as _;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ROOT};
use tilo_automerge_isolate::{Applied, Dependencies, Isolate, Limits};

/// Set in the copy of this binary that plays the program.
const PROGRAM: &str = "TILO_AUTOMERGE_ISOLATE_TEST_PROGRAM";
const TEST: &str = "program::a_program_starts_its_workers_with_the_worker_argument_alone";

#[test]
fn a_program_starts_its_workers_with_the_worker_argument_alone() {
    if std::env::var_os(PROGRAM).is_some() {
        be_the_program();
        return;
    }
    let status = Command::new(std::env::current_exe().unwrap())
        .args([TEST, "--exact", "--nocapture", "--test-threads=1"])
        .env(PROGRAM, "1")
        .status_locked()
        .unwrap();
    assert!(status.success(), "the program's start failed: {status}");
}

/// What a program does: serve its workers first thing, then admit a change,
/// which starts one.
fn be_the_program() {
    assert_eq!(tilo_automerge_isolate::dispatch_early(), None);
    let isolate = Isolate::new(Limits::default());
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "base", 1_i64).unwrap();
    doc.commit();
    let mut remote = doc.fork();
    remote.put(ROOT, "remote", 2_i64).unwrap();
    remote.commit();
    let change = remote.save_after(&doc.get_heads());
    assert_eq!(
        isolate.apply(&mut doc, &change, Dependencies::RequireComplete),
        Ok(Applied::Changed(true))
    );
    let workers: Vec<Vec<String>> = children()
        .into_iter()
        .map(|pid| arguments(pid).into_iter().skip(1).collect())
        .collect();
    assert!(
        workers.contains(&vec!["tilo_automerge_isolate_worker".to_owned()]),
        "this program's children start with {workers:?}"
    );
}

fn children() -> Vec<u32> {
    let mut children = Vec::new();
    for task in std::fs::read_dir("/proc/self/task").unwrap() {
        let listed = std::fs::read_to_string(task.unwrap().path().join("children")).unwrap();
        children.extend(
            listed
                .split_whitespace()
                .map(|pid| pid.parse::<u32>().unwrap()),
        );
    }
    children
}

fn arguments(pid: u32) -> Vec<String> {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .unwrap_or_default()
        .split(|byte| *byte == 0)
        .filter(|argument| !argument.is_empty())
        .map(|argument| String::from_utf8_lossy(argument).into_owned())
        .collect()
}
