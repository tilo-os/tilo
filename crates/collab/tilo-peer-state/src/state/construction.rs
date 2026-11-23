//! Building a replica, and admitting a device into its governed history.

use super::{
    BTreeMap, CachedDocument, DeviceCert, ExactAdmissions, GovernedIdentity, Grantee, PeerCore,
    PeerGovernanceMut, ROOT, RevocationList, Role, Shared, StatePeer, SystemTime, VecDeque,
    VerifiedCertificate, administrator_grant_id, err, held, now_ns, process_default, supersedes,
    workspace,
};
use collab_types::signer::DeviceSigner;
use tilo_action_policy::Change;

use automerge::{ReadDoc as _, transaction::Transactable as _};

impl StatePeer {
    /// Build a replica for `workspace`, owned by the person `owner_device` belongs
    /// to, with that device and the certified `roster`.
    pub fn new(
        workspace: &str,
        owner_device: DeviceCert,
        roster: Vec<DeviceCert>,
        revocations: RevocationList,
        now: SystemTime,
    ) -> Result<Self, String> {
        let now = now_ns(now);
        let roster = roster
            .into_iter()
            .map(|certificate| VerifiedCertificate::new(certificate, now))
            .collect::<Result<Vec<_>, _>>()?;
        Self::with_roster(workspace, owner_device, roster, revocations, now)
    }

    /// A replica of a workspace joined through `admitter`, a key holder of
    /// the workspace, which must be valid at `now`, and
    /// the workspace's `roster` as its governance admitted it. The admitter
    /// is the replica's origin, and the person of its owner only until the
    /// workspace's governance arrives and names the owner the replica holds
    /// to ([`Self::from_join`]). A roster
    /// certificate is the workspace's evidence, checked by its signature alone:
    /// a guest certified for an hour stays in the roster after its hour, as
    /// on the host, and its expiry is checked whenever it is used. Building
    /// the replica at `now` refused the whole workspace over one expired guest,
    /// so no device could join a workspace anyone had watched.
    pub fn joined(
        workspace: &str,
        admitter: DeviceCert,
        roster: Vec<DeviceCert>,
        revocations: RevocationList,
        now: SystemTime,
    ) -> Result<Self, String> {
        let roster = roster
            .into_iter()
            .map(|certificate| VerifiedCertificate::historical_typed(certificate).map_err(err))
            .collect::<Result<Vec<_>, _>>()?;
        Self::with_roster(workspace, admitter, roster, revocations, now_ns(now))
    }

    fn with_roster(
        workspace: &str,
        owner_device: DeviceCert,
        roster: Vec<VerifiedCertificate>,
        revocations: RevocationList,
        now: u64,
    ) -> Result<Self, String> {
        let workspace = uuid::Uuid::parse_str(workspace).map_err(err)?.to_string();
        let (owner, origin) = (owner_device.user_public_key, owner_device.device_public_key);
        let mut certificates = BTreeMap::new();
        for certificate in roster
            .into_iter()
            .map(Ok)
            .chain([VerifiedCertificate::new(owner_device, now)])
        {
            let certificate = certificate?;
            if revocations.refuses(&certificate) {
                return Err("device revoked".into());
            }
            certificates.insert(certificate.device_public_key, certificate);
        }
        Ok(Self {
            core: PeerCore {
                workspace,
                owner,
                origin,
                certificates: certificates.into(),
                revocations,
                receipts: Shared::default(),
                workspace_doc: None,
                documents: super::documents::Documents::default(),
                pending: VecDeque::new(),
                pending_bytes: 0,
                exact_admissions: ExactAdmissions::default(),
                exact_selected: false,
            },
            surfaces: process_default().0,
            app_surfaces: process_default().1,
            missed: held::Missed::default(),
            gathering: super::compaction_set::Gathering::default(),
            workspace_host: false,
            own_device: None,
            governance_now: None,
        })
    }
    /// Add a root-certified device to existing governed history, as a device
    /// of its owner or of a member the signed member list names and has not
    /// blocked ([`crate::PeerGovernanceMut::admit_member_device`]'s rule). Any
    /// member's device that may write governance records it: the person's
    /// root signed the certificate, which gives the device only what that
    /// person already holds.
    ///
    /// # Errors
    ///
    /// Returns an error if `key` is not a member's device, the certificate's
    /// person is not a member, the certificate is invalid, revoked or
    /// conflicts with a recorded one, or governance cannot be updated.
    pub fn admit_certificate(
        &mut self,
        cert: DeviceCert,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.require_member_writer(key, now)?;
        tilo_user_identity::verify_device_cert_at(&cert, now_ns(now)).map_err(err)?;
        if self.core.revocations.refuses(&cert) {
            return Err("device revoked".into());
        }
        if let Some(old) = self.core.certificates.get(&cert.device_public_key)
            && old.as_ref() != &cert
            && !supersedes(old.as_ref(), &cert)
        {
            return Err("certificate conflict".into());
        }
        let doc = self
            .core
            .workspace_doc
            .clone()
            .ok_or("workspace governance missing")?;
        if cert.user_public_key != self.core.owner
            && doc
                .workspace_policy(&self.core.workspace)
                .active_role(&hex::encode(cert.user_public_key))
                .is_none()
        {
            return Err("the device's person is not a member".into());
        }
        let id = hex::encode(cert.device_public_key);
        if workspace::governance_of(Some(&doc))
            .identities
            .get(&id)
            .and_then(|bytes| {
                tilo_failure::ignored!(
                    "an identity row that does not decode is not that certificate",
                    GovernedIdentity::decode(bytes).ok()
                )
            })
            .is_some_and(|old| old.device_cert == cert)
        {
            return Ok(false);
        }
        let identity = GovernedIdentity {
            device_cert: cert,
            revocations: vec![],
            admission: None,
            admissions: vec![],
            delegate: None,
            workspace_delegations: Vec::new(),
        };
        self.put_identity(doc, &identity, key, now)
    }
    /// Write `identity`'s governed row and roster name into `doc`, this
    /// workspace's governance, as the pinned authority `key`, and take the change
    /// in as every member will.
    pub(super) fn put_identity(
        &mut self,
        mut doc: CachedDocument,
        identity: &GovernedIdentity,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        let id = hex::encode(identity.device_cert.device_public_key);
        let encoded = identity.encode().map_err(err)?;
        let identities = doc
            .get(ROOT, "peer_identities_v1")
            .map_err(err)?
            .ok_or("governed identities missing")?
            .1;
        let names = doc
            .get(ROOT, "peers")
            .map_err(err)?
            .ok_or("governed peers missing")?
            .1;
        doc.set_actor(self.workspace_actor_of(key));
        let before = doc.get_heads();
        doc.put(&identities, &id, automerge::ScalarValue::Bytes(encoded))
            .map_err(err)?;
        // The roster names the person, never their device: a device's label stays in its
        // certificate, and a device new to the roster carries no name until its person gives one.
        if doc.get(&names, &id).map_err(err)?.is_none() {
            doc.put(&names, &id, "").map_err(err)?;
        }
        self.write_governance(doc, &before, key, now)
    }

    pub(super) fn set_member(
        &mut self,
        user_public_key: [u8; 32],
        role: Role,
        administrator: bool,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.put_member(user_public_key, role, administrator, true, None, key, now)
    }

    /// Hold `user_public_key` as a member at `role`, an administrator or not,
    /// whether or not one of their devices has been admitted here yet: a
    /// workspace's declaration that someone is in it — its owner declared by a
    /// deployment's operator before they ever join — is governance, so it is
    /// here before they arrive and stays after a restart. `name`, when the
    /// declarer carries one, is how the workspace names them from then on, as
    /// [`PeerGovernanceMut::name_person`] would: an operator declaring someone
    /// an owner knows the name the person goes by, and the workspace lists and
    /// invites them by it before any device of theirs has named them.
    pub(super) fn declare_member(
        &mut self,
        user_public_key: [u8; 32],
        role: Role,
        administrator: bool,
        name: Option<&str>,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.put_member(user_public_key, role, administrator, false, name, key, now)
    }

    fn put_member(
        &mut self,
        user_public_key: [u8; 32],
        role: Role,
        administrator: bool,
        admitted: bool,
        name: Option<&str>,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.require_member_writer(key, now)?;
        // A name the declaration carries is checked before anything is
        // written: a change that recorded the member and refused the name
        // would have to be asked for twice.
        let name = name
            .map(|name| {
                tilo_user_identity::display_name(name)
                    .map_err(tilo_user_identity::DisplayNameError::reason)
            })
            .transpose()?;
        if user_public_key == self.core.owner {
            return Err("owner role cannot be changed through member governance".into());
        }
        let doc = self
            .core
            .workspace_doc
            .clone()
            .ok_or("workspace governance missing")?;
        let person = hex::encode(user_public_key);
        let workspace = self.core.workspace.clone();
        let held = doc.workspace_policy(&workspace).clone();
        // A member is someone a device of theirs was admitted for, or whom
        // the workspace holds already: an owner it was handed from stays a
        // member, though their devices came in as the owner's.
        let policy = doc.policy();
        if admitted
            && !held.members.contains_key(&person)
            && !policy.identities.values().any(|bytes| {
                GovernedIdentity::decode(bytes).is_ok_and(|identity| {
                    identity.device_cert.user_public_key == user_public_key
                        && identity.has_admission()
                })
            })
        {
            return Err("member is absent from governed admission history".into());
        }
        let admin_id = administrator_grant_id(user_public_key);
        let moderation_id = tilo_action_policy::roles::administrator_moderation_grant_id(&person);
        let before_role = held.members.get(&person).copied();
        let before_admin = held.grants.iter().any(|grant| grant.id == admin_id);
        let mut changes = vec![match before_role {
            None => Change::AddMember {
                person: person.clone(),
                role,
            },
            Some(_) => Change::SetRole {
                person: person.clone(),
                role,
            },
        }];
        // An administrator moderates the workspace too: the two grants come and
        // go together.
        if administrator {
            // Whoever makes them an administrator gives the grants: the
            // owner's device or another administrator's, as the workspace's
            // policy decides for every device that replays the change.
            let giver = Grantee::person(hex::encode(
                self.certificate(&key.device_public_key(), now)
                    .map_err(|error| error.to_string())?
                    .user_public_key,
            ));
            for grant in [
                tilo_action_policy::roles::administrator_grant(&workspace, &person, giver.clone()),
                tilo_action_policy::roles::administrator_moderation_grant(
                    &workspace, &person, giver,
                ),
            ] {
                if !held.grants.contains(&grant) {
                    changes.push(Change::Grant(grant));
                }
            }
        } else {
            // The moderation grant goes first: the administrator grant is what
            // lets an administrator end their own moderation.
            for id in [moderation_id, admin_id] {
                if held.grants.iter().any(|grant| grant.id == id) {
                    changes.push(Change::Revoke(id));
                }
            }
        }
        if before_role == Some(role) && changes.len() == 1 && name.is_none() {
            return Ok(false);
        }
        if before_role == Some(role) {
            changes.remove(0);
        }
        let mut wrote = self.write_changes(doc, changes, key, now)? && {
            before_role != Some(role) || before_admin != administrator
        };
        // The name the declaration carries, written after the role: the person
        // is a member by root from the change above, which is what lets the
        // workspace name them before any device of theirs arrives.
        if let Some(name) = name {
            wrote |=
                PeerGovernanceMut { peer: self }.name_person(user_public_key, name, key, now)?;
        }
        Ok(wrote)
    }
}
