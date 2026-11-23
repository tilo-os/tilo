//! Write leases carried in the namespace document.
//!
//! A lease gives one device the right to modify a file in place (a
//! database, a VM image) while other devices read it.
//! Leases travel with the namespace, so every peer that sees a file also
//! sees who holds its lease, in causal order with the file's own changes.
//! Each lease is one root-level scalar keyed by record id. A workspace's lease
//! authority (its always-on host) grants free leases one at a time; a
//! device that cannot reach it takes a lease optimistically. Concurrent
//! writes of the scalar are resolved by content, the same way on every
//! device: the higher epoch wins, then a granted lease over an optimistic
//! one, then the greater holder key. Every tenancy (an epoch and its
//! holder) is also kept, so a version written under a lease can be checked
//! against the holder of its epoch long after the lease moved on.
//!
//! A version written under a lease carries a [`VersionFence`]: the content
//! it installed and the tenancy it was written in, published in the same
//! namespace change as the content itself.

use automerge::{ScalarValue, Value};
use fs_protocol::{FileDocumentId, TreeChunk};
use serde::{Deserialize, Serialize};

use super::{Namespace, Obj};

const LEASE_PREFIX: &str = "lease/";
const TENANCY_PREFIX: &str = "tenancy/";
const FENCE_PREFIX: &str = "fence/";
const REQUEST_PREFIX: &str = "leasereq/";
const AUTHORITY_KEY: &str = "lease-authority";

/// One file's write lease.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteLease {
    pub holder: [u8; 32],
    /// Fencing epoch: raised by every change of holder, never by a renewal.
    pub epoch: u64,
    /// The lease lapses at this time (Unix ms) unless renewed. A released
    /// lease keeps its holder and epoch with an expiry in the past.
    pub expires_ms: u64,
    /// Granted by the workspace's lease authority, rather than taken by a device
    /// that could not reach it.
    pub granted: bool,
}

impl WriteLease {
    pub fn is_live(&self, now_ms: u64) -> bool {
        self.expires_ms > now_ms
    }

    /// Whether `me` may take the lease `current`, and the lease it then
    /// holds. `Err` carries another holder's live lease.
    ///
    /// # Errors
    ///
    /// Another holder's live lease.
    pub fn take(
        current: Option<Self>,
        me: [u8; 32],
        now_ms: u64,
        ttl_ms: u64,
    ) -> Result<Self, Self> {
        let expires_ms = now_ms.saturating_add(ttl_ms);
        match current {
            None => Ok(Self {
                holder: me,
                epoch: 1,
                expires_ms,
                granted: false,
            }),
            Some(l) if l.holder == me && l.is_live(now_ms) => Ok(Self { expires_ms, ..l }),
            Some(l) if l.is_live(now_ms) => Err(l),
            // Lapsed or released: a new tenancy, fenced by a new epoch.
            Some(l) => Ok(Self {
                holder: me,
                epoch: l.epoch.saturating_add(1),
                expires_ms,
                granted: false,
            }),
        }
    }
}

/// The tenancy a version was written in, and what it installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionFence {
    pub holder: [u8; 32],
    pub epoch: u64,
}

/// A device asking the lease authority for a lease.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseRequest {
    /// Distinguishes this request from the device's earlier ones.
    pub nonce: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct FenceRecord {
    content: Option<[u8; 32]>,
    byte_len: u64,
    fence: VersionFence,
}

/// Every concurrent value of a root-level scalar, decoded.
fn all_values<T: for<'de> Deserialize<'de>>(ns: &Namespace, chunk: TreeChunk, key: &str) -> Vec<T> {
    ns.doc
        .get_all(&Obj::root(chunk), key)
        .map(|values| {
            values
                .into_iter()
                .filter_map(|(value, _)| match value {
                    Value::Scalar(s) => match s.as_ref() {
                        ScalarValue::Bytes(bytes) => postcard::from_bytes(bytes).ok(),
                        _ => None,
                    },
                    Value::Object(_) => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Put `value` at `key` of chunk `chunk`'s root: a record's lease lives in its node's chunk,
/// the workspace's authority in the tree's own record.
fn put_value<T: Serialize>(ns: &mut Namespace, chunk: TreeChunk, key: &str, value: &T) -> bool {
    let Ok(bytes) = postcard::to_allocvec(value) else {
        return false;
    };
    if chunk != 0 && ns.doc.chunk_or_new(chunk, true).is_err() {
        return false;
    }
    ns.doc
        .put(&Obj::root(chunk), key, ScalarValue::Bytes(bytes))
        .is_ok()
}

impl Namespace {
    /// The write lease of `record`, if one was ever taken. Concurrent
    /// values resolve to the highest epoch, then the greatest holder.
    pub fn write_lease(&self, record: FileDocumentId) -> Option<WriteLease> {
        let key = format!("{LEASE_PREFIX}{}", record.0);
        all_values::<WriteLease>(self, record.chunk(), &key)
            .into_iter()
            .max_by_key(|l| (l.epoch, l.granted, l.holder, l.expires_ms))
    }

    /// Records a lease change for `record`, and the tenancy when the epoch
    /// is new. The caller broadcasts the next incremental save.
    pub fn put_write_lease(&mut self, record: FileDocumentId, lease: &WriteLease) -> bool {
        let new_tenancy = self.lease_holder(record, lease.epoch) != Some(lease.holder);
        if new_tenancy {
            let tenancy = format!("{TENANCY_PREFIX}{}/{}", record.0, lease.epoch);
            if !put_value(
                self,
                record.chunk(),
                &tenancy,
                &(lease.granted, lease.holder),
            ) {
                return false;
            }
        }
        let key = format!("{LEASE_PREFIX}{}", record.0);
        put_value(self, record.chunk(), &key, lease)
    }

    /// Who held `record`'s lease in `epoch`, resolved like the lease.
    pub fn lease_holder(&self, record: FileDocumentId, epoch: u64) -> Option<[u8; 32]> {
        let key = format!("{TENANCY_PREFIX}{}/{epoch}", record.0);
        let recorded = all_values::<(bool, [u8; 32])>(self, record.chunk(), &key)
            .into_iter()
            .max()
            .map(|(_, holder)| holder);
        // Leases taken before tenancies were kept: the current one.
        recorded.or_else(|| {
            self.write_lease(record)
                .filter(|l| l.epoch == epoch)
                .map(|l| l.holder)
        })
    }

    /// Tags the version of `record` that installs `content`/`byte_len` with
    /// the tenancy it was written in. Put in the same change as the content.
    pub fn put_version_fence(
        &mut self,
        record: FileDocumentId,
        content: Option<[u8; 32]>,
        byte_len: u64,
        fence: VersionFence,
    ) -> bool {
        let key = format!("{FENCE_PREFIX}{}", record.0);
        put_value(
            self,
            record.chunk(),
            &key,
            &FenceRecord {
                content,
                byte_len,
                fence,
            },
        )
    }

    /// The tenancy the version of `record` with this content was written
    /// in, or `None` when it was written without a lease.
    pub fn version_fence(
        &self,
        record: FileDocumentId,
        content: Option<[u8; 32]>,
        byte_len: u64,
    ) -> Option<VersionFence> {
        let key = format!("{FENCE_PREFIX}{}", record.0);
        all_values::<FenceRecord>(self, record.chunk(), &key)
            .into_iter()
            .filter(|f| f.content == content && f.byte_len == byte_len)
            .map(|f| f.fence)
            .max_by_key(|f| (f.epoch, f.holder))
    }

    /// The workspace's lease authority. Two claims resolve to the greater key.
    pub fn lease_authority(&self) -> Option<[u8; 32]> {
        all_values::<[u8; 32]>(self, 0, AUTHORITY_KEY)
            .into_iter()
            .max()
    }

    pub fn put_lease_authority(&mut self, device: [u8; 32]) -> bool {
        put_value(self, 0, AUTHORITY_KEY, &device)
    }

    /// Asks the lease authority for `record`'s lease on behalf of `device`.
    pub fn put_lease_request(
        &mut self,
        record: FileDocumentId,
        device: [u8; 32],
        request: LeaseRequest,
    ) -> bool {
        let key = format!("{REQUEST_PREFIX}{}/{}", record.0, hex::encode(device));
        put_value(self, record.chunk(), &key, &request)
    }

    pub fn remove_lease_request(&mut self, record: FileDocumentId, device: [u8; 32]) {
        let key = format!("{REQUEST_PREFIX}{}/{}", record.0, hex::encode(device));
        let _ = self.doc.delete(&Obj::root(record.chunk()), key.as_str());
    }

    pub fn lease_requests(&self) -> Vec<(FileDocumentId, [u8; 32], LeaseRequest)> {
        self.doc
            .records()
            .flat_map(|chunk| {
                self.doc
                    .keys(&Obj::root(chunk))
                    .into_iter()
                    .map(move |key| (chunk, key))
            })
            .filter_map(|(chunk, key)| {
                let rest = key.strip_prefix(REQUEST_PREFIX)?;
                let (record, device) = rest.split_once('/')?;
                let record = FileDocumentId(uuid::Uuid::parse_str(record).ok()?);
                let device: [u8; 32] = hex::decode(device).ok()?.try_into().ok()?;
                let request = all_values::<LeaseRequest>(self, chunk, &key)
                    .into_iter()
                    .max_by_key(|r| r.nonce)?;
                Some((record, device, request))
            })
            .collect()
    }

    pub fn write_leases(&self) -> Vec<(FileDocumentId, WriteLease)> {
        self.doc
            .records()
            .flat_map(|chunk| self.doc.keys(&Obj::root(chunk)))
            .filter_map(|key| {
                let id = key.strip_prefix(LEASE_PREFIX)?;
                let record = FileDocumentId(uuid::Uuid::parse_str(id).ok()?);
                Some((record, self.write_lease(record)?))
            })
            .collect()
    }
}
