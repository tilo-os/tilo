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
    /// [`PolicyData::workspace_policy`] of the workspace the document belongs to.
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
    /// The lengths and digests of the document's stored parts ([`Self::stored_layout`]),
    /// once asked: what a durable hash folds in, kept without the parts themselves.
    layout: Arc<OnceLock<StoredLayout>>,
    /// [`Self::security_basis`], with the revocations it was worked out
    /// under.
    security_basis: Arc<OnceLock<SecurityBasis>>,
    /// [`Self::changes_named`], once asked.
    named: Arc<OnceLock<Named>>,
    /// [`Self::newest_change_secs`], once asked.
    newest_change: Arc<OnceLock<Option<i64>>>,
}
/// A governance state's changes by the bytes a write names each by
/// ([`collab_types::governance_head`]), and its changes that build on none.
#[derive(Default)]
pub(crate) struct Named {
    pub(crate) by_head: std::collections::HashMap<[u8; 4], Vec<automerge::ChangeHash>>,
    pub(crate) roots: Vec<automerge::ChangeHash>,
}
/// A document state's security basis under the revocations named beside it.
type SecurityBasis = (
    Vec<tilo_user_identity::RevocationEntry>,
    Result<[u8; 32], crate::workspace::basis::BasisError>,
);
/// The redemption receipts device rows keep, by invitation.
pub(crate) type Redemptions =
    std::collections::BTreeMap<[u8; 32], Vec<crate::state::RedemptionReceipt>>;
/// A document as [`CachedDocument::stored`] answers it: its base, the runs
/// of changes since it, and the digest of the runs' bytes.
pub(crate) type StoredParts = (Arc<[u8]>, Vec<Arc<[u8]>>, [u8; 32]);
/// The document as stored: a saved base, then the changes since it, in
/// runs each a state's changes since the one before, then the changes still
/// waiting for what they build on ([`CachedDocument::stored`]).
#[derive(Clone)]
struct Stored {
    /// A whole saved state of the document, of its applied changes alone.
    base: Arc<[u8]>,
    since: Vec<Arc<[u8]>>,
    /// The runs' bytes, one after the other, digested. Boxed: a hasher is
    /// about 2 KB, and every document state carries the one it was changed
    /// from, so a governance state kept beside an access read, and every
    /// future holding one across an await, would carry it inline.
    since_digest: Box<blake3::Hasher>,
    /// The heads the base and the runs hold the changes of.
    heads: Vec<automerge::ChangeHash>,
    /// The changes of this state still waiting for what they build on, as
    /// they wait now. Never part of the base or the runs a later state builds
    /// on: a waiting change a later state drops (one of a history a base
    /// replaced, numbered as a change the document holds is) is gone from
    /// what that state stores, and the stored state loads as the one it is.
    waiting: Arc<[u8]>,
}
impl Stored {
    fn since_len(&self) -> usize {
        self.since.iter().map(|run| run.len()).sum()
    }
    /// The runs as stored: the runs, then the changes waiting.
    fn stored_since(&self) -> Vec<Arc<[u8]>> {
        let mut since = self.since.clone();
        if !self.waiting.is_empty() {
            since.push(Arc::clone(&self.waiting));
        }
        since
    }
    /// The digest of [`Self::stored_since`]'s bytes, one after the other.
    fn stored_since_digest(&self) -> [u8; 32] {
        let mut digest = (*self.since_digest).clone();
        digest.update(&self.waiting);
        *digest.finalize().as_bytes()
    }
}
/// The changes waiting in `doc` for what they build on, one after the other: never one
/// numbered as an applied change, or as one waiting before it, which would make the stored
/// state one automerge refuses to load.
fn waiting_bytes(doc: &automerge::Automerge) -> Arc<[u8]> {
    let mut bytes = Vec::new();
    let mut numbered = std::collections::HashSet::new();
    for change in doc.queued_changes() {
        if doc
            .hash_of_actor_seq(change.actor_id(), change.seq())
            .is_none()
            && numbered.insert((change.actor_id(), change.seq()))
        {
            bytes.extend_from_slice(change.raw_bytes());
        }
    }
    bytes.into()
}
/// `doc` saved without the changes waiting in it: the base a stored state
/// builds on holds only what is applied ([`Stored::waiting`]).
fn saved_applied(doc: &AutoCommit) -> Vec<u8> {
    AutoCommit::clone(doc).save_with_options(automerge::SaveOptions {
        retain_orphans: false,
        ..automerge::SaveOptions::default()
    })
}
/// The lengths and digests of a document's stored parts: its saved base, and the changes made
/// since it. [`CachedDocument::stored_layout`] answers it without keeping either.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StoredLayout {
    pub(crate) base_len: usize,
    pub(crate) base_digest: [u8; 32],
    pub(crate) since_len: usize,
    pub(crate) since_digest: [u8; 32],
}

/// The layout of a document a store already wrote.
fn layout_of(stored: &Stored) -> StoredLayout {
    StoredLayout {
        base_len: stored.base.len(),
        base_digest: *blake3::hash(&stored.base).as_bytes(),
        since_len: stored.since_len().saturating_add(stored.waiting.len()),
        since_digest: stored.stored_since_digest(),
    }
}

/// The derived views of one document state.
struct Views {
    policy: Arc<OnceLock<Arc<PolicyData>>>,
    identities: Arc<OnceLock<IdentityViews>>,
    certificates: Arc<OnceLock<IdentityCertificates>>,
}
/// The device certificate of each identity row of the governance whose
/// signature verifies, keyed like [`PolicyData::identities`].
pub(crate) type IdentityCertificates =
    std::collections::BTreeMap<String, crate::verified_certificate::VerifiedCertificate>;
/// Each identity row of the governance decoded, keyed like
/// [`PolicyData::identities`]; an undecodable row keeps its decode error.
pub(crate) type IdentityViews = std::collections::BTreeMap<
    String,
    Result<crate::state::DeviceRow, std::sync::Arc<crate::state::RowError>>,
>;
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PolicyData {
    pub owner: Option<String>,
    pub identities: std::collections::BTreeMap<String, Vec<u8>>,
    /// The self-asserted display name beside each device row, keyed like
    /// `identities`, and beside a person's user root where the workspace keeps
    /// the name they go by without a device of theirs listed.
    pub names: std::collections::BTreeMap<String, String>,
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
            layout: Arc::default(),
            security_basis: Arc::default(),
            named: Arc::default(),
            newest_change: Arc::default(),
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
        self.layout = Arc::default();
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
        self.named = Arc::default();
        self.newest_change = Arc::default();
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
    /// The authority the document records for `workspace`: built once per
    /// document state, not on every member's lookup.
    pub(crate) fn workspace_policy(&self, workspace: &str) -> &tilo_action_policy::WorkspacePolicy {
        let policy = self.workspace_policy.get_or_init(|| {
            #[cfg(test)]
            WORKSPACE_POLICIES.with(|count| count.set(count.get().saturating_add(1)));
            let mut policy = crate::members::replay(&self.doc, workspace);
            // What the workspace's entry opens to visitors is decided by the entry itself, so
            // every device reading this state admits a visitor's call alike.
            policy.grants.extend(
                tilo_desktop_model::projection::workspace_entry_of(&self.doc)
                    .visitor_grants(workspace),
            );
            policy
        });
        debug_assert_eq!(
            policy.workspace, workspace,
            "a document names one workspace"
        );
        policy
    }
    /// The workspace's entry this state records: whom it lets in.
    pub(crate) fn workspace_entry(&self) -> tilo_action_policy::EntryPolicy {
        tilo_desktop_model::projection::workspace_entry_of(&self.doc)
    }
    /// The changes this state holds by the bytes a write names each by, and
    /// those that build on no other: read once per state.
    pub(crate) fn changes_named(&self) -> &Named {
        self.named.get_or_init(|| {
            let mut named = Named::default();
            for change in self.doc.committed_document().get_changes_meta(&[]) {
                if change.deps.is_empty() {
                    named.roots.push(change.hash);
                }
                named
                    .by_head
                    .entry(collab_types::governance_head(&change.hash.0))
                    .or_default()
                    .push(change.hash);
            }
            named
        })
    }
    /// The time the newest change of this state was made, seconds since the epoch, when one names
    /// a time: the state's own "now", which every device holding it reads alike. Read once per
    /// state: it walks every change.
    pub(crate) fn newest_change_secs(&self) -> Option<i64> {
        *self.newest_change.get_or_init(|| {
            self.doc
                .committed_document()
                .get_changes_meta(&[])
                .iter()
                .map(|change| change.timestamp)
                .max()
                .filter(|secs| *secs > 0)
        })
    }
    /// This state read under `policy` instead of the policy its operations
    /// replay to: what judges a write by the standing a cut kept
    /// ([`crate::standing::Standing::held_in`]). Every other view of the state
    /// is the state's own.
    pub(crate) fn under_policy(mut self, policy: tilo_action_policy::WorkspacePolicy) -> Self {
        self.workspace_policy = Arc::new(OnceLock::from(policy));
        self
    }
    /// Whether the replay of the document moved `workspace`'s owner from the
    /// one its `owner` row names: the transfer it carries
    /// ([`crate::members::replay::owner_moved`]).
    pub(crate) fn owner_moved(&self, workspace: &str) -> bool {
        crate::members::replay::owner_moved(&self.doc, workspace)
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
        // Only from a state whose every change `doc` holds: one whose change
        // `doc` pruned may have held receipts `doc` no longer does.
        let since_checked = self
            .receipts
            .get()
            .filter(|heads| heads.iter().all(|head| doc.has_change(head)))
            .map(|heads| Arc::new(doc.get_changes(heads)));
        Self {
            earlier: self.views().or_else(|| self.earlier.clone()),
            before: self.stored_before(),
            since_checked,
            ..Self::from(doc)
        }
    }
    /// `doc`, a change this device wrote on `held`: [`Self::followed_by`] where the state it
    /// was written on is still held, and `doc` as its own where there is none. A write that
    /// keeps the workspace it was made on goes through here, so what that state already checked
    /// (its invitation receipts among them) is not read again
    /// ([`Self::written_since_receipts_checked`]).
    pub(crate) fn written_on(held: Option<&Self>, doc: AutoCommit) -> Self {
        match held {
            Some(held) => held.followed_by(doc),
            None => doc.into(),
        }
    }
    /// `doc`, this state started over from a base that holds the values it held: the history
    /// differs, so [`Self::followed_by`] cannot tell what `doc` added, but every invitation
    /// receipt it holds is one this state held and checked, and stays checked.
    pub(crate) fn rebased_into(&self, doc: AutoCommit) -> Self {
        let checked = self.receipts.get().is_some();
        let state = Self::from(doc);
        if checked {
            state.receipts_checked();
        }
        state
    }
    /// The earlier state's decoded row for `key`, when its bytes are `bytes`.
    fn earlier_identity(
        &self,
        key: &str,
        bytes: &[u8],
    ) -> Option<&Result<crate::state::DeviceRow, std::sync::Arc<crate::state::RowError>>> {
        let earlier = self.earlier.as_deref()?;
        (earlier.policy.get()?.identities.get(key)? == bytes)
            .then(|| earlier.identities.get()?.get(key))
            .flatten()
    }
    /// The workspace's security basis under `revocations`
    /// ([`crate::workspace::basis::security_basis_of`]), worked out once per
    /// document state: it reads every member's row, and a replica publishes
    /// itself after each write it keeps, most of which leave the governance as
    /// it was. Under revocations other than the ones it was first worked out
    /// under, it is worked out again.
    pub(crate) fn security_basis(
        &self,
        revocations: &[tilo_user_identity::RevocationEntry],
    ) -> Result<[u8; 32], crate::workspace::basis::BasisError> {
        if let Some((under, basis)) = self.security_basis.get()
            && under.as_slice() == revocations
        {
            return *basis;
        }
        crate::work::security_basis_worked_out();
        let basis = crate::workspace::basis::security_basis_of(self, revocations);
        let _ = self.security_basis.set((revocations.to_vec(), basis));
        basis
    }
    /// Cache document data only. Membership, expiry, certificate and revocation
    /// decisions must still be evaluated against the caller's current inputs.
    pub(crate) fn policy(&self) -> &PolicyData {
        self.policy
            .get_or_init(|| crate::workspace::policy::read(&self.doc))
    }
    /// The identity rows of [`Self::policy`], each decoded once per
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
                        crate::state::DeviceRow::decode(bytes).map_err(std::sync::Arc::new);
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
    ) -> Option<&crate::state::DeviceRow> {
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

    /// The lengths and digests of the parts of [`Self::stored`], without the parts: what
    /// [`crate::StatePeer::durable_hash`] folds in. A state a store already wrote answers the
    /// split it was written in; one nothing wrote answers a whole saved base, the one form every
    /// device of the workspace computes the same, and keeps the bytes for nobody. This is what
    /// a session that stores nothing pays for a hash instead of a second copy of every record.
    pub(crate) fn stored_layout(&self) -> StoredLayout {
        use automerge::transaction::Transactable as _;
        self.layout
            .get_or_init(|| {
                if let Some(stored) = self.stored.get() {
                    return layout_of(stored);
                }
                // The split [`Self::stored`] would make for this state: the base of the state it
                // was changed from while the changes since weigh no more than half of it, a
                // base saved anew otherwise. Taken the same way here, so the two never disagree
                // about the bytes a store would write.
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
                let waiting = waiting_bytes(doc);
                if let Some(before) = &self.before
                    && before
                        .heads
                        .iter()
                        .all(|head| doc.get_change_meta_by_hash(head).is_some())
                {
                    let added = doc.save_after(&before.heads);
                    crate::work::read(added.len());
                    let since_len = before.since_len().saturating_add(added.len());
                    if since_len.saturating_mul(2) <= before.base.len() {
                        let mut since_digest = (*before.since_digest).clone();
                        since_digest.update(&added);
                        since_digest.update(&waiting);
                        return StoredLayout {
                            base_len: before.base.len(),
                            base_digest: *blake3::hash(&before.base).as_bytes(),
                            since_len: since_len.saturating_add(waiting.len()),
                            since_digest: *since_digest.finalize().as_bytes(),
                        };
                    }
                }
                let saved = saved_applied(committed);
                crate::work::read(saved.len());
                StoredLayout {
                    base_len: saved.len(),
                    base_digest: *blake3::hash(&saved).as_bytes(),
                    since_len: waiting.len(),
                    since_digest: *blake3::hash(&waiting).as_bytes(),
                }
            })
            .clone()
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
            let waiting = waiting_bytes(doc);
            if let Some(before) = &self.before
                && before
                    .heads
                    .iter()
                    .all(|head| doc.get_change_meta_by_hash(head).is_some())
            {
                let added = doc.save_after(&before.heads);
                crate::work::read(added.len());
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
                        waiting,
                    };
                }
            }
            // A document with nothing waiting saves as its applied changes alone.
            let base = if doc.queued_changes().next().is_none() {
                self.saved_bytes().into()
            } else {
                saved_applied(committed).into()
            };
            Stored {
                base,
                since: Vec::new(),
                since_digest: Box::new(blake3::Hasher::new()),
                heads,
                waiting,
            }
        });
        (
            Arc::clone(&stored.base),
            stored.stored_since(),
            stored.stored_since_digest(),
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
        if heads.is_empty() {
            return Ok(self);
        }
        let applied = self.save_after(&heads);
        let waiting = waiting_bytes(self.committed_document());
        if since.get(..applied.len()) != Some(applied.as_slice())
            || since.get(applied.len()..) != Some(&*waiting)
        {
            return Ok(self);
        }
        // Nothing has kept this state yet: the memo this state was made with
        // holds no base.
        crate::work::read(since.len());
        let mut since_digest = Box::new(blake3::Hasher::new());
        since_digest.update(&applied);
        let _ = self.stored.set(Stored {
            base: base_bytes.into(),
            since: vec![applied.into()],
            since_digest,
            heads: self.committed_heads(),
            waiting,
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
            crate::work::read(saved.len());
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
/// `bytes`, a stored document automerge refuses whole, loaded without the waiting changes
/// that hold a sequence number its applied history gives another change, and without what
/// waits on them: answers the document and the changes left out.
///
/// An earlier layout stored a base with the changes then waiting beside it, and built every
/// later state on that base, also after the document dropped one of them: a change of a
/// history a base replaced, whose actor numbers its changes on the new base again. Once the
/// new history reached that number, the stored state held two changes of one actor's sequence
/// number and no longer loaded. What the document applied is all there; only changes that
/// never applied are left out, which the document left out when it was kept. A stored state
/// whose applied changes themselves disagree is refused, as automerge refused it.
pub(crate) fn load_without_superseded_waiting(
    bytes: &[u8],
) -> Result<(AutoCommit, Vec<automerge::Change>), String> {
    use tilo_automerge::ApplyIncoming as _;
    let base_len = saved_chunk(bytes).ok_or("a stored document holds no saved base")?;
    let (base, rest) = bytes.split_at(base_len);
    let mut doc = AutoCommit::load(base).map_err(|error| error.to_string())?;
    let mut pending =
        automerge::Automerge::parse_changes(rest).map_err(|error| error.to_string())?;
    // The changes whose history is here, applied in the order they were stored.
    loop {
        let mut applied = false;
        let mut waiting = Vec::with_capacity(pending.len());
        for change in pending {
            if doc.committed_document().has_change(&change.hash()) {
                continue;
            }
            if !change
                .deps()
                .iter()
                .all(|dep| doc.committed_document().has_change(dep))
            {
                waiting.push(change);
                continue;
            }
            if doc
                .committed_document()
                .hash_of_actor_seq(change.actor_id(), change.seq())
                .is_some()
            {
                return Err(format!(
                    "two applied changes of actor {} hold sequence number {}",
                    change.actor_id(),
                    change.seq()
                ));
            }
            doc.apply_incoming([change])
                .map_err(|error| error.to_string())?;
            applied = true;
        }
        pending = waiting;
        if !applied {
            break;
        }
    }
    // What still waits: a change numbered as an applied one, or as one waiting before it, is of
    // another history, and so is whatever builds on it.
    let mut superseded = std::collections::HashSet::new();
    let mut numbered = std::collections::HashSet::new();
    let mut left_out = Vec::new();
    let mut kept = Vec::new();
    for change in pending {
        let numbered_applied = doc
            .committed_document()
            .hash_of_actor_seq(change.actor_id(), change.seq())
            .is_some();
        let builds_on_superseded = change.deps().iter().any(|dep| superseded.contains(dep));
        if numbered_applied
            || builds_on_superseded
            || !numbered.insert((change.actor_id().clone(), change.seq()))
        {
            superseded.insert(change.hash());
            left_out.push(change);
        } else {
            kept.push(change);
        }
    }
    // A change waiting on one left out after it in the stored order goes too.
    loop {
        let before = kept.len();
        kept.retain(|change| {
            let goes = change.deps().iter().any(|dep| superseded.contains(dep));
            if goes {
                superseded.insert(change.hash());
                left_out.push(change.clone());
            }
            !goes
        });
        if kept.len() == before {
            break;
        }
    }
    if left_out.is_empty() {
        return Err("a stored document loads with every change it holds".into());
    }
    doc.apply_incoming(kept)
        .map_err(|error| error.to_string())?;
    Ok((doc, left_out))
}
/// Join the last run with the one before it while that one is at most twice
/// as long: run lengths then fall by more than half from each to the next,
/// so there are a few of them.
fn join_runs(runs: &mut Vec<Arc<[u8]>>) {
    while let [.., earlier, last] = runs.as_slice()
        && earlier.len() <= last.len().saturating_mul(2)
    {
        crate::work::read(earlier.len().saturating_add(last.len()));
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
    tilo_test_env::scenario! {
        fn a_cached_document_shares_its_work_until_it_changes;

        phase policy_is_shared_until_mutation_and_does_not_commit_pending_changes;
        {
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

        phase a_stored_document_reopens_on_the_base_it_was_stored_on;
        {
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

        phase unchanged_clones_share_serialization_and_mutations_keep_old_snapshots;
        {
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

        phase clones_share_the_document_until_one_of_them_mutates;
        {
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

        phase unresolved_incrementals_invalidate_even_when_heads_do_not_change;
        {
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

        phase a_waiting_change_the_document_drops_is_gone_from_what_it_stores;
        {
            let (mut seed, superseded) = superseded_history();
            let actor = seed.get_actor().clone();
            let mut cached = CachedDocument::from(seed.fork().with_actor(actor));
            cached.load_incoming(&superseded.raw).unwrap();
            assert_eq!(cached.committed_document().queued_changes().count(), 1);
            let (base, since, _) = cached.stored();
            let stored = [&*base, &since.concat()].concat();
            assert_eq!(
                AutoCommit::load(&stored)
                    .unwrap()
                    .committed_document()
                    .queued_changes()
                    .count(),
                1,
                "a change waiting for what it builds on is stored as waiting"
            );
            // The actor writes on: its own changes take the numbers the waiting change held, and
            // the document drops it.
            for n in 0..2 {
                cached.put(ROOT, "written", n).unwrap();
                cached.commit();
            }
            assert_eq!(cached.committed_document().queued_changes().count(), 0);
            let (base, since, digest) = cached.stored();
            assert!(
                !since.is_empty(),
                "the state is stored on the base it was stored on before, with its changes since"
            );
            let stored = [&*base, &since.concat()].concat();
            let mut reopened = AutoCommit::load(&stored).expect("the stored state loads");
            assert_eq!(reopened.get_heads(), cached.committed_heads());
            assert_eq!(reopened.committed_document().queued_changes().count(), 0);
            let reopened = CachedDocument::from(reopened).stored_as(&stored).unwrap();
            assert_eq!(
                reopened.stored().2,
                digest,
                "the reopened state stores as the bytes it was read from"
            );
        }

        phase a_document_stored_with_a_superseded_waiting_change_opens_without_it;
        {
            let (seed, superseded) = superseded_history();
            // As an earlier layout kept it: a base saved with the change then waiting beside it,
            // and the changes the actor wrote on it since, numbered as the waiting one is.
            let mut doc = seed.clone();
            doc.load_incoming(&superseded.raw).unwrap();
            let base = doc.save();
            let heads = doc.get_heads();
            for n in 0..2 {
                doc.put(ROOT, "written", n).unwrap();
                doc.commit();
            }
            let stored = [base, doc.save_after(&heads)].concat();
            let refused = AutoCommit::load(&stored).expect_err("automerge refuses it whole");
            assert!(refused.to_string().contains("duplicate seq"), "{refused}");
            let (mut opened, left_out) = load_without_superseded_waiting(&stored).unwrap();
            assert_eq!(opened.get_heads(), doc.get_heads());
            assert_eq!(opened.committed_document().queued_changes().count(), 0);
            assert_eq!(
                left_out.iter().map(automerge::Change::hash).collect::<Vec<_>>(),
                vec![superseded.hash],
                "only the change that never applied is left out"
            );
            // Two applied changes of one number are no waiting change to leave out.
            let mut other = seed;
            let seed_heads = other.get_heads();
            other.put(ROOT, "other", 1).unwrap();
            other.commit();
            let forked = [doc.save(), other.save_after(&seed_heads)].concat();
            assert!(AutoCommit::load(&forked).is_err());
            assert!(load_without_superseded_waiting(&forked).is_err());
        }
    }

    /// A change waiting for what it builds on.
    struct Waiting {
        raw: Vec<u8>,
        hash: automerge::ChangeHash,
    }

    /// A document of one actor, large enough that a few changes are stored beside its base, and
    /// a change of the same actor from a history this document never holds: numbered as the
    /// actor's next change but one, built on a change that never comes, as the changes of a
    /// history a base replaced are when the actor writes on the base again.
    fn superseded_history() -> (AutoCommit, Waiting) {
        let actor = automerge::ActorId::from(b"holder".as_slice());
        let mut seed = AutoCommit::new().with_actor(actor.clone());
        for n in 0..2_000 {
            seed.put(ROOT, format!("row-{n}"), n.to_string()).unwrap();
        }
        seed.commit();
        let mut replaced = seed.fork().with_actor(actor);
        replaced.put(ROOT, "replaced", 1).unwrap();
        replaced.commit();
        let missing = replaced.get_heads();
        replaced.put(ROOT, "replaced", 2).unwrap();
        replaced.commit();
        let change = replaced
            .get_changes(&missing)
            .into_iter()
            .next()
            .expect("the change after the missing one");
        let waiting = Waiting {
            raw: change.raw_bytes().to_vec(),
            hash: change.hash(),
        };
        (seed, waiting)
    }
}
