//! Building a replica, and admitting a device into its history.

use super::{
    BTreeMap, CachedDocument, DeviceCert, DeviceRow, ExactAdmissions, Grantee, PeerCore,
    PeerGovernanceMut, ROOT, RevocationList, Role, Shared, StatePeer, SystemTime, VecDeque,
    VerifiedCertificate, administrator_grant_id, err, held, now_ns, process_default,
};
use collab_types::signer::DeviceSigner;
use tilo_action_policy::{Change, RoleAssignment};

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

    /// A replica of a workspace joined through `admitter`, a holder of the
    /// workspace, which must be valid at `now`, and the workspace's `roster`
    /// as its governance admitted it. The admitter is the replica's origin and
    /// nothing more: the replica names no owner until the workspace's
    /// governance arrives and names the owner it holds to
    /// ([`Self::from_join`]), so the device that answered the join is given
    /// no say over the workspace by answering it. A roster
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
        let mut peer = Self::with_roster(workspace, admitter, roster, revocations, now_ns(now))?;
        peer.core.owner = [0; 32];
        Ok(peer)
    }

    /// Refuse, as a device joining through `admitter` at `now` (nanoseconds) would refuse it
    /// building its replica from what this replica holds ([`Self::joined`]), a workspace whose
    /// admitter is not valid then or is revoked here: the host that admits the device asks
    /// this before it commits the admission, so a join the device would refuse never uses up
    /// its invitation. The roster is the host's own, already verified, so it is not checked
    /// again here.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn joinable_through(&self, admitter: &DeviceCert, now: u64) -> Result<(), String> {
        valid_origin(admitter.clone(), &self.core.revocations, now).map(drop)
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
        let origin_certificate = valid_origin(owner_device, &revocations, now)?;
        // A revoked device of the roster is left out of it, and `revocations`, which the
        // replica keeps, refuses it everywhere: one device a workspace ever revoked refused
        // the whole roster, so nobody could join that workspace again.
        let mut certificates = roster
            .into_iter()
            .filter(|certificate| !revocations.refuses(certificate))
            .map(|certificate| (certificate.device_public_key, certificate))
            .collect::<BTreeMap<_, _>>();
        certificates.insert(origin_certificate.device_public_key, origin_certificate);
        Ok(Self {
            core: PeerCore {
                workspace,
                owner,
                origin,
                certificates: certificates.into(),
                placed_devices: Shared::default(),
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
            copied: std::collections::BTreeMap::new(),
            action_stamp: None,
        })
    }
    /// Write `identity`'s member row and roster name into `doc`, this
    /// workspace's governance, as the pinned authority `key`, and take the change
    /// in as every member will.
    pub(super) fn put_identity(
        &mut self,
        mut doc: CachedDocument,
        identity: &DeviceRow,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        let id = hex::encode(identity.device_cert.device_public_key);
        let encoded = identity.encode().map_err(err)?;
        let identities = doc
            .get(ROOT, "peer_identities_v1")
            .map_err(err)?
            .ok_or("member identities missing")?
            .1;
        let names = doc
            .get(ROOT, "peers")
            .map_err(err)?
            .ok_or("peer names missing")?
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
                DeviceRow::decode(bytes).is_ok_and(|identity| {
                    identity.device_cert.user_public_key == user_public_key
                        && identity.has_admission()
                })
            })
        {
            return Err("member is absent from the admission history".into());
        }
        let admin_id = administrator_grant_id(user_public_key);
        let moderation_id = tilo_action_policy::roles::administrator_moderation_grant_id(&person);
        let before_role = held.members.get(&person).and_then(RoleAssignment::whole);
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
        // Holding someone new is adding a member; anything else of theirs this changes, their
        // role or whether they administer the workspace, is changing their role.
        let verb = match before_role {
            None => tilo_action_policy::roles::governance::ADD_MEMBER,
            Some(_) => tilo_action_policy::roles::governance::SET_ROLE,
        };
        let stamp = self.own_action(verb, key, now);
        let mut wrote = self.naming(stamp, |peer| peer.write_changes(doc, changes, key, now))? && {
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

/// The certificate of `device`, the origin of a replica built at `now`: valid then, and not
/// revoked by `revocations`.
fn valid_origin(
    device: DeviceCert,
    revocations: &RevocationList,
    now: u64,
) -> Result<VerifiedCertificate, String> {
    let certificate = VerifiedCertificate::new(device, now)?;
    if revocations.refuses(&certificate) {
        return Err("device revoked".into());
    }
    Ok(certificate)
}
