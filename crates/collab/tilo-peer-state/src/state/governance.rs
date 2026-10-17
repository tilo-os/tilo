//! The membership governance this peer authors and reads: members, roles,
//! grants, delegations and revocations as the room document records them.
use super::facades::{PeerGovernance, PeerGovernanceMut};
use super::{
    BTreeMap, BTreeSet, DeviceCert, GovernedAccess, GovernedIdentity, GovernedMember, Grant,
    GrantId, NeighbourMsg, ROOT, ReadDoc, RecordReaders, RevocationList, Role, SigningKey, Subject,
    SystemTime, Transactable, TransportOrigin, administrator_grant_id, err, governed_map,
    member_role_from_role, now_ns, room,
};

impl PeerGovernance<'_> {
    /// The sandbox capabilities the room's governance grants its apps.
    ///
    /// # Errors
    ///
    /// Returns an error if room governance is missing or a capability is
    /// malformed.
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
    ///
    /// # Errors
    ///
    /// Returns an error when the room's governance is missing or malformed.
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
    ///
    /// # Errors
    ///
    /// Returns an error if room governance is missing or malformed.
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
    /// The governed member devices a Read Deny keeps out of a record's
    /// channel, as a channel steward admits them.
    ///
    /// A channel is one record's, and its id a hash of the record's route,
    /// so a steward decides a channel by its record when it can name it: a
    /// record the room's grants name ([`collab_types::named_records`]), or one
    /// of `routes`, the routes devices proved to it
    /// ([`collab_types::ChannelRoute::proves`]). For each such record
    /// [`RecordReaders::withheld`] lists every device of
    /// [`Self::governed_access`] that may not read it
    /// ([`Self::check_document_read`], the decision every door applies), so a
    /// Deny on the record, on its app or on any pattern over it withholds its
    /// key, and the room's other records are unaffected.
    ///
    /// A channel whose record the steward cannot name may be any record
    /// nothing names. [`RecordReaders::unrouted`] lists every device that
    /// may not read one of the records a Deny over a pattern stands for
    /// ([`collab_types::pattern_read_witnesses`]): such a channel is withheld
    /// from them until a device names its record, and from no one else.
    ///
    /// # Errors
    ///
    /// Returns an error if room governance is missing or malformed.
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
            // Only a grant below the room can decide a record otherwise than
            // the room: a record none covers is read by every member.
            let node = collab_types::document_node(room, app, record);
            let covered = policy
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
        let unrouted = collab_types::pattern_read_witnesses(&policy.grants, room)
            .iter()
            .flat_map(|(app, record)| denied(app, record))
            .collect();
        Ok(RecordReaders { withheld, unrouted })
    }
    /// Authority-authored collaborator projection. The structural owner is
    /// exposed separately by [`PeerGovernance::owner_user_public_key`].
    ///
    /// # Errors
    ///
    /// Returns an error if room governance is missing or malformed.
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
        self.peer.core.authority.user_public_key
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
        device_cert: &DeviceCert,
        request_id: [u8; 32],
        nonce: [u8; 32],
        expires_at_ns: u64,
        now: SystemTime,
    ) -> Result<crate::join::MembershipChallenge, String> {
        if self
            .peer
            .core
            .certificates
            .get(&device_cert.device_public_key)
            .map(AsRef::as_ref)
            != Some(device_cert)
        {
            return Err("membership certificate is absent from governed state".into());
        }
        self.peer.certificate(&device_cert.device_public_key, now)?;
        match self.peer.permissions(device_cert.device_public_key, now) {
            Ok(_) => {}
            Err(error)
                if error.contains("no room membership")
                    && self.peer.has_governed_admission(device_cert) =>
            {
                return Err("member has been removed".into());
            }
            Err(error) => return Err(error),
        }
        crate::join::MembershipChallenge::new(
            &self.peer.core.room,
            workspace_id,
            &self.peer.core.authority,
            device_cert,
            request_id,
            nonce,
            expires_at_ns,
            now_ns(now),
        )
    }
    /// Check a verified membership refresh against current governance.
    ///
    /// # Errors
    ///
    /// Returns an error if the refreshed device is no longer a governed member.
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
    /// Publish same-root, root-signed revocations into pinned governance.
    /// Independent collaborator roots retain their own signed revocation scope.
    ///
    /// # Errors
    ///
    /// Returns an error if `key` is not the governance writer, an entry is not signed by
    /// the authority root, or governance cannot be updated.
    pub fn publish_governed_revocations(
        self,
        entries: &[tilo_user_identity::RevocationEntry],
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<(), String> {
        self.peer.require_governance_writer(key, now)?;
        let mut verified = self.peer.core.revocations.clone();
        for entry in entries {
            if entry.user_public_key != self.peer.core.authority.user_public_key {
                return Err("governed revocation root mismatch".into());
            }
            verified.insert(entry.clone()).map_err(err)?;
        }
        let mut doc = self
            .peer
            .core
            .room_doc
            .clone()
            .ok_or("room governance missing")?;
        let before = doc.get_heads();
        let identities = doc
            .get(ROOT, "peer_identities_v1")
            .map_err(err)?
            .ok_or("governed identities missing")?
            .1;
        doc.set_actor(automerge::ActorId::from(
            key.verifying_key().to_bytes().to_vec(),
        ));
        let mut changed = false;
        for (id, bytes) in room::governance_of(Some(&doc)).identities {
            let mut identity: GovernedIdentity = serde_json::from_slice(&bytes).map_err(err)?;
            if identity.device_cert.user_public_key != self.peer.core.authority.user_public_key {
                continue;
            }
            let mut list = RevocationList::new();
            for entry in &identity.revocations {
                list.insert(entry.clone()).map_err(err)?;
            }
            for entry in verified.iter() {
                if entry.revoked_device_public_key == identity.device_cert.device_public_key
                    || identity.device_cert == self.peer.core.authority
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
        Ok(())
    }
    /// Record a verified workspace delegation in pinned governance.
    ///
    /// # Errors
    ///
    /// Returns an error if `key` is not the governance writer, the delegation does not
    /// verify, its service is absent from the governed roster, or governance is missing.
    pub fn install_workspace_delegation(
        self,
        delegation: crate::join::WorkspaceDelegation,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.peer.require_governance_writer(key, now)?;
        if self
            .peer
            .core
            .certificates
            .get(&delegation.delegate.device_public_key)
            .map(AsRef::as_ref)
            != Some(&delegation.delegate)
        {
            return Err("delegated workspace service is absent from governed roster".into());
        }
        let cached = self
            .peer
            .core
            .room_doc
            .as_ref()
            .ok_or("room governance missing")?;
        let mut document = (**cached).clone();
        match crate::governance::install_workspace_delegation(
            &mut document,
            &self.peer.core.authority,
            delegation,
            key,
            now,
        )? {
            crate::governance::GovernanceOutcome::Unchanged => Ok(false),
            crate::governance::GovernanceOutcome::Applied { changes } => {
                let room = self.peer.core.room.clone();
                let outcome = self.peer.receive_from(
                    // The shared governance transaction has already verified
                    // either the owner or its persisted room delegation.
                    TransportOrigin::RelayDelta(self.peer.core.authority.device_public_key),
                    NeighbourMsg::SyncResp { changes, room },
                    now,
                )?;
                Ok(outcome.changed || outcome.metadata_changed)
            }
        }
    }
    /// Atomically records a verified single-use invitation redemption, its
    /// independent root-certified device, and its room role.
    ///
    /// # Errors
    ///
    /// Returns an error if `key` is not the governance writer, the redemption conflicts
    /// with recorded governance, or governance cannot be updated.
    pub fn admit_invited_member(
        self,
        redemption: &crate::join::VerifiedRedemption,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        let grant = redemption.join().grant();
        if grant.room != self.peer.core.room || grant.authority != self.peer.core.authority {
            return Err("invitation is not for this pinned room authority".into());
        }
        if grant.issuer == self.peer.core.authority {
            self.peer.require_governance_writer(key, now)?;
        } else {
            if key.verifying_key().to_bytes() != grant.issuer.device_public_key {
                return Err("governance writer is not invitation issuer".into());
            }
            if self
                .peer
                .certificate(&grant.issuer.device_public_key, now)?
                != &grant.issuer
            {
                return Err("delegated invitation issuer certificate mismatch".into());
            }
        }
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
        match crate::governance::admit_invited_member(&mut document, redemption, key, now)? {
            crate::governance::AdmissionOutcome::AlreadyApplied => Ok(false),
            crate::governance::AdmissionOutcome::Applied { changes } => {
                let room = self.peer.core.room.clone();
                let outcome = self.peer.receive_from(
                    TransportOrigin::RelayDelta(self.peer.core.authority.device_public_key),
                    NeighbourMsg::SyncResp { changes, room },
                    now,
                )?;
                Ok(outcome.changed || outcome.metadata_changed)
            }
        }
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
        self.peer.require_governance_writer(key, now)?;
        let mut doc = self
            .peer
            .core
            .room_doc
            .clone()
            .ok_or("room governance missing")?;
        let before = doc.get_heads();
        let grants = governed_map(&doc, "grants")?;
        doc.set_actor(automerge::ActorId::from(
            key.verifying_key().to_bytes().to_vec(),
        ));
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
        self.peer.require_governance_writer(key, now)?;
        let mut doc = self
            .peer
            .core
            .room_doc
            .clone()
            .ok_or("room governance missing")?;
        let before = doc.get_heads();
        let map = governed_map(&doc, "grants")?;
        doc.set_actor(automerge::ActorId::from(
            key.verifying_key().to_bytes().to_vec(),
        ));
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
        self.peer.require_governance_writer(key, now)?;
        if user_public_key == self.peer.core.authority.user_public_key {
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
        doc.set_actor(automerge::ActorId::from(
            key.verifying_key().to_bytes().to_vec(),
        ));
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
