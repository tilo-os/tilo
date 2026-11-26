use super::super::internal_download::SelectedManifest;
use super::*;

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
pub(super) fn fault(stage: &'static str) -> Result<(), String> {
    FAILURE.with(|f| {
        if *f.borrow() == Some(stage) {
            Err(format!("injected {stage}"))
        } else {
            Ok(())
        }
    })
}
pub(in crate::setup) fn fail(stage: Option<&'static str>) {
    FAILURE.with(|f| *f.borrow_mut() = stage);
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
type PreparationHook = Box<dyn FnMut(usize) -> Result<(), String>>;
#[cfg(unix)]
thread_local! {
    static PREPARATION_HOOK: RefCell<Option<PreparationHook>> = const { RefCell::new(None) };
}
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
