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

/// # Errors
///
/// Returns [`UriOpenError::InvalidUri`] if `uri` is empty, exceeds
/// [`MAX_URI_BYTES`], carries leading/trailing whitespace, contains a
/// control character, or has no `scheme:` prefix `desktop_policy`
/// recognizes.
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

/// # Errors
///
/// Propagates [`resource_for_uri`]'s error for a malformed `uri`, or
/// returns [`UriOpenError::NoHandler`] if no app claims the URI's scheme
/// under `user_id`'s or `workspace`'s policy.
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

/// # Errors
///
/// Returns [`UriOpenError::InvalidUri`] for a malformed `uri`,
/// [`UriOpenError::NoHandler`] if no app claims (or still claims) the
/// scheme, [`UriOpenError::NoWorkspace`] if there is no active workspace
/// to open into, or [`UriOpenError::Launch`] if starting the app failed.
#[allow(
    clippy::significant_drop_tightening,
    reason = "both guards below already end at the block that binds them, right where the \
              value(s) built from them are handed back — the first's only use is the \
              borrow fed into app_for_resource_with_policy, the second's last use is the \
              room/capabilities tuple it's cloned into; nothing to tighten without cloning \
              the workspace policy out for no real gain"
)]
pub fn open_in_workspace(uri: &str, chrome: &Chrome) -> Result<UriOpened, UriOpenError> {
    let resource = resource_for_uri(uri)?;
    // Host `x-scheme-handler` entries (a mail client, a chat app) take the
    // URI when the person chose one or no Tilo package claims the scheme.
    // Captured now, as the person asked: a window the "+" of a group
    // summoned lands in that group however late it maps.
    let explicit = {
        let state = chrome.borrow();
        crate::shell::placement::explicit_group_placement(&state.tree(), state.open_target)
    };
    let (host, portal_workspace_id) = {
        let state = chrome.borrow();
        let tree = state.tree();
        let workspace = tree.desktop.active_workspace();
        let host = match launcher::handler_for_resource_with_policy(
            &resource,
            &state.local_user_id,
            workspace.map(|workspace| &workspace.default_apps),
        ) {
            Some((launcher::ResourceHandler::Host(host), _)) => Some(host),
            _ => None,
        };
        (host, workspace.map(|workspace| workspace.room.to_string()))
    };
    if let Some(host) = host {
        if let Some(placement) = explicit {
            crate::shell::host_apps::note_placement(&host, placement);
        }
        return crate::shell::host_apps::launch_with_url(
            &host.desktop_id,
            uri,
            portal_workspace_id,
        )
        .map(UriOpened::Host)
        .inspect_err(|_| {
            if explicit.is_some() {
                crate::shell::launcher::clear_pending_placement(&host.desktop_id);
            }
        })
        .map_err(UriOpenError::Launch);
    }
    // The guard's only use is the borrow passed into
    // `app_for_resource_with_policy`, released the moment that call
    // returns; cloning the workspace policy out just to drop the guard a
    // statement earlier would cost more than the lookup it's guarding.
    let app = {
        let state = chrome.borrow();
        let tree = state.tree();
        let workspace = tree.desktop.active_workspace();
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

    // Read just the room and capabilities out of the tree and drop the
    // guard before the launch's file I/O and process spawn, rather than
    // holding a read lock on the whole workspace tree through them.
    let (room, capabilities) = {
        let state = chrome.borrow();
        let tree = state.tree();
        let workspace = tree
            .desktop
            .active_workspace()
            .ok_or(UriOpenError::NoWorkspace)?;
        (
            workspace.room,
            workspace.launch_grants(tilo_clock::unix_now()),
        )
    };
    // Placement is noted before queueing, as the person asked, so the window
    // lands in the group whose "+" summoned it however late it maps. Queued:
    // the launch's file work runs on the preflight worker, not on this (the
    // compositor) thread. A refusal arrives as a launch event, which retires
    // the note.
    if let Some(placement) = explicit {
        launcher::set_pending_placement(&app.package_id, placement);
    }
    launcher::queue_uri_in_room(app.clone(), room, capabilities, uri.to_owned())
        .map(|()| UriOpened::Package(app.clone()))
        .inspect_err(|_| {
            if explicit.is_some() {
                launcher::clear_pending_placement(&app.package_id);
            }
        })
        .map_err(UriOpenError::Launch)
}

/// Which kind of application received a URI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UriOpened {
    Package(InstalledApp),
    Host(crate::shell::host_apps::HostApp),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_open_normalizes_schemes_and_rejects_reserved_or_argument_values() {
        // Phase `accepts_normal_uri_and_normalizes_scheme`.
        {
            let resource = resource_for_uri("HTTPS://example.test/a?b=c").unwrap();
            assert_eq!(resource.as_str(), "scheme:https");
        }

        // Phase `rejects_reserved_or_argument_shaped_values`.
        {
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
}
