//! Authenticated launcher capabilities bound to the app's I/O runtime.
//! This client-only adapter never enables gateway server or inference code.
use agent_provider_config::protocol::{ErrorKind, ModelBackend, ModelError};
use std::rc::Rc;

#[cfg(unix)]
#[path = "runtime_client_native.rs"]
mod native;

/// Capture the launcher's authenticated descriptor and use the server default.
/// Provider credentials and URLs remain within the gateway.
pub fn from_launcher() -> Result<Rc<dyn ModelBackend>, ModelError> {
    #[cfg(unix)]
    {
        native::default_from_transport(native::launcher_transport()?)
    }
    #[cfg(not(unix))]
    {
        Err(ModelError::new(ErrorKind::Unavailable))
    }
}

/// Bind provider administration without requiring a model selection.
/// The launcher capability and backend still enforce the manager role.
pub fn provider_admin_from_launcher()
-> Result<std::sync::Arc<dyn agent_provider_config::management::ProviderAdmin>, ModelError> {
    #[cfg(unix)]
    {
        native::admin_from_transport(native::launcher_transport()?)
    }
    #[cfg(not(unix))]
    {
        Err(ModelError::new(ErrorKind::Unavailable))
    }
}

/// Bind an authenticated descriptor and explicit public selection. No environment
/// or profile discovery occurs; no other RPC client may use the source.
///
/// # Errors
/// Rejects invalid or disconnected descriptors and unavailable app runtimes.
#[cfg(unix)]
pub fn from_descriptor_selection(
    descriptor: std::os::fd::BorrowedFd<'_>,
    profile_id: String,
    model_id: String,
) -> Result<Rc<dyn ModelBackend>, ModelError> {
    let transport = crate::transport::FdTransport::new(descriptor)
        .map_err(|_| ModelError::new(ErrorKind::Unavailable))?;
    native::from_transport(transport, profile_id, model_id)
}

#[cfg(all(test, not(unix)))]
mod tests {
    use super::*;

    #[test]
    fn unsupported_launcher_is_unavailable() {
        assert!(matches!(from_launcher(), Err(error) if error.kind == ErrorKind::Unavailable));
    }
}
