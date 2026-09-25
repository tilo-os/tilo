//! Inert stream over actual owned publication backings, never snapshot authority.
use crate::{
    host_bytes::{ByteCredit, HostBytes},
    publication_custody::PublicationCustody,
};
use tilo_relay::delivery_v3::{
    commit::Digest,
    state::{
        MAX_SNAPSHOT_CHUNKS, MAX_TRANSFER_ITEMS, SNAPSHOT_CHUNK_BYTES, TransferKind,
        TransferRecordHeader, VisibleOrder,
    },
};
#[derive(Debug, PartialEq, Eq)]
pub enum SnapshotFailure {
    Capacity,
    Invalid,
    Unsupported,
}
pub struct SnapshotStream {
    pin: PublicationCustody,
    records: Vec<Record>,
    hashes: Vec<Digest>,
    total: u64,
    visible: Digest,
    _credit: ByteCredit,
}
struct Record {
    offset: u64,
    header: [u8; 45],
}
impl SnapshotStream {
    pub fn prepare(pin: PublicationCustody, pool: &HostBytes) -> Result<Self, SnapshotFailure> {
        if !pin.same_pool(pool) || pin.len() == 0 || pin.len() > MAX_TRANSFER_ITEMS as usize {
            return Err(SnapshotFailure::Invalid);
        }
        // Count from actual original roots before allocating any index or copy.
        let mut canonical = 0u64;
        for i in 0..pin.len() {
            let bytes = pin.get(i).ok_or(SnapshotFailure::Invalid)?.bytes;
            if bytes.is_empty() || bytes.len() > 64 * 1024 * 1024 {
                return Err(SnapshotFailure::Capacity);
            }
            canonical = canonical
                .checked_add(bytes.len() as u64)
                .ok_or(SnapshotFailure::Capacity)?;
        }
        if canonical > 256 * 1024 * 1024 {
            return Err(SnapshotFailure::Capacity);
        }
        let total = canonical
            .checked_add((pin.len() as u64) * 45)
            .ok_or(SnapshotFailure::Capacity)?;
        let chunks = total.div_ceil(SNAPSHOT_CHUNK_BYTES as u64) as usize;
        if chunks > MAX_SNAPSHOT_CHUNKS as usize {
            return Err(SnapshotFailure::Capacity);
        }
        let retained = pin
            .len()
            .checked_mul(size_of::<Record>())
            .and_then(|bytes| {
                chunks
                    .checked_mul(size_of::<Digest>())
                    .and_then(|hashes| bytes.checked_add(hashes))
            })
            .ok_or(SnapshotFailure::Capacity)?;
        let mut credit = pool
            .try_acquire_bulk(
                retained
                    .checked_add(SNAPSHOT_CHUNK_BYTES)
                    .ok_or(SnapshotFailure::Capacity)?,
            )
            .ok_or(SnapshotFailure::Capacity)?;
        let scratch_credit = credit
            .split(SNAPSHOT_CHUNK_BYTES)
            .ok_or(SnapshotFailure::Capacity)?;
        let mut records = Vec::with_capacity(pin.len());
        let hashes = Vec::with_capacity(chunks);
        let mut scratch = vec![0; SNAPSHOT_CHUNK_BYTES];
        if records.capacity() != pin.len()
            || hashes.capacity() != chunks
            || scratch.capacity() != SNAPSHOT_CHUNK_BYTES
        {
            return Err(SnapshotFailure::Capacity);
        }
        let mut offset = 0;
        let mut order =
            VisibleOrder::new(pin.len() as u32).map_err(|_| SnapshotFailure::Invalid)?;
        for i in 0..pin.len() {
            let bytes = pin.get(i).ok_or(SnapshotFailure::Invalid)?.bytes;
            let digest = *blake3::hash(bytes).as_bytes();
            if pin.reference(i).ok_or(SnapshotFailure::Invalid)?.payload != digest {
                return Err(SnapshotFailure::Invalid);
            }
            let header = TransferRecordHeader::new(
                i as u32,
                native_family(bytes)?,
                bytes.len() as u64,
                digest,
            )
            .map_err(|_| SnapshotFailure::Invalid)?;
            records.push(Record {
                offset,
                header: header.encode(),
            });
            order
                .push(i as u32, digest)
                .map_err(|_| SnapshotFailure::Invalid)?;
            offset += 45 + bytes.len() as u64;
        }
        let mut stream = Self {
            pin,
            records,
            hashes,
            total,
            visible: order.finish().map_err(|_| SnapshotFailure::Invalid)?,
            _credit: credit,
        };
        for index in 0..chunks {
            let length = stream.fill_chunk(index as u32, &mut scratch)?;
            stream
                .hashes
                .push(*blake3::hash(&scratch[..length]).as_bytes());
        }
        drop(scratch);
        drop(scratch_credit);
        Ok(stream)
    }
    /// Writes into caller-owned scratch; it must remain charged by the original
    /// request/worker. This returns bytes only, never a boundary or authority.
    pub fn fill_chunk(&self, index: u32, output: &mut [u8]) -> Result<usize, SnapshotFailure> {
        let start = u64::from(index)
            .checked_mul(SNAPSHOT_CHUNK_BYTES as u64)
            .ok_or(SnapshotFailure::Invalid)?;
        if start >= self.total {
            return Err(SnapshotFailure::Invalid);
        }
        let length = (self.total - start).min(SNAPSHOT_CHUNK_BYTES as u64) as usize;
        if output.len() < length {
            return Err(SnapshotFailure::Capacity);
        }
        let mut position = start;
        let mut written = 0;
        let mut record = self
            .records
            .partition_point(|r| r.offset <= start)
            .checked_sub(1)
            .ok_or(SnapshotFailure::Invalid)?;
        while written < length {
            let indexed = self.records.get(record).ok_or(SnapshotFailure::Invalid)?;
            let bytes = self.pin.get(record).ok_or(SnapshotFailure::Invalid)?.bytes;
            let within =
                usize::try_from(position - indexed.offset).map_err(|_| SnapshotFailure::Invalid)?;
            let source = if within < 45 {
                &indexed.header[within..]
            } else {
                bytes.get(within - 45..).ok_or(SnapshotFailure::Invalid)?
            };
            let count = source.len().min(length - written);
            if count == 0 {
                return Err(SnapshotFailure::Invalid);
            }
            output[written..written + count].copy_from_slice(&source[..count]);
            written += count;
            position += count as u64;
            if position == indexed.offset + 45 + bytes.len() as u64 {
                record += 1;
            }
        }
        Ok(length)
    }
    pub fn chunk_digests(&self) -> &[Digest] {
        &self.hashes
    }
    pub fn stream_bytes(&self) -> u64 {
        self.total
    }
    pub fn visible_order(&self) -> Digest {
        self.visible
    }
    pub fn item_count(&self) -> usize {
        self.pin.len()
    }
    /// Borrow original charged roots for a concrete owner's fresh checks.
    /// Byte custody itself is never selection or current authorization evidence.
    pub fn custody(&self) -> &PublicationCustody {
        &self.pin
    }
}
// This classifier applies only to original already-admitted pinned bytes. It
// does not decode, authorize or admit arbitrary native input.
fn native_family(bytes: &[u8]) -> Result<TransferKind, SnapshotFailure> {
    match bytes.first() {
        Some(1) => Ok(TransferKind::Governance),
        Some(12) => Ok(TransferKind::State),
        _ => Err(SnapshotFailure::Unsupported),
    }
}
pub(crate) mod pair;
#[cfg(test)]
mod tests;
