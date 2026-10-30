//! A task agent's connection: the inherited service descriptor, joined with
//! the identity its desktop handed it for one task.

use super::{
    CertifiedEndpointIdentity, CollaborationManager, SdkConnectSource, SdkJoinIdentity, Uuid,
};

impl CertifiedEndpointIdentity {
    /// The identity a task agent joins with: the presentation carrying the
    /// device's certificate and task delegation from `credentials`, proved
    /// by the task key's signature over the room's join bytes, and that key
    /// to sign its writes.
    ///
    /// # Errors
    ///
    /// Postcard's serialization error when the presentation cannot be
    /// encoded.
    pub fn for_task_agent(
        credentials: &runtime_collab::TaskAgentCredentials,
    ) -> Result<Self, postcard::Error> {
        use ed25519_dalek::Signer as _;
        let key = ed25519_dalek::SigningKey::from_bytes(&credentials.agent_key);
        let proof = key
            .sign(&runtime_identity::collaboration_join_signing_bytes(
                &credentials.workspace_id,
            ))
            .to_bytes();
        Ok(Self::delegated(
            credentials.presentation(proof)?,
            key,
            inherited_channel_keys(),
        ))
    }
}

/// The channel keys of a task agent's routes, asked of the identity service
/// its launcher handed the process, as every launched app's are: the task
/// key signs and its delegation admits, but the keys that seal and open the
/// room's records are the device's to hand out, for the routes its person
/// may read. The service is reached on the first ask, so a process with
/// none (a test) is told the keys are unavailable rather than failing to
/// start.
fn inherited_channel_keys() -> super::EndpointChannelKeys {
    let service = std::sync::OnceLock::new();
    std::sync::Arc::new(
        move |workspace_id: &str, app_id: &str, record_id: &str, generation: u64| {
            let identity = service
                .get_or_init(|| runtime_identity::load().map_err(|error| error.to_string()))
                .as_ref()
                .map_err(|error: &String| {
                    runtime_portal::NotAvailable::ConnectionFailed(std::io::Error::other(format!(
                        "a task agent's channel keys need its launcher's identity service: {error}"
                    )))
                })?;
            identity.channel_keys(workspace_id, app_id, record_id, generation)
        },
    )
}

impl CollaborationManager {
    /// [`Self::for_agent_route`] over the inherited service descriptor, but
    /// joining with `identity` instead of the launcher's identity service: a
    /// task agent presents the task delegation its desktop signed and proves
    /// it holds the task key, which also signs its writes.
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
                participant_kind: "agent",
                signer: identity.document_signer(),
            }),
            "agent",
        )
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test these are the assertion")]

    use super::CertifiedEndpointIdentity;

    /// A room that seals its records refuses every write of a connection
    /// with no channel keys, so a task agent that joined without them could
    /// write nothing, its reports included, and its run stopped unanswered.
    #[test]
    fn a_task_agent_seals_its_writes_under_channel_keys() {
        let credentials = runtime_collab::TaskAgentCredentials {
            workspace_id: "6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b".into(),
            device_certificate: b"{}".to_vec(),
            delegation: vec![1, 2, 3],
            agent_key: [7; 32],
        };
        let identity = CertifiedEndpointIdentity::for_task_agent(&credentials).unwrap();
        assert!(
            matches!(identity.signer, super::super::EndpointSigner::Delegated(_)),
            "a task agent signs as a delegated agent and asks for its routes' channel keys"
        );
    }
}
