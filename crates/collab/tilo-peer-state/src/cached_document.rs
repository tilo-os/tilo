//! Serialization memoization owned by the exact document, not by its heads.
use automerge::AutoCommit;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, OnceLock};

#[derive(Clone)]
pub(crate) struct CachedDocument {
    doc: AutoCommit,
    serialized: Arc<OnceLock<Vec<u8>>>,
    policy: Arc<OnceLock<PolicyData>>,
}
pub(crate) struct PolicyData {
    pub owner: Option<String>,
    pub caps: Vec<collab_types::caps::CapOverride>,
    pub identities: std::collections::BTreeMap<String, Vec<u8>>,
}
impl From<AutoCommit> for CachedDocument {
    fn from(doc: AutoCommit) -> Self {
        Self {
            doc,
            serialized: Arc::default(),
            policy: Arc::default(),
        }
    }
}
impl Deref for CachedDocument {
    type Target = AutoCommit;
    fn deref(&self) -> &AutoCommit {
        &self.doc
    }
}
impl DerefMut for CachedDocument {
    fn deref_mut(&mut self) -> &mut AutoCommit {
        // Mutable access can add pending or unresolved operations without
        // changing heads. Detach before lending; older clones retain their bytes.
        self.serialized = Arc::default();
        self.policy = Arc::default();
        &mut self.doc
    }
}
impl CachedDocument {
    /// Cache document data only. Membership, expiry, certificate and revocation
    /// decisions must still be evaluated against the caller's current inputs.
    pub(crate) fn policy(&self) -> &PolicyData {
        self.policy
            .get_or_init(|| crate::room::policy::legacy(&self.doc))
    }
    pub(crate) fn saved(&self) -> Vec<u8> {
        self.serialized
            .get_or_init(|| {
                #[cfg(test)]
                SERIALIZATIONS.with(|count| count.set(count.get().saturating_add(1)));
                self.doc.clone().save()
            })
            .clone()
    }
}
#[cfg(test)]
thread_local! { static SERIALIZATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
#[cfg(test)]
mod tests {
    use super::*;
    use automerge::{ROOT, ReadDoc, transaction::Transactable};
    use tilo_automerge::ApplyIncoming;
    #[test]
    fn policy_is_shared_until_mutation_and_does_not_commit_pending_changes() {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "owner", "first").unwrap();
        let identities = doc
            .put_object(ROOT, "peer_identities_v1", automerge::ObjType::Map)
            .unwrap();
        doc.put(
            &identities,
            "device",
            automerge::ScalarValue::Bytes(vec![1]),
        )
        .unwrap();
        doc.commit();
        let mut cached = CachedDocument::from(doc);
        let old = cached.clone();
        assert!(std::ptr::eq(cached.policy(), old.policy()));
        assert_eq!(cached.policy().owner.as_deref(), Some("first"));
        cached.put(ROOT, "owner", "second").unwrap();
        cached
            .put(
                &identities,
                "device",
                automerge::ScalarValue::Bytes(vec![2]),
            )
            .unwrap();
        let pending = cached.pending_ops();
        assert!(pending > 0);
        assert_eq!(cached.policy().owner.as_deref(), Some("second"));
        assert_eq!(cached.policy().identities.get("device"), Some(&vec![2]));
        assert_eq!(old.policy().owner.as_deref(), Some("first"));
        assert_eq!(old.policy().identities.get("device"), Some(&vec![1]));
        assert_eq!(cached.pending_ops(), pending);
        cached.delete(ROOT, "owner").unwrap();
        assert_eq!(cached.policy().owner, None);
    }
    #[test]
    fn unchanged_clones_share_serialization_and_mutations_keep_old_snapshots() {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "value", 1).unwrap();
        doc.commit();
        let mut cached = CachedDocument::from(doc);
        SERIALIZATIONS.with(|count| count.set(0));
        let initial = cached.saved();
        for _ in 0..100 {
            assert_eq!(cached.clone().saved(), initial);
        }
        assert_eq!(SERIALIZATIONS.with(std::cell::Cell::get), 1);
        let old = cached.clone();
        cached.put(ROOT, "value", 2).unwrap();
        let pending = cached.pending_ops();
        assert!(pending > 0);
        let changed = cached.saved();
        assert_ne!(changed, initial);
        assert_eq!(
            cached.pending_ops(),
            pending,
            "saving must not commit the owner"
        );
        assert_eq!(old.saved(), initial);
        assert_eq!(cached.saved(), changed);
        assert_eq!(SERIALIZATIONS.with(std::cell::Cell::get), 2);
        assert_eq!(
            AutoCommit::load(&changed)
                .unwrap()
                .get(ROOT, "value")
                .unwrap()
                .unwrap()
                .0
                .as_i64(),
            Some(2)
        );
    }
    #[test]
    fn unresolved_incrementals_invalidate_even_when_heads_do_not_change() {
        let mut seed = AutoCommit::new();
        seed.put(ROOT, "value", 1).unwrap();
        seed.commit();
        let mut remote = seed.fork();
        remote.put(ROOT, "value", 2).unwrap();
        remote.commit();
        let dependency = remote.get_heads();
        remote.put(ROOT, "value", 3).unwrap();
        remote.commit();
        let orphan = remote.save_after(&dependency);
        let mut cached = CachedDocument::from(seed);
        let before = cached.saved();
        let heads = cached.base_heads();
        cached.load_incoming(&orphan).unwrap();
        assert_eq!(cached.base_heads(), heads);
        assert!(!cached.get_missing_deps(&[]).is_empty());
        let after = cached.saved();
        assert_ne!(
            after, before,
            "queued changes must survive the durable snapshot"
        );
        assert_eq!(after, cached.doc.clone().save());
        let mut restored = AutoCommit::load(&after).unwrap();
        restored.load_incoming(&remote.save_after(&heads)).unwrap();
        assert_eq!(restored.get_heads(), remote.get_heads());
    }
}
