//! Policy-backed URI dispatch for packaged applications.
//!
//! A URI is data, never a command. The shell validates only its bounded
//! scheme-bearing shape, resolves the handler through the same personal /
//! workspace policy as files, re-reads the installed manifest immediately
//! before launch, and passes the complete URI as one opaque argument.

use desktop_policy::ResourceKind;

use crate::shell::chrome_state::Chrome;
use crate::shell::launcher::{self, InstalledApp, LaunchError};

pub const MAX_URI_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UriOpenError {
    InvalidUri,
    NoHandler { scheme: String },
    NoWorkspace,
    Launch(LaunchError),
}

pub fn resource_for_uri(uri: &str) -> Result<ResourceKind, UriOpenError> {
    if uri.is_empty()
        || uri.len() > MAX_URI_BYTES
        || uri.trim() != uri
        || uri.chars().any(char::is_control)
    {
        return Err(UriOpenError::InvalidUri);
    }
    let (scheme, rest) = uri.split_once(':').ok_or(UriOpenError::InvalidUri)?;
    if rest.is_empty() {
        return Err(UriOpenError::InvalidUri);
    }
    ResourceKind::scheme(scheme).map_err(|_| UriOpenError::InvalidUri)
}

pub fn resolve_for(
    uri: &str,
    user_id: &str,
    workspace: Option<&desktop_policy::WorkspaceDefaults>,
) -> Result<InstalledApp, UriOpenError> {
    let resource = resource_for_uri(uri)?;
    launcher::app_for_resource_with_policy(&resource, user_id, workspace).ok_or_else(|| {
        UriOpenError::NoHandler {
            scheme: resource.scheme_name().unwrap_or_default().to_string(),
        }
    })
}

pub fn open_in_workspace(uri: &str, chrome: &Chrome) -> Result<InstalledApp, UriOpenError> {
    let resource = resource_for_uri(uri)?;
    let app = {
        let state = chrome.borrow();
        let tree = state.tree();
        let workspace = tree.active_workspace();
        launcher::app_for_resource_with_policy(
            &resource,
            &state.local_user_id,
            workspace.map(|workspace| &workspace.default_apps),
        )
        .ok_or_else(|| UriOpenError::NoHandler {
            scheme: resource.scheme_name().unwrap_or_default().to_string(),
        })?
    };

    if !launcher::revalidate_resource_claim(&app.package_id, &resource) {
        return Err(UriOpenError::NoHandler {
            scheme: resource.scheme_name().unwrap_or_default().to_string(),
        });
    }

    let state = chrome.borrow();
    let tree = state.tree();
    let workspace = tree.active_workspace().ok_or(UriOpenError::NoWorkspace)?;
    launcher::launch_uri_in_workspace(&app.package_id, workspace, uri).map_err(UriOpenError::Launch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_normal_uri_and_normalizes_scheme() {
        let resource = resource_for_uri("HTTPS://example.test/a?b=c").unwrap();
        assert_eq!(resource.as_str(), "scheme:https");
    }

    #[test]
    fn rejects_reserved_or_argument_shaped_values() {
        for uri in [
            "file:///etc/passwd",
            "data:text/plain,payload",
            "javascript:alert(1)",
            " https://example.test",
            "https://example.test\n--evil",
            "https:",
            "--flag",
        ] {
            assert_eq!(resource_for_uri(uri), Err(UriOpenError::InvalidUri));
        }
    }
}
