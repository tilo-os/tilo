//! Anonymous, private sorted metadata runs. The retained files are an index,
//! never source authority. Only a completely synced index can be read.
use super::{Fact, NativePath, changed};
use std::{
    fs::File,
    io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Mutex, atomic::AtomicBool},
};

#[cfg(target_os = "linux")]
pub mod scratch;

#[derive(Clone, Default)]
struct Factory {
    #[cfg(target_os = "linux")]
    scratch: Option<std::sync::Arc<scratch::InventoryScratch>>,
}
struct Run {
    file: File,
    // Retain the lease until after the last owned file descriptor closes.
    #[cfg(target_os = "linux")]
    _allocation: Option<scratch::Allocation>,
}
impl std::ops::Deref for Run {
    type Target = File;
    fn deref(&self) -> &File {
        &self.file
    }
}
impl Read for Run {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.file.read(bytes)
    }
}
impl Write for Run {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.file.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}
impl Seek for Run {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.file.seek(position)
    }
}
impl Factory {
    fn check(&self) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        if let Some(scratch) = &self.scratch {
            scratch.check_custody()?;
        }
        Ok(())
    }

    fn allocate(&self, bytes: u64) -> io::Result<Run> {
        #[cfg(target_os = "linux")]
        if let Some(scratch) = &self.scratch {
            let allocation = scratch.allocate(bytes)?;
            return Ok(Run {
                file: allocation.file.try_clone()?,
                _allocation: Some(allocation),
            });
        }
        let _ = bytes;
        Ok(Run {
            file: private_file()?,
            #[cfg(target_os = "linux")]
            _allocation: None,
        })
    }
}

const BUFFER: usize = 512;
const RECORD_BYTES: usize = 128 * 1024;
const BUFFER_BYTES: usize = 8 * 1024 * 1024;
/// Each consumer retains its existing bounded individual metadata policy.
pub trait IndexValue: serde::Serialize + serde::de::DeserializeOwned {
    const RECORD_BYTES: usize;
}
impl IndexValue for Fact {
    const RECORD_BYTES: usize = self::RECORD_BYTES;
}
type Item<T> = (PathBuf, T);
pub(super) type Storage = SortedIndex<Fact>;

pub struct SortedIndex<T: IndexValue> {
    factory: Factory,
    building: Option<Builder<T>>,
    final_index: Option<Mutex<Index>>,
    count: usize,
    #[cfg(test)]
    pub(super) peak_buffer: usize,
}
struct Builder<T: IndexValue> {
    buffer: Vec<Item<T>>,
    buffered_bytes: usize,
    // Binary carries keep at most one run at each level, not one FD per chunk.
    runs: Vec<Option<Run>>,
    factory: Factory,
}
struct Index {
    records: Run,
    offsets: Run,
}
// tempfile::tempfile's Linux O_TMPFILE fast path inherits 0666/umask.
// NamedTempFile creates 0600 exclusively; remove its empty name successfully
// before writing any reviewed metadata, then retain only the anonymous FD.
fn private_file() -> io::Result<File> {
    let (file, path) = tempfile::NamedTempFile::new()?.into_parts();
    super::super::destination_security::check_private_file(&file)?;
    path.close()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if file.metadata()?.nlink() != 0 {
            return Err(changed("Inventory temporary file was not unlinked"));
        }
    }
    Ok(file)
}
impl<T: IndexValue> Default for SortedIndex<T> {
    fn default() -> Self {
        Self::new()
    }
}
impl<T: IndexValue> SortedIndex<T> {
    pub fn new() -> Self {
        Self::from_factory(Factory::default())
    }
    #[cfg(target_os = "linux")]
    pub fn with_scratch(scratch: std::sync::Arc<scratch::InventoryScratch>) -> Self {
        Self::from_factory(Factory {
            scratch: Some(scratch),
        })
    }
    #[cfg(target_os = "linux")]
    pub(super) fn working_storage(&self) -> Option<std::sync::Arc<scratch::InventoryScratch>> {
        self.factory.scratch.clone()
    }
    #[must_use]
    pub fn empty_like(&self) -> Self {
        Self::from_factory(self.factory.clone())
    }
    fn from_factory(factory: Factory) -> Self {
        Self {
            factory: factory.clone(),
            building: Some(Builder {
                buffer: Vec::new(),
                buffered_bytes: 0,
                runs: Vec::new(),
                factory,
            }),
            final_index: None,
            count: 0,
            #[cfg(test)]
            peak_buffer: 0,
        }
    }
    pub fn len(&self) -> usize {
        self.count
    }
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
    pub fn insert(&mut self, path: PathBuf, fact: T, cancel: &AtomicBool) -> io::Result<()> {
        super::super::migration::check_cancel(cancel)?;
        let builder = self
            .building
            .as_mut()
            .ok_or_else(|| changed("Inventory already finalized"))?;
        let bytes = item_bytes(&path, &fact)?.len();
        builder.buffered_bytes = builder
            .buffered_bytes
            .checked_add(bytes)
            .ok_or_else(|| changed("Inventory buffer length overflow"))?;
        builder.buffer.push((path, fact));
        self.count = self
            .count
            .checked_add(1)
            .ok_or_else(|| changed("Inventory count overflow"))?;
        #[cfg(test)]
        {
            self.peak_buffer = self.peak_buffer.max(builder.buffer.len());
        }
        if builder.buffer.len() == BUFFER || builder.buffered_bytes >= BUFFER_BYTES {
            builder.flush(cancel)?;
        }
        Ok(())
    }
    pub fn finish(&mut self, cancel: &AtomicBool) -> io::Result<()> {
        super::super::migration::check_cancel(cancel)?;
        let Some(mut builder) = self.building.take() else {
            return if self.final_index.is_some() {
                Ok(())
            } else {
                Err(changed("Inventory finalization previously failed"))
            };
        };
        builder.flush(cancel)?;
        let mut records = None;
        for run in builder.runs.into_iter().flatten() {
            records = Some(match records {
                Some(previous) => merge::<T>(previous, run, &self.factory, cancel)?,
                None => run,
            });
        }
        let mut records = match records {
            Some(records) => records,
            None => self.factory.allocate(0)?,
        };
        records.seek(SeekFrom::Start(0))?;
        let mut offsets = self.factory.allocate(offset_bytes(self.count)?)?;
        let mut reader = BufReader::new(records.try_clone()?);
        let mut writer = BufWriter::new(&mut offsets);
        let mut offset = 0u64;
        let mut count = 0usize;
        while let Some(bytes) = read_bytes(&mut reader, T::RECORD_BYTES)? {
            super::super::migration::check_cancel(cancel)?;
            if count >= self.count {
                return Err(changed("Inventory run count exceeded bound"));
            }
            writer.write_all(&offset.to_le_bytes())?;
            offset = offset
                .checked_add(4 + u64::try_from(bytes.len()).map_err(io::Error::other)?)
                .ok_or_else(|| changed("Inventory offset overflow"))?;
            count += 1;
        }
        writer.flush()?;
        drop(writer);
        if count != self.count {
            return Err(changed("Inventory run count changed"));
        }
        super::super::migration::check_cancel(cancel)?;
        records.sync_all()?;
        offsets.sync_all()?;
        #[cfg(target_os = "linux")]
        if let Some(scratch) = &self.factory.scratch {
            scratch.require_validation(
                records.metadata()?.len(),
                u64::try_from(count).map_err(io::Error::other)?,
            )?;
        }
        super::super::migration::check_cancel(cancel)?;
        self.final_index = Some(Mutex::new(Index { records, offsets }));
        Ok(())
    }
    fn at(&self, position: usize) -> io::Result<Item<T>> {
        self.factory.check()?;
        let mut index = self
            .final_index
            .as_ref()
            .ok_or_else(|| changed("Inventory not finalized"))?
            .lock()
            .map_err(|_| changed("Inventory index lock failed"))?;
        let location = u64::try_from(position)
            .map_err(io::Error::other)?
            .checked_mul(8)
            .ok_or_else(|| changed("Inventory index overflow"))?;
        index.offsets.seek(SeekFrom::Start(location))?;
        let mut bytes = [0u8; 8];
        index.offsets.read_exact(&mut bytes)?;
        index
            .records
            .seek(SeekFrom::Start(u64::from_le_bytes(bytes)))?;
        read_item::<T>(&mut index.records)?.ok_or_else(|| changed("Inventory index was truncated"))
    }
    pub fn get(&self, path: &Path) -> io::Result<Option<T>> {
        let mut low = 0usize;
        let mut high = self.count;
        while low < high {
            let middle = low + (high - low) / 2;
            let (candidate, fact) = self.at(middle)?;
            match candidate.as_path().cmp(path) {
                std::cmp::Ordering::Less => low = middle + 1,
                std::cmp::Ordering::Greater => high = middle,
                std::cmp::Ordering::Equal => return Ok(Some(fact)),
            }
        }
        Ok(None)
    }
    pub fn first_at_or_after(&self, path: &Path) -> io::Result<Option<(PathBuf, T)>> {
        let mut low = 0usize;
        let mut high = self.count;
        while low < high {
            let middle = low + (high - low) / 2;
            let (candidate, _) = self.at(middle)?;
            if candidate.as_path() < path {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        if low == self.count {
            Ok(None)
        } else {
            self.at(low).map(Some)
        }
    }
    pub fn page(
        &self,
        offset: usize,
        limit: usize,
    ) -> io::Result<impl Iterator<Item = io::Result<(PathBuf, T)>> + '_> {
        if offset > self.count {
            return Err(changed("Inventory page offset exceeds bounds"));
        }
        let end = offset
            .checked_add(limit)
            .ok_or_else(|| changed("Inventory page overflow"))?
            .min(self.count);
        Ok((offset..end).map(|index| self.at(index)))
    }
    pub fn iter(&self) -> impl Iterator<Item = io::Result<Item<T>>> + '_ {
        (0..self.count).map(|index| self.at(index))
    }
}
impl<T: IndexValue> Builder<T> {
    fn flush(&mut self, cancel: &AtomicBool) -> io::Result<()> {
        super::super::migration::check_cancel(cancel)?;
        if self.buffer.is_empty() {
            return Ok(());
        }
        self.buffer.sort_by(|left, right| left.0.cmp(&right.0));
        let size = self.buffer.iter().try_fold(0u64, |total, (path, fact)| {
            super::super::migration::check_cancel(cancel)?;
            let bytes = item_bytes(path, fact)?;
            total
                .checked_add(4 + bytes.len() as u64)
                .ok_or_else(|| changed("Inventory run length overflow"))
        })?;
        let mut file = self.factory.allocate(size)?;
        let mut writer = BufWriter::new(&mut file);
        let mut previous = None;
        for (path, fact) in self.buffer.drain(..) {
            super::super::migration::check_cancel(cancel)?;
            if previous.as_ref() == Some(&path) {
                return Err(changed("Duplicate source inventory path"));
            }
            write_item(&mut writer, &path, &fact)?;
            previous = Some(path);
        }
        writer.flush()?;
        drop(writer);
        self.buffered_bytes = 0;
        if file.stream_position()? != size {
            return Err(changed("Inventory run length changed"));
        }
        let mut level = 0usize;
        loop {
            if level == self.runs.len() {
                self.runs.push(None);
            }
            let slot = self
                .runs
                .get_mut(level)
                .ok_or_else(|| changed("Inventory run level missing"))?;
            if let Some(previous) = slot.take() {
                file = merge::<T>(previous, file, &self.factory, cancel)?;
                level += 1;
            } else {
                *slot = Some(file);
                return Ok(());
            }
        }
    }
}
fn offset_bytes(count: usize) -> io::Result<u64> {
    u64::try_from(count)
        .map_err(io::Error::other)?
        .checked_mul(8)
        .ok_or_else(|| changed("Inventory index length overflow"))
}
fn item_bytes<T: IndexValue>(path: &Path, fact: &T) -> io::Result<Vec<u8>> {
    let bytes = serde_json::to_vec(&(NativePath::encode(path), fact))?;
    if bytes.len() > T::RECORD_BYTES {
        return Err(changed("Inventory record exceeds bound"));
    }
    Ok(bytes)
}
fn write_item<T: IndexValue>(writer: &mut impl Write, path: &Path, fact: &T) -> io::Result<()> {
    let bytes = item_bytes(path, fact)?;
    let length = u32::try_from(bytes.len()).map_err(io::Error::other)?;
    writer.write_all(&length.to_le_bytes())?;
    writer.write_all(&bytes)
}
fn read_bytes(reader: &mut impl Read, record_limit: usize) -> io::Result<Option<Vec<u8>>> {
    let mut length = [0u8; 4];
    let first = length
        .first_mut()
        .ok_or_else(|| changed("Inventory length missing"))?;
    if reader.read(std::slice::from_mut(first))? == 0 {
        return Ok(None);
    }
    reader.read_exact(
        length
            .get_mut(1..)
            .ok_or_else(|| changed("Inventory length missing"))?,
    )?;
    let count = usize::try_from(u32::from_le_bytes(length)).map_err(io::Error::other)?;
    if count == 0 || count > record_limit {
        return Err(changed("Inventory record length invalid"));
    }
    let mut bytes = vec![0; count];
    reader.read_exact(&mut bytes)?;
    Ok(Some(bytes))
}
fn read_item<T: IndexValue>(reader: &mut impl Read) -> io::Result<Option<Item<T>>> {
    read_bytes(reader, T::RECORD_BYTES)?
        .map(|bytes| {
            let (path, fact): (NativePath, T) = serde_json::from_slice(&bytes)?;
            Ok((path.decode().map_err(io::Error::other)?, fact))
        })
        .transpose()
}
fn merge<T: IndexValue>(
    mut left: Run,
    mut right: Run,
    factory: &Factory,
    cancel: &AtomicBool,
) -> io::Result<Run> {
    super::super::migration::check_cancel(cancel)?;
    let bytes = left
        .metadata()?
        .len()
        .checked_add(right.metadata()?.len())
        .ok_or_else(|| changed("Inventory merge length overflow"))?;
    left.seek(SeekFrom::Start(0))?;
    right.seek(SeekFrom::Start(0))?;
    let mut left = BufReader::new(left);
    let mut right = BufReader::new(right);
    let mut output = factory.allocate(bytes)?;
    let mut writer = BufWriter::new(&mut output);
    let mut a = read_item::<T>(&mut left)?;
    let mut b = read_item::<T>(&mut right)?;
    while a.is_some() || b.is_some() {
        super::super::migration::check_cancel(cancel)?;
        let take_left = match (&a, &b) {
            (Some(a), Some(b)) if a.0 == b.0 => {
                return Err(changed("Duplicate source inventory path"));
            }
            (Some(a), Some(b)) => a.0 < b.0,
            (Some(_), None) => true,
            _ => false,
        };
        let item = if take_left { a.take() } else { b.take() }
            .ok_or_else(|| changed("Inventory merge item missing"))?;
        write_item(&mut writer, &item.0, &item.1)?;
        if take_left {
            a = read_item::<T>(&mut left)?;
        } else {
            b = read_item::<T>(&mut right)?;
        }
    }
    writer.flush()?;
    drop(writer);
    if output.stream_position()? != bytes {
        return Err(changed("Inventory merge length changed"));
    }
    Ok(output)
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;
    use crate::migration::PinnedDirectory;
    use crate::migration_review::SourceInventory;
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;

    #[test]
    fn full_home_inventory_spills_and_preserves_revision_checks_and_paths() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().canonicalize().unwrap();
        // Actual transfer inventory, including hidden app state and literal links.
        std::fs::create_dir_all(source.join(".var/app/test/cache")).unwrap();
        for index in (0..(BUFFER * 4 + 3)).rev() {
            std::fs::write(source.join(format!(".var/app/test/cache/{index}")), b"data").unwrap();
        }
        std::os::unix::fs::symlink("cache/1", source.join(".var/app/test/link")).unwrap();
        let pin = PinnedDirectory::open_absolute(&source).unwrap();
        let inventory = SourceInventory::capture_home(&pin).unwrap();
        assert_eq!(inventory.facts.peak_buffer, BUFFER);
        assert!(inventory.facts.building.is_none());
        {
            use std::os::unix::fs::MetadataExt;
            let index = inventory
                .facts
                .final_index
                .as_ref()
                .unwrap()
                .lock()
                .unwrap();
            assert_eq!(index.records.metadata().unwrap().mode() & 0o777, 0o600);
            assert_eq!(index.offsets.metadata().unwrap().mode() & 0o777, 0o600);
            assert_eq!(index.records.metadata().unwrap().nlink(), 0);
            assert_eq!(index.offsets.metadata().unwrap().nlink(), 0);
        }
        assert_eq!(inventory.summary().unwrap().files, (BUFFER * 4 + 3) as u64);
        assert_eq!(inventory.summary().unwrap().links, 1);
        let map: BTreeMap<_, _> = inventory.facts.iter().map(Result::unwrap).collect();
        let mut digest = Sha256::new();
        for (path, fact) in map {
            let bytes = serde_json::to_vec(&(NativePath::encode(&path), fact)).unwrap();
            digest.update((bytes.len() as u64).to_le_bytes());
            digest.update(bytes);
        }
        let expected: [u8; 32] = digest.finalize().into();
        assert_eq!(inventory.revision().unwrap(), expected);
        inventory.validate(&pin).unwrap();
        let cancelled = AtomicBool::new(true);
        assert!(inventory.validate_with_cancel(&pin, &cancelled).is_err());
        assert!(inventory.revision_with_cancel(&cancelled).is_err());
        let changed_path = source.join(".var/app/test/cache/1");
        std::fs::write(&changed_path, b"changed").unwrap();
        let metadata =
            cap_primitives::fs::Metadata::from_file(&File::open(&changed_path).unwrap()).unwrap();
        assert!(
            inventory
                .check(Path::new(".var/app/test/cache/1"), &metadata)
                .is_err()
        );
        assert!(inventory.validate(&pin).is_err());
        assert_eq!(std::fs::read(&changed_path).unwrap(), b"changed");
    }

    #[test]
    fn sorted_runs_reject_cross_run_duplicates_and_corrupt_lengths() {
        let temporary = tempfile::tempdir().unwrap();
        let file = File::create(temporary.path().join("source")).unwrap();
        let metadata = cap_primitives::fs::Metadata::from_file(&file).unwrap();
        let cancel = AtomicBool::new(false);
        let mut duplicate = Storage::new();
        for index in 0..BUFFER {
            duplicate
                .insert(
                    PathBuf::from(format!("{index}")),
                    Fact::capture(&metadata).unwrap(),
                    &cancel,
                )
                .unwrap();
        }
        duplicate
            .insert(
                PathBuf::from("0"),
                Fact::capture(&metadata).unwrap(),
                &cancel,
            )
            .unwrap();
        assert!(duplicate.finish(&cancel).is_err());
        assert!(
            duplicate
                .finish(&cancel)
                .unwrap_err()
                .to_string()
                .contains("previously failed")
        );
        let mut damaged = Storage::new();
        damaged
            .insert(
                PathBuf::from("file"),
                Fact::capture(&metadata).unwrap(),
                &cancel,
            )
            .unwrap();
        damaged.finish(&cancel).unwrap();
        let mut index = damaged.final_index.as_ref().unwrap().lock().unwrap();
        index.records.seek(SeekFrom::Start(0)).unwrap();
        index.records.write_all(&u32::MAX.to_le_bytes()).unwrap();
        drop(index);
        assert!(damaged.get(Path::new("file")).is_err());
        assert!(damaged.iter().next().unwrap().is_err());
    }
}
