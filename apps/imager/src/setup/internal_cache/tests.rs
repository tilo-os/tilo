use super::super::internal_download::SelectedManifest;
use super::*;
use crate::release::InternalRelease;
use crate::setup::test_support::isolated;

fn selected() -> SelectedManifest {
    let bytes = super::super::internal_bundle::tests::bytes(
        &super::super::internal_bundle::tests::fixture(),
    );
    let release =
        InternalRelease::transport_fixture(&bytes, "https://code.tilo.so/releases/manifest.json");
    SelectedManifest::from_bytes(&release, bytes).unwrap()
}
/// Working namespace, record parser and impossible epochs.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn working_names_and_fixed_records_are_bounded_and_disjoint() {
    isolated(
        "working_names_exclude_all_selected_final_names_and_unsafe_leaves",
        || {
            let selected = selected();
            assert!(
                validate_leaf(&selected, ".tilo-part-0123456789abcdef0123456789abcdef").is_ok()
            );
            // Whatever a provider mints must pass the check it is held to.
            for name in [0, 1, u128::MAX, 0x0123_4567_89ab_cdef_0123_4567_89ab_cdef] {
                let leaf = working_leaf(name);
                assert!(validate_leaf(&selected, &leaf).is_ok(), "refused {leaf:?}");
            }
            for leaf in [
                "../payload",
                "",
                ".tilo-part-ABCDEF0123456789abcdef0123456789",
                ".tilo-part-0123456789abcdef0123456789abcde",
                ".tilo-part-0123456789abcdef0123456789abcdef0",
                // Right length, wrong prefix.
                ".tilo-pack-0123456789abcdef0123456789abcdef",
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
        },
    );

    isolated(
        "reserved_working_namespace_is_disjoint_from_both_authenticated_final_names",
        || {
            for index in 0..2 {
                let mut value = super::super::internal_bundle::tests::fixture();
                value["components"][index]["filename"] =
                    serde_json::json!(".tilo-part-0123456789abcdef0123456789abcdef");
                let bytes = super::super::internal_bundle::tests::bytes(&value);
                let release = InternalRelease::transport_fixture(
                    &bytes,
                    "https://code.tilo.so/releases/manifest.json",
                );
                assert!(SelectedManifest::from_bytes(&release, bytes).is_err());
                let normal = selected();
                assert!(
                    validate_leaf(&normal, ".tilo-part-0123456789abcdef0123456789abcdef").is_ok()
                );
                assert!(
                    validate_leaf(&normal, ".tilo-part-fedcba9876543210fedcba9876543210").is_ok()
                );
            }
        },
    );

    isolated(
        "fixed_record_parser_has_platform_independent_bounded_positive_and_negative_controls",
        || {
            use super::checkpoint::Record;
            let mut bytes = Vec::new();
            bytes.extend_from_slice(b"TILOCP01");
            bytes.push(1);
            bytes.extend_from_slice(&[0; 96]);
            bytes.extend_from_slice(b".tilo-part-0123456789abcdef0123456789abcdef");
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
                    2 => bad[layout::KIND] = 0,
                    3 => bad[0] ^= 1,
                    4 => bad[layout::CHECKSUM] ^= 1,
                    5 => bad[layout::GENERATION..layout::EPOCH]
                        .copy_from_slice(&u64::MAX.to_le_bytes()),
                    6 => bad[layout::LENGTH..layout::FULL_SIZE]
                        .copy_from_slice(&4097u64.to_le_bytes()),
                    7 => bad.resize(4097, 0),
                    8 => bad[layout::KIND] = 2, // reset cannot have epoch zero
                    9 => {
                        bad[layout::GENERATION..layout::EPOCH].copy_from_slice(&2u64.to_le_bytes());
                        bad[layout::EPOCH..layout::LENGTH].copy_from_slice(&2u64.to_le_bytes());
                        bad[layout::LENGTH..layout::FULL_SIZE].copy_from_slice(&4u64.to_le_bytes());
                    }
                    10 => {
                        bad[layout::LENGTH..layout::FULL_SIZE].copy_from_slice(&4u64.to_le_bytes())
                    } // generation one is the initial zero record
                    _ => unreachable!(),
                }
                if [2, 3, 5, 6, 8, 9, 10].contains(&case) {
                    rewrite_checksum(&mut bad);
                }
                assert!(Record::decode(&bad).is_err(), "case {case}");
            }
        },
    );

    isolated(
        "impossible_nonreset_epoch_is_rejected_without_a_predecessor",
        || {
            use super::checkpoint::Record;
            for kind in [1, 3] {
                for (generation, epoch) in [(3, 1), (4, 2), (u32::MAX as u64, u32::MAX as u64 - 2)]
                {
                    assert!(
                        Record::decode(&standalone_epoch_record(kind, generation, epoch)).is_ok()
                    );
                }
                for (generation, epoch) in [(2, 1), (3, 2), (u32::MAX as u64, u32::MAX as u64 - 1)]
                {
                    assert!(
                        Record::decode(&standalone_epoch_record(kind, generation, epoch)).is_err(),
                        "kind={kind}, generation={generation}, epoch={epoch}"
                    );
                }
            }
        },
    );
}

use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, Write},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};
thread_local! {
    static FAILURE: RefCell<Option<&'static str>> = const { RefCell::new(None) };
    static READS: RefCell<Vec<(&'static str, u64, u64)>> = const { RefCell::new(Vec::new()) };
}
pub(super) fn fault(stage: &'static str) -> Result<(), String> {
    FAILURE.with(|f| {
        if *f.borrow() == Some(stage) {
            Err(format!("injected {stage}"))
        } else {
            Ok(())
        }
    })
}
pub(in crate::setup) fn read_event(phase: &'static str, requested: u64, actual: u64) {
    READS.with(|r| r.borrow_mut().push((phase, requested, actual)));
}
pub(in crate::setup) fn fail(stage: Option<&'static str>) {
    FAILURE.with(|f| *f.borrow_mut() = stage);
}
pub(in crate::setup) fn read_totals(phase: &'static str) -> (u64, u64) {
    READS.with(|r| {
        r.borrow()
            .iter()
            .filter(|r| r.0 == phase)
            .fold((0, 0), |(a, b), r| (a + r.1, b + r.2))
    })
}
pub(in crate::setup) fn clear_reads() {
    READS.with(|r| r.borrow_mut().clear());
}
#[cfg(unix)]
fn file_id(file: &File) -> [u8; 32] {
    use std::os::unix::fs::MetadataExt;
    let m = file.metadata().unwrap();
    let mut h = Sha256::new();
    h.update(m.dev().to_le_bytes());
    h.update(m.ino().to_le_bytes());
    h.finalize().into()
}

/// Fresh creation, restart trim, failed successors and reset intent.
#[test]
#[cfg(unix)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn creation_and_restart_keep_only_committed_prefixes() {
    isolated(
        "fresh_creation_has_durable_zero_before_cancelled_preparation",
        || {
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
        },
    );

    isolated(
        "restart_rehashes_once_trims_only_uncommitted_tail_and_restores_cursor",
        || {
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
        },
    );

    isolated(
        "failed_successor_retains_previous_and_unknown_survivors_are_untouched",
        || {
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
        },
    );

    isolated(
        "reset_intent_recovery_never_revives_older_prefix_before_or_after_truncation",
        || {
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
        },
    );
}
/// Host fixture only: real separate journal parent, held identity, exclusive
/// ordinary record files, record/file/parent syncs. Not a native security provider.
#[cfg(unix)]
pub(in crate::setup) struct DiskProvider {
    dir: PathBuf,
    own: Ownership,
}
#[cfg(unix)]
impl DiskProvider {
    pub fn new(root: &Path) -> (Self, File) {
        fs::create_dir(root.join("journal")).unwrap();
        let leaf = ".tilo-part-0123456789abcdef0123456789abcdef";
        let file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(root.join(leaf))
            .unwrap();
        let parent = File::open(root).unwrap();
        (
            Self {
                dir: root.join("journal"),
                own: Ownership {
                    parent: file_id(&parent),
                    file: file_id(&file),
                    leaf: leaf.into(),
                },
            },
            file,
        )
    }
    pub fn reopen(root: &Path, own: Ownership) -> (Self, File) {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(root.join(&own.leaf))
            .unwrap();
        (
            Self {
                dir: root.join("journal"),
                own,
            },
            file,
        )
    }
}
#[cfg(unix)]
impl Provider for DiskProvider {
    fn ownership(&self) -> Ownership {
        self.own.clone()
    }
    fn verify(&mut self, file: &File, own: &Ownership) -> Result<(), String> {
        if own != &self.own
            || file_id(file) != own.file
            || file_id(&File::open(self.dir.parent().unwrap()).map_err(|e| e.to_string())?)
                != own.parent
        {
            return Err("Held identity mismatch".into());
        }
        Ok(())
    }
    fn scan(&mut self, visit: &mut dyn FnMut(&[u8]) -> Result<(), String>) -> Result<(), String> {
        for (n, entry) in fs::read_dir(&self.dir)
            .map_err(|e| e.to_string())?
            .enumerate()
        {
            if n >= MAX_SCAN {
                return Err("scan bound".into());
            }
            let mut file =
                File::open(entry.map_err(|e| e.to_string())?.path()).map_err(|e| e.to_string())?;
            if file.metadata().unwrap().len() > MAX_RECORD as u64 {
                return Err("record bound".into());
            }
            let mut b = Vec::new();
            file.read_to_end(&mut b).map_err(|e| e.to_string())?;
            visit(&b)?;
        }
        Ok(())
    }
    fn retain(&mut self, keep: &[u64]) -> Result<(), String> {
        for entry in fs::read_dir(&self.dir).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name();
            let name = name.to_str().unwrap();
            let n = name
                .strip_prefix("record-")
                .or_else(|| name.strip_prefix("pending-"))
                .and_then(|n| n.parse::<u64>().ok())
                .ok_or("unknown record leaf")?;
            if !keep.contains(&n) {
                fs::remove_file(entry.path()).map_err(|e| e.to_string())?;
            }
        }
        File::open(&self.dir)
            .unwrap()
            .sync_all()
            .map_err(|e| e.to_string())
    }
    fn publish(&mut self, generation: u64, bytes: &[u8], keep: &[u64]) -> Result<(), String> {
        fault("record-create")?;
        let temp = self.dir.join(format!("pending-{generation}"));
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|e| e.to_string())?;
        let middle = bytes.len() / 2;
        f.write_all(&bytes[..middle]).map_err(|e| e.to_string())?;
        fault("record-write")?;
        f.write_all(&bytes[middle..]).map_err(|e| e.to_string())?;
        f.flush().map_err(|e| e.to_string())?;
        fault("record-flush")?;
        f.sync_all().map_err(|e| e.to_string())?;
        fault("record-publish")?;
        fs::hard_link(&temp, self.dir.join(format!("record-{generation}")))
            .map_err(|e| e.to_string())?;
        fs::remove_file(temp).map_err(|e| e.to_string())?;
        File::open(&self.dir)
            .unwrap()
            .sync_all()
            .map_err(|e| e.to_string())?;
        fault("lost-ack")?;
        for entry in fs::read_dir(&self.dir).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name();
            let name = name.to_str().unwrap();
            if let Some(n) = name
                .strip_prefix("record-")
                .and_then(|n| n.parse::<u64>().ok())
            {
                if !keep.contains(&n) {
                    fs::remove_file(entry.path()).unwrap();
                }
            }
        }
        File::open(&self.dir)
            .unwrap()
            .sync_all()
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}
#[cfg(unix)]
pub(in crate::setup) fn prefix(
    prepared: &mut PreparedPrefix<'_>,
    bytes: &[u8],
) -> Result<(), String> {
    prepared.file.write_all(bytes).unwrap();
    prepared.hashes.written(bytes);
    prepared
        .hashes
        .verify_tail(prepared.file, &AtomicBool::new(false))?;
    let ack = prepared.observer.boundary(
        prepared.id,
        prepared.file,
        Candidate {
            length: prepared.hashes.length,
            digest: prepared.hashes.digest(),
            kind: Kind::Prefix,
        },
    )?;
    prepared.id = ack.id;
    prepared.committed = prepared.hashes.length;
    Ok(())
}
/// Stale attempts, malformed records, corrupt prefixes and unknown payloads.
#[test]
#[cfg(unix)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn stale_malformed_or_unknown_state_never_mutates_payload() {
    isolated(
        "stale_operation_attempt_epoch_generation_and_identity_cannot_publish",
        || {
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
        },
    );

    isolated(
        "malformed_unknown_stale_and_overflow_records_never_mutate_payload_or_survivors",
        || {
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
                    2 => b[layout::KIND] = 99,
                    3 => b[layout::BINDING] ^= 1,
                    4 => b[layout::PARENT] ^= 1,
                    5 => b[layout::FILE] ^= 1,
                    6 => b[layout::LEAF] = b'/',
                    7 => b[layout::GENERATION..layout::EPOCH]
                        .copy_from_slice(&u64::MAX.to_le_bytes()),
                    8 => b[layout::EPOCH..layout::LENGTH].copy_from_slice(&8u64.to_le_bytes()),
                    9 => {
                        b[layout::LENGTH..layout::FULL_SIZE].copy_from_slice(&4097u64.to_le_bytes())
                    }
                    10 => b[layout::DIGEST] ^= 1,
                    11 => b[0] = b'X',
                    12 => {
                        b.resize(4097, 0);
                    }
                    13 => {
                        b[layout::GENERATION..layout::EPOCH].copy_from_slice(&1u64.to_le_bytes());
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
        },
    );

    isolated(
        "corrupt_prefix_refuses_before_trimming_and_unknown_files_survive",
        || {
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
        },
    );

    isolated(
        "unknown_unrecorded_payload_is_never_adopted_or_trimmed",
        || {
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
        },
    );
}
/// Periodic readback, flush failures and previous digest checks.
#[test]
#[cfg(unix)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn readback_and_io_failures_preserve_the_last_record() {
    isolated(
        "periodic_readback_is_linear_and_detects_changed_disk_bytes",
        || {
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
        },
    );

    isolated(
        "data_flush_and_sync_failures_preserve_the_last_record_and_exact_tail",
        || {
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
        },
    );

    isolated(
        "surviving_previous_digest_is_checked_in_the_same_restart_pass",
        || {
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
            corrupt[layout::DIGEST] ^= 1;
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
        },
    );
}

fn rewrite_checksum(b: &mut [u8]) {
    let end = b.len() - 32;
    let h = Sha256::digest(&b[..end]);
    b[end..].copy_from_slice(&h);
}
/// Bounded scans, duplicate transitions and impossible epochs on recovery.
#[test]
#[cfg(unix)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn conflicting_histories_and_transitions_refuse_without_mutation() {
    isolated(
        "duplicate_prefix_transition_refuses_without_consuming_generation",
        || {
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
        },
    );

    isolated(
        "bounded_scan_conflicting_histories_and_generation_exhaustion_refuse_without_mutation",
        || {
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
                    b[layout::GENERATION..layout::EPOCH].copy_from_slice(&4u64.to_le_bytes());
                    rewrite_checksum(&mut b);
                    fs::write(dir.path().join("journal/record-2"), b).unwrap();
                }
                if case == 2 {
                    let mut b = latest.clone();
                    b[layout::EPOCH..layout::LENGTH].copy_from_slice(&1u64.to_le_bytes());
                    rewrite_checksum(&mut b);
                    fs::write(dir.path().join("journal/record-2"), b).unwrap();
                }
                if case == 3 {
                    fs::remove_file(dir.path().join("journal/record-1")).unwrap();
                    let mut b = latest.clone();
                    b[layout::GENERATION..layout::EPOCH]
                        .copy_from_slice(&(u32::MAX as u64).to_le_bytes());
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
        },
    );

    isolated(
        "impossible_nonreset_epoch_refuses_recovery_before_any_tail_trim",
        || {
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
                    record[layout::KIND] = kind;
                    record[layout::GENERATION..layout::CHECKSUM]
                        .copy_from_slice(&fields[layout::GENERATION..layout::CHECKSUM]);
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
        },
    );
}
#[cfg(unix)]
pub(in crate::setup) fn start_prefix(prepared: &mut PreparedPrefix<'_>, bytes: &[u8]) {
    prepared
        .observer
        .begin_attempt(prepared.id, prepared.file)
        .unwrap();
    prefix(prepared, bytes).unwrap();
}
#[cfg(unix)]
pub(in crate::setup) fn test_prepared(parts: TransferParts<'_>) -> PreparedPrefix<'_> {
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
    bytes.extend_from_slice(b"TILOCP01");
    bytes.push(kind);
    bytes.extend_from_slice(&[0; 96]);
    bytes.extend_from_slice(b".tilo-part-0123456789abcdef0123456789abcdef");
    for value in [generation, epoch, length, 4096] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes.extend_from_slice(&Sha256::digest(vec![b'E'; length as usize]));
    let checksum = Sha256::digest(&bytes);
    bytes.extend_from_slice(&checksum);
    bytes
}
/// Bootstrap hints, preparation receipts and failures.
#[test]
#[cfg(unix)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn preparation_receipts_follow_real_history_and_revoke_on_failure() {
    isolated(
        "bootstrap_hint_checks_selection_size_and_leaf_without_minting_ownership",
        || {
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
                    0 => bad[layout::BINDING] ^= 1,
                    1 => bad[layout::FULL_SIZE..layout::DIGEST]
                        .copy_from_slice(&4095u64.to_le_bytes()),
                    2 => bad[layout::LEAF] = b'/',
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
        },
    );

    isolated(
        "preparation_receipt_follows_real_history_hash_normalization_trim_and_seek",
        || {
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
        },
    );

    isolated(
        "post_scan_preparation_failures_revoke_without_mutating_rejected_evidence",
        || {
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
                let path = journal.join("record-3");
                let mut bytes = fs::read(&path).unwrap();
                if corrupt_prefix {
                    bytes[layout::DIGEST] ^= 1;
                    rewrite_checksum(&mut bytes);
                    fs::write(path, bytes).unwrap();
                } else {
                    bytes[layout::GENERATION..layout::EPOCH].copy_from_slice(&4u64.to_le_bytes());
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
        },
    );

    isolated(
        "failed_receipt_callback_and_cancelled_reprepare_do_not_leave_success",
        || {
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
        },
    );
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
    fn prepared(&mut self, proof: PreparationComplete<'_>) -> Result<(), String> {
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
    fn verify(&mut self, file: &File, own: &Ownership) -> Result<(), String> {
        self.disk.verify(file, own)
    }
    fn scan(&mut self, visit: &mut dyn FnMut(&[u8]) -> Result<(), String>) -> Result<(), String> {
        self.disk.scan(visit)?;
        self.events.push("scan");
        Ok(())
    }
    fn retain(&mut self, keep: &[u64]) -> Result<(), String> {
        self.disk.retain(keep)?;
        self.events.push("retain");
        Ok(())
    }
    fn publish(&mut self, g: u64, bytes: &[u8], keep: &[u64]) -> Result<(), String> {
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
// Scoped host fixture hook at the actual final preparation receipt, after
// Session's real record normalization, tail trimming and cursor restoration.
#[cfg(unix)]
type PreparationHook = Box<dyn FnMut(usize) -> Result<(), String>>;
#[cfg(unix)]
thread_local! {
    static PREPARATION_HOOK: RefCell<Option<PreparationHook>> = const { RefCell::new(None) };
}
#[cfg(unix)]
pub(in crate::setup) fn with_preparation_hook(hook: PreparationHook, run: impl FnOnce()) {
    struct Reset(Option<PreparationHook>);
    impl Drop for Reset {
        fn drop(&mut self) {
            PREPARATION_HOOK.with(|h| *h.borrow_mut() = self.0.take());
        }
    }
    let _reset = Reset(PREPARATION_HOOK.with(|h| h.replace(Some(hook))));
    run();
}

/// Real-file common-protocol fixture. Freshness is minted only by create_new;
/// this models provider policy, not Linux security, locking or publication.
#[cfg(unix)]
pub(in crate::setup) struct FreshDisk<'a> {
    disk: DiskProvider,
    selected: &'a SelectedManifest,
    index: usize,
    fresh: Option<Ownership>,
    poisoned: bool,
    scanned: bool,
    prepared: bool,
    cancel_during_publish: Option<std::sync::Arc<AtomicBool>>,
}
#[cfg(unix)]
impl<'a> FreshDisk<'a> {
    pub(in crate::setup) fn reopen(
        root: &Path,
        selected: &'a SelectedManifest,
        index: usize,
        own: Ownership,
    ) -> (Self, File) {
        let (disk, file) = DiskProvider::reopen(root, own);
        (
            Self {
                disk,
                selected,
                index,
                fresh: None,
                poisoned: false,
                scanned: false,
                prepared: false,
                cancel_during_publish: None,
            },
            file,
        )
    }
    pub(in crate::setup) fn create(
        root: &Path,
        selected: &'a SelectedManifest,
        index: usize,
    ) -> (Self, File) {
        let (disk, file) = DiskProvider::new(root);
        let fresh = Some(disk.ownership());
        (
            Self {
                disk,
                selected,
                index,
                fresh,
                poisoned: false,
                scanned: false,
                prepared: false,
                cancel_during_publish: None,
            },
            file,
        )
    }
}
#[cfg(unix)]
impl Provider for &mut FreshDisk<'_> {
    fn revoke_preparation(&mut self) {
        self.scanned = false;
        self.prepared = false;
    }
    fn prepared(&mut self, proof: PreparationComplete<'_>) -> Result<(), String> {
        if self.poisoned
            || !self.scanned
            || !proof.authorizes(self.selected, self.index, &self.disk.own)?
        {
            return Err("no preparation authority".into());
        }
        PREPARATION_HOOK.with(|h| {
            if let Some(hook) = h.borrow_mut().as_mut() {
                hook(self.index)?;
            }
            Ok::<_, String>(())
        })?;
        self.prepared = true;
        Ok(())
    }
    fn ownership(&self) -> Ownership {
        self.disk.ownership()
    }
    fn verify(&mut self, file: &File, own: &Ownership) -> Result<(), String> {
        if self.poisoned {
            return Err("poisoned initialization".into());
        }
        self.disk.verify(file, own)
    }
    fn scan(&mut self, visit: &mut dyn FnMut(&[u8]) -> Result<(), String>) -> Result<(), String> {
        self.revoke_preparation();
        if self.poisoned {
            return Err("poisoned initialization".into());
        }
        self.disk.scan(visit)?;
        self.scanned = true;
        Ok(())
    }
    fn retain(&mut self, keep: &[u64]) -> Result<(), String> {
        if !self.scanned || self.poisoned {
            return Err("no retirement authority".into());
        }
        self.disk.retain(keep)
    }
    fn publish(&mut self, generation: u64, bytes: &[u8], keep: &[u64]) -> Result<(), String> {
        if !self.scanned || self.poisoned {
            return Err("no publication authority".into());
        }
        if let Some(cancel) = &self.cancel_during_publish {
            cancel.store(true, std::sync::atomic::Ordering::Release);
        }
        self.disk.publish(generation, bytes, keep)
    }
}
#[cfg(unix)]
impl FreshProvider for &mut FreshDisk<'_> {
    fn take_fresh(
        &mut self,
        selected: &SelectedManifest,
        index: usize,
    ) -> Result<Ownership, String> {
        let own = self.fresh.take().ok_or("no fresh owner")?;
        if self.poisoned
            || selected.manifest_sha256() != self.selected.manifest_sha256()
            || index != self.index
        {
            return Err("fresh selection mismatch".into());
        }
        Ok(own)
    }
    fn initialization_failed(&mut self) {
        self.fresh = None;
        self.poisoned = true;
        self.revoke_preparation();
    }
}
/// Fresh settlement success and refusals.
#[test]
#[cfg(unix)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn fresh_settlements_require_normal_preparation_and_preserve_evidence() {
    isolated(
        "independent_fresh_settlements_preserve_cancellation_and_require_normal_preparation",
        || {
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
                assert_eq!(
                    &before[0].1[layout::GENERATION..layout::EPOCH],
                    &1u64.to_le_bytes()
                );
                assert_eq!(&before[0].1[layout::EPOCH..layout::FULL_SIZE], &[0; 16]); // epoch and committed length
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
        },
    );

    isolated(
        "fresh_settlement_refuses_wrong_selection_ownership_nonempty_history_and_reopened_empty",
        || {
            let selected = selected();
            let mut changed_bytes = selected.bytes().to_vec();
            changed_bytes.push(b' ');
            let release = InternalRelease::transport_fixture(
                &changed_bytes,
                "https://code.tilo.so/releases/manifest.json",
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
                        let (mut other, mut other_file) =
                            FreshDisk::create(donor.path(), &selected, 0);
                        initialize_fresh(&selected, 0, &mut &mut other, &mut other_file).unwrap();
                        fs::copy(other.disk.dir.join("record-1"), p.disk.dir.join("record-1"))
                            .unwrap();
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
        },
    );

    isolated(
        "fresh_settlement_uncertain_flush_and_journal_failures_preserve_evidence_and_poison",
        || {
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
        },
    );
}
