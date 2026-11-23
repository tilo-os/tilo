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
//! way every key their root certifies is ([`devices::keep`]): their device lists show it, marked
//! as an agent's, and revoke it as they revoke a device.
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

use crate::workspaces::Workspaces;
use crate::workspaces::person_workspace::devices;

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

    /// The certificate of `key`, a key [`Self::grant`] gave, and the key itself: what signs, as
    /// the agent, the invocations a connection carries to another device.
    fn made(&self, key: &[u8; 32]) -> Option<(DeviceCert, SigningKey)>;

    /// Keep every key [`Self::grant`] gives from now on in the record of the person's keys that
    /// `record` holds, on the runtime this is called on. The first record stands; nothing by
    /// default.
    fn keep_in(&self, _record: &Workspaces) {}

    /// Keep `cert`, a key the person's root certified on this device for an agent, among their
    /// keys where [`Self::keep_in`] named a record, once it does. Nothing by default.
    fn keep_key(&self, _cert: &DeviceCert) {}

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

impl crate::workspaces::Workspaces {
    /// Sign `shortcut` as saved in `workspace` by `agent` for the person whose root is `root`,
    /// with the key this device derives for the agent from that root: what the agent saves acts
    /// as the agent, held to what its record bounds it to.
    ///
    /// # Errors
    /// The workspace's list of its agents names no active agent `agent` acting for that person,
    /// this device holds no identity store or not that person's root, or the key does not sign.
    pub async fn sign_shortcut_as_agent(
        &self,
        workspace: &str,
        agent: &str,
        root: &[u8; 32],
        shortcut: &mut tilo_automation::Shortcut,
    ) -> Result<(), String> {
        let id = WorkspaceId::parse(workspace).ok_or("not a workspace id")?;
        let list = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
        let kept = self
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
            .ok()
            .and_then(|person| person.root());
        if !kept.is_active() || person != Some(*root) {
            return Err("the agent does not act for that person here".to_owned());
        }
        let keys = FleetAgentKeys::from_env()
            .map_err(|error| format!("this device's identity store is not open: {error}"))?;
        keys.keep_in(self);
        let key = keys.grant(id, agent, &kept.name, *root)?;
        keys.sign_shortcut(id, &key, agent, shortcut)
    }
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
    /// Where each key made is kept among the person's keys, and the runtime that writes it.
    record: OnceLock<(Workspaces, tokio::runtime::Handle)>,
    /// The certificates made before a record was named, kept once it is.
    waiting: Mutex<Vec<DeviceCert>>,
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
            record: OnceLock::new(),
            waiting: Mutex::default(),
        }
    }

    /// Keep `cert` among its person's keys: once for each key this process makes, and again for
    /// each certificate that renews it, as soon as a record is known.
    fn keep(&self, cert: &DeviceCert) {
        let Some((workspaces, runtime)) = self.record.get() else {
            self.waiting.lock_recover().push(cert.clone());
            return;
        };
        let (workspaces, cert) = (workspaces.clone(), cert.clone());
        runtime.spawn(async move {
            if let Err(error) = devices::keep(&workspaces, &cert).await {
                tracing::warn!(%error, "an agent's key is not among its person's keys");
            }
        });
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
        let signature = self.fleets.sign_root(identity, &digest).map_err(|error| {
            format!(
                "this computer cannot give the agent a key of its own: it does not hold its \
                 person's root here ({error}). Approve this computer's ticket on a device of \
                 theirs that holds the root (`tilo device add <ticket>`), then connect the agent \
                 again"
            )
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
        let fresh = lasting.is_none();
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
        let first = self
            .made
            .lock_recover()
            .insert(public, (cert.clone(), key))
            .is_none();
        if fresh || first {
            self.keep(&cert);
        }
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

    fn made(&self, key: &[u8; 32]) -> Option<(DeviceCert, SigningKey)> {
        self.made.lock_recover().get(key).cloned()
    }

    fn keep_in(&self, record: &Workspaces) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        if self.record.set((record.clone(), runtime)).is_ok() {
            let waiting = std::mem::take(&mut *self.waiting.lock_recover());
            for cert in &waiting {
                self.keep(cert);
            }
        }
    }

    fn keep_key(&self, cert: &DeviceCert) {
        self.keep(cert);
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

    /// The certificate this agent's key's person's root signed for it, and a signer that signs
    /// with the key itself: what a connection to another device signs its invocations as, so the
    /// device that answers checks them as this agent's.
    ///
    /// # Errors
    /// The key was not made on this device.
    pub fn signing(
        &self,
    ) -> Result<(DeviceCert, Arc<dyn collab_types::signer::DeviceSigner>), String> {
        let (cert, key) = self
            .keys
            .made(&self.key)
            .ok_or("the agent's key was not made on this device")?;
        Ok((cert, Arc::new(KeySigner(key))))
    }
}

/// One agent key this device made, as the signer of the invocations it sends.
struct KeySigner(SigningKey);

impl collab_types::signer::DeviceSigner for KeySigner {
    fn device_public_key(&self) -> [u8; 32] {
        self.0.verifying_key().to_bytes()
    }

    fn sign(&self, message: Vec<u8>) -> collab_types::signer::SignFuture<'_> {
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

    /// Copy `from`'s files to `to`, as a disk copy of a computer carries what
    /// its account's identity directory holds.
    fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_tree(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), &target).unwrap();
            }
        }
    }

    /// A person's own computer: the account's identity directory, carried over
    /// from the computer that made their root, still holds that root here, and
    /// this computer's device key is certified under it.
    fn a_computer_holding_its_persons_root(
        dir: &std::path::Path,
    ) -> (
        tilo_identity::device_fleet::DeviceFleetStore,
        String,
        [u8; 32],
    ) {
        let first = dir.join("first");
        let first_identity = first.join("config/tilo/identity");
        let first_paths = tilo_device_identity::IdentityPaths::under(first.join("system"));
        tilo_identity::provisioning::ensure_collaboration_identity(
            &first_identity,
            &first_paths,
            tilo_identity::provisioning::IdentityStorage::Files,
        )
        .unwrap();
        let record = tilo_identity::store::resolve_active_identity(&first_identity).unwrap();
        let root = tilo_identity::store::load_from_path(record).unwrap();
        let person = root.group_public_key();

        let here = dir.join("here");
        let identity_dir = here.join("config/tilo/identity");
        copy_tree(&first_identity, &identity_dir);
        let paths = tilo_device_identity::IdentityPaths::under(here.join("system"));
        let sign = |digest: &[u8]| {
            let digest: [u8; 32] = digest.try_into().map_err(|_| "not a digest".to_owned())?;
            root.sign(&digest).map_err(|error| error.to_string())
        };
        tilo_device_identity::persist::provision_joined_device(
            &paths,
            person,
            sign,
            "this computer",
            tilo_clock::unix_now_ns(),
        )
        .unwrap();
        // Its session's start takes up the identity this device is enrolled for, as a device's
        // session does before it lists what it holds.
        tilo_identity::provisioning::complete_enrolled_device(
            &identity_dir,
            &paths,
            tilo_clock::unix_now_ns(),
        )
        .unwrap();

        (
            tilo_identity::device_fleet::DeviceFleetStore::at(&identity_dir, paths),
            hex::encode(person),
            person,
        )
    }

    #[test]
    fn a_computer_that_holds_its_persons_root_gives_its_agents_keys_of_their_own() {
        let dir = tempfile::tempdir().unwrap();
        let (fleets, identity, person) = a_computer_holding_its_persons_root(dir.path());
        assert_eq!(
            fleets
                .sign_root(&identity, &[3; 32])
                .map_err(|error| error.to_string())
                .expect("the root this computer holds signs")
                .len(),
            64,
            "the person's root signs where the identity directory carries it"
        );

        let workspace = WorkspaceId::new();
        let keys = FleetAgentKeys::of(fleets);
        let key = keys.grant(workspace, "scout", "Scout", person).unwrap();
        let mut asked = request();
        keys.sign(workspace, &key, &mut asked).unwrap();
        let signer = tilo_server_protocol::invocation::verify_invocation(
            &workspace.to_string(),
            &asked,
            &tilo_user_identity::RevocationList::new(),
            tilo_clock::unix_now_ns(),
        )
        .expect("the agent's own key signed it");
        assert_eq!(signer.device_public_key, key);
        assert_eq!(signer.user_public_key, person);
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
            refused.contains("computer's ticket on a device of theirs that holds the root"),
            "{refused}"
        );
        let mut asked = request();
        assert!(keys.sign(workspace, &[7; 32], &mut asked).is_err());
        assert!(asked.signed.is_none());
    }

    /// A grant the person's fleet holds that is about to run out is certified again, under the
    /// same key, with a fresh lifetime; one that still has time left is kept as it is. This is
    /// how an agent keeps acting across a lifetime that passes, the way a member's device
    /// certificate is renewed.
    #[test]
    fn a_grant_nearing_its_expiry_is_certified_again_for_the_same_key() {
        let dir = tempfile::tempdir().unwrap();
        let (fleets, identity) = holding_a_root(dir.path());
        let person: [u8; 32] = hex::decode(&identity).unwrap().try_into().unwrap();
        let workspace = WorkspaceId::new();
        let key = FleetAgentKeys::of(fleets)
            .grant(workspace, "scribe", "Scribe", person)
            .unwrap();

        // The fleet keeps a certificate for that key with a day left, as a fleet does when a
        // lifetime passes.
        let (fleets, _) = holding_a_root(dir.path());
        let now = tilo_clock::unix_now_ns();
        let near = fleets
            .certify_agent_grant(
                &identity,
                key,
                AgentScope::agent_in(*workspace.as_uuid().as_bytes(), "scribe"),
                "Scribe",
                now,
                now.saturating_add(DAY_NS),
            )
            .unwrap();
        assert!(near.expires_at_ns.unwrap().saturating_sub(now) < RENEW_WITHIN_NS);

        // Granted again, the agent keeps its key and its person's root certifies it anew.
        let keys = FleetAgentKeys::of(fleets);
        assert_eq!(
            keys.grant(workspace, "scribe", "Scribe", person).unwrap(),
            key,
            "renewal keeps the key the agent signs with"
        );
        let (fleets, _) = holding_a_root(dir.path());
        let cert = fleets
            .load(&identity)
            .unwrap()
            .certificates
            .iter()
            .find(|cert| cert.device_public_key == key)
            .cloned()
            .expect("the person's devices list the agent's key");
        assert_ne!(cert, near, "the certificate the fleet keeps is a new one");
        assert!(
            cert.expires_at_ns > near.expires_at_ns,
            "the certificate's lifetime starts over"
        );
        assert!(
            cert.expires_at_ns.unwrap().saturating_sub(now) > RENEW_WITHIN_NS,
            "it has time left again"
        );

        // With time left, the same grant is kept: no certificate is written again.
        let issued = cert.issued_at_ns;
        assert_eq!(
            keys.grant(workspace, "scribe", "Scribe", person).unwrap(),
            key
        );
        let (fleets, _) = holding_a_root(dir.path());
        let kept = fleets
            .load(&identity)
            .unwrap()
            .certificates
            .iter()
            .find(|cert| cert.device_public_key == key)
            .cloned()
            .expect("the person's devices list the agent's key");
        assert_eq!(
            kept.issued_at_ns, issued,
            "a grant with time left is used as it is"
        );
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
