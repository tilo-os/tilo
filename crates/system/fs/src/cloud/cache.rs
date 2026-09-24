//! The content cache of mounted cloud accounts.
//!
//! Bytes arrive in ranges (a reader touches the middle of a video; a
//! sequential reader pulls windows ahead of itself) and are written into a
//! sparse *partial* file per remote revision, with the resident ranges
//! tracked in the same [`RangeSet`] the Tier 1 placeholders use. Once every
//! byte of a revision is present, the file is hashed with BLAKE3 and
//! promoted into `blobs/<hash>`: from then on the content is addressed by
//! its own hash, so two revisions (or two accounts' copies) with the same
//! bytes share one blob, and an uploaded local file enters the cache under
//! the hash of what was sent.
//!
//! The cache is bounded: when resident bytes exceed the budget the least
//! recently used entries are dropped (partials first by age, then blobs).
//! Nothing here is authoritative — the remote server is — so eviction only
//! costs a re-download. Local edits never live here; they are staged
//! outside the cache until uploaded.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::ops::Range;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use fs_native::ranges::RangeSet;
use serde::{Deserialize, Serialize};

/// The cache key of one remote revision of one item.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RevKey(pub String);

impl RevKey {
    pub fn new(id: &cloud_source::ItemId, revision: Option<&cloud_source::Revision>) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(id.as_str().as_bytes());
        hasher.update(&[0]);
        hasher.update(revision.map_or("", |r| r.as_str()).as_bytes());
        Self(hasher.finalize().to_hex().to_string())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
enum Entry {
    Partial {
        size: u64,
        resident: Vec<(u64, u64)>,
        last_use: u64,
    },
    Blob {
        hash: String,
        size: u64,
        last_use: u64,
    },
}

#[derive(Default, Serialize, Deserialize)]
struct Index {
    entries: HashMap<RevKey, Entry>,
    clock: u64,
}

/// Where a revision's bytes can be read from.
pub enum Resident {
    /// Fully present in a blob or partial file.
    Complete(PathBuf),
    /// These ranges of the request are missing.
    Missing(Vec<Range<u64>>),
}

pub struct BlobCache {
    root: PathBuf,
    budget: u64,
    index: Mutex<Index>,
}

fn ranges_of(set: &RangeSet) -> Vec<(u64, u64)> {
    set.iter().map(|r| (r.start, r.end)).collect()
}

fn set_of(ranges: &[(u64, u64)]) -> RangeSet {
    let mut set = RangeSet::new();
    for (start, end) in ranges {
        set.insert(*start..*end);
    }
    set
}

impl BlobCache {
    /// Open (or create) a cache under `root` holding at most `budget`
    /// resident bytes.
    pub fn open(root: &Path, budget: u64) -> io::Result<Self> {
        fs::create_dir_all(root.join("partial"))?;
        fs::create_dir_all(root.join("blobs"))?;
        let index = fs::read(root.join("index.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Ok(Self {
            root: root.to_path_buf(),
            budget,
            index: Mutex::new(index),
        })
    }

    fn index(&self) -> std::sync::MutexGuard<'_, Index> {
        self.index
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn partial_path(&self, key: &RevKey) -> PathBuf {
        self.root.join("partial").join(&key.0)
    }

    fn blob_path(&self, hash: &str) -> PathBuf {
        self.root.join("blobs").join(hash)
    }

    /// Persist the index.
    pub fn save(&self) -> io::Result<()> {
        let bytes = {
            let index = self.index();
            serde_json::to_vec(&*index).map_err(io::Error::other)?
        };
        atomic_file::write(&self.root.join("index.json"), &bytes)
    }

    /// Resident bytes currently held.
    pub fn used(&self) -> u64 {
        self.index()
            .entries
            .values()
            .map(|entry| match entry {
                Entry::Partial { resident, .. } => resident
                    .iter()
                    .map(|(a, b)| b.saturating_sub(*a))
                    .sum::<u64>(),
                Entry::Blob { size, .. } => *size,
            })
            .sum()
    }

    /// What of `range` is resident for `key` (a revision of `size` bytes).
    pub fn resident(&self, key: &RevKey, size: u64, range: Range<u64>) -> Resident {
        let mut index = self.index();
        index.clock = index.clock.saturating_add(1);
        let now = index.clock;
        let range = range.start.min(size)..range.end.min(size);
        match index.entries.get_mut(key) {
            Some(Entry::Blob { hash, last_use, .. }) => {
                *last_use = now;
                let path = self.blob_path(hash);
                if path.exists() {
                    return Resident::Complete(path);
                }
                index.entries.remove(key);
                Resident::Missing(vec![range])
            }
            Some(Entry::Partial {
                resident,
                last_use,
                size: known,
            }) if *known == size => {
                *last_use = now;
                let mut missing = RangeSet::new();
                if !range.is_empty() {
                    missing.insert(range);
                }
                for r in resident.iter() {
                    missing.remove(r.0..r.1);
                }
                if missing.is_empty() {
                    Resident::Complete(self.partial_path(key))
                } else {
                    Resident::Missing(missing.iter().cloned().collect())
                }
            }
            _ => {
                if size == 0 {
                    return Resident::Complete(self.empty_blob());
                }
                Resident::Missing(if range.is_empty() {
                    Vec::new()
                } else {
                    vec![range]
                })
            }
        }
    }

    fn empty_blob(&self) -> PathBuf {
        let hash = blake3::hash(b"").to_hex().to_string();
        let path = self.blob_path(&hash);
        if !path.exists() {
            let _ = File::create(&path);
        }
        path
    }

    /// Whether every byte of `key` is resident.
    pub fn is_complete(&self, key: &RevKey, size: u64) -> bool {
        matches!(self.resident(key, size, 0..size), Resident::Complete(_))
    }

    /// Write `len` bytes read from `body` at `offset` of revision `key`
    /// (`size` bytes in total), streaming through a small buffer.
    pub fn fill(
        &self,
        key: &RevKey,
        size: u64,
        offset: u64,
        body: &mut dyn Read,
        len: u64,
    ) -> io::Result<()> {
        let path = self.partial_path(key);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)?;
        if file.metadata()?.len() != size {
            file.set_len(size)?;
        }
        let mut buffer = vec![0_u8; 256 * 1024];
        let mut at = offset;
        let end = offset.saturating_add(len).min(size);
        while at < end {
            let want = usize::try_from(end.saturating_sub(at))
                .unwrap_or(usize::MAX)
                .min(buffer.len());
            let chunk = buffer.get_mut(..want).unwrap_or_default();
            let read = body.read(chunk)?;
            if read == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "remote body ended early",
                ));
            }
            file.write_all_at(chunk.get(..read).unwrap_or_default(), at)?;
            at = at.saturating_add(read as u64);
        }
        let complete = {
            let mut index = self.index();
            index.clock = index.clock.saturating_add(1);
            let now = index.clock;
            let entry = index.entries.entry(key.clone()).or_insert(Entry::Partial {
                size,
                resident: Vec::new(),
                last_use: now,
            });
            match entry {
                Entry::Partial {
                    resident,
                    size: known,
                    last_use,
                } => {
                    if *known != size {
                        *known = size;
                        resident.clear();
                    }
                    let mut set = set_of(resident);
                    set.insert(offset..end);
                    *resident = ranges_of(&set);
                    *last_use = now;
                    set.total() >= size
                }
                Entry::Blob { .. } => false,
            }
        };
        if complete {
            self.promote(key, &path, size)?;
        }
        self.evict(Some(key));
        Ok(())
    }

    /// Move a complete partial into the blob store under its hash.
    fn promote(&self, key: &RevKey, path: &Path, size: u64) -> io::Result<()> {
        let hash = hash_file(path)?;
        let target = self.blob_path(&hash);
        if target.exists() {
            fs::remove_file(path)?;
        } else {
            fs::rename(path, &target)?;
        }
        let mut index = self.index();
        let now = index.clock;
        index.entries.insert(
            key.clone(),
            Entry::Blob {
                hash,
                size,
                last_use: now,
            },
        );
        Ok(())
    }

    /// Take a complete file (an uploaded local edit) into the cache as
    /// revision `key`. The file is moved, not copied. Returns its hash.
    pub fn adopt(&self, key: &RevKey, file: &Path) -> io::Result<[u8; 32]> {
        let size = fs::metadata(file)?.len();
        let hash = hash_file(file)?;
        let target = self.blob_path(&hash);
        if target.exists() {
            fs::remove_file(file)?;
        } else if fs::rename(file, &target).is_err() {
            fs::copy(file, &target)?;
            fs::remove_file(file)?;
        }
        {
            let mut index = self.index();
            index.clock = index.clock.saturating_add(1);
            let now = index.clock;
            index.entries.insert(
                key.clone(),
                Entry::Blob {
                    hash: hash.clone(),
                    size,
                    last_use: now,
                },
            );
        }
        self.evict(Some(key));
        let mut out = [0_u8; 32];
        hex::decode_to_slice(&hash, &mut out).map_err(io::Error::other)?;
        Ok(out)
    }

    /// Read `len` bytes at `offset` from a resident path.
    pub fn read_at(path: &Path, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        let file = File::open(path)?;
        let size = file.metadata()?.len();
        let len = usize::try_from(size.saturating_sub(offset))
            .unwrap_or(usize::MAX)
            .min(len);
        let mut out = vec![0_u8; len];
        let mut done = 0_usize;
        while done < len {
            let read = file.read_at(
                out.get_mut(done..).unwrap_or_default(),
                offset.saturating_add(done as u64),
            )?;
            if read == 0 {
                break;
            }
            done = done.saturating_add(read);
        }
        out.truncate(done);
        Ok(out)
    }

    /// Forget a revision (its item changed or went away). A promoted blob
    /// stays until evicted, since other revisions may share it.
    pub fn forget(&self, key: &RevKey) {
        let removed = self.index().entries.remove(key);
        if let Some(Entry::Partial { .. }) = removed {
            let _ = fs::remove_file(self.partial_path(key));
        }
    }

    /// Drop least-recently-used entries until the budget holds, never
    /// `keep` (the entry just filled).
    fn evict(&self, keep: Option<&RevKey>) {
        loop {
            if self.used() <= self.budget {
                return;
            }
            let victim = {
                let index = self.index();
                index
                    .entries
                    .iter()
                    .filter(|(key, _)| Some(*key) != keep)
                    .min_by_key(|(_, entry)| match entry {
                        Entry::Partial { last_use, .. } | Entry::Blob { last_use, .. } => *last_use,
                    })
                    .map(|(key, entry)| (key.clone(), entry.clone()))
            };
            let Some((key, entry)) = victim else {
                return;
            };
            self.index().entries.remove(&key);
            match entry {
                Entry::Partial { .. } => {
                    let _ = fs::remove_file(self.partial_path(&key));
                }
                Entry::Blob { hash, .. } => {
                    let shared = self
                        .index()
                        .entries
                        .values()
                        .any(|e| matches!(e, Entry::Blob { hash: h, .. } if *h == hash));
                    if !shared {
                        let _ = fs::remove_file(self.blob_path(&hash));
                    }
                }
            }
        }
    }
}

/// BLAKE3 of a file, streamed.
pub fn hash_file(path: &Path) -> io::Result<String> {
    let mut hasher = blake3::Hasher::new();
    hasher.update_reader(File::open(path)?)?;
    Ok(hasher.finalize().to_hex().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_fill_promote_and_evict_under_budget() {
        let dir = tempfile::tempdir().unwrap();
        let cache = BlobCache::open(dir.path(), 150).unwrap();
        let a = RevKey("a".into());
        let bytes: Vec<u8> = (0..100_u8).collect();
        assert!(matches!(
            cache.resident(&a, 100, 10..20),
            Resident::Missing(m) if m.len() == 1 && m.first() == Some(&(10..20))
        ));
        cache.fill(&a, 100, 10, &mut &bytes[10..20], 10).unwrap();
        match cache.resident(&a, 100, 0..30) {
            Resident::Missing(m) => assert_eq!(m, vec![0..10, 20..30]),
            Resident::Complete(_) => panic!("only 10..20 is resident"),
        }
        cache.fill(&a, 100, 0, &mut &bytes[..], 100).unwrap();
        let Resident::Complete(path) = cache.resident(&a, 100, 0..100) else {
            panic!("complete");
        };
        assert!(path.starts_with(dir.path().join("blobs")));
        assert_eq!(BlobCache::read_at(&path, 95, 10).unwrap(), &bytes[95..]);

        // A second 100-byte revision pushes the first out of a 150-byte budget.
        let b = RevKey("b".into());
        let other = [7_u8; 100];
        cache.fill(&b, 100, 0, &mut &other[..], 100).unwrap();
        assert!(cache.used() <= 150);
        assert!(!cache.is_complete(&a, 100));
        assert!(cache.is_complete(&b, 100));

        // Identical bytes share one blob.
        let c = RevKey("c".into());
        cache.fill(&c, 100, 0, &mut &other[..], 100).unwrap();
        cache.save().unwrap();
        let reopened = BlobCache::open(dir.path(), 1_000).unwrap();
        assert!(reopened.is_complete(&c, 100));
    }
}
