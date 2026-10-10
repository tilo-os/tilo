//! What one commit costs and what survives a crash (TIL-732): the barriers a
//! delta commit issues, the sequence reservation across a restart, and a
//! writer killed in the middle of committing.
#![allow(
    clippy::significant_drop_tightening,
    reason = "each test keeps its workspace owner state alive until it ends, as a daemon's owner lives"
)]

use super::{SEQUENCE_RESERVATION_NS, State, isolated_test_workers, replay};
use crate::coedit::CoeditPayload;
use crate::store::WorkspaceStore;
use crate::store::replay::{Step, WorkDiscard, WorkLimit};
use crate::transport::Envelope;
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Semaphore;

const WORKSPACE: u64 = 7;
const CHANNEL: [u8; 32] = [9; 32];
/// Set by `a_killed_writer_loses_no_acknowledged_edit` for its child.
const KILLED_WRITER_ROOT: &str = "TILO_RELAY_TEST_KILLED_WRITER_ROOT";
/// The child reports each commit after it returned, as `acked <index> <seq>`.
const ACKED: &str = "acked ";

fn open(root: &Path) -> State {
    State {
        store: WorkspaceStore::open(root, WORKSPACE, false).unwrap(),
        doc: None,
        sequence: None,
        reserved: 0,
        replays: replay::Replays::default(),
        retention_scratch: None,
        retention_candidates: Vec::new(),
        workers: isolated_test_workers(),
        replay_worker: Arc::new(Semaphore::new(1)),
    }
}

/// One edit's retained frame; its body is `index`.
fn edit(index: u32) -> Vec<u8> {
    crate::transport::encode(&Envelope::Delta {
        workspace_id: WORKSPACE,
        author_device_public_key: [1; 32],
        authored_at_ns: u64::from(index),
        delta: CoeditPayload::RetainedDelta {
            channel: CHANNEL,
            body: index.to_le_bytes().to_vec(),
        }
        .encode()
        .unwrap(),
        signature: [0; 64],
    })
    .unwrap()
}

/// The owner's work for one admitted edit, as `peer.rs` drives it: take a
/// sequence, retain the frame to `Done`, then dispose of the transaction's
/// scratch. Asserts the retention created no scratch directory.
fn commit(state: &State, sequence: u64, index: u32) {
    let mut build = state
        .store
        .start_retain(&CHANNEL, Some(sequence), edit(index))
        .unwrap();
    let scratch = build.scratch().to_owned();
    while let Step::More = build.step(WorkLimit::default()).unwrap() {
        assert!(!scratch.exists(), "a delta's retention created scratch");
    }
    assert!(!scratch.exists(), "a delta's retention created scratch");
    drop(build);
    let mut discard = WorkDiscard::new(scratch).unwrap();
    while let Step::More = discard.step(WorkLimit::default()).unwrap() {}
}

/// The bodies of the workspace's retained edits, in retained order.
fn retained_edits(store: &WorkspaceStore) -> Vec<u32> {
    store
        .load_retained()
        .unwrap()
        .iter()
        .map(|frame| {
            let body = crate::store::retained::body_of(frame, WORKSPACE).unwrap();
            u32::from_le_bytes(body.try_into().unwrap())
        })
        .collect()
}

#[test]
fn a_delta_commit_costs_its_record_barriers_only() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = open(dir.path());
    let now = 1_800_000_000_000_000_000;
    // The first commits provision the lineage, its generation and the
    // reservation, and leave every ancestor synced.
    for index in 0..2 {
        let sequence = state.next_sequence_at(now).unwrap();
        commit(&state, sequence, index);
    }
    let before = crate::store::thread_fsyncs();
    for index in 2..10 {
        let sequence = state.next_sequence_at(now).unwrap();
        commit(&state, sequence, index);
    }
    // The record and its directory; Apple repeats the directory with
    // F_FULLFSYNC (`retained::durable_write`).
    let barriers = if cfg!(target_vendor = "apple") { 3 } else { 2 };
    assert_eq!(
        crate::store::thread_fsyncs() - before,
        barriers * 8,
        "an edit inside the reservation costs its record barriers, nothing else"
    );
    // Renewing the reservation costs the file and the store root, once.
    let renewal = barriers;
    let before = crate::store::thread_fsyncs();
    let renewed = state
        .next_sequence_at(now + SEQUENCE_RESERVATION_NS + 1)
        .unwrap();
    assert_eq!(crate::store::thread_fsyncs() - before, renewal);
    // Commits up to the new bound renew nothing.
    let before = crate::store::thread_fsyncs();
    for offset in [1, SEQUENCE_RESERVATION_NS - 1] {
        state.next_sequence_at(renewed + offset).unwrap();
    }
    assert_eq!(crate::store::thread_fsyncs() - before, 0);
    assert_eq!(retained_edits(&state.store), (0..10).collect::<Vec<_>>());
}

#[test]
fn the_sequence_survives_a_restart_with_the_clock_stepped_back() {
    let dir = tempfile::tempdir().unwrap();
    let now = 1_800_000_000_000_000_000;
    let issued = {
        let mut state = open(dir.path());
        let issued: Vec<u64> = (0..3)
            .map(|tick| state.next_sequence_at(now + tick).unwrap())
            .collect();
        assert!(issued.windows(2).all(|pair| pair[0] < pair[1]));
        // Sequences outrun a clock that stands still.
        assert_eq!(issued, [now, now + 1, now + 2]);
        issued
    };
    // A new owner after a crash, with the clock an hour behind.
    let mut state = open(dir.path());
    let resumed = state.next_sequence_at(now - 3_600_000_000_000).unwrap();
    assert!(
        resumed > issued[2],
        "{resumed} reissues host order {issued:?}"
    );
    assert!(
        resumed <= issued[2] + SEQUENCE_RESERVATION_NS + 1,
        "a restart resumes at most one reservation ahead"
    );
    // And it persisted its own reservation before handing that out.
    let mut again = open(dir.path());
    assert!(again.next_sequence_at(0).unwrap() > resumed);
}

/// The child half of `a_killed_writer_loses_no_acknowledged_edit`: commits
/// edits until it is killed, reporting each after its commit returned. Run
/// outside that test it has nothing to do and returns.
#[test]
fn killed_writer_child() {
    let Some(root) = std::env::var_os(KILLED_WRITER_ROOT) else {
        return;
    };
    let mut state = open(Path::new(&root));
    let mut out = std::io::stdout().lock();
    // Bounded, in case the parent died without killing this process.
    for index in 0..100_000 {
        let sequence = state.next_sequence().unwrap();
        commit(&state, sequence, index);
        writeln!(out, "{ACKED}{index} {sequence}").unwrap();
        out.flush().unwrap();
    }
}

#[test]
fn a_killed_writer_loses_no_acknowledged_edit() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "persistence::durability_tests::killed_writer_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(KILLED_WRITER_ROOT, dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut acked = Vec::new();
    for line in BufReader::new(child.stdout.take().unwrap()).lines() {
        let line = line.unwrap();
        // libtest prints the test's name on the line its first output joins.
        let Some((index, sequence)) = line
            .split_once(ACKED)
            .and_then(|(_, rest)| rest.split_once(' '))
        else {
            continue;
        };
        acked.push((
            index.parse::<u32>().unwrap(),
            sequence.parse::<u64>().unwrap(),
        ));
        if acked.len() == 64 {
            break;
        }
    }
    // SIGKILL: no destructor or flush runs, wherever the next commit was.
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(acked.len(), 64, "the writer stopped before it was killed");
    assert!(
        acked
            .iter()
            .enumerate()
            .all(|(at, (index, _))| usize::try_from(*index).unwrap() == at)
    );
    assert!(acked.windows(2).all(|pair| pair[0].1 < pair[1].1));

    let mut state = open(dir.path());
    // Every acknowledged edit is retained, in order. Commits that finished
    // after the last report may follow, without a gap.
    let retained = retained_edits(&state.store);
    assert!(
        retained.len() >= acked.len(),
        "lost acknowledged edits: {retained:?}"
    );
    assert_eq!(
        retained,
        (0..u32::try_from(retained.len()).unwrap()).collect::<Vec<_>>()
    );
    // Host order continues past every sequence handed out, even with the
    // clock wound back to the epoch.
    let resumed = state.next_sequence_at(0).unwrap();
    let last = acked.last().unwrap().1;
    assert!(resumed > last, "{resumed} reissues host order {last}");
    // The store takes the next edit cleanly after the crash.
    let next = u32::try_from(retained.len()).unwrap();
    commit(&state, resumed, next);
    assert_eq!(retained_edits(&state.store), (0..=next).collect::<Vec<_>>());
}
