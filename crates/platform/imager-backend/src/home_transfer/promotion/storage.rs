//! Private derived metadata cache. The durable v1 journal and independently
//! verified archive remain authority; row references never enter app requests.
use super::*;
use crate::migration_review::{InventoryScratch, storage::scratch::Allocation};
use serde::ser::SerializeSeq;
use std::{marker::PhantomData, os::unix::fs::FileExt, sync::Mutex};
const NONE: u64 = u64::MAX;
const ROW_BYTES: usize = 1024 * 1024;
// Each derived file keeps its existing 256 MiB bound. The actual retained
// scratch quota controls aggregate allocation; no segment is preallocated.
const CACHE_BYTES: u64 = 4 * MAX as u64;
const CACHE_SEGMENTS: usize = 512;
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Range {
    head: u64,
    tail: u64,
    count: usize,
}
struct Backing {
    allocations: Vec<Allocation>,
    frozen: bool,
    failed: bool,
}
pub(super) struct Store {
    backing: Mutex<Backing>,
    scratch: Arc<InventoryScratch>,
    cancel: Arc<AtomicBool>,
}
impl Store {
    pub(super) fn new(
        scratch: Arc<InventoryScratch>,
        cancel: Arc<AtomicBool>,
    ) -> io::Result<Arc<Self>> {
        super::super::super::migration::check_cancel(&cancel)?;
        let allocation = scratch.allocate(0)?;
        Ok(Arc::new(Self {
            backing: Mutex::new(Backing {
                allocations: vec![allocation],
                frozen: false,
                failed: false,
            }),
            scratch,
            cancel,
        }))
    }
    pub(super) fn builder<T: Row>(self: &Arc<Self>) -> Builder<T> {
        Builder {
            store: self.clone(),
            range: Range {
                head: NONE,
                tail: NONE,
                count: 0,
            },
            _row: PhantomData,
        }
    }
    pub(super) fn sequence<T: Row>(self: &Arc<Self>, range: Range) -> io::Result<Sequence<T>> {
        if range.count > ENTRIES
            || (range.count == 0) != (range.head == NONE && range.tail == NONE)
            || (range.count != 0 && (range.head > range.tail || range.tail == NONE))
        {
            return Err(changed("Invalid retained metadata sequence"));
        }
        Ok(Sequence {
            store: self.clone(),
            range,
            _row: PhantomData,
        })
    }
    fn append(&self, bytes: &[u8], previous: u64) -> io::Result<u64> {
        super::super::super::migration::check_cancel(&self.cancel)?;
        if bytes.is_empty() || bytes.len() > ROW_BYTES {
            return Err(changed("Retained metadata row exceeds bounds"));
        }
        let mut backing = self
            .backing
            .lock()
            .map_err(|_| changed("Retained metadata lock poisoned"))?;
        if backing.frozen || backing.failed {
            return Err(changed("Retained metadata writer is closed"));
        }
        let row_bytes = 12u64
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| changed("Retained metadata row length overflow"))?;
        let last = backing.allocations.len() - 1;
        let last_length = backing.allocations[last].file.metadata()?.len();
        if last_length
            .checked_add(row_bytes)
            .is_none_or(|n| n > CACHE_BYTES)
        {
            if backing.allocations.len() == CACHE_SEGMENTS {
                return Err(changed("Retained metadata segment count exceeds bounds"));
            }
            backing.allocations.push(self.scratch.allocate(0)?);
        }
        let segment = backing.allocations.len() - 1;
        let local = backing.allocations[segment].file.metadata()?.len();
        let offset = (segment as u64)
            .checked_mul(CACHE_BYTES)
            .and_then(|base| base.checked_add(local))
            .ok_or_else(|| changed("Retained metadata offset overflow"))?;
        let length = local
            .checked_add(row_bytes)
            .filter(|n| *n <= CACHE_BYTES)
            .ok_or_else(|| changed("Retained metadata row exceeds segment bounds"))?;
        let result = (|| {
            backing.allocations[segment].grow(length)?;
            let file = &backing.allocations[segment].file;
            file.write_all_at(&(bytes.len() as u32).to_le_bytes(), local)?;
            file.write_all_at(&NONE.to_le_bytes(), local + 4)?;
            file.write_all_at(bytes, local + 12)?;
            if previous != NONE {
                if previous >= offset {
                    return Err(changed("Retained metadata sequence regressed"));
                }
                let index = usize::try_from(previous / CACHE_BYTES).map_err(io::Error::other)?;
                let previous_file = backing
                    .allocations
                    .get(index)
                    .ok_or_else(|| changed("Retained metadata predecessor segment missing"))?;
                previous_file
                    .file
                    .write_all_at(&offset.to_le_bytes(), previous % CACHE_BYTES + 4)?;
            }
            Ok(offset)
        })();
        if result.is_err() {
            backing.failed = true;
        }
        result
    }
    fn read(&self, offset: u64) -> io::Result<(u64, Vec<u8>)> {
        super::super::super::migration::check_cancel(&self.cancel)?;
        self.scratch.check_custody()?;
        let backing = self
            .backing
            .lock()
            .map_err(|_| changed("Retained metadata lock poisoned"))?;
        if backing.failed {
            return Err(changed("Retained metadata cache failed"));
        }
        let segment = usize::try_from(offset / CACHE_BYTES).map_err(io::Error::other)?;
        let local = offset % CACHE_BYTES;
        let file = &backing
            .allocations
            .get(segment)
            .ok_or_else(|| changed("Retained metadata segment missing"))?
            .file;
        let mut header = [0u8; 12];
        file.read_exact_at(&mut header, local)?;
        let length = u32::from_le_bytes(header[..4].try_into().map_err(io::Error::other)?) as usize;
        let next = u64::from_le_bytes(header[4..].try_into().map_err(io::Error::other)?);
        let file_length = file.metadata()?.len();
        if length == 0
            || length > ROW_BYTES
            || next != NONE && next <= offset
            || local
                .checked_add(12)
                .and_then(|n| n.checked_add(length as u64))
                .is_none_or(|n| n > file_length)
        {
            return Err(changed("Retained metadata row framing changed"));
        }
        let mut bytes = vec![0; length];
        file.read_exact_at(&mut bytes, local + 12)?;
        Ok((next, bytes))
    }
    pub(super) fn freeze(&self) -> io::Result<()> {
        super::super::super::migration::check_cancel(&self.cancel)?;
        let mut backing = self
            .backing
            .lock()
            .map_err(|_| changed("Retained metadata lock poisoned"))?;
        if backing.failed {
            return Err(changed("Retained metadata cache failed"));
        }
        for allocation in &backing.allocations {
            durable::sync_file(&allocation.file)?;
        }
        self.scratch.check_restoration()?;
        backing.frozen = true;
        Ok(())
    }
    pub(super) fn scratch(&self) -> Arc<InventoryScratch> {
        self.scratch.clone()
    }
}
pub(super) trait Row: Serialize + Sized {
    fn encode(&self) -> io::Result<Vec<u8>>;
    fn decode(bytes: &[u8], store: &Arc<Store>) -> io::Result<Self>;
}
impl Row for NativePath {
    fn encode(&self) -> io::Result<Vec<u8>> {
        serde_json::to_vec(self).map_err(io::Error::other)
    }
    fn decode(bytes: &[u8], _: &Arc<Store>) -> io::Result<Self> {
        serde_json::from_slice(bytes).map_err(io::Error::other)
    }
}
pub(super) struct Sequence<T: Row> {
    store: Arc<Store>,
    range: Range,
    _row: PhantomData<T>,
}
impl<T: Row> Clone for Sequence<T> {
    fn clone(&self) -> Self {
        Self {
            store: self.store.clone(),
            range: self.range,
            _row: PhantomData,
        }
    }
}
impl<T: Row> Sequence<T> {
    pub(super) fn len(&self) -> usize {
        self.range.count
    }
    pub(super) fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub(super) fn range(&self) -> Range {
        self.range
    }
    pub(super) fn store(&self) -> Arc<Store> {
        self.store.clone()
    }
    pub(super) fn iter(&self) -> Iter<T> {
        Iter {
            store: self.store.clone(),
            next: self.range.head,
            tail: self.range.tail,
            remaining: self.range.count,
            _row: PhantomData,
        }
    }
}
impl<T: Row> Serialize for Sequence<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.len()))?;
        for row in self.iter() {
            let row = row.map_err(serde::ser::Error::custom)?;
            sequence.serialize_element(&row)?;
        }
        sequence.end()
    }
}
pub(super) struct Builder<T: Row> {
    store: Arc<Store>,
    range: Range,
    _row: PhantomData<T>,
}
impl<T: Row> Builder<T> {
    pub(super) fn len(&self) -> usize {
        self.range.count
    }
    pub(super) fn store(&self) -> Arc<Store> {
        self.store.clone()
    }
    pub(super) fn push(&mut self, row: T) -> io::Result<()> {
        let count = self
            .range
            .count
            .checked_add(1)
            .filter(|n| *n <= ENTRIES)
            .ok_or_else(|| changed("Retained metadata sequence exceeds entry bounds"))?;
        let offset = self.store.append(&row.encode()?, self.range.tail)?;
        if self.range.count == 0 {
            self.range.head = offset;
        }
        self.range.tail = offset;
        self.range.count = count;
        Ok(())
    }
    pub(super) fn finish(self) -> Sequence<T> {
        Sequence {
            store: self.store,
            range: self.range,
            _row: PhantomData,
        }
    }
}
pub(super) struct Iter<T: Row> {
    store: Arc<Store>,
    next: u64,
    tail: u64,
    remaining: usize,
    _row: PhantomData<T>,
}
impl<T: Row> Iterator for Iter<T> {
    type Item = io::Result<T>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let result = (|| {
            if self.next == NONE || self.remaining == 1 && self.next != self.tail {
                return Err(changed("Retained metadata sequence was truncated"));
            }
            let (next, bytes) = self.store.read(self.next)?;
            if self.remaining == 1 && next != NONE {
                return Err(changed("Retained metadata tail changed"));
            }
            let row = T::decode(&bytes, &self.store)?;
            self.next = next;
            self.remaining -= 1;
            Ok(row)
        })();
        if result.is_err() {
            self.remaining = 0;
        }
        Some(result)
    }
}
