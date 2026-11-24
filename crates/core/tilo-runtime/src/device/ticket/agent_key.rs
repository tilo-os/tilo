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
use tilo_device_identity::login::{APPROVE, Direction, PENDING, SigningIn, Tab, Ticket};
use tilo_lock::MutexRecover as _;
use tilo_user_identity::{AgentScope, DeviceCapabilities, DeviceCert};

use super::Tickets;

/// One agent's key waiting for the person's root, and where its certificate goes.
pub(super) struct AgentKey {
    ticket: Ticket,
    tab: Tab,
    /// The agent's id, as the approval prompt names it.
    agent: String,
    /// The one workspace the grant reaches, and the agent it names.
    scope: AgentScope,
    certified: tokio::sync::watch::Sender<Option<DeviceCert>>,
}

impl Tickets {
    /// Serve a sign-in ticket at this daemon's endpoint for `tab`, an agent's key, asking the
    /// person's root to certify it as `agent`'s grant in `workspace` (its UUID's 16 bytes). The
    /// receiver hears the certificate once a device that holds the root approved it. The ticket
    /// replaces one served for the same agent before.
    ///
    /// # Errors
    /// `tab` names no key.
    pub(crate) fn serve_agent_key(
        &self,
        tab: Tab,
        workspace: [u8; 16],
        agent: &str,
    ) -> Result<(Ticket, tokio::sync::watch::Receiver<Option<DeviceCert>>)> {
        tab.key()
            .map_err(|_| anyhow!("the agent's key is not a device key"))?;
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
            agent: agent.to_owned(),
            scope: AgentScope::agent_in(workspace, agent),
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
                capability: format!("{} in your workspace, for 30 days", waiting.agent),
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
                if waiting.tab.key().ok() != Some(cert.device_public_key) {
                    bail!("The certificate names another key than the agent's.");
                }
                if cert.capabilities != DeviceCapabilities::agent_grant(waiting.scope) {
                    bail!(
                        "The certificate gives the agent's key more or other than {} in its \
                         workspace.",
                        waiting.agent
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
