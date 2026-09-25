//! Actual immutable publication byte custody. Never owner or snapshot authority.
use crate::{
    host_bytes::{ByteCredit, HostBytes},
    owner::{
        PublicationView,
        resources::{AccountedCredit, RetainedSerializedBytes},
    },
};
use std::sync::Arc;
use tilo_relay::delivery_v3::commit::{
    CommitError, MAX_REFERENCES, PublicationPayload, PublicationPool, PublicationRef,
};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicationCustodyFailure {
    Capacity,
    InvalidPayload,
}
impl std::fmt::Display for PublicationCustodyFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Capacity => "publication byte custody capacity unavailable",
            Self::InvalidPayload => "publication byte custody has invalid retained roots",
        })
    }
}
impl std::error::Error for PublicationCustodyFailure {}
fn invalid(error: CommitError) -> PublicationCustodyFailure {
    match error {
        CommitError::Quota => PublicationCustodyFailure::Capacity,
        _ => PublicationCustodyFailure::InvalidPayload,
    }
}
fn count(value: usize) -> Result<usize, PublicationCustodyFailure> {
    if value == 0 || value > MAX_REFERENCES {
        Err(PublicationCustodyFailure::Capacity)
    } else {
        Ok(value)
    }
}
fn allocation(count: usize, element: usize) -> Result<usize, PublicationCustodyFailure> {
    count
        .checked_mul(element)
        .ok_or(PublicationCustodyFailure::Capacity)
}
struct PinnedPublication {
    payload: PublicationPayload,
    reference: PublicationRef,
}
enum BackingCredit {
    Native {
        _original: Arc<RetainedSerializedBytes>,
        /// Pays for the pinned payloads after the owner's pool drops them.
        _generation: Arc<AccountedCredit>,
    },
    Independent {
        _credit: ByteCredit,
    },
}
/// Closed resource-only holder. No caller-paid count, raw Arc/Weak, or Clone.
/// Exact roots and their Vec retire before original backing and metadata credit.
pub struct PublicationCustody {
    entries: Vec<PinnedPublication>,
    _backing: BackingCredit,
    pool: HostBytes,
    _metadata: ByteCredit,
}
impl PublicationCustody {
    /// Pin actual existing roots with independent same-pool allocation charges.
    /// This may conservatively double-charge an unqualified live source owner;
    /// it does not qualify that owner's other allocations or grant authority.
    pub fn from_pool<'a>(
        pool: &PublicationPool,
        references: impl ExactSizeIterator<Item = &'a PublicationRef>,
        host: &HostBytes,
    ) -> Result<Self, PublicationCustodyFailure> {
        let count = count(references.len())?;
        let scratch = allocation(count, size_of::<PublicationRef>() + size_of::<usize>())?;
        let retained = allocation(count, size_of::<PinnedPublication>())?;
        let mut metadata = host
            .try_acquire_bulk(
                scratch
                    .checked_add(retained)
                    .ok_or(PublicationCustodyFailure::Capacity)?,
            )
            .ok_or(PublicationCustodyFailure::Capacity)?;
        let scratch_credit = metadata
            .split(scratch)
            .ok_or(PublicationCustodyFailure::Capacity)?;
        let mut refs = Vec::with_capacity(count);
        if refs.capacity() != count {
            return Err(PublicationCustodyFailure::Capacity);
        }
        for reference in references {
            if refs.len() == count {
                return Err(PublicationCustodyFailure::InvalidPayload);
            }
            refs.push(reference.clone());
        }
        if refs.len() != count {
            return Err(PublicationCustodyFailure::InvalidPayload);
        }
        let mut indices = Vec::with_capacity(count);
        if indices.capacity() != count {
            return Err(PublicationCustodyFailure::Capacity);
        }
        indices.extend(0..count);
        // No allocator-backed set or quadratic duplicate scan. The scratch
        // backing was reserved before either vector existed.
        indices.sort_unstable_by_key(|&i| refs[i].payload);
        let mut previous = None;
        let mut bytes = 0usize;
        for index in &indices {
            let digest = refs[*index].payload;
            if previous == Some(digest) {
                continue;
            }
            bytes = bytes
                .checked_add(pool.retained_allocation_bytes(&digest).map_err(invalid)?)
                .ok_or(PublicationCustodyFailure::Capacity)?;
            previous = Some(digest);
        }
        let backing = host
            .try_acquire_bulk(bytes)
            .ok_or(PublicationCustodyFailure::Capacity)?;
        let entries = Self::entries(pool, refs.iter(), count)?;
        drop(indices);
        drop(refs);
        drop(scratch_credit);
        Ok(Self {
            entries,
            _backing: BackingCredit::Independent { _credit: backing },
            pool: host.clone(),
            _metadata: metadata,
        })
    }
    /// Only the actual native owner calls this after validating its active
    /// resource scope, original pool, and retained-credit custody.
    pub(crate) fn from_native<'a>(
        pool: &PublicationPool,
        references: impl ExactSizeIterator<Item = &'a PublicationRef>,
        host: &HostBytes,
        original: Arc<RetainedSerializedBytes>,
    ) -> Result<Self, PublicationCustodyFailure> {
        let count = count(references.len())?;
        let metadata = host
            .try_acquire_bulk(allocation(count, size_of::<PinnedPublication>())?)
            .ok_or(PublicationCustodyFailure::Capacity)?;
        let entries = Self::entries(pool, references, count)?;
        Ok(Self {
            entries,
            _backing: BackingCredit::Native {
                _generation: original.current_generation(),
                _original: original,
            },
            pool: host.clone(),
            _metadata: metadata,
        })
    }
    /// Caller holds actual capacity credit before invoking this allocation.
    fn entries<'a>(
        pool: &PublicationPool,
        references: impl IntoIterator<Item = &'a PublicationRef>,
        count: usize,
    ) -> Result<Vec<PinnedPublication>, PublicationCustodyFailure> {
        let mut entries = Vec::with_capacity(count);
        if entries.capacity() != count {
            return Err(PublicationCustodyFailure::Capacity);
        }
        for reference in references {
            if entries.len() == count {
                return Err(PublicationCustodyFailure::InvalidPayload);
            }
            let payload = pool.pin(&reference.payload).map_err(invalid)?;
            entries.push(PinnedPublication {
                payload,
                reference: reference.clone(),
            });
        }
        if entries.len() != count {
            return Err(PublicationCustodyFailure::InvalidPayload);
        }
        Ok(entries)
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn reference(&self, index: usize) -> Option<&PublicationRef> {
        self.entries.get(index).map(|entry| &entry.reference)
    }
    pub fn get(&self, index: usize) -> Option<PublicationView<'_>> {
        self.entries.get(index).map(|entry| PublicationView {
            bytes: entry.payload.bytes(),
            kind: entry.reference.kind,
        })
    }
    pub fn same_pool(&self, pool: &HostBytes) -> bool {
        self.pool.same_pool(pool)
    }
}
#[cfg(test)]
#[path = "publication_custody/tests.rs"]
mod tests;
