//! Drive Send administration futures on the app's I/O runtime.
use crate::transport::{AdministrationClient, FdTransport};
use agent_provider_config::{
    ProviderConfig,
    management::{AdminFuture, CredentialStatus, ProviderAdmin, ProviderSnapshot},
    protocol::{ErrorKind, ModelError},
};
use std::sync::Arc;

struct NativeAdministration {
    client: AdministrationClient<FdTransport>,
    runtime: tokio::runtime::Handle,
}
impl NativeAdministration {
    fn drive<'a, T: 'a>(&'a self, mut future: AdminFuture<'a, T>) -> AdminFuture<'a, T> {
        Box::pin(std::future::poll_fn(move |context| {
            let _entered = self.runtime.enter();
            future.as_mut().poll(context)
        }))
    }
}
impl ProviderAdmin for NativeAdministration {
    fn get_config(&self) -> AdminFuture<'_, ProviderSnapshot> {
        self.drive(self.client.get_config())
    }
    fn set_config(
        &self,
        expected_revision: String,
        config: ProviderConfig,
    ) -> AdminFuture<'_, ProviderSnapshot> {
        self.drive(self.client.set_config(expected_revision, config))
    }
    fn credential_status(&self, profile_id: String) -> AdminFuture<'_, CredentialStatus> {
        self.drive(self.client.credential_status(profile_id))
    }
    fn set_api_key(&self, profile_id: String, api_key: String) -> AdminFuture<'_, ()> {
        self.drive(self.client.set_api_key(profile_id, api_key))
    }
    fn remove_api_key(&self, profile_id: String) -> AdminFuture<'_, ()> {
        self.drive(self.client.remove_api_key(profile_id))
    }
    fn migrate_api_key(
        &self,
        profile_id: String,
        original_endpoint: String,
    ) -> AdminFuture<'_, ()> {
        self.drive(self.client.migrate_api_key(profile_id, original_endpoint))
    }
    fn remove_legacy_api_key(&self, profile_id: String) -> AdminFuture<'_, ()> {
        self.drive(self.client.remove_legacy_api_key(profile_id))
    }
}
pub(crate) fn from_transport(transport: FdTransport) -> Result<Arc<dyn ProviderAdmin>, ModelError> {
    let runtime = tilo_rt::try_app().map_err(|_| ModelError::new(ErrorKind::Unavailable))?;
    Ok(Arc::new(NativeAdministration {
        client: AdministrationClient::new(transport),
        runtime: runtime.handle().clone(),
    }))
}
