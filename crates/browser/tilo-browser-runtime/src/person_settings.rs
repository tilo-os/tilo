//! The person's mail accounts and model providers, as their page reaches
//! them.
//!
//! A page cannot open a mail server's session and keeps no vault of its own,
//! so these asks are the workspace host's to answer. The page's desktop
//! hands them to the session it already holds with the host
//! ([`SessionActions`]), as it hands the MCP server there: the host keeps
//! the accounts, proves each sign-in, and seals the secrets in the
//! person's own vault there. Nothing is stored in the page.

use std::sync::Arc;

use tilo_actions::remote::{person_accounts, person_models, person_vault};
use tilo_desktop_surface::session::DesktopSession;
use tilo_session_client::remote::SessionActions;

/// Carry the accounts and providers asks of the desktop `session` to the
/// host `host` reaches.
pub(crate) fn serve(session: &DesktopSession, host: &SessionActions) {
    for provider in [
        person_accounts::PROVIDER,
        person_models::PROVIDER,
        person_vault::PROVIDER,
    ] {
        session.serve_person_api(provider, Arc::new(host.clone()));
    }
}
