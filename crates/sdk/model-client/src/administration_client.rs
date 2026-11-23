//! Administration: the descriptions are settings, read and set in this
//! process's settings (`agent_provider_config::management`); what only the
//! gateway can answer, a provider's model list, uses the same authenticated
//! RPC channel as inference.
use crate::{Request, Response, transport::Transport};
use agent_provider_config::{
    ModelDescriptor, ProviderConfig,
    management::{self, AdminFuture, ProviderAdmin, ProviderSnapshot},
    protocol::{ErrorKind, ModelError},
};

pub struct AdministrationClient<T> {
    transport: T,
}
impl<T: Transport> AdministrationClient<T> {
    pub fn new(transport: T) -> Self {
        Self { transport }
    }
    async fn call(&self, request: Request) -> Result<Response, ModelError> {
        let response = self.transport.call(request, &mut |_| false).await?;
        if !response.ok {
            return Err(response.failure_error().model_error());
        }
        if response.event.is_some() {
            return Err(ModelError::new(ErrorKind::InvalidResponse));
        }
        Ok(response)
    }
    async fn read<R: serde::de::DeserializeOwned>(
        &self,
        request: Request,
    ) -> Result<R, ModelError> {
        let response = self.call(request).await?;
        response
            .output
            .as_deref()
            .and_then(|text| serde_json::from_str(text).ok())
            .ok_or_else(|| ModelError::new(ErrorKind::InvalidResponse))
    }
}
impl<T: Transport> ProviderAdmin for AdministrationClient<T> {
    fn get_config(&self) -> AdminFuture<'_, ProviderSnapshot> {
        Box::pin(std::future::ready(management::snapshot()))
    }
    fn set_config(
        &self,
        expected_revision: String,
        config: ProviderConfig,
    ) -> AdminFuture<'_, ProviderSnapshot> {
        Box::pin(std::future::ready(management::replace(
            &expected_revision,
            config,
        )))
    }
    fn list_models(&self, profile_id: String) -> AdminFuture<'_, Vec<ModelDescriptor>> {
        Box::pin(self.read(Request::ListModels { profile_id }))
    }
}
