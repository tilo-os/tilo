//! Who a device is in the workspace and what it may do: the certificates a
//! replica keeps for it, the role and grants that decide its changes, and the
//! governance each of those is checked against.

use super::{
    ActionKind, Answer, Authority, AutoCommit, DeviceCert, Ed25519PublicKey, GovernedIdentity,
    MembershipError, Principal, Role, Route, SignedOp, StatePeer, SystemTime, TreePath,
    WorkspacePolicy, backend, err, held_units, now_ns, record_kind, record_surface, workspace,
    workspace_host_actor,
};
use collab_types::signer::DeviceSigner;

/// A current member's authority in the workspace: who they are, their role, the
/// workspace's policy as admitted, and whether their device is held to the
/// foreign-device bound.
#[derive(Clone, Debug)]
pub struct MemberAuthority {
    pub role: Role,
    pub person: String,
    pub policy: WorkspacePolicy,
    pub foreign: bool,
    /// The bound on a delegate's device, an agent's or a bot's: its
    /// person's grants within its own, never above an Editor's.
    pub delegate: Option<crate::state::ActingAs>,
}
/// `person`'s standing under `policy`, held to the foreign-device bound on a
/// `foreign` device and to `delegate`'s bound on a delegate's.
fn standing_of(
    policy: impl Into<std::sync::Arc<WorkspacePolicy>>,
    person: String,
    foreign: bool,
    delegate: Option<crate::state::ActingAs>,
) -> tilo_action_policy::Standing {
    let mut standing = tilo_action_policy::Standing::person(policy, person);
    if foreign {
        standing = standing.on_foreign_device();
    }
    if let Some(delegation) = delegate {
        standing = standing.acting_as(delegation.actor, delegation.grants);
    }
    standing
}
impl MemberAuthority {
    /// The member's standing in the workspace, held to the foreign-device bound
    /// on a foreign device: what a receiver decides their ops with.
    #[must_use]
    pub fn standing(self) -> tilo_action_policy::Standing {
        standing_of(self.policy, self.person, self.foreign, self.delegate)
    }

    /// The member's authority, capped on a foreign device.
    #[must_use]
    pub fn authority(&self) -> Authority<'_> {
        backend::access::member_authority(
            &self.policy,
            &self.person,
            self.foreign,
            self.delegate.as_ref(),
        )
    }
    /// Whether the member may perform an action of `kind` at `node` at `now`.
    #[must_use]
    pub fn allows(&self, node: &TreePath, kind: ActionKind, now: SystemTime) -> bool {
        self.authority()
            .decide(node, kind, &tilo_action_policy::Context::at_time(now))
            .answer
            == Answer::Allow
    }

    /// Whether the member may run `action` of `resource` in `app` now: the
    /// workspace's own grants for that app action decide it, whether the
    /// member acts as a person's device, an agent or a bot.
    #[must_use]
    pub fn allows_app_action(
        &self,
        app: &str,
        resource: &str,
        action: &str,
        now: SystemTime,
    ) -> bool {
        let Ok(named) = crate::workspace_membership::AppAction::new(app, app, resource, action)
        else {
            return false;
        };
        self.allows(&named.node(&self.policy.node()), ActionKind::Undoable, now)
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
    /// elsewhere: a page signs with the key its custody holds.
    pub(crate) fn require_member_device(
        &self,
        device: &[u8; 32],
        now: SystemTime,
    ) -> Result<(), String> {
        self.certificate(device, now)
            .map_err(|error| error.to_string())?;
        self.role(device, now).map(drop)
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
    /// The current authority of the device `source`: its person, role, the
    /// workspace's policy and whether it is held to the foreign-device bound.
    ///
    /// # Errors
    /// Returns [`MembershipError`] saying why the device holds no authority
    /// here: an unknown, expired or revoked certificate, a device absent from
    /// governance, or a person who is no longer a member.
    pub fn permissions(
        &self,
        source: [u8; 32],
        now: SystemTime,
    ) -> Result<MemberAuthority, MembershipError> {
        let member = self.member(&source, now)?;
        // `member` above already read the workspace document this policy is
        // decided against, and the peer holds it for its whole life.
        let policy = self
            .core
            .workspace_doc
            .as_ref()
            .ok_or(MembershipError::GovernanceMissing)?
            .workspace_policy(&self.core.workspace)
            .clone();
        Ok(MemberAuthority {
            role: member.role,
            person: member.person,
            policy,
            foreign: member.foreign,
            delegate: member.delegate,
        })
    }
    /// The standing of the device `source` under `policy`, the workspace's
    /// own: [`Self::permissions`]'s standing, sharing the policy every
    /// member device's standing holds.
    pub(crate) fn member_with(
        &self,
        source: [u8; 32],
        policy: std::sync::Arc<WorkspacePolicy>,
        now: SystemTime,
    ) -> Result<tilo_action_policy::Standing, MembershipError> {
        let member = self.member(&source, now)?;
        Ok(standing_of(
            policy,
            member.person,
            member.foreign,
            member.delegate,
        ))
    }
    /// [`Self::permissions`] without the workspace's policy.
    fn member(
        &self,
        source: &[u8; 32],
        now: SystemTime,
    ) -> Result<backend::access::Member, MembershipError> {
        let (role, agent) = backend::member::legacy(&self.core, source, now)?;
        let cert = self.certificate(source, now)?;
        let person = Principal::person_from_key(Ed25519PublicKey::from_bytes(cert.user_public_key));
        Ok(backend::access::Member {
            role,
            person: person.id().to_owned(),
            foreign: backend::member::foreign(cert, &self.core.origin),
            delegate: agent,
        })
    }
    /// [`Self::permissions`] as the text refusal the workspace's own write paths
    /// report to the app that asked.
    pub(crate) fn membership(
        &self,
        device: [u8; 32],
        now: SystemTime,
    ) -> Result<MemberAuthority, String> {
        self.permissions(device, now)
            .map_err(|error| error.to_string())
    }
    /// Whether the device `source` may run `action` of `resource` in `app`
    /// here now: the workspace's per-action permission for whatever its
    /// principal is decides it
    /// ([`MemberAuthority::allows_app_action`]), never the kind of
    /// collaborator a device belongs to.
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
        Ok(member.allows_app_action(app, resource, action, now))
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
        backend::access::check_document_read(
            &member.policy,
            &member.person,
            member.foreign,
            member.delegate.as_ref(),
            (&self.core.workspace, app, record),
            now,
        )
    }
    /// [`Self::check_document_read`] under `policy`, the workspace's current
    /// policy, which a caller deciding many devices builds once rather than
    /// once for each of them.
    pub(crate) fn check_document_read_under(
        &self,
        policy: &WorkspacePolicy,
        device: [u8; 32],
        app: &str,
        record: &str,
        now: SystemTime,
    ) -> Result<(), String> {
        let member = self
            .member(&device, now)
            .map_err(|error| error.to_string())?;
        backend::access::check_document_read(
            policy,
            &member.person,
            member.foreign,
            member.delegate.as_ref(),
            (&self.core.workspace, app, record),
            now,
        )
    }
    pub fn has_governance(&self) -> bool {
        self.core.workspace_doc.is_some()
    }
    /// An encrypted record a replica without exact admission evidence could
    /// only mark, not keep. A v5 replica keeps every sealed write, as its
    /// ciphertext or, as the key holder, as the plaintext it verified.
    pub fn incomplete_reason(&self) -> Option<String> {
        (!self.core.admission_v5
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
    pub(super) fn role(&self, source: &[u8; 32], now: SystemTime) -> Result<Role, String> {
        backend::member::legacy(&self.core, source, now)
            .map(|(role, _)| role)
            .map_err(err)
    }

    /// The bot actor `source` may show other names as: a bot device that
    /// acts in the workspace now (live delegation, live installation, installer
    /// still a member) and whose installation allows `Personas`.
    pub(super) fn appearance_author(&self, source: &[u8; 32], now: SystemTime) -> Option<String> {
        self.permissions(*source, now).ok()?;
        let doc = self.core.workspace_doc.as_ref()?;
        let bytes = doc.policy().identities.get(&hex::encode(source))?.clone();
        let identity = GovernedIdentity::decode(&bytes).ok()?;
        let installation = identity.delegate?.installation?;
        (installation.appearance == crate::installation::AppearancePolicy::Personas)
            .then(|| format!("bot:{}", installation.id))
    }

    /// The authority of `source` for the change `envelope` makes on `record`
    /// of `app_id`, refused when the change is not granted; `None` while
    /// the app surface it is decided against is not here.
    pub(super) fn authorize_document(
        &self,
        source: &[u8; 32],
        app_id: &str,
        record: &str,
        envelope: &SignedOp,
        authorization_time: SystemTime,
    ) -> Result<Option<MemberAuthority>, String> {
        let member = self.membership(*source, authorization_time)?;
        // A change made with another version of the app than the workspace runs
        // is held: its author's desktop switches to the workspace's version, and
        // the same changes then name that version's surface.
        if envelope.kind == collab_types::OpKind::Change
            && let Some(running) = self.workspace_surface(app_id)
            && envelope.surface != Some(running)
        {
            return Ok(None);
        }
        let route = Route {
            app: app_id.into(),
            record: record.into(),
        };
        // The record as this peer holds it, read only when changes are
        // decided against app surfaces: which of the app's resources it is,
        // and which units a change writes.
        let enforced = self.surfaces.enforced();
        let document = enforced.then(|| self.core.documents.get(&route)).flatten();
        let kind = document.and_then(|document| record_kind(&*document.doc));
        let pinned = document.and_then(|document| record_surface(&*document.doc));
        // A record this peer does not hold yet is read from its first
        // changes: they carry its unit index from the start.
        let held = match document {
            Some(document) => Some(held_units(&document.doc, envelope)),
            None => enforced.then(AutoCommit::new),
        };
        let allows =
            |node: &TreePath, kind: ActionKind| member.allows(node, kind, authorization_time);
        let decided = collab_types::decide_op_pinned(
            envelope,
            &collab_types::Record {
                room: &self.core.workspace,
                app: app_id,
                record,
            },
            kind.as_deref(),
            held,
            pinned,
            &self.surfaces,
            &|node, kind| allows(node, kind),
        );
        match decided {
            Ok(collab_types::Receipt::Admit) => Ok(Some(member)),
            Ok(collab_types::Receipt::Hold) => Ok(None),
            Err(refusal) => Err(format!("document change forbidden: {refusal}")),
        }
    }
}
