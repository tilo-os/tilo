//! Source-independent private journal storage. Validation always precedes repair.
use super::migration::{OwnedTemporary, PinnedDirectory, changed};
use super::{
    destination_security, migration_durability as durability,
    nofollow::{metadata_nofollow, same_directory_identity},
};
use advisory_lock::FileLock;
use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_primitives::fs::{FollowSymlinks, Metadata, OpenOptions};
use serde::Serialize;
use std::{
    ffi::OsString,
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
    sync::Arc,
};
pub const MAX_RECORD: usize = 128 * 1024;
pub fn write_record<T: Serialize>(
    file: &mut File,
    sequence: &mut u64,
    record: &T,
) -> io::Result<()> {
    use sha2::{Digest, Sha256};
    let payload = serde_json::to_vec(record)?;
    if payload.len() > MAX_RECORD {
        return Err(changed("journal record exceeds size bound"));
    }
    let len = u32::try_from(payload.len())
        .map_err(|_| changed("record length overflow"))?
        .to_le_bytes();
    let seq = sequence.to_le_bytes();
    let mut hash = Sha256::new();
    hash.update(len);
    hash.update(seq);
    hash.update(&payload);
    file.write_all(&len)?;
    file.write_all(&seq)?;
    file.write_all(&hash.finalize())?;
    file.write_all(&payload)?;
    *sequence = sequence
        .checked_add(1)
        .ok_or_else(|| changed("journal sequence exhausted"))?;
    Ok(())
}

// A partial last frame may be an interrupted write. A complete frame with a
// bad checksum/sequence/schema is corruption and must never be skipped.
pub fn read_record<T: serde::de::DeserializeOwned>(
    file: &mut File,
    sequence: u64,
) -> io::Result<Option<T>> {
    use sha2::{Digest, Sha256};
    let mut header = [0; 44];
    match file.read_exact(&mut header) {
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        result => result?,
    }
    let len = u32::from_le_bytes(
        header[..4]
            .try_into()
            .map_err(|_| changed("invalid length"))?,
    ) as usize;
    let seq = u64::from_le_bytes(
        header[4..12]
            .try_into()
            .map_err(|_| changed("invalid sequence"))?,
    );
    if len == 0 || len > MAX_RECORD || seq != sequence {
        return Err(changed("invalid journal record bounds or sequence"));
    }
    let mut payload = vec![0; len];
    match file.read_exact(&mut payload) {
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        result => result?,
    }
    let mut hash = Sha256::new();
    hash.update(&header[..12]);
    hash.update(&payload);
    if hash.finalize().as_slice() != &header[12..] {
        return Err(changed("journal record checksum mismatch"));
    }
    Ok(Some(serde_json::from_slice(&payload)?))
}

pub fn parent(path: &Path) -> io::Result<(Arc<PinnedDirectory>, OsString)> {
    if !path.is_absolute()
        || path.as_os_str().len() > 4096
        || path.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
    {
        return Err(changed("invalid journal path"));
    }
    let name = path
        .file_name()
        .filter(|n| n.len() <= 255)
        .ok_or_else(|| changed("journal filename missing or too long"))?
        .to_os_string();
    let parent = PinnedDirectory::open_absolute(
        path.parent()
            .ok_or_else(|| changed("journal parent missing"))?,
    )?;
    parent.validate_destination()?;
    destination_security::check_private_file(&parent.file)?;
    Ok((parent, name))
}
pub fn open_private(
    parent: &Arc<PinnedDirectory>,
    name: &std::ffi::OsStr,
) -> io::Result<(FileLock, Metadata)> {
    let expected = metadata_nofollow(&parent.file, Path::new(name))?;
    if !expected.is_file() || expected.is_symlink() {
        return Err(changed("journal is not a regular file"));
    }
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .follow(FollowSymlinks::No)
        .nonblock(true);
    let file = cap_primitives::fs::open(&parent.file, Path::new(name), &options)?;
    destination_security::check_private_file(&file)?;
    let file =
        FileLock::try_exclusive(file).map_err(|e| changed(&format!("journal is busy: {e}")))?;
    let identity = Metadata::from_file(&file)?;
    if !super::migration::same_snapshot(&expected, &identity) {
        return Err(changed("journal replaced while opening"));
    }
    Ok((file, identity))
}
pub fn validate(
    parent: &PinnedDirectory,
    name: &std::ffi::OsStr,
    file: &File,
    identity: &Metadata,
) -> io::Result<()> {
    parent.validate_destination()?;
    destination_security::check_private_file(&parent.file)?;
    destination_security::check_private_file(file)?;
    let current = metadata_nofollow(&parent.file, Path::new(name))?;
    if !current.is_file() || current.is_symlink() || !same_directory_identity(&current, identity) {
        return Err(changed("journal replaced; reselect and review operation"));
    }
    Ok(())
}
pub fn repair(
    file: &mut File,
    end: u64,
    validate_location: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    validate_location()?;
    if end != file.metadata()?.len() {
        file.set_len(end)?;
        durability::sync_file(file)?;
    }
    file.seek(SeekFrom::Start(end))?;
    Ok(())
}
pub fn append_synced(
    file: &mut File,
    sequence: &mut u64,
    records: impl FnOnce(&mut File, &mut u64) -> io::Result<()>,
    before_sync: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    records(file, sequence)?;
    before_sync()?;
    durability::sync_file(file)
}
pub fn create_private<T: Serialize>(
    parent: &Arc<PinnedDirectory>,
    name: &std::ffi::OsStr,
    magic: [u8; 8],
    header: &T,
) -> io::Result<(FileLock, Metadata, u64)> {
    durability::supported()?;

    let mut temporary = OwnedTemporary::create(parent, name)?;
    temporary.file.write_all(&magic)?;
    let mut sequence = 0;
    write_record(&mut temporary.file, &mut sequence, header)?;
    durability::sync_file(&temporary.file)?;
    parent.validate_destination()?;
    temporary.validate()?;
    cap_primitives::fs::hard_link(
        &parent.file,
        Path::new(&temporary.name),
        &parent.file,
        Path::new(&name),
    )?;
    durability::sync_directory(&parent.file)?;
    let file = FileLock::try_exclusive(temporary.file.try_clone()?)
        .map_err(|e| changed(&format!("journal is busy: {e}")))?;
    let identity = Metadata::from_file(&file)?;
    Ok((file, identity, sequence))
}
pub struct Log {
    pub file: FileLock,
    pub sequence: u64,
    parent: Arc<PinnedDirectory>,
    name: OsString,
    identity: Metadata,
    failed: bool,
}
impl Log {
    pub fn create<T: Serialize>(path: &Path, magic: [u8; 8], header: &T) -> io::Result<Self> {
        let (parent, name) = parent(path)?;
        let (file, identity, sequence) = create_private(&parent, &name, magic, header)?;
        Ok(Self {
            file,
            sequence,
            parent,
            name,
            identity,
            failed: false,
        })
    }
    pub fn open(path: &Path, magic: [u8; 8]) -> io::Result<Self> {
        durability::supported()?;
        let (parent, name) = parent(path)?;
        let (mut file, identity) = open_private(&parent, &name)?;
        let mut observed = [0; 8];
        file.read_exact(&mut observed)?;
        if observed != magic {
            return Err(changed("native journal schema mismatch"));
        }
        Ok(Self {
            file,
            sequence: 0,
            parent,
            name,
            identity,
            failed: false,
        })
    }
    pub fn validate(&self) -> io::Result<()> {
        validate(&self.parent, &self.name, &self.file, &self.identity)
    }
    pub fn repair(
        &mut self,
        end: u64,
        validate_destination: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        let Self {
            file,
            parent,
            name,
            identity,
            ..
        } = self;
        let validation_file = file.try_clone()?;
        repair(file, end, || {
            validate_destination()?;
            validate(parent, name, &validation_file, identity)
        })
    }
    pub fn append<T: Serialize>(&mut self, record: &T) -> io::Result<()> {
        use super::migration_journal::{PersistenceBoundary, persistence_result};
        if self.failed {
            return Err(changed("journal persistence already failed"));
        }
        let result = (|| {
            persistence_result(self.validate(), PersistenceBoundary::JournalUnavailable)?;
            persistence_result(
                append_synced(
                    &mut self.file,
                    &mut self.sequence,
                    |file, seq| write_record(file, seq, record),
                    || Ok(()),
                ),
                PersistenceBoundary::JournalReplacementUncertain,
            )
        })();
        self.failed = result.is_err();
        result
    }
}
