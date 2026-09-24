//! Single-writer leases for files modified in place (`storage.md` §11.2).
//!
//! A file becomes lease-class the first time it is modified in place: its
//! inode survives the change and the new version still shares extents with
//! the previous one (a write inside the file, not a truncate-and-rewrite or
//! a replace-by-rename). Detection is behavioural and knows no file types.
//!
//! The lease itself lives in the namespace (`crate::namespace::WriteLease`),
//! so every device sees who holds it in causal order with the file's
//! changes. On this device:
//!
//! - a lease-class file whose lease nobody holds is gated with
//!   [`WriteGate::Ask`]: the first write of a session acquires the lease
//!   before it proceeds, and the gate is then lifted so the holder's I/O
//!   raises no events;
//! - a file whose live lease another device holds is gated with
//!   `WriteGate::Deny(EBUSY)`: readers read, a writer gets `EBUSY`;
//! - a held lease is renewed while the file is being written and released
//!   shortly after the last write, which hands it to the next writer; a
//!   device that goes offline loses it when it expires.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fs_native::hydrate::{Hydrator as Placeholders, WriteGate, WritePolicy};

use crate::fs::QuickFS;
use crate::namespace::WriteLease;

/// Lease lifetime and renewal cadence.
#[derive(Clone, Copy, Debug)]
pub struct LeaseTiming {
    pub ttl: Duration,
    pub renew_every: Duration,
    /// A held lease is released this long after the last write.
    pub release_after_idle: Duration,
}

impl Default for LeaseTiming {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(60),
            renew_every: Duration::from_secs(20),
            release_after_idle: Duration::from_secs(5),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Gate {
    Ask,
    Deny,
}

#[derive(Default)]
struct Shared {
    /// Files gated with `Ask`, by `(st_dev, st_ino)`.
    asking: HashMap<(u64, u64), PathBuf>,
    /// Paths whose lease the policy took; the keeper lifts their gates.
    taken: Vec<PathBuf>,
    /// Paths written under a held lease, with the time of the last write.
    writes: HashMap<PathBuf, Instant>,
}

/// The [`WritePolicy`] installed on the placeholder hydrator.
pub struct LeasePolicy {
    fs: Arc<QuickFS>,
    me: [u8; 32],
    timing: LeaseTiming,
    shared: Arc<Mutex<Shared>>,
}

impl WritePolicy for LeasePolicy {
    fn admit_writer(&self, file: (u64, u64)) -> Result<(), i32> {
        let Some(path) = self
            .shared
            .lock()
            .ok()
            .and_then(|s| s.asking.get(&file).cloned())
        else {
            return Ok(());
        };
        match self.fs.take_write_lease(&path, self.me, self.timing.ttl) {
            Ok(lease) => {
                let fresh = self.shared.lock().is_ok_and(|mut s| {
                    let fresh = s.writes.insert(path.clone(), Instant::now()).is_none();
                    s.taken.push(path.clone());
                    fresh
                });
                if fresh {
                    println!(
                        "took the write lease of {} (epoch {})",
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

/// Keeps gates and held leases in step with the namespace.
pub struct LeaseKeeper {
    fs: Arc<QuickFS>,
    placeholders: Arc<Placeholders>,
    live: PathBuf,
    me: [u8; 32],
    timing: LeaseTiming,
    shared: Arc<Mutex<Shared>>,
    lease_class: HashSet<PathBuf>,
    /// Gates in force, with the inode each was placed on: a file replaced
    /// by a newer version is a new inode and is gated again.
    gates: HashMap<PathBuf, (Gate, (u64, u64))>,
    renewed: HashMap<PathBuf, Instant>,
    notified: HashSet<(PathBuf, u64)>,
    /// Absolute paths gated with `Deny`, shared with the projector.
    write_denied: Arc<Mutex<HashSet<PathBuf>>>,
}

impl LeaseKeeper {
    pub fn new(
        fs: Arc<QuickFS>,
        placeholders: Arc<Placeholders>,
        live: PathBuf,
        me: [u8; 32],
        timing: LeaseTiming,
        lease_class: HashSet<PathBuf>,
        write_denied: Arc<Mutex<HashSet<PathBuf>>>,
    ) -> Self {
        let shared = Arc::new(Mutex::new(Shared::default()));
        placeholders.set_write_policy(Arc::new(LeasePolicy {
            fs: Arc::clone(&fs),
            me,
            timing,
            shared: Arc::clone(&shared),
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
            write_denied,
        }
    }

    pub fn lease_class(&self) -> &HashSet<PathBuf> {
        &self.lease_class
    }

    /// Marks `rel` lease-class. Returns whether it is new.
    pub fn classify(&mut self, rel: &Path) -> bool {
        self.lease_class.insert(rel.to_path_buf())
    }

    /// Notes that a writer of `rel` closed the file (from the journal): the
    /// lease is released at the next tick.
    pub fn note_close(&self, rel: &Path) {
        if let Ok(mut s) = self.shared.lock()
            && let Some(at) = s.writes.get_mut(rel)
        {
            *at = Instant::now()
                .checked_sub(self.timing.release_after_idle)
                .unwrap_or_else(Instant::now);
        }
    }

    /// Notes a local write to `rel` (from the journal).
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
        if let Ok(mut denied) = self.write_denied.lock() {
            if gate == Some(Gate::Deny) {
                denied.insert(abs.clone());
            } else {
                denied.remove(&abs);
            }
        }
        let result = match gate {
            Some(Gate::Deny) => self
                .placeholders
                .gate(&abs, WriteGate::Deny(libc::EBUSY))
                .map(|_| ()),
            Some(Gate::Ask) => self.placeholders.gate(&abs, WriteGate::Ask).map(|key| {
                if let Ok(mut s) = self.shared.lock() {
                    s.asking.insert(key, rel.to_path_buf());
                }
            }),
            None => self.placeholders.ungate_writes(&abs),
        };
        if gate != Some(Gate::Ask)
            && let Ok(key) = Placeholders::file_key(&abs)
            && let Ok(mut s) = self.shared.lock()
        {
            s.asking.remove(&key);
        }
        // A failure means the file is gone (or not there yet): the next tick
        // tries again.
        if let (Ok(()), Some(g), Some(k)) = (result, gate, key) {
            self.gates.insert(rel.to_path_buf(), (g, k));
        } else {
            self.gates.remove(rel);
        }
    }

    /// Brings gates, renewals and releases up to date. Called about once a
    /// second and after every epoch.
    pub fn tick(&mut self, now_ms: u64) {
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
                    if self.notified.insert((rel.clone(), l.epoch)) {
                        eprintln!(
                            "{} is read-only here: device {} holds its write lease (epoch {})",
                            rel.display(),
                            hex::encode(&l.holder[..8]),
                            l.epoch
                        );
                    }
                    self.set_gate(&rel, Some(Gate::Deny));
                }
                Some(l) if l.holder == self.me && l.is_live(now_ms) => {
                    self.set_gate(&rel, None);
                    self.keep_or_release(&rel, now_ms);
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

    fn keep_or_release(&mut self, rel: &Path, now_ms: u64) {
        let last_write = self
            .shared
            .lock()
            .ok()
            .and_then(|s| s.writes.get(rel).copied());
        let idle = last_write.is_none_or(|t| t.elapsed() >= self.timing.release_after_idle);
        if idle {
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

    /// Releases every lease this device holds (clean shutdown).
    pub fn release_all(&mut self, now_ms: u64) {
        for (rel, lease) in self.fs.write_leases_by_path() {
            if lease.holder == self.me && lease.is_live(now_ms) {
                self.fs.release_write_lease(&rel, self.me, now_ms);
            }
        }
    }
}
