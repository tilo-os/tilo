//! An agent's key, approved through a sign-in ticket this daemon serves: the key asks the
//! person's root to certify it, as a tab asks, and a device that holds the root redeems the ticket
//! the ordinary way (`tilo device add <ticket>`): `devices#pending` shows the key and what
//! certifying it makes it, and `devices#approve` hands back the certificate the root signed. The
//! certificate is the agent's grant in one workspace, and nothing more. The device's tickets serve
//! it ([`tilo_peer_state::new_device::Tickets::serve_agent_key`]).
//!
//! A ticket for an agent's key lasts until it is spent, or until the same agent's key is served
//! again, which replaces it.

use anyhow::{Result, anyhow};
use tilo_device_identity::login::{AgentAsked, Tab, Ticket};
use tilo_user_identity::DeviceCert;

use super::Tickets;

impl Tickets {
    /// Serve a sign-in ticket at this daemon's endpoint for `tab`, an agent's key, asking the
    /// person's root to certify it as the grant of the agent `tab` names in its one workspace. The
    /// receiver hears the certificate once a device that holds the root approved it, and closes
    /// once the ticket is no longer served. The ticket replaces one served for the same agent
    /// before.
    ///
    /// # Errors
    /// `tab` names no key, or no agent in a workspace.
    pub(crate) fn serve_agent_key(
        &self,
        tab: Tab,
    ) -> Result<(Ticket, tokio::sync::watch::Receiver<Option<DeviceCert>>)> {
        self.0
            .service
            .tickets()
            .serve_agent_key(
                tab,
                tilo_api::daemon::enter::reached_of(&self.0.endpoint.addr()),
            )
            .map_err(|error| anyhow!(error))
    }
}

/// The daemon's tickets are where its agents' keys are asked for: the engine that runs the
/// agents of other people asks with one of them for each such agent.
impl tilo_workspace_service::agents::ticketed_keys::AgentKeyTickets for Tickets {
    fn ask(
        &self,
        ask: &tilo_workspace_service::agents::ticketed_keys::AgentKeyAsk,
    ) -> Result<tilo_workspace_service::agents::ticketed_keys::AgentKeyTicket, String> {
        let tab = Tab {
            device_public_key: hex::encode(ask.key),
            label: ask.label.clone(),
            agent: Some(AgentAsked {
                workspace: ask.workspace.to_string(),
                agent: ask.agent.clone(),
            }),
        };
        let (ticket, certified) = self
            .serve_agent_key(tab)
            .map_err(|error| format!("{error:#}"))?;
        Ok(
            tilo_workspace_service::agents::ticketed_keys::AgentKeyTicket {
                ticket: ticket.encode(),
                certified,
            },
        )
    }
}
