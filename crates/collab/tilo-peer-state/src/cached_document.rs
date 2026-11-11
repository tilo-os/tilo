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
    certificates: Arc<OnceLock<IdentityCertificates>>,
    /// [`Self::redemptions`], once read.
    redemptions: Arc<OnceLock<Result<Redemptions, String>>>,
    /// [`PolicyData::workspace_policy`] of the workspace the document governs.
    workspace_policy: Arc<OnceLock<tilo_action_policy::WorkspacePolicy>>,
    /// The views of the state this one was changed from, when they were
    /// read: a change to a workspace of hundreds touches a row or two, so the
    /// rows whose bytes are as they were keep what reading them took
    /// (decoding, a certificate's signature) rather than paying it again.
    earlier: Option<Arc<Views>>,
    /// The document as stored ([`Self::stored`]), once asked.
    stored: Arc<OnceLock<Stored>>,
    /// The saved base the state this one was changed from was stored on.
    base: Option<Base>,
}
/// The redemption receipts device rows keep, by invitation.
pub(crate) type Redemptions =
    std::collections::BTreeMap<[u8; 32], Vec<crate::state::RedemptionReceipt>>;
/// A whole saved state of the document and the heads it was saved at.
#[derive(Clone)]
struct Base {
    bytes: Arc<[u8]>,
    heads: Vec<automerge::ChangeHash>,
}
/// The document as stored: a saved base, then the changes since it.
#[derive(Clone)]
struct Stored {
    base: Base,
    since: Arc<[u8]>,
}
/// The derived views of one document state.
struct Views {
    policy: Arc<OnceLock<PolicyData>>,
    identities: Arc<OnceLock<IdentityViews>>,
    certificates: Arc<OnceLock<IdentityCertificates>>,
}
/// The device certificate of each governed identity row whose signature
/// verifies, keyed like [`PolicyData::identities`].
pub(crate) type IdentityCertificates =
    std::collections::BTreeMap<String, crate::verified_certificate::VerifiedCertificate>;
/// Each governed identity row decoded, keyed like
/// [`PolicyData::identities`]; an undecodable row keeps its decode error.
pub(crate) type IdentityViews = std::collections::BTreeMap<
    String,
    Result<crate::state::GovernedIdentity, std::sync::Arc<serde_json::Error>>,
>;
pub(crate) struct PolicyData {
    pub owner: Option<String>,
    pub members: std::collections::BTreeMap<String, tilo_action_policy::Role>,
    /// When each member was admitted, Unix milliseconds.
    pub joined: std::collections::BTreeMap<String, u64>,
    pub grants: Vec<tilo_action_policy::Grant>,
    pub identities: std::collections::BTreeMap<String, Vec<u8>>,
    /// The self-asserted display name beside each device row, keyed like
    /// `identities`.
    pub names: std::collections::BTreeMap<String, String>,
    /// The rows that did not decode, so the grants and roles above are missing
    /// from them and a person shut out by that is told so.
    pub damaged: crate::workspace::policy::Damaged,
}
impl PolicyData {
    /// The workspace's authority as recorded: owner, members and the grants on
    /// `workspace`'s nodes.
    pub(crate) fn workspace_policy(&self, workspace: &str) -> tilo_action_policy::WorkspacePolicy {
        #[cfg(test)]
        WORKSPACE_POLICIES.with(|count| count.set(count.get().saturating_add(1)));
        tilo_action_policy::WorkspacePolicy {
            workspace: workspace.to_owned(),
            owner: self.owner.clone(),
            members: self.members.clone(),
            joined: self.joined.clone(),
            grants: self
                .grants
                .iter()
                .filter(|grant| grant.node.workspace_id() == Some(workspace))
                .cloned()
                .collect(),
        }
        .with_current_roles()
    }
}
impl From<AutoCommit> for CachedDocument {
    fn from(doc: AutoCommit) -> Self {
        Self {
            doc: Arc::new(doc),
            serialized: Arc::default(),
            policy: Arc::default(),
            identities: Arc::default(),
            certificates: Arc::default(),
            workspace_policy: Arc::default(),
            redemptions: Arc::default(),
            earlier: None,
            stored: Arc::default(),
            base: None,
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
        if let Some(views) = self.views() {
            self.earlier = Some(views);
        }
        self.base = self.stored_base();
        self.stored = Arc::default();
        self.policy = Arc::default();
        self.identities = Arc::default();
        self.certificates = Arc::default();
        self.workspace_policy = Arc::default();
        self.redemptions = Arc::default();
        #[cfg(test)]
        if Arc::strong_count(&self.doc) > 1 {
            DETACHES.with(|count| count.set(count.get().saturating_add(1)));
        }
        Arc::make_mut(&mut self.doc)
    }
}
impl CachedDocument {
    /// The authority the document records for `workspace`, the one it
    /// governs: built once per document state, not on every member's
    /// lookup.
    pub(crate) fn workspace_policy(&self, workspace: &str) -> &tilo_action_policy::WorkspacePolicy {
        let policy = self
            .workspace_policy
            .get_or_init(|| self.policy().workspace_policy(workspace));
        debug_assert_eq!(
            policy.workspace, workspace,
            "a document governs one workspace"
        );
        policy
    }
    /// The redemption receipts the device rows keep, by the invitation each
    /// redeemed: whether an invitation was redeemed is then a lookup, not a
    /// reading of every member's row. Refused, as the rows are, when a row
    /// does not decode.
    pub(crate) fn redemptions(&self) -> Result<&Redemptions, &str> {
        self.redemptions
            .get_or_init(|| {
                let mut redemptions = Redemptions::new();
                for identity in self.identities().values() {
                    let identity = identity.as_ref().map_err(ToString::to_string)?;
                    for receipt in identity.admission.iter().chain(&identity.admissions) {
                        redemptions
                            .entry(receipt.invite_id)
                            .or_default()
                            .push(receipt.clone());
                    }
                }
                Ok(redemptions)
            })
            .as_ref()
            .map_err(String::as_str)
    }
    /// This state's views, when any of them was read.
    fn views(&self) -> Option<Arc<Views>> {
        (self.identities.get().is_some() || self.certificates.get().is_some()).then(|| {
            Arc::new(Views {
                policy: Arc::clone(&self.policy),
                identities: Arc::clone(&self.identities),
                certificates: Arc::clone(&self.certificates),
            })
        })
    }
    /// `doc`, a later state of this document (a received change applied to
    /// a copy of it), keeping what reading this state's rows took for the
    /// rows `doc` holds unchanged.
    pub(crate) fn followed_by(&self, doc: AutoCommit) -> Self {
        Self {
            earlier: self.views().or_else(|| self.earlier.clone()),
            base: self.stored_base(),
            ..Self::from(doc)
        }
    }
    /// The earlier state's decoded row for `key`, when its bytes are `bytes`.
    fn earlier_identity(
        &self,
        key: &str,
        bytes: &[u8],
    ) -> Option<&Result<crate::state::GovernedIdentity, std::sync::Arc<serde_json::Error>>> {
        let earlier = self.earlier.as_deref()?;
        (earlier.policy.get()?.identities.get(key)? == bytes)
            .then(|| earlier.identities.get()?.get(key))
            .flatten()
    }
    /// Cache document data only. Membership, expiry, certificate and revocation
    /// decisions must still be evaluated against the caller's current inputs.
    pub(crate) fn policy(&self) -> &PolicyData {
        self.policy
            .get_or_init(|| crate::workspace::policy::legacy(&self.doc))
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
                    if let Some(identity) = self.earlier_identity(key, bytes) {
                        return (key.clone(), identity.clone());
                    }
                    #[cfg(test)]
                    IDENTITY_DECODES.with(|count| count.set(count.get().saturating_add(1)));
                    let identity =
                        crate::state::GovernedIdentity::decode(bytes).map_err(std::sync::Arc::new);
                    (key.clone(), identity)
                })
                .collect()
        })
    }
    /// The identity row `key` as [`Self::identities`] decoded it, when
    /// `bytes` are the row it decoded: a reader walking the document its own
    /// way takes the row from here instead of decoding it again, and decodes
    /// it itself whenever this answers `None`.
    pub(crate) fn decoded_identity(
        &self,
        key: &str,
        bytes: &[u8],
    ) -> Option<&crate::state::GovernedIdentity> {
        if self.policy().identities.get(key).map(Vec::as_slice) != Some(bytes) {
            return None;
        }
        self.identities().get(key)?.as_ref().ok()
    }
    /// The signature-verified device certificates of [`Self::identities`],
    /// each verified once per document state rather than whenever the
    /// workspace's member devices are listed. Validity at an instant,
    /// revocation and membership are still the caller's to check.
    pub(crate) fn identity_certificates(&self) -> &IdentityCertificates {
        self.certificates.get_or_init(|| {
            self.identities()
                .iter()
                .filter_map(|(key, identity)| {
                    let certificate = identity.as_ref().ok()?.device_cert.clone();
                    if let Some(verified) = self
                        .earlier
                        .as_deref()
                        .and_then(|earlier| earlier.certificates.get()?.get(key))
                        .filter(|verified| verified.as_ref() == &certificate)
                    {
                        return Some((key.clone(), verified.clone()));
                    }
                    let verified =
                        crate::verified_certificate::VerifiedCertificate::historical_typed(
                            certificate,
                        )
                        .ok()?;
                    Some((key.clone(), verified))
                })
                .collect()
        })
    }
    /// The base this state is stored on, or the one it inherited.
    fn stored_base(&self) -> Option<Base> {
        self.stored
            .get()
            .map(|stored| stored.base.clone())
            .or_else(|| self.base.clone())
    }
    /// The document as a store keeps it: a saved base, then the changes
    /// made since it, which together are the document's storage
    /// ([`automerge::AutoCommit::load`] reads them as one).
    ///
    /// Saving the whole document with every change made each change to a
    /// workspace of hundreds cost a save of every member's row. The base is
    /// the one the state this one was changed from was stored on, while the
    /// changes since it weigh no more than half of it: a base is saved anew
    /// at most once per that much in changes, so each change's share of
    /// those saves is about its own size. The base's bytes are the same
    /// allocation as long as it lasts, so a store keeping it apart writes it
    /// once.
    pub(crate) fn stored(&self) -> (Arc<[u8]>, Arc<[u8]>) {
        use automerge::ReadDoc as _;
        let stored = self.stored.get_or_init(|| {
            let mut doc = AutoCommit::clone(&self.doc);
            if let Some(base) = &self.base
                && base
                    .heads
                    .iter()
                    .all(|head| doc.get_change_by_hash(head).is_some())
            {
                let since = doc.save_after(&base.heads);
                if since.len().saturating_mul(2) <= base.bytes.len() {
                    return Stored {
                        base: base.clone(),
                        since: since.into(),
                    };
                }
            }
            let heads = doc.get_heads();
            Stored {
                base: Base {
                    bytes: self.saved_bytes().into(),
                    heads,
                },
                since: Arc::from([]),
            }
        });
        (Arc::clone(&stored.base.bytes), Arc::clone(&stored.since))
    }
    pub(crate) fn saved(&self) -> Vec<u8> {
        self.saved_bytes().to_vec()
    }
    /// [`Self::saved`] without copying the bytes.
    pub(crate) fn saved_bytes(&self) -> &[u8] {
        self.serialized.get_or_init(|| {
            #[cfg(test)]
            SERIALIZATIONS.with(|count| count.set(count.get().saturating_add(1)));
            AutoCommit::clone(&self.doc).save()
        })
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
thread_local! {
    /// The workspace policies built from governance data on this thread.
    pub(crate) static WORKSPACE_POLICIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
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
