//! Write leases carried in the namespace document.
//!
//! A lease gives one device the right to modify a file in place (a
//! database, a VM image) while other devices read it (`storage.md` §11.2).
//! Leases travel with the namespace, so every peer that sees a file also
//! sees who holds its lease, in causal order with the file's own changes.
//! Each lease is one root-level scalar keyed by record id. A room's lease
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

use automerge::transaction::Transactable;
use automerge::{ROOT, ReadDoc, ScalarValue, Value};
use fs_protocol::FileRecordId;
use serde::{Deserialize, Serialize};

use super::Namespace;

const LEASE_PREFIX: &str = "lease/";
const TENANCY_PREFIX: &str = "tenancy/";
const FENCE_PREFIX: &str = "fence/";
const REQUEST_PREFIX: &str = "leasereq/";
const AUTHORITY_KEY: &str = "lease-authority";

/// One file's write lease.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteLease {
    /// Device key of the holder.
    pub holder: [u8; 32],
    /// Fencing epoch: raised by every change of holder, never by a renewal.
    pub epoch: u64,
    /// The lease lapses at this time (Unix ms) unless renewed. A released
    /// lease keeps its holder and epoch with an expiry in the past.
    pub expires_ms: u64,
    /// Granted by the room's lease authority, rather than taken by a device
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
    /// Device that wrote the version, holding the lease.
    pub holder: [u8; 32],
    /// Lease epoch it was written in.
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
fn all_values<T: for<'de> Deserialize<'de>>(ns: &Namespace, key: &str) -> Vec<T> {
    ns.doc
        .get_all(&ROOT, key)
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

fn put_value<T: Serialize>(ns: &mut Namespace, key: &str, value: &T) -> bool {
    let Ok(bytes) = postcard::to_allocvec(value) else {
        return false;
    };
    ns.doc.put(&ROOT, key, ScalarValue::Bytes(bytes)).is_ok()
}

impl Namespace {
    /// The write lease of `record`, if one was ever taken. Concurrent
    /// values resolve to the highest epoch, then the greatest holder.
    pub fn write_lease(&self, record: FileRecordId) -> Option<WriteLease> {
        let key = format!("{LEASE_PREFIX}{}", record.0);
        all_values::<WriteLease>(self, &key)
            .into_iter()
            .max_by_key(|l| (l.epoch, l.granted, l.holder, l.expires_ms))
    }

    /// Records a lease change for `record`, and the tenancy when the epoch
    /// is new. The caller broadcasts the next incremental save.
    pub fn put_write_lease(&mut self, record: FileRecordId, lease: &WriteLease) -> bool {
        let new_tenancy = self.lease_holder(record, lease.epoch) != Some(lease.holder);
        if new_tenancy {
            let tenancy = format!("{TENANCY_PREFIX}{}/{}", record.0, lease.epoch);
            if !put_value(self, &tenancy, &(lease.granted, lease.holder)) {
                return false;
            }
        }
        let key = format!("{LEASE_PREFIX}{}", record.0);
        put_value(self, &key, lease)
    }

    /// Who held `record`'s lease in `epoch`, resolved like the lease.
    pub fn lease_holder(&self, record: FileRecordId, epoch: u64) -> Option<[u8; 32]> {
        let key = format!("{TENANCY_PREFIX}{}/{epoch}", record.0);
        let recorded = all_values::<(bool, [u8; 32])>(self, &key)
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
        record: FileRecordId,
        content: Option<[u8; 32]>,
        byte_len: u64,
        fence: VersionFence,
    ) -> bool {
        let key = format!("{FENCE_PREFIX}{}", record.0);
        put_value(
            self,
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
        record: FileRecordId,
        content: Option<[u8; 32]>,
        byte_len: u64,
    ) -> Option<VersionFence> {
        let key = format!("{FENCE_PREFIX}{}", record.0);
        all_values::<FenceRecord>(self, &key)
            .into_iter()
            .filter(|f| f.content == content && f.byte_len == byte_len)
            .map(|f| f.fence)
            .max_by_key(|f| (f.epoch, f.holder))
    }

    /// The room's lease authority. Two claims resolve to the greater key.
    pub fn lease_authority(&self) -> Option<[u8; 32]> {
        all_values::<[u8; 32]>(self, AUTHORITY_KEY)
            .into_iter()
            .max()
    }

    /// Claims the lease authority for `device`.
    pub fn put_lease_authority(&mut self, device: [u8; 32]) -> bool {
        put_value(self, AUTHORITY_KEY, &device)
    }

    /// Asks the lease authority for `record`'s lease on behalf of `device`.
    pub fn put_lease_request(
        &mut self,
        record: FileRecordId,
        device: [u8; 32],
        request: LeaseRequest,
    ) -> bool {
        let key = format!("{REQUEST_PREFIX}{}/{}", record.0, hex::encode(device));
        put_value(self, &key, &request)
    }

    /// Withdraws `device`'s request for `record`'s lease.
    pub fn remove_lease_request(&mut self, record: FileRecordId, device: [u8; 32]) {
        let key = format!("{REQUEST_PREFIX}{}/{}", record.0, hex::encode(device));
        let _ = self.doc.delete(&ROOT, key.as_str());
    }

    /// Every pending lease request: record, device, request.
    pub fn lease_requests(&self) -> Vec<(FileRecordId, [u8; 32], LeaseRequest)> {
        self.doc
            .keys(&ROOT)
            .filter_map(|key| {
                let rest = key.strip_prefix(REQUEST_PREFIX)?;
                let (record, device) = rest.split_once('/')?;
                let record = FileRecordId(uuid::Uuid::parse_str(record).ok()?);
                let device: [u8; 32] = hex::decode(device).ok()?.try_into().ok()?;
                let request = all_values::<LeaseRequest>(self, &key)
                    .into_iter()
                    .max_by_key(|r| r.nonce)?;
                Some((record, device, request))
            })
            .collect()
    }

    /// Every lease in the namespace.
    pub fn write_leases(&self) -> Vec<(FileRecordId, WriteLease)> {
        self.doc
            .keys(&ROOT)
            .filter_map(|key| {
                let id = key.strip_prefix(LEASE_PREFIX)?;
                let record = FileRecordId(uuid::Uuid::parse_str(id).ok()?);
                Some((record, self.write_lease(record)?))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: [u8; 32] = [1; 32];
    const OTHER: [u8; 32] = [2; 32];
    const TTL: u64 = 60_000;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn lease_takes_resolve_by_epoch_grant_and_holder_with_exact_fences() {
        // Phase `a_free_or_lapsed_lease_is_taken_with_a_new_epoch`.
        {
            let first = WriteLease::take(None, ME, 1_000, TTL).unwrap();
            assert_eq!(
                (first.holder, first.epoch, first.expires_ms),
                (ME, 1, 61_000)
            );
            let renewed = WriteLease::take(Some(first), ME, 30_000, TTL).unwrap();
            assert_eq!(
                (renewed.epoch, renewed.expires_ms),
                (1, 90_000),
                "renewal keeps the epoch"
            );
            let held = WriteLease::take(Some(renewed), OTHER, 40_000, TTL).unwrap_err();
            assert_eq!(held.holder, ME);
            let lapsed = WriteLease::take(Some(renewed), OTHER, 90_000, TTL).unwrap();
            assert_eq!((lapsed.holder, lapsed.epoch), (OTHER, 2));
            let released = WriteLease {
                expires_ms: 45_000,
                ..renewed
            };
            assert!(!released.granted, "a device's own take is optimistic");
            let handed_over = WriteLease::take(Some(released), OTHER, 45_000, TTL).unwrap();
            assert_eq!(handed_over.epoch, 2, "a released lease is a new tenancy");
        }
        // Phase `concurrent_takes_resolve_by_epoch_then_holder_and_keep_every_tenancy`.
        {
            let mut a = Namespace::new("a").unwrap();
            let record = FileRecordId::new();
            let first = WriteLease::take(None, [5; 32], 0, TTL).unwrap();
            assert!(a.put_write_lease(record, &first));
            let mut b = Namespace::from_bytes("b", &a.save()).unwrap();
            // Released, then taken by both at once.
            let released = WriteLease {
                expires_ms: 1,
                ..first
            };
            a.put_write_lease(record, &released);
            let delta = a.save_incremental();
            b.apply_incremental(&delta).unwrap();
            let on_a = WriteLease::take(a.write_lease(record), [1; 32], 2, TTL).unwrap();
            let on_b = WriteLease::take(b.write_lease(record), [9; 32], 2, TTL).unwrap();
            assert_eq!((on_a.epoch, on_b.epoch), (2, 2));
            a.put_write_lease(record, &on_a);
            b.put_write_lease(record, &on_b);
            let from_a = a.save_incremental();
            let from_b = b.save_incremental();
            a.apply_incremental(&from_b).unwrap();
            b.apply_incremental(&from_a).unwrap();
            for ns in [&a, &b] {
                assert_eq!(ns.write_lease(record).unwrap().holder, [9; 32]);
                assert_eq!(ns.lease_holder(record, 2), Some([9; 32]));
                assert_eq!(ns.lease_holder(record, 1), Some([5; 32]));
            }
            // A renewal racing a newer tenancy loses to it.
            let renewed = WriteLease {
                expires_ms: 99_999,
                ..on_b
            };
            let next = WriteLease {
                holder: [1; 32],
                epoch: 3,
                expires_ms: 50,
                granted: false,
            };
            b.put_write_lease(record, &renewed);
            a.put_write_lease(record, &next);
            let from_a = a.save_incremental();
            b.apply_incremental(&from_a).unwrap();
            assert_eq!(b.write_lease(record), Some(next));
        }
        // Phase `a_granted_lease_wins_over_an_optimistic_one_of_the_same_epoch`.
        {
            let mut a = Namespace::new("a").unwrap();
            let record = FileRecordId::new();
            let mut b = Namespace::from_bytes("b", &a.save()).unwrap();
            let optimistic = WriteLease::take(None, [9; 32], 0, TTL).unwrap();
            let granted = WriteLease {
                granted: true,
                ..WriteLease::take(None, [1; 32], 0, TTL).unwrap()
            };
            a.put_write_lease(record, &granted);
            b.put_write_lease(record, &optimistic);
            let from_a = a.save_incremental();
            let from_b = b.save_incremental();
            a.apply_incremental(&from_b).unwrap();
            b.apply_incremental(&from_a).unwrap();
            for ns in [&a, &b] {
                assert_eq!(ns.write_lease(record), Some(granted));
                assert_eq!(ns.lease_holder(record, 1), Some([1; 32]));
            }
        }
        // Phase `a_version_fence_names_the_tenancy_of_exactly_that_content`.
        {
            let mut ns = Namespace::new("a").unwrap();
            let record = FileRecordId::new();
            let fence = VersionFence {
                holder: [3; 32],
                epoch: 4,
            };
            assert!(ns.put_version_fence(record, Some([7; 32]), 10, fence));
            assert_eq!(ns.version_fence(record, Some([7; 32]), 10), Some(fence));
            assert_eq!(ns.version_fence(record, Some([8; 32]), 10), None);
        }
    }

    #[test]
    fn lease_requests_and_holders_replicate_and_converge() {
        // Phase `requests_and_the_authority_replicate`.
        {
            let mut a = Namespace::new("a").unwrap();
            let record = FileRecordId::new();
            assert!(a.lease_authority().is_none());
            assert!(a.put_lease_authority([4; 32]));
            assert!(a.put_lease_request(record, [2; 32], LeaseRequest { nonce: 7 }));
            let b = Namespace::from_bytes("b", &a.save()).unwrap();
            assert_eq!(b.lease_authority(), Some([4; 32]));
            assert_eq!(
                b.lease_requests(),
                vec![(record, [2; 32], LeaseRequest { nonce: 7 })]
            );
            a.remove_lease_request(record, [2; 32]);
            assert!(a.lease_requests().is_empty());
        }
        // Phase `leases_replicate_and_concurrent_takes_converge_on_one_holder`.
        {
            let mut a = Namespace::new("a").unwrap();
            let record = FileRecordId::new();
            assert!(a.write_lease(record).is_none());
            let first = WriteLease {
                holder: [1; 32],
                epoch: 1,
                expires_ms: 100,
                granted: false,
            };
            assert!(a.put_write_lease(record, &first));
            let mut b = Namespace::from_bytes("b", &a.save()).unwrap();
            assert_eq!(b.write_lease(record), Some(first));

            // Both devices take the lapsed lease at once.
            a.put_write_lease(
                record,
                &WriteLease {
                    holder: [1; 32],
                    epoch: 2,
                    expires_ms: 500,
                    granted: false,
                },
            );
            b.put_write_lease(
                record,
                &WriteLease {
                    holder: [2; 32],
                    epoch: 2,
                    expires_ms: 500,
                    granted: false,
                },
            );
            let from_a = a.save_incremental();
            let from_b = b.save_incremental();
            a.apply_incremental(&from_b).unwrap();
            b.apply_incremental(&from_a).unwrap();
            assert_eq!(a.write_lease(record), b.write_lease(record));
            assert_eq!(a.write_leases().len(), 1);
        }
    }
}
