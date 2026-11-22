//! The keys an agent signs with on the person's device, one for each workspace it works in.
//!
//! On a host, an agent acts under the key its person approved for it there. On the person's device
//! it acts under a key of its own as well, never under the device's: every invocation of its turn
//! is signed by that key, and the key's certificate is an agent's grant the person's root signed,
//! reaching that one workspace and naming that one agent
//! ([`tilo_user_identity::AgentScope::Agent`]). Whoever reads a change it made reads it as the
//! agent's, for its person, and a revocation the person signs ends it as it ends a device.
//!
//! The key is derived, not kept: the person's root signs a statement naming the workspace and the
//! agent, and the key is made from that signature. A device holding the root makes the same key
//! every time, so nothing secret is written down, and a device without the root runs no turn of
//! the agent's: it cannot make the key, and it never signs in its place.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use ed25519_dalek::SigningKey;
use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, ConfirmedInvocation,
    InvocationReceipt, InvocationState, InvokeRequest, Pending,
};
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::{AgentScope, DeviceCert};

/// What the device's agents sign with: the key of each agent of each workspace, certified.
pub trait AgentKeys: Send + Sync {
    /// The public key `agent` signs with in `workspace`, certified by the root of `person` as a
    /// grant reaching that workspace and that agent alone, which the person's devices list as
    /// `name`.
    ///
    /// # Errors
    /// This device does not hold the root of `person`, the person revoked the key, or the fleet
    /// could not certify it.
    fn grant(
        &self,
        workspace: WorkspaceId,
        agent: &str,
        name: &str,
        person: [u8; 32],
    ) -> Result<[u8; 32], String>;

    /// Sign `request` in `workspace` with the agent key `key`, a key [`Self::grant`] gave.
    ///
    /// # Errors
    /// `key` is no key this device granted, or it does not sign.
    fn sign(
        &self,
        workspace: WorkspaceId,
        key: &[u8; 32],
        request: &mut InvokeRequest,
    ) -> Result<(), String>;
}

/// What separates the statement an agent's key is made from from anything else a root signs.
const KEY_DOMAIN: &[u8] = b"tilo-agent-device-key-v1\0";

/// How long a certified grant lasts before the device certifies it again.
const LIFETIME_NS: u64 = 30 * DAY_NS;
/// How much of a grant may be left when the device certifies it again.
const RENEW_WITHIN_NS: u64 = 7 * DAY_NS;
const DAY_NS: u64 = 24 * 60 * 60 * 1_000_000_000;

/// The agents' keys a device makes from the roots its identity store holds, certified in the
/// people's fleets.
pub struct FleetAgentKeys {
    fleets: tilo_identity::device_fleet::DeviceFleetStore,
    /// The keys made so far, by their public key, with their certificates.
    made: Mutex<HashMap<[u8; 32], (DeviceCert, SigningKey)>>,
}

impl FleetAgentKeys {
    /// The keys this device makes from the identity store its environment names.
    ///
    /// # Errors
    /// The environment names no identity store.
    pub fn from_env() -> std::io::Result<Self> {
        Ok(Self::of(
            tilo_identity::device_fleet::DeviceFleetStore::from_env()?,
        ))
    }

    /// The keys made from the roots `fleets` holds.
    #[must_use]
    pub fn of(fleets: tilo_identity::device_fleet::DeviceFleetStore) -> Self {
        Self {
            fleets,
            made: Mutex::default(),
        }
    }

    /// The key of `agent` in `workspace`, made from `identity`'s root.
    fn key(
        &self,
        identity: &str,
        workspace: WorkspaceId,
        agent: &str,
    ) -> Result<SigningKey, String> {
        let mut statement = KEY_DOMAIN.to_vec();
        statement.extend_from_slice(workspace.as_uuid().as_bytes());
        statement.extend_from_slice(agent.as_bytes());
        let digest = *blake3::hash(&statement).as_bytes();
        let signature = self.fleets.sign_root(identity, &digest).map_err(|_| {
            "this computer does not hold its person's root, so it cannot give the agent a key of \
             its own: approve this computer's ticket on a device of theirs that holds the root \
             (`tilo device add <ticket>`), then connect the agent again"
                .to_owned()
        })?;
        Ok(SigningKey::from_bytes(blake3::hash(&signature).as_bytes()))
    }
}

impl AgentKeys for FleetAgentKeys {
    fn grant(
        &self,
        workspace: WorkspaceId,
        agent: &str,
        name: &str,
        person: [u8; 32],
    ) -> Result<[u8; 32], String> {
        let identity = hex::encode(person);
        let key = self.key(&identity, workspace, agent)?;
        let public = key.verifying_key().to_bytes();
        let now = tilo_clock::unix_now_ns();
        let fleet = self
            .fleets
            .load(&identity)
            .map_err(|error| format!("the person's devices could not be read here: {error}"))?;
        if fleet.is_revoked(&public) {
            return Err("the agent's person revoked its key".to_owned());
        }
        let lasting = fleet
            .certificates
            .iter()
            .filter(|cert| cert.device_public_key == public)
            .filter(|cert| {
                cert.expires_at_ns
                    .is_some_and(|expires| expires.saturating_sub(now) > RENEW_WITHIN_NS)
            })
            .max_by_key(|cert| cert.expires_at_ns)
            .cloned();
        let cert = match lasting {
            Some(cert) => cert,
            None => self
                .fleets
                .certify_agent_grant(
                    &identity,
                    public,
                    AgentScope::agent_in(*workspace.as_uuid().as_bytes(), agent),
                    name,
                    now,
                    now.saturating_add(LIFETIME_NS),
                )
                .map_err(|error| {
                    format!("the person's root did not certify the agent's key: {error}")
                })?,
        };
        self.made.lock_recover().insert(public, (cert, key));
        Ok(public)
    }

    fn sign(
        &self,
        workspace: WorkspaceId,
        key: &[u8; 32],
        request: &mut InvokeRequest,
    ) -> Result<(), String> {
        let (cert, agent_key) = self
            .made
            .lock_recover()
            .get(key)
            .cloned()
            .ok_or("the agent's key was not made on this device")?;
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
}

/// The keys this device's agents sign with, made from the identity store the process's
/// environment names: one for the process, so a key made once is held while it runs.
///
/// # Errors
/// The environment names no identity store.
pub fn this_device() -> Result<Arc<dyn AgentKeys>, String> {
    static KEYS: OnceLock<Arc<dyn AgentKeys>> = OnceLock::new();
    if let Some(keys) = KEYS.get() {
        return Ok(Arc::clone(keys));
    }
    let keys = FleetAgentKeys::from_env()
        .map_err(|error| format!("this device's identity store is not open: {error}"))?;
    Ok(Arc::clone(KEYS.get_or_init(|| Arc::new(keys))))
}

/// One agent's key in one workspace, as this device made it: what signs every call the agent
/// makes there.
#[derive(Clone)]
pub struct AgentSigner {
    keys: Arc<dyn AgentKeys>,
    workspace: WorkspaceId,
    key: [u8; 32],
}

impl AgentSigner {
    /// Agent `agent`, which its person's devices list as `name`, working for the person `person`
    /// (a person id) in `workspace`, under a key of its own from `keys`: what signs its calls, and
    /// the caller they go as. An MCP client is such an agent: its record keeps no bound of its
    /// own, so it acts with what its person's grant gives an agent.
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
            .held_to(Vec::new());
        Ok((
            Self {
                keys,
                workspace: id,
                key,
            },
            caller,
        ))
    }

    /// Sign `request` with the agent's key.
    ///
    /// # Errors
    /// The key does not sign.
    pub fn sign(&self, request: &mut InvokeRequest) -> Result<(), ActionCallError> {
        self.keys
            .sign(self.workspace, &self.key, request)
            .map_err(|reason| ActionCallError::Refused { reason })
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

    fn confirm(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Pending<Result<ConfirmedInvocation, ActionCallError>> {
        match self.signed(request.clone()) {
            Ok(request) => self.provider.confirm(caller, &request),
            Err(error) => Pending::ready(Err(error)),
        }
    }

    fn content_puts(&self) -> Option<Arc<dyn tilo_actions::remote::ContentPuts>> {
        self.provider.content_puts()
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

    fn readings(&self) -> Option<Arc<dyn tilo_actions::remote::Readings>> {
        self.provider.readings()
    }

    fn resource_changes(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceChanges>> {
        self.provider.resource_changes()
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::remote::{InvokeArgument, RemoteActionTarget};

    /// A device's identity store holding a person's root, and that person's identity id.
    fn holding_a_root(
        dir: &std::path::Path,
    ) -> (tilo_identity::device_fleet::DeviceFleetStore, String) {
        let identity_dir = dir.join("config/identity");
        let paths = tilo_device_identity::IdentityPaths::under(dir.join("system"));
        tilo_identity::provisioning::ensure_collaboration_identity(
            &identity_dir,
            &paths,
            tilo_identity::provisioning::IdentityStorage::Files,
        )
        .unwrap();
        let identity = tilo_identity::store::IdentityRegistryStore::at(&identity_dir)
            .load()
            .unwrap()
            .default_identity
            .unwrap();
        (
            tilo_identity::device_fleet::DeviceFleetStore::at(&identity_dir, paths),
            identity,
        )
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
        let dir = tempfile::tempdir().unwrap();
        let (fleets, identity) = holding_a_root(dir.path());
        let person: [u8; 32] = hex::decode(&identity).unwrap().try_into().unwrap();
        let workspace = WorkspaceId::new();
        let keys = FleetAgentKeys::of(fleets);

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
        let scope = signer.capabilities.agent.expect("an agent's grant");
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
    fn a_device_without_the_persons_root_makes_no_key_and_signs_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (fleets, _) = holding_a_root(dir.path());
        let keys = FleetAgentKeys::of(fleets);
        let workspace = WorkspaceId::new();
        let refused = keys
            .grant(workspace, "scribe", "Scribe", [7; 32])
            .unwrap_err();
        assert!(
            refused.contains(
                "approve this computer's ticket on a device of theirs that holds the root"
            ),
            "{refused}"
        );
        let mut asked = request();
        assert!(keys.sign(workspace, &[7; 32], &mut asked).is_err());
        assert!(asked.signed.is_none());
    }

    #[test]
    fn a_key_its_person_revoked_is_never_certified_again() {
        let dir = tempfile::tempdir().unwrap();
        let (fleets, identity) = holding_a_root(dir.path());
        let person: [u8; 32] = hex::decode(&identity).unwrap().try_into().unwrap();
        let workspace = WorkspaceId::new();
        let key = FleetAgentKeys::of(fleets)
            .grant(workspace, "scribe", "Scribe", person)
            .unwrap();
        let (fleets, _) = holding_a_root(dir.path());
        fleets
            .revoke(
                &identity,
                key,
                tilo_user_identity::RevocationReason::Retired,
                tilo_clock::unix_now_ns(),
            )
            .unwrap();
        let refused = FleetAgentKeys::of(fleets)
            .grant(workspace, "scribe", "Scribe", person)
            .unwrap_err();
        assert!(refused.contains("revoked"), "{refused}");
    }
}
