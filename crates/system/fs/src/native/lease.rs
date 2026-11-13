//! Single-writer leases for files modified in place.
//!
//! A file becomes lease-class the first time it is modified in place: the new
//! version keeps some of the previous one's bytes where they were, in shared
//! extents or, for a small file whose every block was rewritten, in identical
//! sectors ([`kept_bytes_in_place`]). The lease lives in the namespace
//! (`crate::namespace::WriteLease`), so every device sees who holds it in causal
//! order with the file's changes. On this device a lease nobody holds is gated with
//! [`WriteGate::Ask`] and the first write acquires it; a lease another device holds
//! gates the file with `WriteGate::Deny(EBUSY)`; a held lease is renewed while the
//! file is written and released after the last write, once the versions under it
//! are published. Every version written under a lease is tagged with its tenancy
//! ([`crate::namespace::VersionFence`]), so a version from an epoch its author did
//! not hold never replaces the holder's.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fs_native::hydrate::{Hydrator as Placeholders, WriteGate, WritePolicy};

use crate::fs::QuickFS;
use crate::fs::native_ops::LeaseSource;
use crate::namespace::{VersionFence, WriteLease};

/// Sector size of the byte comparison in [`kept_bytes_in_place`].
const SECTOR: usize = 512;

/// Whether `new`, which kept the inode of `old`, overwrote some of `old`'s bytes
/// and left others where they were. `changed` is the extent diff of the two.
///
/// Shared extents answer most files without reading. A small file whose every
/// block was rewritten in place shares none, so its rewritten ranges are compared
/// sector by sector: a sector holding the same non-zero bytes at the same offset
/// was kept, and at least one other sector must differ. A sector of zeros proves
/// nothing.
pub fn kept_bytes_in_place(
    old: &std::fs::File,
    new: &std::fs::File,
    changed: &[std::ops::Range<u64>],
    old_len: u64,
    new_len: u64,
) -> std::io::Result<bool> {
    use std::os::unix::fs::FileExt as _;
    let changed_bytes: u64 = changed.iter().map(|r| r.end.saturating_sub(r.start)).sum();
    if changed_bytes < new_len {
        return Ok(true);
    }
    // Bytes past the previous end are new, not rewritten.
    let end = old_len.min(new_len);
    let (mut a, mut b) = (vec![0u8; 64 * SECTOR], vec![0u8; 64 * SECTOR]);
    let (mut kept, mut rewritten) = (false, false);
    for range in changed {
        let mut at = range.start.min(end);
        let stop = range.end.min(end);
        while at < stop {
            let left = usize::try_from(stop.saturating_sub(at)).unwrap_or(usize::MAX);
            let (Some(a), Some(b)) = (
                a.get_mut(..left.min(64 * SECTOR)),
                b.get_mut(..left.min(64 * SECTOR)),
            ) else {
                break;
            };
            old.read_exact_at(a, at)?;
            new.read_exact_at(b, at)?;
            for (x, y) in a.chunks(SECTOR).zip(b.chunks(SECTOR)) {
                if x != y {
                    rewritten = true;
                } else if x.iter().any(|&byte| byte != 0) {
                    kept = true;
                }
                if kept && rewritten {
                    return Ok(true);
                }
            }
            at = at.saturating_add(u64::try_from(a.len()).unwrap_or(u64::MAX));
        }
    }
    Ok(false)
}

/// Lease lifetime and renewal cadence.
#[derive(Clone, Copy, Debug)]
pub struct LeaseTiming {
    pub ttl: Duration,
    pub renew_every: Duration,
    /// A held lease is released this long after the last write.
    pub release_after_idle: Duration,
    /// How long a writer waits for the lease authority's answer before it
    /// takes a free lease optimistically.
    pub authority_wait: Duration,
}

impl Default for LeaseTiming {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(60),
            renew_every: Duration::from_secs(20),
            release_after_idle: Duration::from_secs(5),
            authority_wait: Duration::from_millis(1500),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Gate {
    Ask,
    Deny,
}

/// The write gates on lease-class files, shared with the projector: a new
/// version of a gated file is gated before it is renamed into place, so no
/// descriptor of it is ever opened unwatched (the kernel decides at open
/// whether a descriptor raises pre-content events).
#[derive(Default)]
pub struct WriteGates {
    by_path: Mutex<HashMap<PathBuf, Gate>>,
    asking: Mutex<HashMap<(u64, u64), PathBuf>>,
}

impl WriteGates {
    /// Gates `staged`, a new version of `rel` (at `abs`) not yet renamed
    /// into place, as the file it replaces is gated.
    pub fn gate_new_version(
        &self,
        placeholders: &Placeholders,
        staged: &Path,
        abs: &Path,
        rel: &Path,
    ) -> std::io::Result<()> {
        let gate = self.by_path.lock().ok().and_then(|g| g.get(abs).copied());
        match gate {
            Some(Gate::Deny) => placeholders
                .gate(staged, WriteGate::Deny(libc::EBUSY))
                .map(|_| ()),
            Some(Gate::Ask) => {
                let key = placeholders.gate(staged, WriteGate::Ask)?;
                if let Ok(mut asking) = self.asking.lock() {
                    asking.insert(key, rel.to_path_buf());
                }
                Ok(())
            }
            None => Ok(()),
        }
    }

    fn asked(&self, key: (u64, u64)) -> Option<PathBuf> {
        self.asking.lock().ok()?.get(&key).cloned()
    }
}

#[derive(Default)]
struct Shared {
    taken: Vec<PathBuf>,
    writes: HashMap<PathBuf, Instant>,
}

pub struct LeasePolicy {
    fs: Arc<QuickFS>,
    me: [u8; 32],
    timing: LeaseTiming,
    shared: Arc<Mutex<Shared>>,
    gates: Arc<WriteGates>,
}

impl WritePolicy for LeasePolicy {
    fn admit_writer(&self, file: (u64, u64)) -> Result<(), i32> {
        let Some(path) = self.gates.asked(file) else {
            return Ok(());
        };
        match self.fs.acquire_write_lease(
            &path,
            self.me,
            self.timing.ttl,
            self.timing.authority_wait,
        ) {
            Ok((lease, source)) => {
                let fresh = self.shared.lock().is_ok_and(|mut s| {
                    let fresh = s.writes.insert(path.clone(), Instant::now()).is_none();
                    s.taken.push(path.clone());
                    fresh
                });
                if fresh {
                    let how = match source {
                        LeaseSource::Local => "",
                        LeaseSource::Granted => ", granted by the lease authority",
                        LeaseSource::Optimistic => {
                            ", optimistically: the lease authority did not answer"
                        }
                    };
                    println!(
                        "took the write lease of {} (epoch {}{how})",
                        path.display(),
                        lease.epoch
                    );
                }
                Ok(())
            }
            Err(Some(holder)) => {
                eprintln!(
                    "write to {} refused: its write lease is held by device {} until {} ms",
                    path.display(),
                    hex::encode(&holder.holder[..8]),
                    holder.expires_ms
                );
                Err(libc::EBUSY)
            }
            Err(None) => Err(libc::EIO),
        }
    }
}

pub struct LeaseKeeper {
    fs: Arc<QuickFS>,
    placeholders: Arc<Placeholders>,
    live: PathBuf,
    me: [u8; 32],
    timing: LeaseTiming,
    shared: Arc<Mutex<Shared>>,
    lease_class: HashSet<PathBuf>,
    /// Gates in force, with the inode each was placed on: a file replaced by a newer
    /// version is a new inode and is gated again.
    gates: HashMap<PathBuf, (Gate, (u64, u64))>,
    renewed: HashMap<PathBuf, Instant>,
    notified: HashSet<(PathBuf, u64)>,
    write_gates: Arc<WriteGates>,
    authority: bool,
    answered: HashSet<(fs_protocol::FileRecordId, [u8; 32], u64)>,
}

impl LeaseKeeper {
    pub fn new(
        fs: Arc<QuickFS>,
        placeholders: Arc<Placeholders>,
        live: PathBuf,
        me: [u8; 32],
        timing: LeaseTiming,
        lease_class: HashSet<PathBuf>,
        write_gates: Arc<WriteGates>,
    ) -> Self {
        let shared = Arc::new(Mutex::new(Shared::default()));
        placeholders.set_write_policy(Arc::new(LeasePolicy {
            fs: Arc::clone(&fs),
            me,
            timing,
            shared: Arc::clone(&shared),
            gates: Arc::clone(&write_gates),
        }));
        Self {
            fs,
            placeholders,
            live,
            me,
            timing,
            shared,
            lease_class,
            gates: HashMap::new(),
            renewed: HashMap::new(),
            notified: HashSet::new(),
            write_gates,
            authority: false,
            answered: HashSet::new(),
        }
    }

    pub fn serve_as_authority(&mut self) {
        self.authority = true;
        self.fs.claim_lease_authority(self.me);
        println!("arbitrating write leases for this workspace");
    }

    pub fn arbitrate(&mut self) {
        if !self.authority {
            return;
        }
        for (path, lease) in self
            .fs
            .arbitrate_lease_requests(self.timing.ttl, &mut self.answered)
        {
            println!(
                "granted the write lease of {} to device {} (epoch {})",
                path.display(),
                hex::encode(&lease.holder[..8]),
                lease.epoch
            );
        }
    }

    /// The tenancy to tag a version of `rel` with: this device's, when it
    /// holds (or last held) the file's lease.
    pub fn fence_for(&self, rel: &Path) -> Option<VersionFence> {
        self.fs
            .write_lease_at(rel)
            .filter(|l| l.holder == self.me)
            .map(|l| VersionFence {
                holder: self.me,
                epoch: l.epoch,
            })
    }

    pub fn lease_class(&self) -> &HashSet<PathBuf> {
        &self.lease_class
    }

    pub fn classify(&mut self, rel: &Path) -> bool {
        self.lease_class.insert(rel.to_path_buf())
    }

    /// Follows a rename of `from` (and everything below it) to `to`: the
    /// file stays lease-class under its new name. Gates stay on the inode.
    pub fn moved(&mut self, from: &Path, to: &Path) {
        let rebase = |p: &Path| super::project::rebase(p, from, to);
        self.lease_class = self
            .lease_class
            .drain()
            .map(|p| rebase(&p).unwrap_or(p))
            .collect();
        self.gates = self
            .gates
            .drain()
            .map(|(p, g)| (rebase(&p).unwrap_or(p), g))
            .collect();
        self.renewed = self
            .renewed
            .drain()
            .map(|(p, t)| (rebase(&p).unwrap_or(p), t))
            .collect();
        if let Ok(mut asking) = self.write_gates.asking.lock() {
            for p in asking.values_mut() {
                if let Some(new) = rebase(p) {
                    *p = new;
                }
            }
        }
        if let Ok(mut s) = self.shared.lock() {
            s.writes = s
                .writes
                .drain()
                .map(|(p, t)| (rebase(&p).unwrap_or(p), t))
                .collect();
        }
        if let Ok(mut by_path) = self.write_gates.by_path.lock() {
            let live = self.live.clone();
            *by_path = by_path
                .drain()
                .map(|(abs, g)| {
                    let moved = abs
                        .strip_prefix(&live)
                        .ok()
                        .and_then(rebase)
                        .map_or_else(|| abs.clone(), |rel| live.join(rel));
                    (moved, g)
                })
                .collect();
        }
    }

    pub fn note_close(&self, rel: &Path) {
        if let Ok(mut s) = self.shared.lock()
            && let Some(at) = s.writes.get_mut(rel)
        {
            *at = Instant::now()
                .checked_sub(self.timing.release_after_idle)
                .unwrap_or_else(Instant::now);
        }
    }

    pub fn note_write(&self, rel: &Path) {
        if let Ok(mut s) = self.shared.lock()
            && let Some(at) = s.writes.get_mut(rel)
        {
            *at = Instant::now();
        }
    }

    fn set_gate(&mut self, rel: &Path, gate: Option<Gate>) {
        let abs = self.live.join(rel);
        let key = Placeholders::file_key(&abs).ok();
        let current = self.gates.get(rel).copied();
        let unchanged = match (current, gate) {
            (None, None) => true,
            (Some((g, k)), Some(want)) => g == want && Some(k) == key,
            _ => false,
        };
        if unchanged {
            return;
        }
        if let Ok(mut by_path) = self.write_gates.by_path.lock() {
            match gate {
                Some(g) => by_path.insert(abs.clone(), g),
                None => by_path.remove(&abs),
            };
        }
        let result = match gate {
            Some(Gate::Deny) => self
                .placeholders
                .gate(&abs, WriteGate::Deny(libc::EBUSY))
                .map(|_| ()),
            Some(Gate::Ask) => self.placeholders.gate(&abs, WriteGate::Ask).map(|key| {
                if let Ok(mut asking) = self.write_gates.asking.lock() {
                    asking.insert(key, rel.to_path_buf());
                }
            }),
            None => self.placeholders.ungate_writes(&abs),
        };
        if gate != Some(Gate::Ask)
            && let Ok(key) = Placeholders::file_key(&abs)
            && let Ok(mut asking) = self.write_gates.asking.lock()
        {
            asking.remove(&key);
        }
        if let (Ok(()), Some(g), Some(k)) = (result, gate, key) {
            self.gates.insert(rel.to_path_buf(), (g, k));
        } else {
            self.gates.remove(rel);
        }
    }

    /// Brings gates, renewals and releases up to date, about once a second and after
    /// every epoch. A lease is not released while `unpublished` names its file: the
    /// next holder must start from the versions written under it.
    pub fn tick(&mut self, now_ms: u64, unpublished: &HashSet<PathBuf>) {
        self.arbitrate();
        let taken: Vec<PathBuf> = self
            .shared
            .lock()
            .map(|mut s| std::mem::take(&mut s.taken))
            .unwrap_or_default();
        for rel in taken {
            self.renewed.insert(rel.clone(), Instant::now());
            self.set_gate(&rel, None);
        }

        let leases: HashMap<PathBuf, WriteLease> =
            self.fs.write_leases_by_path().into_iter().collect();
        let mut paths: HashSet<PathBuf> = self.lease_class.clone();
        paths.extend(leases.keys().cloned());
        for rel in paths {
            let lease = leases.get(&rel).copied();
            match lease {
                Some(l) if l.holder != self.me && l.is_live(now_ms) => {
                    self.set_gate(&rel, Some(Gate::Deny));
                    if self.notified.insert((rel.clone(), l.epoch)) {
                        eprintln!(
                            "{} is read-only here: device {} holds its write lease (epoch {})",
                            rel.display(),
                            hex::encode(&l.holder[..8]),
                            l.epoch
                        );
                    }
                }
                Some(l) if l.holder == self.me && l.is_live(now_ms) => {
                    self.set_gate(&rel, None);
                    self.keep_or_release(&rel, now_ms, unpublished.contains(&rel));
                }
                _ => {
                    if let Ok(mut s) = self.shared.lock() {
                        s.writes.remove(&rel);
                    }
                    // A file with any lease record is lease-class on every
                    // device, not only where the in-place write was seen.
                    let class = self.lease_class.contains(&rel) || lease.is_some();
                    let gate = class.then_some(Gate::Ask);
                    self.set_gate(&rel, gate);
                }
            }
        }
    }

    fn keep_or_release(&mut self, rel: &Path, now_ms: u64, unpublished: bool) {
        let last_write = self
            .shared
            .lock()
            .ok()
            .and_then(|s| s.writes.get(rel).copied());
        let idle = !unpublished
            && last_write.is_none_or(|t| t.elapsed() >= self.timing.release_after_idle);
        if idle {
            // Gated before the release is visible: no other device can take
            // the lease while this device's file is still open to writers.
            self.set_gate(rel, Some(Gate::Ask));
            self.fs.release_write_lease(rel, self.me, now_ms);
            if let Ok(mut s) = self.shared.lock() {
                s.writes.remove(rel);
            }
            self.renewed.remove(rel);
            return;
        }
        let due = self
            .renewed
            .get(rel)
            .is_none_or(|t| t.elapsed() >= self.timing.renew_every);
        if due
            && self
                .fs
                .take_write_lease(rel, self.me, self.timing.ttl)
                .is_ok()
        {
            self.renewed.insert(rel.to_path_buf(), Instant::now());
        }
    }

    pub fn release_all(&mut self, now_ms: u64) {
        for (rel, lease) in self.fs.write_leases_by_path() {
            if lease.holder == self.me && lease.is_live(now_ms) {
                self.fs.release_write_lease(&rel, self.me, now_ms);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::arithmetic_side_effects, reason = "test fixtures")]

    use std::io::Write as _;

    use super::kept_bytes_in_place;

    fn span(start: u64, end: u64) -> Vec<std::ops::Range<u64>> {
        std::iter::once(start..end).collect()
    }

    fn file(bytes: &[u8]) -> std::fs::File {
        let mut f = tempfile::tempfile().unwrap();
        f.write_all(bytes).unwrap();
        f
    }

    fn database(counter: u8, rows: &[u8]) -> Vec<u8> {
        let mut db = vec![0u8; 8192];
        db[..16].copy_from_slice(b"SQLite format 3\0");
        db[24] = counter;
        db[4000..4025].copy_from_slice(b"CREATE TABLE t(v INTEGER)");
        db[8192 - rows.len()..].copy_from_slice(rows);
        db
    }

    #[test]
    fn shared_extents_alone_prove_a_write_inside_the_file() {
        let (old, new) = (file(&[1; 8192]), file(&[2; 8192]));
        assert!(kept_bytes_in_place(&old, &new, &span(4096, 8192), 8192, 8192).unwrap());
    }

    #[test]
    fn a_small_database_whose_every_page_was_rewritten_kept_its_other_bytes() {
        let old = file(&database(1, b"\x01\x02\x03"));
        let new = file(&database(2, b"\x0b\x0c\x0d"));
        assert!(kept_bytes_in_place(&old, &new, &span(0, 8192), 8192, 8192).unwrap());
    }

    #[test]
    fn different_bytes_everywhere_are_a_rewrite() {
        let (old, new) = (file(&[1; 8192]), file(&[2; 8192]));
        assert!(!kept_bytes_in_place(&old, &new, &span(0, 8192), 8192, 8192).unwrap());
    }

    #[test]
    fn the_same_bytes_written_again_change_nothing_in_place() {
        let db = database(1, b"\x01");
        let (old, new) = (file(&db), file(&db));
        assert!(!kept_bytes_in_place(&old, &new, &span(0, 8192), 8192, 8192).unwrap());
    }

    #[test]
    fn zeros_at_the_same_offsets_are_not_kept_bytes() {
        let mut a = vec![0u8; 8192];
        let mut b = vec![0u8; 8192];
        a[0] = 1;
        b[0] = 2;
        let (old, new) = (file(&a), file(&b));
        assert!(!kept_bytes_in_place(&old, &new, &span(0, 8192), 8192, 8192).unwrap());
    }

    #[test]
    fn bytes_appended_past_the_previous_end_are_not_kept_bytes() {
        let old = file(&[1; 4096]);
        let mut grown = vec![2u8; 4096];
        grown.extend_from_slice(&[1; 4096]);
        let new = file(&grown);
        assert!(!kept_bytes_in_place(&old, &new, &span(0, 8192), 4096, 8192).unwrap());
    }
}
