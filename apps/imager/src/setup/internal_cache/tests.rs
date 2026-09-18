use super::super::internal_download::SelectedManifest;
use super::*;
use crate::release::InternalRelease;

fn selected() -> SelectedManifest {
    let bytes = super::super::internal_bundle::tests::bytes(
        &super::super::internal_bundle::tests::fixture(),
    );
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
        let leaf = ".qos-part-0123456789abcdef0123456789abcdef";
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
        let mut value = super::super::internal_bundle::tests::fixture();
        value["components"][index]["filename"] =
            serde_json::json!(".qos-part-0123456789abcdef0123456789abcdef");
        let bytes = super::super::internal_bundle::tests::bytes(&value);
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
