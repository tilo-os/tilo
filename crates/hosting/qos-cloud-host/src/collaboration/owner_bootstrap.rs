//! Structural owner completion. This component does not install an HTTP route.
use super::*;
use qos_peer_state::join::OwnerBootstrapV1;
mod receipt;
use receipt::{OwnerReceipt, ReceiptPurpose};

fn receipt_key(request_id: [u8; 32]) -> String {
    format!("owner-membership-v1:{}", hex::encode(request_id))
}

/// Result of a membership refresh, selected from governed Store state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MembershipRefresh {
    Member(JoinBootstrap),
    Owner(OwnerBootstrapV1),
}

impl MembershipRefresh {
    pub fn into_response(self) -> JoinTransportResponse {
        match self {
            Self::Member(bootstrap) => JoinTransportResponse::MembershipComplete(bootstrap),
            Self::Owner(bootstrap) => JoinTransportResponse::OwnerMembershipComplete(bootstrap),
        }
    }
}

impl Store {
    /// Routes a membership completion to the owner or collaborator path.
    /// The route comes only from host-held state: an existing typed receipt,
    /// or the host-issued challenge's certificate root equal to the enrolled
    /// structural root. Each path then performs its own full verification, so
    /// routing never grants authority; the collaborator path still refuses
    /// owners and the owner path still refuses non-owners.
    pub fn membership_refresh(
        &mut self,
        remote_endpoint: [u8; 32],
        request: MembershipCompleteRequest,
        now: u64,
    ) -> Result<MembershipRefresh> {
        self.ensure_reconciled()?;
        let request_id = request.proof.challenge.request_id;
        let key = hex::encode(request_id);
        let owner = if self.state.consumed.contains_key(&receipt_key(request_id)) {
            true
        } else if self.state.membership_responses.contains_key(&key) {
            false
        } else {
            let root = self.state.bound.as_ref().map(|bound| bound.root);
            self.state
                .membership_challenges
                .get(&key)
                .is_some_and(|issued| Some(issued.device_cert.user_public_key) == root)
        };
        if owner {
            let bootstrap = self.owner_membership_complete(remote_endpoint, request, now)?;
            Ok(MembershipRefresh::Owner(bootstrap))
        } else {
            let bootstrap = self.membership_complete(remote_endpoint, request, now)?;
            self.verify_cached_membership(remote_endpoint, &bootstrap, now)?;
            Ok(MembershipRefresh::Member(bootstrap))
        }
    }

    /// Rechecks a refresh result before disclosure after an asynchronous step.
    pub fn verify_membership_refresh(
        &self,
        remote_endpoint: [u8; 32],
        refresh: &MembershipRefresh,
        now: u64,
    ) -> Result<()> {
        match refresh {
            MembershipRefresh::Member(bootstrap) => {
                self.verify_cached_membership(remote_endpoint, bootstrap, now)
            }
            MembershipRefresh::Owner(bootstrap) => {
                self.ensure_reconciled()?;
                if bootstrap.device_cert.device_public_key != remote_endpoint {
                    bail!("owner membership endpoint mismatch")
                }
                let now = self.policy_now_or(now)?;
                let peer = self.peer_at(now)?;
                self.verify_owner_scope(
                    &peer,
                    &bootstrap.device_cert,
                    &bootstrap.authority,
                    &bootstrap.room,
                    bootstrap.workspace_id,
                )?;
                peer.verify_current_owner(&bootstrap.device_cert, now_time(now))
                    .map_err(anyhow::Error::msg)
            }
        }
    }

    /// Completes only a currently verified structural owner's membership proof.
    /// The eventual HTTP dispatcher must choose this path from actual governed
    /// role and enrolled root, never from a client flag or material label.
    pub fn owner_membership_complete(
        &mut self,
        remote_endpoint: [u8; 32],
        request: MembershipCompleteRequest,
        now: u64,
    ) -> Result<OwnerBootstrapV1> {
        self.ensure_reconciled()?;
        let now = self.policy_now_or(now)?;
        let request_id = request.proof.challenge.request_id;
        if request_id == [0; 32] {
            bail!("owner membership request ID is absent")
        }
        let key = receipt_key(request_id);
        let proof_digest = *blake3::hash(&postcard::to_stdvec(&request)?).as_bytes();
        if let Some((stored_digest, value)) = self.state.consumed.get(&key) {
            let stored = OwnerReceipt::decode(value)?;
            if stored_digest != &hex::encode(proof_digest)
                || stored.proof_digest != proof_digest
                || stored.request != request
            {
                bail!("owner membership proof request ID substitution")
            }
            self.verify_owner_receipt(remote_endpoint, &stored, now)?;
            return Ok(stored.bootstrap);
        }
        let challenge_key = hex::encode(request_id);
        let issued = self
            .state
            .membership_challenges
            .get(&challenge_key)
            .cloned()
            .ok_or_else(|| anyhow!("membership challenge was not issued"))?;
        if issued.device_cert.device_public_key != remote_endpoint {
            bail!("owner membership proof endpoint mismatch")
        }
        let (before, mut peer) = self.peer_transition_at(now)?;
        let membership = verify_membership_proof(
            &request.proof,
            &issued.challenge,
            &issued.device_cert,
            &revocations(&peer.revocations())?,
            now,
        )
        .map_err(anyhow::Error::msg)?;
        if membership.workspace_id() != qos_relay::room_workspace_id(&self.state.room)? {
            bail!("owner membership workspace mismatch")
        }
        peer.verify_owner_refresh(&membership, now_time(self.policy_now_or(now)?))
            .map_err(anyhow::Error::msg)?;
        self.verify_owner_scope(
            &peer,
            &issued.device_cert,
            peer.authority(),
            &self.state.room,
            membership.workspace_id(),
        )?;
        let generation = self.next_generation()?;
        let messages = crate::durable_bootstrap_messages(
            &mut peer,
            &self.key(),
            now_time(self.policy_now_or(now)?),
            generation,
        )?;
        let live = live_projection_from_bootstrap(&before, &peer, &messages)?;
        let bootstrap = OwnerBootstrapV1 {
            version: 1,
            device_cert: issued.device_cert.clone(),
            room: self.state.room.clone(),
            workspace_id: membership.workspace_id(),
            authority: peer.authority().clone(),
            certified_roster: peer.certificates(),
            revocations: peer.revocations(),
            records: peer.rows(),
            governance_messages: messages
                .iter()
                .map(postcard::to_stdvec)
                .collect::<Result<Vec<_>, _>>()?,
            generation,
        };
        let stored = OwnerReceipt {
            version: 1,
            purpose: ReceiptPurpose::OwnerMembership,
            proof_digest,
            completed_at_ns: now,
            issued,
            request,
            bootstrap,
        };
        let value = stored.encode()?;
        let mut state = self.state.clone();
        state.outbox.extend(live);
        state
            .bound
            .as_mut()
            .ok_or_else(|| anyhow!("workspace not enrolled"))?
            .peer = peer
            .durable()
            .map_err(|error| anyhow::Error::msg(error).context(crate::StoreUnavailable))?;
        state.membership_challenges.remove(&challenge_key);
        state
            .consumed
            .insert(key, (hex::encode(proof_digest), value));
        peer.verify_current_owner(
            &stored.bootstrap.device_cert,
            now_time(self.policy_now_or(now)?),
        )
        .map_err(anyhow::Error::msg)?;
        self.commit(state)?;
        audit(
            "collaboration_owner_membership_refreshed",
            serde_json::json!({
                "room": self.state.room,
                "request_id": hex::encode(request_id),
                "caller_device": hex::encode(remote_endpoint),
                "caller_root": hex::encode(stored.bootstrap.device_cert.user_public_key),
                "generation": self.state.generation.to_string(),
                "result": "committed",
            }),
        );
        peer.verify_current_owner(
            &stored.bootstrap.device_cert,
            now_time(self.policy_now_or(now)?),
        )
        .map_err(anyhow::Error::msg)?;
        Ok(stored.bootstrap)
    }

    fn verify_owner_scope(
        &self,
        peer: &qos_peer_state::StatePeer,
        cert: &DeviceCert,
        authority: &DeviceCert,
        room: &str,
        workspace: u64,
    ) -> Result<()> {
        let bound = self
            .state
            .bound
            .as_ref()
            .ok_or_else(|| anyhow!("workspace not enrolled"))?;
        if room != self.state.room
            || workspace != qos_relay::room_workspace_id(&self.state.room)?
            || authority != &bound.authority
            || authority != peer.authority()
            || authority.device_public_key != self.key().verifying_key().to_bytes()
            || cert.user_public_key != bound.root
            || authority.user_public_key != bound.root
        {
            bail!("owner membership pinned scope mismatch")
        }
        Ok(())
    }

    fn verify_owner_receipt(
        &self,
        remote: [u8; 32],
        stored: &OwnerReceipt,
        now: u64,
    ) -> Result<()> {
        if stored.bootstrap.device_cert.device_public_key != remote
            || stored.bootstrap.device_cert != stored.issued.device_cert
            || stored.bootstrap.generation == 0
            || stored.bootstrap.generation > self.state.generation
            || stored.completed_at_ns > now
            || stored.request.proof.challenge != stored.issued.challenge
        {
            bail!("owner membership receipt binding mismatch")
        }
        let issued_request = MembershipChallengeRequest {
            room: stored.issued.challenge.room.clone(),
            workspace_id: stored.issued.challenge.workspace_id,
            device_cert: stored.issued.device_cert.clone(),
            request_id: stored.issued.challenge.request_id,
        };
        if stored.issued.request_digest
            != *blake3::hash(&postcard::to_stdvec(&issued_request)?).as_bytes()
        {
            bail!("owner membership issued request mismatch")
        }
        let peer = self.peer_at(self.policy_now_or(now)?)?;
        self.verify_owner_scope(
            &peer,
            &stored.bootstrap.device_cert,
            &stored.bootstrap.authority,
            &stored.bootstrap.room,
            stored.bootstrap.workspace_id,
        )?;
        // Historical proof establishes the exact retained exchange only. Fresh
        // current membership/host checks below establish disclosure permission.
        let membership = verify_membership_proof(
            &stored.request.proof,
            &stored.issued.challenge,
            &stored.issued.device_cert,
            &revocations(&peer.revocations())?,
            stored.completed_at_ns,
        )
        .map_err(anyhow::Error::msg)?;
        if membership.workspace_id() != stored.bootstrap.workspace_id {
            bail!("owner membership cached workspace mismatch")
        }
        peer.verify_owner_refresh(&membership, now_time(self.policy_now_or(now)?))
            .map_err(anyhow::Error::msg)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
