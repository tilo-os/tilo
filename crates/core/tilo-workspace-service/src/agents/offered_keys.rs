//! The key a device offers for an agent whose person's root it does not hold, and the keys a
//! device's agents sign with when it may hold either kind.
//!
//! A server that keeps a workspace runs a member's agent only where that member let it. It makes
//! a key of its own for the agent, from its own device key, the way a person's device makes one
//! from their root ([`super::agent_keys::FleetAgentKeys`]): its device key signs a statement
//! naming the workspace and the agent, and the key is made from that signature, so nothing secret
//! is written down. It offers the key's public half in the agent's entry of the workspace's agents
//! record. The person approves it from a device that holds their root
//! (`workspace/agents#approve`), which certifies that key for that one agent in that one workspace
//! and writes the certificate beside the offer. The server signs as the agent only while that
//! certificate holds: it is checked whenever a key is granted and again at every signing, so a
//! certificate taken back, run out or revoked by the person's root signs nothing more.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ed25519_dalek::SigningKey;
use tilo_actions::remote::InvokeRequest;
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::{AgentScope, DeviceCapabilities, DeviceCert};
use tilo_workspace_agent::Agent;

use super::agent_keys::{AgentKeys, RENEW_WITHIN_NS};

/// What separates the statement a device's offered key for an agent is made from from anything
/// else its device key signs.
const OFFERED_DOMAIN: &[u8] = b"tilo-agent-offered-key-v1\0";

/// The keys a device offers for agents whose person's root it does not hold: one per agent per
/// workspace, made from the device's own key, and used once the agent's entry in the
/// workspace's agents record holds a certificate the agent's person's root issued for it that
/// is still valid and not revoked.
pub struct OfferedAgentKeys {
    workspaces: crate::workspaces::WeakWorkspaces,
    /// The keys used so far, by their public key, with their certificates.
    made: Mutex<HashMap<[u8; 32], (DeviceCert, SigningKey)>>,
}

impl OfferedAgentKeys {
    /// The keys `workspaces`' device offers.
    #[must_use]
    pub fn of(workspaces: &crate::workspaces::Workspaces) -> Self {
        Self {
            workspaces: workspaces.downgrade(),
            made: Mutex::default(),
        }
    }

    /// The key of `agent` in `workspace` that this device makes from its device key.
    fn key(
        workspaces: &crate::workspaces::Workspaces,
        workspace: WorkspaceId,
        agent: &str,
    ) -> SigningKey {
        let mut statement = OFFERED_DOMAIN.to_vec();
        statement.extend_from_slice(workspace.as_uuid().as_bytes());
        statement.extend_from_slice(agent.as_bytes());
        workspaces.derived_device_key(&statement)
    }
}

impl AgentKeys for OfferedAgentKeys {
    fn grant(&self, _: WorkspaceId, _: &str, _: &str, _: [u8; 32]) -> Result<[u8; 32], String> {
        Err("this device signs as an agent only with a key its person let it use here".to_owned())
    }

    fn grant_for(&self, workspace: WorkspaceId, agent: &Agent) -> Result<[u8; 32], String> {
        let workspaces = self
            .workspaces
            .upgrade()
            .ok_or("this device's workspaces stopped")?;
        let device = hex::encode(workspaces.serving_certificate().device_public_key);
        let key = Self::key(&workspaces, workspace, &agent.id.0);
        let public = key.verifying_key().to_bytes();
        let runs = agent
            .runs_on
            .iter()
            .find(|runs| runs.device == device)
            .ok_or("this device has not offered to run the agent")?;
        if runs.key != hex::encode(public) {
            return Err("the key this device offered for the agent is not the one it makes".into());
        }
        let certificate = runs
            .certificate
            .as_deref()
            .ok_or("the agent's person has not let this device run it")?;
        let cert = decode_certificate(certificate)?;
        let root = tilo_action_policy::PersonId::parse(&agent.acts_for)
            .ok()
            .and_then(|person| person.root())
            .ok_or("the agent belongs to no person")?;
        let scope = AgentScope::agent_in(*workspace.as_uuid().as_bytes(), &agent.id.0);
        if cert.user_public_key != root
            || cert.device_public_key != public
            || cert.capabilities != DeviceCapabilities::agent_grant(scope)
        {
            return Err("the certificate is not the agent's person's grant of this key".into());
        }
        holds(&cert, &workspaces)?;
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

    fn offered(&self, workspace: WorkspaceId, agent: &str) -> Option<[u8; 32]> {
        let workspaces = self.workspaces.upgrade()?;
        Some(
            Self::key(&workspaces, workspace, agent)
                .verifying_key()
                .to_bytes(),
        )
    }
}

/// The keys a device's agents sign with: made from the roots its identity store holds, where it
/// holds the agent's person's, and otherwise the key it offered for the agent, once its person
/// certified it.
pub struct DeviceAgentKeys {
    roots: Option<Arc<dyn AgentKeys>>,
    offered: OfferedAgentKeys,
}

impl DeviceAgentKeys {
    /// The keys of `workspaces`' device, made from the roots `roots` holds where it has an
    /// identity store.
    #[must_use]
    pub fn of(
        workspaces: &crate::workspaces::Workspaces,
        roots: Option<Arc<dyn AgentKeys>>,
    ) -> Self {
        Self {
            roots,
            offered: OfferedAgentKeys::of(workspaces),
        }
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

    fn grant_for(&self, workspace: WorkspaceId, agent: &Agent) -> Result<[u8; 32], String> {
        let from_root = match &self.roots {
            Some(roots) => match roots.grant_for(workspace, agent) {
                Ok(key) => return Ok(key),
                Err(error) => Some(error),
            },
            None => None,
        };
        self.offered
            .grant_for(workspace, agent)
            .map_err(|offered| match from_root {
                Some(from_root) => format!(
                    "{from_root}; nor does it run the agent where its person let it: {offered}"
                ),
                None => offered,
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
            _ => self.offered.sign(workspace, key, request),
        }
    }

    fn made(&self, key: &[u8; 32]) -> Option<(DeviceCert, SigningKey)> {
        self.roots
            .as_ref()
            .and_then(|roots| roots.made(key))
            .or_else(|| self.offered.made(key))
    }

    fn revocation(
        &self,
        person: &[u8; 32],
        key: &[u8; 32],
    ) -> Result<Option<tilo_user_identity::RevocationEntry>, String> {
        match &self.roots {
            Some(roots) => roots.revocation(person, key),
            None => Ok(None),
        }
    }

    fn certify(
        &self,
        workspace: WorkspaceId,
        agent: &str,
        name: &str,
        person: [u8; 32],
        key: [u8; 32],
    ) -> Result<DeviceCert, String> {
        self.roots
            .as_ref()
            .ok_or(
                "this device holds no identity store, so it holds no root to certify a key with",
            )?
            .certify(workspace, agent, name, person, key)
    }

    fn offered(&self, workspace: WorkspaceId, agent: &str) -> Option<[u8; 32]> {
        self.offered.offered(workspace, agent)
    }

    fn keep_in(&self, record: &crate::workspaces::Workspaces) {
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

/// Whether `cert` holds now on `workspaces`' device: it verifies, has not run out, and no
/// revocation the device holds from the person's root refuses it.
fn holds(cert: &DeviceCert, workspaces: &crate::workspaces::Workspaces) -> Result<(), String> {
    tilo_user_identity::verify_device_cert_at(cert, tilo_clock::unix_now_ns())
        .map_err(|error| format!("the certificate of the key does not hold: {error}"))?;
    if workspaces.revocations().refuses(cert) {
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

/// Whether the certificate `text` holds runs out soon enough to be certified again, as a grant
/// the person's own device makes is ([`RENEW_WITHIN_NS`]).
#[must_use]
pub fn renews(text: &str) -> bool {
    decode_certificate(text).is_ok_and(|cert| {
        cert.expires_at_ns.is_some_and(|expires| {
            expires.saturating_sub(tilo_clock::unix_now_ns()) <= RENEW_WITHIN_NS
        })
    })
}

/// Sign `request` in `workspace` with `made`, a key this device made and its certificate.
fn sign_with(
    made: Option<(DeviceCert, SigningKey)>,
    workspace: WorkspaceId,
    request: &mut InvokeRequest,
) -> Result<(), String> {
    let (cert, agent_key) = made.ok_or("the agent's key was not made on this device")?;
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
    Ok(())
}
