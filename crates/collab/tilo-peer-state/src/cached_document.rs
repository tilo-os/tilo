//! A copy-on-write document with serialization memoized by the exact
//! document, not by its heads.
//!
//! Cloning shares the document; the first mutable borrow of a shared clone
//! detaches it (`Arc::make_mut`). A staged admission that clones a whole peer
//! therefore copies only the documents it actually changes, and a durable
//! snapshot re-saves only those.
use automerge::AutoCommit;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, OnceLock};

#[derive(Clone)]
pub(crate) struct CachedDocument {
    doc: Arc<AutoCommit>,
    serialized: Arc<OnceLock<Vec<u8>>>,
    policy: Arc<OnceLock<PolicyData>>,
    identities: Arc<OnceLock<IdentityViews>>,
}
/// Each governed identity row decoded, keyed like
/// [`PolicyData::identities`]; an undecodable row keeps its decode error.
pub(crate) type IdentityViews =
    std::collections::BTreeMap<String, Result<crate::state::GovernedIdentity, String>>;
pub(crate) struct PolicyData {
    pub owner: Option<String>,
    pub caps: Vec<collab_types::caps::CapOverride>,
    pub identities: std::collections::BTreeMap<String, Vec<u8>>,
}
impl From<AutoCommit> for CachedDocument {
    fn from(doc: AutoCommit) -> Self {
        Self {
            doc: Arc::new(doc),
            serialized: Arc::default(),
            policy: Arc::default(),
            identities: Arc::default(),
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
        self.identities = Arc::default();
        #[cfg(test)]
        if Arc::strong_count(&self.doc) > 1 {
            DETACHES.with(|count| count.set(count.get().saturating_add(1)));
        }
        Arc::make_mut(&mut self.doc)
    }
}
impl CachedDocument {
    /// Cache document data only. Membership, expiry, certificate and revocation
    /// decisions must still be evaluated against the caller's current inputs.
    pub(crate) fn policy(&self) -> &PolicyData {
        self.policy
            .get_or_init(|| crate::room::policy::legacy(&self.doc))
    }
    /// The governed identity rows of [`Self::policy`], each decoded once per
    /// document state rather than on every membership lookup. Data only, like
    /// `policy`: certificate, revocation and role checks still run per call.
    pub(crate) fn identities(&self) -> &IdentityViews {
        self.identities.get_or_init(|| {
            self.policy()
                .identities
                .iter()
                .map(|(key, bytes)| {
                    #[cfg(test)]
                    IDENTITY_DECODES.with(|count| count.set(count.get().saturating_add(1)));
                    let identity = serde_json::from_slice(bytes).map_err(|e| e.to_string());
                    (key.clone(), identity)
                })
                .collect()
        })
    }
    pub(crate) fn saved(&self) -> Vec<u8> {
        self.serialized
            .get_or_init(|| {
                #[cfg(test)]
                SERIALIZATIONS.with(|count| count.set(count.get().saturating_add(1)));
                AutoCommit::clone(&self.doc).save()
            })
            .clone()
    }
}
#[cfg(test)]
thread_local! {
    static SERIALIZATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static DETACHES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
/// This thread's document copies (shared clones detached by a mutation) and
/// full saves since the last call, which resets both.
#[cfg(test)]
pub(crate) fn take_copies_and_saves() -> (usize, usize) {
    (
        DETACHES.with(|count| count.replace(0)),
        SERIALIZATIONS.with(|count| count.replace(0)),
    )
}
#[cfg(test)]
thread_local! { pub(crate) static IDENTITY_DECODES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
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
    fn clones_share_the_document_until_one_of_them_mutates() {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "value", 1).unwrap();
        doc.commit();
        let mut cached = CachedDocument::from(doc);
        take_copies_and_saves();
        cached.put(ROOT, "value", 2).unwrap();
        assert_eq!(
            take_copies_and_saves(),
            (0, 0),
            "an unshared document is never copied"
        );
        let old = cached.clone();
        let readers: Vec<_> = (0..10).map(|_| cached.clone()).collect();
        assert!(
            readers
                .iter()
                .all(|reader| Arc::ptr_eq(&reader.doc, &cached.doc))
        );
        cached.put(ROOT, "value", 3).unwrap();
        cached.put(ROOT, "value", 4).unwrap();
        assert_eq!(
            take_copies_and_saves(),
            (1, 0),
            "only the first shared write copies"
        );
        let value = |doc: &CachedDocument| doc.get(ROOT, "value").unwrap().unwrap().0.as_i64();
        assert_eq!(value(&old), Some(2));
        assert_eq!(value(&readers[0]), Some(2));
        assert_eq!(value(&cached), Some(4));
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
        assert_eq!(after, (*cached.doc).clone().save());
        let mut restored = AutoCommit::load(&after).unwrap();
        restored.load_incoming(&remote.save_after(&heads)).unwrap();
        assert_eq!(restored.get_heads(), remote.get_heads());
    }
}
