//! An agent's key, approved through a sign-in ticket this daemon serves: the key asks the
//! person's root to certify it, as a tab asks, and a device that holds the root redeems the ticket
//! the ordinary way (`tilo device add <ticket>`): `devices#pending` shows the key and what
//! certifying it makes it, and `devices#approve` hands back the certificate the root signed. The
//! certificate is the agent's grant in one workspace, and nothing more. The device's tickets serve
//! it ([`tilo_peer_state::new_device::Tickets::serve_key`]), as they serve any key asked for
//! something narrower than a browser.
//!
//! A ticket for an agent's key lasts until it is spent, or until the same agent's key is served
//! again, which replaces it.

use anyhow::{Result, anyhow};
use tilo_device_identity::login::{AgentAsked, KeyFor, Tab, Ticket};
use tilo_user_identity::DeviceCert;

use super::Tickets;

impl Tickets {
    /// Serve a sign-in ticket at this daemon's endpoint for `tab`, a key asked for what
    /// [`Tab::key_for`] names, asking the person's root to certify it so. The receiver hears the
    /// certificate once a device that holds the root approved it, and closes once the ticket is
    /// no longer served. The ticket replaces one served for the same thing before.
    ///
    /// # Errors
    /// `tab` names no key, or nothing it is for in a workspace.
    pub(crate) fn serve_key(
        &self,
        tab: Tab,
    ) -> Result<(Ticket, tokio::sync::watch::Receiver<Option<DeviceCert>>)> {
        self.0
            .service
            .tickets()
            .serve_key(
                tab,
                tilo_api::daemon::enter::reached_of(&self.0.endpoint.addr()),
            )
            .map_err(|error| anyhow!(error))
    }
}

/// The daemon's tickets are where its agents' keys are asked for: the engine that runs the
/// agents of other people asks with one of them for each such agent.
impl tilo_app_host::agents::ticketed_keys::AgentKeyTickets for Tickets {
    fn ask(
        &self,
        ask: &tilo_app_host::agents::ticketed_keys::AgentKeyAsk,
    ) -> Result<tilo_app_host::agents::ticketed_keys::AgentKeyTicket, String> {
        let tab = Tab {
            device_public_key: hex::encode(ask.key),
            label: ask.label.clone(),
            key_for: Some(KeyFor::Agent(AgentAsked {
                workspace: ask.workspace.to_string(),
                agent: ask.agent.clone(),
            })),
        };
        let (ticket, certified) = self.serve_key(tab).map_err(|error| format!("{error:#}"))?;
        Ok(tilo_app_host::agents::ticketed_keys::AgentKeyTicket {
            ticket: ticket.encode(),
            certified,
        })
    }
}
