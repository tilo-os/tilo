//! Building a replica, and admitting a device into its governed history.

use super::{
    BTreeMap, CachedDocument, DeviceCert, ExactAdmissions, GovernedIdentity, PeerCore, ROOT,
    RevocationList, Role, Shared, StatePeer, Subject, SystemTime, VecDeque, VerifiedCertificate,
    administrator_grant_id, err, governed_map, held, now_ns, process_default, put_member,
    put_member_joined_once, supersedes, workspace,
};
use collab_types::signer::DeviceSigner;

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

    /// A replica of a workspace joined through `admitter`, a device of the workspace's
    /// owner, which must be valid at `now`, and
    /// the workspace's `roster` as its governance admitted it. A roster
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
                documents: Shared::default(),
                pending: VecDeque::new(),
                pending_bytes: 0,
                exact_admissions: ExactAdmissions::default(),
                admission_v5: false,
                native_cut_publisher: None,
            },
            surfaces: process_default().0,
            app_surfaces: process_default().1,
            missed: held::Missed::default(),
            gathering: super::compaction_set::Gathering::default(),
            workspace_host: false,
            brought_up: false,
            durable_digests: super::durable::PartDigests::default(),
        })
    }
    /// Add a root-certified viewer to existing governed history, as a member
    /// device of this workspace.
    ///
    /// # Errors
    ///
    /// Returns an error if `key` is not a member's device, the certificate is invalid,
    /// revoked or conflicts with a recorded one, or governance cannot be updated.
    pub fn admit_certificate(
        &mut self,
        cert: DeviceCert,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.require_member_writer(key, now)?;
        if cert.user_public_key != self.core.owner {
            return Err("enrollment root mismatch".into());
        }
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
        let id = hex::encode(cert.device_public_key);
        if workspace::governance_of(Some(&doc))
            .identities
            .get(&id)
            // ignored-failure: an identity row that does not decode is not that certificate
            .and_then(|bytes| GovernedIdentity::decode(bytes).ok())
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
        doc.put(&names, &id, identity.device_cert.device_label.as_str())
            .map_err(err)?;
        doc.commit();
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
        self.require_member_writer(key, now)?;
        if user_public_key == self.core.owner {
            return Err("owner role cannot be changed through member governance".into());
        }
        let mut doc = self
            .core
            .workspace_doc
            .clone()
            .ok_or("workspace governance missing")?;
        let policy = doc.policy();
        if !policy.identities.values().any(|bytes| {
            GovernedIdentity::decode(bytes).is_ok_and(|identity| {
                identity.device_cert.user_public_key == user_public_key && identity.has_admission()
            })
        }) {
            return Err("member is absent from governed admission history".into());
        }
        let person = hex::encode(user_public_key);
        let admin_id = administrator_grant_id(user_public_key);
        let before_role = policy.members.get(&person).copied();
        let before_admin = policy.grants.iter().any(|grant| grant.id == admin_id);
        let before = doc.get_heads();
        let members = governed_map(&doc, "members")?;
        let grants = governed_map(&doc, "grants")?;
        doc.set_actor(self.workspace_actor_of(key));
        put_member(&mut doc, &members, user_public_key, role)?;
        // Their admission time is recorded beside the role, once: it is part of
        // the membership an operation reproduces, not a row only some writers
        // remember.
        put_member_joined_once(&mut doc, user_public_key, now_ns(now) / 1_000_000)?;
        // An administrator moderates the workspace too: the two grants come and
        // go together.
        let moderation_id = tilo_action_policy::roles::administrator_moderation_grant_id(&person);
        if administrator {
            let giver = Subject::Person(hex::encode(self.core.owner));
            let workspace = self.core.workspace.clone();
            for grant in [
                tilo_action_policy::roles::administrator_grant(&workspace, &person, giver.clone()),
                tilo_action_policy::roles::administrator_moderation_grant(
                    &workspace, &person, giver,
                ),
            ] {
                doc.put(
                    &grants,
                    grant.id.as_str(),
                    automerge::ScalarValue::Bytes(serde_json::to_vec(&grant).map_err(err)?),
                )
                .map_err(err)?;
            }
        } else {
            for id in [admin_id.as_str(), moderation_id.as_str()] {
                if doc.get(&grants, id).map_err(err)?.is_some() {
                    doc.delete(&grants, id).map_err(err)?;
                }
            }
        }
        doc.commit();
        let changed = self.write_governance(doc, &before, key, now)?;
        Ok(changed && (before_role != Some(role) || before_admin != administrator))
    }
}
