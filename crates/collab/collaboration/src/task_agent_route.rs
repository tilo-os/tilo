//! An agent's connection: the inherited service descriptor, joined with the
//! identity its desktop handed it.

use super::{
    CertifiedEndpointIdentity, CollaborationManager, SdkConnectSource, SdkJoinIdentity, Uuid,
};

impl CollaborationManager {
    /// [`Self::for_agent_route`] over the inherited service descriptor, but
    /// joining with `identity` instead of the launcher's identity service: an
    /// agent presents the agent certificate its desktop signed and proves it
    /// holds the agent key, which also signs its writes.
    ///
    /// # Errors
    ///
    /// Returns [`runtime_portal::NotAvailable`] when the collaboration
    /// service is unavailable or the broker refuses the join.
    pub fn for_agent_route_with_identity(
        app_id: impl Into<String>,
        cluster_id: Uuid,
        record: Option<String>,
        agent_id: impl Into<String>,
        identity: CertifiedEndpointIdentity,
    ) -> Result<Self, runtime_portal::NotAvailable> {
        Self::for_route_from(
            SdkConnectSource::Env,
            app_id.into(),
            cluster_id,
            record,
            Some(agent_id.into()),
            Some(SdkJoinIdentity {
                presentation: identity.presentation.clone(),
                member_kind: "agent",
                signer: identity.document_signer(),
            }),
            "agent",
        )
    }
}
