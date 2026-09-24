//! Inert disk staging. Callers retain the original admitted request and revalidate
//! actual authority before I/O handoff and native completion; a prefix is no receipt.
use crate::{
    host_bytes::{ByteCredit, HostBytes},
    staging_manifest::{StageIdentity, StageManifest},
    store::StagingReservation,
};
use std::{
    fs::File,
    io::{Read, Seek, Write},
};
use tilo_relay::delivery_v3::state::{FlowCredit, StateItemChunk};
const UPLOAD_IDLE_NS: u64 = 300_000_000_000;
const SCRATCH_BYTES: usize = 64 * 1024;
#[derive(Debug, PartialEq, Eq)]
pub enum UploadFailure {
    Invalid,
    Capacity,
    Conflict,
    Expired,
    Storage,
    Corrupt,
}
impl std::fmt::Display for UploadFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "bounded upload staging failed: {self:?}")
    }
}
impl std::error::Error for UploadFailure {}
/// Field order retires allocated input before its actual host credit.
pub struct UploadChunk {
    bytes: Vec<u8>,
    delivery: [u8; 16],
    digest: [u8; 32],
    total: u64,
    offset: u64,
    pool: HostBytes,
    _credit: ByteCredit,
}
impl UploadChunk {
    pub fn copy(chunk: &StateItemChunk<'_>, pool: &HostBytes) -> Result<Self, UploadFailure> {
        // Public Deserialize yields inert fields, not a checked frame witness.
        chunk.encoded_len().map_err(|_| UploadFailure::Invalid)?;
        if chunk.canonical_size() == 0 || chunk.canonical_size() > 64 * 1024 * 1024 {
            return Err(UploadFailure::Conflict);
        }
        let amount = chunk
            .bytes()
            .len()
            .checked_add(size_of::<Self>())
            .ok_or(UploadFailure::Capacity)?;
        let credit = pool
            .try_acquire_bulk(amount)
            .ok_or(UploadFailure::Capacity)?;
        let bytes = chunk.bytes().to_vec();
        if bytes.capacity() > chunk.bytes().len() {
            return Err(UploadFailure::Capacity);
        }
        Ok(Self {
            bytes,
            delivery: chunk.delivery(),
            digest: chunk.input_digest(),
            total: chunk.canonical_size(),
            offset: chunk.offset(),
            pool: pool.clone(),
            _credit: credit,
        })
    }
}
/// One file/hash state moves through exactly one blocking worker; never Clone.
/// File and allocated scratch are destroyed before custody/credit on cancellation.
pub struct UploadStage {
    file: File,
    metadata: StageManifest,
    hasher: blake3::Hasher,
    scratch: Vec<u8>,
    reservation: StagingReservation,
    credit: ByteCredit,
}
/// Complete bytes are still inert and require the writer's genuine StateStaged turn.
pub struct CompletedUpload {
    file: File,
    identity: StageIdentity,
    reservation: StagingReservation,
    credit: ByteCredit,
}
impl CompletedUpload {
    pub fn identity(&self) -> &StageIdentity {
        &self.identity
    }
    pub fn into_parts(self) -> (File, StageIdentity, StagingReservation, ByteCredit) {
        (self.file, self.identity, self.reservation, self.credit)
    }
}
fn storage(_: std::io::Error) -> UploadFailure {
    UploadFailure::Storage
}
fn host(error: crate::store::HostWorkFailure) -> UploadFailure {
    match error {
        crate::store::HostWorkFailure::Capacity => UploadFailure::Capacity,
        crate::store::HostWorkFailure::Conflict => UploadFailure::Conflict,
        crate::store::HostWorkFailure::Unavailable => UploadFailure::Storage,
    }
}
impl UploadStage {
    /// Called inside a bounded blocking worker. No native authority is retained here.
    pub fn open(
        reservation: StagingReservation,
        pool: &HostBytes,
        now: u64,
    ) -> Result<Self, UploadFailure> {
        if !reservation.same_pool(pool) {
            return Err(UploadFailure::Conflict);
        }
        let credit = pool
            .try_acquire_bulk(SCRATCH_BYTES + size_of::<Self>())
            .ok_or(UploadFailure::Capacity)?;
        let mut scratch = vec![0; SCRATCH_BYTES];
        if scratch.capacity() > SCRATCH_BYTES {
            return Err(UploadFailure::Capacity);
        }
        let existing = reservation.read_manifest().map_err(host)?;
        let identity = *reservation.identity();
        let metadata = match existing {
            Some(metadata) => metadata,
            None => StageManifest {
                identity,
                prefix: 0,
                expires_at_ns: now
                    .checked_add(UPLOAD_IDLE_NS)
                    .ok_or(UploadFailure::Expired)?,
                prefix_digest: *blake3::hash(&[]).as_bytes(),
            },
        };
        if metadata.expires_at_ns <= now {
            return Err(UploadFailure::Expired);
        }
        let mut file = if existing.is_some() {
            reservation.open_data(false).map_err(host)?
        } else {
            // A recognized data file without metadata contains no committed prefix.
            // Create-new never overwrites it; reopening still validates private file custody.
            match reservation.open_data(true) {
                Ok(file) => file,
                Err(_) => reservation.open_data(false).map_err(host)?,
            }
        };
        let length = file.metadata().map_err(storage)?.len();
        if length < metadata.prefix || length > identity.total {
            return Err(UploadFailure::Corrupt);
        }
        let mut hasher = blake3::Hasher::new();
        let mut remaining = metadata.prefix;
        file.seek(std::io::SeekFrom::Start(0)).map_err(storage)?;
        while remaining > 0 {
            let n = remaining.min(SCRATCH_BYTES as u64) as usize;
            file.read_exact(&mut scratch[..n]).map_err(storage)?;
            hasher.update(&scratch[..n]);
            remaining -= n as u64;
        }
        if *hasher.finalize().as_bytes() != metadata.prefix_digest {
            return Err(UploadFailure::Corrupt);
        }
        if length != metadata.prefix {
            reservation.preflight().map_err(host)?;
            file.set_len(metadata.prefix).map_err(storage)?;
            file.sync_all().map_err(storage)?;
        }
        if existing.is_none() {
            file.sync_all().map_err(storage)?;
            reservation.write_manifest(&metadata).map_err(host)?;
        } else {
            // A prior rename may have become visible without its directory
            // barrier succeeding. Never expose that prefix until recovery syncs.
            reservation.recover_barrier().map_err(host)?;
        }
        Ok(Self {
            file,
            metadata,
            hasher,
            scratch,
            reservation,
            credit,
        })
    }
    /// Compare all overlap before writing; prefix publication follows both file barriers.
    pub fn append(mut self, chunk: UploadChunk, now: u64) -> Result<Self, UploadFailure> {
        let identity = self.metadata.identity;
        if !self.reservation.same_pool(&chunk.pool) {
            return Err(UploadFailure::Conflict);
        }
        if chunk.delivery != identity.delivery
            || chunk.digest != identity.digest
            || chunk.total != identity.total
        {
            return Err(UploadFailure::Conflict);
        }
        if self.metadata.expires_at_ns <= now {
            return Err(UploadFailure::Expired);
        }
        let end = chunk
            .offset
            .checked_add(chunk.bytes.len() as u64)
            .ok_or(UploadFailure::Conflict)?;
        if end > identity.total {
            return Err(UploadFailure::Conflict);
        }
        if chunk.offset > self.metadata.prefix {
            return Ok(self);
        } // A gap returns only the actual durable prefix.
        let overlap = (self.metadata.prefix - chunk.offset).min(chunk.bytes.len() as u64) as usize;
        self.file
            .seek(std::io::SeekFrom::Start(chunk.offset))
            .map_err(storage)?;
        let mut compared = 0;
        while compared < overlap {
            let n = (overlap - compared).min(self.scratch.len());
            self.file
                .read_exact(&mut self.scratch[..n])
                .map_err(storage)?;
            if self.scratch[..n] != chunk.bytes[compared..compared + n] {
                return Err(UploadFailure::Conflict);
            }
            compared += n;
        }
        if end <= self.metadata.prefix {
            return Ok(self);
        } // Duplicates never refresh inactivity expiry.
        let expiry = now
            .checked_add(UPLOAD_IDLE_NS)
            .ok_or(UploadFailure::Expired)?;
        let suffix = &chunk.bytes[overlap..];
        let mut next_hash = self.hasher.clone();
        next_hash.update(suffix);
        let digest = *next_hash.finalize().as_bytes();
        if end == identity.total && digest != identity.digest {
            return Err(UploadFailure::Conflict);
        }
        let metadata = StageManifest {
            identity,
            prefix: end,
            expires_at_ns: expiry,
            prefix_digest: digest,
        };
        self.reservation.preflight().map_err(host)?;
        self.file
            .seek(std::io::SeekFrom::Start(self.metadata.prefix))
            .map_err(storage)?;
        self.file.write_all(suffix).map_err(storage)?;
        self.file.sync_all().map_err(storage)?;
        self.reservation.write_manifest(&metadata).map_err(host)?;
        self.metadata = metadata;
        self.hasher = next_hash;
        Ok(self)
    }
    pub fn identity(&self) -> &StageIdentity {
        &self.metadata.identity
    }
    pub fn progress(&self) -> Result<FlowCredit, UploadFailure> {
        let id = self.metadata.identity;
        FlowCredit::new(id.delivery, id.digest, id.total, self.metadata.prefix)
            .map_err(|_| UploadFailure::Corrupt)
    }
    /// This handoff is complete inert bytes, never an admission receipt or authority.
    pub fn complete(mut self) -> Result<CompletedUpload, UploadFailure> {
        if self.metadata.prefix != self.metadata.identity.total
            || *self.hasher.finalize().as_bytes() != self.metadata.identity.digest
        {
            return Err(UploadFailure::Conflict);
        }
        self.file
            .seek(std::io::SeekFrom::Start(0))
            .map_err(storage)?;
        let Self {
            file,
            metadata,
            hasher,
            scratch,
            reservation,
            credit,
        } = self;
        drop(scratch);
        drop(hasher);
        Ok(CompletedUpload {
            file,
            identity: metadata.identity,
            reservation,
            credit,
        })
    }
}
#[cfg(test)]
#[path = "upload/owner_test_support.rs"]
mod owner_test_support;
#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) use owner_test_support::stage_for_owner_test;
