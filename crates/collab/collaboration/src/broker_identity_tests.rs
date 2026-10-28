use super::SdkCertifiedIdentity;

impl SdkCertifiedIdentity for runtime_identity_host::SigningIdentity {
    fn channel_keys(
        &self,
        workspace_id: &str,
        app_id: &str,
        record_id: &str,
        known_generation: u64,
    ) -> Result<runtime_identity::wire::ChannelKeysReply, runtime_portal::NotAvailable> {
        runtime_identity_host::SigningIdentity::channel_keys(
            self,
            workspace_id,
            app_id,
            record_id,
            known_generation,
        )
    }
    fn device_certificate(&self) -> Option<&[u8]> {
        runtime_identity_host::SigningIdentity::device_certificate(self)
    }
    fn signing_public_key(&self) -> [u8; 32] {
        runtime_identity_host::SigningIdentity::signing_public_key(self)
    }
    fn sign_collaboration_join(
        &self,
        workspace_id: &str,
    ) -> Result<[u8; 64], runtime_portal::NotAvailable> {
        runtime_identity_host::SigningIdentity::sign_collaboration_join(self, workspace_id)
    }
    fn sign_collaboration_op(
        &self,
        workspace_id: &str,
        app_id: &str,
        record_id: &str,
        unsigned_op: &[u8],
    ) -> Result<[u8; 64], runtime_portal::NotAvailable> {
        runtime_identity_host::SigningIdentity::sign_collaboration_op(
            self,
            workspace_id,
            app_id,
            record_id,
            unsigned_op,
        )
    }
}
