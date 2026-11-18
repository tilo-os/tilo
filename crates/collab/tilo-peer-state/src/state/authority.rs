//! Who a device is in the workspace and what it may do: the certificates a
//! replica keeps for it, the role and grants that decide its changes, and the
//! governance each of those is checked against.

use super::{
    ActionKind, AutoCommit, DeviceCert, GovernedIdentity, MemberAccess, MembershipError, Role,
    Route, SignedOp, StatePeer, SystemTime, TreePath, WorkspacePolicy, backend, document_surface,
    err, held_units, now_ns, resource_type, workspace, workspace_host_actor,
};
use collab_types::signer::DeviceSigner;

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
    /// the change ([`crate::removals`]).
    Admit { member: MemberAccess, history: bool },
    /// Not decided until what it waits for is here.
    Hold(crate::MissingPrerequisite),
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

    /// Whether `key`'s device may take `action` on the workspace's governance
    /// now: its person's role there and the grants on it decide, as every
    /// receiver of a governance operation judges it
    /// ([`crate::workspace::membership`]). The owner's role holds every
    /// governance action by default.
    pub(super) fn require_governance_action(
        &self,
        key: &dyn DeviceSigner,
        action: &str,
        now: SystemTime,
    ) -> Result<(), String> {
        let member = self.membership(key.device_public_key(), now)?;
        member
            .policy
            .check_action(
                &member.authority(),
                action,
                &tilo_action_policy::Context::at_time(now),
                false,
            )
            .map_err(|refusal| refusal.to_string())
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

    /// Whether `device` is a known, unrevoked device of the owner's, at no
    /// particular time: for evidence a device of theirs signed earlier.
    #[must_use]
    pub fn signed_by_owner(&self, device: &[u8; 32]) -> bool {
        self.known_device(device)
            && self
                .core
                .certificates
                .get(device)
                .is_some_and(|cert| cert.user_public_key == self.core.owner)
    }

    /// The member's own record `app`'s `record` is, as a write of it or the
    /// app keeping it said ([`Self::keep_own_document`]); `None` for a record
    /// of the workspace.
    #[must_use]
    pub fn own_resource(&self, app: &str, record: &str) -> Option<collab_types::OwnResource> {
        self.core.own_of(app, record)
    }

    /// [`Self::own_resource`] for a write the device `author` signs: also the
    /// author's person's own record of a kind the app's manifest declares each
    /// member's own ([`collab_types::OwnResource::recognise`]).
    #[must_use]
    pub fn own_write(
        &self,
        app: &str,
        record: &str,
        author: &[u8; 32],
    ) -> Option<collab_types::OwnResource> {
        self.core.own_write(app, record, author)
    }

    /// The person (user root key) of every member's own record this replica
    /// knows, by app and record.
    #[must_use]
    pub fn own_resources(&self) -> std::collections::BTreeMap<(String, String), [u8; 32]> {
        self.core
            .own
            .iter()
            .map(|(route, own)| ((route.app.clone(), route.record.clone()), own.owner))
            .collect()
    }

    /// Keep `app`'s record `own` names as that member's own record: every
    /// write of it this replica signs carries its kind and person
    /// ([`neighbour_wire::NeighbourMsg::AppDocument`]'s `own`).
    pub fn keep_own_document(&mut self, app: &str, own: &collab_types::OwnResource) {
        self.core.learn_own(app, &own.document_id(), Some(own));
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
            .ok_or(MembershipError::GovernanceMissing)?
            .workspace_policy(&self.core.workspace)
            .clone();
        let (person, foreign, delegate) = self.member_of(&source, now)?;
        Ok(access_of(policy, person, foreign, delegate))
    }
    /// The access of the device `source` under `policy`, the workspace's
    /// own: [`Self::permissions`]'s access, sharing the policy every
    /// member device's access holds.
    pub(crate) fn member_with(
        &self,
        source: [u8; 32],
        policy: std::sync::Arc<WorkspacePolicy>,
        now: SystemTime,
    ) -> Result<MemberAccess, MembershipError> {
        let (person, foreign, delegate) = self.member_of(&source, now)?;
        Ok(access_of(policy, person, foreign, delegate))
    }
    /// Who the device `source` is at `now`, apart from the workspace's policy:
    /// its person, whether its device is foreign, and the delegation it holds.
    fn member_of(
        &self,
        source: &[u8; 32],
        now: SystemTime,
    ) -> Result<(String, bool, Option<crate::state::ActingAs>), MembershipError> {
        let (_, delegate) = backend::member::legacy(&self.core, source, now)?;
        let cert = self.certificate(source, now)?;
        let person = hex::encode(cert.user_public_key);
        Ok((
            person,
            backend::member::foreign(cert, &self.core.origin),
            delegate,
        ))
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
            .ok_or(MembershipError::GovernanceMissing)?
            .workspace_policy(&self.core.workspace)
            .clone();
        let (_, delegate) = backend::member::legacy_in_history(&self.core, source, now)?;
        let cert = self
            .core
            .certificate_held(source, now)
            .map_err(MembershipError::from)?;
        Ok(access_of(
            policy,
            hex::encode(cert.user_public_key),
            backend::member::foreign(cert, &self.core.origin),
            delegate,
        ))
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
    /// # Errors
    /// Why the author may not make the write, as [`Self::permissions`].
    pub(super) fn written_as(
        &self,
        app: &str,
        document: &str,
        write: &SignedOp,
        now: SystemTime,
    ) -> Result<Option<(MemberAccess, bool)>, MembershipError> {
        let author = write.author.as_bytes();
        match self.permissions(*author, now) {
            Ok(member) => Ok(Some((member, false))),
            Err(MembershipError::Revoked) => {
                let Some(workspace) = self.core.workspace_doc.as_deref() else {
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
    pub fn permissions_as_written(
        &self,
        app: &str,
        document: &str,
        write: &SignedOp,
        now: SystemTime,
    ) -> Result<MemberAccess, MembershipError> {
        self.written_as(app, document, write, now)?
            .map(|(member, _)| member)
            .ok_or(MembershipError::Revoked)
    }
    /// Whether the device that wrote a change is one of the owner's: a known
    /// device of theirs now, or, for a change that is `history`, one they
    /// removed since.
    pub(super) fn wrote_as_owner(&self, device: &[u8; 32], history: bool) -> bool {
        self.signed_by_owner(device)
            || (history
                && self
                    .core
                    .certificates
                    .get(device)
                    .is_some_and(|cert| cert.user_public_key == self.core.owner))
    }
    /// The id the cut of a removal names `write`, a write to `document` of
    /// `app`, by: `None` when the write does not encode.
    fn write_id(&self, app: &str, document: &str, write: &SignedOp) -> Option<[u8; 32]> {
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
    /// ([`workspace::media::security_basis`]) under this replica's
    /// revocations, reading the identity rows its governance state decoded
    /// once, and worked out once for that state: every copy of this replica
    /// that holds the same governance answers with it. `None` while no
    /// workspace is admitted.
    pub fn security_basis(&self) -> Option<Result<[u8; 32], workspace::media::MediaPolicyError>> {
        let doc = self.core.workspace_doc.as_ref()?;
        Some(doc.security_basis(&self.revocations()))
    }

    /// Committed, admitted workspace only. Callers cannot mutate the admission owner.
    pub fn admitted_workspace(&self) -> Option<&AutoCommit> {
        self.core.workspace_doc.as_deref()
    }

    /// Read permission for an exact canonical record using the same governed
    /// actor/resource vocabulary as document mutation admission.
    pub fn check_document_read(
        &self,
        device: [u8; 32],
        app: &str,
        record: &str,
        now: SystemTime,
    ) -> Result<(), String> {
        let member = self.membership(device, now)?;
        let own = self.core.own_write(app, record, &device);
        if backend::access::document_read(
            &member.authority(),
            &self.core.workspace,
            (app, record),
            own.as_ref(),
            now,
        ) {
            Ok(())
        } else {
            Err(format!("record read forbidden: {app}/{record}"))
        }
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
    /// Validated data from this exact snapshot, not an authorization credential.
    /// Missing exact Chat prerequisites and unpromoted local revocations fail
    /// closed. Current device permissions/time still require separate checks.
    pub fn validated_media_policy(
        &self,
    ) -> Result<Option<workspace::media::WorkspaceMediaV1>, String> {
        let policy = self.validate_present_media_policy()?;
        if let Some(policy) = &policy {
            backend::media::ready(
                policy,
                |app, record| backend::media::legacy_binding(self, policy, app, record),
                || {
                    self.security_basis()
                        .ok_or("missing workspace")?
                        .map_err(err)
                },
            )
            .map_err(backend::media::ReadinessError::legacy)?;
        }
        Ok(policy)
    }
    pub(super) fn validate_present_media_policy(
        &self,
    ) -> Result<Option<workspace::media::WorkspaceMediaV1>, String> {
        self.core
            .workspace_doc
            .as_deref()
            .map(|doc| {
                workspace::media::validate_transition(
                    None,
                    doc,
                    workspace::media::TransitionMode::Restore,
                )
                .map_err(err)
            })
            .transpose()
            .map(Option::flatten)
    }
    pub(super) fn validate_media_bindings(
        &self,
        policy: &workspace::media::WorkspaceMediaV1,
        require_complete: bool,
    ) -> Result<(), String> {
        backend::media::bindings(policy, require_complete, |app, record| {
            backend::media::legacy_binding(self, policy, app, record)
        })
        .map_err(backend::media::ReadinessError::legacy)
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
        backend::member::legacy(&self.core, source, now).map(|(role, _)| role)
    }

    /// The bot actor `source` may show other names as: a bot device that
    /// acts in the workspace now (live delegation, live installation, installer
    /// still a member) and whose installation allows `Personas`.
    pub(super) fn appearance_author(
        &self,
        source: &[u8; 32],
        now: SystemTime,
        history: bool,
    ) -> Option<collab_types::MemberId> {
        // A change that is history of a device removed since showed what the
        // device could show when the workspace took it.
        if history {
            self.permissions_in_history(source, now).ok()?;
        } else {
            self.permissions(*source, now).ok()?;
        }
        let doc = self.core.workspace_doc.as_ref()?;
        let bytes = doc.policy().identities.get(&hex::encode(source))?.clone();
        let identity = GovernedIdentity::decode(&bytes).ok()?;
        let installation = identity.delegate?.installation?;
        (installation.appearance == crate::installation::AppearancePolicy::Personas)
            .then(|| collab_types::MemberId::bot(installation.id))
    }

    /// The authority of `source` for the change `envelope` makes on `record`
    /// of `app_id`, refused when the change is not granted; held while what
    /// it is decided against is not here: its app surface, or the cut of its
    /// author's removal ([`Self::written_as`]).
    pub(super) fn authorize_document(
        &self,
        source: &[u8; 32],
        app_id: &str,
        record: &str,
        envelope: &SignedOp,
        authorization_time: SystemTime,
    ) -> Result<Authorized, String> {
        if envelope.author.as_bytes() != source {
            return Err("the change is not its sender's own".into());
        }
        let Some((member, history)) = self
            .written_as(app_id, record, envelope, authorization_time)
            .map_err(|error| error.to_string())?
        else {
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
            Some(document) => Some(held_units(&document.doc, envelope)),
            None => enforced.then(AutoCommit::new),
        };
        // A member's own record is decided as its person's: they do anything in
        // it whatever their role, and nobody else anything.
        let person = self
            .core
            .own_write(app_id, record, source)
            .map(|own| own.person());
        let mut cx = tilo_action_policy::Context::at_time(authorization_time);
        if let Some(person) = &person {
            cx = cx.with_own_resource(record, person);
        }
        let allows =
            |invoker: Option<&tilo_action_policy::Acting>, node: &TreePath, kind: ActionKind| {
                collab_types::ChangeAuthor::answer(&member, invoker, node, kind, &cx)
                    == tilo_action_policy::Answer::Allow
            };
        let decided = collab_types::decide_op_pinned(
            envelope,
            &collab_types::WrittenDocument {
                workspace: &self.core.workspace,
                app: app_id,
                document: record,
            },
            kind.as_deref(),
            held,
            pinned,
            &self.surfaces,
            &allows,
        );
        match decided {
            Ok(collab_types::Receipt::Admit) => Ok(Authorized::Admit { member, history }),
            Ok(collab_types::Receipt::Hold) => {
                Ok(Authorized::Hold(crate::MissingPrerequisite::AppSurface))
            }
            Err(refusal) => Err(format!("document change forbidden: {refusal}")),
        }
    }
}
