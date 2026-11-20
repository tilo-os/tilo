//! Actions a window asks of the workspace's server, on a page whose workspace has one: those of
//! a resource of the workspace, and what that server keeps for the workspace's
//! people (its accounts, model providers, vault and what it tells them).
//!
//! Chat asks a shared note for its link; any window may ask a record another app keeps for one of
//! its actions (provider `record:<app>:<record>`). On a native desktop the broker hands that to
//! whatever serves the record. A page reaches the workspace's host, which runs a record's actions
//! whoever has it open and decides each by the asking person's own access: the page grants
//! nothing itself. No device of the person's is asked. The desktop's session carries such an
//! action here
//! ([`tilo_desktop_surface::session::DesktopSession::serve_workspace_api`]).

use std::sync::Arc;

use tilo_session_actions::CurrentSession;

/// The server of the workspace `config`'s page shows, as the page reaches it now; `None` for a
/// page with no workspace server.
///
/// The page's session with its server comes and goes: a page that certifies its device again
/// keeps its id while a new session replaces the one under it. So the server's action API is
/// looked up for every request, and what a window follows (a Settings pane's reading of the
/// person's accounts, an action still running) is asked again of the session that replaced the
/// one it was asked of ([`CurrentSession`]): a pane open across the replacement hears the new
/// session's answers, not the last answer of the one before.
pub(in crate::browser) fn workspace_server(
    config: &super::super::Config,
) -> Option<Arc<CurrentSession>> {
    let peer = config.peer?;
    let workspace = crate::peer::workspace_id(&config.workspace).ok()?;
    Some(Arc::new(crate::peer_sync::current_session(peer, workspace)))
}
