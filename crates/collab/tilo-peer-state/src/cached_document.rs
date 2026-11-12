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
    policy: Arc<OnceLock<Arc<PolicyData>>>,
    identities: Arc<OnceLock<IdentityViews>>,
    certificates: Arc<OnceLock<IdentityCertificates>>,
    /// The heads of a state of this document whose every invitation receipt
    /// was checked ([`Self::receipts_checked`]): this one, or one it was
    /// changed from in place.
    receipts: Arc<OnceLock<Vec<automerge::ChangeHash>>>,
    /// The changes this state holds beyond the one it was changed from, when
    /// every invitation receipt that one held was checked: the receipts
    /// these changes did not write need no checking again
    /// ([`Self::written_since_receipts_checked`]).
    since_checked: Option<Arc<Vec<automerge::Change>>>,
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
    /// How the state this one was changed from was stored.
    before: Option<Stored>,
    /// [`Self::security_basis`], with the revocations it was worked out
    /// under.
    security_basis: Arc<OnceLock<SecurityBasis>>,
}
/// A document state's security basis under the revocations named beside it.
type SecurityBasis = (
    Vec<tilo_user_identity::RevocationEntry>,
    Result<[u8; 32], crate::workspace::media::MediaPolicyError>,
);
/// The redemption receipts device rows keep, by invitation.
pub(crate) type Redemptions =
    std::collections::BTreeMap<[u8; 32], Vec<crate::state::RedemptionReceipt>>;
/// A document as [`CachedDocument::stored`] answers it: its base, the runs
/// of changes since it, and the digest of the runs' bytes.
pub(crate) type StoredParts = (Arc<[u8]>, Vec<Arc<[u8]>>, [u8; 32]);
/// The document as stored: a saved base, then the changes since it, in
/// runs each a state's changes since the one before ([`CachedDocument::stored`]).
#[derive(Clone)]
struct Stored {
    /// A whole saved state of the document.
    base: Arc<[u8]>,
    since: Vec<Arc<[u8]>>,
    /// The runs' bytes, one after the other, digested.
    since_digest: blake3::Hasher,
    /// The heads the base and the runs hold the changes of.
    heads: Vec<automerge::ChangeHash>,
}
impl Stored {
    fn since_len(&self) -> usize {
        self.since.iter().map(|run| run.len()).sum()
    }
}
/// The derived views of one document state.
struct Views {
    policy: Arc<OnceLock<Arc<PolicyData>>>,
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
    Result<crate::state::GovernedIdentity, std::sync::Arc<crate::state::RowError>>,
>;
#[derive(Clone, Debug, PartialEq, Eq)]
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
            receipts: Arc::default(),
            since_checked: None,
            workspace_policy: Arc::default(),
            redemptions: Arc::default(),
            earlier: None,
            stored: Arc::default(),
            before: None,
            security_basis: Arc::default(),
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
        self.before = self.stored_before();
        self.stored = Arc::default();
        self.policy = Arc::default();
        self.identities = Arc::default();
        self.certificates = Arc::default();
        // What the heads `receipts` names held stays checked: the changes
        // made from here are told apart from them when this state is
        // followed.
        self.since_checked = None;
        self.workspace_policy = Arc::default();
        self.redemptions = Arc::default();
        self.security_basis = Arc::default();
        #[cfg(test)]
        if Arc::strong_count(&self.doc) > 1 {
            DETACHES.with(|count| count.set(count.get().saturating_add(1)));
        }
        Arc::make_mut(&mut self.doc)
    }
}
impl CachedDocument {
    /// Whether `a` and `b` are copies of one document state no copy changed
    /// since: a mutable borrow of a shared copy detaches it.
    pub(crate) fn same_state(a: &Self, b: &Self) -> bool {
        Arc::ptr_eq(&a.doc, &b.doc)
    }
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
    /// The changes this state holds beyond the one it was changed from, when
    /// every invitation receipt that one held was checked; `None` otherwise.
    pub(crate) fn written_since_receipts_checked(&self) -> Option<&[automerge::Change]> {
        self.since_checked.as_deref().map(Vec::as_slice)
    }
    /// Note that every invitation receipt this state holds was checked.
    pub(crate) fn receipts_checked(&self) {
        use automerge::transaction::Transactable as _;
        let _ = self.receipts.set(self.doc.base_heads());
    }
    /// `doc`, a later state of this document (a received change applied to
    /// a copy of it), keeping what reading this state's rows took for the
    /// rows `doc` holds unchanged.
    pub(crate) fn followed_by(&self, mut doc: AutoCommit) -> Self {
        use automerge::ReadDoc as _;
        // Only from a state whose every change `doc` holds: one whose change
        // `doc` pruned may have held receipts `doc` no longer does.
        let since_checked = self
            .receipts
            .get()
            .filter(|heads| {
                heads
                    .iter()
                    .all(|head| doc.get_change_by_hash(head).is_some())
            })
            .map(|heads| Arc::new(doc.get_changes(heads)));
        Self {
            earlier: self.views().or_else(|| self.earlier.clone()),
            before: self.stored_before(),
            since_checked,
            ..Self::from(doc)
        }
    }
    /// The earlier state's decoded row for `key`, when its bytes are `bytes`.
    fn earlier_identity(
        &self,
        key: &str,
        bytes: &[u8],
    ) -> Option<&Result<crate::state::GovernedIdentity, std::sync::Arc<crate::state::RowError>>>
    {
        let earlier = self.earlier.as_deref()?;
        (earlier.policy.get()?.identities.get(key)? == bytes)
            .then(|| earlier.identities.get()?.get(key))
            .flatten()
    }
    /// The workspace's security basis under `revocations`
    /// ([`crate::workspace::media::security_basis_of`]), worked out once per
    /// document state: it reads every member's row, and a replica publishes
    /// itself after each write it keeps, most of which leave the governance as
    /// it was. Under revocations other than the ones it was first worked out
    /// under, it is worked out again.
    pub(crate) fn security_basis(
        &self,
        revocations: &[tilo_user_identity::RevocationEntry],
    ) -> Result<[u8; 32], crate::workspace::media::MediaPolicyError> {
        if let Some((under, basis)) = self.security_basis.get()
            && under.as_slice() == revocations
        {
            return *basis;
        }
        crate::governance_work::security_basis_worked_out();
        let basis = crate::workspace::media::security_basis_of(self, revocations);
        let _ = self.security_basis.set((revocations.to_vec(), basis));
        basis
    }
    /// Cache document data only. Membership, expiry, certificate and revocation
    /// decisions must still be evaluated against the caller's current inputs.
    pub(crate) fn policy(&self) -> &PolicyData {
        self.policy
            .get_or_init(|| crate::workspace::policy::read(&self.doc))
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
    /// How this state is stored, or the state it was changed from.
    fn stored_before(&self) -> Option<Stored> {
        self.stored.get().cloned().or_else(|| self.before.clone())
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
    ///
    /// The changes since the base are runs: the earlier state's, then this
    /// state's own changes since it, so a state saves only what it added. A
    /// run at most twice as long as the one after it is joined with it,
    /// which keeps a few runs however many states there were, each byte
    /// joined into a longer run a few times at most.
    ///
    /// Beside the base and the runs, it answers the digest of the runs'
    /// bytes one after the other, which a state takes on from the one it
    /// was changed from by what it added.
    pub(crate) fn stored(&self) -> StoredParts {
        use automerge::transaction::Transactable as _;
        let stored = self.stored.get_or_init(|| {
            // Copying the document to close a transaction cost a copy of its
            // whole history per write; a document with nothing pending is
            // read as it is.
            let copy;
            let committed = if self.doc.pending_ops() == 0 {
                &*self.doc
            } else {
                let mut doc = AutoCommit::clone(&self.doc);
                let _ = doc.document();
                copy = doc;
                &copy
            };
            let doc = committed.committed_document();
            let heads = committed.committed_heads();
            if let Some(before) = &self.before
                && before
                    .heads
                    .iter()
                    .all(|head| doc.get_change_meta_by_hash(head).is_some())
            {
                let added = doc.save_after(&before.heads);
                crate::write_work::read(added.len());
                let since = before.since_len().saturating_add(added.len());
                if since.saturating_mul(2) <= before.base.len() {
                    let mut runs = before.since.clone();
                    let mut since_digest = before.since_digest.clone();
                    if !added.is_empty() {
                        since_digest.update(&added);
                        runs.push(added.into());
                        join_runs(&mut runs);
                    }
                    return Stored {
                        base: before.base.clone(),
                        since: runs,
                        since_digest,
                        heads,
                    };
                }
            }
            Stored {
                base: self.saved_bytes().into(),
                since: Vec::new(),
                since_digest: blake3::Hasher::new(),
                heads,
            }
        });
        (
            Arc::clone(&stored.base),
            stored.since.clone(),
            *stored.since_digest.finalize().as_bytes(),
        )
    }
    /// A document as a store kept it: [`Self::stored`]'s saved base, then
    /// the changes made since it, which [`AutoCommit::load`] reads as one
    /// state. Its base is the saved chunk the bytes lead with, so the state
    /// reopens on the base it was stored on and stores as these very bytes
    /// again. A state reopened as one whole save would be saved anew on its
    /// next change, and the aggregate a peer reports for an unchanged state
    /// would differ after every restart.
    ///
    /// Bytes that do not lead with a saved base, or whose changes since it
    /// are not what storing the state again makes of them, reopen whole.
    pub(crate) fn stored_as(mut self, bytes: &[u8]) -> Result<Self, String> {
        let Some(base_len) = saved_chunk(bytes) else {
            return Ok(self);
        };
        let Some((base_bytes, since)) = bytes.split_at_checked(base_len) else {
            return Ok(self);
        };
        let mut base = AutoCommit::load(base_bytes).map_err(|error| error.to_string())?;
        let heads = base.get_heads();
        if heads.is_empty() || self.save_after(&heads) != since {
            return Ok(self);
        }
        // Nothing has kept this state yet: the memo this state was made with
        // holds no base.
        crate::write_work::read(since.len());
        let mut since_digest = blake3::Hasher::new();
        since_digest.update(since);
        let _ = self.stored.set(Stored {
            base: base_bytes.into(),
            since: vec![since.into()],
            since_digest,
            heads: self.committed_heads(),
        });
        Ok(self)
    }
    pub(crate) fn saved(&self) -> Vec<u8> {
        self.saved_bytes().to_vec()
    }
    /// [`Self::saved`] without copying the bytes.
    pub(crate) fn saved_bytes(&self) -> &[u8] {
        self.serialized.get_or_init(|| {
            #[cfg(test)]
            SERIALIZATIONS.with(|count| count.set(count.get().saturating_add(1)));
            let saved = AutoCommit::clone(&self.doc).save();
            crate::write_work::read(saved.len());
            saved
        })
    }
}
/// The length of the saved chunk `bytes` lead with, when they lead with one
/// that is not the whole of them: automerge's storage leads a chunk with its
/// magic bytes, a checksum, the chunk's type and the length of its data, and
/// `save` writes one document chunk, while the changes after it follow as
/// chunks of their own ([`AutoCommit::save_after`]).
fn saved_chunk(bytes: &[u8]) -> Option<usize> {
    const MAGIC: [u8; 4] = [0x85, 0x6f, 0x4a, 0x83];
    /// A chunk holding a saved document, the kind [`AutoCommit::save`] writes.
    const DOCUMENT: u8 = 0;
    /// The magic bytes, the checksum and the chunk's type, before its length.
    const FIXED: usize = 9;
    if bytes.get(..4)? != MAGIC || *bytes.get(8)? != DOCUMENT {
        return None;
    }
    let (mut data, mut shift, mut at) = (0usize, 0u32, FIXED);
    loop {
        let byte = *bytes.get(at)?;
        at = at.saturating_add(1);
        data = data.checked_add(((byte & 0x7f) as usize).checked_shl(shift)?)?;
        if byte & 0x80 == 0 {
            break;
        }
        shift = shift.checked_add(7)?;
        if shift > usize::BITS {
            return None;
        }
    }
    let total = at.checked_add(data)?;
    (total < bytes.len()).then_some(total)
}
/// Join the last run with the one before it while that one is at most twice
/// as long: run lengths then fall by more than half from each to the next,
/// so there are a few of them.
fn join_runs(runs: &mut Vec<Arc<[u8]>>) {
    while let [.., earlier, last] = runs.as_slice()
        && earlier.len() <= last.len().saturating_mul(2)
    {
        crate::write_work::read(earlier.len().saturating_add(last.len()));
        let joined: Arc<[u8]> = [&**earlier, &**last].concat().into();
        runs.truncate(runs.len().saturating_sub(2));
        runs.push(joined);
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
    fn a_stored_document_reopens_on_the_base_it_was_stored_on() {
        let mut doc = AutoCommit::new();
        for n in 0..2_000 {
            doc.put(ROOT, format!("row-{n}"), n.to_string()).unwrap();
        }
        let mut cached = CachedDocument::from(doc);
        cached.commit();
        let _ = cached.stored();
        // A long change, then a short one: the runs stay apart.
        for value in ["a change since the base ".repeat(4), "short".to_owned()] {
            cached.put(ROOT, value.clone(), value).unwrap();
            cached.commit();
            let _ = cached.stored();
        }
        let (base, since, digest) = cached.stored();
        assert!(
            since.len() > 1,
            "the changes since the base are stored beside it, run by run"
        );
        let bytes = [&*base, &since.concat()].concat();
        let reopened = CachedDocument::from(AutoCommit::load(&bytes).unwrap())
            .stored_as(&bytes)
            .unwrap();
        let (reopened_base, reopened_since, reopened_digest) = reopened.stored();
        assert_eq!(
            [&*reopened_base, &reopened_since.concat()].concat(),
            bytes,
            "the state reopened as the bytes that were stored"
        );
        assert_eq!(
            (reopened_since.concat(), reopened_digest),
            (since.concat(), digest),
            "the reopened state keeps the changes beside the base it was stored on"
        );
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
