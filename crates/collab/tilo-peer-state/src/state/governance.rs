//! The membership governance this peer authors and reads: members, roles,
//! grants, delegations and revocations as the workspace document records them.
use super::facades::{PeerGovernance, PeerGovernanceMut};
use super::{
    BTreeMap, BTreeSet, DeviceCert, GovernedAccess, GovernedIdentity, GovernedMember, Grant,
    GrantId, MembershipError, ROOT, ReadDoc, RecordReaders, RevocationList, Role, SigningKey,
    Subject, SystemTime, Transactable, administrator_grant_id, err, governed_map,
    member_role_from_role, now_ns, workspace,
};

impl PeerGovernance<'_> {
    /// The sandbox capabilities the workspace's governance grants its apps.
    pub fn governed_sandbox(self) -> Result<Vec<collab_types::caps::CapOverride>, String> {
        let doc = self
            .peer
            .core
            .workspace_doc
            .as_ref()
            .ok_or("workspace governance missing")?;
        workspace::governance_of(Some(doc))
            .sandbox
            .values()
            .map(|bytes| serde_json::from_slice(bytes).map_err(err))
            .collect()
    }
    /// The standing of every governed member device at `now`: what a relay
    /// that does not hold the workspace's governance decides their changes
    /// with, each held to its foreign-device bound.
    pub fn governed_standings(
        self,
        now: SystemTime,
    ) -> Result<std::collections::BTreeMap<[u8; 32], tilo_action_policy::Standing>, String> {
        let access = self.peer.governance().governed_access(now)?;
        Ok(access
            .member_devices
            .into_iter()
            .filter_map(|device| {
                self.peer
                    .permissions(device, now)
                    .ok()
                    .map(|member| (device, member.standing()))
            })
            .collect())
    }
    /// Current governed access derived from the admitted workspace document.
    pub fn governed_access(self, now: SystemTime) -> Result<GovernedAccess, String> {
        let policy = self
            .peer
            .core
            .workspace_doc
            .as_ref()
            .ok_or("workspace governance missing")?
            .policy();
        let workspace_policy = policy.workspace_policy(&self.peer.core.workspace);
        let mut member_devices = Vec::new();
        let mut member_user_roots = Vec::new();
        for bytes in policy.identities.values() {
            let identity: GovernedIdentity = serde_json::from_slice(bytes).map_err(err)?;
            // An agent outside its delegation reads and writes nothing.
            if identity
                .delegate_acting(&self.peer.core.workspace, now_ns(now))
                .is_err()
            {
                continue;
            }
            let cert = identity.device_cert;
            if tilo_user_identity::verify_device_cert_at(&cert, now_ns(now)).is_err()
                || self.peer.core.revocations.refuses(&cert)
            {
                continue;
            }
            let person = hex::encode(cert.user_public_key);
            if workspace_policy.active_role(&person).is_some() {
                member_devices.push(cert.device_public_key);
                member_user_roots.push(cert.user_public_key);
            }
        }
        member_devices.sort_unstable();
        member_devices.dedup();
        member_user_roots.sort_unstable();
        member_user_roots.dedup();
        Ok(GovernedAccess {
            member_devices,
            member_user_roots,
        })
    }

    /// Whether `key`'s device may issue an invitation into the workspace at
    /// `now`: a current member whose part there grants inviting. What the
    /// surfaces that offer inviting show; an invitation's write path asks it
    /// again, with the part its link hands out.
    pub fn may_invite(self, key: &SigningKey, now: SystemTime) -> bool {
        self.peer.require_inviter(key, now).is_ok()
    }

    /// Whether `key`'s device may hand out `role` in an invitation into the
    /// workspace at `now`: what the surfaces offering a part to grant list.
    pub fn may_invite_role(self, key: &SigningKey, role: Role, now: SystemTime) -> bool {
        self.peer
            .require_invitation_authority(key, role, now)
            .is_ok()
    }

    /// The devices that may read `channel` at `now`: the projection of the
    /// workspace's membership a channel's MLS group follows. `channel` is the
    /// workspace's key scope ([`collab_types::workspace_scope`]) — read by
    /// every participant — or one unit's own channel, a record whose readers
    /// a Read Deny narrows. `route` is that record when the caller knows it
    /// ([`Self::withheld_record_readers`]). `None` when neither `route` nor a
    /// grant names the record: who may read it is then unknown, not no one.
    pub fn channel_readers(
        self,
        now: SystemTime,
        channel: collab_types::ChannelId,
        route: Option<&collab_types::ChannelRoute>,
    ) -> Result<Option<BTreeSet<[u8; 32]>>, String> {
        // The scope that carries the units whose readers are the workspace's
        // participants is read by exactly them: it is named by the workspace
        // itself, so every participant can decide it without knowing a record.
        if channel == collab_types::workspace_scope(&self.peer.core.workspace) {
            let access = self.peer.governance().governed_access(now)?;
            return Ok(Some(access.member_devices.into_iter().collect()));
        }
        let routes: Vec<collab_types::ChannelRoute> = route.cloned().into_iter().collect();
        let readers = self
            .peer
            .governance()
            .withheld_record_readers(now, &routes)?;
        let Some(withheld) = readers.withheld.get(&channel) else {
            return Ok(None);
        };
        Ok(Some(
            readers
                .readers
                .into_iter()
                .filter(|device| !withheld.contains(device))
                .collect(),
        ))
    }

    /// The key scope the storage unit on `route` seals under: the workspace's
    /// ([`collab_types::workspace_scope`]) while every device that may read
    /// the workspace may read the record too, the record's own channel when a
    /// Read Deny (or the record being a person's own) narrows its readers.
    /// The two reader sets never share a key: a narrowed record's group holds
    /// only the devices its own policy admits.
    pub fn unit_scope(
        self,
        now: SystemTime,
        route: &collab_types::ChannelRoute,
    ) -> Result<collab_types::ChannelId, String> {
        let readers = self
            .peer
            .governance()
            .withheld_record_readers(now, std::slice::from_ref(route))?;
        let unit =
            collab_types::document_channel_id(&route.app, &self.peer.core.workspace, &route.record);
        Ok(
            if readers
                .withheld
                .get(&unit)
                .is_some_and(|denied| !denied.is_empty())
            {
                unit
            } else {
                collab_types::workspace_scope(&self.peer.core.workspace)
            },
        )
    }

    /// The governed member devices a Read Deny keeps out of a record's
    /// channel, as every participant of the channel admits them.
    ///
    /// A channel is one record's, and its id a hash of the record's route,
    /// so a participant decides a channel by its record when it can name it: a
    /// record the workspace's grants name ([`collab_types::named_records`]), or one
    /// of `routes`, the routes devices proved to it
    /// ([`collab_types::ChannelRoute::proves`]). For each such record
    /// [`RecordReaders::withheld`] lists every device of
    /// [`Self::governed_access`] that may not read it
    /// ([`StatePeer::check_document_read`](crate::StatePeer::check_document_read), the decision every door applies), so a
    /// Deny on the record, on its app or on any pattern over it withholds its
    /// key, and the workspace's other records are unaffected.
    ///
    /// A channel whose record the caller cannot name may be any record, a
    /// member's own included, which only that person reads
    /// ([`collab_types::member_record`]); its id does not say whose. So
    /// [`RecordReaders::unrouted`] lists every member device: such a channel
    /// admits no one until a device names its record.
    pub fn withheld_record_readers(
        self,
        now: SystemTime,
        routes: &[collab_types::ChannelRoute],
    ) -> Result<RecordReaders, String> {
        let access = self.peer.governance().governed_access(now)?;
        let policy = self
            .peer
            .core
            .workspace_doc
            .as_ref()
            .ok_or("workspace governance missing")?
            .policy()
            .workspace_policy(&self.peer.core.workspace);
        let workspace = self.peer.core.workspace.as_str();
        let denied = |app: &str, record: &str| -> BTreeSet<[u8; 32]> {
            access
                .member_devices
                .iter()
                .copied()
                .filter(|device| {
                    self.peer
                        .check_document_read(*device, app, record, now)
                        .is_err()
                })
                .collect()
        };
        let mut records: BTreeSet<(&str, &str)> =
            collab_types::named_records(&policy.grants, workspace).collect();
        records.extend(
            routes
                .iter()
                .map(|route| (route.app.as_str(), route.record.as_str())),
        );
        let mut withheld = std::collections::HashMap::new();
        for (app, record) in records {
            // Only a grant below the workspace or a member's own record (read
            // by that person alone) can decide a record otherwise than the
            // workspace: a record neither covers is read by every member.
            let node = collab_types::document_node(workspace, app, record);
            let covered = collab_types::member_record(record).is_some()
                || policy
                    .grants
                    .iter()
                    .any(|grant| grant.node.len() > 1 && grant.node.covers(&node));
            let devices = if covered {
                denied(app, record)
            } else {
                BTreeSet::new()
            };
            withheld.insert(
                collab_types::document_channel_id(app, workspace, record),
                devices,
            );
        }
        // Every channel whose record the node cannot name may be any record, a
        // member's own included: no device is admitted to it until a device
        // names its record.
        let readers: BTreeSet<[u8; 32]> = access.member_devices.iter().copied().collect();
        let unrouted = readers.clone();
        Ok(RecordReaders {
            readers,
            withheld,
            unrouted,
        })
    }

    /// The workspace's people and grants as its governance records them: its
    /// owner, its members with the part each takes, when each was admitted,
    /// and the grants on its nodes. This is the one source every list of its
    /// people shows.
    pub fn governed_policy(self) -> Result<tilo_action_policy::WorkspacePolicy, String> {
        let doc = self
            .peer
            .core
            .workspace_doc
            .as_ref()
            .ok_or("workspace governance missing")?;
        Ok(doc.policy().workspace_policy(&self.peer.core.workspace))
    }
    /// Authority-authored collaborator projection. The structural owner is
    /// exposed separately by [`PeerGovernance::owner_user_public_key`].
    pub fn governed_members(self, now: SystemTime) -> Result<Vec<GovernedMember>, String> {
        let doc = self
            .peer
            .core
            .workspace_doc
            .as_ref()
            .ok_or("workspace governance missing")?;
        let policy = doc.policy();
        let workspace_policy = policy.workspace_policy(&self.peer.core.workspace);
        let names = workspace::governance_of(Some(doc)).peers;
        let mut rows: BTreeMap<[u8; 32], GovernedMember> = BTreeMap::new();
        for (id, bytes) in &policy.identities {
            let identity: GovernedIdentity = serde_json::from_slice(bytes).map_err(err)?;
            let Some(receipt) = identity
                .admission
                .as_ref()
                .or_else(|| identity.admissions.last())
            else {
                continue;
            };
            let root = identity.device_cert.user_public_key;
            let person = hex::encode(root);
            let role = workspace_policy.active_role(&person);
            let administrator = role == Some(Role::Owner)
                || workspace_policy
                    .grants
                    .iter()
                    .any(|grant| grant.id == administrator_grant_id(root));
            let active_role = if role == Some(Role::Owner) {
                Some(crate::join::MemberRole::Editor)
            } else {
                role.and_then(member_role_from_role)
            };
            let row = rows.entry(root).or_insert_with(|| GovernedMember {
                user_public_key: root,
                role: active_role.unwrap_or(receipt.role),
                display_name: names.get(id).cloned(),
                active_device_keys: Vec::new(),
                removed: active_role.is_none(),
                administrator,
            });
            if let Some(role) = active_role {
                row.role = role;
                row.removed = false;
                let cert = &identity.device_cert;
                if tilo_user_identity::verify_device_cert_at(cert, now_ns(now)).is_ok()
                    && !self.peer.core.revocations.refuses(&cert)
                {
                    row.active_device_keys.push(cert.device_public_key);
                }
            }
            if row.display_name.is_none() {
                row.display_name = names.get(id).cloned();
            }
        }
        let mut rows = rows.into_values().collect::<Vec<_>>();
        for row in &mut rows {
            row.active_device_keys.sort_unstable();
            row.active_device_keys.dedup();
        }
        Ok(rows)
    }
    pub fn owner_user_public_key(self) -> [u8; 32] {
        self.peer.core.owner
    }
    /// Issue a membership refresh challenge for a governed device.
    ///
    /// # Errors
    ///
    /// Returns an error if the device is not a current governed member at `now` or the
    /// challenge cannot be signed.
    pub fn membership_challenge(
        self,
        workspace_id: u64,
        host: &DeviceCert,
        device_cert: &DeviceCert,
        request_id: [u8; 32],
        nonce: [u8; 32],
        expires_at_ns: u64,
        now: SystemTime,
    ) -> Result<crate::join::MembershipChallenge, String> {
        // A renewed certificate is challenged as the device it renews; its
        // membership is decided once the governor holds it
        // ([`PeerGovernanceMut::renew_member_certificate`]).
        if self.renewal_of(device_cert, now)?.is_none() {
            self.peer
                .certificate(&device_cert.device_public_key, now)
                .map_err(|error| error.to_string())?;
            // A device the workspace admitted and then removed is refused as
            // removed, not as a stranger.
            match self.peer.permissions(device_cert.device_public_key, now) {
                Ok(_) => {}
                Err(MembershipError::NoMembership)
                    if self.peer.has_governed_admission(device_cert) =>
                {
                    return Err("member has been removed".into());
                }
                Err(error) => return Err(error.to_string()),
            }
        }
        crate::join::MembershipChallenge::new(
            &self.peer.core.workspace,
            workspace_id,
            host,
            device_cert,
            request_id,
            nonce,
            expires_at_ns,
            now_ns(now),
        )
    }
    /// The role a member's device that proved `membership` holds now, as the
    /// answer to it names it: the person's role in the workspace, which is
    /// what the device takes part as. A device of the workspace's owner holds
    /// the workspace as its owner, and an answer names the strongest
    /// collaborator role for it: a member's answer carries a role, never the
    /// owner's authority, which governance alone decides.
    pub fn member_role(
        self,
        membership: &crate::join::VerifiedMembership,
        now: SystemTime,
    ) -> Result<crate::join::MemberRole, String> {
        let role = self.peer.current_membership_role(membership, now)?;
        Ok(member_role_from_role(role).unwrap_or(crate::join::MemberRole::Moderator))
    }
}
impl PeerGovernanceMut<'_> {
    /// Publish root-signed revocations into pinned governance, so every
    /// replica refuses the revoked devices. The authority's root revokes any
    /// of its keys, kept on the authority's own row too so a device revoked
    /// before it was ever admitted stays refused; any other root revokes only
    /// a device the workspace certifies to it. Returns whether governance changed.
    ///
    /// # Errors
    ///
    /// Returns an error if the change is not `key`'s device's to make, an entry's root
    /// may not revoke its device here, or governance cannot be updated.
    pub fn publish_governed_revocations(
        self,
        entries: &[tilo_user_identity::RevocationEntry],
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        let owner = self.peer.core.owner;
        let mut doc = self
            .peer
            .core
            .workspace_doc
            .clone()
            .ok_or("workspace governance missing")?;
        let rows = super::governed_rows::identity_rows(&doc).map_err(err)?;
        let mut verified = self.peer.core.revocations.clone();
        for entry in entries {
            let certified = rows.iter().any(|(_, _, identity)| {
                identity.device_cert.device_public_key == entry.revoked_device_public_key
                    && identity.device_cert.user_public_key == entry.user_public_key
            });
            if entry.user_public_key != owner && !certified {
                return Err("governed revocation root mismatch".into());
            }
            verified.insert(entry.clone()).map_err(err)?;
        }
        let before = doc.get_heads();
        doc.set_actor(self.peer.workspace_actor_of(key));
        let mut projected = rows;
        for (_, _, identity) in &mut projected {
            let root = identity.device_cert.user_public_key;
            let own_row = root == owner;
            let mut list = RevocationList::new();
            for entry in &identity.revocations {
                list.insert(entry.clone()).map_err(err)?;
            }
            for entry in verified
                .iter()
                .filter(|entry| entry.user_public_key == root)
            {
                if own_row
                    || entry.revoked_device_public_key == identity.device_cert.device_public_key
                {
                    list.insert(entry.clone()).map_err(err)?;
                }
            }
            let mut written = list.iter().cloned().collect::<Vec<_>>();
            written.sort_by_key(|entry| entry.revoked_device_public_key);
            identity.revocations = written;
        }
        let changed = super::governed_rows::put_identity_rows(&mut doc, &projected).map_err(err)?;
        if changed {
            doc.commit();
            self.peer.write_governance(doc, &before, key, now)?;
        } else {
            self.peer.core.revocations = verified;
        }
        Ok(changed)
    }
    /// Atomically records a verified single-use invitation redemption, its
    /// independent root-certified device, and its workspace role, written by
    /// `key`: the device of any member who may invite. The invitation is
    /// checked on its own ([`crate::workspace::membership::invitation_admits`]),
    /// as every receiver checks it.
    ///
    /// # Errors
    ///
    /// Returns an error if `key` is not a member's device that may invite,
    /// the invitation does not admit the redemption, it conflicts with
    /// recorded governance, or governance cannot be updated.
    pub fn admit_invited_member(
        self,
        redemption: &crate::join::VerifiedRedemption,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        if redemption.join().grant().room != self.peer.core.workspace {
            return Err("invitation is not for this workspace".into());
        }
        self.peer.require_inviter(key, now)?;
        let cert = redemption.certificate();
        if self.peer.core.revocations.refuses(&cert) {
            return Err("device revoked".into());
        }
        if let Some(old) = self.peer.core.certificates.get(&cert.device_public_key)
            && old.as_ref() != cert
            && !super::supersedes(old.as_ref(), cert)
        {
            return Err("certificate conflict".into());
        }
        let cached = self
            .peer
            .core
            .workspace_doc
            .as_ref()
            .ok_or("workspace governance missing")?;
        let mut document = (**cached).clone();
        document.set_actor(self.peer.workspace_actor_of(key));
        let before = document.get_heads();
        match crate::governance::admit_invited_member(&mut document, redemption, now)? {
            crate::governance::AdmissionOutcome::AlreadyApplied => Ok(false),
            crate::governance::AdmissionOutcome::Applied { .. } => {
                self.peer
                    .write_governance(document.into(), &before, key, now)
            }
        }
    }
    /// Admit `device`, a device certified by the root of someone already a
    /// member, as that member's: it needs no invitation, and any member's
    /// device `key` writes it in when it first asks. Whether the workspace changed.
    ///
    /// # Errors
    ///
    /// Returns an error if `key` is not a current member's device, `device`
    /// does not verify, is revoked or conflicts with the workspace's, its person
    /// is not a member, or governance cannot be updated.
    pub fn admit_member_device(
        self,
        device: &DeviceCert,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.peer.require_member_writer(key, now)?;
        tilo_user_identity::verify_device_cert_at(device, now_ns(now)).map_err(err)?;
        if self.peer.core.revocations.refuses(&device) {
            return Err("device revoked".into());
        }
        match self.peer.core.certificates.get(&device.device_public_key) {
            Some(old) if old.as_ref() == device => return Ok(false),
            Some(_) => return Err("certificate conflict".into()),
            None => {}
        }
        let mut doc = self
            .peer
            .core
            .workspace_doc
            .clone()
            .ok_or("workspace governance missing")?;
        let person = hex::encode(device.user_public_key);
        let owner = self.peer.core.owner == device.user_public_key;
        if !owner
            && doc
                .policy()
                .workspace_policy(&self.peer.core.workspace)
                .active_role(&person)
                .is_none()
        {
            return Err("the device's person is not a member".into());
        }
        let identities = doc
            .get(ROOT, "peer_identities_v1")
            .map_err(err)?
            .ok_or("governed identities missing")?
            .1;
        let identity = GovernedIdentity {
            device_cert: device.clone(),
            revocations: Vec::new(),
            admission: None,
            admissions: Vec::new(),
            delegate: None,
            workspace_delegations: Vec::new(),
        };
        let before = doc.get_heads();
        doc.set_actor(self.peer.workspace_actor_of(key));
        doc.put(
            &identities,
            hex::encode(device.device_public_key),
            automerge::ScalarValue::Bytes(serde_json::to_vec(&identity).map_err(err)?),
        )
        .map_err(err)?;
        doc.commit();
        self.peer.write_governance(doc, &before, key, now)
    }
    /// Take out of the workspace's governance every device row whose
    /// certificate has expired: a device whose certificate no longer verifies
    /// is no member device any more ([`Self::governed_access`] leaves it out),
    /// so the row it left keeps nobody reading. A page's device certificate
    /// lasts an hour and a page opens a fresh one on every load, so this is
    /// what keeps a workspace that many people visit from keeping a row per
    /// visit. Rows that record an admission, a delegation or a revocation
    /// stay: a person's member devices are the workspace's for as long as the
    /// person is. Whether the workspace changed.
    ///
    /// # Errors
    ///
    /// Returns an error if the change is not `key`'s device's to make, a row does not
    /// read back, or governance cannot be updated.
    pub fn forget_expired_devices(self, key: &SigningKey, now: SystemTime) -> Result<bool, String> {
        let Some(doc) = self
            .peer
            .core
            .workspace_doc
            .as_ref()
            .map(crate::cached_document::CachedDocument::policy)
        else {
            return Ok(false);
        };
        let expired: Vec<[u8; 32]> = doc
            .identities
            .values()
            .map(|bytes| serde_json::from_slice::<GovernedIdentity>(bytes).map_err(err))
            .collect::<Result<Vec<_>, String>>()?
            .into_iter()
            .filter(|row| row.revocations.is_empty())
            .filter(|row| row.admission.is_none() && row.admissions.is_empty())
            .filter(|row| row.delegate.is_none())
            .filter(|row| {
                row.device_cert
                    .expires_at_ns
                    .is_some_and(|expiry| expiry <= now_ns(now))
            })
            .map(|row| row.device_cert.device_public_key)
            .collect();
        if expired.is_empty() {
            return Ok(false);
        }
        self.forget_identity_rows(&expired, key, now)
    }

    /// Take `devices`' rows out of the workspace's governance as the governing
    /// `key`, in one change, and take it in as every member will. Whether the
    /// workspace changed.
    fn forget_identity_rows(
        self,
        devices: &[[u8; 32]],
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        let mut doc = self
            .peer
            .core
            .workspace_doc
            .clone()
            .ok_or("workspace governance missing")?;
        let identities = doc
            .get(ROOT, "peer_identities_v1")
            .map_err(err)?
            .ok_or("governed identities missing")?
            .1;
        let before = doc.get_heads();
        doc.set_actor(self.peer.workspace_actor_of(key));
        for device in devices {
            doc.delete(&identities, hex::encode(device)).map_err(err)?;
        }
        doc.commit();
        self.peer.write_governance(doc, &before, key, now)
    }

    /// Change a governed collaborator's workspace role.
    ///
    /// # Errors
    ///
    /// Returns an error if the change is not `key`'s device's to make, the user is the owner or
    /// absent from governed admission history, or governance cannot be updated.
    pub fn set_member_role(
        self,
        user_public_key: [u8; 32],
        role: crate::join::MemberRole,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.peer
            .set_member(user_public_key, role.into(), false, key, now)
    }
    /// Make a governed collaborator an administrator of the workspace: an Editor
    /// holding every governance action
    /// ([`tilo_action_policy::roles::administrator_grant`]).
    ///
    /// # Errors
    ///
    /// As [`Self::set_member_role`].
    pub fn set_member_administrator(
        self,
        user_public_key: [u8; 32],
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.peer
            .set_member(user_public_key, Role::Editor, true, key, now)
    }
    /// Name the admitted person `user_public_key` `name` in the workspace: the
    /// name every reader of [`PeerGovernance::governed_members`] shows for
    /// them, on each of their admitted devices' rows. A device's own label
    /// stays on its certificate. Whether the workspace changed: naming someone
    /// what they are already called writes nothing.
    ///
    /// # Errors
    ///
    /// Returns an error if the change is not `key`'s device's to make, `name` is not
    /// a display name ([`tilo_user_identity::display_name`]), the person has
    /// no admitted device here, or governance cannot be updated.
    pub fn name_person(
        self,
        user_public_key: [u8; 32],
        name: &str,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        let name = tilo_user_identity::display_name(name)
            .map_err(tilo_user_identity::DisplayNameError::reason)?;
        let mut doc = self
            .peer
            .core
            .workspace_doc
            .clone()
            .ok_or("workspace governance missing")?;
        let policy = doc.policy();
        let names = workspace::governance_of(Some(&doc)).peers;
        let mut devices = Vec::new();
        for (id, bytes) in &policy.identities {
            let identity: GovernedIdentity = serde_json::from_slice(bytes).map_err(err)?;
            if identity.device_cert.user_public_key == user_public_key && identity.has_admission() {
                devices.push(id.clone());
            }
        }
        if devices.is_empty() {
            return Err("the person is not admitted to the workspace".into());
        }
        devices.retain(|id| names.get(id).map(String::as_str) != Some(name));
        if devices.is_empty() {
            return Ok(false);
        }
        let peers = doc
            .get(ROOT, "peers")
            .map_err(err)?
            .ok_or("governed peers missing")?
            .1;
        let before = doc.get_heads();
        doc.set_actor(self.peer.workspace_actor_of(key));
        for id in &devices {
            doc.put(&peers, id, name).map_err(err)?;
        }
        doc.commit();
        self.peer.write_governance(doc, &before, key, now)
    }
    /// Rewrite each administrator's grant still in the shape workspaces held
    /// before [`tilo_action_policy::roles::administrator_grant`] (on the
    /// workspace's resource, where the Editor's own Deny outranked it) to that
    /// grant, keeping its id and giver. A device runs it before it publishes
    /// governance, so a workspace's next governance change carries its
    /// administrators' authority. Whether the workspace changed.
    ///
    /// # Errors
    ///
    /// Returns an error if a grant needs rewriting and `key`'s device may not
    /// change it, or governance cannot be updated.
    pub fn reconcile_administrator_grants(
        self,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        let Some(doc) = self.peer.core.workspace_doc.as_ref() else {
            return Ok(false);
        };
        let workspace = self.peer.core.workspace.as_str();
        let policy = doc.policy();
        let grants = &policy.grants;
        let mut rewritten = grants
            .iter()
            .filter(|grant| {
                tilo_action_policy::roles::is_legacy_administrator_grant(workspace, grant)
            })
            .filter_map(|grant| match &grant.subject {
                Subject::Person(person) => Some(tilo_action_policy::roles::administrator_grant(
                    workspace,
                    person,
                    grant.granted_by.clone(),
                )),
                _ => None,
            })
            .collect::<Vec<_>>();
        // Every administrator moderates the workspace too.
        for grant in grants {
            if let Subject::Person(person) = &grant.subject
                && grant.id == tilo_action_policy::roles::administrator_grant_id(person)
                && grant.answer == tilo_action_policy::Answer::Allow
            {
                let moderation_id =
                    tilo_action_policy::roles::administrator_moderation_grant_id(person);
                if !grants.iter().any(|held| held.id == moderation_id) {
                    rewritten.push(tilo_action_policy::roles::administrator_moderation_grant(
                        workspace,
                        person,
                        grant.granted_by.clone(),
                    ));
                }
            }
        }
        if rewritten.is_empty() {
            return Ok(false);
        }
        self.peer
            .governance_mut()
            .put_governed_grants(&rewritten, key, now)
    }
    /// Remove grants a member's device decided to take back, by id.
    /// Memberships are removed through member governance.
    ///
    /// # Errors
    ///
    /// Returns an error if the change is not `key`'s device's to make or governance
    /// cannot be updated.
    pub fn remove_governed_grants(
        self,
        ids: &[GrantId],
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.peer.require_member_writer(key, now)?;
        let mut doc = self
            .peer
            .core
            .workspace_doc
            .clone()
            .ok_or("workspace governance missing")?;
        let before = doc.get_heads();
        let grants = governed_map(&doc, "grants")?;
        doc.set_actor(self.peer.workspace_actor_of(key));
        for id in ids {
            if doc.get(&grants, id.as_str()).map_err(err)?.is_some() {
                doc.delete(&grants, id.as_str()).map_err(err)?;
            }
        }
        doc.commit();
        self.peer.write_governance(doc, &before, key, now)
    }
    /// Write grants a member's device decided, such as those an assigned
    /// grant set expands to, keyed by their ids.
    ///
    /// # Errors
    ///
    /// Returns an error if the change is not `key`'s device's to make, a grant has no
    /// id or names another workspace, or governance cannot be updated.
    pub fn put_governed_grants(
        self,
        grants: &[Grant],
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.peer.require_member_writer(key, now)?;
        let mut doc = self
            .peer
            .core
            .workspace_doc
            .clone()
            .ok_or("workspace governance missing")?;
        let before = doc.get_heads();
        let map = governed_map(&doc, "grants")?;
        doc.set_actor(self.peer.workspace_actor_of(key));
        for grant in grants {
            if grant.id.as_str().trim().is_empty()
                || grant.node.workspace_id() != Some(self.peer.core.workspace.as_str())
            {
                return Err(format!(
                    "grant {:?} cannot be written as workspace policy",
                    grant.id
                ));
            }
            doc.put(
                &map,
                grant.id.as_str(),
                automerge::ScalarValue::Bytes(serde_json::to_vec(grant).map_err(err)?),
            )
            .map_err(err)?;
        }
        doc.commit();
        self.peer.write_governance(doc, &before, key, now)
    }
    /// Remove a governed collaborator and their grants; someone removing
    /// themselves also takes their own devices' rows.
    ///
    /// # Errors
    ///
    /// Returns an error if the change is not `key`'s device's to make, the user is the structural
    /// owner or not a member, or governance cannot be updated.
    pub fn remove_member(
        self,
        user_public_key: [u8; 32],
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.peer.require_member_writer(key, now)?;
        if user_public_key == self.peer.core.owner {
            return Err("workspace owner cannot be removed".into());
        }
        let mut doc = self
            .peer
            .core
            .workspace_doc
            .clone()
            .ok_or("workspace governance missing")?;
        let person = hex::encode(user_public_key);
        let members = governed_map(&doc, "members")?;
        let grants = governed_map(&doc, "grants")?;
        let personal = doc
            .policy()
            .grants
            .iter()
            .filter(|grant| grant.subject == Subject::Person(person.clone()))
            .map(|grant| grant.id.as_str().to_owned())
            .collect::<Vec<_>>();
        let member = doc.get(&members, person.as_str()).map_err(err)?.is_some();
        if !member && personal.is_empty() {
            return Ok(false);
        }
        let before = doc.get_heads();
        doc.set_actor(self.peer.workspace_actor_of(key));
        if member {
            doc.delete(&members, person.as_str()).map_err(err)?;
            crate::state::delete_member_joined(&mut doc, &person)?;
        }
        for id in personal {
            doc.delete(&grants, id.as_str()).map_err(err)?;
        }
        // Someone who leaves takes their own devices off the workspace with
        // them: the rows that admit a device go, so no key of theirs stays
        // named here. Another person's devices stay until whoever may remove
        // them takes them.
        if self
            .peer
            .certificate(&key.verifying_key().to_bytes(), now)
            .is_ok_and(|cert| cert.user_public_key == user_public_key)
        {
            let identities = governed_map(&doc, "peer_identities_v1")?;
            let own: Vec<String> = crate::workspace::governance_of(Some(&doc))
                .identities
                .into_iter()
                .filter(|(_, bytes)| {
                    serde_json::from_slice::<super::GovernedIdentity>(bytes).is_ok_and(|identity| {
                        identity.device_cert.user_public_key == user_public_key
                    })
                })
                .map(|(device, _)| device)
                .collect();
            for device in own {
                doc.delete(&identities, device.as_str()).map_err(err)?;
            }
        }
        doc.commit();
        self.peer.write_governance(doc, &before, key, now)
    }
}

impl super::StatePeer {
    /// The current member writing with `key`, as its own device's authority
    /// holds it: what inviting is decided by.
    fn inviter(&self, key: &SigningKey, now: SystemTime) -> Result<super::MemberAuthority, String> {
        let device = key.verifying_key().to_bytes();
        self.require_member_writer(key, now)?;
        self.membership(device, now)
    }

    /// A current member's device whose person may invite: what an
    /// admission through an invitation is written by.
    fn require_inviter(&self, key: &SigningKey, now: SystemTime) -> Result<(), String> {
        let member = self.inviter(key, now)?;
        let bound = tilo_action_policy::roles::foreign_device_bound();
        member
            .policy
            .check_action(
                &member.authority(&bound),
                tilo_action_policy::roles::governance::INVITE,
                &tilo_action_policy::Context::at_time(now),
                false,
            )
            .map_err(|refusal| format!("the writer may not invite: {refusal}"))
    }

    /// A current member's device whose person may invite, and may hand out
    /// `role` with what it makes: checked where an invitation is written, so
    /// no surface issues a link its person could not redeem itself.
    pub(crate) fn require_invitation_authority(
        &self,
        key: &SigningKey,
        role: Role,
        now: SystemTime,
    ) -> Result<(), String> {
        let member = self.inviter(key, now)?;
        let bound = tilo_action_policy::roles::foreign_device_bound();
        member
            .policy
            .check_invite(
                &member.person,
                &member.authority(&bound),
                role,
                &tilo_action_policy::Context::at_time(now),
                false,
            )
            .map_err(|refusal| format!("the writer may not invite: {refusal}"))
    }
}
