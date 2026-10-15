//! Authenticated launcher capabilities bound to the app's I/O runtime.
//! This client-only adapter never enables gateway server or inference code.
use agent_provider_config::protocol::{ErrorKind, ModelBackend, ModelError};
use std::rc::Rc;

#[cfg(unix)]
#[path = "runtime_client_native.rs"]
mod native;

/// Capture the launcher's authenticated descriptor and deliberate model selection.
/// Provider credentials and URLs remain within the gateway.
///
/// # Errors
/// Returns unavailable without a supported launcher, runtime or enabled selection.
pub fn from_launcher() -> Result<Rc<dyn ModelBackend>, ModelError> {
    #[cfg(unix)]
    {
        let config = agent_provider_config::ProviderConfig::load();
        let profile = config
            .active_profile()
            .ok_or_else(|| ModelError::new(ErrorKind::Unavailable))?;
        let model = profile
            .default_model
            .as_ref()
            .filter(|id| profile.models.iter().any(|model| model.id == **id))
            .ok_or_else(|| ModelError::new(ErrorKind::Unavailable))?;
        from_launcher_selection(profile.id.clone(), model.clone())
    }
    #[cfg(not(unix))]
    {
        Err(ModelError::new(ErrorKind::Unavailable))
    }
}

/// Bind an explicit public selection to the launcher's authenticated capability.
/// The gateway, not these identifiers, authorizes access.
///
/// # Errors
/// Returns unavailable if the capability or app runtime cannot be adopted.
pub fn from_launcher_selection(
    profile_id: String,
    model_id: String,
) -> Result<Rc<dyn ModelBackend>, ModelError> {
    #[cfg(unix)]
    {
        let transport = crate::transport::FdTransport::from_launcher()
            .map_err(|_| ModelError::new(ErrorKind::Unavailable))?;
        native::from_transport(transport, profile_id, model_id)
    }
    #[cfg(not(unix))]
    {
        let _ = (profile_id, model_id);
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
        assert!(
            matches!(from_launcher_selection("profile".into(), "model".into()),
            Err(error) if error.kind == ErrorKind::Unavailable)
        );
    }
}
