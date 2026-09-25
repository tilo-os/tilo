//! Narrow current-policy hooks used only by the authenticated native control owner.
use super::*;
impl MediaAuthority {
    pub(crate) fn pending_control_request(
        &self,
        nonce: [u8; 32],
    ) -> Result<&GrantRequest, MediaError> {
        let pending = self.pending.get(&nonce).ok_or(MediaError::InvalidProof)?;
        match &pending.purpose {
            Purpose::Native { request, .. } => Ok(request),
            Purpose::Relay { metadata, .. } | Purpose::Refresh { metadata, .. } => {
                Ok(&metadata.request)
            }
        }
    }
    pub(crate) fn control_presence_expiry(
        &self,
        peer: &StatePeer,
        policy: &dyn MediaCallPolicy,
        session: &RelaySession,
        now: SystemTime,
    ) -> Result<u64, MediaError> {
        self.check_session(session)?;
        self.check_live(peer, policy, &session.metadata, now)?;
        let current = self
            .participations
            .get(&participation_key(&session.metadata.request))
            .ok_or(MediaError::StaleGeneration)?;
        let expiry = current
            .presence_expires_at_ns
            .min(session.metadata.expires_at_ns);
        if !current.active || expiry <= timestamp(now)? {
            return Err(MediaError::Expired);
        }
        Ok(expiry)
    }
    pub(crate) fn control_grant(
        &self,
        peer: &StatePeer,
        policy: &dyn MediaCallPolicy,
        grant: &[u8],
        now: SystemTime,
    ) -> Result<GrantMetadata, MediaError> {
        self.verify_grant(peer, policy, grant, now)
    }
    pub(crate) fn control_view(
        &self,
        peer: &StatePeer,
        policy: &dyn MediaCallPolicy,
        request: &GrantRequest,
        now: SystemTime,
    ) -> Result<(CallPolicy, [u8; 32], MediaNamespace, Vec<MediaNamespace>), MediaError> {
        let (root, current) = self.permission(peer, policy, request, now)?;
        let own = self.namespace(root, request, current.epoch)?;
        let mut names = Vec::new();
        if request.directions.subscribe {
            for participation in self.participations.values().filter(|v| {
                v.active
                    && v.metadata.namespace.call == own.call
                    && v.metadata.request.directions.publish
            }) {
                if self
                    .check_live(peer, policy, &participation.metadata, now)
                    .is_ok()
                {
                    names.push(participation.metadata.namespace);
                }
            }
            names.sort_unstable();
            names.dedup();
            if names.len() > MAX_CALL_PARTICIPANTS {
                return Err(MediaError::Capacity);
            }
        }
        Ok((current, self.host_boot, own, names))
    }
}
