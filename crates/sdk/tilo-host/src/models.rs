//! Asynchronous model access explicitly supplied by an authenticated host.
//!
//! Unlike presentation services this capability has no process default. A host
//! binds its authenticated session to a backend before passing it to an app.
use std::rc::Rc;

pub use agent_provider_config::management::{CredentialStatus, ProviderAdmin, ProviderSnapshot};
pub use agent_provider_config::protocol::*;

/// Host-scoped authority to administer providers, independent of inference access.
#[derive(Clone, Default)]
pub struct ProviderAdministration(Option<std::sync::Arc<dyn ProviderAdmin>>);

impl ProviderAdministration {
    /// Bind a backend whose session the host has explicitly authorized.
    pub fn new(backend: std::sync::Arc<dyn ProviderAdmin>) -> Self {
        Self(Some(backend))
    }

    pub fn is_available(&self) -> bool {
        self.0.is_some()
    }

    /// Obtain this host's authority, never an ambient or another mount's session.
    ///
    /// # Errors
    /// Returns unavailable when the host did not grant administration.
    pub fn get(&self) -> Result<std::sync::Arc<dyn ProviderAdmin>, ModelError> {
        self.0
            .clone()
            .ok_or_else(|| ModelError::new(ErrorKind::Unavailable))
    }

    /// Capture the launcher's separately authorized administration capability.
    ///
    /// # Errors
    /// Returns unavailable when launcher administration is unsupported or absent.
    #[cfg(feature = "native")]
    pub fn from_launcher() -> Result<Self, ModelError> {
        tilo_model_gateway::runtime_client::provider_admin_from_launcher().map(Self::new)
    }

    /// Launcher access requires native host services.
    ///
    /// # Errors
    /// Always returns unavailable; an embedding host may use [`Self::new`].
    #[cfg(not(feature = "native"))]
    pub fn from_launcher() -> Result<Self, ModelError> {
        Err(ModelError::new(ErrorKind::Unavailable))
    }
}

/// A cloneable model capability. Clones retain the originating host session;
/// constructing another host does not replace it. Browser backends need not be Send.
#[derive(Clone, Default)]
pub struct Models(Option<Rc<dyn ModelBackend>>);

#[cfg(test)]
mod tests;

impl Models {
    /// The host must authenticate and authorize this backend before injection.
    pub fn new(backend: Rc<dyn ModelBackend>) -> Self {
        Self(Some(backend))
    }

    /// Capture the launcher's authenticated capability and deliberate selection.
    ///
    /// # Errors
    /// Returns unavailable without a supported launcher, runtime or selection.
    #[cfg(feature = "native")]
    pub fn from_launcher() -> Result<Self, ModelError> {
        tilo_model_gateway::runtime_client::from_launcher().map(Self::new)
    }

    /// Launcher access requires native host services.
    ///
    /// # Errors
    /// Always returns unavailable; hosts can inject a backend with [`Self::new`].
    #[cfg(not(feature = "native"))]
    pub fn from_launcher() -> Result<Self, ModelError> {
        Err(ModelError::new(ErrorKind::Unavailable))
    }

    /// Stream presentation updates using only this host's capability.
    pub fn stream<'a>(
        &'a self,
        request: &'a ModelRequest,
        sink: StreamSink<'a>,
    ) -> CompletionFuture<'a> {
        match &self.0 {
            Some(backend) => backend.stream(request, sink),
            None => self.complete(request),
        }
    }

    pub fn is_available(&self) -> bool {
        self.0.is_some()
    }

    /// Complete without blocking the UI. No backend means unavailable, never
    /// an ambient client, provider key lookup or fallback to another session.
    /// The future is intentionally local: browser host capabilities are not `Send`.
    pub fn complete<'a>(&'a self, request: &'a ModelRequest) -> CompletionFuture<'a> {
        match &self.0 {
            Some(backend) => backend.complete(request),
            None => Box::pin(async {
                Err(ModelError {
                    kind: ErrorKind::Unavailable,
                    message: "model access is unavailable from this host".into(),
                })
            }),
        }
    }
}
