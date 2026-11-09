//! Native governance transactions. Public policy data is never an authority proof.
use super::facades::{PeerMediaGovernance, PeerMediaGovernanceMut};
use super::{ROOT, RevocationList, Route, SigningKey, SystemTime, Transactable, err, workspace};
use workspace::media::{self, WorkspaceMediaV1};

impl PeerMediaGovernance<'_> {
    /// Data publication allows workspace-first missing Chat; media readiness does not.
    pub fn validate_media_governance_for_publication(self) -> Result<(), String> {
        if let Some(policy) = self.peer.validate_present_media_policy()? {
            self.peer.validate_media_bindings(&policy, false)?;
            let basis = media::security_basis(
                self.peer
                    .core
                    .workspace_doc
                    .as_deref()
                    .ok_or("missing workspace")?,
                &self.peer.revocations(),
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
    /// The committed call policy as a governor taking part in the workspace's
    /// channels reads it:
    /// [`StatePeer::validated_media_policy`], except that a call bound to a
    /// Chat its members write sealed counts as bound. This replica keeps
    /// such a Chat as ciphertext and cannot check the channel itself; the
    /// governor resolved the binding through the channel's keys when it
    /// opened the call, and only a governor that holds them may vouch so.
    pub fn keyed_media_policy(self) -> Result<Option<WorkspaceMediaV1>, String> {
        let peer = self.peer;
        let policy = peer.validate_present_media_policy()?;
        if let Some(policy) = &policy {
            super::backend::media::ready(
                policy,
                |app, record| {
                    Ok(
                        super::backend::media::legacy_binding(peer, policy, app, record)?
                            || peer.publication().is_sealed_record(app, record),
                    )
                },
                || {
                    media::security_basis(
                        peer.core
                            .workspace_doc
                            .as_deref()
                            .ok_or("missing workspace")?,
                        &peer.revocations(),
                    )
                    .map_err(err)
                },
            )
            .map_err(super::backend::media::ReadinessError::legacy)?;
        }
        Ok(policy)
    }
    /// Resolve the exact admitted Chat binding of a call channel.
    pub fn resolve_media_binding(
        self,
        app: &str,
        record: &str,
        channel: &str,
    ) -> Result<media::ChatCallBindingV1, String> {
        let document = self
            .peer
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
}
impl PeerMediaGovernanceMut<'_> {
    /// Promote verified revocations to an identity row of the same admitted root,
    /// then refresh the existing basis exactly once. Never initializes a policy.
    ///
    /// # Errors
    ///
    /// Returns an error if the change is not `key`'s device's to make or the policy cannot be
    /// refreshed.
    pub fn reconcile_media_governance(
        self,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        let policy = self.peer.validate_present_media_policy()?;
        let Some(original) = self.peer.core.workspace_doc.as_deref() else {
            return Ok(false);
        };
        // Disabled media adds no new prerequisites to legacy data publication.
        // Explicit Create still requires the strict security basis independently.
        if policy.is_none() && self.peer.core.revocations.iter().next().is_none() {
            return Ok(false);
        }
        let computed =
            media::security_basis(original, &self.peer.revocations()).and_then(|basis| {
                media::security_basis(original, &[]).map(|governed| (basis, governed))
            });
        let (basis, governed_basis) = match computed {
            Ok(values) => values,
            Err(_) if policy.is_none() => return Ok(false),
            Err(error) => return Err(err(error)),
        };
        if basis == governed_basis {
            if let Some(policy) = &policy {
                self.peer.validate_media_bindings(policy, false)?;
            }
            return Ok(false);
        }
        self.peer.require_governance_action(
            key,
            tilo_action_policy::roles::governance::SETTINGS,
            now,
        )?;
        let mut doc = self
            .peer
            .core
            .workspace_doc
            .clone()
            .ok_or("missing workspace")?;
        let before = doc.get_heads();
        let mut rows = super::governed_rows::identity_rows(&doc).map_err(err)?;
        for entry in self.peer.core.revocations.iter() {
            let target = hex::encode(entry.revoked_device_public_key);
            let index = rows
                .iter()
                .position(|(id, _, row)| {
                    *id == target && row.device_cert.user_public_key == entry.user_public_key
                })
                .or_else(|| {
                    rows.iter().position(|(_, _, row)| {
                        row.device_cert.user_public_key == entry.user_public_key
                    })
                })
                .ok_or("revocation root has no admitted identity")?;
            let (_, _, row) = rows
                .get_mut(index)
                .ok_or("revocation root has no admitted identity")?;
            let mut union = RevocationList::new();
            for revocation in row.revocations.iter().chain([entry]) {
                union.insert(revocation.clone()).map_err(err)?;
            }
            row.revocations = union.iter().cloned().collect();
            row.revocations
                .sort_by_key(|revocation| revocation.revoked_device_public_key);
        }
        doc.set_actor(self.peer.workspace_actor_of(key));
        super::governed_rows::put_identity_rows(&mut doc, &rows).map_err(err)?;
        doc.commit();
        // write_governance stages its receive; failed refresh cannot mutate self.
        let mut candidate = self.peer.clone();
        candidate.write_governance(doc, &before, key, now)?;
        candidate
            .media()
            .validate_media_governance_for_publication()?;
        *self.peer = candidate;
        Ok(true)
    }
    pub fn author_media_policy(
        self,
        policy: &WorkspaceMediaV1,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<(), String> {
        self.peer.require_governance_action(
            key,
            tilo_action_policy::roles::governance::SETTINGS,
            now,
        )?;
        self.peer
            .media()
            .validate_media_governance_for_publication()?;
        let mut doc = self
            .peer
            .core
            .workspace_doc
            .clone()
            .ok_or("missing workspace")?;
        let before = doc.get_heads();
        doc.set_actor(self.peer.workspace_actor_of(key));
        doc.put(
            ROOT,
            media::FIELD,
            automerge::ScalarValue::Bytes(media::encode(policy).map_err(err)?),
        )
        .map_err(err)?;
        doc.commit();
        media::validate_transition(
            self.peer.core.workspace_doc.as_deref(),
            &doc,
            media::TransitionMode::Authoring,
        )
        .map_err(err)?;
        self.peer.write_governance(doc, &before, key, now)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
