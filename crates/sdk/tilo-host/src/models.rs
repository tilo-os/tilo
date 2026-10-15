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
