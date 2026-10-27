//! The append-only store: what each transition costs on disk, and what a
//! restart makes of every way the file can be left.
use super::*;
use serde_json::{Value, json};

fn fresh() -> (tempfile::TempDir, PathBuf, InvocationJournal) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("journal");
    let journal = InvocationJournal::open(&root).unwrap();
    (dir, root, journal)
}

fn key(journal: &InvocationJournal, n: u128) -> InvocationKey {
    InvocationKey {
        room: "5ca720b0-7649-4bdc-9c30-77380f7c2be5".into(),
        host: journal.host(),
        owner: InvocationOwner::Local {
            actor: "person".into(),
        },
        invocation: InvocationId(uuid::Uuid::from_u128(0x7000_0000_0000_0000 | n)),
    }
}

/// The request part exactly as the broker builds it for an admission.
fn request(argument: &str) -> Value {
    json!({"peer_id": 7, "app_id": "so.tilo.notes", "record_id": "notes",
        "resource": "note:1", "action": "write", "argument": argument})
}

fn intent(argument: &str) -> Value {
    json!({"request": request(argument), "contract": tilo_actions::ActionContract::default(),
        "operation_kind": tilo_actions::OperationKind::Effect, "actor": "person",
        "app_id": "so.tilo.notes", "resource": "note:1", "action": "write", "revision": null})
}

fn succeeded(key: &InvocationKey, value: &str) -> InvocationUpdate {
    InvocationUpdate::Succeeded {
        invocation: key.invocation,
        output: tilo_actions::ActionOutput {
            value: Some(json!(value)),
            resources: vec![],
        },
    }
}

fn progress(key: &InvocationKey, sequence: u64) -> InvocationUpdate {
    InvocationUpdate::Progress {
        invocation: key.invocation,
        sequence,
        phase: "copying".into(),
        completed: Some(sequence),
        total: Some(1000),
        message: Some("still going".into()),
    }
}

fn store(root: &Path) -> Vec<u8> {
    std::fs::read(root.join("records.json")).unwrap()
}

/// Lines after the snapshot line; each is one appended entry.
fn entries(root: &Path) -> Vec<Value> {
    let bytes = store(root);
    bytes
        .split(|byte| *byte == b'\n')
        .skip(1)
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect()
}

#[test]
fn append_journal_invocation_costs_one_data_barrier_per_durable_transition() {
    let (_dir, root, mut journal) = fresh();
    let k = key(&journal, 1);
    let opened = journal.barriers();
    assert!(journal.admit(k.clone(), intent("draft"), 1).unwrap());
    let admitted = journal.barriers();
    assert_eq!(
        (
            admitted.writes - opened.writes,
            admitted.file - opened.file,
            admitted.directory - opened.directory
        ),
        (1, 1, 0),
        "an admission is one appended entry and one data barrier"
    );
    let length = store(&root).len();
    for sequence in 1..=500 {
        journal.update(&k, progress(&k, sequence), false).unwrap();
    }
    assert_eq!(
        journal.barriers(),
        admitted,
        "progress never writes or waits on the disk"
    );
    assert_eq!(store(&root).len(), length);
    assert!(
        matches!(
            journal.lookup(&k).unwrap().unwrap().update,
            InvocationUpdate::Progress { sequence: 500, .. }
        ),
        "observers still see the latest progress"
    );
    journal.update(&k, succeeded(&k, "saved"), false).unwrap();
    let done = journal.barriers();
    assert_eq!(
        (
            done.writes - admitted.writes,
            done.file - admitted.file,
            done.directory - admitted.directory
        ),
        (1, 1, 0),
        "a terminal outcome is one appended entry and one data barrier"
    );
    assert_eq!(entries(&root).len(), 2);
    drop(journal);
    let journal = InvocationJournal::open(&root).unwrap();
    let record = journal.lookup(&k).unwrap().unwrap();
    assert_eq!(record.update, succeeded(&k, "saved"));
    assert!(!record.uncertain);
}

#[test]
fn append_journal_crash_between_admission_and_dispatch_never_redispatches() {
    let (_dir, root, mut journal) = fresh();
    let k = key(&journal, 2);
    assert!(journal.admit(k.clone(), intent("draft"), 1).unwrap());
    journal.update(&k, progress(&k, 1), false).unwrap();
    assert_eq!(entries(&root).len(), 1, "only the admission is on disk");
    // The process dies here: nothing but the appended admission survives.
    drop(journal);
    let mut journal = InvocationJournal::open(&root).unwrap();
    let record = journal.lookup(&k).unwrap().unwrap();
    assert!(record.uncertain);
    assert!(
        matches!(record.update, InvocationUpdate::Failed { ref error, .. } if error.code == "action_outcome_unknown" && !error.retryable)
    );
    assert!(
        !journal.admit(k.clone(), intent("draft"), 2).unwrap(),
        "recovery never issues a second dispatch permit"
    );
    assert!(
        journal.admit(k.clone(), intent("changed"), 2).is_err(),
        "the reserved identity refuses a different request"
    );
    assert!(entries(&root).is_empty(), "reopening compacts the store");
    let snapshot: Value = serde_json::from_slice(&store(&root)).unwrap();
    assert_eq!(snapshot["records"][0]["uncertain"], true);
}

#[test]
fn append_journal_terminal_outcome_survives_restart_and_is_reported_again() {
    let (_dir, root, mut journal) = fresh();
    let k = key(&journal, 3);
    assert!(journal.admit(k.clone(), intent("draft"), 1).unwrap());
    journal
        .update(&k, succeeded(&k, "original"), false)
        .unwrap();
    drop(journal);
    let mut journal = InvocationJournal::open(&root).unwrap();
    assert!(!journal.admit(k.clone(), intent("draft"), 2).unwrap());
    let record = journal.lookup(&k).unwrap().unwrap();
    assert_eq!(record.update, succeeded(&k, "original"));
    assert!(!record.uncertain);
    assert!(record.admitted_request_is(&request("draft")));
    assert!(!record.admitted_request_is(&request("draft ")));
    assert!(journal.update(&k, succeeded(&k, "again"), false).is_err());
}

#[test]
fn append_journal_failed_entry_is_taken_back_and_poisons_the_writer() {
    for failure in [BarrierFailure::File, BarrierFailure::Directory] {
        let (_dir, root, mut journal) = fresh();
        let k = key(&journal, 4);
        assert!(journal.admit(k.clone(), intent("draft"), 1).unwrap());
        let length = store(&root).len();
        journal.fail_at = Some(failure);
        let outcome = journal.update(&k, succeeded(&k, "lost"), false);
        assert!(
            outcome.is_err(),
            "a failed write is reported: {:?} failed={}",
            outcome,
            journal.failed()
        );
        assert!(
            journal.failed() == (failure == BarrierFailure::Directory),
            "only an unconfirmed write fails the journal (failed={})",
            journal.failed()
        );
        if failure == BarrierFailure::Directory {
            // The rename may have made it durable: the journal requires
            // recovery, which reads the store back.
            assert!(journal.lookup(&k).is_err(), "a failed barrier poisons");
        } else {
            assert_eq!(
                store(&root).len(),
                length,
                "the unacknowledged entry is withdrawn"
            );
            assert!(
                !journal.failed(),
                "a withdrawn write left the store as it was"
            );
            assert!(
                matches!(
                    journal.lookup(&k).unwrap().unwrap().update,
                    InvocationUpdate::Accepted { .. }
                ),
                "the record still reads as the store has it"
            );
        }
        drop(journal);
        let journal = InvocationJournal::open(&root).unwrap();
        let record = journal.lookup(&k).unwrap().unwrap();
        if failure == BarrierFailure::File {
            assert!(record.uncertain, "the outcome was never acknowledged");
        } else {
            assert_eq!(record.update, succeeded(&k, "lost"));
            assert!(!record.uncertain, "the durable outcome stands");
        }
    }
}

#[test]
fn append_journal_reconciles_a_kept_store_before_appending_to_it() {
    let (_dir, root, mut journal) = fresh();
    let k = key(&journal, 13);
    assert!(journal.admit(k.clone(), intent("draft"), 1).unwrap());
    journal.update(&k, succeeded(&k, "done"), false).unwrap();
    drop(journal);
    // Reopening compacts; the next open keeps that clean store for appends.
    drop(InvocationJournal::open(&root).unwrap());
    let called = std::cell::Cell::new(false);
    let refused = InvocationJournal::open_with_barriers(
        &root,
        |_| Ok(()),
        |observed, directory| {
            called.set(true);
            assert_eq!(
                observed.metadata().unwrap().ino(),
                std::fs::metadata(root.join("records.json")).unwrap().ino()
            );
            assert!(directory.metadata().unwrap().is_dir());
            Err(std::io::Error::other("injected reconciliation failure"))
        },
    );
    assert!(called.get(), "a kept store skipped reconciliation");
    assert!(
        refused.is_err(),
        "no entry may follow a rename whose directory barrier is unproven"
    );
    let mut journal = InvocationJournal::open(&root).unwrap();
    assert!(journal.admit(key(&journal, 14), intent("next"), 2).unwrap());
    assert_eq!(
        entries(&root).len(),
        1,
        "the reconciled store takes entries"
    );
}

/// A store with A retired and B admitted, B's admission last.
fn two_invocations() -> (tempfile::TempDir, PathBuf, InvocationKey, InvocationKey) {
    let (dir, root, mut journal) = fresh();
    let (a, b) = (key(&journal, 5), key(&journal, 6));
    assert!(journal.admit(a.clone(), intent("a"), 1).unwrap());
    journal.update(&a, succeeded(&a, "a done"), false).unwrap();
    assert!(journal.admit(b.clone(), intent("b"), 2).unwrap());
    drop(journal);
    (dir, root, a, b)
}

#[test]
fn append_journal_ignores_only_a_torn_final_entry() {
    let torn: [fn(&mut Vec<u8>); 3] = [
        |bytes| {
            let keep = bytes.len() - 40;
            bytes.truncate(keep);
        },
        // The final entry's blocks were never written.
        |bytes| {
            let start = bytes[..bytes.len() - 1]
                .iter()
                .rposition(|byte| *byte == b'\n')
                .unwrap()
                + 1;
            let end = bytes.len() - 1;
            bytes[start..end].fill(0);
        },
        // Only its first block was written.
        |bytes| {
            let end = bytes.len();
            bytes[end - 30..end].fill(0);
        },
    ];
    for tear in torn {
        let (_dir, root, a, b) = two_invocations();
        let mut bytes = store(&root);
        tear(&mut bytes);
        std::fs::write(root.join("records.json"), &bytes).unwrap();
        let mut journal = InvocationJournal::open(&root).unwrap();
        let retired = journal.lookup(&a).unwrap().unwrap();
        assert_eq!(retired.update, succeeded(&a, "a done"));
        assert!(
            journal.lookup(&b).unwrap().is_none(),
            "a torn admission was never acknowledged, so never dispatched"
        );
        assert!(
            entries(&root).is_empty(),
            "the torn bytes are compacted away"
        );
        assert!(journal.admit(b, intent("b"), 3).unwrap());
        drop(journal);
        assert!(InvocationJournal::open(&root).is_ok());
    }
}

#[test]
fn append_journal_corruption_before_the_final_entry_stays_fatal() {
    let corrupt: [fn(&mut Vec<u8>); 4] = [
        // A middle entry lost bytes but kept its terminator.
        |bytes| {
            let second = bytes.iter().position(|byte| *byte == b'\n').unwrap() + 1;
            bytes.drain(second + 10..second + 20);
        },
        |bytes| {
            let second = bytes.iter().position(|byte| *byte == b'\n').unwrap() + 1;
            bytes[second + 5..second + 15].fill(0);
        },
        // The final entry is complete but is not an entry.
        |bytes| {
            let end = bytes.len() - 1;
            bytes[end - 5..end].copy_from_slice(b"xxxxx");
        },
        // Garbage instead of a separator after the snapshot.
        |bytes| {
            let first = bytes.iter().position(|byte| *byte == b'\n').unwrap();
            bytes[first] = b'x';
        },
    ];
    for (case, damage) in corrupt.into_iter().enumerate() {
        let (_dir, root, _, _) = two_invocations();
        let mut bytes = store(&root);
        damage(&mut bytes);
        std::fs::write(root.join("records.json"), &bytes).unwrap();
        assert!(
            InvocationJournal::open(&root).is_err(),
            "case {case} opened"
        );
    }
}

#[test]
fn append_journal_entries_only_move_a_live_record_forward() {
    let snapshot_line = |root: &Path| -> Vec<u8> {
        let bytes = store(root);
        let end = bytes.iter().position(|byte| *byte == b'\n').unwrap() + 1;
        bytes[..end].to_vec()
    };
    let rewrite = |root: &Path, lines: &[Value]| {
        let mut bytes = snapshot_line(root);
        for line in lines {
            bytes.extend(serde_json::to_vec(line).unwrap());
            bytes.push(b'\n');
        }
        std::fs::write(root.join("records.json"), bytes).unwrap();
    };
    let (_dir, root, mut journal) = fresh();
    let k = key(&journal, 7);
    assert!(journal.admit(k.clone(), intent("draft"), 1).unwrap());
    journal.update(&k, succeeded(&k, "done"), false).unwrap();
    drop(journal);
    let original = entries(&root);
    let [admission, terminal] = original.as_slice() else {
        panic!("one admission and one outcome")
    };
    let mut downgraded = terminal.clone();
    downgraded["version"] = 0.into();
    let mut foreign_handle = terminal.clone();
    foreign_handle["record"]["wire_id"] = 9.into();
    let mut foreign_host = admission.clone();
    foreign_host["record"]["key"]["host"] = uuid::Uuid::from_u128(1).to_string().into();
    let mut both_forms = admission.clone();
    both_forms["record"]["intent"]["request"] = request("draft");
    let mut unknown_member = admission.clone();
    unknown_member["record"]["secret"] = "x".into();
    for lines in [
        // A retired record cannot be reopened, nor retired twice.
        vec![admission.clone(), terminal.clone(), admission.clone()],
        vec![admission.clone(), terminal.clone(), terminal.clone()],
        vec![admission.clone(), downgraded],
        vec![admission.clone(), foreign_handle],
        vec![foreign_host],
        vec![both_forms],
        vec![unknown_member],
    ] {
        rewrite(&root, &lines);
        assert!(InvocationJournal::open(&root).is_err(), "{lines:?}");
    }
    rewrite(&root, &original);
    let journal = InvocationJournal::open(&root).unwrap();
    assert_eq!(
        journal.lookup(&k).unwrap().unwrap().update,
        succeeded(&k, "done")
    );
}

#[test]
fn append_journal_upgrades_a_store_written_by_the_previous_writer() {
    // Byte-for-byte what the previous writer left: one compact snapshot, no
    // separator, full requests retained, one retired and one live record.
    let host = "00000000-0000-0000-0000-000000000063";
    let row = |n: u128, update: Value| {
        json!({"key":{"room":"00000000-0000-0000-0000-000000000001","host":host,
            "owner":{"Local":{"actor":"person"}},"invocation":uuid::Uuid::from_u128(n).to_string()},
            "intent":intent(&format!("legacy {n}")),"wire_id":n,"update":update,
            "uncertain":false,"cancellation_requested":false})
    };
    let retired = row(
        4,
        json!({"state":"succeeded","invocation":uuid::Uuid::from_u128(4).to_string(),"output":{"value":"legacy result"}}),
    );
    let live = row(
        5,
        json!({"state":"accepted","invocation":uuid::Uuid::from_u128(5).to_string()}),
    );
    for (records, recovers) in [
        (vec![retired.clone()], false),
        (vec![retired.clone(), live], true),
    ] {
        let legacy =
            serde_json::to_vec(&json!({"version":1,"host":host,"records":records})).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("journal");
        drop(InvocationJournal::open(&root).unwrap());
        std::fs::write(root.join("records.json"), &legacy).unwrap();
        let mut journal = InvocationJournal::open(&root).unwrap();
        let old: InvocationKey = serde_json::from_value(retired["key"].clone()).unwrap();
        assert_eq!(
            store(&root) == legacy,
            !recovers,
            "a store needing no recovery is not rewritten by opening it"
        );
        assert!(!journal.admit(old.clone(), intent("legacy 4"), 9).unwrap());
        assert!(journal.admit(old.clone(), intent("legacy 4!"), 9).is_err());
        let record = journal.lookup(&old).unwrap().unwrap();
        assert!(record.admitted_request_is(&request("legacy 4")));
        assert!(!record.admitted_request_is(&request("legacy 5")));
        if recovers {
            let five: InvocationKey = serde_json::from_value(records_key(5)).unwrap();
            assert!(journal.lookup(&five).unwrap().unwrap().uncertain);
        }
        let first = key(&journal, 10);
        let second = key(&journal, 11);
        assert!(journal.admit(first, intent("new"), 10).unwrap());
        assert!(journal.admit(second, intent("newer"), 11).unwrap());
        assert_eq!(
            entries(&root).len(),
            if recovers { 2 } else { 1 },
            "the open or the first write converted the store; later writes append"
        );
        assert!(
            serde_json::from_slice::<serde::de::IgnoredAny>(&store(&root)).is_err(),
            "the previous writer's whole-file decode refuses pending entries"
        );
        drop(journal);
        let journal = InvocationJournal::open(&root).unwrap();
        assert_eq!(journal.lookup(&old).unwrap().unwrap(), record);
        let saved: Value = serde_json::from_slice(&store(&root)).unwrap();
        assert_eq!(
            saved["records"][0], retired,
            "retained rows are not rewritten"
        );
        assert_eq!(
            saved["records"].as_array().unwrap().len(),
            records.len() + 2
        );
    }
}

fn records_key(n: u128) -> Value {
    json!({"room":"00000000-0000-0000-0000-000000000001","host":"00000000-0000-0000-0000-000000000063",
        "owner":{"Local":{"actor":"person"}},"invocation":uuid::Uuid::from_u128(n).to_string()})
}

#[test]
fn append_journal_keys_stay_reserved_after_compaction_without_their_arguments() {
    let (_dir, root, mut journal) = fresh();
    let k = key(&journal, 12);
    let secret = "argument that must never reach the disk";
    assert!(journal.admit(k.clone(), intent(secret), 1).unwrap());
    journal.update(&k, succeeded(&k, "done"), false).unwrap();
    assert!(!String::from_utf8_lossy(&store(&root)).contains(secret));
    drop(journal);
    let mut journal = InvocationJournal::open(&root).unwrap();
    assert!(entries(&root).is_empty(), "compacted");
    assert!(!String::from_utf8_lossy(&store(&root)).contains(secret));
    assert!(!journal.admit(k.clone(), intent(secret), 2).unwrap());
    assert!(journal.admit(k.clone(), intent("other"), 2).is_err());
    let mut reordered = intent(secret);
    let fields = reordered.as_object_mut().unwrap();
    let request = fields.remove("request").unwrap();
    let mut members: Vec<_> = request.as_object().unwrap().clone().into_iter().collect();
    members.reverse();
    fields.insert(
        "request".into(),
        Value::Object(members.into_iter().collect()),
    );
    assert!(
        !journal.admit(k.clone(), reordered, 2).unwrap(),
        "member order is not part of the request"
    );
    let mut foreign = k.clone();
    foreign.owner = InvocationOwner::Local {
        actor: "someone else".into(),
    };
    let Err(conflict) = journal.admit(foreign, intent(secret), 2) else {
        panic!("a retired UUID was admitted for another owner")
    };
    assert!(conflict.contains("UUID already reserved"), "{conflict}");
    let record = journal.lookup(&k).unwrap().unwrap();
    assert!(record.intent.get("request").is_none());
    assert_eq!(
        record.intent["action"], "write",
        "authorization inputs stay"
    );
}

#[test]
fn append_journal_size_stays_bounded_over_ten_thousand_invokes() {
    const INVOKES: u128 = 10_000;
    let (_dir, root, mut journal) = fresh();
    // This measures bytes, not durability.
    journal.elide_barriers();
    let argument = "a".repeat(4096);
    let mut largest = 0;
    for n in 1..=INVOKES {
        let k = key(&journal, n);
        let wire = u64::try_from(n).unwrap();
        assert!(
            journal
                .admit(k.clone(), intent(&format!("{n}{argument}")), wire)
                .unwrap()
        );
        journal.update(&k, progress(&k, 1), false).unwrap();
        journal.update(&k, succeeded(&k, "ok"), false).unwrap();
        largest = largest.max(std::fs::metadata(root.join("records.json")).unwrap().len());
    }
    let barriers = journal.barriers();
    assert!(
        barriers.directory > 1,
        "the entries were compacted along the way"
    );
    assert_eq!(
        (barriers.writes, barriers.file),
        (2 * 10_000 + 1, 2 * 10_000 + 1),
        "one write and one barrier per admission and per outcome, none for progress"
    );
    let first = key(&journal, 1);
    assert!(
        !journal
            .admit(first.clone(), intent(&format!("1{argument}")), 1)
            .unwrap()
    );
    assert!(journal.admit(first, intent("changed"), 1).is_err());
    drop(journal);
    drop(InvocationJournal::open(&root).unwrap());
    let compacted = std::fs::metadata(root.join("records.json")).unwrap().len();
    let per_invocation = compacted / 10_000;
    assert!(
        per_invocation < 2048,
        "{per_invocation} bytes retained per invocation; arguments are 4 KiB"
    );
    assert!(
        largest <= 2 * compacted + InvocationJournal::COMPACT_AFTER,
        "the store peaked at {largest} bytes for a {compacted}-byte live state"
    );
    assert!(!String::from_utf8_lossy(&store(&root)).contains(&argument));
}

#[test]
fn append_journal_write_taken_back_leaves_the_next_write_possible() {
    let (_dir, root, mut journal) = fresh();
    let k = key(&journal, 21);
    assert!(journal.admit(k.clone(), intent("draft"), 1).unwrap());
    let admitted = store(&root);
    journal.fail_at = Some(BarrierFailure::File);
    assert!(journal.update(&k, succeeded(&k, "dropped"), false).is_err());
    assert_eq!(store(&root), admitted, "the failed entry was withdrawn");
    assert!(
        !journal.failed(),
        "a write that left nothing durable requires no recovery"
    );
    journal.fail_at = None;
    journal.update(&k, succeeded(&k, "kept"), false).unwrap();
    let record = journal.lookup(&k).unwrap().unwrap();
    assert_eq!(record.update, succeeded(&k, "kept"));
    assert!(!record.uncertain);
    drop(journal);
    let journal = InvocationJournal::open(&root).unwrap();
    assert_eq!(
        journal.lookup(&k).unwrap().unwrap().update,
        succeeded(&k, "kept")
    );
}

#[test]
fn append_journal_recovers_a_durable_but_unconfirmed_write_without_restart() {
    let (_dir, root, mut journal) = fresh();
    let k = key(&journal, 22);
    assert!(journal.admit(k.clone(), intent("draft"), 1).unwrap());
    let before = store(&root).len();
    journal.unconfirmed_next = true;
    assert!(
        journal.update(&k, succeeded(&k, "maybe"), false).is_err(),
        "the caller is told the write failed"
    );
    assert!(journal.failed(), "the store may hold it");
    assert!(
        journal.lookup(&k).is_err(),
        "nothing is read while recovery is required"
    );
    assert_ne!(store(&root).len(), before, "the entry did reach the store");
    journal.recover().unwrap();
    assert!(!journal.failed());
    let record = journal.lookup(&k).unwrap().unwrap();
    assert_eq!(
        record.update,
        succeeded(&k, "maybe"),
        "recovery reads back the terminal the store holds"
    );
    assert!(!record.uncertain);
    assert!(
        entries(&root).is_empty(),
        "recovery folds the store into one snapshot"
    );
    // The admission already made is still known, so it is never reissued.
    assert!(!journal.admit(k.clone(), intent("draft"), 2).unwrap());
    assert!(
        journal.update(&k, succeeded(&k, "after"), false).is_err(),
        "a terminal is final"
    );
    assert!(
        matches!(
            journal.lookup(&k).unwrap().unwrap().update,
            InvocationUpdate::Succeeded { .. }
        ),
        "the journal goes on working once recovery settled the store"
    );
    drop(journal);
    let journal = InvocationJournal::open(&root).unwrap();
    assert_eq!(
        journal.lookup(&k).unwrap().unwrap().update,
        succeeded(&k, "maybe")
    );
}

#[test]
fn append_journal_recovery_turns_nonterminal_writes_uncertain_and_goes_on_working() {
    let (_dir, root, mut journal) = fresh();
    let other = key(&journal, 24);
    // A second admission the failed write leaves untouched: a fresh
    // invocation the recovered journal can still run to its end.
    let running = key(&journal, 23);
    assert!(journal.admit(running.clone(), intent("draft"), 1).unwrap());
    assert!(journal.admit(other.clone(), intent("draft"), 2).unwrap());
    journal.unconfirmed_next = true;
    assert!(
        journal
            .update(&running, succeeded(&running, "maybe"), false)
            .is_err()
    );
    assert!(
        journal
            .update(&other, succeeded(&other, "x"), false)
            .is_err()
    );
    journal.recover().unwrap();
    assert!(
        journal.lookup(&other).unwrap().is_some(),
        "recovery keeps every admission already made"
    );
    let later = key(&journal, 25);
    assert!(
        journal.admit(later.clone(), intent("draft"), 3).unwrap(),
        "recovery leaves a writer that admits again"
    );
    journal
        .update(&later, succeeded(&later, "after"), false)
        .unwrap();
    let later_key = later.clone();
    assert_eq!(
        journal.lookup(&later).unwrap().unwrap().update,
        succeeded(&later, "after")
    );
    assert_eq!(journal.host(), journal.snapshot.host);
    drop(journal);
    let journal = InvocationJournal::open(&root).unwrap();
    let running = journal.lookup(&running).unwrap().unwrap();
    assert!(
        matches!(&running.update, InvocationUpdate::Succeeded { .. }),
        "recovery reads the store, and the store holds the terminal"
    );
    assert!(
        !running.uncertain,
        "a write the store holds whole is no longer uncertain"
    );
    let later = journal.lookup(&later_key).unwrap().unwrap();
    assert_eq!(later.update, succeeded(&later_key, "after"));
    assert!(
        !later.uncertain,
        "a transition made durable after recovery stands"
    );
}

#[test]
fn append_journal_next_transition_recovers_without_restart() {
    let (_dir, root, mut journal) = fresh();
    let k = key(&journal, 26);
    assert!(journal.admit(k.clone(), intent("draft"), 1).unwrap());
    journal.unconfirmed_next = true;
    assert!(journal.update(&k, succeeded(&k, "maybe"), false).is_err());
    assert!(journal.failed(), "the store may hold it");
    // No `recover` call and no restart: the next transition recovers the
    // journal it finds failed, then is staged on the recovered store.
    let later = key(&journal, 27);
    assert!(
        journal.admit(later.clone(), intent("draft"), 2).unwrap(),
        "the next admission recovers the journal and is made"
    );
    assert!(!journal.failed());
    assert_eq!(
        journal.lookup(&k).unwrap().unwrap().update,
        succeeded(&k, "maybe"),
        "recovery read back the terminal the store holds"
    );
    journal
        .update(&later, succeeded(&later, "after"), false)
        .unwrap();
    assert_eq!(
        journal.lookup(&later).unwrap().unwrap().update,
        succeeded(&later, "after")
    );
    drop(journal);
    let journal = InvocationJournal::open(&root).unwrap();
    assert_eq!(
        journal.lookup(&k).unwrap().unwrap().update,
        succeeded(&k, "maybe")
    );
    assert_eq!(
        journal.lookup(&later).unwrap().unwrap().update,
        succeeded(&later, "after")
    );
}
