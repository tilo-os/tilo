//! An agent's key, approved through a sign-in ticket this daemon serves: the key asks the
//! person's root to certify it, as a tab asks, and a device that holds the root redeems the ticket
//! the ordinary way (`tilo device add <ticket>`): `devices#pending` shows the key and what
//! certifying it makes it, and `devices#approve` hands back the certificate the root signed. The
//! certificate is the agent's grant in one workspace, and nothing more.
//!
//! A ticket for an agent's key lasts until it is spent, or until the same agent's key is served
//! again, which replaces it.

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Value, json};
use tilo_device_identity::login::{
    APPROVE, AgentAsked, Direction, PENDING, SigningIn, Tab, Ticket,
};
use tilo_lock::MutexRecover as _;
use tilo_user_identity::DeviceCert;

use super::Tickets;

/// One agent's key waiting for the person's root, and where its certificate goes.
pub(super) struct AgentKey {
    ticket: Ticket,
    tab: Tab,
    /// The agent the key is asked for, in its one workspace.
    agent: AgentAsked,
    certified: tokio::sync::watch::Sender<Option<DeviceCert>>,
}

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
        tab.key()
            .map_err(|_| anyhow!("the agent's key is not a device key"))?;
        let agent = tab
            .agent
            .clone()
            .context("a ticket for an agent's key names the agent")?;
        agent
            .scope()
            .map_err(|_| anyhow!("the agent's workspace is not a workspace's id"))?;
        let ticket = Ticket::in_direction(
            Direction::SignIn,
            tilo_api::daemon::enter::reached_of(&self.0.endpoint.addr()),
        );
        let (certified, heard) = tokio::sync::watch::channel(None);
        let mut keys = self.0.agent_keys.lock_recover();
        keys.retain(|key| key.agent != agent);
        keys.push(AgentKey {
            ticket: ticket.clone(),
            tab,
            agent,
            certified,
        });
        drop(keys);
        Ok((ticket, heard))
    }

    /// Whether `args` names the secret of an agent's key this daemon serves a ticket for.
    pub(super) fn asks_for_agent_key(&self, args: &Value) -> bool {
        let Some(secret) = args.get("secret").and_then(Value::as_str) else {
            return false;
        };
        self.0
            .agent_keys
            .lock_recover()
            .iter()
            .any(|key| key.ticket.secret_hex() == secret)
    }

    /// Answer `action`, [`PENDING`] or [`APPROVE`], for the agent's key `args` names by its
    /// ticket's secret, as a device that holds the person's root asks it.
    pub(super) fn agent_key(&self, action: &str, args: &Value) -> Result<Value> {
        let secret = args
            .get("secret")
            .and_then(Value::as_str)
            .context("a sign-in names its ticket's secret")?;
        let mut keys = self.0.agent_keys.lock_recover();
        let at = keys
            .iter()
            .position(|key| key.ticket.secret_hex() == secret)
            .context("No agent's key waits for this ticket here: it was used, or a newer one replaced it.")?;
        let waiting = keys.get(at).context("the agent's key is listed")?;
        match action {
            PENDING => Ok(json!(SigningIn {
                device: waiting.tab.device_public_key.clone(),
                label: waiting.tab.fitted_label(),
                capability: format!("{} in your workspace, for 30 days", waiting.agent.agent),
                agent: Some(waiting.agent.clone()),
            })),
            APPROVE => {
                let cert: DeviceCert = args
                    .get("cert")
                    .cloned()
                    .and_then(|cert| serde_json::from_value(cert).ok())
                    .context(
                        "Approving an agent's key takes the certificate your identity signed for it.",
                    )?;
                tilo_user_identity::verify_device_cert_at(&cert, tilo_clock::unix_now_ns())
                    .map_err(|_| anyhow!("The certificate for the agent's key does not hold."))?;
                // What the root signs for this key is the one certificate a ticket for it asks
                // for: the agent's grant in its workspace, for as long as such a grant lasts.
                let want = waiting
                    .tab
                    .certificate(cert.user_public_key, cert.issued_at_ns)
                    .map_err(|error| anyhow!("The agent's key cannot be certified: {error}."))?;
                if cert.device_public_key != want.device_public_key {
                    bail!("The certificate names another key than the agent's.");
                }
                if cert.capabilities != want.capabilities
                    || cert.expires_at_ns != want.expires_at_ns
                {
                    bail!(
                        "The certificate gives the agent's key more or other than {} in its \
                         workspace.",
                        waiting.agent.agent
                    );
                }
                let spent = keys.remove(at);
                drop(keys);
                let answer = json!({
                    "label": cert.device_label,
                    "device": hex::encode(cert.device_public_key),
                });
                spent.certified.send_replace(Some(cert));
                Ok(answer)
            }
            _ => bail!("an agent's key is shown and approved, nothing more"),
        }
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
