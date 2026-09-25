pub(crate) use super::fixture_storage::*;
use super::*;
use crate::catalog::InternalRelease;
use crate::transport::SelectedManifest;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Seek, Write},
    path::Path,
    sync::atomic::AtomicBool,
};

fn selected() -> SelectedManifest {
    let bytes = crate::bundle::tests::bytes(&crate::bundle::tests::fixture());
    let release = InternalRelease::transport_fixture(
        &bytes,
        "https://code.podesta.ai/releases/manifest.json",
    );
    SelectedManifest::from_bytes(&release, bytes).unwrap()
}
#[test]
fn working_names_exclude_all_selected_final_names_and_unsafe_leaves() {
    let selected = selected();
    assert!(validate_leaf(&selected, ".qos-part-0123456789abcdef0123456789abcdef").is_ok());
    for leaf in [
        "../payload",
        "",
        ".qos-part-ABCDEF0123456789abcdef0123456789",
    ] {
        assert!(validate_leaf(&selected, leaf).is_err(), "accepted {leaf:?}");
    }
    for index in 0..2 {
        assert!(
            validate_leaf(
                &selected,
                selected.bundle().component(index).unwrap().filename
            )
            .is_err()
        );
    }
}

#[cfg(unix)]
#[test]
fn fresh_creation_has_durable_zero_before_cancelled_preparation() {
    let selected = selected();
    let dir = tempfile::tempdir().unwrap();
    let (mut provider, mut file) = FreshDisk::create(dir.path(), &selected, 0);
    let ownership = provider.disk.ownership();
    let cancel = AtomicBool::new(true); // cancellation after exclusive creation
    initialize_fresh(&selected, 0, &mut &mut provider, &mut file).unwrap();
    let mut session = Session::new(&selected, 0, 1, &mut provider).unwrap();
    assert!(session.prepare(&selected, &mut file, &cancel).is_err());
    drop(session);
    drop(file);
    drop(provider);
    assert!(cancel.load(std::sync::atomic::Ordering::Acquire));
    let records: Vec<_> = fs::read_dir(dir.path().join("journal")).unwrap().collect();
    assert_eq!(
        records.len(),
        1,
        "known fresh ownership must survive cancellation"
    );
    let (provider, mut file) = DiskProvider::reopen(dir.path(), ownership.clone());
    assert_eq!(file_id(&file), ownership.file);
    assert_eq!(file.metadata().unwrap().len(), 0);
    let mut session = Session::new(&selected, 0, 2, provider).unwrap();
    assert_eq!(
        session
            .prepare(&selected, &mut file, &AtomicBool::new(false))
            .unwrap()
            .committed(),
        0
    );
}
#[cfg(unix)]
#[test]
fn restart_rehashes_once_trims_only_uncommitted_tail_and_restores_cursor() {
    let s = selected();
    let dir = tempfile::tempdir().unwrap();
    let cancel = AtomicBool::new(false);
    let (p, mut f) = DiskProvider::new(dir.path());
    let own = p.ownership();
    {
        let mut session = Session::new(&s, 0, 1, p).unwrap();
        let mut prepared = session.prepare(&s, &mut f, &cancel).unwrap();
        prepared
            .observer
            .begin_attempt(prepared.id, prepared.file)
            .unwrap();
        prefix(&mut prepared, b"EEEE").unwrap();
        assert_eq!(prepared.file.stream_position().unwrap(), 4);
        prepared.file.write_all(b"uncommitted").unwrap();
    }
    drop(f);
    clear_reads();
    let (p, mut f) = DiskProvider::reopen(dir.path(), own);
    let mut session = Session::new(&s, 0, 2, p).unwrap();
    let prepared = session.prepare(&s, &mut f, &cancel).unwrap();
    assert_eq!(prepared.committed, 4);
    assert_eq!(prepared.file.metadata().unwrap().len(), 4);
    assert_eq!(prepared.file.stream_position().unwrap(), 4);
    assert_eq!(read_totals("restart"), (4, 4));
}
#[cfg(unix)]
#[test]
fn failed_successor_retains_previous_and_unknown_survivors_are_untouched() {
    for stage in [
        "record-create",
        "record-write",
        "record-flush",
        "record-publish",
        "lost-ack",
    ] {
        let s = selected();
        let dir = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let (p, mut f) = DiskProvider::new(dir.path());
        let own = p.ownership();
        {
            let mut session = Session::new(&s, 0, 1, p).unwrap();
            let mut prepared = session.prepare(&s, &mut f, &cancel).unwrap();
            prepared
                .observer
                .begin_attempt(prepared.id, prepared.file)
                .unwrap();
            prefix(&mut prepared, b"EEEE").unwrap();
            fail(Some(stage));
            assert!(prefix(&mut prepared, b"EEEE").is_err());
            fail(None);
            assert_eq!(prepared.committed, 4);
            assert!(prefix(&mut prepared, b"E").is_err());
        }
        drop(f);
        let before: Vec<_> = fs::read_dir(dir.path().join("journal"))
            .unwrap()
            .map(|e| {
                let p = e.unwrap().path();
                (p.clone(), fs::read(p).unwrap())
            })
            .collect();
        assert!(
            before
                .iter()
                .any(|(p, _)| p.file_name().unwrap() == "record-2")
        );
        let (p, mut f) = DiskProvider::reopen(dir.path(), own);
        let mut session = Session::new(&s, 0, 2, p).unwrap();
        let result = session.prepare(&s, &mut f, &cancel);
        if stage == "record-write" {
            assert!(result.is_err());
        } else {
            assert_eq!(
                result.unwrap().committed,
                if stage == "record-create" { 4 } else { 8 }
            );
        }
        for (p, b) in before {
            if p.file_name().unwrap() != "record-1"
                || stage == "record-create"
                || stage == "record-write"
            {
                assert_eq!(fs::read(p).unwrap(), b);
            }
        }
    }
}
#[cfg(unix)]
#[test]
fn reset_intent_recovery_never_revives_older_prefix_before_or_after_truncation() {
    for truncate in [false, true] {
        let s = selected();
        let dir = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let (p, mut f) = DiskProvider::new(dir.path());
        let own = p.ownership();
        {
            let mut session = Session::new(&s, 0, 1, p).unwrap();
            let mut prepared = session.prepare(&s, &mut f, &cancel).unwrap();
            prepared
                .observer
                .begin_attempt(prepared.id, prepared.file)
                .unwrap();
            prefix(&mut prepared, b"EEEE").unwrap();
            prepared
                .observer
                .boundary(
                    prepared.id,
                    prepared.file,
                    Candidate {
                        length: 0,
                        digest: Sha256::digest([]).into(),
                        kind: Kind::Reset,
                    },
                )
                .unwrap();
            if truncate {
                prepared.file.set_len(0).unwrap();
                prepared.file.sync_all().unwrap();
            }
        }
        drop(f);
        let (p, mut f) = DiskProvider::reopen(dir.path(), own);
        let mut session = Session::new(&s, 0, 2, p).unwrap();
        let prepared = session.prepare(&s, &mut f, &cancel).unwrap();
        assert_eq!(prepared.committed, 0);
        assert_eq!(prepared.file.metadata().unwrap().len(), 0);
    }
}
#[cfg(unix)]
#[test]
fn stale_operation_attempt_epoch_generation_and_identity_cannot_publish() {
    for case in 0..7 {
        let s = selected();
        let dir = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let (p, mut f) = DiskProvider::new(dir.path());
        let mut session = Session::new(&s, 0, 1, p).unwrap();
        let mut prepared = session.prepare(&s, &mut f, &cancel).unwrap();
        prepared
            .observer
            .begin_attempt(prepared.id, prepared.file)
            .unwrap();
        prefix(&mut prepared, b"EEEE").unwrap();
        let before: Vec<_> = fs::read_dir(dir.path().join("journal"))
            .unwrap()
            .map(|e| {
                let p = e.unwrap().path();
                (p.clone(), fs::read(p).unwrap())
            })
            .collect();
        let mut id = prepared.id;
        match case {
            0 => id.operation += 1,
            1 => id.component += 1,
            2 => id.attempt += 1,
            3 => id.epoch += 1,
            4 => id.generation -= 1,
            _ => {}
        }
        let candidate = Candidate {
            length: 0,
            digest: Sha256::digest([]).into(),
            kind: Kind::Reset,
        };
        let result = if case == 5 {
            let mut stranger = tempfile::tempfile().unwrap();
            prepared.observer.boundary(id, &mut stranger, candidate)
        } else if case == 6 {
            prepared
                .observer
                .begin_attempt(id, prepared.file)
                .map(|()| Ack { id })
        } else {
            prepared.observer.boundary(id, prepared.file, candidate)
        };
        assert!(result.is_err(), "case {case}");
        assert_eq!(prepared.file.metadata().unwrap().len(), 4);
        for (p, b) in &before {
            assert_eq!(fs::read(p).unwrap(), *b);
        }
        assert_eq!(
            fs::read_dir(dir.path().join("journal")).unwrap().count(),
            before.len()
        );
        // Valid transition succeeds in the same still-live session after refusal.
        prefix(&mut prepared, b"EEEE").unwrap();
        assert_eq!(prepared.committed, 8);
    }
}
#[test]
fn reserved_working_namespace_is_disjoint_from_both_authenticated_final_names() {
    for index in 0..2 {
        let mut value = crate::bundle::tests::fixture();
        value["components"][index]["filename"] =
            serde_json::json!(".qos-part-0123456789abcdef0123456789abcdef");
        let bytes = crate::bundle::tests::bytes(&value);
        let release = InternalRelease::transport_fixture(
            &bytes,
            "https://code.podesta.ai/releases/manifest.json",
        );
        assert!(SelectedManifest::from_bytes(&release, bytes).is_err());
        let normal = selected();
        assert!(validate_leaf(&normal, ".qos-part-0123456789abcdef0123456789abcdef").is_ok());
        assert!(validate_leaf(&normal, ".qos-part-fedcba9876543210fedcba9876543210").is_ok());
    }
}
#[cfg(unix)]
#[test]
fn periodic_readback_is_linear_and_detects_changed_disk_bytes() {
    let s = selected();
    let dir = tempfile::tempdir().unwrap();
    let cancel = AtomicBool::new(false);
    let (p, mut f) = DiskProvider::new(dir.path());
    let mut session = Session::new(&s, 0, 1, p).unwrap();
    let mut prepared = session.prepare(&s, &mut f, &cancel).unwrap();
    prepared
        .observer
        .begin_attempt(prepared.id, prepared.file)
        .unwrap();
    clear_reads();
    for _ in 0..10 {
        prefix(&mut prepared, &[b'E'; 100]).unwrap();
    }
    assert_eq!(read_totals("periodic"), (1000, 1000));
    prepared.file.write_all(b"BAD").unwrap();
    prepared.hashes.written(b"EEE");
    assert!(prepared.hashes.verify_tail(prepared.file, &cancel).is_err());
    assert_eq!(prepared.file.stream_position().unwrap(), 1003);
    assert_eq!(prepared.committed, 1000);
    assert_eq!(fs::read_dir(dir.path().join("journal")).unwrap().count(), 2);
}

fn rewrite_checksum(b: &mut [u8]) {
    let end = b.len() - 32;
    let h = Sha256::digest(&b[..end]);
    b[end..].copy_from_slice(&h);
}
#[cfg(unix)]
#[test]
fn malformed_unknown_stale_and_overflow_records_never_mutate_payload_or_survivors() {
    for case in 0..14 {
        let s = selected();
        let dir = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let (p, mut f) = DiskProvider::new(dir.path());
        let own = p.ownership();
        {
            let mut session = Session::new(&s, 0, 1, p).unwrap();
            let mut prepared = session.prepare(&s, &mut f, &cancel).unwrap();
            prepared
                .observer
                .begin_attempt(prepared.id, prepared.file)
                .unwrap();
            prefix(&mut prepared, b"EEEE").unwrap();
            prepared.file.write_all(b"TAIL").unwrap();
        }
        drop(f);
        let record = dir.path().join("journal/record-2");
        let mut b = fs::read(&record).unwrap();
        match case {
            0 => {
                b.pop();
            }
            1 => b.push(0),
            2 => b[8] = 99,
            3 => b[9] ^= 1,
            4 => b[41] ^= 1,
            5 => b[73] ^= 1,
            6 => b[105] = b'/',
            7 => b[147..155].copy_from_slice(&u64::MAX.to_le_bytes()),
            8 => b[155..163].copy_from_slice(&8u64.to_le_bytes()),
            9 => b[163..171].copy_from_slice(&4097u64.to_le_bytes()),
            10 => b[179] ^= 1,
            11 => b[0] = b'X',
            12 => {
                b.resize(4097, 0);
            }
            13 => {
                b[147..155].copy_from_slice(&1u64.to_le_bytes());
            }
            _ => unreachable!(),
        }
        if ![0, 1, 12].contains(&case) {
            rewrite_checksum(&mut b);
        }
        fs::write(&record, &b).unwrap();
        let before = fs::read(dir.path().join(&own.leaf)).unwrap();
        let (p, mut f) = DiskProvider::reopen(dir.path(), own.clone());
        let mut session = Session::new(&s, 0, 2, p).unwrap();
        assert!(session.prepare(&s, &mut f, &cancel).is_err(), "case {case}");
        assert_eq!(fs::read(&record).unwrap(), b);
        assert_eq!(fs::read(dir.path().join(&own.leaf)).unwrap(), before);
    }
}
#[cfg(unix)]
#[test]
fn corrupt_prefix_refuses_before_trimming_and_unknown_files_survive() {
    for unknown in [false, true] {
        let s = selected();
        let dir = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let (p, mut f) = DiskProvider::new(dir.path());
        let own = p.ownership();
        {
            let mut session = Session::new(&s, 0, 1, p).unwrap();
            let mut prepared = session.prepare(&s, &mut f, &cancel).unwrap();
            prepared
                .observer
                .begin_attempt(prepared.id, prepared.file)
                .unwrap();
            prefix(&mut prepared, b"EEEE").unwrap();
            prepared.file.write_all(b"TAIL").unwrap();
        }
        drop(f);
        if unknown {
            fs::write(dir.path().join("journal/unknown"), b"unowned").unwrap();
        } else {
            let mut f = OpenOptions::new()
                .write(true)
                .open(dir.path().join(&own.leaf))
                .unwrap();
            f.write_all(b"X").unwrap();
        }
        let before = fs::read(dir.path().join(&own.leaf)).unwrap();
        let (p, mut f) = DiskProvider::reopen(dir.path(), own.clone());
        let mut session = Session::new(&s, 0, 2, p).unwrap();
        assert!(session.prepare(&s, &mut f, &cancel).is_err());
        assert_eq!(fs::read(dir.path().join(&own.leaf)).unwrap(), before);
        if unknown {
            assert_eq!(
                fs::read(dir.path().join("journal/unknown")).unwrap(),
                b"unowned"
            );
        }
    }
}
#[cfg(unix)]
#[test]
fn data_flush_and_sync_failures_preserve_the_last_record_and_exact_tail() {
    for stage in ["data-flush", "data-sync"] {
        let s = selected();
        let dir = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let (p, mut f) = DiskProvider::new(dir.path());
        let own = p.ownership();
        {
            let mut session = Session::new(&s, 0, 1, p).unwrap();
            let mut prepared = session.prepare(&s, &mut f, &cancel).unwrap();
            prepared
                .observer
                .begin_attempt(prepared.id, prepared.file)
                .unwrap();
            prefix(&mut prepared, b"EEEE").unwrap();
            fail(Some(stage));
            assert!(prefix(&mut prepared, b"TAIL").is_err());
            fail(None);
            assert_eq!(prepared.committed, 4);
            assert_eq!(prepared.file.metadata().unwrap().len(), 8);
        }
        drop(f);
        let (p, mut f) = DiskProvider::reopen(dir.path(), own);
        let mut session = Session::new(&s, 0, 2, p).unwrap();
        assert_eq!(session.prepare(&s, &mut f, &cancel).unwrap().committed, 4);
    }
}

#[cfg(unix)]
#[test]
fn bounded_scan_conflicting_histories_and_generation_exhaustion_refuse_without_mutation() {
    for case in 0..4 {
        let s = selected();
        let dir = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let (p, mut f) = DiskProvider::new(dir.path());
        let own = p.ownership();
        {
            let mut session = Session::new(&s, 0, 1, p).unwrap();
            let mut prepared = session.prepare(&s, &mut f, &cancel).unwrap();
            prepared
                .observer
                .begin_attempt(prepared.id, prepared.file)
                .unwrap();
            prefix(&mut prepared, b"EEEE").unwrap();
        }
        drop(f);
        let latest = fs::read(dir.path().join("journal/record-2")).unwrap();
        if case == 0 {
            for i in 3..20 {
                fs::write(dir.path().join(format!("journal/record-{i}")), &latest).unwrap();
            }
        }
        if case == 1 {
            let mut b = latest.clone();
            b[147..155].copy_from_slice(&4u64.to_le_bytes());
            rewrite_checksum(&mut b);
            fs::write(dir.path().join("journal/record-2"), b).unwrap();
        }
        if case == 2 {
            let mut b = latest.clone();
            b[155..163].copy_from_slice(&1u64.to_le_bytes());
            rewrite_checksum(&mut b);
            fs::write(dir.path().join("journal/record-2"), b).unwrap();
        }
        if case == 3 {
            fs::remove_file(dir.path().join("journal/record-1")).unwrap();
            let mut b = latest.clone();
            b[147..155].copy_from_slice(&(u32::MAX as u64).to_le_bytes());
            rewrite_checksum(&mut b);
            fs::write(dir.path().join("journal/record-2"), b).unwrap();
        }
        let before: Vec<_> = fs::read_dir(dir.path().join("journal"))
            .unwrap()
            .map(|e| {
                let p = e.unwrap().path();
                (p.clone(), fs::read(p).unwrap())
            })
            .collect();
        let (p, mut f) = DiskProvider::reopen(dir.path(), own.clone());
        let mut session = Session::new(&s, 0, 2, p).unwrap();
        if case == 3 {
            let prepared = session.prepare(&s, &mut f, &cancel).unwrap();
            prepared
                .observer
                .begin_attempt(prepared.id, prepared.file)
                .unwrap();
            assert!(
                prepared
                    .observer
                    .boundary(
                        prepared.id,
                        prepared.file,
                        Candidate {
                            length: 0,
                            digest: Sha256::digest([]).into(),
                            kind: Kind::Reset
                        }
                    )
                    .is_err()
            );
        } else {
            assert!(session.prepare(&s, &mut f, &cancel).is_err());
        }
        assert_eq!(fs::read(dir.path().join(own.leaf)).unwrap(), b"EEEE");
        for (p, b) in before {
            assert_eq!(fs::read(p).unwrap(), b);
        }
    }
}
#[cfg(unix)]
#[test]
fn unknown_unrecorded_payload_is_never_adopted_or_trimmed() {
    let s = selected();
    let dir = tempfile::tempdir().unwrap();
    let (p, mut f) = DiskProvider::new(dir.path());
    f.write_all(b"UNKNOWN").unwrap();
    let mut session = Session::new(&s, 0, 1, p).unwrap();
    assert!(
        session
            .prepare(&s, &mut f, &AtomicBool::new(false))
            .is_err()
    );
    assert_eq!(f.metadata().unwrap().len(), 7);
    assert_eq!(fs::read_dir(dir.path().join("journal")).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn surviving_previous_digest_is_checked_in_the_same_restart_pass() {
    let s = selected();
    let dir = tempfile::tempdir().unwrap();
    let cancel = AtomicBool::new(false);
    let (p, mut f) = DiskProvider::new(dir.path());
    let own = p.ownership();
    {
        let mut session = Session::new(&s, 0, 1, p).unwrap();
        let mut prepared = session.prepare(&s, &mut f, &cancel).unwrap();
        prepared
            .observer
            .begin_attempt(prepared.id, prepared.file)
            .unwrap();
        prefix(&mut prepared, b"EEEE").unwrap();
        prefix(&mut prepared, b"EEEE").unwrap();
        prepared.file.write_all(b"TAIL").unwrap();
    }
    drop(f);
    let previous = dir.path().join("journal/record-2");
    let original = fs::read(&previous).unwrap();
    let mut corrupt = original.clone();
    corrupt[179] ^= 1;
    rewrite_checksum(&mut corrupt);
    fs::write(&previous, &corrupt).unwrap();
    let (p, mut f) = DiskProvider::reopen(dir.path(), own.clone());
    let mut session = Session::new(&s, 0, 2, p).unwrap();
    assert!(session.prepare(&s, &mut f, &cancel).is_err());
    assert_eq!(f.metadata().unwrap().len(), 12);
    drop(session);
    drop(f);
    fs::write(&previous, &original).unwrap();
    clear_reads();
    let (p, mut f) = DiskProvider::reopen(dir.path(), own);
    let mut session = Session::new(&s, 0, 3, p).unwrap();
    assert_eq!(session.prepare(&s, &mut f, &cancel).unwrap().committed, 8);
    assert_eq!(read_totals("restart"), (8, 8));
}

#[cfg(unix)]
#[test]
fn duplicate_prefix_transition_refuses_without_consuming_generation() {
    let s = selected();
    let dir = tempfile::tempdir().unwrap();
    let (p, mut f) = DiskProvider::new(dir.path());
    let mut session = Session::new(&s, 0, 1, p).unwrap();
    let mut prepared = session
        .prepare(&s, &mut f, &AtomicBool::new(false))
        .unwrap();
    prepared
        .observer
        .begin_attempt(prepared.id, prepared.file)
        .unwrap();
    prefix(&mut prepared, b"EEEE").unwrap();
    let id = prepared.id;
    assert!(
        prepared
            .observer
            .boundary(
                id,
                prepared.file,
                Candidate {
                    length: 4,
                    digest: prepared.hashes.digest(),
                    kind: Kind::Prefix
                }
            )
            .is_err()
    );
    assert_eq!(fs::read_dir(dir.path().join("journal")).unwrap().count(), 2);
    prefix(&mut prepared, b"E").unwrap();
    assert_eq!(prepared.id.generation, id.generation + 1);
}

#[test]
fn fixed_record_parser_has_platform_independent_bounded_positive_and_negative_controls() {
    use super::checkpoint::Record;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"QOSCP001");
    bytes.push(1);
    bytes.extend_from_slice(&[0; 96]);
    bytes.extend_from_slice(b".qos-part-0123456789abcdef0123456789abcdef");
    for value in [1u64, 0, 0, 4096] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes.extend_from_slice(&Sha256::digest([]));
    let checksum = Sha256::digest(&bytes);
    bytes.extend_from_slice(&checksum);
    assert!(Record::decode(&bytes).is_ok());
    for case in 0..11 {
        let mut bad = bytes.clone();
        match case {
            0 => {
                bad.pop();
            }
            1 => bad.push(0),
            2 => bad[8] = 0,
            3 => bad[0] ^= 1,
            4 => bad[211] ^= 1,
            5 => bad[147..155].copy_from_slice(&u64::MAX.to_le_bytes()),
            6 => bad[163..171].copy_from_slice(&4097u64.to_le_bytes()),
            7 => bad.resize(4097, 0),
            8 => bad[8] = 2, // reset cannot have epoch zero
            9 => {
                bad[147..155].copy_from_slice(&2u64.to_le_bytes());
                bad[155..163].copy_from_slice(&2u64.to_le_bytes());
                bad[163..171].copy_from_slice(&4u64.to_le_bytes());
            }
            10 => bad[163..171].copy_from_slice(&4u64.to_le_bytes()), // generation one is the initial zero record
            _ => unreachable!(),
        }
        if [2, 3, 5, 6, 8, 9, 10].contains(&case) {
            rewrite_checksum(&mut bad);
        }
        assert!(Record::decode(&bad).is_err(), "case {case}");
    }
}

#[cfg(unix)]
pub(crate) fn start_prefix(prepared: &mut PreparedPrefix<'_>, bytes: &[u8]) {
    prepared
        .observer
        .begin_attempt(prepared.id, prepared.file)
        .unwrap();
    prefix(prepared, bytes).unwrap();
}
#[cfg(unix)]
pub(crate) fn test_prepared(parts: TransferParts<'_>) -> PreparedPrefix<'_> {
    let TransferParts {
        selected,
        index,
        file,
        observer,
        id,
        hashes,
        committed,
        final_candidate,
    } = parts;
    PreparedPrefix {
        selected,
        index,
        file,
        observer,
        id,
        hashes,
        committed,
        final_candidate,
    }
}

fn standalone_epoch_record(kind: u8, generation: u64, epoch: u64) -> Vec<u8> {
    let length = if kind == 3 { 4096u64 } else { 4 };
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"QOSCP001");
    bytes.push(kind);
    bytes.extend_from_slice(&[0; 96]);
    bytes.extend_from_slice(b".qos-part-0123456789abcdef0123456789abcdef");
    for value in [generation, epoch, length, 4096] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes.extend_from_slice(&Sha256::digest(vec![b'E'; length as usize]));
    let checksum = Sha256::digest(&bytes);
    bytes.extend_from_slice(&checksum);
    bytes
}
#[test]
fn impossible_nonreset_epoch_is_rejected_without_a_predecessor() {
    use super::checkpoint::Record;
    for kind in [1, 3] {
        for (generation, epoch) in [(3, 1), (4, 2), (u32::MAX as u64, u32::MAX as u64 - 2)] {
            assert!(Record::decode(&standalone_epoch_record(kind, generation, epoch)).is_ok());
        }
        for (generation, epoch) in [(2, 1), (3, 2), (u32::MAX as u64, u32::MAX as u64 - 1)] {
            assert!(
                Record::decode(&standalone_epoch_record(kind, generation, epoch)).is_err(),
                "kind={kind}, generation={generation}, epoch={epoch}"
            );
        }
    }
}
#[cfg(unix)]
#[test]
fn impossible_nonreset_epoch_refuses_recovery_before_any_tail_trim() {
    for kind in [1, 3] {
        for generation in [3u64, 2] {
            let s = selected();
            let dir = tempfile::tempdir().unwrap();
            let (p, mut f) = DiskProvider::new(dir.path());
            let own = p.ownership();
            let cancel = AtomicBool::new(false);
            {
                let mut session = Session::new(&s, 0, 1, p).unwrap();
                drop(session.prepare(&s, &mut f, &cancel).unwrap());
            }
            let length = if kind == 3 { 4096usize } else { 4 };
            let mut payload = vec![b'E'; length];
            if kind == 1 {
                payload.extend_from_slice(b"TAIL");
            }
            f.write_all(&payload).unwrap();
            f.sync_all().unwrap();
            drop(f);
            let path = dir.path().join("journal/record-1");
            let mut record = fs::read(&path).unwrap();
            let fields = standalone_epoch_record(kind, generation, 1);
            record[8] = kind;
            record[147..211].copy_from_slice(&fields[147..211]);
            rewrite_checksum(&mut record);
            fs::write(&path, &record).unwrap();
            assert_eq!(fs::read_dir(dir.path().join("journal")).unwrap().count(), 1);
            let (p, mut f) = DiskProvider::reopen(dir.path(), own.clone());
            let mut session = Session::new(&s, 0, 2, p).unwrap();
            if generation == 3 {
                assert_eq!(
                    session.prepare(&s, &mut f, &cancel).unwrap().committed(),
                    length as u64
                );
                assert_eq!(f.metadata().unwrap().len(), length as u64);
            } else {
                assert!(session.prepare(&s, &mut f, &cancel).is_err(), "kind={kind}");
                assert_eq!(fs::read(dir.path().join(&own.leaf)).unwrap(), payload);
                assert_eq!(fs::read(&path).unwrap(), record);
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn bootstrap_hint_checks_selection_size_and_leaf_without_minting_ownership() {
    use super::checkpoint::Record;
    let selected = selected();
    let dir = tempfile::tempdir().unwrap();
    let (provider, mut file) = DiskProvider::new(dir.path());
    let own = provider.ownership();
    let mut session = Session::new(&selected, 0, 1, provider).unwrap();
    drop(
        session
            .prepare(&selected, &mut file, &AtomicBool::new(false))
            .unwrap(),
    );
    let bytes = fs::read(dir.path().join("journal/record-1")).unwrap();
    let record = Record::decode(&bytes).unwrap();
    let hint = record.hint_for(&selected, 0).unwrap();
    assert_eq!(hint.leaf, own.leaf);
    assert_eq!(hint.parent_digest, &own.parent);
    assert_eq!(hint.file_digest, &own.file);
    assert_eq!(hint.generation, 1);
    assert!(record.hint_for(&selected, 1).is_err());
    assert!(record.hint_for(&selected, 2).is_err());
    for case in 0..3 {
        let mut bad = bytes.clone();
        match case {
            0 => bad[9] ^= 1,
            1 => bad[171..179].copy_from_slice(&4095u64.to_le_bytes()),
            2 => bad[105] = b'/',
            _ => unreachable!(),
        }
        rewrite_checksum(&mut bad);
        assert!(
            Record::decode(&bad)
                .unwrap()
                .hint_for(&selected, 0)
                .is_err(),
            "case {case}"
        );
    }
}

/// Ordinary-file lifecycle observer. No native security or Linux publication
/// behavior is substituted: this checks the real Session's callback ordering.
#[cfg(unix)]
struct LifecycleProvider<'a> {
    disk: DiskProvider,
    selected: &'a SelectedManifest,
    events: Vec<&'static str>,
    complete: bool,
    expected_length: u64,
    reject_receipt: bool,
}
#[cfg(unix)]
impl Provider for &mut LifecycleProvider<'_> {
    fn revoke_preparation(&mut self) {
        self.complete = false;
        self.events.push("revoke");
    }
    fn prepared(&mut self, proof: PreparationComplete<'_>) -> Result<(), crate::error::Failure> {
        assert!(proof.authorizes(self.selected, 0, &self.disk.own)?);
        assert!(!proof.authorizes(self.selected, 1, &self.disk.own)?);
        let payload = self.disk.dir.parent().unwrap().join(&self.disk.own.leaf);
        // Successful preparation must already have trimmed the uncommitted tail
        // and normalized any third survivor before issuing the receipt.
        assert_eq!(fs::metadata(payload).unwrap().len(), self.expected_length);
        assert!(fs::read_dir(&self.disk.dir).unwrap().count() <= 2);
        self.events.push("complete");
        self.complete = true;
        if self.reject_receipt {
            Err("receipt callback failure".into())
        } else {
            Ok(())
        }
    }
    fn ownership(&self) -> Ownership {
        self.disk.ownership()
    }
    fn verify(&mut self, file: &File, own: &Ownership) -> Result<(), crate::error::Failure> {
        self.disk.verify(file, own)
    }
    fn scan(
        &mut self,
        visit: &mut dyn FnMut(&[u8]) -> Result<(), crate::error::Failure>,
    ) -> Result<(), crate::error::Failure> {
        self.disk.scan(visit)?;
        self.events.push("scan");
        Ok(())
    }
    fn retain(&mut self, keep: &[u64]) -> Result<(), crate::error::Failure> {
        self.disk.retain(keep)?;
        self.events.push("retain");
        Ok(())
    }
    fn publish(&mut self, g: u64, bytes: &[u8], keep: &[u64]) -> Result<(), crate::error::Failure> {
        self.disk.publish(g, bytes, keep)
    }
}
#[cfg(unix)]
fn lifecycle_records(selected: &SelectedManifest, dir: &Path) -> (DiskProvider, File) {
    let (disk, mut file) = DiskProvider::new(dir);
    let own = disk.ownership();
    let mut session = Session::new(selected, 0, 1, disk).unwrap();
    let mut prepared = session
        .prepare(selected, &mut file, &AtomicBool::new(false))
        .unwrap();
    let zero = fs::read(dir.join("journal/record-1")).unwrap();
    start_prefix(&mut prepared, b"EE");
    prefix(&mut prepared, b"EE").unwrap();
    drop(prepared);
    drop(session);
    // Emulate the durable uncertain third successor that recovery normalizes.
    fs::write(dir.join("journal/record-1"), zero).unwrap();
    let (disk, mut file) = DiskProvider::reopen(dir, own);
    file.seek(std::io::SeekFrom::End(0)).unwrap();
    file.write_all(b"TAIL").unwrap();
    file.sync_all().unwrap();
    (disk, file)
}
#[cfg(unix)]
fn record_snapshot(dir: &Path) -> Vec<(std::ffi::OsString, Vec<u8>)> {
    let mut records: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (e.file_name(), fs::read(e.path()).unwrap())
        })
        .collect();
    records.sort();
    records
}
#[cfg(unix)]
#[test]
fn preparation_receipt_follows_real_history_hash_normalization_trim_and_seek() {
    let selected = selected();
    let dir = tempfile::tempdir().unwrap();
    let (disk, mut file) = lifecycle_records(&selected, dir.path());
    let mut provider = LifecycleProvider {
        disk,
        selected: &selected,
        events: vec![],
        complete: false,
        expected_length: 4,
        reject_receipt: false,
    };
    clear_reads();
    {
        let mut session = Session::new(&selected, 0, 2, &mut provider).unwrap();
        let prepared = session
            .prepare(&selected, &mut file, &AtomicBool::new(false))
            .unwrap();
        assert_eq!(prepared.committed(), 4);
        assert_eq!(prepared.file.stream_position().unwrap(), 4);
    }
    assert!(provider.complete);
    assert_eq!(
        provider.events,
        vec!["revoke", "revoke", "scan", "retain", "complete"]
    );
    assert_eq!(
        fs::read(dir.path().join(&provider.disk.own.leaf)).unwrap(),
        b"EEEE"
    );
    assert_eq!(fs::read_dir(&provider.disk.dir).unwrap().count(), 2);
    assert_eq!(read_totals("restart"), (4, 4));
    // Failed construction must revoke a prior success before it can return Err.
    assert!(Session::new(&selected, 0, 0, &mut provider).is_err());
    assert!(!provider.complete);
}
#[cfg(unix)]
#[test]
fn post_scan_preparation_failures_revoke_without_mutating_rejected_evidence() {
    for corrupt_prefix in [false, true] {
        let selected = selected();
        let dir = tempfile::tempdir().unwrap();
        let (disk, mut file) = lifecycle_records(&selected, dir.path());
        let mut provider = LifecycleProvider {
            disk,
            selected: &selected,
            events: vec![],
            complete: false,
            expected_length: 4,
            reject_receipt: false,
        };
        {
            let mut session = Session::new(&selected, 0, 2, &mut provider).unwrap();
            drop(
                session
                    .prepare(&selected, &mut file, &AtomicBool::new(false))
                    .unwrap(),
            );
        }
        assert!(provider.complete);
        let journal = dir.path().join("journal");
        if corrupt_prefix {
            let path = journal.join("record-3");
            let mut bytes = fs::read(&path).unwrap();
            bytes[179] ^= 1;
            rewrite_checksum(&mut bytes);
            fs::write(path, bytes).unwrap();
        } else {
            let path = journal.join("record-3");
            let mut bytes = fs::read(&path).unwrap();
            bytes[147..155].copy_from_slice(&4u64.to_le_bytes());
            rewrite_checksum(&mut bytes);
            fs::remove_file(path).unwrap();
            fs::write(journal.join("record-4"), bytes).unwrap();
        }
        file.seek(std::io::SeekFrom::End(0)).unwrap();
        file.write_all(b"TAIL").unwrap();
        let payload = fs::read(dir.path().join(&provider.disk.own.leaf)).unwrap();
        let records = record_snapshot(&journal);
        provider.events.clear();
        {
            let mut session = Session::new(&selected, 0, 3, &mut provider).unwrap();
            assert!(
                session
                    .prepare(&selected, &mut file, &AtomicBool::new(false))
                    .is_err()
            );
        }
        assert!(!provider.complete);
        assert_eq!(provider.events, vec!["revoke", "revoke", "scan", "revoke"]);
        assert_eq!(
            fs::read(dir.path().join(&provider.disk.own.leaf)).unwrap(),
            payload
        );
        assert_eq!(record_snapshot(&journal), records);
    }
}
#[cfg(unix)]
#[test]
fn failed_receipt_callback_and_cancelled_reprepare_do_not_leave_success() {
    let selected = selected();
    let dir = tempfile::tempdir().unwrap();
    let (disk, mut file) = lifecycle_records(&selected, dir.path());
    let mut provider = LifecycleProvider {
        disk,
        selected: &selected,
        events: vec![],
        complete: false,
        expected_length: 4,
        reject_receipt: true,
    };
    {
        let mut session = Session::new(&selected, 0, 2, &mut provider).unwrap();
        assert!(
            session
                .prepare(&selected, &mut file, &AtomicBool::new(false))
                .is_err()
        );
    }
    assert!(!provider.complete);
    assert!(provider.events.contains(&"complete"));
    assert_eq!(provider.events.last(), Some(&"revoke"));
    provider.reject_receipt = false;
    {
        let mut session = Session::new(&selected, 0, 3, &mut provider).unwrap();
        drop(
            session
                .prepare(&selected, &mut file, &AtomicBool::new(false))
                .unwrap(),
        );
        assert!(
            session
                .prepare(&selected, &mut file, &AtomicBool::new(true))
                .is_err()
        );
    }
    assert!(!provider.complete);
}

#[cfg(unix)]
#[test]
fn independent_fresh_settlements_preserve_cancellation_and_require_normal_preparation() {
    let selected = selected();
    let dirs = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
    let mut owners = Vec::new();
    for (index, dir) in dirs.iter().enumerate() {
        // Independent admitted creations; neither is created under a cancelled flag.
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let (mut p, mut f) = FreshDisk::create(dir.path(), &selected, index);
        p.cancel_during_publish = Some(cancel.clone());
        let own = p.disk.ownership();
        initialize_fresh(&selected, index, &mut &mut p, &mut f).unwrap();
        assert!(cancel.load(std::sync::atomic::Ordering::Acquire));
        let before = record_snapshot(&p.disk.dir);
        assert!((&mut p).retain(&[]).is_err());
        assert!(!p.prepared && !p.scanned && !p.poisoned);
        assert_eq!(record_snapshot(&p.disk.dir), before);
        let record = checkpoint::Record::decode(&before[0].1).unwrap();
        assert_eq!(record.hint_for(&selected, index).unwrap().generation, 1);
        assert_eq!(
            record.hint_for(&selected, index).unwrap().file_digest,
            &own.file
        );
        assert_eq!(&before[0].1[147..155], &1u64.to_le_bytes());
        assert_eq!(&before[0].1[155..171], &[0; 16]); // epoch and committed length
        let mut session = Session::new(&selected, index, index as u64 + 1, &mut p).unwrap();
        assert!(session.prepare(&selected, &mut f, &cancel).is_err());
        drop(session);
        assert!(!p.prepared && !p.scanned);
        let mut session = Session::new(&selected, index, index as u64 + 3, &mut p).unwrap();
        drop(
            session
                .prepare(&selected, &mut f, &AtomicBool::new(false))
                .unwrap(),
        );
        drop(session);
        assert!(p.prepared);
        assert_eq!(record_snapshot(&p.disk.dir), before);
        owners.push(own);
    }
    let again = self::selected();
    for (index, dir) in dirs.iter().enumerate() {
        let before = record_snapshot(&dir.path().join("journal"));
        let (p, mut f) = DiskProvider::reopen(dir.path(), owners[index].clone());
        assert_eq!(file_id(&f), owners[index].file);
        assert_eq!(fs::read(dir.path().join(&owners[index].leaf)).unwrap(), b"");
        let mut session = Session::new(&again, index, index as u64 + 10, p).unwrap();
        let mut ready = session
            .prepare(&again, &mut f, &AtomicBool::new(false))
            .unwrap();
        assert_eq!(ready.committed(), 0);
        assert_eq!(record_snapshot(&dir.path().join("journal")), before);
        start_prefix(&mut ready, b"EE");
        assert_eq!(ready.committed(), 2);
    }
}
#[cfg(unix)]
#[test]
fn fresh_settlement_refuses_wrong_selection_ownership_nonempty_history_and_reopened_empty() {
    let selected = selected();
    let mut changed_bytes = selected.bytes().to_vec();
    changed_bytes.push(b' ');
    let release = InternalRelease::transport_fixture(
        &changed_bytes,
        "https://code.podesta.ai/releases/manifest.json",
    );
    let changed = SelectedManifest::from_bytes(&release, changed_bytes).unwrap();
    for case in 0..10 {
        let dir = tempfile::tempdir().unwrap();
        let (mut p, mut f) = FreshDisk::create(dir.path(), &selected, 0);
        let mut index = 0;
        match case {
            0 => index = 1,
            1 => p.fresh.as_mut().unwrap().file[0] ^= 1,
            2 => {
                f.write_all(b"unknown").unwrap();
                f.sync_all().unwrap();
            }
            3 => fs::write(p.disk.dir.join("unknown"), b"unknown").unwrap(),
            4 => {
                let own = p.disk.ownership();
                drop(f);
                let (disk, reopened) = DiskProvider::reopen(dir.path(), own);
                p.disk = disk;
                p.fresh = None;
                f = reopened;
            }
            5 => {
                initialize_fresh(&selected, 0, &mut &mut p, &mut f).unwrap();
            }
            6 => {
                p.fresh.as_mut().unwrap().parent[0] ^= 1;
            }
            7 => {}
            8 => {
                f = tempfile::tempfile().unwrap();
            }
            9 => {
                let donor = tempfile::tempdir().unwrap();
                let (mut other, mut other_file) = FreshDisk::create(donor.path(), &selected, 0);
                initialize_fresh(&selected, 0, &mut &mut other, &mut other_file).unwrap();
                fs::copy(other.disk.dir.join("record-1"), p.disk.dir.join("record-1")).unwrap();
            }
            _ => unreachable!(),
        }
        let before = record_snapshot(&p.disk.dir);
        let payload = fs::read(dir.path().join(&p.disk.own.leaf)).unwrap();
        assert!(
            initialize_fresh(
                if case == 7 { &changed } else { &selected },
                index,
                &mut &mut p,
                &mut f
            )
            .is_err()
        );
        assert!(p.poisoned && p.fresh.is_none() && !p.scanned && !p.prepared);
        assert!(initialize_fresh(&selected, 0, &mut &mut p, &mut f).is_err());
        assert_eq!(record_snapshot(&p.disk.dir), before);
        assert_eq!(
            fs::read(dir.path().join(&p.disk.own.leaf)).unwrap(),
            payload
        );
        let mut session = Session::new(&selected, 0, 99, &mut p).unwrap();
        assert!(
            session
                .prepare(&selected, &mut f, &AtomicBool::new(false))
                .is_err()
        );
    }
}
#[cfg(unix)]
#[test]
fn fresh_settlement_uncertain_flush_and_journal_failures_preserve_evidence_and_poison() {
    let selected = selected();
    for stage in [
        "initialize-flush",
        "record-create",
        "record-write",
        "record-flush",
        "record-publish",
        "lost-ack",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (mut p, mut f) = FreshDisk::create(dir.path(), &selected, 0);
        fail(Some(stage));
        let result = initialize_fresh(&selected, 0, &mut &mut p, &mut f);
        fail(None);
        assert!(result.is_err(), "{stage}");
        assert!(p.poisoned && p.fresh.is_none() && !p.scanned && !p.prepared);
        let before = record_snapshot(&p.disk.dir);
        assert_eq!(
            before.is_empty(),
            matches!(stage, "initialize-flush" | "record-create")
        );
        assert!(initialize_fresh(&selected, 0, &mut &mut p, &mut f).is_err());
        assert!((&mut p).retain(&[]).is_err());
        assert_eq!(record_snapshot(&p.disk.dir), before);
        assert_eq!(fs::read(dir.path().join(&p.disk.own.leaf)).unwrap(), b"");
        if stage == "record-write" {
            assert!(checkpoint::Record::decode(&before[0].1).is_err());
        } else if stage == "lost-ack" {
            // Disk state may be complete despite an error; only independent
            // reopen/recovery may accept it, never this poisoned owner.
            let own = p.disk.ownership();
            drop(p);
            drop(f);
            let (p, mut f) = DiskProvider::reopen(dir.path(), own);
            let mut session = Session::new(&selected, 0, 42, p).unwrap();
            assert_eq!(
                session
                    .prepare(&selected, &mut f, &AtomicBool::new(false))
                    .unwrap()
                    .committed(),
                0
            );
        }
    }
}
