//! Content in a `tilo-files` location: how a host whose files have no native
//! path (a browser page) keeps content across restarts.

use std::collections::BTreeSet;
use std::io::Read;

use tilo_files::{Dir, FsError, Kind, Location, VPath, blocking};

use crate::memory::check_new_item;
use crate::{
    Collected, ContentBackend, ContentError, ContentLimits, ContentReader, Stored, hash_bytes,
    valid_hash,
};

/// Content stored one file per address in a [`Location`], through
/// `tilo-files`' synchronous operations (in a page, the profile's mirror of
/// its app files, written through to the origin-private file system).
pub struct FilesBackend {
    location: Location,
    limits: ContentLimits,
}

fn io(error: &FsError) -> ContentError {
    ContentError::Io(error.to_string())
}

impl FilesBackend {
    pub fn new(location: Location, limits: ContentLimits) -> Self {
        Self { location, limits }
    }

    pub fn limits(&self) -> ContentLimits {
        self.limits
    }

    fn dir(&self) -> Result<Dir, ContentError> {
        Dir::open(self.location.clone()).map_err(|error| io(&error))
    }

    fn path(hash: &str) -> Result<VPath, ContentError> {
        if !valid_hash(hash) {
            return Err(ContentError::InvalidHash(hash.to_owned()));
        }
        VPath::new(&hash.to_ascii_lowercase()).map_err(|_| ContentError::InvalidHash(hash.into()))
    }

    /// Every stored address and its size.
    fn entries(&self, dir: &Dir) -> Result<Vec<(String, u64)>, ContentError> {
        let listed = match blocking::list(dir, &VPath::root()) {
            Ok(listed) => listed,
            Err(FsError::NotFound) => return Ok(Vec::new()),
            Err(error) => return Err(io(&error)),
        };
        if listed.len() > self.limits.max_scan_entries {
            return Err(ContentError::ScanLimitExceeded {
                limit: self.limits.max_scan_entries,
            });
        }
        Ok(listed
            .into_iter()
            .filter(|entry| entry.kind == Kind::File && valid_hash(&entry.name))
            .filter_map(|entry| {
                let len = blocking::metadata(dir, &VPath::new(&entry.name).ok()?)
                    .ok()?
                    .len;
                Some((entry.name, len))
            })
            .collect())
    }
}

impl ContentBackend for FilesBackend {
    fn put(&self, reader: &mut dyn Read, _mime: &str) -> Result<Stored, ContentError> {
        let mut bytes = Vec::new();
        let sentinel = u64::try_from(self.limits.max_item_bytes)
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        reader.take(sentinel).read_to_end(&mut bytes)?;
        let hash = hash_bytes(&bytes);
        let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let dir = self.dir()?;
        let path = Self::path(&hash)?;
        let entries = self.entries(&dir)?;
        let replacing = entries.iter().any(|(address, _)| address == &hash);
        let held = entries
            .iter()
            .filter(|(address, _)| address != &hash)
            .fold(0_usize, |sum, (_, len)| {
                sum.saturating_add(usize::try_from(*len).unwrap_or(usize::MAX))
            });
        let count = entries.len().saturating_sub(usize::from(replacing));
        check_new_item(&self.limits, held, count, bytes.len())?;
        // The browser mirror retains bytes even when their OPFS write failed.
        // Presence cannot suppress a retry: queue this exact write again, and
        // replace corrupt bytes at the address without double-counting quota.
        blocking::write_atomic(&dir, &path, &bytes).map_err(|error| io(&error))?;
        Ok(Stored { hash, len })
    }

    fn open(&self, hash: &str) -> Result<Option<ContentReader>, ContentError> {
        let path = Self::path(hash)?;
        match blocking::read(&self.dir()?, &path) {
            Ok(bytes) => Ok(Some(Box::new(std::io::Cursor::new(bytes)))),
            Err(FsError::NotFound) => Ok(None),
            Err(error) => Err(io(&error)),
        }
    }

    fn has(&self, hash: &str) -> bool {
        match (self.dir(), Self::path(hash)) {
            (Ok(dir), Ok(path)) => blocking::metadata(&dir, &path).is_ok(),
            _ => false,
        }
    }

    fn collect_unreferenced(&self, live: &BTreeSet<String>) -> Result<Collected, ContentError> {
        let dir = self.dir()?;
        let mut collected = Collected::default();
        for (hash, len) in self.entries(&dir)? {
            if live.contains(&hash) {
                continue;
            }
            let path = Self::path(&hash)?;
            blocking::remove(&dir, &path).map_err(|error| io(&error))?;
            collected.bytes = collected.bytes.saturating_add(len);
            collected.hashes.push(hash);
        }
        collected.hashes.sort();
        Ok(collected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ContentStore;

    fn store(limits: ContentLimits) -> ContentStore {
        let location = tilo_paths::AppPaths::memory("so.tilo.test").data();
        ContentStore::new(FilesBackend::new(location, limits))
    }

    #[test]
    fn content_round_trips_and_outlives_the_handle() {
        let limits = ContentLimits::default();
        let hash = store(limits).put(b"page content").unwrap();
        let reopened = store(limits);
        assert_eq!(reopened.read(&hash).as_deref(), Some(&b"page content"[..]));
        assert!(!reopened.has(&"0".repeat(64)));
    }

    #[test]
    fn quota_and_collection_count_what_is_stored() {
        let limits = ContentLimits {
            max_total_bytes: 8,
            ..ContentLimits::default()
        };
        let location = tilo_paths::AppPaths::memory("so.tilo.quota").data();
        let store = ContentStore::new(FilesBackend::new(location, limits));
        let kept = store.put(b"12345").unwrap();
        assert!(matches!(
            store.put(b"67890"),
            Err(ContentError::TotalQuotaExceeded { .. })
        ));
        let collected = store
            .collect_unreferenced(&BTreeSet::new())
            .expect("a files store collects");
        assert_eq!(collected.hashes, vec![kept]);
        assert!(store.put(b"67890").is_ok());
    }

    #[test]
    fn retry_reissues_the_content_write_without_consuming_more_quota() {
        let location = tilo_paths::AppPaths::memory("so.tilo.content-retry").data();
        let dir = Dir::open(location.clone()).unwrap();
        let watch = dir.watch(&VPath::root()).unwrap();
        let store = ContentStore::new(FilesBackend::new(
            location,
            ContentLimits {
                max_total_bytes: 5,
                max_entries: 1,
                ..ContentLimits::default()
            },
        ));
        let hash = store.put(b"12345").unwrap();
        assert!(watch.try_next().is_some());
        while watch.try_next().is_some() {}
        assert_eq!(store.put(b"12345").unwrap(), hash);
        assert!(
            watch.try_next().is_some(),
            "a retry must issue another storage write even when the mirror has the bytes"
        );
        assert_eq!(store.read(&hash).as_deref(), Some(&b"12345"[..]));
    }

    #[test]
    fn retry_replaces_corrupt_bytes_at_the_existing_address() {
        let location = tilo_paths::AppPaths::memory("so.tilo.content-repair").data();
        let dir = Dir::open(location.clone()).unwrap();
        let store = ContentStore::new(FilesBackend::new(location, ContentLimits::default()));
        let hash = store.put(b"reviewed image").unwrap();
        blocking::write_atomic(&dir, &FilesBackend::path(&hash).unwrap(), b"corrupt").unwrap();
        assert_ne!(store.read(&hash).as_deref(), Some(&b"reviewed image"[..]));
        assert_eq!(store.put(b"reviewed image").unwrap(), hash);
        assert_eq!(store.read(&hash).as_deref(), Some(&b"reviewed image"[..]));
    }
}
