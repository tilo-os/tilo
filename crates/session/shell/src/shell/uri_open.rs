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

impl UriOpenError {
    /// What a person sees when a link or page did not open: the outcome and
    /// what to do about it, never the variant's fields.
    pub fn user_message(&self) -> String {
        match self {
            Self::InvalidUri => crate::shell::i18n::t("uri-open-invalid"),
            Self::NoHandler { scheme } if scheme == "settings" => {
                crate::shell::i18n::t("uri-open-no-settings")
            }
            Self::NoHandler { .. } => crate::shell::i18n::t("uri-open-no-handler"),
            Self::NoWorkspace => crate::shell::i18n::t("uri-open-no-workspace"),
            Self::Launch(error) => error.notification_message(),
        }
    }
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
pub fn open_in_workspace(uri: &str, chrome: &Chrome) -> Result<UriOpened, UriOpenError> {
    open_for(uri, chrome, true)
}

/// [`open_in_workspace`] for a link an app asked to open. When the person's
/// own input is behind the request (`person`,
/// [`crate::compositor::activation::person_initiated`]) they are taken to
/// it, as when they open it themselves; otherwise the window opens beside
/// the one they are in, and they stay where they are.
///
/// # Errors
///
/// Same conditions as [`open_in_workspace`].
pub fn open_for_app(uri: &str, chrome: &Chrome, person: bool) -> Result<UriOpened, UriOpenError> {
    open_for(uri, chrome, person)
}

/// When the person opened it, the app that takes the link is where they
/// go: a window of it already open (where a browser shows the link) comes
/// forward at once; a window it opens comes forward when it arrives.
#[allow(
    clippy::too_many_lines,
    reason = "one resolution of a link to its host or package handler, each with its placement and the bring-forward that follows"
)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "both guards below already end at the block that binds them, right where the \
              value(s) built from them are handed back — the first's only use is the \
              borrow fed into app_for_resource_with_policy, the second's last use is the \
              room/capabilities tuple it's cloned into; nothing to tighten without cloning \
              the workspace policy out for no real gain"
)]
fn open_for(uri: &str, chrome: &Chrome, person: bool) -> Result<UriOpened, UriOpenError> {
    let resource = resource_for_uri(uri)?;
    // Host `x-scheme-handler` entries (a mail client, a chat app) take the
    // URI when the person chose one or no Tilo package claims the scheme.
    // Captured now, as the person asked: a window the "+" of a group
    // summoned lands in that group however late it maps. Asked for by
    // someone else, it lands beside the person's window instead.
    let explicit = {
        let state = chrome.borrow();
        let tree = state.tree();
        if person {
            crate::shell::placement::explicit_group_placement(&tree, state.open_target)
        } else {
            crate::shell::placement::aside(&tree)
        }
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
        (
            host,
            workspace.map(|workspace| workspace.room().to_string()),
        )
    };
    if let Some(host) = host {
        if let Some(placement) = explicit {
            crate::shell::host_apps::note_placement(&host, placement);
        }
        let identities = host.window_identities.clone();
        return crate::shell::host_apps::launch_with_url(
            &host.desktop_id,
            uri,
            portal_workspace_id,
        )
        .inspect(|_| {
            if person && explicit.is_none() {
                bring_forward_window_of(chrome, |window| {
                    identities
                        .iter()
                        .any(|identity| identity.eq_ignore_ascii_case(window.trim()))
                });
            }
        })
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
            workspace.room(),
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
        .inspect(|()| {
            if person && explicit.is_none() {
                bring_forward_window_of(chrome, |window| {
                    launcher::app_id_matches_package(window, &app.package_id)
                });
            }
        })
        .map(|()| UriOpened::Package(app.clone()))
        .inspect_err(|_| {
            if explicit.is_some() {
                launcher::clear_pending_placement(&app.package_id);
            }
        })
        .map_err(UriOpenError::Launch)
}

/// Take the person to the window of the app that takes their link, when one
/// is open in the workspace in front: the one showing that app, else any
/// holding it ([`crate::shell::tab_binding::bring_forward`]).
fn bring_forward_window_of(chrome: &Chrome, is_app: impl Fn(&str) -> bool) {
    let window = {
        let state = chrome.borrow();
        let tree = state.tree();
        tree.desktop.active_workspace().and_then(|workspace| {
            let shows = |tab: &crate::shell::state::Tab| {
                matches!(&tab.tile, crate::shell::state::TileContent::Foreign { app_id, .. } if is_app(app_id))
            };
            let groups = || {
                workspace
                    .strips
                    .iter()
                    .flat_map(|strip| strip.window_groups.iter().map(move |group| (strip.id, group)))
            };
            groups()
                .find_map(|(strip, group)| {
                    group
                        .tabs
                        .iter()
                        .find(|tab| tab.id == group.active_tab && shows(tab))
                        .map(|tab| (strip, group.id, tab.id))
                })
                .or_else(|| {
                    groups().find_map(|(strip, group)| {
                        group.tabs.iter().find(|tab| shows(tab)).map(|tab| (strip, group.id, tab.id))
                    })
                })
                .map(|(strip, group, tab)| (workspace.id, strip, group, tab))
        })
    };
    if let Some((workspace, strip, group, tab)) = window {
        chrome.borrow_mut().select_tab(workspace, strip, group, tab);
    }
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
    fn a_link_the_person_opens_brings_the_running_browser_forward() {
        use crate::shell::state::{
            AppKind, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, WorkspaceTree,
        };
        let mut tree = WorkspaceTree::bootstrap_demo();
        let workspace = tree.desktop.active;
        let here = tree.desktop.workspaces[0].strips[0].id;
        let mut elsewhere = Strip::new(StripId(9_300_001), "Elsewhere");
        elsewhere.window_groups.push(WindowGroup {
            id: 9_300_002,
            tabs: vec![
                Tab {
                    id: 9_300_003,
                    title: "Notes".into(),
                    tile: TileContent::Stub {
                        kind: AppKind::Other("Notes".into()),
                        label: "Notes".into(),
                    },
                    app_title: None,
                },
                Tab {
                    id: 9_300_004,
                    title: "Browser".into(),
                    tile: TileContent::Foreign {
                        app_id: "so.tilo.browser".into(),
                        surface_id: 9_300_005,
                    },
                    app_title: None,
                },
            ],
            active_tab: 9_300_003,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        });
        tree.desktop.workspaces[0].strips.push(elsewhere);
        tree.desktop.active_strip.insert(workspace, here);
        let chrome = crate::shell::chrome_state::ChromeState::new(tree);

        bring_forward_window_of(&chrome, |window| {
            launcher::app_id_matches_package(window, "so.tilo.browser")
        });

        let state = chrome.borrow();
        let tree = state.tree();
        assert_eq!(
            tree.desktop.active_strip.get(&workspace),
            Some(&StripId(9_300_001)),
            "the view goes to the Browser's strip"
        );
        assert_eq!(
            tree.desktop.active_group_for(StripId(9_300_001)),
            Some(9_300_002)
        );
        assert_eq!(
            tree.desktop.workspaces[0].strips[1].window_groups[0].active_tab, 9_300_004,
            "the Browser's tab is the one shown"
        );
        drop(tree);
        drop(state);
    }

    #[test]
    fn uri_open_normalizes_schemes_and_rejects_reserved_or_argument_values() {
        {
            let resource = resource_for_uri("HTTPS://example.test/a?b=c").unwrap();
            assert_eq!(resource.as_str(), "scheme:https");
        }

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
