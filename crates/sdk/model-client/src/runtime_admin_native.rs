//! Drive Send administration futures on the app's I/O runtime.
use crate::transport::{AdministrationClient, FdTransport};
use agent_provider_config::{
    ModelDescriptor, ProviderConfig,
    management::{AdminFuture, ProviderAdmin, ProviderSnapshot},
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
    fn list_models(&self, profile_id: String) -> AdminFuture<'_, Vec<ModelDescriptor>> {
        self.drive(self.client.list_models(profile_id))
    }
}
pub(crate) fn from_transport(transport: FdTransport) -> Result<Arc<dyn ProviderAdmin>, ModelError> {
    let runtime = tilo_rt::try_app().map_err(|_| ModelError::new(ErrorKind::Unavailable))?;
    Ok(Arc::new(NativeAdministration {
        client: AdministrationClient::new(transport),
        runtime: runtime.handle().clone(),
    }))
}
