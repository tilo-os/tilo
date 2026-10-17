//! Native descriptor lease for a protected installed record.
//! A lease proves byte stability and namespace control, not record semantics.
use super::{bytes, chain, controlled, invalid, operation, stamp};
use std::{
    ffi::OsStr,
    fs::File,
    io,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};
use tilo_files::native::atomic::{Privacy, StorageDirectory};

/// Nonserializable root-owned record custody. No frontend-supplied descriptor,
/// callback or deserialized record can construct this lease.
pub struct InstalledRecordLease {
    path: PathBuf,
    chain: Vec<File>,
    name: String,
    record: File,
    snapshot: Vec<u8>,
}
impl InstalledRecordLease {
    /// Open only through the native root-role and controlled-parent gate.
    /// The returned bytes still require the caller's closed typed validation.
    pub fn open(path: &Path, id: &str) -> io::Result<Self> {
        operation(id)?;
        // chain checks the actual effective uid and each parent descriptor,
        // including root ownership, write permissions and namespace ACLs.
        let chain = chain(path, false)?;
        let storage = StorageDirectory::from_file(chain.last().ok_or_else(invalid)?.try_clone()?)?;
        let name = format!("{id}.json");
        let record = storage.open_read(OsStr::new(&name), Privacy::OwnerOnly)?;
        let snapshot = bytes(&record)?;
        let value = Self {
            path: path.to_owned(),
            chain,
            name,
            record,
            snapshot,
        };
        value.revalidate()?;
        Ok(value)
    }
    pub fn bytes(&self) -> &[u8] {
        &self.snapshot
    }
    pub fn revalidate(&self) -> io::Result<()> {
        let current = chain(&self.path, false)?;
        if current.len() != self.chain.len() {
            return Err(invalid());
        }
        for (actual, held) in current.iter().zip(&self.chain) {
            controlled(held)?;
            let (a, h) = (actual.metadata()?, held.metadata()?);
            if (a.dev(), a.ino()) != (h.dev(), h.ino()) {
                return Err(invalid());
            }
        }
        let storage =
            StorageDirectory::from_file(current.last().ok_or_else(invalid)?.try_clone()?)?;
        let named = storage.open_read(OsStr::new(&self.name), Privacy::OwnerOnly)?;
        if stamp(&named)? != stamp(&self.record)?
            || bytes(&named)? != self.snapshot
            || bytes(&self.record)? != self.snapshot
        {
            return Err(invalid());
        }
        Ok(())
    }
}
