//! Content held in memory, for tests and headless tools.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::sync::RwLock;

use crate::{
    Collected, ContentBackend, ContentError, ContentLimits, ContentReader, Stored, hash_bytes,
};

/// Content that lives as long as the backend.
pub struct MemoryBackend {
    limits: ContentLimits,
    inner: RwLock<State>,
}

#[derive(Default)]
struct State {
    bytes: usize,
    entries: BTreeMap<String, std::sync::Arc<[u8]>>,
}

impl Default for MemoryBackend {
    fn default() -> Self {
        Self::new(ContentLimits::default())
    }
}

impl MemoryBackend {
    pub fn new(limits: ContentLimits) -> Self {
        Self {
            limits,
            inner: RwLock::new(State::default()),
        }
    }

    pub fn limits(&self) -> ContentLimits {
        self.limits
    }
}

impl ContentBackend for MemoryBackend {
    fn put(&self, reader: &mut dyn Read, _mime: &str) -> Result<Stored, ContentError> {
        let mut bytes = Vec::new();
        let sentinel = u64::try_from(self.limits.max_item_bytes)
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        reader.take(sentinel).read_to_end(&mut bytes)?;
        let hash = hash_bytes(&bytes);
        let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let mut state = self
            .inner
            .write()
            .map_err(|_| ContentError::Io("memory store lock poisoned".into()))?;
        // Re-storing content already retained consumes no more quota.
        if state.entries.contains_key(&hash) {
            return Ok(Stored { hash, len });
        }
        check_new_item(&self.limits, state.bytes, state.entries.len(), bytes.len())?;
        state.bytes = state.bytes.saturating_add(bytes.len());
        state.entries.insert(hash.clone(), bytes.into());
        drop(state);
        Ok(Stored { hash, len })
    }

    fn open(&self, hash: &str) -> Result<Option<ContentReader>, ContentError> {
        let state = self
            .inner
            .read()
            .map_err(|_| ContentError::Io("memory store lock poisoned".into()))?;
        let key = hash.to_ascii_lowercase();
        Ok(state.entries.get(&key).map(|bytes| {
            Box::new(std::io::Cursor::new(std::sync::Arc::clone(bytes))) as ContentReader
        }))
    }

    fn has(&self, hash: &str) -> bool {
        self.inner
            .read()
            .is_ok_and(|state| state.entries.contains_key(&hash.to_ascii_lowercase()))
    }

    fn collect_unreferenced(&self, live: &BTreeSet<String>) -> Result<Collected, ContentError> {
        let mut state = self
            .inner
            .write()
            .map_err(|_| ContentError::Io("memory store lock poisoned".into()))?;
        let unreferenced: Vec<String> = state
            .entries
            .keys()
            .filter(|hash| !live.contains(*hash))
            .cloned()
            .collect();
        let mut collected = Collected::default();
        for hash in unreferenced {
            if let Some(bytes) = state.entries.remove(&hash) {
                state.bytes = state.bytes.saturating_sub(bytes.len());
                collected.bytes = collected
                    .bytes
                    .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
                collected.hashes.push(hash);
            }
        }
        drop(state);
        Ok(collected)
    }
}

/// Whether one more item of `incoming` bytes fits `limits` on top of what is held.
pub(crate) fn check_new_item(
    limits: &ContentLimits,
    current_bytes: usize,
    current_entries: usize,
    incoming: usize,
) -> Result<(), ContentError> {
    if incoming > limits.max_item_bytes {
        return Err(ContentError::ItemTooLarge {
            actual: incoming,
            limit: limits.max_item_bytes,
        });
    }
    if current_entries >= limits.max_entries {
        return Err(ContentError::EntryLimitExceeded {
            limit: limits.max_entries,
        });
    }
    let attempted = current_bytes.saturating_add(incoming);
    if attempted > limits.max_total_bytes {
        return Err(ContentError::TotalQuotaExceeded {
            attempted,
            limit: limits.max_total_bytes,
        });
    }
    Ok(())
}
