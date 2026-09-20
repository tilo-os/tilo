//! Sealed read-only source boundary. Only source owners can attest actual reads.
use super::migration::{PinnedDirectory, changed, reject_windows_placeholder, same_snapshot};
use super::migration_journal::FileReceipt;
use super::nofollow::metadata_nofollow;
use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_primitives::fs::{FollowSymlinks, Metadata, OpenOptions};
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::Path,
    sync::{Arc, atomic::AtomicBool},
    time::SystemTime,
};
pub(super) mod sealed {
    pub trait Sealed {}
}
pub(super) struct CopyTimes {
    pub accessed: SystemTime,
    pub modified: SystemTime,
}
pub(super) enum CopyPass {
    Initial,
    IndependentReread,
}
pub(super) trait CopyFileSource: sealed::Sealed {
    type Receipt;
    fn expected_len(&self) -> u64;
    fn times(&self) -> io::Result<CopyTimes>;
    fn begin_pass(&mut self, pass: CopyPass) -> io::Result<()>;
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize>;
    fn attest(&mut self, bytes: u64, digest: [u8; 32], cancel: &AtomicBool) -> io::Result<()>;
    fn receipt(
        &self,
        destination: &File,
        bytes: u64,
        digest: [u8; 32],
        durable: bool,
    ) -> io::Result<Self::Receipt>;
}
pub(super) struct SourcePass<'a, S: CopyFileSource>(pub &'a mut S);
impl<S: CopyFileSource> Read for SourcePass<'_, S> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.0.read(out)
    }
}
pub(super) struct LocalCopySource {
    parent: Arc<PinnedDirectory>,
    name: OsString,
    file: File,
    original: Metadata,
}
impl LocalCopySource {
    pub(super) fn open(
        parent: &Arc<PinnedDirectory>,
        name: &OsStr,
        expected: &Metadata,
    ) -> io::Result<Self> {
        parent.validate()?;
        reject_windows_placeholder(expected)?;
        if !expected.is_file() || expected.is_symlink() {
            return Err(changed("source is not a regular file"));
        }
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No).nonblock(true);
        let file = cap_primitives::fs::open(&parent.file, Path::new(name), &options)?;
        let original = Metadata::from_file(&file)?;
        if !same_snapshot(expected, &original) {
            return Err(changed("source changed while opening"));
        }
        Ok(Self {
            parent: Arc::clone(parent),
            name: name.to_os_string(),
            file,
            original,
        })
    }
}
impl sealed::Sealed for LocalCopySource {}
impl CopyFileSource for LocalCopySource {
    type Receipt = FileReceipt;
    fn expected_len(&self) -> u64 {
        self.original.len()
    }
    fn times(&self) -> io::Result<CopyTimes> {
        Ok(CopyTimes {
            accessed: self.original.accessed()?.into_std(),
            modified: self.original.modified()?.into_std(),
        })
    }
    fn begin_pass(&mut self, _: CopyPass) -> io::Result<()> {
        self.file.seek(SeekFrom::Start(0)).map(|_| ())
    }
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.file.read(out)
    }
    fn attest(&mut self, _: u64, _: [u8; 32], _: &AtomicBool) -> io::Result<()> {
        self.parent.validate()?;
        let by_handle = Metadata::from_file(&self.file)?;
        let by_name = metadata_nofollow(&self.parent.file, Path::new(&self.name))?;
        if !same_snapshot(&self.original, &by_handle) || !same_snapshot(&self.original, &by_name) {
            return Err(changed(
                "source metadata or identity changed during copy; retry required",
            ));
        }
        Ok(())
    }
    fn receipt(
        &self,
        destination: &File,
        bytes: u64,
        digest: [u8; 32],
        durable: bool,
    ) -> io::Result<FileReceipt> {
        FileReceipt::new(
            &self.file,
            &self.original,
            destination,
            bytes,
            digest,
            durable,
        )
    }
}
