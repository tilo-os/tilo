//! Write leases carried in the namespace document.
//!
//! A lease gives one device the right to modify a file in place (a
//! database, a VM image) while other devices read it (`storage.md` §11.2).
//! Leases travel with the namespace, so every peer that sees a file also
//! sees who holds its lease, in causal order with the file's own changes.
//! Each lease is one root-level scalar keyed by record id: concurrent
//! acquisitions resolve to the same winner on every device (Automerge's
//! deterministic register order), and the loser observes it and stops.

use automerge::transaction::Transactable;
use automerge::{ROOT, ReadDoc, ScalarValue, Value};
use collab_types::FileRecordId;
use serde::{Deserialize, Serialize};

use super::Namespace;

const LEASE_PREFIX: &str = "lease/";

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
            }),
            Some(l) if l.holder == me && l.is_live(now_ms) => Ok(Self { expires_ms, ..l }),
            Some(l) if l.is_live(now_ms) => Err(l),
            // Lapsed or released: a new tenancy, fenced by a new epoch.
            Some(l) => Ok(Self {
                holder: me,
                epoch: l.epoch.saturating_add(1),
                expires_ms,
            }),
        }
    }
}

impl Namespace {
    /// The write lease of `record`, if one was ever taken.
    pub fn write_lease(&self, record: FileRecordId) -> Option<WriteLease> {
        let key = format!("{LEASE_PREFIX}{}", record.0);
        match self.doc.get(&ROOT, key.as_str()) {
            Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
                ScalarValue::Bytes(bytes) => postcard::from_bytes(bytes).ok(),
                _ => None,
            },
            _ => None,
        }
    }

    /// Records a lease change for `record`. The caller broadcasts the next
    /// incremental save.
    pub fn put_write_lease(&mut self, record: FileRecordId, lease: &WriteLease) -> bool {
        let Ok(bytes) = postcard::to_allocvec(lease) else {
            return false;
        };
        let key = format!("{LEASE_PREFIX}{}", record.0);
        self.doc
            .put(&ROOT, key.as_str(), ScalarValue::Bytes(bytes))
            .is_ok()
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
    fn a_free_or_lapsed_lease_is_taken_with_a_new_epoch() {
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
        let handed_over = WriteLease::take(Some(released), OTHER, 45_000, TTL).unwrap();
        assert_eq!(handed_over.epoch, 2, "a released lease is a new tenancy");
    }

    #[test]
    fn leases_replicate_and_concurrent_takes_converge_on_one_holder() {
        let mut a = Namespace::new("a").unwrap();
        let record = FileRecordId::new();
        assert!(a.write_lease(record).is_none());
        let first = WriteLease {
            holder: [1; 32],
            epoch: 1,
            expires_ms: 100,
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
            },
        );
        b.put_write_lease(
            record,
            &WriteLease {
                holder: [2; 32],
                epoch: 2,
                expires_ms: 500,
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
