//! The membership governance this peer authors and reads: members, roles,
//! grants, delegations and revocations as the room document records them.
use super::facades::{PeerGovernance, PeerGovernanceMut};
use super::{
    BTreeMap, BTreeSet, DeviceCert, GovernedAccess, GovernedIdentity, GovernedMember, Grant,
    GrantId, NeighbourMsg, ROOT, ReadDoc, RecordReaders, RevocationList, Role, SigningKey, Subject,
    MembershipError, SystemTime, Transactable, TransportOrigin, administrator_grant_id, err,
    governed_map,
    member_role_from_role, now_ns, room,
};

impl PeerGovernance<'_> {
    /// The sandbox capabilities the room's governance grants its apps.
    pub fn governed_sandbox(self) -> Result<Vec<collab_types::caps::CapOverride>, String> {
        let doc = self
            .peer
            .core
            .room_doc
            .as_ref()
            .ok_or("room governance missing")?;
        room::governance_of(Some(doc))
            .sandbox
            .values()
            .map(|bytes| serde_json::from_slice(bytes).map_err(err))
            .collect()
    }
    /// The standing of every governed member device at `now`: what a relay
    /// that does not hold the room's governance decides their changes
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
    /// Current governed access derived from the admitted room document.
    pub fn governed_access(self, now: SystemTime) -> Result<GovernedAccess, String> {
        let policy = self
            .peer
            .core
            .room_doc
            .as_ref()
            .ok_or("room governance missing")?
            .policy();
        let room_policy = policy.room_policy(&self.peer.core.room);
        let mut member_devices = Vec::new();
        let mut member_user_roots = Vec::new();
        for bytes in policy.identities.values() {
            let identity: GovernedIdentity = serde_json::from_slice(bytes).map_err(err)?;
            // An agent outside its delegation reads and writes nothing.
            if identity
                .delegate_acting(&self.peer.core.room, now_ns(now))
                .is_err()
            {
                continue;
            }
            let cert = identity.device_cert;
            if tilo_user_identity::verify_device_cert_at(&cert, now_ns(now)).is_err()
                || self
                    .peer
                    .core
                    .revocations
                    .is_revoked(&cert.device_public_key)
            {
                continue;
            }
            let person = hex::encode(cert.user_public_key);
            if room_policy.active_role(&person).is_some() {
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

    /// The member devices that may read `channel`, one of the room's record
    /// channels, at `now`: the projection of the room's membership a
    /// channel's MLS group follows. `route` is the channel's record when the
    /// caller knows it ([`Self::withheld_record_readers`]). `None` when
    /// neither `route` nor a grant names the record: who may read it is then
    /// unknown, not no one.
    ///
    /// # Errors
    ///
    /// The room's governance is missing or cannot be read.
    pub fn channel_readers(
        self,
        now: SystemTime,
        channel: collab_types::ChannelId,
        route: Option<&collab_types::ChannelRoute>,
    ) -> Result<Option<BTreeSet<[u8; 32]>>, String> {
        let access = self.peer.governance().governed_access(now)?;
        let routes: Vec<collab_types::ChannelRoute> = route.cloned().into_iter().collect();
        let readers = self
            .peer
            .governance()
            .withheld_record_readers(now, &routes)?;
        let Some(withheld) = readers.withheld.get(&channel) else {
            return Ok(None);
        };
        Ok(Some(
            access
                .member_devices
                .into_iter()
                .filter(|device| !withheld.contains(device))
                .collect(),
        ))
    }

    /// The governed member devices a Read Deny keeps out of a record's
    /// channel, as every participant of the channel admits them.
    ///
    /// A channel is one record's, and its id a hash of the record's route,
    /// so a participant decides a channel by its record when it can name it: a
    /// record the room's grants name ([`collab_types::named_records`]), or one
    /// of `routes`, the routes devices proved to it
    /// ([`collab_types::ChannelRoute::proves`]). For each such record
    /// [`RecordReaders::withheld`] lists every device of
    /// [`Self::governed_access`] that may not read it
    /// ([`StatePeer::check_document_read`](crate::StatePeer::check_document_read), the decision every door applies), so a
    /// Deny on the record, on its app or on any pattern over it withholds its
    /// key, and the room's other records are unaffected.
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
            .room_doc
            .as_ref()
            .ok_or("room governance missing")?
            .policy()
            .room_policy(&self.peer.core.room);
        let room = self.peer.core.room.as_str();
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
            collab_types::named_records(&policy.grants, room).collect();
        records.extend(
            routes
                .iter()
                .map(|route| (route.app.as_str(), route.record.as_str())),
        );
        let mut withheld = std::collections::HashMap::new();
        for (app, record) in records {
            // Only a grant below the room, or a member's own record (read by
            // that person alone), can decide a record otherwise than the
            // room: a record neither covers is read by every member.
            let node = collab_types::document_node(room, app, record);
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
                collab_types::document_channel_id(app, room, record),
                devices,
            );
        }
        let unrouted = access.member_devices.iter().copied().collect();
        Ok(RecordReaders { withheld, unrouted })
    }
    /// Authority-authored collaborator projection. The structural owner is
    /// exposed separately by [`PeerGovernance::owner_user_public_key`].
    pub fn governed_members(self, now: SystemTime) -> Result<Vec<GovernedMember>, String> {
        let doc = self
            .peer
            .core
            .room_doc
            .as_ref()
            .ok_or("room governance missing")?;
        let policy = doc.policy();
        let room_policy = policy.room_policy(&self.peer.core.room);
        let names = room::governance_of(Some(doc)).peers;
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
            let role = room_policy.active_role(&person);
            let administrator = role == Some(Role::Owner)
                || room_policy
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
                    && !self
                        .peer
                        .core
                        .revocations
                        .is_revoked(&cert.device_public_key)
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
            // A device the room admitted and then removed is refused as
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
            &self.peer.core.room,
            workspace_id,
            host,
            device_cert,
            request_id,
            nonce,
            expires_at_ns,
            now_ns(now),
        )
    }
    /// Check a verified membership refresh against current governance.
    pub fn verify_member_refresh(
        self,
        membership: &crate::join::VerifiedMembership,
        now: SystemTime,
    ) -> Result<crate::join::MemberRole, String> {
        let role = self.peer.current_membership_role(membership, now)?;
        member_role_from_role(role)
            .ok_or_else(|| "structural owner does not use collaborator refresh".into())
    }
}
impl PeerGovernanceMut<'_> {
    /// Publish root-signed revocations into pinned governance, so every
    /// replica refuses the revoked devices. The authority's root revokes any
    /// of its keys, kept on the authority's own row too so a device revoked
    /// before it was ever admitted stays refused; any other root revokes only
    /// a device the room certifies to it. Returns whether governance changed.
    ///
    /// # Errors
    ///
    /// Returns an error if `key` is not the governance writer, an entry's root
    /// may not revoke its device here, or governance cannot be updated.
    pub fn publish_governed_revocations(
        self,
        entries: &[tilo_user_identity::RevocationEntry],
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.peer.require_governance_writer(key, now)?;
        let owner = self.peer.core.owner;
        let mut doc = self
            .peer
            .core
            .room_doc
            .clone()
            .ok_or("room governance missing")?;
        let rows = room::governance_of(Some(&doc))
            .identities
            .into_iter()
            .map(|(id, bytes)| {
                let identity: GovernedIdentity = serde_json::from_slice(&bytes).map_err(err)?;
                Ok((id, bytes, identity))
            })
            .collect::<Result<Vec<_>, String>>()?;
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
        let identities = doc
            .get(ROOT, "peer_identities_v1")
            .map_err(err)?
            .ok_or("governed identities missing")?
            .1;
        doc.set_actor(self.peer.room_actor_of(key));
        let mut changed = false;
        for (id, bytes, mut identity) in rows {
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
            let mut projected = list.iter().cloned().collect::<Vec<_>>();
            projected.sort_by_key(|entry| entry.revoked_device_public_key);
            identity.revocations = projected;
            let encoded = serde_json::to_vec(&identity).map_err(err)?;
            if encoded != bytes {
                doc.put(&identities, id, automerge::ScalarValue::Bytes(encoded))
                    .map_err(err)?;
                changed = true;
            }
        }
        if changed {
            doc.commit();
            self.peer.apply_governance_doc(doc, &before, key, now)?;
        } else {
            self.peer.core.revocations = verified;
        }
        Ok(changed)
    }
    /// Atomically records a verified single-use invitation redemption, its
    /// independent root-certified device, and its room role, written by
    /// `key`: the device of any member who may invite. The invitation is
    /// checked on its own ([`crate::room::membership::invitation_admits`]),
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
        if redemption.join().grant().room != self.peer.core.room {
            return Err("invitation is not for this room".into());
        }
        self.peer.require_inviter(key, now)?;
        let cert = redemption.certificate();
        if self
            .peer
            .core
            .revocations
            .is_revoked(&cert.device_public_key)
        {
            return Err("device revoked".into());
        }
        if let Some(old) = self.peer.core.certificates.get(&cert.device_public_key)
            && old.as_ref() != cert
        {
            return Err("certificate conflict".into());
        }
        let cached = self
            .peer
            .core
            .room_doc
            .as_ref()
            .ok_or("room governance missing")?;
        let mut document = (**cached).clone();
        document.set_actor(self.peer.room_actor_of(key));
        match crate::governance::admit_invited_member(&mut document, redemption, now)? {
            crate::governance::AdmissionOutcome::AlreadyApplied => Ok(false),
            crate::governance::AdmissionOutcome::Applied { changes } => {
                let room = self.peer.core.room.clone();
                let outcome = self.peer.receive_from(
                    TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
                    NeighbourMsg::SyncResp { changes, room },
                    now,
                )?;
                Ok(outcome.changed || outcome.metadata_changed)
            }
        }
    }
    /// Admit `device`, a device certified by the root of someone already a
    /// member, as that member's: it needs no invitation, and any member's
    /// device `key` writes it in when it first asks. Whether the room changed.
    ///
    /// # Errors
    ///
    /// Returns an error if `key` is not a current member's device, `device`
    /// does not verify, is revoked or conflicts with the room's, its person
    /// is not a member, or governance cannot be updated.
    pub fn admit_member_device(
        self,
        device: &DeviceCert,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.peer.require_member_writer(key, now)?;
        tilo_user_identity::verify_device_cert_at(device, now_ns(now)).map_err(err)?;
        if self
            .peer
            .core
            .revocations
            .is_revoked(&device.device_public_key)
        {
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
            .room_doc
            .clone()
            .ok_or("room governance missing")?;
        let person = hex::encode(device.user_public_key);
        let owner = self.peer.core.owner == device.user_public_key;
        if !owner
            && doc
                .policy()
                .room_policy(&self.peer.core.room)
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
        };
        let before = doc.get_heads();
        doc.set_actor(self.peer.room_actor_of(key));
        doc.put(
            &identities,
            hex::encode(device.device_public_key),
            automerge::ScalarValue::Bytes(serde_json::to_vec(&identity).map_err(err)?),
        )
        .map_err(err)?;
        doc.commit();
        let changes = doc
            .get_changes(&before)
            .into_iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect();
        let room = self.peer.core.room.clone();
        let outcome = self.peer.receive_from(
            TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
            NeighbourMsg::SyncResp { changes, room },
            now,
        )?;
        Ok(outcome.changed || outcome.metadata_changed)
    }
    /// Change a governed collaborator's room role.
    ///
    /// # Errors
    ///
    /// Returns an error if `key` is not the governance writer, the user is the owner or
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
    /// Make a governed collaborator an administrator of the room: an Editor
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
    /// Name the admitted person `user_public_key` `name` in the room: the
    /// name every reader of [`PeerGovernance::governed_members`] shows for
    /// them, on each of their admitted devices' rows. A device's own label
    /// stays on its certificate. Whether the room changed: naming someone
    /// what they are already called writes nothing.
    ///
    /// # Errors
    ///
    /// Returns an error if `key` is not the governance writer, `name` is not
    /// a display name ([`tilo_user_identity::display_name`]), the person has
    /// no admitted device here, or governance cannot be updated.
    pub fn name_person(
        self,
        user_public_key: [u8; 32],
        name: &str,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.peer.require_governance_writer(key, now)?;
        let name = tilo_user_identity::display_name(name)
            .map_err(tilo_user_identity::DisplayNameError::reason)?;
        let mut doc = self
            .peer
            .core
            .room_doc
            .clone()
            .ok_or("room governance missing")?;
        let policy = doc.policy();
        let names = room::governance_of(Some(&doc)).peers;
        let mut devices = Vec::new();
        for (id, bytes) in &policy.identities {
            let identity: GovernedIdentity = serde_json::from_slice(bytes).map_err(err)?;
            if identity.device_cert.user_public_key == user_public_key && identity.has_admission() {
                devices.push(id.clone());
            }
        }
        if devices.is_empty() {
            return Err("the person is not admitted to the room".into());
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
        doc.set_actor(self.peer.room_actor_of(key));
        for id in &devices {
            doc.put(&peers, id, name).map_err(err)?;
        }
        doc.commit();
        let changes = doc
            .get_changes(&before)
            .into_iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect();
        let room = self.peer.core.room.clone();
        let outcome = self.peer.receive_from(
            TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
            NeighbourMsg::SyncResp { changes, room },
            now,
        )?;
        Ok(outcome.changed || outcome.metadata_changed)
    }
    /// Rewrite each administrator's grant still in the shape rooms held
    /// before [`tilo_action_policy::roles::administrator_grant`] (on the
    /// room's resource, where the Editor's own Deny outranked it) to that
    /// grant, keeping its id and giver. The governance writer runs it before
    /// it publishes governance, so a room's next governance change carries
    /// its administrators' authority. Whether the room changed.
    ///
    /// # Errors
    ///
    /// Returns an error if a grant needs rewriting and `key` is not the
    /// governance writer, or governance cannot be updated.
    pub fn reconcile_administrator_grants(
        self,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        let Some(doc) = self.peer.core.room_doc.as_ref() else {
            return Ok(false);
        };
        let room = self.peer.core.room.as_str();
        let policy = doc.policy();
        let grants = &policy.grants;
        let mut rewritten = grants
            .iter()
            .filter(|grant| tilo_action_policy::roles::is_legacy_administrator_grant(room, grant))
            .filter_map(|grant| match &grant.subject {
                Subject::Person(person) => Some(tilo_action_policy::roles::administrator_grant(
                    room,
                    person,
                    grant.granted_by.clone(),
                )),
                _ => None,
            })
            .collect::<Vec<_>>();
        // Every administrator moderates the room too.
        for grant in grants {
            if let Subject::Person(person) = &grant.subject
                && grant.id == tilo_action_policy::roles::administrator_grant_id(person)
                && grant.answer == tilo_action_policy::Answer::Allow
            {
                let moderation_id =
                    tilo_action_policy::roles::administrator_moderation_grant_id(person);
                if !grants.iter().any(|held| held.id == moderation_id) {
                    rewritten.push(tilo_action_policy::roles::administrator_moderation_grant(
                        room,
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
    /// Remove grants the governance writer decided to take back, by id.
    /// Memberships are removed through member governance.
    ///
    /// # Errors
    ///
    /// Returns an error if `key` is not the governance writer or governance
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
            .room_doc
            .clone()
            .ok_or("room governance missing")?;
        let before = doc.get_heads();
        let grants = governed_map(&doc, "grants")?;
        doc.set_actor(self.peer.room_actor_of(key));
        for id in ids {
            if doc.get(&grants, id.as_str()).map_err(err)?.is_some() {
                doc.delete(&grants, id.as_str()).map_err(err)?;
            }
        }
        doc.commit();
        self.peer.apply_governance_doc(doc, &before, key, now)
    }
    /// Write grants the governance writer decided, such as those an assigned
    /// grant set expands to, keyed by their ids.
    ///
    /// # Errors
    ///
    /// Returns an error if `key` is not the governance writer, a grant has no
    /// id or names another room, or governance cannot be updated.
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
            .room_doc
            .clone()
            .ok_or("room governance missing")?;
        let before = doc.get_heads();
        let map = governed_map(&doc, "grants")?;
        doc.set_actor(self.peer.room_actor_of(key));
        for grant in grants {
            if grant.id.as_str().trim().is_empty()
                || grant.node.room_id() != Some(self.peer.core.room.as_str())
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
        self.peer.apply_governance_doc(doc, &before, key, now)
    }
    /// Remove a governed collaborator and revoke their devices.
    ///
    /// # Errors
    ///
    /// Returns an error if `key` is not the governance writer, the user is the structural
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
            .room_doc
            .clone()
            .ok_or("room governance missing")?;
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
        doc.set_actor(self.peer.room_actor_of(key));
        if member {
            doc.delete(&members, person.as_str()).map_err(err)?;
            crate::state::delete_member_joined(&mut doc, &person)?;
        }
        for id in personal {
            doc.delete(&grants, id.as_str()).map_err(err)?;
        }
        doc.commit();
        self.peer.apply_governance_doc(doc, &before, key, now)
    }
}

impl super::StatePeer {
    /// A current member's device whose person may invite: what an
    /// admission through an invitation is written by.
    fn require_inviter(&self, key: &SigningKey, now: SystemTime) -> Result<(), String> {
        let device = key.verifying_key().to_bytes();
        self.require_member_writer(key, now)?;
        let member = self.permissions(device, now).map_err(|error| error.to_string())?;
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
}
