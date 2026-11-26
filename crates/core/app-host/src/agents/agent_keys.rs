//! The keys an agent signs with on the person's device, one for each workspace it works in.
//!
//! On a host, an agent acts under the key its person approved for it there. On the person's device
//! it acts under a key of its own as well, never under the device's: every invocation of its turn
//! is signed by that key, and the key's certificate is an agent's grant the person's root signed,
//! reaching that one workspace and naming that one agent
//! ([`tilo_user_identity::AgentScope::Agent`]). Whoever reads a change it made reads it as the
//! agent's, for its person, and a revocation the person signs ends it as it ends a device.
//!
//! Each key is an entry of the record of the person's keys in their own workspace, kept the one
//! way every key their root certifies is ([`tilo_workspace_service::workspaces::person_workspace::devices::keep`]):
//! their device lists show it, marked as an agent's, and revoke it as they revoke a device.
//!
//! The key is derived, not kept: this device's key signs a statement naming the workspace and the
//! agent, and the key is made from that signature. The device makes the same key every time, so
//! nothing secret is written down, and only the daemon its person's root is kept by certifies it
//! ([`AgentGrants`]), as it certifies every key of theirs.
//!
//! A device that does not hold the person's root runs the agent only where the person approved
//! it: it makes a key of its own for the agent and asks for it with a ticket, which the person
//! redeems on a device that holds their root ([`super::ticketed_keys`]). No key leaves the device
//! that made it; only the certificate travels.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use ed25519_dalek::SigningKey;
use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeRequest, Pending,
};
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::{AgentScope, DeviceCert, RevocationEntry};
use tilo_workspace_agent::Agent;

use tilo_workspace_service::workspaces::Workspaces;

/// What an agent whose key its person revoked is told, on the call that finds it and as its
/// connection ends.
pub const REVOKED: &str = "This agent's access was revoked: its person took its key back, so \
                           it can't act for them any more. Ask them to connect it again if they \
                           want it back.";

/// What the device's agents sign with: the key of each agent of each workspace, certified.
pub trait AgentKeys: Send + Sync {
    /// The public key `agent` signs with in `workspace`, certified by the root of `person` as a
    /// grant reaching that workspace and that agent alone, which the person's devices list as
    /// `name`.
    ///
    /// # Errors
    /// The person revoked the key, or the daemon that keeps their root did not certify it.
    fn grant(
        &self,
        workspace: WorkspaceId,
        agent: &str,
        name: &str,
        person: [u8; 32],
    ) -> Result<[u8; 32], String>;

    /// Sign `request` in `workspace` with the agent key `key`, a key [`Self::grant`] gave, while
    /// its person has not revoked it: every signing reads their revocations as they stand then.
    ///
    /// # Errors
    /// `key` is no key this device granted, its person revoked it ([`REVOKED`]), or it does not
    /// sign.
    fn sign(
        &self,
        workspace: WorkspaceId,
        key: &[u8; 32],
        request: &mut InvokeRequest,
    ) -> Result<(), String>;

    /// The certificate of `key`, a key [`Self::grant`] gave, and the key itself: what signs, as
    /// the agent, the invocations a connection carries to another device. `None` once its person
    /// revoked it.
    fn made(&self, key: &[u8; 32]) -> Option<(DeviceCert, SigningKey)>;

    /// The revocation the root of `person` signed for `key`, as their devices say now: `None`
    /// while the key acts. A key found revoked is forgotten here, so nothing signs with it
    /// again. Nothing is revoked by default.
    ///
    /// # Errors
    /// The person's revocations cannot be read here.
    fn revocation(
        &self,
        _person: &[u8; 32],
        _key: &[u8; 32],
    ) -> Result<Option<RevocationEntry>, String> {
        Ok(None)
    }

    /// The record of the person's keys this device holds, which some keys are kept in as they
    /// are granted. Nothing by default: the daemon that certifies a key keeps it there.
    fn keep_in(&self, _record: &Workspaces) {}

    /// Keep `cert`, a key the person's root certified on this device for an agent, among their
    /// keys where [`Self::keep_in`] named a record, once it does. Nothing by default.
    fn keep_key(&self, _cert: &DeviceCert) {}

    /// The public key `agent` signs with in `workspace`, as its entry in the workspace's agents
    /// record stands: one this device made and its person's root certified, or, where the person
    /// did not certify it here, the key this device asked to run it with once they approved it.
    ///
    /// # Errors
    /// As [`Self::grant`], and where the person has not approved this device's key for it.
    fn grant_for(&self, workspace: WorkspaceId, agent: &Agent) -> Result<[u8; 32], String> {
        let root = tilo_action_policy::PersonId::parse(&agent.acts_for)
            .ok()
            .and_then(|person| person.root())
            .ok_or("the agent belongs to no person")?;
        self.grant(workspace, &agent.id.0, &agent.name, root)
    }

    /// Sign `shortcut` as saved in `workspace` by `agent`, with the agent key `key`, a key
    /// [`Self::grant`] gave: what the agent saves acts as the agent.
    ///
    /// # Errors
    /// `key` is no key this device made, or it does not sign.
    fn sign_shortcut(
        &self,
        workspace: WorkspaceId,
        key: &[u8; 32],
        agent: &str,
        shortcut: &mut tilo_automation::Shortcut,
    ) -> Result<(), String> {
        let (cert, agent_key) = self
            .made(key)
            .ok_or("the agent's key was not made on this device")?;
        tilo_peer_state::workspace::sign_saved_shortcut_as_agent(
            &workspace.to_string(),
            shortcut,
            agent,
            &cert,
            &agent_key,
        )
    }
}

/// Sign `shortcut` as saved in `workspace` of `workspaces` by `agent` for the person whose root is `root`,
/// with the key this device derives for the agent from that root: what the agent saves acts
/// as the agent, held to what its record bounds it to.
///
/// # Errors
/// The workspace's list of its agents names no active agent `agent` acting for that person,
/// this device holds no identity store or not that person's root, or the key does not sign.
pub async fn sign_shortcut_as_agent(
    workspaces: &tilo_workspace_service::workspaces::Workspaces,
    workspace: &str,
    agent: &str,
    root: &[u8; 32],
    shortcut: &mut tilo_automation::Shortcut,
) -> Result<(), String> {
    let id = WorkspaceId::parse(workspace).ok_or("not a workspace id")?;
    let list = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
    let kept = workspaces
        .read_resource::<tilo_workspace_agent::WorkspaceAgents>(
            workspace,
            (tilo_workspace_agent::OWNER, &list),
        )
        .await
        .map_err(|error| format!("the workspace's agents could not be read: {error}"))?
        .and_then(|agents| {
            agents
                .agents
                .find(&tilo_resource::Id::from_raw(agent.to_owned()))
                .cloned()
        })
        .ok_or("the workspace keeps no such agent")?;
    let person = tilo_action_policy::PersonId::parse(&kept.acts_for)
        .map_err(|error| format!("the agent acts for no person ({error})"))?
        .root();
    if !kept.is_active() || person != Some(*root) {
        return Err("the agent does not act for that person here".to_owned());
    }
    let keys = this_device().map_err(|error| format!("this device makes no agent key: {error}"))?;
    let key = keys.grant(id, agent, &kept.name, *root)?;
    keys.sign_shortcut(id, &key, agent, shortcut)
}

/// What separates the statement an agent's key is made from from anything else a root signs.
const KEY_DOMAIN: &[u8] = b"tilo-agent-device-key-v1\0";

/// How long a certified grant lasts before the device certifies it again.
#[cfg(test)]
pub(crate) const LIFETIME_NS: u64 = 30 * DAY_NS;
/// How much of a grant may be left when the device certifies it again.
pub(crate) const RENEW_WITHIN_NS: u64 = 7 * DAY_NS;
const DAY_NS: u64 = 24 * 60 * 60 * 1_000_000_000;

/// What certifies an agent's key with the person's root and says when they took one back: the
/// device's daemon, which keeps the root (`tilo.device#certifyKey`). A device that keeps none
/// hands the call on to one of theirs that does.
pub trait AgentGrants: Send + Sync {
    /// The certificate the person's root gives `grant` for `agent` in `workspace`, named
    /// `name`; `None` where they took the key back.
    ///
    /// # Errors
    /// The person's root could not certify it here.
    fn certify(
        &self,
        person: [u8; 32],
        grant: [u8; 32],
        workspace: WorkspaceId,
        agent: &str,
        name: &str,
    ) -> Result<Option<DeviceCert>, String>;

    /// The revocation the root of `person` signed for `key`, as their devices say now.
    ///
    /// # Errors
    /// The person's revocations cannot be read here.
    fn revocation(
        &self,
        person: &[u8; 32],
        key: &[u8; 32],
    ) -> Result<Option<RevocationEntry>, String>;
}

static GRANTS: std::sync::RwLock<Option<Arc<dyn AgentGrants>>> = std::sync::RwLock::new(None);

/// Certify this process's agents' keys through `grants` from now on: the daemon installs the
/// root it keeps, which answers where it keeps none by handing the call on.
pub fn install_grants(grants: Arc<dyn AgentGrants>) {
    *GRANTS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(grants);
}

/// What certifies this process's agents' keys, when its host installed an answer.
#[must_use]
pub fn grants() -> Option<Arc<dyn AgentGrants>> {
    GRANTS
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// The agent keys this device makes: a key of its own for each agent in each workspace, made from
/// its device key the way the key a server asks with is, and certified with the agent's person's
/// root by the daemon that keeps it ([`AgentGrants`]).
pub struct DeviceMadeKeys {
    grants: Arc<dyn AgentGrants>,
    /// The keys made so far, by their public key, with their certificates.
    made: Mutex<HashMap<[u8; 32], (DeviceCert, SigningKey)>>,
}

impl DeviceMadeKeys {
    /// The keys certified through `grants`.
    #[must_use]
    pub fn of(grants: Arc<dyn AgentGrants>) -> Self {
        Self {
            grants,
            made: Mutex::default(),
        }
    }

    /// The key of `agent` in `workspace`, made from this device's key: the same key every time,
    /// and nothing secret written down. The device key is the one this daemon took, never read
    /// again.
    fn key(workspace: WorkspaceId, agent: &str) -> Result<SigningKey, String> {
        let paths = tilo_identity::provisioning::collaboration_identity_paths();
        let device = tilo_device_identity::persist::taken_device_key(&paths).map_err(|error| {
            format!("this computer has no device key to make an agent's key from ({error})")
        })?;
        let mut statement = KEY_DOMAIN.to_vec();
        statement.extend_from_slice(workspace.as_uuid().as_bytes());
        statement.extend_from_slice(agent.as_bytes());
        let digest = blake3::hash(&statement);
        let signature = ed25519_dalek::Signer::sign(&device, digest.as_bytes());
        Ok(SigningKey::from_bytes(
            blake3::hash(signature.to_bytes().as_slice()).as_bytes(),
        ))
    }

    /// The key `key` this device made and its certificate, while its person has not revoked it:
    /// their devices are asked as they stand now. A revoked key is forgotten here, so nothing
    /// signs with it again.
    fn current(&self, key: &[u8; 32]) -> Result<(DeviceCert, SigningKey), String> {
        let made = self
            .made
            .lock_recover()
            .get(key)
            .cloned()
            .ok_or("the agent's key was not made on this device")?;
        if self
            .grants
            .revocation(&made.0.user_public_key, key)?
            .is_some()
        {
            self.made.lock_recover().remove(key);
            return Err(REVOKED.to_owned());
        }
        Ok(made)
    }
}

impl AgentKeys for DeviceMadeKeys {
    fn grant(
        &self,
        workspace: WorkspaceId,
        agent: &str,
        name: &str,
        person: [u8; 32],
    ) -> Result<[u8; 32], String> {
        let key = Self::key(workspace, agent)?;
        let public = key.verifying_key().to_bytes();
        // A key its person took back stays taken back: nothing certifies it again.
        if self.grants.revocation(&person, &public)?.is_some() {
            self.made.lock_recover().remove(&public);
            return Err(REVOKED.to_owned());
        }
        let Some(cert) = self
            .grants
            .certify(person, public, workspace, agent, name)?
        else {
            self.made.lock_recover().remove(&public);
            return Err(REVOKED.to_owned());
        };
        tilo_user_identity::verify_device_cert_at(&cert, tilo_clock::unix_now_ns()).map_err(
            |error| format!("the person's root did not certify the agent's key: {error}"),
        )?;
        let scope = AgentScope::agent_in(*workspace.as_uuid().as_bytes(), agent);
        if cert.user_public_key != person
            || cert.device_public_key != public
            || cert.capabilities != tilo_user_identity::DeviceCapabilities::agent_grant(scope)
        {
            return Err(
                "the certificate is not the person's grant of this key to this agent".into(),
            );
        }
        self.made.lock_recover().insert(public, (cert, key));
        Ok(public)
    }

    fn sign(
        &self,
        workspace: WorkspaceId,
        key: &[u8; 32],
        request: &mut InvokeRequest,
    ) -> Result<(), String> {
        let (cert, agent_key) = self.current(key)?;
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

    fn made(&self, key: &[u8; 32]) -> Option<(DeviceCert, SigningKey)> {
        self.current(key).ok()
    }

    fn revocation(
        &self,
        person: &[u8; 32],
        key: &[u8; 32],
    ) -> Result<Option<RevocationEntry>, String> {
        let revoked = self.grants.revocation(person, key)?;
        if revoked.is_some() {
            self.made.lock_recover().remove(key);
        }
        Ok(revoked)
    }
}

/// Why no agent's key is certified in a process whose daemon installed no answer.
const NO_GRANTS: &str = "this computer's daemon keeps no identity to certify an agent's key with";

/// What this process's daemon installed last ([`install_grants`]), as each call finds it: the
/// daemon installs it anew for each part of the person's it starts, over that part's workspaces,
/// and the keys made for the process outlive the part they were first made in.
struct Installed;

impl AgentGrants for Installed {
    fn certify(
        &self,
        person: [u8; 32],
        grant: [u8; 32],
        workspace: WorkspaceId,
        agent: &str,
        name: &str,
    ) -> Result<Option<DeviceCert>, String> {
        grants()
            .ok_or(NO_GRANTS)?
            .certify(person, grant, workspace, agent, name)
    }

    fn revocation(
        &self,
        person: &[u8; 32],
        key: &[u8; 32],
    ) -> Result<Option<RevocationEntry>, String> {
        grants().ok_or(NO_GRANTS)?.revocation(person, key)
    }
}

/// The keys this device's agents sign with, made as this device's own and certified through what
/// its daemon installed: one for the process, so a key made once is held while it runs.
///
/// # Errors
/// This process's daemon installed no answer: it keeps no root here, and none of the person's.
pub fn this_device() -> Result<Arc<dyn AgentKeys>, String> {
    static KEYS: OnceLock<Arc<dyn AgentKeys>> = OnceLock::new();
    if let Some(keys) = KEYS.get() {
        return Ok(Arc::clone(keys));
    }
    grants().ok_or_else(|| NO_GRANTS.to_owned())?;
    Ok(Arc::clone(KEYS.get_or_init(|| {
        Arc::new(DeviceMadeKeys::of(Arc::new(Installed)))
    })))
}

/// One agent's key in one workspace, as this device made it: what signs every call the agent
/// makes there.
#[derive(Clone)]
pub struct AgentSigner {
    keys: Arc<dyn AgentKeys>,
    workspace: WorkspaceId,
    key: [u8; 32],
    /// The root of the agent's person, whose revocations end it.
    person: [u8; 32],
}

impl AgentSigner {
    /// Agent `agent`, which its person's devices list as `name`, working for the person `person`
    /// (a person id) in `workspace`, under a key of its own from `keys`: what signs its calls, and
    /// the caller they go as. An MCP client is such an agent: its record keeps no bound of its
    /// own, so it acts with what its person's grant gives an agent. The device that makes the
    /// key hands the run its session ([`Caller::session_handed`]), so the certificate's
    /// delegation reaches it.
    ///
    /// # Errors
    /// The workspace or the person is not one, or [`AgentKeys::grant`] refused the key.
    pub fn grant(
        keys: Arc<dyn AgentKeys>,
        workspace: &str,
        (agent, name): (&str, &str),
        person: &str,
    ) -> Result<(Self, Caller), String> {
        let id = WorkspaceId::parse(workspace).ok_or("the agent's workspace is no workspace id")?;
        let root = tilo_action_policy::PersonId::parse(person)
            .ok()
            .and_then(|person| person.root())
            .ok_or("the agent belongs to no person")?;
        let key = keys.grant(id, agent, name, root)?;
        let caller = Caller::delegated_agent(agent, root, key)
            .in_workspace(workspace)
            .named(name)
            .session_handed();
        Ok((
            Self {
                keys,
                workspace: id,
                key,
                person: root,
            },
            caller,
        ))
    }

    /// Why the agent acts no further, once its person revoked its key ([`REVOKED`]) or their
    /// revocations cannot be read: what ends every connection it has open, at its next call.
    #[must_use]
    pub fn ended(&self) -> Option<String> {
        match self.keys.revocation(&self.person, &self.key) {
            Ok(None) => None,
            Ok(Some(_)) => Some(REVOKED.to_owned()),
            Err(why) => Some(why),
        }
    }

    /// Sign `request` with the agent's key, while its person has not revoked it.
    ///
    /// # Errors
    /// The key is revoked ([`REVOKED`]), or it does not sign.
    pub fn sign(&self, request: &mut InvokeRequest) -> Result<(), ActionCallError> {
        self.keys
            .sign(self.workspace, &self.key, request)
            .map_err(|reason| ActionCallError::Refused { reason })
    }

    /// The certificate this agent's key's person's root signed for it, and a signer that signs
    /// with the key itself: what a connection to another device signs its invocations as, so the
    /// device that answers checks them as this agent's.
    ///
    /// # Errors
    /// The key was not made on this device, or its person revoked it.
    pub fn signing(
        &self,
    ) -> Result<(DeviceCert, Arc<dyn collab_types::signer::DeviceSigner>), String> {
        let (cert, key) = self
            .keys
            .made(&self.key)
            .ok_or("the agent's key was not made on this device, or its person revoked it")?;
        Ok((cert, Arc::new(KeySigner(key, self.clone()))))
    }
}

/// One agent key this device made, as the signer of the invocations it sends: it signs nothing
/// once the agent's person revoked the key, whatever connection carries it.
struct KeySigner(SigningKey, AgentSigner);

impl collab_types::signer::DeviceSigner for KeySigner {
    fn device_public_key(&self) -> [u8; 32] {
        self.0.verifying_key().to_bytes()
    }

    /// The connection to another device dials as the agent's key: the device that answers
    /// carries a frame only from the connection that names the key which signed it.
    fn endpoint_secret(&self) -> Option<[u8; 32]> {
        Some(self.0.to_bytes())
    }

    fn sign(&self, message: Vec<u8>) -> collab_types::signer::SignFuture<'_> {
        if let Some(why) = self.1.ended() {
            return Box::pin(std::future::ready(Err(collab_types::signer::SignError(
                why,
            ))));
        }
        let signature: ed25519_dalek::Signature =
            ed25519_dalek::Signer::sign(&self.0, message.as_slice());
        Box::pin(std::future::ready(Ok(signature.to_bytes())))
    }
}

/// `provider`, with every call asked of it signed by the agent's key, where an agent asks: what
/// an agent's tools call through, so nothing it asks goes under the device's key.
pub struct Signed<P> {
    provider: P,
    signer: Option<AgentSigner>,
}

impl<P> Signed<P> {
    #[must_use]
    pub const fn new(provider: P, signer: Option<AgentSigner>) -> Self {
        Self { provider, signer }
    }

    fn signed(&self, mut request: InvokeRequest) -> Result<InvokeRequest, ActionCallError> {
        if let Some(signer) = &self.signer {
            signer.sign(&mut request)?;
        }
        Ok(request)
    }
}

impl<P: ActionProvider> ActionProvider for Signed<P> {
    fn retire_execution(&self, caller: &Caller) {
        self.provider.retire_execution(caller);
    }

    fn published(&self) -> Vec<tilo_actions::ResourceSpec> {
        self.provider.published()
    }

    fn serves(&self, provider: &str) -> bool {
        self.provider.serves(provider)
    }

    fn introduced(&self, caller: &Caller, client: &str) {
        self.provider.introduced(caller, client);
    }

    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        self.provider.query(caller, query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        match self.signed(request) {
            Ok(request) => self.provider.invoke(caller, request),
            Err(error) => Pending::ready(Err(error)),
        }
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.provider.observe(caller, id)
    }

    fn observed(&self, caller: &Caller, id: &InvocationId, state: Option<&InvocationState>) {
        self.provider.observed(caller, id, state);
    }

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        self.provider.next_change(caller, id)
    }

    fn session_state(&self, caller: &Caller) -> Option<tilo_actions::remote::SessionState> {
        self.provider.session_state(caller)
    }

    fn session_state_changed(&self, caller: &Caller) -> Option<Pending<()>> {
        self.provider.session_state_changed(caller)
    }

    fn ended(&self, caller: &Caller) -> Option<String> {
        self.signer
            .as_ref()
            .and_then(AgentSigner::ended)
            .or_else(|| self.provider.ended(caller))
    }

    fn changed(
        &self,
        caller: &Caller,
        scope: tilo_actions::remote::WatchScope,
    ) -> Option<Pending<Result<(), ActionCallError>>> {
        self.provider.changed(caller, scope)
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        self.provider.cancel(caller, id)
    }

    fn content_puts(&self) -> Option<Arc<dyn tilo_actions::remote::ContentPuts>> {
        self.provider.content_puts()
    }

    fn content_puts_for(
        &self,
        target: Option<&tilo_actions::remote::RemoteActionTarget>,
    ) -> Option<Arc<dyn tilo_actions::remote::ContentPuts>> {
        self.provider.content_puts_for(target)
    }

    fn content_gets(&self) -> Option<Arc<dyn tilo_actions::remote::ContentGets>> {
        self.provider.content_gets()
    }

    fn here(&self, caller: &Caller) -> Option<Vec<tilo_actions::remote::Here>> {
        self.provider.here(caller)
    }

    fn resource_reads(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceReads>> {
        self.provider.resource_reads()
    }

    fn resource_search(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceSearch>> {
        self.provider.resource_search()
    }

    fn change_feed(&self) -> Option<Arc<dyn tilo_actions::remote::ChangeFeed>> {
        self.provider.change_feed()
    }

    fn installed_surface(&self, app_id: &str) -> Pending<Option<Vec<u8>>> {
        self.provider.installed_surface(app_id)
    }

    fn invocation(
        &self,
        address: &tilo_actions::ActionAddress,
        argument: Option<&str>,
    ) -> Option<tilo_actions::api::ActionDescriptor> {
        self.provider.invocation(address, argument)
    }

    fn run(
        &self,
        caller: &Caller,
        request: InvokeRequest,
        admitted: tilo_actions::remote::Admitted,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        match self.signed(request) {
            Ok(request) => self.provider.run(caller, request, admitted),
            Err(error) => Pending::ready(Err(error)),
        }
    }

    fn asks_first(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        admitted: &tilo_actions::remote::Admitted,
    ) -> Option<Pending<Result<(), ActionCallError>>> {
        match self.signed(request.clone()) {
            Ok(request) => self.provider.asks_first(caller, &request, admitted),
            Err(error) => Some(Pending::ready(Err(error))),
        }
    }

    fn watch(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<tilo_actions::remote::LiveView, String> {
        self.provider.watch(caller, request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::remote::{InvokeArgument, RemoteActionTarget};
    use tilo_user_identity::{DeviceCapabilities, RevocationReason, sign_device_cert};

    /// A person's root, held here in place of the daemon that keeps one: it certifies the keys its
    /// device makes for its agents, keeps what it certified, and takes a key back.
    struct TestRoot {
        root: SigningKey,
        certs: Mutex<HashMap<[u8; 32], DeviceCert>>,
        revoked: Mutex<Vec<RevocationEntry>>,
    }

    impl TestRoot {
        fn new() -> Self {
            Self {
                root: SigningKey::from_bytes(&[9; 32]),
                certs: Mutex::default(),
                revoked: Mutex::default(),
            }
        }

        fn person(&self) -> [u8; 32] {
            self.root.verifying_key().to_bytes()
        }

        /// Take `key` back, as the person does from their device list.
        fn revoke(&self, key: [u8; 32]) {
            let mut entry = RevocationEntry {
                user_public_key: self.person(),
                revoked_device_public_key: key,
                revoked_at_ns: tilo_clock::unix_now_ns(),
                reason: RevocationReason::Retired,
                signature: [0; 64],
            };
            tilo_user_identity::sign_revocation_entry(&mut entry, &self.root).unwrap();
            self.revoked.lock_recover().push(entry);
        }
    }

    impl AgentGrants for TestRoot {
        fn certify(
            &self,
            person: [u8; 32],
            grant: [u8; 32],
            workspace: WorkspaceId,
            agent: &str,
            name: &str,
        ) -> Result<Option<DeviceCert>, String> {
            if person != self.person() {
                return Err("this root made no key of that person's".into());
            }
            if self
                .revoked
                .lock_recover()
                .iter()
                .any(|entry| entry.revoked_device_public_key == grant)
            {
                return Ok(None);
            }
            let now = tilo_clock::unix_now_ns();
            // One that still has time left is answered as it is, as the daemon answers it.
            if let Some(cert) = self.certs.lock_recover().get(&grant).cloned()
                && cert
                    .expires_at_ns
                    .is_some_and(|expires| expires.saturating_sub(now) > RENEW_WITHIN_NS)
            {
                return Ok(Some(cert));
            }
            let scope = AgentScope::agent_in(*workspace.as_uuid().as_bytes(), agent);
            let mut cert = DeviceCert {
                user_public_key: person,
                device_public_key: grant,
                device_label: name.to_owned(),
                capabilities: DeviceCapabilities::agent_grant(scope),
                issued_at_ns: now,
                expires_at_ns: Some(now.saturating_add(LIFETIME_NS)),
                signature: [0; 64],
                issuer: None,
            };
            sign_device_cert(&mut cert, &self.root).map_err(|error| error.to_string())?;
            self.certs.lock_recover().insert(grant, cert.clone());
            Ok(Some(cert))
        }

        fn revocation(
            &self,
            person: &[u8; 32],
            key: &[u8; 32],
        ) -> Result<Option<RevocationEntry>, String> {
            Ok(self
                .revoked
                .lock_recover()
                .iter()
                .find(|entry| {
                    entry.user_public_key == *person && entry.revoked_device_public_key == *key
                })
                .cloned())
        }
    }

    /// A provisioned device the environment names, whose key this process took as its daemon
    /// does.
    fn device() -> crate::test_support::ProvisionedHome {
        crate::test_support::ProvisionedHome::new()
    }

    fn request() -> InvokeRequest {
        InvokeRequest {
            target: RemoteActionTarget {
                provider: "so.tilo.notes".into(),
                address: tilo_actions::api::ActionAddress::new("notes:1", "rename"),
            },
            argument: InvokeArgument::Text("Plans".into()),
            invocation: tilo_actions::InvocationId::new(),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        }
    }

    #[test]
    fn an_agent_signs_as_its_own_key_the_persons_root_certified_for_one_agent_in_one_workspace() {
        let _home = device();
        let root = Arc::new(TestRoot::new());
        let person = root.person();
        let workspace = WorkspaceId::new();
        let keys = DeviceMadeKeys::of(root);

        let key = keys.grant(workspace, "scribe", "Scribe", person).unwrap();
        assert_eq!(
            keys.grant(workspace, "scribe", "Scribe", person).unwrap(),
            key,
            "the same agent in the same workspace makes the same key"
        );
        let elsewhere = keys
            .grant(WorkspaceId::new(), "scribe", "Scribe", person)
            .unwrap();
        assert_ne!(elsewhere, key, "each workspace has a key of its own");
        let other = keys.grant(workspace, "critic", "Critic", person).unwrap();
        assert_ne!(other, key, "each agent has a key of its own");

        let mut asked = request();
        keys.sign(workspace, &key, &mut asked).unwrap();
        let signer = tilo_server_protocol::invocation::verify_invocation(
            &workspace.to_string(),
            &asked,
            &tilo_user_identity::RevocationList::new(),
            tilo_clock::unix_now_ns(),
        )
        .expect("it verifies as the agent's own signature");
        assert_eq!(
            signer.device_public_key, key,
            "signed by the agent's key, not the device's"
        );
        assert_eq!(
            signer.user_public_key, person,
            "certified by its person's root"
        );
        let scope = signer
            .capabilities
            .agent
            .as_ref()
            .expect("an agent's grant");
        assert_eq!(
            scope.workspace(),
            Some(*workspace.as_uuid().as_bytes()),
            "a grant reaching that one workspace"
        );
        assert!(scope.is_agent("scribe"), "certified for that agent");
        assert!(
            !scope.is_agent("critic"),
            "another agent of the same person cannot sign under it"
        );
        // Signed for one workspace, it is nobody's call in another.
        assert!(
            tilo_server_protocol::invocation::verify_invocation(
                &WorkspaceId::new().to_string(),
                &asked,
                &tilo_user_identity::RevocationList::new(),
                tilo_clock::unix_now_ns(),
            )
            .is_err()
        );
    }

    #[test]
    fn a_grant_nearing_its_expiry_is_certified_again_for_the_same_key() {
        let _home = device();
        let root = Arc::new(TestRoot::new());
        let person = root.person();
        let workspace = WorkspaceId::new();
        let key = DeviceMadeKeys::of(Arc::clone(&root) as Arc<dyn AgentGrants>)
            .grant(workspace, "scribe", "Scribe", person)
            .unwrap();
        let first = root.certs.lock_recover()[&key].clone();
        // What the daemon answers with time left: the certificate it kept.
        let keys = DeviceMadeKeys::of(root);
        assert_eq!(
            keys.grant(workspace, "scribe", "Scribe", person).unwrap(),
            key,
            "a grant with time left keeps the key and its certificate"
        );
        assert_eq!(keys.made(&key).unwrap().0, first);
    }

    #[test]
    fn a_key_its_person_revoked_is_never_certified_again() {
        let _home = device();
        let root = Arc::new(TestRoot::new());
        let person = root.person();
        let workspace = WorkspaceId::new();
        let keys = DeviceMadeKeys::of(Arc::clone(&root) as Arc<dyn AgentGrants>);
        let key = keys.grant(workspace, "scribe", "Scribe", person).unwrap();
        root.revoke(key);
        let refused = keys
            .grant(workspace, "scribe", "Scribe", person)
            .unwrap_err();
        assert_eq!(refused, REVOKED);
    }

    /// An agent mid-run whose person revokes its key stops at its next call: the key it was
    /// granted signs nothing more, and the connection is told why.
    #[test]
    fn a_run_whose_key_its_person_revokes_signs_nothing_from_its_next_call() {
        let _home = device();
        let root = Arc::new(TestRoot::new());
        let person = root.person();
        let workspace = WorkspaceId::new();
        let keys: Arc<dyn AgentKeys> =
            Arc::new(DeviceMadeKeys::of(Arc::clone(&root) as Arc<dyn AgentGrants>));
        let (signer, _) = AgentSigner::grant(
            Arc::clone(&keys),
            &workspace.to_string(),
            ("scribe", "Scribe"),
            &hex::encode(person),
        )
        .unwrap();
        let mut first = request();
        signer.sign(&mut first).unwrap();
        assert_eq!(signer.ended(), None, "the agent acts");
        let key = tilo_server_protocol::invocation::verify_invocation(
            &workspace.to_string(),
            &first,
            &tilo_user_identity::RevocationList::new(),
            tilo_clock::unix_now_ns(),
        )
        .unwrap()
        .device_public_key;

        // The person revokes the key while the run goes on.
        root.revoke(key);

        let mut next = request();
        let Err(ActionCallError::Refused { reason }) = signer.sign(&mut next) else {
            panic!("the next call is refused");
        };
        assert_eq!(reason, REVOKED);
        assert!(next.signed.is_none(), "nothing was signed");
        assert_eq!(signer.ended().as_deref(), Some(REVOKED));
        assert!(keys.made(&key).is_none(), "the key is forgotten here");
        assert!(signer.signing().is_err(), "no connection signs with it");
    }

    #[test]
    fn a_device_with_no_answer_for_the_key_signs_nothing() {
        let _home = device();
        let workspace = WorkspaceId::new();
        let root = Arc::new(TestRoot::new());
        // A person whose root this daemon does not keep: it certifies nothing of theirs.
        let keys = DeviceMadeKeys::of(root);
        assert!(keys.grant(workspace, "scribe", "Scribe", [7; 32]).is_err());
        let mut asked = request();
        assert!(keys.sign(workspace, &[7; 32], &mut asked).is_err());
        assert!(asked.signed.is_none());
    }
}
