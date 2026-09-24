//! Owned serialized buffers retained by collaboration records.

use super::*;

impl CollaborationState {
    /// Owned Vec/String backing only, including unused Vec slots. This does not
    /// account allocator headers, maps, native documents, or serde temporaries.
    /// These records contain no Arc-backed buffers: every nested buffer is owned
    /// independently and is counted once. Walking does not populate any cache.
    pub(crate) fn retained_serialized_capacity(&self) -> Result<usize, String> {
        let mut bytes = Bytes(0);
        bytes.vector(&self.invitations)?;
        for invitation in &self.invitations {
            bytes.vector(&invitation.issued.signed_invitation)?;
            bytes.grant(&invitation.grant)?;
        }
        bytes.vector(&self.join_challenges)?;
        for record in &self.join_challenges {
            bytes.certificate(&record.device_cert)?;
            bytes.vector(&record.challenge.signed_invitation)?;
            bytes.string(&record.challenge.room)?;
        }
        bytes.vector(&self.join_completions)?;
        for record in &self.join_completions {
            bytes.bootstrap(&record.bootstrap)?;
        }
        bytes.vector(&self.membership_challenges)?;
        for record in &self.membership_challenges {
            bytes.certificate(&record.device_cert)?;
            bytes.string(&record.challenge.room)?;
        }
        bytes.vector(&self.administration)?;
        for record in &self.administration {
            if let Some(result) = &record.media_result {
                bytes.string(&result.binding.app_id)?;
                bytes.string(&result.binding.record_id)?;
                bytes.string(&result.binding.channel_id)?;
                bytes.string(&result.binding.channel_object)?;
                bytes.string(&result.binding.chat_workspace_id)?;
            }
        }
        Ok(bytes.0)
    }
}

struct Bytes(usize);
fn overflow() -> String {
    "collaboration serialized backing capacity overflow".into()
}
impl Bytes {
    fn add(&mut self, capacity: usize) -> Result<(), String> {
        self.0 = self.0.checked_add(capacity).ok_or_else(overflow)?;
        Ok(())
    }
    fn vector<T>(&mut self, value: &Vec<T>) -> Result<(), String> {
        self.add(
            value
                .capacity()
                .checked_mul(size_of::<T>())
                .ok_or_else(overflow)?,
        )
    }
    fn string(&mut self, value: &String) -> Result<(), String> {
        self.add(value.capacity())
    }
    fn certificate(&mut self, value: &tilo_user_identity::DeviceCert) -> Result<(), String> {
        self.string(&value.device_label)
    }
    fn grant(&mut self, value: &InviteGrant) -> Result<(), String> {
        self.string(&value.room)?;
        self.certificate(&value.authority)?;
        self.certificate(&value.issuer)?;
        if let Some(delegation) = &value.delegation {
            self.string(&delegation.room)?;
            self.certificate(&delegation.delegate)?;
        }
        if let Some(name) = &value.invitee_name {
            self.string(name)?;
        }
        self.vector(&value.transport)?;
        for transport in &value.transport {
            self.string(&transport.kind)?;
            self.string(&transport.opaque_json)?;
            self.certificate(&transport.host)?;
        }
        Ok(())
    }
    fn bootstrap(&mut self, value: &JoinBootstrap) -> Result<(), String> {
        self.certificate(&value.device_cert)?;
        self.string(&value.room)?;
        self.certificate(&value.authority)?;
        self.vector(&value.certified_roster)?;
        for certificate in &value.certified_roster {
            self.certificate(certificate)?;
        }
        self.vector(&value.revocations)?;
        self.vector(&value.records)?;
        for record in &value.records {
            self.string(&record.app_id)?;
            self.string(&record.record_id)?;
            self.vector(&record.heads)?;
            for head in &record.heads {
                self.string(head)?;
            }
        }
        self.vector(&value.governance_messages)?;
        for message in &value.governance_messages {
            self.vector(message)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qos_peer_state::join::sign_join_proof;

    #[test]
    fn collaboration_resources_capacity_overflow_keeps_previous_total() {
        let mut bytes = Bytes(usize::MAX - 2);
        assert!(bytes.add(3).is_err());
        assert_eq!(bytes.0, usize::MAX - 2);
    }

    #[test]
    fn collaboration_resources_count_reserved_empty_record_storage() {
        let mut state = CollaborationState::default();
        assert_eq!(state.retained_serialized_capacity().unwrap(), 0);
        state.invitations.reserve_exact(7);
        state.join_challenges.reserve_exact(3);
        state.join_completions.reserve_exact(5);
        state.membership_challenges.reserve_exact(9);
        state.administration.reserve_exact(11);
        let expected = state.invitations.capacity() * size_of::<InvitationRecord>()
            + state.join_challenges.capacity() * size_of::<JoinChallengeRecord>()
            + state.join_completions.capacity() * size_of::<JoinCompletionRecord>()
            + state.membership_challenges.capacity() * size_of::<MembershipChallengeRecord>()
            + state.administration.capacity() * size_of::<AdministrationRecord>();
        assert_eq!(state.retained_serialized_capacity().unwrap(), expected);
    }

    #[test]
    fn collaboration_resources_follow_actual_completed_join_buffer_capacities() {
        let temp = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
        let now = current_time_ns();
        let authority = owner.peer().authority().clone();
        let root = ed25519_dalek::SigningKey::from_bytes(&[91; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[92; 32]);
        let mut cert = tilo_user_identity::DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "capacity guest".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: now - 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
        let service = CollaborationService::new(vec![]);
        let issued = service
            .issue(
                &mut owner,
                authority.device_public_key,
                IssueInvitationRequest {
                    request_id: [81; 32],
                    role: MemberRole::Viewer,
                    recipient_user_public_key: Some(cert.user_public_key),
                    invitee_name: Some("Capacity guest".into()),
                    expires_at_ns: now + 180_000_000_000,
                },
                now,
            )
            .unwrap();
        let challenge = service
            .challenge(
                &mut owner,
                cert.device_public_key,
                JoinChallengeRequest {
                    signed_invitation: issued.signed_invitation,
                    device_cert: cert.clone(),
                    request_id: [82; 32],
                },
                now,
            )
            .unwrap()
            .challenge;
        service
            .complete(
                &mut owner,
                cert.device_public_key,
                JoinCompleteRequest {
                    proof: sign_join_proof(challenge, &device).unwrap(),
                },
                now,
            )
            .unwrap();
        let mut state = owner.collaboration_state().clone();
        assert!(
            !state.join_completions[0]
                .bootstrap
                .governance_messages
                .is_empty()
        );
        let before = state.retained_serialized_capacity().unwrap();
        assert!(before >= state.invitations[0].issued.signed_invitation.len());
        let invitation = &mut state.invitations[0].issued.signed_invitation;
        let old = invitation.capacity();
        invitation.reserve_exact(4096);
        let added = invitation.capacity() - old;
        assert_eq!(
            state.retained_serialized_capacity().unwrap(),
            before + added
        );
        let before = state.retained_serialized_capacity().unwrap();
        let raw = &mut state.join_completions[0].bootstrap.governance_messages[0];
        let old = raw.capacity();
        raw.reserve_exact(8192);
        let added = raw.capacity() - old;
        assert_eq!(
            state.retained_serialized_capacity().unwrap(),
            before + added
        );
        let before = state.retained_serialized_capacity().unwrap();
        let label = &mut state.join_completions[0].bootstrap.certified_roster[0].device_label;
        let old = label.capacity();
        label.reserve_exact(512);
        let added = label.capacity() - old;
        assert_eq!(
            state.retained_serialized_capacity().unwrap(),
            before + added
        );
    }
}
