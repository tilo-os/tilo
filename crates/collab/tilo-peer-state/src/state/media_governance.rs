//! Native governance transactions. Public policy data is never an authority proof.
use super::{
    BTreeMap, DeviceCert, DurableV4, DurableV5, GovernedIdentity, NeighbourMsg, ROOT, ReadDoc,
    RevocationList, Role, Route, SigningKey, StatePeer, SystemTime, Transactable, TransportOrigin,
    err, now_ns, room,
};
use room::media::{self, RoomMediaV1};

/// Proof of one exact monotone, signature-verified security metadata delta.
/// Private fingerprints prevent a caller from reusing it for another candidate.
#[derive(Debug)]
pub struct VerifiedSecurityMetadataOnly {
    before: [u8; 32],
    after: [u8; 32],
}
impl VerifiedSecurityMetadataOnly {
    /// Check that `after` differs from `before` only in verified security metadata.
    ///
    /// # Errors
    ///
    /// Returns an error if any other state changed.
    pub fn validate(&self, before: &StatePeer, after: &StatePeer) -> Result<(), String> {
        let checked = before.verify_security_metadata_only(after)?;
        if checked.before != self.before || checked.after != self.after {
            return Err("verified security metadata proof does not bind this turn".into());
        }
        Ok(())
    }
}

impl StatePeer {
    /// Stable proof input across independently restored peers. Only watermark
    /// map iteration order is normalized; signed payloads and all other durable
    /// fields remain byte-for-byte inputs. This is never a storage export.
    fn canonical_security_metadata_state(&self) -> Result<Vec<u8>, String> {
        let bytes = self.durable()?;
        let sort_watermarks = |value: &mut DurableV4| {
            for document in &mut value.base.documents {
                document.watermarks.sort_unstable_by_key(|entry| entry.0);
            }
        };
        if self.core.admission_v5 {
            let mut value: DurableV5<'_> = postcard::from_bytes(&bytes).map_err(err)?;
            sort_watermarks(&mut value.base);
            postcard::to_allocvec(&value).map_err(err)
        } else {
            let mut value: DurableV4 = postcard::from_bytes(&bytes).map_err(err)?;
            sort_watermarks(&mut value);
            postcard::to_allocvec(&value).map_err(err)
        }
    }

    /// Project independently observed verified revocations onto this committed
    /// baseline. Volatile deferred input and every other observed field remain
    /// outside the durable metadata turn. The caller must separately validate
    /// the live receive delta before using this projection.
    ///
    /// # Errors
    ///
    /// Returns an error if the observed revocations cannot be projected onto this baseline.
    pub fn project_verified_security_metadata(
        &self,
        observed: &Self,
    ) -> Result<(Self, VerifiedSecurityMetadataOnly), String> {
        if self.core.room != observed.core.room || self.core.authority != observed.core.authority {
            return Err("security metadata projection has a different pinned authority".into());
        }
        let mut projected = self.clone();
        for entry in observed.core.revocations.iter() {
            projected
                .core
                .revocations
                .insert(entry.clone())
                .map_err(err)?;
        }
        // This also verifies root/target binding against the stored room and
        // requires a strict monotone delta that suspends existing media or
        // changes the verified security basis when media is not configured.
        let proof = self.verify_security_metadata_only(&projected)?;
        Ok((projected, proof))
    }

    /// Data publication allows room-first missing Chat; media readiness does not.
    ///
    /// # Errors
    ///
    /// Returns an error if the room media governance is invalid.
    pub fn validate_media_governance_for_publication(&self) -> Result<(), String> {
        if let Some(policy) = self.validate_present_media_policy()? {
            self.validate_media_bindings(&policy, false)?;
            let basis = media::security_basis(
                self.core.room_doc.as_deref().ok_or("missing room")?,
                &self.revocations(),
            )
            .map_err(err)?;
            if basis != policy.authorization_basis {
                return Err(
                    "media security basis requires governed revocation reconciliation".into(),
                );
            }
        }
        Ok(())
    }

    /// Verify an exact metadata-only delta. No document, receipt, pending input,
    /// coverage, admission, certificate or checkpoint scheduling change is allowed.
    ///
    /// # Errors
    ///
    /// Returns an error if `candidate` changes anything other than security metadata.
    pub fn verify_security_metadata_only(
        &self,
        candidate: &Self,
    ) -> Result<VerifiedSecurityMetadataOnly, String> {
        let room = candidate.core.room_doc.as_deref().ok_or("missing room")?;
        let policy = candidate.validate_present_media_policy()?;
        let basis = media::security_basis(room, &candidate.revocations()).map_err(err)?;
        if let Some(policy) = policy {
            if basis == policy.authorization_basis {
                return Err("security metadata does not suspend media".into());
            }
        } else {
            let before_basis = media::security_basis(
                self.core.room_doc.as_deref().ok_or("missing room")?,
                &self.revocations(),
            )
            .map_err(err)?;
            if basis == before_basis {
                return Err("security metadata does not change the verified security basis".into());
            }
        }
        let mut union = self.core.revocations.clone();
        for entry in candidate.core.revocations.iter() {
            union.insert(entry.clone()).map_err(err)?;
        }
        let entries = |list: &RevocationList| {
            let mut values = list.iter().cloned().collect::<Vec<_>>();
            values.sort_by_key(|entry| entry.revoked_device_public_key);
            values
        };
        if entries(&union) != entries(&candidate.core.revocations)
            || entries(&union) == entries(&self.core.revocations)
        {
            return Err(
                "security metadata must strictly extend the verified revocation union".into(),
            );
        }
        let mut expected = self.clone();
        expected.core.revocations = union;
        let before = self.canonical_security_metadata_state()?;
        let after = candidate.canonical_security_metadata_state()?;
        let channels_unchanged = self.core.documents.iter().all(|(route, old)| {
            candidate.core.documents.get(route).is_some_and(|new| {
                let a = &old.channel;
                let b = &new.channel;
                a.attestations == b.attestations
                    && a.authorised_signers == b.authorised_signers
                    && a.identity_attestations == b.identity_attestations
                    && a.accepted_trust_roots == b.accepted_trust_roots
                    && a.artifacts == b.artifacts
                    && a.artifact_apps == b.artifact_apps
                    && a.artifact_records == b.artifact_records
                    && a.room == b.room
                    && a.grants == b.grants
                    && a.artifact_classifications == b.artifact_classifications
                    && a.channel_min_clearance == b.channel_min_clearance
                    && a.last_seq_per_author == b.last_seq_per_author
                    && a.op_log == b.op_log
                    && a.max_in_memory_ops == b.max_in_memory_ops
                    && a.redacted_ops == b.redacted_ops
                    && a.original_envelopes == b.original_envelopes
                    && old.covered_snapshot == new.covered_snapshot
            })
        });
        if expected.canonical_security_metadata_state()? != after
            || self.core.checkpoint_times != candidate.core.checkpoint_times
            || !channels_unchanged
        {
            return Err("suspended turn changed more than verified security metadata".into());
        }
        Ok(VerifiedSecurityMetadataOnly {
            before: *blake3::hash(&before).as_bytes(),
            after: *blake3::hash(&after).as_bytes(),
        })
    }

    /// Promote verified revocations to an identity row of the same admitted root,
    /// then refresh the existing basis exactly once. Never initializes a policy.
    ///
    /// # Errors
    ///
    /// Returns an error if `key` is not the governance writer or the policy cannot be
    /// refreshed.
    pub fn reconcile_media_governance(
        &mut self,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        let policy = self.validate_present_media_policy()?;
        let Some(original) = self.core.room_doc.as_deref() else {
            return Ok(false);
        };
        // Disabled media adds no new prerequisites to legacy data publication.
        // Explicit Create still requires the strict security basis independently.
        if policy.is_none() && self.core.revocations.iter().next().is_none() {
            return Ok(false);
        }
        let computed = media::security_basis(original, &self.revocations()).and_then(|basis| {
            media::security_basis(original, &[]).map(|governed| (basis, governed))
        });
        let (basis, governed_basis) = match computed {
            Ok(values) => values,
            Err(_) if policy.is_none() => return Ok(false),
            Err(error) => return Err(err(error)),
        };
        if basis == governed_basis {
            if let Some(policy) = &policy {
                self.validate_media_bindings(policy, false)?;
            }
            return Ok(false);
        }
        self.require_media_governor(key, now)?;
        let mut doc = self.core.room_doc.clone().ok_or("missing room")?;
        let before = doc.get_heads();
        let identities = doc
            .get(ROOT, "peer_identities_v1")
            .map_err(err)?
            .ok_or("missing identities")?
            .1;
        let mut rows = room::governance_of(Some(&doc))
            .identities
            .into_iter()
            .map(|(id, bytes)| {
                serde_json::from_slice::<GovernedIdentity>(&bytes)
                    .map(|row| (id, (bytes, row)))
                    .map_err(err)
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        for entry in self.core.revocations.iter() {
            let target = hex::encode(entry.revoked_device_public_key);
            let id = rows
                .get(&target)
                .filter(|(_, row)| row.device_cert.user_public_key == entry.user_public_key)
                .map(|_| target)
                .or_else(|| {
                    rows.iter()
                        .find(|(_, (_, row))| {
                            row.device_cert.user_public_key == entry.user_public_key
                        })
                        .map(|(id, _)| id.clone())
                })
                .ok_or("revocation root has no admitted identity")?;
            let (_, row) = rows.get_mut(&id).ok_or("missing identity")?;
            let mut union = RevocationList::new();
            for revocation in row.revocations.iter().chain([entry]) {
                union.insert(revocation.clone()).map_err(err)?;
            }
            row.revocations = union.iter().cloned().collect();
            row.revocations
                .sort_by_key(|revocation| revocation.revoked_device_public_key);
        }
        doc.set_actor(automerge::ActorId::from(
            key.verifying_key().to_bytes().to_vec(),
        ));
        for (id, (old, row)) in rows {
            let bytes = serde_json::to_vec(&row).map_err(err)?;
            if bytes != old {
                doc.put(&identities, id, automerge::ScalarValue::Bytes(bytes))
                    .map_err(err)?;
            }
        }
        doc.commit();
        // apply_governance_doc stages its receive; failed refresh cannot mutate self.
        let mut candidate = self.clone();
        candidate.apply_governance_doc(doc, &before, key, now)?;
        candidate.validate_media_governance_for_publication()?;
        *self = candidate;
        Ok(true)
    }

    fn require_media_governor(&self, key: &SigningKey, now: SystemTime) -> Result<(), String> {
        self.require_governance_writer(key, now)?;
        let member = self.permissions(self.core.authority.device_public_key, now)?;
        if member.role != Role::Owner
            || room::governance_of(self.core.room_doc.as_deref())
                .owner
                .as_deref()
                != Some(&hex::encode(self.core.authority.user_public_key))
        {
            return Err("media governor is not the structural owner".into());
        }
        if !member.allows(
            &tilo_action_policy::roles::governance_action(
                &self.core.room,
                tilo_action_policy::roles::governance::SETTINGS,
            ),
            tilo_action_policy::roles::GOVERNANCE_KIND,
            now,
        ) {
            return Err("media governor lacks exact room administration".into());
        }
        Ok(())
    }

    /// Resolve the exact admitted Chat binding of a call channel.
    ///
    /// # Errors
    ///
    /// Returns an error if the route or channel does not resolve.
    pub fn resolve_media_binding(
        &self,
        app: &str,
        record: &str,
        channel: &str,
    ) -> Result<media::ChatCallBindingV1, String> {
        let document = self
            .core
            .documents
            .get(&Route {
                app: app.into(),
                record: record.into(),
            })
            .ok_or("exact admitted Chat missing")?;
        if document.encryption_required {
            return Err("exact admitted Chat is encrypted".into());
        }
        media::resolve_chat_binding(app, record, channel, &*document.doc).map_err(err)
    }

    /// Author a room media policy as the governance writer.
    ///
    /// # Errors
    ///
    /// Returns an error if `key` is not the governance writer or the policy is invalid for
    /// the current room.
    pub fn author_media_policy(
        &mut self,
        policy: &RoomMediaV1,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<(), String> {
        self.require_media_governor(key, now)?;
        self.validate_media_governance_for_publication()?;
        let mut doc = self.core.room_doc.clone().ok_or("missing room")?;
        let before = doc.get_heads();
        doc.set_actor(automerge::ActorId::from(
            key.verifying_key().to_bytes().to_vec(),
        ));
        doc.put(
            ROOT,
            media::FIELD,
            automerge::ScalarValue::Bytes(media::encode(policy).map_err(err)?),
        )
        .map_err(err)?;
        doc.commit();
        media::validate_transition(
            self.core.room_doc.as_deref(),
            &doc,
            media::TransitionMode::Authoring,
        )
        .map_err(err)?;
        let changes = doc
            .get_changes(&before)
            .into_iter()
            .map(|c| c.raw_bytes().to_vec())
            .collect();
        self.receive_from(
            TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
            NeighbourMsg::SyncResp {
                room: self.core.room.clone(),
                changes,
            },
            now,
        )?;
        Ok(())
    }

    /// Host enrollment and its invitation-only delegation share one basis change.
    ///
    /// # Errors
    ///
    /// Returns an error if `key` is not the governance writer or the certificate or
    /// delegation is refused.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "public API: callers hand over the certificate"
    )]
    pub fn admit_certificate_with_workspace_delegation(
        &mut self,
        cert: DeviceCert,
        delegation: crate::join::WorkspaceDelegation,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.require_governance_writer(key, now)?;
        if cert.user_public_key != self.core.authority.user_public_key
            || cert != delegation.delegate
        {
            return Err("enrollment/delegation certificate mismatch".into());
        }
        tilo_user_identity::verify_device_cert_at(&cert, now_ns(now)).map_err(err)?;
        if self.core.revocations.is_revoked(&cert.device_public_key) {
            return Err("device revoked".into());
        }
        if self
            .core
            .certificates
            .get(&cert.device_public_key)
            .is_some_and(|known| known.as_ref() != &cert)
        {
            return Err("certificate conflict".into());
        }
        if room::governance_of(self.core.room_doc.as_deref())
            .owner
            .as_deref()
            != Some(&hex::encode(self.core.authority.user_public_key))
        {
            return Err("delegation authority is not structural owner".into());
        }
        crate::join::verify_workspace_delegation(
            &delegation,
            &self.core.authority,
            &self.core.revocations,
            now_ns(now),
        )?;
        if delegation.room != self.core.room {
            return Err("delegation room mismatch".into());
        }
        let mut doc = self.core.room_doc.clone().ok_or("missing room")?;
        let before = doc.get_heads();
        let identities = doc
            .get(ROOT, "peer_identities_v1")
            .map_err(err)?
            .ok_or("missing identities")?
            .1;
        let peers = doc
            .get(ROOT, "peers")
            .map_err(err)?
            .ok_or("missing peers")?
            .1;
        let rows = room::governance_of(Some(&doc)).identities;
        let authority_id = hex::encode(self.core.authority.device_public_key);
        let mut authority: GovernedIdentity =
            serde_json::from_slice(rows.get(&authority_id).ok_or("missing authority")?)
                .map_err(err)?;
        if authority.device_cert != self.core.authority {
            return Err("governed authority mismatch".into());
        }
        let id = hex::encode(cert.device_public_key);
        doc.set_actor(automerge::ActorId::from(
            key.verifying_key().to_bytes().to_vec(),
        ));
        if let Some(bytes) = rows.get(&id) {
            let known: GovernedIdentity = serde_json::from_slice(bytes).map_err(err)?;
            if known.device_cert != cert {
                return Err("governed certificate conflict".into());
            }
        } else {
            let row = GovernedIdentity {
                device_cert: cert.clone(),
                revocations: vec![],
                admission: None,
                admissions: vec![],
                workspace_delegations: vec![],
            };
            doc.put(
                &identities,
                &id,
                automerge::ScalarValue::Bytes(serde_json::to_vec(&row).map_err(err)?),
            )
            .map_err(err)?;
            doc.put(&peers, &id, cert.device_label.as_str())
                .map_err(err)?;
        }
        if !authority.workspace_delegations.contains(&delegation) {
            authority.workspace_delegations.push(delegation);
            authority
                .workspace_delegations
                .sort_by_key(|d| (d.delegate.device_public_key, d.nonce));
            doc.put(
                &identities,
                &authority_id,
                automerge::ScalarValue::Bytes(serde_json::to_vec(&authority).map_err(err)?),
            )
            .map_err(err)?;
        }
        doc.commit();
        if before == doc.get_heads() {
            return Ok(false);
        }
        self.apply_governance_doc(doc, &before, key, now)
    }
}

#[cfg(test)]
mod tests;
