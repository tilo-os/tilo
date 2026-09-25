use super::*;
use crate::transport::SelectedManifest;
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};
thread_local! {
    static FAILURE: RefCell<Option<&'static str>> = const { RefCell::new(None) };
    static READS: RefCell<Vec<(&'static str, u64, u64)>> = const { RefCell::new(Vec::new()) };
}
pub(super) fn fault(stage: &'static str) -> Result<(), crate::error::Failure> {
    FAILURE.with(|f| {
        if *f.borrow() == Some(stage) {
            Err(format!("injected {stage}").into())
        } else {
            Ok(())
        }
    })
}
#[cfg(test)]
pub(crate) fn read_event(phase: &'static str, requested: u64, actual: u64) {
    READS.with(|r| r.borrow_mut().push((phase, requested, actual)));
}
#[cfg(test)]
pub(crate) fn fail(stage: Option<&'static str>) {
    FAILURE.with(|f| *f.borrow_mut() = stage);
}
#[cfg(test)]
pub(crate) fn read_totals(phase: &'static str) -> (u64, u64) {
    READS.with(|r| {
        r.borrow()
            .iter()
            .filter(|r| r.0 == phase)
            .fold((0, 0), |(a, b), r| (a + r.1, b + r.2))
    })
}
#[cfg(test)]
pub(crate) fn clear_reads() {
    READS.with(|r| r.borrow_mut().clear());
}
#[cfg(unix)]
pub(super) fn file_id(file: &File) -> [u8; 32] {
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
pub(crate) struct DiskProvider {
    pub(super) dir: PathBuf,
    pub(super) own: Ownership,
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
    fn verify(&mut self, file: &File, own: &Ownership) -> Result<(), crate::error::Failure> {
        if own != &self.own
            || file_id(file) != own.file
            || file_id(
                &File::open(self.dir.parent().unwrap())
                    .map_err(|e| crate::error::Failure::from(e.to_string()))?,
            ) != own.parent
        {
            return Err("Held identity mismatch".into());
        }
        Ok(())
    }
    fn scan(
        &mut self,
        visit: &mut dyn FnMut(&[u8]) -> Result<(), crate::error::Failure>,
    ) -> Result<(), crate::error::Failure> {
        for (n, entry) in fs::read_dir(&self.dir)
            .map_err(|e| crate::error::Failure::from(e.to_string()))?
            .enumerate()
        {
            if n >= MAX_SCAN {
                return Err("scan bound".into());
            }
            let mut file = File::open(
                entry
                    .map_err(|e| crate::error::Failure::from(e.to_string()))?
                    .path(),
            )
            .map_err(|e| crate::error::Failure::from(e.to_string()))?;
            if file.metadata().unwrap().len() > MAX_RECORD as u64 {
                return Err("record bound".into());
            }
            let mut b = Vec::new();
            file.read_to_end(&mut b)
                .map_err(|e| crate::error::Failure::from(e.to_string()))?;
            visit(&b)?;
        }
        Ok(())
    }
    fn retain(&mut self, keep: &[u64]) -> Result<(), crate::error::Failure> {
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
                fs::remove_file(entry.path())
                    .map_err(|e| crate::error::Failure::from(e.to_string()))?;
            }
        }
        File::open(&self.dir)
            .unwrap()
            .sync_all()
            .map_err(|e| crate::error::Failure::from(e.to_string()))
    }
    fn publish(
        &mut self,
        generation: u64,
        bytes: &[u8],
        keep: &[u64],
    ) -> Result<(), crate::error::Failure> {
        fault("record-create")?;
        let temp = self.dir.join(format!("pending-{generation}"));
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|e| crate::error::Failure::from(e.to_string()))?;
        let middle = bytes.len() / 2;
        f.write_all(&bytes[..middle])
            .map_err(|e| crate::error::Failure::from(e.to_string()))?;
        fault("record-write")?;
        f.write_all(&bytes[middle..])
            .map_err(|e| crate::error::Failure::from(e.to_string()))?;
        f.flush()
            .map_err(|e| crate::error::Failure::from(e.to_string()))?;
        fault("record-flush")?;
        f.sync_all()
            .map_err(|e| crate::error::Failure::from(e.to_string()))?;
        fault("record-publish")?;
        fs::hard_link(&temp, self.dir.join(format!("record-{generation}")))
            .map_err(|e| crate::error::Failure::from(e.to_string()))?;
        fs::remove_file(temp).map_err(|e| crate::error::Failure::from(e.to_string()))?;
        File::open(&self.dir)
            .unwrap()
            .sync_all()
            .map_err(|e| crate::error::Failure::from(e.to_string()))?;
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
            .map_err(|e| crate::error::Failure::from(e.to_string()))?;
        Ok(())
    }
}
#[cfg(all(test, unix))]
pub(crate) fn prefix(
    prepared: &mut PreparedPrefix<'_>,
    bytes: &[u8],
) -> Result<(), crate::error::Failure> {
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
// Scoped host fixture hook at the actual final preparation receipt, after
// Session's real record normalization, tail trimming and cursor restoration.
#[cfg(unix)]
type PreparationHook = Box<dyn FnMut(usize) -> Result<(), crate::error::Failure>>;
#[cfg(unix)]
thread_local! {
    static PREPARATION_HOOK: RefCell<Option<PreparationHook>> = const { RefCell::new(None) };
}
#[cfg(all(test, unix))]
pub(crate) fn with_preparation_hook(hook: PreparationHook, run: impl FnOnce()) {
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
pub(crate) struct FreshDisk<'a> {
    pub(super) disk: DiskProvider,
    pub(super) selected: &'a SelectedManifest,
    pub(super) index: usize,
    pub(super) fresh: Option<Ownership>,
    pub(super) poisoned: bool,
    pub(super) scanned: bool,
    pub(super) prepared: bool,
    pub(super) cancel_during_publish: Option<std::sync::Arc<AtomicBool>>,
}
#[cfg(unix)]
impl<'a> FreshDisk<'a> {
    pub(crate) fn reopen(
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
    pub(crate) fn create(
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
    fn prepared(&mut self, proof: PreparationComplete<'_>) -> Result<(), crate::error::Failure> {
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
            Ok::<_, crate::error::Failure>(())
        })?;
        self.prepared = true;
        Ok(())
    }
    fn ownership(&self) -> Ownership {
        self.disk.ownership()
    }
    fn verify(&mut self, file: &File, own: &Ownership) -> Result<(), crate::error::Failure> {
        if self.poisoned {
            return Err("poisoned initialization".into());
        }
        self.disk.verify(file, own)
    }
    fn scan(
        &mut self,
        visit: &mut dyn FnMut(&[u8]) -> Result<(), crate::error::Failure>,
    ) -> Result<(), crate::error::Failure> {
        self.revoke_preparation();
        if self.poisoned {
            return Err("poisoned initialization".into());
        }
        self.disk.scan(visit)?;
        self.scanned = true;
        Ok(())
    }
    fn retain(&mut self, keep: &[u64]) -> Result<(), crate::error::Failure> {
        if !self.scanned || self.poisoned {
            return Err("no retirement authority".into());
        }
        self.disk.retain(keep)
    }
    fn publish(
        &mut self,
        generation: u64,
        bytes: &[u8],
        keep: &[u64],
    ) -> Result<(), crate::error::Failure> {
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
    ) -> Result<Ownership, crate::error::Failure> {
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
