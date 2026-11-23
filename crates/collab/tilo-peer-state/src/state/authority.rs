//! Who a device is in the workspace and what it may do: the certificates a
//! replica keeps for it, the role and grants that decide its changes, and the
//! governance each of those is checked against.

use std::collections::BTreeSet;

use super::written_under::Under;
use super::{
    ActionKind, AutoCommit, DeviceCert, GovernedIdentity, MemberAccess, MembershipError, Role,
    Route, SignedOp, StatePeer, SystemTime, TreePath, WorkspacePolicy, backend, document_surface,
    held_units, now_ns, resource_type, workspace, workspace_host_actor,
};
use collab_types::signer::DeviceSigner;

/// What a refused change made by a key adds: the workspace holds no grant
/// its person approved for the key (or its grant was revoked), and the one
/// command that approves one.
const KEY_NOT_APPROVED: &str = ". This key is not approved for this workspace, or its grant \
     was revoked: the person types `tilo keys create \"<name>\"` and approves the new key in \
     their Tilo tab (on their own computer, `tilo login` again)";

/// How much work deriving access did on this thread, for the tests that hold
/// it to one walk of the workspace's certificates and grants however many
/// devices it covers: a count that cannot pass by waiting longer.
#[cfg(any(test, feature = "test-support"))]
pub(super) mod walks {
    use std::cell::Cell;

    thread_local! {
        static GRANT_WALKS: Cell<u64> = const { Cell::new(0) };
    }

    pub(super) fn count() {
        GRANT_WALKS.with(|walks| walks.set(walks.get().saturating_add(1)));
    }

    /// What `derive` did, and how many walks it took.
    pub(in crate::state) fn taken<T>(derive: impl FnOnce() -> T) -> (T, u64) {
        GRANT_WALKS.with(|walks| walks.set(0));
        let value = derive();
        (value, GRANT_WALKS.with(Cell::get))
    }
}

/// How a write is decided ([`StatePeer::written_as`]): what its author may
/// do, and whether it is history of a device its person removed since.
type Written = (MemberAccess, bool);

/// A caller a change was made for, as the change's history entry names them
/// and the signed invocation it keeps proves it: the person who asked, and
/// their access in the governance this copy judges the write by.
pub(super) struct CallerAccess {
    person: String,
    access: MemberAccess,
}

/// The changes of a cleartext change `envelope`, when its payload carries
/// them: `None` for a sealed one, a checkpoint, or bytes that do not parse.
fn cleartext_changes_of(envelope: &SignedOp) -> Option<Vec<automerge::Change>> {
    if envelope.encryption.is_some() {
        return None;
    }
    let collab_types::DocumentMutation::Incremental(bytes) =
        collab_types::DocumentMutation::decode(&envelope.payload).ok()?
    else {
        return None;
    };
    tilo_failure::ignored!(
        "change bytes that do not parse name no caller",
        collab_types::claim::parse_changes(&bytes).ok()
    )
}

/// The person a change's invoker is decided as: the actor itself when it is a
/// person, else the person it acts for.
fn caller_person(acting: &tilo_action_policy::Acting) -> Option<&str> {
    acting.person_id().or_else(|| acting.acted_for_person())
}

/// `person`'s access under `policy`, held to the foreign-device bound on a
/// `foreign` device and to `delegate`'s bound on a delegate's.
fn access_of(
    policy: impl Into<std::sync::Arc<WorkspacePolicy>>,
    person: String,
    foreign: bool,
    delegate: Option<crate::state::ActingAs>,
) -> MemberAccess {
    let mut access = MemberAccess::person(policy, person);
    if foreign {
        access = access.on_foreign_device();
    }
    if let Some(delegation) = delegate {
        access = access.acting_as(delegation.actor, delegation.grants);
    }
    access
}

/// What [`StatePeer::authorize_document`] decided of a change.
pub(super) enum Authorized {
    /// It is its author's to make, under `member`. `history` when its
    /// author's device was removed since and the cut of the removal keeps
    /// the change ([`crate::removals`]). `by_governor` when its author wrote
    /// it as one of the workspace's governors ([`StatePeer::wrote_as_governor`]),
    /// as the governance it was judged by says.
    Admit {
        member: MemberAccess,
        history: bool,
        by_governor: bool,
    },
    /// Not decided until what it waits for is here.
    Hold(crate::MissingPrerequisite),
}

/// Why [`StatePeer::authorize_document`] refuses a change.
pub(super) enum AuthorizeRefusal {
    /// The change is not its sender's own, or its author holds nothing here: why, as its author
    /// is told it.
    Denied(crate::AdmissionError),
    /// The workspace's rules refuse it: what its author's session is told, typed as a server's
    /// refusal of a change is, and in words that name who made the change and which device
    /// signed it.
    Rules(collab_types::ReceiptRefusal, String),
}

impl std::fmt::Display for AuthorizeRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Denied(refusal) => refusal.fmt(f),
            Self::Rules(_, said) => f.write_str(said),
        }
    }
}

impl From<AuthorizeRefusal> for String {
    fn from(refusal: AuthorizeRefusal) -> Self {
        refusal.to_string()
    }
}

impl StatePeer {
    pub(super) fn has_governed_admission(&self, cert: &DeviceCert) -> bool {
        self.core.workspace_doc.as_ref().is_some_and(|doc| {
            doc.policy().identities.values().any(|bytes| {
                GovernedIdentity::decode(bytes)
                    .is_ok_and(|identity| identity.device_cert == *cert && identity.has_admission())
            })
        })
    }

    /// A current member's device. What it writes into the workspace is admitted
    /// as far as its person's role and grants reach, as every receiver admits
    /// it (`workspace::membership`).
    pub(crate) fn require_member_writer(
        &self,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<(), String> {
        self.require_member_device(&key.device_public_key(), now)
    }

    /// [`Self::require_member_writer`] of a device whose signature was made
    /// elsewhere: a page signs with the key its ownership holds.
    pub(crate) fn require_member_device(
        &self,
        device: &[u8; 32],
        now: SystemTime,
    ) -> Result<(), String> {
        self.certificate(device, now)
            .map_err(|error| error.to_string())?;
        self.role(device, now)
            .map(drop)
            .map_err(|error| error.to_string())
    }

    /// A device that may admit a member's further device: a current member's,
    /// or one of the workspace's key-holding holders, which keeps the
    /// workspace's keys without being a member.
    pub(super) fn require_admitting_writer(
        &self,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<(), String> {
        if self.require_member_writer(key, now).is_ok()
            || self.holder_certificate(&key.device_public_key()).is_some()
        {
            return Ok(());
        }
        self.require_member_writer(key, now)
    }

    /// `device`'s certificate when it is one of the owner's devices, current
    /// at `now`: the owner's own device publishes the records they wrote, from
    /// any device their root certifies, so no one device of theirs is needed
    /// and a revoked one hands nothing over.
    pub(super) fn owner_device(
        &self,
        device: &[u8; 32],
        now: SystemTime,
    ) -> Result<&DeviceCert, String> {
        let cert = self
            .certificate(device, now)
            .map_err(|error| error.to_string())?;
        if cert.user_public_key != self.core.owner {
            return Err("the publisher is not a device of the workspace's owner".into());
        }
        Ok(cert)
    }

    /// Whether `device` is a known, unrevoked device, at no particular time:
    /// for evidence a device signed earlier, of any person taking part.
    #[must_use]
    pub(super) fn known_device(&self, device: &[u8; 32]) -> bool {
        self.core
            .certificates
            .get(device)
            .is_some_and(|cert| !self.core.revocations.refuses(cert))
    }

    /// Whether `person` is a member of this workspace, at any role: the owner,
    /// or anyone the governance this replica holds names as a member.
    #[must_use]
    pub fn is_member(&self, person: &[u8; 32]) -> bool {
        self.core.workspace_doc.as_ref().is_some_and(|doc| {
            doc.workspace_policy(&self.core.workspace)
                .is_member(&hex::encode(person))
        })
    }

    /// Whether `person` governs the workspace's history: its owner, or one of
    /// its administrators as the governance this replica holds names them.
    /// Their devices sign the workspace's checkpoints and bases and relay
    /// what its members write, so a workspace whose owner handed it on keeps
    /// being served by the administrator that holds it, visibly, through the
    /// grant every member reads and its owner may withdraw.
    #[must_use]
    pub fn governs_history(&self, person: &[u8; 32]) -> bool {
        *person == self.core.owner
            || self.core.workspace_doc.as_ref().is_some_and(|doc| {
                doc.workspace_policy(&self.core.workspace)
                    .is_administrator(&hex::encode(person))
            })
    }

    /// The workspace's administrators, by their root keys, as the governance
    /// this replica holds names them ([`Self::governs_history`]).
    #[must_use]
    pub(super) fn administrator_roots(&self) -> Vec<[u8; 32]> {
        let Some(doc) = self.core.workspace_doc.as_ref() else {
            return Vec::new();
        };
        doc.workspace_policy(&self.core.workspace)
            .administrators()
            .filter_map(|person| {
                let mut root = [0; 32];
                hex::decode_to_slice(person, &mut root).ok()?;
                Some(root)
            })
            .collect()
    }

    /// Whether `device` is a known, unrevoked device of a person who governs
    /// the workspace's history ([`Self::governs_history`]), at no particular
    /// time: for evidence a device of theirs signed earlier.
    #[must_use]
    pub fn signed_by_governor(&self, device: &[u8; 32]) -> bool {
        self.known_device(device)
            && self
                .core
                .certificates
                .get(device)
                .is_some_and(|cert| self.governs_history(&cert.user_public_key))
    }

    /// Verified public identity metadata; never includes signing keys.
    pub fn certificates(&self) -> Vec<DeviceCert> {
        self.core
            .certificates
            .values()
            .map(|cert| cert.as_ref().clone())
            .collect()
    }

    /// Whether a replica that takes in this peer's governance at `now` holds
    /// `cert` from that governance alone: an identity row lists the device
    /// with this very certificate, the import takes the row (the owner's
    /// device, an admitted device or a member's), and the certificate is
    /// valid at `now`, as the import checks a certificate it did not hold.
    /// A newcomer is then sent the certificate once, in the governance,
    /// rather than again beside it in its bootstrap's roster or its device's
    /// identity hello.
    #[must_use]
    pub fn governance_certifies(&self, cert: &DeviceCert, now: SystemTime) -> bool {
        let Some(doc) = self.core.workspace_doc.as_ref() else {
            return false;
        };
        let key = hex::encode(cert.device_public_key);
        let Some(Ok(identity)) = doc.identities().get(&key) else {
            return false;
        };
        if identity.device_cert != *cert
            || doc
                .identity_certificates()
                .get(&key)
                .is_none_or(|verified| verified.current_typed(now_ns(now)).is_err())
        {
            return false;
        }
        let person = hex::encode(cert.user_public_key);
        cert.user_public_key == self.core.owner
            || identity.has_admission()
            || doc
                .workspace_policy(&self.core.workspace)
                .members
                .contains_key(&person)
    }

    /// The certificates a replica built from a join answer needs beside this
    /// peer's governance: every certificate this peer holds that the
    /// governance does not carry on its own ([`Self::governance_certifies`]),
    /// and always those of `kept`, the device the answer is for and the host
    /// that answers, and this peer's governing device's, which a replica
    /// holds before any governance arrives. In a workspace of hundreds the
    /// roster repeated every member's certificate the governance after it
    /// carries.
    #[must_use]
    pub fn roster_beside_governance(&self, kept: &[[u8; 32]], now: SystemTime) -> Vec<DeviceCert> {
        self.core
            .certificates
            .values()
            .map(AsRef::as_ref)
            .filter(|cert| {
                kept.contains(&cert.device_public_key)
                    || cert.device_public_key == self.core.origin
                    || !self.governance_certifies(cert, now)
            })
            .cloned()
            .collect()
    }

    /// Whether `other` holds everything authorization reads from this
    /// replica as this one does: its governance, the certificates and
    /// revocations it holds and the receipts its governance is checked
    /// against. Its app records are not part of it. Copies are compared, not
    /// contents, where a change detaches the copy: two that share it are
    /// the same.
    pub(crate) fn same_policy(&self, other: &Self) -> bool {
        let (ours, theirs) = (&self.core, &other.core);
        let governance = match (&ours.workspace_doc, &theirs.workspace_doc) {
            (None, None) => true,
            (Some(ours), Some(theirs)) => {
                crate::cached_document::CachedDocument::same_state(ours, theirs)
            }
            _ => false,
        };
        governance
            && ours.workspace == theirs.workspace
            && ours.owner == theirs.owner
            && ours.origin == theirs.origin
            && ours.receipts == theirs.receipts
            && (crate::shared::Shared::ptr_eq(&ours.certificates, &theirs.certificates)
                || ours
                    .certificates
                    .iter()
                    .map(|(key, cert)| (key, cert.as_ref()))
                    .eq(theirs
                        .certificates
                        .iter()
                        .map(|(key, cert)| (key, cert.as_ref()))))
            && self.revocations() == other.revocations()
    }

    pub fn revocations(&self) -> Vec<tilo_user_identity::RevocationEntry> {
        let mut entries = self.core.revocations.iter().cloned().collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.revoked_device_public_key);
        entries
    }
    pub fn replay_floors(&self) -> Vec<(String, String, [u8; 32], u64)> {
        let mut rows = self
            .core
            .documents
            .iter()
            .flat_map(|(route, document)| {
                document
                    .channel
                    .last_seq_per_author
                    .iter()
                    .map(|(author, seq)| {
                        (
                            route.app.clone(),
                            route.record.clone(),
                            *author.as_bytes(),
                            *seq,
                        )
                    })
            })
            .collect::<Vec<_>>();
        rows.sort();
        rows
    }
    /// The person who owns the workspace: their root key.
    #[must_use]
    pub fn owner(&self) -> [u8; 32] {
        self.core.owner
    }
    /// The certificates of the owner's devices this replica knows.
    #[must_use]
    pub fn owner_certificates(&self) -> Vec<DeviceCert> {
        self.core
            .certificates
            .values()
            .filter(|cert| cert.user_public_key == self.core.owner)
            .map(|cert| cert.as_ref().clone())
            .collect()
    }
    /// The workspace this peer holds, named as its documents and the wire name
    /// it.
    #[must_use]
    pub fn workspace_id(&self) -> &str {
        &self.core.workspace
    }
    /// Whether the certificate the workspace holds for `source` has run out:
    /// the device is certified here, by a certificate past its expiry.
    #[must_use]
    pub fn certificate_expired(&self, source: &[u8; 32], now: SystemTime) -> bool {
        matches!(
            self.core.certificate_typed(source, now),
            Err(backend::CertificateLookupError::Validity(
                tilo_user_identity::DeviceCertError::Expired
            ))
        )
    }
    /// The access of the device `source` now: its person, the workspace's
    /// policy as admitted, the grants for its software, and whether it is held
    /// to the foreign-device bound or to a delegate's.
    ///
    /// # Errors
    /// Returns [`MembershipError`] saying why the device holds no authority
    /// here: an unknown, expired or revoked certificate, a device absent from
    /// governance, or a person who is no longer a member.
    pub fn permissions(
        &self,
        source: [u8; 32],
        now: SystemTime,
    ) -> Result<MemberAccess, MembershipError> {
        // The workspace document read here is the one the policy is decided
        // against, and the peer holds it for its whole life.
        let policy = self
            .core
            .workspace_doc
            .as_ref()
            .ok_or(MembershipError::RulesMissing)?
            .workspace_policy(&self.core.workspace)
            .clone();
        let (person, foreign, delegate) = self.member_of(&source, now)?;
        Ok(access_of(policy, person, foreign, delegate).granted(self.approved_keys(now)))
    }
    /// The access of the device `source` under `policy`, the workspace's
    /// own: [`Self::permissions`]'s access, sharing the policy every
    /// member device's access holds.
    pub(crate) fn member_with(
        &self,
        source: [u8; 32],
        policy: std::sync::Arc<WorkspacePolicy>,
        approved: &[tilo_action_policy::Grant],
        now: SystemTime,
    ) -> Result<MemberAccess, MembershipError> {
        let (person, foreign, delegate) = self.member_of(&source, now)?;
        Ok(access_of(policy, person, foreign, delegate).granted(approved.iter().cloned()))
    }
    /// Who the device `source` is at `now`, apart from the workspace's policy:
    /// its person, whether its device is foreign, and the delegation it holds.
    fn member_of(
        &self,
        source: &[u8; 32],
        now: SystemTime,
    ) -> Result<(String, bool, Option<crate::state::ActingAs>), MembershipError> {
        // A key its person revoked here is nobody's member: every read, write and action it
        // would make is refused, as the workspace's governance calls refuse it.
        if self.governance().revoked_key(source) {
            return Err(MembershipError::Revoked);
        }
        match backend::member::role(&self.core, source, now) {
            Ok((_, delegate)) => {
                let cert = self.certificate(source, now)?;
                let person = hex::encode(cert.user_public_key);
                Ok((
                    person,
                    backend::member::foreign(cert, &self.core.origin),
                    delegate,
                ))
            }
            Err(error) => Err(error),
        }
    }
    /// What `source` may do, for a write of its history: [`Self::permissions`]
    /// as if its person had not removed the device since. Asked only of a
    /// write the cut of the removal keeps ([`Self::written_as`]); it never
    /// says what the device may do now.
    fn permissions_in_history(
        &self,
        source: &[u8; 32],
        now: SystemTime,
    ) -> Result<MemberAccess, MembershipError> {
        let policy = self
            .core
            .workspace_doc
            .as_ref()
            .ok_or(MembershipError::RulesMissing)?
            .workspace_policy(&self.core.workspace)
            .clone();
        let (_, delegate) = backend::member::role_in_history(&self.core, source, now)?;
        let cert = self
            .core
            .certificate_held(source, now)
            .map_err(MembershipError::from)?;
        Ok(access_of(
            policy,
            hex::encode(cert.user_public_key),
            backend::member::foreign(cert, &self.core.origin),
            delegate,
        )
        .granted(self.approved_keys(now)))
    }
    /// How `write`, a write to `document` of `app`, is decided at `now`: what
    /// its author may do, and whether the write is history of a device its
    /// person removed since. `None` while the
    /// removal is known here and its cut is not: nothing is decided about
    /// the write until then.
    ///
    /// Removing a device takes its future, not its person's past. A removed
    /// device's write is history when the cut of its removal keeps it
    /// ([`crate::removals`]): the workspace had taken it when the removal
    /// was recorded, and it is decided as its person's, as it was then.
    /// Every other write of a removed device is refused, whoever hands it
    /// over and whatever time it claims: its place in the document decides,
    /// never a clock.
    ///
    /// A member who takes their work back from a server takes the server's
    /// future for them in the same way. What the cut of their withdrawal
    /// keeps of the server's work is history, decided as that member's work
    /// as it was and answered with the form that keeps it
    /// ([`Self::withdrawal_keeping`]); every other write the server makes for
    /// them is refused.
    ///
    /// # Errors
    /// Why the author may not make the write, as [`Self::permissions`].
    pub(super) fn written_as(
        &self,
        app: &str,
        document: &str,
        write: &SignedOp,
        now: SystemTime,
    ) -> Result<Option<Written>, MembershipError> {
        let author = write.author.as_bytes();
        match self.permissions(*author, now) {
            Ok(member) => Ok(Some((member, false))),
            Err(MembershipError::Revoked) => {
                // The cut is read where it is kept: the governance this copy
                // holds now, whatever the write is judged by.
                let Some(workspace) = self.governance_held_now().map(|doc| &**doc) else {
                    return Err(MembershipError::Revoked);
                };
                // A compaction restates history under a number its author
                // used before, so its number says nothing: the cut names the
                // very compactions the workspace had taken.
                let compaction = if write.kind == collab_types::OpKind::Compaction {
                    Some(
                        self.write_id(app, document, write)
                            .ok_or(MembershipError::Revoked)?,
                    )
                } else {
                    None
                };
                // Cuts that do not read keep nothing: the device stays refused.
                match crate::removals::place(
                    workspace,
                    author,
                    (app, document),
                    write.seq,
                    compaction.as_ref(),
                ) {
                    Ok(crate::removals::Place::Before) => self
                        .permissions_in_history(author, now)
                        .map(|member| Some((member, true))),
                    Ok(crate::removals::Place::Unrecorded) => Ok(None),
                    Ok(crate::removals::Place::After) | Err(_) => Err(MembershipError::Revoked),
                }
            }
            Err(error) => Err(error),
        }
    }
    /// What the author of `write`, a write to `document` of `app`, may do, for
    /// whoever hands the write to an app: what its device may now, or, for a
    /// device its person removed since, what its person may when the cut of
    /// the removal keeps the write ([`Self::written_as`]).
    ///
    /// # Errors
    /// Why the author may not make the write, as [`Self::permissions`]; a
    /// removed device's write whose cut is not here yet is
    /// [`MembershipError::Revoked`] too.
    ///
    /// The write is one this copy took, judged by the governance it was
    /// written under ([`Self::judged_under`]).
    pub fn permissions_as_written(
        &self,
        app: &str,
        document: &str,
        write: &SignedOp,
        now: SystemTime,
    ) -> Result<MemberAccess, MembershipError> {
        let written = match self
            .judged_under(app, document, write, true)
            .map_err(|_| MembershipError::RulesMissing)?
        {
            Under::Now | Under::Unresolved(None) => self.written_as(app, document, write, now)?,
            Under::Past(judged) | Under::Unresolved(Some(judged)) => {
                judged.written_as(app, document, write, now)?
            }
            Under::Missing => return Err(MembershipError::RulesMissing),
        };
        written
            .map(|(member, _)| member)
            .ok_or(MembershipError::Revoked)
    }
    /// Whether the device that wrote a change is one of a governor's
    /// ([`Self::governs_history`]): a known device of theirs now, or, for a
    /// change that is `history`, one they removed since.
    pub(super) fn wrote_as_governor(&self, device: &[u8; 32], history: bool) -> bool {
        self.signed_by_governor(device)
            || (history
                && self
                    .core
                    .certificates
                    .get(device)
                    .is_some_and(|cert| self.governs_history(&cert.user_public_key)))
    }
    /// The id the cut of a removal names `write`, a write to `document` of
    /// `app`, by: `None` when the write does not encode.
    pub(super) fn write_id(&self, app: &str, document: &str, write: &SignedOp) -> Option<[u8; 32]> {
        crate::admission::DocumentAdmission::from_op(&self.core.workspace, app, document, write)
            .ok()
            .map(|admitted| admitted.operation_id())
    }
    /// What this replica has taken of `device`, by route
    /// ([`crate::coverage::route_key`]): the number of its last write and its
    /// compactions in each document's history. What the cut of its removal
    /// holds.
    pub(super) fn taken_from(
        &self,
        device: &[u8; 32],
    ) -> std::collections::BTreeMap<String, crate::removals::Held> {
        let author = collab_types::Ed25519PublicKey::from_bytes(*device);
        self.core
            .documents
            .iter()
            .filter_map(|(route, document)| {
                let (app, document_id) = (&route.app, &route.record);
                let held = crate::removals::Held {
                    last: document
                        .channel
                        .last_seq_per_author
                        .get(&author)
                        .copied()
                        .unwrap_or(0),
                    compactions: document
                        .sealed
                        .iter()
                        .filter(|write| {
                            write.author == author && write.kind == collab_types::OpKind::Compaction
                        })
                        .filter_map(|write| self.write_id(app, document_id, write))
                        .collect(),
                };
                (held != crate::removals::Held::default())
                    .then(|| (crate::coverage::route_key(app, document_id), held))
            })
            .collect()
    }
    /// [`Self::permissions`] as the text refusal the workspace's own write paths
    /// report to the app that asked.
    pub(crate) fn membership(
        &self,
        device: [u8; 32],
        now: SystemTime,
    ) -> Result<MemberAccess, String> {
        self.permissions(device, now)
            .map_err(|error| error.to_string())
    }
    /// Whether the device `source` may run `action` of `resource` in `app`
    /// here now: the workspace's per-action permission for whatever its
    /// actor is decides it, never the kind of member a device belongs to.
    ///
    /// # Errors
    ///
    /// Why the device is no member of this workspace at all, as
    /// [`Self::permissions`].
    pub fn permits_app_action(
        &self,
        source: [u8; 32],
        app: &str,
        resource: &str,
        action: &str,
        now: SystemTime,
    ) -> Result<bool, MembershipError> {
        let member = self.permissions(source, now)?;
        let node = TreePath::workspace(&member.policy.workspace)
            .app(app)
            .rest()
            .resource(resource)
            .rest()
            .action(action);
        Ok(member.allows(&node, ActionKind::Undoable, now))
    }

    /// Whether the device `source` may open `resource` of `app` here now: read
    /// the record the work it would take is about.
    ///
    /// # Errors
    ///
    /// Why the device is no member of this workspace at all, as
    /// [`Self::permissions`].
    pub fn opens_app_record(
        &self,
        source: [u8; 32],
        app: &str,
        resource: &str,
        now: SystemTime,
    ) -> Result<bool, MembershipError> {
        let member = self.permissions(source, now)?;
        let node = TreePath::workspace(&member.policy.workspace)
            .app(app)
            .rest()
            .resource(resource);
        Ok(member.allows(&node, ActionKind::Read, now))
    }
    /// The app surface this peer names in the changes it signs for `app`'s
    /// records, if it names one.
    #[must_use]
    pub fn signed_surface(&self, app: &str) -> Option<[u8; 32]> {
        self.app_surfaces.0.as_ref().and_then(|of| of(app))
    }
    /// Make this peer a workspace host's copy of its workspace: the changes it makes
    /// from now on go under the host's own actor ([`workspace_host_actor`]).
    pub fn write_as_workspace_host(&mut self) {
        self.workspace_host = true;
    }
    /// Tell this copy which device it is the copy of: what it writes against
    /// a base that replaced its history is carried onto the base
    /// ([`Self::receive_from`]).
    pub fn write_as_device(&mut self, device: [u8; 32]) {
        self.own_device = Some(device);
    }

    /// The Automerge actor this peer makes the workspace changes `key` signs
    /// under: the device key for a replica, [`workspace_host_actor`] for a workspace
    /// host's copy.
    pub(crate) fn workspace_actor_of(&self, key: &dyn DeviceSigner) -> automerge::ActorId {
        self.workspace_actor_for(&key.device_public_key())
    }

    /// [`Self::workspace_actor_of`] by the signing device's public key.
    #[must_use]
    pub fn workspace_actor_for(&self, device: &[u8; 32]) -> automerge::ActorId {
        if self.workspace_host {
            workspace_host_actor(device)
        } else {
            automerge::ActorId::from(device.to_vec())
        }
    }

    /// The admitted workspace's security basis
    /// ([`workspace::basis::security_basis`]) under this replica's
    /// revocations, reading the identity rows its governance state decoded
    /// once, and worked out once for that state: every copy of this replica
    /// that holds the same governance answers with it. `None` while no
    /// workspace is admitted.
    pub fn security_basis(&self) -> Option<Result<[u8; 32], workspace::basis::BasisError>> {
        let doc = self.core.workspace_doc.as_ref()?;
        Some(doc.security_basis(&self.revocations()))
    }

    /// Committed, admitted workspace only. Callers cannot mutate the admission owner.
    pub fn admitted_workspace(&self) -> Option<&AutoCommit> {
        self.core.workspace_doc.as_deref()
    }

    /// Read permission for an exact canonical document using the same governed
    /// actor/resource vocabulary as document mutation admission.
    pub fn check_document_read(
        &self,
        device: [u8; 32],
        app: &str,
        document: &str,
        now: SystemTime,
    ) -> Result<(), String> {
        let member = self.membership(device, now)?;
        self.check_read_as(&member, (app, document), now)
    }

    pub(super) fn check_read_as(
        &self,
        member: &MemberAccess,
        (app, document): (&str, &str),
        now: SystemTime,
    ) -> Result<(), String> {
        if backend::access::document_read(
            &member.authority(),
            &self.core.workspace,
            (app, document),
            now,
        ) {
            Ok(())
        } else {
            Err(format!("document read forbidden: {app}/{document}"))
        }
    }

    /// Whether `member` holds the keys of `document` of `app`: it reads the
    /// document, or may act on some part of it, which it cannot do without
    /// the content the keys seal.
    fn holds_document_keys_as(
        &self,
        member: &MemberAccess,
        (app, document): (&str, &str),
        now: SystemTime,
    ) -> bool {
        if self.check_read_as(member, (app, document), now).is_ok() {
            return true;
        }
        let root = collab_types::document_access_target(app, document).unwrap_or(document);
        if tilo_action_policy::roles::lacks_private_record_owner(app, root) {
            return false;
        }
        let kind = self
            .core
            .documents
            .get(&Route {
                app: app.into(),
                record: root.into(),
            })
            .and_then(|held| super::resource_type(&*held.doc));
        backend::access::document_reached(
            &member.authority(),
            &self.core.workspace,
            (app, document),
            kind.as_deref(),
            now,
        )
    }

    /// Certified key holders whose current authority allows reading this
    /// document. Store-only devices never receive content keys. A call's media
    /// keys ([`collab_types::calls::media_document`]) go only to the devices
    /// of the people the call's rule admits now.
    pub fn document_key_holders(
        &self,
        app: &str,
        document: &str,
        now: SystemTime,
    ) -> Result<BTreeSet<[u8; 32]>, String> {
        let devices: Vec<_> = self.governance().key_holders(now)?.into_iter().collect();
        let mut holders: BTreeSet<[u8; 32]> = self
            .governance()
            .access_of(&devices, now)?
            .into_iter()
            .filter_map(|(device, member)| {
                self.holds_document_keys_as(&member, (app, document), now)
                    .then_some(device)
            })
            .collect();
        if app == collab_types::scope::MEDIA_APP
            && let Some((tag, generation)) = collab_types::calls::parse_media_document(document)
        {
            let (rules, persons) = self.governance().call_gate(now)?;
            let rule = rules
                .into_iter()
                .find(|(channel, _)| collab_types::calls::tag(channel) == tag)
                .map(|(_, rule)| rule)
                .unwrap_or_default();
            // Only the rule's current generation has holders. Nobody takes keys of one it has
            // not reached, which a change leaving them out would then seal under; and nobody is
            // added to one it moved past, so a person let back in is never handed the keys of
            // what was said while they were left out.
            if generation != rule.generation {
                return Ok(BTreeSet::new());
            }
            // No device holds them by role: whichever device the rule admits needs them first
            // founds the group.
            holders.retain(|device| rule.may(persons.get(device).map(String::as_str)).any());
        }
        Ok(holders)
    }

    /// Devices allowed by both workspace policy and the document's declared readers.
    pub fn document_key_holders_read_by(
        &self,
        (app, document): (&str, &str),
        named: Option<&str>,
        now: SystemTime,
    ) -> Result<BTreeSet<[u8; 32]>, String> {
        let mut devices = self.document_key_holders(app, document, now)?;
        if let Some(named) = named {
            let Some(collab_types::Readers::People(people)) = collab_types::Readers::parse(named)
            else {
                return Err("invalid named readers".into());
            };
            let allowed = self.governance().reader_key_holders(&people, now)?;
            devices.retain(|device| allowed.contains(device));
        }
        Ok(devices)
    }

    /// Whether sealing with the workspace key would give this document to a
    /// device whose read access is restricted.
    pub fn requires_document_keys(
        &self,
        app: &str,
        document: &str,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.document_keys_basis(now).requires(app, document)
    }

    /// What deciding [`Self::requires_document_keys`] of many documents at `now`
    /// reads once for all of them: who holds the workspace's keys and what each
    /// may do. Asked document by document, a device holding a workspace of
    /// hundreds re-derived every key holder's access for each record it opened.
    #[must_use]
    pub fn document_keys_basis(&self, now: SystemTime) -> DocumentKeysBasis<'_> {
        DocumentKeysBasis {
            peer: self,
            now,
            holders: std::cell::OnceCell::new(),
        }
    }

    /// Validate the group bound into the signed encryption metadata. A
    /// historical write may still use the original workspace key.
    pub(super) fn check_document_key_scope(
        &self,
        (app, document): (&str, &str),
        readers: Option<&str>,
        encryption: Option<collab_types::EncryptionMetadata>,
        now: SystemTime,
        historical: bool,
    ) -> Result<(), String> {
        if let Some(collab_types::EncryptionMetadata {
            scheme: collab_types::EncryptionScheme::DocumentContentKey { group, .. },
            ..
        }) = encryption
        {
            let workspace = collab_types::WorkspaceId::parse(&self.core.workspace)
                .ok_or("invalid workspace for a document key group")?;
            let scope = neighbour_wire::KeyScope::document(workspace, app, document, readers)
                .ok_or("readers do not match the document")?;
            return (group == scope.group())
                .then_some(())
                .ok_or_else(|| "the encryption group belongs to another document".to_owned());
        }
        if neighbour_wire::KeyScope::has_named_readers(document) || readers.is_some() {
            let workspace = collab_types::WorkspaceId::parse(&self.core.workspace)
                .ok_or("invalid workspace for a reader set")?;
            neighbour_wire::KeyScope::document(workspace, app, document, readers)
                .ok_or("readers do not match the document")?;
            if encryption.is_none() {
                return Err("a named reader set requires encryption".into());
            }
        }
        if !historical
            && self.has_governance()
            && self.requires_document_keys(app, document, now)?
        {
            return Err("this document requires its own encryption group".into());
        }
        Ok(())
    }
    pub fn has_governance(&self) -> bool {
        self.core.workspace_doc.is_some()
    }
    /// An encrypted record a replica without exact admission evidence could
    /// only mark, not keep. A replica with exact admissions keeps every sealed write, as its
    /// ciphertext or, as the key holder, as the plaintext it verified.
    pub fn incomplete_reason(&self) -> Option<String> {
        (!self.core.exact_selected
            && self
                .core
                .documents
                .values()
                .any(|entry| entry.encryption_required))
        .then(|| {
            "incomplete: authenticated persisted MLS group required; plaintext downgrade forbidden"
                .into()
        })
    }
    pub(super) fn certificate(
        &self,
        source: &[u8; 32],
        now: SystemTime,
    ) -> Result<&DeviceCert, MembershipError> {
        self.core.certificate_typed(source, now).map_err(Into::into)
    }

    /// The certificate of `source` as the workspace names it among its
    /// holders, by the address it signed: a holder that is no member, which
    /// writes the redemptions of invitations it answers and nothing else
    /// ([`crate::workspace::SenderAccess::Holder`]).
    pub(super) fn holder_certificate(&self, source: &[u8; 32]) -> Option<DeviceCert> {
        let doc = self.core.workspace_doc.as_deref()?;
        crate::state::keeping_devices_of(doc)
            .into_iter()
            .find(|hint| hint.host.device_public_key == *source)
            .map(|hint| hint.host)
    }
    /// `source`'s role in the workspace now, capped for a delegate's device.
    ///
    /// # Errors
    /// Why `source` holds no role here, as [`Self::permissions`].
    pub fn role(&self, source: &[u8; 32], now: SystemTime) -> Result<Role, MembershipError> {
        backend::member::role(&self.core, source, now).map(|(role, _)| role)
    }

    /// What the keys their people approved for this workspace, and the agent
    /// devices they delegated, may do at
    /// `now`, as grants naming each key's agent
    /// ([`collab_types::workspace_grant::key_agent_id`]): what an agent of its
    /// person may ([`tilo_action_policy::Acting::handed_preset`]). A key is
    /// approved where the workspace holds its certificate, which its person's
    /// root signed, live and unrevoked, as an agent's grant reaching this
    /// workspace, and lists no revocation of its grant. That certificate is
    /// the grant its person signed when they approved the key, so
    /// whoever carries the key's calls out and signs what they change decides
    /// nothing about it: every device that holds the workspace reads the
    /// same grants, and the person and the signer still bound the agent.
    pub(crate) fn approved_keys(&self, now: SystemTime) -> Vec<tilo_action_policy::Grant> {
        #[cfg(any(test, feature = "test-support"))]
        walks::count();
        let workspace = &self.core.workspace;
        let grants = self.governance().workspace_grants();
        let now_ms = now_ns(now) / 1_000_000;
        // The workspace lists the grant once a server admitted the key to it;
        // the person revoking it there ends it, whatever its certificate says.
        let ended = |device: &[u8; 32]| {
            let id = collab_types::workspace_grant::WorkspaceGrant::id_of(device);
            grants.iter().any(|grant| {
                grant.id == id
                    && (grant.revoked_ms.is_some()
                        || grant.expires_ms.is_some_and(|expiry| now_ms >= expiry))
            })
        };
        // An agent device its person delegated, with the delegation their
        // root signed, which the workspace holds the same way.
        let delegated = self.core.certificates.keys().filter_map(|device| {
            match backend::member::role(&self.core, device, now) {
                Ok((_, Some(delegate)))
                    if delegate
                        .actor
                        .member_id()
                        .is_some_and(|member| member.agent_id().is_some()) =>
                {
                    Some(delegate.grants)
                }
                _ => None,
            }
        });
        let delegated: Vec<tilo_action_policy::Grant> = delegated.flatten().collect();
        self.core
            .certificates
            .keys()
            .filter(|device| !ended(device))
            .filter_map(|device| {
                let cert = self.certificate(device, now).ok()?;
                cert.capabilities
                    .agent
                    .filter(|scope| scope.reaches(workspace))?;
                Some(
                    tilo_action_policy::Acting::handed_agent(
                        collab_types::workspace_grant::key_agent_id(device),
                        hex::encode(cert.user_public_key),
                    )
                    .handed_preset(workspace),
                )
            })
            .flatten()
            .chain(delegated)
            .collect()
    }

    /// The heads of the workspace's governance as this peer holds it: what a
    /// write it drafts or admits says it was written under
    /// ([`SignedOp::governance`]).
    ///
    /// # Errors
    /// This peer holds no governance of the workspace, so no write of it can
    /// say what it was decided under.
    pub(crate) fn governance_heads(&self) -> Result<Vec<[u8; 32]>, String> {
        let doc = self
            .core
            .workspace_doc
            .as_ref()
            .ok_or("the workspace's governance is not here to write under")?;
        Ok(automerge::transaction::Transactable::base_heads(&**doc)
            .into_iter()
            .map(|head| head.0)
            .collect())
    }

    /// The authority of `source` for the change `envelope` makes on `record`
    /// of `app_id`, judged by the governance it was written under
    /// ([`Self::judged_under`]): refused when that governance does not grant
    /// it, held while what it is decided against is not here: that
    /// governance, its app surface, or the cut of its author's removal
    /// ([`Self::written_as`]). `retained` for a change a holder hands on from
    /// what it kept.
    pub(super) fn authorize_document(
        &self,
        source: &[u8; 32],
        app_id: &str,
        record: &str,
        envelope: &SignedOp,
        authorization_time: SystemTime,
        retained: bool,
    ) -> Result<Authorized, AuthorizeRefusal> {
        self.authorize_document_as(
            source,
            app_id,
            record,
            envelope,
            (envelope, None),
            authorization_time,
            retained,
        )
    }

    /// [`Self::authorize_document`] for `envelope`, a sealed write whose
    /// plaintext this device opened as `changes`: the write's identity, the
    /// governance heads it names and its author's authority are `envelope`'s,
    /// and what its changes say of who made them, the invocation a caller
    /// signed included, is `changes`'. A write too large for one op is
    /// decided by the changes it opened to (`opened`), which `changes` then
    /// does not carry.
    pub(super) fn authorize_document_as(
        &self,
        source: &[u8; 32],
        app_id: &str,
        record: &str,
        envelope: &SignedOp,
        changes: (&SignedOp, Option<&[automerge::Change]>),
        authorization_time: SystemTime,
        retained: bool,
    ) -> Result<Authorized, AuthorizeRefusal> {
        if envelope.author.as_bytes() != source {
            return Err(AuthorizeRefusal::Denied(crate::AdmissionError::forbidden(
                "the change is not its sender's own",
            )));
        }
        let decided = match self
            .judged_under(app_id, record, envelope, retained)
            .map_err(|error| AuthorizeRefusal::Denied(crate::AdmissionError::malformed(error)))?
        {
            Under::Missing => return Ok(Authorized::Hold(crate::MissingPrerequisite::Rules)),
            Under::Now => self.decide_written(
                source,
                app_id,
                record,
                envelope,
                changes,
                authorization_time,
            )?,
            Under::Past(judged) => judged.decide_written(
                source,
                app_id,
                record,
                envelope,
                changes,
                authorization_time,
            )?,
            // The governance it names may be on its way: what this copy holds
            // refusing it, it waits for that.
            Under::Unresolved(judged) => {
                match judged.as_deref().unwrap_or(self).decide_written(
                    source,
                    app_id,
                    record,
                    envelope,
                    changes,
                    authorization_time,
                ) {
                    Ok(decided) => decided,
                    Err(_) => {
                        return Ok(Authorized::Hold(crate::MissingPrerequisite::Rules));
                    }
                }
            }
        };
        Ok(decided)
    }

    /// [`Self::authorize_document`] by the governance this peer holds.
    fn decide_written(
        &self,
        source: &[u8; 32],
        app_id: &str,
        record: &str,
        envelope: &SignedOp,
        (changes, opened): (&SignedOp, Option<&[automerge::Change]>),
        authorization_time: SystemTime,
    ) -> Result<Authorized, AuthorizeRefusal> {
        let written = self
            .written_as(app_id, record, envelope, authorization_time)
            .map_err(|error| AuthorizeRefusal::Denied(error.into()))?;
        let Some((member, history)) = written else {
            return Ok(Authorized::Hold(crate::MissingPrerequisite::RemovalCut));
        };
        // A change made with another version of the app than the workspace runs
        // is held: its author's desktop switches to the workspace's version, and
        // the same changes then name that version's surface.
        if envelope.kind == collab_types::OpKind::Change
            && let Some(running) = self.workspace_surface(app_id)
            && envelope.surface != Some(running)
        {
            return Ok(Authorized::Hold(crate::MissingPrerequisite::AppSurface));
        }
        let route = Route {
            app: app_id.into(),
            record: record.into(),
        };
        // The document as this peer holds it, read only when changes are
        // decided against app surfaces: which of the app's resources it is,
        // and which units a change writes.
        let enforced = self.surfaces.enforced();
        let document = enforced.then(|| self.core.documents.get(&route)).flatten();
        let kind = document.and_then(|document| resource_type(&*document.doc));
        let pinned = document.and_then(|document| document_surface(&*document.doc));
        // A document this peer does not hold yet is read from its first
        // changes: they carry its unit index from the start.
        let held = match document {
            Some(document) => Some(held_units(&document.doc, changes, opened)),
            None => enforced.then(AutoCommit::new),
        };
        let cx = tilo_action_policy::Context::at_time(authorization_time);
        // A change a host made for a caller on another device proves who asked
        // with the invocation the caller signed.
        let caller = self.recorded_caller(
            app_id,
            record,
            source,
            (changes, opened),
            &member,
            authorization_time,
        )?;
        // What the caller was refused, when the author was granted the claim
        // but the caller was not: the node and kind the claim was decided at,
        // as the one builder of decision nodes names them.
        let caller_refused = std::cell::RefCell::new(None);
        let allows =
            |invoker: Option<&tilo_action_policy::Acting>, node: &TreePath, kind: ActionKind| {
                if collab_types::ChangeAuthor::answer(&member, invoker, node, kind, &cx)
                    != tilo_action_policy::Answer::Allow
                {
                    return false;
                }
                // The caller asked for this action, so the member list must let
                // them take it, not only the device that ran it for them.
                match (&caller, invoker) {
                    (Some(caller), Some(acting))
                        if caller_person(acting) == Some(caller.person.as_str()) =>
                    {
                        if collab_types::ChangeAuthor::answer(
                            &caller.access,
                            Some(acting),
                            node,
                            kind,
                            &cx,
                        ) == tilo_action_policy::Answer::Allow
                        {
                            true
                        } else {
                            *caller_refused.borrow_mut() =
                                Some((caller.person.clone(), node.to_string(), kind));
                            false
                        }
                    }
                    _ => true,
                }
            };
        // A write of a history one of the person's devices imported: what it made naming no one
        // is the person's, as the import step said.
        let imported = self
            .governance()
            .imported_invoker(source, app_id, record, envelope.seq);
        let decided = collab_types::decide_opened(
            (changes, opened),
            &collab_types::WrittenDocument {
                workspace: &self.core.workspace,
                app: app_id,
                document: record,
            },
            (kind.as_deref(), held, pinned),
            document.map(|document| document.doc.committed_document()),
            &self.surfaces,
            &allows,
            imported.as_ref(),
        )
        .and_then(|receipt| match receipt {
            collab_types::Receipt::Admit => self
                .recorded_invocations((changes, opened), app_id, authorization_time)
                .map(|()| receipt),
            collab_types::Receipt::Hold => Ok(receipt),
        });
        match decided {
            Ok(collab_types::Receipt::Admit) => Ok(Authorized::Admit {
                by_governor: self.wrote_as_governor(source, history),
                member,
                history,
            }),
            Ok(collab_types::Receipt::Hold) => {
                Ok(Authorized::Hold(crate::MissingPrerequisite::AppSurface))
            }
            Err(refusal) => {
                // The author was granted the claim and its caller was not:
                // the refusal is the caller's, not the author's.
                let refusal = match caller_refused.borrow().clone() {
                    Some((person, node, kind)) => {
                        collab_types::ReceiptRefusal::CallerNotGranted { person, node, kind }
                    }
                    None => refusal,
                };
                let said = refused_change(&refusal, &route, source, envelope);
                Err(AuthorizeRefusal::Rules(refusal, said))
            }
        }
    }

    /// The caller a change `envelope` of `record` in `app_id` was made for, if
    /// any: a history entry names a caller on another device, so the change
    /// must carry the invocation that caller signed, and it must check.
    /// `Ok(None)` for a change a device made for itself, which names no caller
    /// on another device and needs none.
    ///
    /// The caller's access is taken under the governance this copy judges the
    /// write by ([`Self::decide_written`]), never the governance it holds now
    /// when that is not the one the write names.
    fn recorded_caller(
        &self,
        app_id: &str,
        record: &str,
        source: &[u8; 32],
        (envelope, opened): (&SignedOp, Option<&[automerge::Change]>),
        member: &MemberAccess,
        authorization_time: SystemTime,
    ) -> Result<Option<CallerAccess>, AuthorizeRefusal> {
        let parsed;
        let changes = match opened {
            Some(opened) => opened,
            None => {
                let Some(changes) = cleartext_changes_of(envelope) else {
                    return Ok(None);
                };
                parsed = changes;
                parsed.as_slice()
            }
        };
        self.callers_named_in(
            app_id,
            record,
            source,
            envelope,
            changes,
            member,
            authorization_time,
        )
    }

    /// The caller the changes `changes` of `envelope` name, each invocation
    /// they keep checked: what [`Self::recorded_caller`] asks of a received
    /// change, and a device opening a sealed write asks of the open one.
    pub(super) fn callers_named_in(
        &self,
        app_id: &str,
        record: &str,
        source: &[u8; 32],
        envelope: &SignedOp,
        changes: &[automerge::Change],
        member: &MemberAccess,
        authorization_time: SystemTime,
    ) -> Result<Option<CallerAccess>, AuthorizeRefusal> {
        let route = Route {
            app: app_id.into(),
            record: record.into(),
        };
        let mut caller: Option<CallerAccess> = None;
        for change in changes {
            let Some(context) = change
                .message()
                .and_then(tilo_actions::ActionContext::from_change_message)
            else {
                continue;
            };
            // Every invocation a change keeps is checked, whether or not it
            // names a caller on another device.
            let certificate = tilo_server_protocol::invocation::verify_recorded(
                &context,
                &self.core.workspace,
                &self.core.revocations,
                crate::state::now_ns(authorization_time),
            )
            .map_err(|error| {
                refused_invocation(
                    &route,
                    source,
                    envelope,
                    &format!("the invocation a change made for a caller does not check: {error}"),
                )
            })?;
            let Some(invoker) = context.invoker.as_ref() else {
                // Made naming no one: the person's own, when an import step holds the write.
                if self
                    .governance()
                    .imported_invoker(source, app_id, record, envelope.seq)
                    .is_some()
                {
                    continue;
                }
                return Err(refused_no_invoker(&route, source, envelope));
            };
            if invoker.device.is_none() {
                continue;
            }
            let Some(certificate) = certificate else {
                return Err(refused_invocation(
                    &route,
                    source,
                    envelope,
                    "a change made for a caller carries no invocation that caller signed",
                ));
            };
            let person = hex::encode(certificate.user_public_key);
            match &caller {
                Some(held) if held.person != person => {
                    return Err(refused_invocation(
                        &route,
                        source,
                        envelope,
                        "the changes of one action name more than one caller",
                    ));
                }
                Some(_) => {}
                None => {
                    // The member list at the write's heads decides the caller:
                    // the role and grants its person had then, and the keys
                    // approved then.
                    let access = MemberAccess::person(member.policy.clone(), person.clone())
                        .granted(self.approved_keys(authorization_time));
                    caller = Some(CallerAccess { person, access });
                }
            }
        }
        Ok(caller)
    }
}

/// `reason` as a change of `route` from `source` is refused: typed as the
/// workspace's rules refuse a change, and in words that name who made it and
/// which device signed it.
fn refused_invocation(
    route: &Route,
    source: &[u8; 32],
    envelope: &SignedOp,
    reason: &str,
) -> AuthorizeRefusal {
    let refusal = collab_types::ReceiptRefusal::SignedInvocation(reason.to_owned());
    let said = refused_change(&refusal, route, source, envelope);
    AuthorizeRefusal::Rules(refusal, said)
}

/// A change of `route` from `source` that names no invoker is refused: every
/// change says who made it, a device's own person or agent or a caller that
/// signed.
///
/// A compaction and a checkpoint are no changes of their own: they carry
/// other writes, which keep their own invokers, and they are judged by those
/// and by their signer. They are exempt from this rule for that reason, and
/// not from admission.
fn refused_no_invoker(route: &Route, source: &[u8; 32], envelope: &SignedOp) -> AuthorizeRefusal {
    let refusal = collab_types::ReceiptRefusal::NoInvoker;
    let said = refused_change(&refusal, route, source, envelope);
    AuthorizeRefusal::Rules(refusal, said)
}

impl StatePeer {
    /// That every invocation `envelope`'s changes keep shows who asked for it
    /// ([`tilo_server_protocol::invocation::verify_recorded`]), against this workspace's
    /// revocations as of `written`, when the change was written: an agent acting with its own key
    /// asked under a key its person certified for it. What a sealed op keeps is checked by the
    /// devices that open it, from the changes it opened to (`opened`, for one too large for an op).
    fn recorded_invocations(
        &self,
        (envelope, opened): (&SignedOp, Option<&[automerge::Change]>),
        app: &str,
        written: SystemTime,
    ) -> Result<(), collab_types::ReceiptRefusal> {
        let contexts: Vec<tilo_actions::ActionContext> = match opened {
            Some(opened) => opened
                .iter()
                .filter_map(automerge::Change::message)
                .filter_map(tilo_actions::ActionContext::from_change_message)
                .collect(),
            None => collab_types::recorded_contexts(envelope)?,
        };
        for context in contexts {
            tilo_server_protocol::invocation::verify_recorded(
                &context,
                &self.core.workspace,
                &self.core.revocations,
                crate::state::now_ns(written),
            )
            .map_err(|error| collab_types::ReceiptRefusal::Invocation {
                code: error.code(),
                reason: error.to_string(),
            })?;
        }
        // A resource store's change keeps the invocation its caller signed in its stamp: it is
        // that app's, made under the invocation signed for it.
        let stamps: Vec<collab_types::Stamp> = match opened {
            Some(opened) => opened
                .iter()
                .filter_map(automerge::Change::message)
                .filter_map(collab_types::Stamp::of_message)
                .collect(),
            None => collab_types::receipt::recorded_stamps(envelope)?,
        };
        for stamp in stamps {
            tilo_server_protocol::invocation::verify_stamp(
                &stamp,
                &self.core.workspace,
                app,
                &self.core.revocations,
            )
            .map_err(|error| collab_types::ReceiptRefusal::Invocation {
                code: error.code(),
                reason: error.to_string(),
            })?;
        }
        Ok(())
    }
}

/// What a change the workspace's rules refuse by `refusal` is told, logged
/// with who the change names as having made it and which device signed it:
/// what a refusal report needs to say whose change it was. A key the
/// workspace holds no grant for is told how to get one.
fn refused_change(
    refusal: &collab_types::ReceiptRefusal,
    route: &impl std::fmt::Debug,
    source: &[u8; 32],
    envelope: &SignedOp,
) -> String {
    let claimed = collab_types::claimed_invokers(envelope);
    let invokers: Vec<String> = claimed
        .iter()
        .map(|invoker| serde_json::to_string(invoker).unwrap_or_default())
        .collect();
    // A key the workspace holds no grant for is told how to get one.
    let by_key = claimed.iter().any(|invoker| {
        invoker.nodes().into_iter().any(|node| {
            node.agent_id()
                .and_then(tilo_action_policy::key_of_agent)
                .is_some()
        })
    });
    tracing::warn!(
        route = ?route,
        signer = %hex::encode(source),
        invokers = ?invokers,
        %refusal,
        "a change was refused"
    );
    let mut said = format!(
        "document change forbidden: {refusal} (made by {}, signed by device {})",
        if invokers.is_empty() {
            "its signer".to_owned()
        } else {
            invokers.join(", ")
        },
        hex::encode(source)
    );
    if by_key {
        said.push_str(KEY_NOT_APPROVED);
    }
    said
}

/// The key holders of one state of a workspace's governance at one moment, read
/// once for every document asked of them ([`StatePeer::document_keys_basis`]).
pub struct DocumentKeysBasis<'p> {
    peer: &'p StatePeer,
    now: SystemTime,
    holders: std::cell::OnceCell<Result<KeyAccess, String>>,
}

/// Every device holding a workspace's keys, and the access of each that is a member's.
type KeyAccess = (
    Vec<[u8; 32]>,
    std::collections::BTreeMap<[u8; 32], MemberAccess>,
);

impl DocumentKeysBasis<'_> {
    /// The workspace's key holders and their access, read on the first document that needs them.
    fn holders(&self) -> Result<&KeyAccess, String> {
        self.holders
            .get_or_init(|| {
                crate::work::key_access_read();
                let governance = self.peer.governance();
                let devices: Vec<_> = governance.key_holders(self.now)?.into_iter().collect();
                let access = governance.access_of(&devices, self.now)?;
                Ok((devices, access))
            })
            .as_ref()
            .map_err(Clone::clone)
    }

    /// [`StatePeer::requires_document_keys`] of `document` of `app`.
    pub fn requires(&self, app: &str, document: &str) -> Result<bool, String> {
        let (peer, now) = (self.peer, self.now);
        if collab_types::validator::requires_independent_document_keys(app, document)
            || peer.publication().has_document_keys(app, document)
        {
            return Ok(true);
        }
        let governance = peer.governance();
        let policy = peer
            .core
            .workspace_doc
            .as_ref()
            .ok_or("workspace governance missing")?
            .workspace_policy(&peer.core.workspace);
        let node = collab_types::document_node(&peer.core.workspace, app, document);
        let node = tilo_action_policy::roles::decided_node(&node);
        let cx = tilo_action_policy::Context::at_time(now);
        // A rule can exclude a person before they join on a device. Protect
        // the history now, before that device receives the workspace key.
        if policy.grants.iter().any(|grant| {
            grant.kinds.contains(ActionKind::Read)
                && grant.answer != tilo_action_policy::Answer::Allow
                && grant.is_live(&cx)
                && cx.covers(&grant.node, &node)
        }) {
            return Ok(true);
        }
        let (devices, access) = self.holders()?;
        Ok(devices.iter().any(|device| match access.get(device) {
            Some(member) => !peer.holds_document_keys_as(member, (app, document), now),
            // A holder the governance names to hold the workspace's keys reads
            // what they seal because it is named so, not through a member's
            // role. A server that takes no member's work, or no longer does,
            // has no member access, and the workspace key reaches it by right.
            None => governance.holder_role(device) != Some(super::CopyRole::KeyHolder),
        }))
    }
}
