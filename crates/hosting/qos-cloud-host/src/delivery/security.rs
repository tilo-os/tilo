//! A verified revocation can become durable after the host loses signing power.
//! This path never fabricates a publication, replay admission or live authority.
use super::*;

impl Store {
    pub(crate) fn commit_verified_security_metadata_only(
        &mut self,
        observed: StatePeer,
        proof: qos_peer_state::VerifiedSecurityMetadataOnly,
        now: u64,
    ) -> Result<()> {
        self.ensure_reconciled()?;
        ensure!(
            self.state.version == VERSION,
            "selected delivery Store required"
        );
        let committed = committed_peer(&self.state, now)?;
        proof
            .validate(&committed, &observed)
            .map_err(anyhow::Error::msg)?;
        let (projected, stored_proof) = committed
            .project_verified_security_metadata(&observed)
            .map_err(anyhow::Error::msg)?;
        stored_proof
            .validate(&committed, &projected)
            .map_err(anyhow::Error::msg)?;
        let mut next = self.state.clone();
        next.generation = self.next_generation()?;
        let bound = next.bound.as_mut().context("enrolled Store required")?;
        bound.peer = projected.durable().map_err(anyhow::Error::msg)?;
        bound.revocations = projected.revocations();
        let revoked = crate::revocations(&bound.revocations)?;
        bound
            .roster
            .retain(|cert| !revoked.is_revoked(&cert.device_public_key));
        let policy = revision(&projected)?;
        let delivery = next.delivery.as_mut().context("delivery state missing")?;
        ensure!(
            policy != delivery.policy_revision,
            "verified security delta lacks policy change"
        );
        delivery.policy_lineage = delivery
            .policy_lineage
            .checked_add(1)
            .context("policy lineage exhausted")?;
        delivery.policy_revision = policy;
        delivery.ledger.resolved_through = next.generation;
        delivery.validate()?;
        // No post-state restore is attempted: a revoked pinned authority must
        // fail ordinary restore. The live qps proof above binds every persisted
        // native byte, while all unrelated State/ledger/pool fields remain exact.
        self.publish_prepared(next)
    }
}
