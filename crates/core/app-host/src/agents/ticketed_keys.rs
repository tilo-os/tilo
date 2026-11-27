//! The key a device makes for an agent whose person's root it does not hold, approved the one way
//! a key is added: with a ticket.
//!
//! A server that keeps a workspace may run another person's agent there. It makes a key of its
//! own for the agent, from its own device key, the way a person's device makes one from their
//! root ([`super::agent_keys::FleetAgentKeys`]): its device key signs a statement naming the
//! workspace and the agent, and the key is made from that signature, so nothing secret is written
//! down. It asks for that key with a ticket the device serves ([`AgentKeyTickets`]), and the
//! agent shows the ticket while it waits (its entry's `runs_on` in the workspace's agents
//! record). The person redeems it on a device that holds their root, `tilo device add <ticket>`
//! as for any device of theirs, which certifies the key for that one agent in that one
//! workspace. The certificate comes back to this device, which keeps it beside the ticket's place
//! and signs as the agent only while it holds: it is checked whenever a key is granted and again
//! at every signing, so a certificate that ran out or that the person's root revoked signs
//! nothing more. A certificate close to running out is asked for again with a new ticket.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use ed25519_dalek::SigningKey;
use tilo_actions::remote::InvokeRequest;
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::{AgentScope, DeviceCapabilities, DeviceCert};
use tilo_workspace_agent::Agent;

use super::agent_keys::{AgentKeys, RENEW_WITHIN_NS};

/// What separates the statement a device's key for an agent is made from from anything else its
/// device key signs.
const KEY_DOMAIN: &[u8] = b"tilo-agent-ticketed-key-v1\0";

/// The agent a device asks to run with a ticket, and the key the ticket names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentKeyAsk {
    pub workspace: WorkspaceId,
    /// The agent's id.
    pub agent: String,
    /// The name the agent's key is listed by among its person's keys.
    pub label: String,
    /// The key the device signs as the agent with.
    pub key: [u8; 32],
}

/// A ticket the device serves for an agent's key ([`AgentKeyAsk`]): its text, as the agent shows
/// it, and where the certificate comes once a device of the agent's person's redeems it. The
/// ticket is no longer served once the sender is dropped.
pub struct AgentKeyTicket {
    pub ticket: String,
    pub certified: tokio::sync::watch::Receiver<Option<DeviceCert>>,
}

/// How a device serves a ticket for an agent's key: the device's own ticket actions, which serve
/// every ticket it issues.
pub trait AgentKeyTickets: Send + Sync {
    /// Serve a ticket asking for `ask`'s key to be approved, replacing the one served for the
    /// same agent before.
    ///
    /// # Errors
    /// The device serves no tickets now.
    fn ask(&self, ask: &AgentKeyAsk) -> Result<AgentKeyTicket, String>;
}

/// The keys a device makes for agents whose person's root it does not hold: one per agent per
/// workspace, made from the device's own key, and used once the agent's entry in the workspace's
/// agents record holds a certificate the agent's person's root issued for it that is still valid
/// and not revoked.
pub struct TicketedAgentKeys {
    workspaces: tilo_workspace_service::workspaces::WeakWorkspaces,
    /// The keys used so far, by their public key, with their certificates.
    made: Mutex<HashMap<[u8; 32], (DeviceCert, SigningKey)>>,
    /// The agents a ticket is served for now, by workspace and agent.
    asking: Arc<Mutex<HashSet<(WorkspaceId, String)>>>,
}

impl TicketedAgentKeys {
    /// The keys `workspaces`' device makes.
    #[must_use]
    pub fn of(workspaces: &tilo_workspace_service::workspaces::Workspaces) -> Self {
        Self {
            workspaces: workspaces.downgrade(),
            made: Mutex::default(),
            asking: Arc::default(),
        }
    }

    /// The key of `agent` in `workspace` that this device makes from its device key.
    fn key(
        workspaces: &tilo_workspace_service::workspaces::Workspaces,
        workspace: WorkspaceId,
        agent: &str,
    ) -> SigningKey {
        let mut statement = KEY_DOMAIN.to_vec();
        statement.extend_from_slice(workspace.as_uuid().as_bytes());
        statement.extend_from_slice(agent.as_bytes());
        workspaces.derived_device_key(&statement)
    }

    /// The certificate this device runs `agent` of `workspace` under, as the agent's entry keeps
    /// it, once it is the agent's person's grant of this device's key and holds now.
    fn approved(
        workspaces: &tilo_workspace_service::workspaces::Workspaces,
        workspace: WorkspaceId,
        agent: &Agent,
    ) -> Result<(DeviceCert, SigningKey), String> {
        let device = hex::encode(workspaces.device_cert().device_public_key);
        let certificate = agent
            .runs_on
            .iter()
            .find(|runs| runs.device == device)
            .and_then(|runs| runs.certificate.as_deref())
            .ok_or("the agent's person has not approved this device's key for it")?;
        let cert = decode_certificate(certificate)?;
        let key = Self::key(workspaces, workspace, &agent.id.0);
        granted(&cert, workspace, agent, key.verifying_key().to_bytes())?;
        holds(&cert, workspaces)?;
        Ok((cert, key))
    }

    /// Ask, with a ticket `tickets` serves, for this device's key for `agent` of `workspace` to
    /// be approved, unless it runs the agent under a certificate that holds and is not close to
    /// running out, or a ticket for it is served now. The ticket shows on the agent while it
    /// waits; the certificate a redeemed ticket gives is kept on the agent's entry.
    pub(crate) async fn ask_unless_approved(
        &self,
        tickets: &Arc<dyn AgentKeyTickets>,
        workspace: WorkspaceId,
        agent: &Agent,
    ) {
        let Some(workspaces) = self.workspaces.upgrade() else {
            return;
        };
        let current = Self::approved(&workspaces, workspace, agent);
        if current.as_ref().is_ok_and(|(cert, _)| !renews(cert)) {
            return;
        }
        // A key its person revoked stays revoked: no ticket asks for it again.
        let device = hex::encode(workspaces.device_cert().device_public_key);
        let revoked = agent
            .runs_on
            .iter()
            .find(|runs| runs.device == device)
            .and_then(|runs| runs.certificate.as_deref())
            .and_then(|certificate| {
                tilo_failure::ignored!(
                    "a certificate that does not read is no revoked key: a ticket asks anew",
                    decode_certificate(certificate)
                )
                .ok()
            })
            .is_some_and(|cert| {
                workspaces.revocations().refuses(&cert) || workspaces.blocked_keys().revokes(&cert)
            });
        if revoked {
            return;
        }
        let asked = (workspace, agent.id.0.clone());
        if !self.asking.lock_recover().insert(asked.clone()) {
            return;
        }
        let key = Self::key(&workspaces, workspace, &agent.id.0);
        let here = workspaces.device_cert().device_label.clone();
        let ask = AgentKeyAsk {
            workspace,
            agent: agent.id.0.clone(),
            label: format!("{} on {here}", agent.name),
            key: key.verifying_key().to_bytes(),
        };
        let served = match tickets.ask(&ask) {
            Ok(served) => served,
            Err(error) => {
                self.asking.lock_recover().remove(&asked);
                tracing::warn!(%workspace, agent = %agent.id.0, %error, "no ticket asks for an agent's key to be approved");
                return;
            }
        };
        let place = workspace.to_string();
        if let Err(error) = workspaces
            .wait_to_run_agent(&place, &agent.id.0, &served.ticket)
            .await
        {
            self.asking.lock_recover().remove(&asked);
            tracing::warn!(%workspace, agent = %agent.id.0, %error, "the ticket asking for an agent's key to be approved does not show on it");
            return;
        }
        let asking = Arc::clone(&self.asking);
        let weak = self.workspaces.clone();
        let agent = agent.clone();
        let mut certified = served.certified;
        tokio::spawn(async move {
            let cert = certified
                .wait_for(Option::is_some)
                .await
                .ok()
                .and_then(|cert| cert.clone());
            asking.lock_recover().remove(&asked);
            let (Some(cert), Some(workspaces)) = (cert, weak.upgrade()) else {
                return;
            };
            if let Err(error) =
                granted(&cert, workspace, &agent, ask.key).and_then(|()| holds(&cert, &workspaces))
            {
                tracing::warn!(%workspace, agent = %agent.id.0, %error, "a redeemed ticket's certificate is not the agent's person's grant");
                return;
            }
            // The key lists itself on this device's copy, signing its own row and grant there,
            // and every holder takes the row in as it syncs: its person approved it, and that is
            // the one step it takes.
            if let Err(error) = workspaces.enter(&place, (&cert, &key)).await {
                tracing::warn!(%workspace, agent = %agent.id.0, %error, "an agent's approved key did not list itself");
                return;
            }
            if let Err(error) = workspaces.run_agent_on(&place, &agent.id.0, &cert).await {
                tracing::warn!(%workspace, agent = %agent.id.0, %error, "the certificate of an agent's key was not kept");
            }
        });
    }
}

impl AgentKeys for TicketedAgentKeys {
    fn key_here(&self, workspace: WorkspaceId, agent: &str) -> Option<[u8; 32]> {
        let workspaces = self.workspaces.upgrade()?;
        Some(
            Self::key(&workspaces, workspace, agent)
                .verifying_key()
                .to_bytes(),
        )
    }

    fn grant(&self, _: WorkspaceId, _: &str, _: &str, _: [u8; 32]) -> Result<[u8; 32], String> {
        Err("this device signs as an agent only with a key its person approved here".to_owned())
    }

    fn grant_for(&self, workspace: WorkspaceId, agent: &Agent) -> Result<[u8; 32], String> {
        let workspaces = self
            .workspaces
            .upgrade()
            .ok_or("this device's workspaces stopped")?;
        let (cert, key) = Self::approved(&workspaces, workspace, agent)?;
        let public = key.verifying_key().to_bytes();
        self.made.lock_recover().insert(public, (cert, key));
        Ok(public)
    }

    fn sign(
        &self,
        workspace: WorkspaceId,
        key: &[u8; 32],
        request: &mut InvokeRequest,
    ) -> Result<(), String> {
        let made = self
            .made(key)
            .ok_or("the agent's key was not made on this device")?;
        let workspaces = self
            .workspaces
            .upgrade()
            .ok_or("this device's workspaces stopped")?;
        // Checked again at every signing: a certificate that ran out or that the person's root
        // revoked since the key was granted signs nothing more.
        holds(&made.0, &workspaces).inspect_err(|_| {
            self.made.lock_recover().remove(key);
        })?;
        sign_with(made, workspace, request)
    }

    fn made(&self, key: &[u8; 32]) -> Option<(DeviceCert, SigningKey)> {
        self.made.lock_recover().get(key).cloned()
    }

    fn revocation(
        &self,
        person: &[u8; 32],
        key: &[u8; 32],
    ) -> Result<Option<tilo_user_identity::RevocationEntry>, String> {
        let workspaces = self
            .workspaces
            .upgrade()
            .ok_or("this device's workspaces stopped")?;
        // The keys here are other people's agents': their roots' revocations reach this device
        // as the workspace announces them, beside what its own person's devices record holds.
        let announced = workspaces.revocations();
        let keys = workspaces.blocked_keys();
        let entry = announced
            .iter()
            .chain(keys.revocations().iter())
            .find(|entry| {
                entry.user_public_key == *person && entry.revoked_device_public_key == *key
            })
            .cloned();
        if entry.is_some() {
            self.made.lock_recover().remove(key);
        }
        Ok(entry)
    }
}

/// The keys a device's agents sign with: made from the roots its identity store holds, where it
/// holds the agent's person's, and otherwise the key it asked to run the agent with, once its
/// person approved it.
pub struct DeviceAgentKeys {
    roots: Option<Arc<dyn AgentKeys>>,
    ticketed: TicketedAgentKeys,
}

impl DeviceAgentKeys {
    /// The keys of `workspaces`' device, made from the roots `roots` holds where it has an
    /// identity store.
    #[must_use]
    pub fn of(
        workspaces: &tilo_workspace_service::workspaces::Workspaces,
        roots: Option<Arc<dyn AgentKeys>>,
    ) -> Self {
        Self {
            roots,
            ticketed: TicketedAgentKeys::of(workspaces),
        }
    }

    /// Whether `agent` of `workspace` signs with a key made from its person's root here.
    pub(crate) fn signs_from_root(&self, workspace: WorkspaceId, agent: &Agent) -> bool {
        self.roots
            .as_ref()
            .is_some_and(|roots| roots.grant_for(workspace, agent).is_ok())
    }

    /// The keys this device makes for agents whose person's root it does not hold.
    pub(crate) const fn ticketed(&self) -> &TicketedAgentKeys {
        &self.ticketed
    }
}

impl AgentKeys for DeviceAgentKeys {
    fn grant(
        &self,
        workspace: WorkspaceId,
        agent: &str,
        name: &str,
        person: [u8; 32],
    ) -> Result<[u8; 32], String> {
        self.roots
            .as_ref()
            .ok_or("this device holds no identity store, so it makes no agent key from a root")?
            .grant(workspace, agent, name, person)
    }

    fn key_here(&self, workspace: WorkspaceId, agent: &str) -> Option<[u8; 32]> {
        self.roots
            .as_ref()
            .and_then(|roots| roots.key_here(workspace, agent))
            .or_else(|| self.ticketed.key_here(workspace, agent))
    }

    fn grant_for(&self, workspace: WorkspaceId, agent: &Agent) -> Result<[u8; 32], String> {
        let from_root = match &self.roots {
            Some(roots) => match roots.grant_for(workspace, agent) {
                Ok(key) => return Ok(key),
                Err(error) => Some(error),
            },
            None => None,
        };
        self.ticketed
            .grant_for(workspace, agent)
            .map_err(|ticketed| match from_root {
                Some(from_root) => format!(
                    "{from_root}; nor did its person approve this device's key for it: {ticketed}"
                ),
                None => ticketed,
            })
    }

    fn sign(
        &self,
        workspace: WorkspaceId,
        key: &[u8; 32],
        request: &mut InvokeRequest,
    ) -> Result<(), String> {
        match &self.roots {
            Some(roots) if roots.made(key).is_some() => roots.sign(workspace, key, request),
            _ => self.ticketed.sign(workspace, key, request),
        }
    }

    fn made(&self, key: &[u8; 32]) -> Option<(DeviceCert, SigningKey)> {
        self.roots
            .as_ref()
            .and_then(|roots| roots.made(key))
            .or_else(|| self.ticketed.made(key))
    }

    fn revocation(
        &self,
        person: &[u8; 32],
        key: &[u8; 32],
    ) -> Result<Option<tilo_user_identity::RevocationEntry>, String> {
        match &self.roots {
            Some(roots) if roots.made(key).is_some() => roots.revocation(person, key),
            _ => self.ticketed.revocation(person, key),
        }
    }

    fn keep_in(&self, record: &tilo_workspace_service::workspaces::Workspaces) {
        if let Some(roots) = &self.roots {
            roots.keep_in(record);
        }
    }

    fn keep_key(&self, cert: &DeviceCert) {
        if let Some(roots) = &self.roots {
            roots.keep_key(cert);
        }
    }
}

/// Whether `cert` is the grant of `key` to `agent` of `workspace` by the agent's person's root:
/// for that agent in that workspace alone.
fn granted(
    cert: &DeviceCert,
    workspace: WorkspaceId,
    agent: &Agent,
    key: [u8; 32],
) -> Result<(), String> {
    let root = tilo_member_types::PersonId::parse(&agent.acts_for)
        .map_err(|error| format!("the agent belongs to no person ({error})"))?
        .root()
        .ok_or("the agent belongs to no person with a root")?;
    let scope = AgentScope::agent_in(*workspace.as_uuid().as_bytes(), &agent.id.0);
    if cert.user_public_key != root
        || cert.device_public_key != key
        || cert.capabilities != DeviceCapabilities::agent_grant(scope)
    {
        return Err("the certificate is not the agent's person's grant of this key".into());
    }
    Ok(())
}

/// Whether `cert` holds now on `workspaces`' device: it verifies, has not run out, and the record
/// of the person's devices holds no root-signed revocation of it.
fn holds(
    cert: &DeviceCert,
    workspaces: &tilo_workspace_service::workspaces::Workspaces,
) -> Result<(), String> {
    tilo_user_identity::verify_device_cert_at(cert, tilo_clock::unix_now_ns())
        .map_err(|error| format!("the certificate of the key does not hold: {error}"))?;
    // The key's own person's devices record, and the revocations the workspace announces: a
    // member's root revokes the key it approved for its agent on its own devices.
    if workspaces.revocations().refuses(cert) || workspaces.blocked_keys().revokes(cert) {
        return Err("the agent's person revoked the key".to_owned());
    }
    Ok(())
}

/// The certificate `text` holds, as an agent's entry keeps it: its bytes in hex.
///
/// # Errors
/// It is no certificate.
pub fn decode_certificate(text: &str) -> Result<DeviceCert, String> {
    let bytes = hex::decode(text).map_err(|_| "the certificate is not hex".to_owned())?;
    postcard::from_bytes(&bytes).map_err(|error| format!("the certificate does not read: {error}"))
}

/// Whether `cert` runs out soon enough to be asked for again, as a grant the person's own device
/// makes is certified again ([`RENEW_WITHIN_NS`]).
fn renews(cert: &DeviceCert) -> bool {
    cert.expires_at_ns
        .is_some_and(|expires| expires.saturating_sub(tilo_clock::unix_now_ns()) <= RENEW_WITHIN_NS)
}

/// Sign `request` in `workspace` with `made`, a key this device made and its certificate.
fn sign_with(
    made: (DeviceCert, SigningKey),
    workspace: WorkspaceId,
    request: &mut InvokeRequest,
) -> Result<(), String> {
    let (cert, agent_key) = made;
    request.signed = None;
    let invocation = tilo_server_protocol::invocation::sign_invocation_now(
        &workspace.to_string(),
        request,
        &cert,
        &agent_key,
        tilo_clock::unix_now_ns(),
    )
    .map_err(|error| error.to_string())?;
    request.signed = Some(Box::new(invocation));
    tilo_server_protocol::invocation::sign_parts_now(request, &agent_key)
        .map_err(|error| error.to_string())?;
    Ok(())
}
