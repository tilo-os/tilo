//! Asynchronous model access explicitly supplied by an authenticated host.
//!
//! Unlike presentation services this capability has no process default. A host
//! binds its authenticated session to a backend before passing it to an app.
use std::rc::Rc;

pub use agent_provider_config::protocol::*;

/// A cloneable model capability. Clones retain the originating host session;
/// constructing another host does not replace it. Browser backends need not be Send.
#[derive(Clone, Default)]
pub struct Models(Option<Rc<dyn ModelBackend>>);

#[cfg(all(unix, feature = "native"))]
mod native;
#[cfg(test)]
mod tests;

impl Models {
    /// The host must authenticate and authorize this backend before injection.
    pub fn new(backend: Rc<dyn ModelBackend>) -> Self {
        Self(Some(backend))
    }

    /// Capture the native launcher's authenticated descriptor and the user's
    /// deliberate model selection once. No default profile/model is invented;
    /// provider credentials and URLs remain entirely within the gateway.
    ///
    /// # Errors
    /// Returns unavailable without an explicit enabled selection or capability.
    #[cfg(all(unix, feature = "native"))]
    pub fn from_launcher() -> Result<Self, ModelError> {
        let config = agent_provider_config::ProviderConfig::load();
        let profile = config.active_profile().ok_or_else(|| ModelError {
            kind: ErrorKind::Unavailable,
            message: "choose an enabled model provider".into(),
        })?;
        let model = profile
            .default_model
            .as_ref()
            .filter(|id| profile.models.iter().any(|model| model.id == **id))
            .ok_or_else(|| ModelError {
                kind: ErrorKind::Unavailable,
                message: "choose a model for the selected provider".into(),
            })?;
        Self::from_launcher_selection(profile.id.clone(), model.clone())
    }

    /// Launcher descriptors are unavailable on this platform.
    ///
    /// # Errors
    /// Always returns unavailable; hosts can inject a backend with [`Self::new`].
    #[cfg(not(all(unix, feature = "native")))]
    pub fn from_launcher() -> Result<Self, ModelError> {
        Err(ModelError {
            kind: ErrorKind::Unavailable,
            message: "launcher model access is unavailable on this platform".into(),
        })
    }

    /// Bind an explicit public selection to this host's launcher capability.
    /// The gateway, not these caller-supplied identifiers, authorizes access.
    ///
    /// # Errors
    /// Returns unavailable if the launcher's descriptor cannot be adopted.
    #[cfg(all(unix, feature = "native"))]
    pub fn from_launcher_selection(
        profile_id: String,
        model_id: String,
    ) -> Result<Self, ModelError> {
        use tilo_model_gateway::transport::FdTransport;
        let transport = FdTransport::from_launcher().map_err(|message| ModelError {
            kind: ErrorKind::Unavailable,
            message,
        })?;
        Self::from_transport(transport, profile_id, model_id)
    }

    /// Bind an explicitly supplied, already authenticated descriptor and public
    /// model selection. The source must not be used by another RPC client.
    /// No environment or profile discovery occurs here.
    ///
    /// # Errors
    /// Rejects invalid or disconnected descriptors.
    #[cfg(all(unix, feature = "native"))]
    pub fn from_descriptor_selection(
        descriptor: std::os::fd::BorrowedFd<'_>,
        profile_id: String,
        model_id: String,
    ) -> Result<Self, ModelError> {
        let transport =
            tilo_model_gateway::transport::FdTransport::new(descriptor).map_err(|message| {
                ModelError {
                    kind: ErrorKind::Unavailable,
                    message,
                }
            })?;
        Self::from_transport(transport, profile_id, model_id)
    }

    #[cfg(all(unix, feature = "native"))]
    fn from_transport(
        transport: tilo_model_gateway::transport::FdTransport,
        profile_id: String,
        model_id: String,
    ) -> Result<Self, ModelError> {
        let client = tilo_model_gateway::transport::ModelClient::new(
            transport,
            profile_id,
            model_id,
            std::time::Duration::from_secs(120),
        );
        Ok(Self::new(Rc::new(native::NativeModels::new(client)?)))
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
