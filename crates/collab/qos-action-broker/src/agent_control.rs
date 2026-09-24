//! The native private agent provider shared by catalogue, invoke and read binding.
use launcher::managed::{ManagedRuntimeFacts, VerifiedManagedProcess, VerifiedManagedRuntime};
use qos_agent_control_protocol::{AgentBinding, AgentRequest, GrantAction, GrantOffer};
use qos_peer_state::owner_commit::{AgentIntentV2, AgentInvocationScopeV2, AgentProviderRouteV2};
use std::sync::Arc;
use tilo_relay::{client::CertifiedAction, peer_actions::ActionMessage};
use tilo_user_identity::DeviceCert;
use uuid::Uuid;

pub const APP_ID: &str = "so.tilo.agent";

/// Host-owned current publication snapshot, never accepted from a remote caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentProviderState {
    pub machine: String,
    pub room: Uuid,
    pub host: Uuid,
    pub host_device: [u8; 32],
    pub app_peer: u64,
    pub governance_revision: String,
    pub publication_revision: String,
    pub grant_offers: Vec<GrantOffer>,
    pub private_v2_ready: bool,
    pub required_services_ready: bool,
}

/// Installed by the native host. The snapshot reads current live publications;
/// authorization checks current person-owner permission and retained parent tasks.
/// Snapshot must not acquire the Store write lock: admission revalidation is also
/// used by the Store while validating its candidate transaction.
pub trait AgentProviderAuthority: Send + Sync {
    /// Exact immutable grants of the retained same-caller Spawn; never today's offer.
    fn parent_grants(&self, caller: &DeviceCert, agent: &str) -> Result<Vec<GrantAction>, String>;
    fn parent_process(
        &self,
        caller: &DeviceCert,
        agent: &str,
    ) -> Result<VerifiedManagedProcess, String>;
    fn snapshot(&self) -> Result<AgentProviderState, String>;
    /// Message must revalidate every inherited grant against current policy and
    /// publications. Any revoked grant rejects the whole request, without shrinking it.
    fn authorize(
        &self,
        caller: &DeviceCert,
        request: &AgentRequest,
    ) -> Result<Vec<GrantAction>, String>;
}

pub struct AgentControlProvider {
    runtime: Option<VerifiedManagedRuntime>,
    authority: Arc<dyn AgentProviderAuthority>,
}

pub use tilo_relay::agent_reads::{AgentMutationBinding, AgentMutationRoute, AgentReadBinding};

/// No public/data constructor: only verified certified InvokeBound can mint this.
pub struct VerifiedAgentAdmission {
    invocation: Uuid,
    intent: AgentIntentV2,
    provider: Arc<AgentControlProvider>,
    publication: AgentProviderState,
    runtime: ManagedRuntimeFacts,
    parent_process: Option<VerifiedManagedProcess>,
}
impl VerifiedAgentAdmission {
    pub fn invocation(&self) -> Uuid {
        self.invocation
    }
    pub fn intent(&self) -> &AgentIntentV2 {
        &self.intent
    }
    pub fn governance_revision(&self) -> &str {
        &self.publication.governance_revision
    }
    pub fn parent_process(&self) -> Option<&VerifiedManagedProcess> {
        self.parent_process.as_ref()
    }
    pub fn revalidate(&self) -> Result<(), String> {
        if let Some(process) = &self.parent_process {
            if process.facts()?.runtime != self.runtime {
                return Err("agent parent runtime changed".into());
            }
        }
        let (state, runtime) = self.provider.current()?;
        if state != self.publication || runtime != self.runtime {
            return Err("agent publication changed after admission verification".into());
        }
        Ok(())
    }
}

impl AgentControlProvider {
    pub fn new(
        runtime: VerifiedManagedRuntime,
        authority: Arc<dyn AgentProviderAuthority>,
    ) -> Arc<Self> {
        Arc::new(Self {
            runtime: Some(runtime),
            authority,
        })
    }
    /// Retained reads still require the installed current Store authority.
    /// This provider cannot mint mutation bindings or admission without a live runtime.
    pub fn new_read_only(authority: Arc<dyn AgentProviderAuthority>) -> Arc<Self> {
        Arc::new(Self {
            runtime: None,
            authority,
        })
    }
    fn current(&self) -> Result<(AgentProviderState, ManagedRuntimeFacts), String> {
        let state = self.authority.snapshot()?;
        let runtime = self
            .runtime
            .as_ref()
            .ok_or("agent mutation runtime unavailable")?
            .revalidate()?;
        validate_provider_state(&state, &runtime)?;
        if !state.private_v2_ready || !state.required_services_ready {
            return Err("agent mutation runtime unavailable".into());
        }
        Ok((state, runtime))
    }
    pub fn mutation_bindings(&self) -> Result<[AgentMutationBinding; 2], String> {
        let (state, runtime) = self.current()?;
        Ok([
            mutation_binding(&state, &runtime, "spawn")?,
            mutation_binding(&state, &runtime, "message")?,
        ])
    }
    pub fn catalogue_surface(&self) -> Result<serde_json::Value, String> {
        let (state, runtime) = self.current()?;
        Ok(
            serde_json::json!({"peer_id":state.app_peer.to_string(),"app_id":APP_ID,
            "cluster_id":state.room.to_string(),"record_id":"","live":true,
            "actions":[mutation_binding(&state,&runtime,"spawn")?.descriptor,
                       mutation_binding(&state,&runtime,"message")?.descriptor]}),
        )
    }
    pub fn read_binding(&self, caller: &DeviceCert) -> Result<AgentReadBinding, String> {
        self.authority
            .authorize(caller, &AgentRequest::Capabilities { version: 1 })?;
        let state = self.authority.snapshot()?;
        let bindings = self
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.revalidate().ok())
            .and_then(|runtime| {
                if !state.private_v2_ready
                    || !state.required_services_ready
                    || validate_provider_state(&state, &runtime).is_err()
                {
                    return None;
                }
                Some([
                    mutation_binding(&state, &runtime, "spawn").ok()?,
                    mutation_binding(&state, &runtime, "message").ok()?,
                ])
            });
        let binding = AgentReadBinding {
            action_host: state.host.to_string(),
            agent: AgentBinding {
                machine: state.machine,
                room: state.room.to_string(),
                device: hex::encode(caller.device_public_key),
                host: state.host.to_string(),
            },
            spawn: bindings.as_ref().map(|b| b[0].clone()),
            message: bindings.as_ref().map(|b| b[1].clone()),
        };
        if !tilo_relay::agent_reads::valid_binding(&binding, state.host) {
            return Err("invalid current agent read binding".into());
        }
        Ok(binding)
    }
    pub fn verify_invocation(
        self: &Arc<Self>,
        evidence: &CertifiedAction,
    ) -> Result<VerifiedAgentAdmission, String> {
        let (state, runtime) = self.current()?;
        let ActionMessage::InvokeBound {
            host,
            invocation,
            app_peer,
            app_id,
            record_id,
            resource,
            action,
            argument,
            confirmation,
            revision,
        } = &evidence.action().message
        else {
            return Err("agent control requires certified bound invocation".into());
        };
        if invocation.0.is_nil()
            || *host != state.host
            || evidence.action().room != state.room
            || evidence.action().target != state.host_device
            || confirmation.is_some()
        {
            return Err("agent host, room, recipient or confirmation mismatch".into());
        }
        let binding = mutation_binding(&state, &runtime, action)?;
        if *app_peer != binding.route.app_peer
            || app_id != APP_ID
            || !record_id.is_empty()
            || *resource != binding.route.resource
            || revision.as_deref() != Some(binding.route.revision.as_str())
        {
            return Err("agent invocation differs from current native descriptor".into());
        }
        let caller = evidence.certificate();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "clock unavailable")?
            .as_nanos()
            .min(u128::from(u64::MAX)) as u64;
        tilo_user_identity::verify_device_cert_at(caller, now)
            .map_err(|_| "agent caller certificate rejected")?;
        if caller.capabilities.can_act_as_host
            || evidence.action().source != caller.device_public_key
        {
            return Err("agent control requires original person device".into());
        }
        let bytes = serde_json::to_vec(argument.as_ref().ok_or("agent request missing")?)
            .map_err(|e| e.to_string())?;
        let request = decode_control_request(action, &bytes)?;
        if let AgentRequest::Spawn { grant_set_id, .. } = &request {
            if grant_set_id != &state.grant_offers[0].id {
                return Err("agent grant offer changed".into());
            }
        }
        let grants = self.authority.authorize(caller, &request)?;
        let parent_grants = if let AgentRequest::Message { agent_id, .. } = &request {
            Some(self.authority.parent_grants(caller, agent_id)?)
        } else {
            None
        };
        validate_control_grants(
            &request,
            &state.grant_offers[0],
            &grants,
            parent_grants.as_deref(),
        )?;
        let parent_process = if let AgentRequest::Message { agent_id, .. } = &request {
            let proof = self.authority.parent_process(caller, agent_id)?;
            let facts = proof.facts()?;
            if facts.binding.agent != *agent_id
                || facts.binding.room != state.room.to_string()
                || facts.runtime != runtime
            {
                return Err("agent message parent live custody mismatch".into());
            }
            Some(proof)
        } else {
            None
        };
        let request = String::from_utf8(
            qos_agent_control_protocol::canonical_request(&request).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let intent = AgentIntentV2 {
            scope: AgentInvocationScopeV2 {
                machine: state.machine.clone(),
                room: state.room.to_string(),
                host: state.host.to_string(),
                caller: caller.clone(),
            },
            route: AgentProviderRouteV2 {
                app_id: APP_ID.into(),
                app_peer: state.app_peer.to_string(),
                cluster_id: state.room.to_string(),
                record_id: String::new(),
                resource: binding.route.resource,
                resource_kind: "workspace.agents".into(),
                action: action.clone(),
            },
            descriptor_revision: binding.route.revision,
            request,
            grants,
        };
        let verified = VerifiedAgentAdmission {
            invocation: invocation.0,
            intent,
            provider: self.clone(),
            publication: state,
            runtime,
            parent_process,
        };
        verified.revalidate()?;
        Ok(verified)
    }
}

fn validate_provider_state(
    state: &AgentProviderState,
    runtime: &ManagedRuntimeFacts,
) -> Result<(), String> {
    let hash = |s: &str| {
        s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    if state.machine.is_empty()
        || state.room.is_nil()
        || state.host.is_nil()
        || state.app_peer == 0
        || runtime.package_id != APP_ID
        || state.grant_offers.len() != 1
        || !hash(&state.governance_revision)
        || !hash(&state.publication_revision)
        || !hash(&state.grant_offers[0].revision)
        || state.grant_offers[0].actions.is_empty()
        || state.grant_offers[0].actions.len() > 64
    {
        return Err("invalid current agent provider binding".into());
    }
    validate_grant_actions(&state.grant_offers[0].actions)
}
fn validate_grant_actions(grants: &[GrantAction]) -> Result<(), String> {
    if grants.is_empty() || grants.len() > 64 {
        return Err("invalid agent grant count".into());
    }
    let mut keys = std::collections::BTreeSet::new();
    for grant in grants {
        if [&grant.app_id, &grant.action].iter().any(|value| {
            value.is_empty() || value.len() > 128 || value.chars().any(char::is_control)
        }) || qos_agent_control_protocol::validate_grant_resource(&grant.resource).is_err()
            || grant.record_id.len() > 128
            || grant.record_id.chars().any(char::is_control)
            || !keys.insert((
                &grant.app_id,
                &grant.record_id,
                &grant.resource,
                &grant.action,
            ))
        {
            return Err("invalid or duplicate current agent grant".into());
        }
    }
    Ok(())
}
fn mutation_binding(
    state: &AgentProviderState,
    runtime: &ManagedRuntimeFacts,
    action: &str,
) -> Result<AgentMutationBinding, String> {
    validate_provider_state(state, runtime)?;
    if !state.private_v2_ready || !state.required_services_ready {
        return Err("agent runtime unavailable".into());
    }
    if !matches!(action, "spawn" | "message") {
        return Err("unsupported agent action".into());
    }
    let resource = format!("workspace:{}:agents", state.room);
    let contract = tilo_actions::ActionContract {
        permission: tilo_actions::PermissionRequirement::Admin,
        confirmation: tilo_actions::ConfirmationPolicy::None,
        retry: tilo_actions::RetryPolicy::AtMostOnce,
        execution: tilo_actions::ExecutionMode::Background { cancellable: false },
        result: tilo_actions::ResultSpec::Json {
            schema: serde_json::json!({"type":"object","additionalProperties":false,
                "required":["invocationId","state","agentId","taskId","error"],
                "properties":{"invocationId":{"type":"string"},"state":{"type":"string","enum":["accepted"]},
                    "agentId":{"type":"string"},"taskId":{"type":"string"},"error":{"type":"null"}}}),
        },
        ..Default::default()
    };
    let mut descriptor = serde_json::json!({"resource":resource,"action":action,"resource_kind":"workspace.agents",
        "resource_label":"Workspace agents","action_label":if action=="spawn" {"Start agent"} else {"Message agent"},
        "description":"Accept one private agent task","operation_kind":"effect","params":[],
        "contract":contract,"input":{"kind":"none"}});
    let canonical = serde_json::to_vec(&serde_json::json!({"version":1,"descriptor":descriptor,
        "machine":state.machine,"host_device":hex::encode(state.host_device),"room":state.room,"host":state.host,"app_peer":state.app_peer.to_string(),"app_id":APP_ID,
        "package_hash":runtime.package_hash,"executable_hash":runtime.executable_hash,
        "runtime_hash":runtime.runtime_hash,"runtime_epoch":runtime.runtime_epoch,
        "governance":state.governance_revision,"publications":state.publication_revision,"grant":state.grant_offers}))
        .map_err(|e| e.to_string())?;
    let revision = blake3::hash(&canonical).to_hex().to_string();
    descriptor["revision"] = revision.clone().into();
    Ok(AgentMutationBinding {
        route: AgentMutationRoute {
            app_peer: state.app_peer.to_string(),
            app_id: APP_ID.into(),
            record_id: String::new(),
            resource,
            action: action.into(),
            revision,
        },
        descriptor,
    })
}
fn decode_control_request(action: &str, bytes: &[u8]) -> Result<AgentRequest, String> {
    let request = qos_agent_control_protocol::decode_request(bytes).map_err(|e| e.to_string())?;
    if !matches!(
        (action, &request),
        ("spawn", AgentRequest::Spawn { .. }) | ("message", AgentRequest::Message { .. })
    ) {
        return Err("agent action and request kind disagree".into());
    }
    Ok(request)
}

fn validate_control_grants(
    request: &AgentRequest,
    offer: &GrantOffer,
    grants: &[GrantAction],
    parent: Option<&[GrantAction]>,
) -> Result<(), String> {
    let expected = match request {
        AgentRequest::Spawn { .. } => offer.actions.as_slice(),
        AgentRequest::Message { .. } => parent.ok_or("agent parent grants unavailable")?,
        _ => return Err("agent mutation grant request required".into()),
    };
    if grants != expected {
        return Err("agent grants differ from the exact authorized selection".into());
    }
    validate_grant_actions(grants)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn provider_data() -> (AgentProviderState, ManagedRuntimeFacts) {
        (
            AgentProviderState {
                machine: "machine".into(),
                room: Uuid::from_u128(1),
                host: Uuid::from_u128(2),
                host_device: [8; 32],
                app_peer: 71,
                governance_revision: "a".repeat(64),
                publication_revision: "b".repeat(64),
                grant_offers: vec![GrantOffer {
                    id: "grant-1".into(),
                    revision: "c".repeat(64),
                    expires_at: "2100-01-01T00:00:00Z".into(),
                    label: "Document actions".into(),
                    actions: vec![GrantAction {
                        app_id: "so.tilo.notes".into(),
                        record_id: "note-1".into(),
                        resource: "document".into(),
                        action: "insert".into(),
                    }],
                }],
                private_v2_ready: true,
                required_services_ready: true,
            },
            ManagedRuntimeFacts {
                runtime_epoch: Uuid::from_u128(3).to_string(),
                package_id: APP_ID.into(),
                package_hash: "d".repeat(64),
                executable_hash: "e".repeat(64),
                runtime_hash: "f".repeat(64),
            },
        )
    }
    struct ReadAuthority {
        state: std::sync::Mutex<Option<AgentProviderState>>,
        caller: DeviceCert,
        allowed: std::sync::atomic::AtomicBool,
    }
    impl AgentProviderAuthority for ReadAuthority {
        fn parent_grants(&self, _: &DeviceCert, _: &str) -> Result<Vec<GrantAction>, String> {
            Err("no parent".into())
        }
        fn parent_process(
            &self,
            _: &DeviceCert,
            _: &str,
        ) -> Result<VerifiedManagedProcess, String> {
            Err("no live parent".into())
        }
        fn snapshot(&self) -> Result<AgentProviderState, String> {
            self.state
                .lock()
                .unwrap()
                .clone()
                .ok_or("Store unavailable".into())
        }
        fn authorize(
            &self,
            caller: &DeviceCert,
            request: &AgentRequest,
        ) -> Result<Vec<GrantAction>, String> {
            if !self.allowed.load(std::sync::atomic::Ordering::SeqCst)
                || caller != &self.caller
                || !matches!(request, AgentRequest::Capabilities { version: 1 })
            {
                return Err("current caller denied".into());
            }
            Ok(vec![])
        }
    }
    fn read_authority() -> Arc<ReadAuthority> {
        let root = ed25519_dalek::SigningKey::from_bytes(&[67; 32]);
        let key = ed25519_dalek::SigningKey::from_bytes(&[68; 32]);
        let mut caller = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "reader".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::primary(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut caller, &root).unwrap();
        Arc::new(ReadAuthority {
            state: std::sync::Mutex::new(Some(provider_data().0)),
            caller,
            allowed: std::sync::atomic::AtomicBool::new(true),
        })
    }
    #[test]
    fn agent_control_read_only_preserves_current_reads_without_execution_prerequisites() {
        let authority = read_authority();
        let provider = AgentControlProvider::new_read_only(authority.clone());
        let binding = provider
            .read_binding(&authority.caller)
            .expect("current retained reads do not need runtime custody");
        assert!(binding.spawn.is_none() && binding.message.is_none());
        assert!(provider.mutation_bindings().is_err());
        assert!(provider.catalogue_surface().is_err());
        assert!(provider.current().is_err());
        {
            let mut slot = authority.state.lock().unwrap();
            let state = slot.as_mut().unwrap();
            state.private_v2_ready = false;
            state.required_services_ready = false;
            state.grant_offers.clear();
            state.app_peer = 0;
            state.host = Uuid::from_u128(99);
        }
        let current = provider.read_binding(&authority.caller).unwrap();
        assert_eq!(current.action_host, Uuid::from_u128(99).to_string());
        assert!(current.spawn.is_none() && current.message.is_none());
        let mut other = authority.caller.clone();
        other.device_label = "changed certificate".into();
        assert!(provider.read_binding(&other).is_err());
        authority
            .allowed
            .store(false, std::sync::atomic::Ordering::SeqCst);
        assert!(provider.read_binding(&authority.caller).is_err());
        authority
            .allowed
            .store(true, std::sync::atomic::Ordering::SeqCst);
        *authority.state.lock().unwrap() = None;
        assert!(provider.read_binding(&authority.caller).is_err());
    }
    #[test]
    fn agent_control_read_only_rejects_malformed_current_binding() {
        for field in [
            "empty-machine",
            "long-machine",
            "unicode-machine",
            "room",
            "host",
        ] {
            let authority = read_authority();
            {
                let mut slot = authority.state.lock().unwrap();
                let state = slot.as_mut().unwrap();
                match field {
                    "empty-machine" => state.machine.clear(),
                    "long-machine" => state.machine = "m".repeat(129),
                    "unicode-machine" => state.machine = "é".into(),
                    "room" => state.room = Uuid::nil(),
                    _ => state.host = Uuid::nil(),
                }
            }
            assert!(
                AgentControlProvider::new_read_only(authority.clone())
                    .read_binding(&authority.caller)
                    .is_err(),
                "{field}"
            );
        }
    }
    #[test]
    fn agent_control_grant_resource_preserves_sdk_alias_bound_only() {
        let (state, _) = provider_data();
        let mut grant = state.grant_offers[0].actions[0].clone();
        grant.resource = "r".repeat(4096);
        assert!(
            validate_grant_actions(&[grant.clone()]).is_ok(),
            "actual SDK resource byte bound"
        );
        assert!(
            validate_grant_actions(&[grant.clone(), grant.clone()]).is_err(),
            "duplicates still reject"
        );
        for invalid in [
            "r".repeat(4097),
            " x".into(),
            "x\n".into(),
            "é".into(),
            String::new(),
        ] {
            let mut bad = grant.clone();
            bad.resource = invalid;
            assert!(validate_grant_actions(&[bad]).is_err());
        }
        for field in ["app", "record", "action"] {
            let mut bad = grant.clone();
            match field {
                "app" => bad.app_id = "a".repeat(129),
                "record" => bad.record_id = "a".repeat(129),
                _ => bad.action = "a".repeat(129),
            };
            assert!(validate_grant_actions(&[bad]).is_err(), "{field}");
        }
    }
    #[test]
    fn agent_control_descriptor_binds_current_provider_runtime_and_publications() {
        let (state, runtime) = provider_data();
        let spawn = mutation_binding(&state, &runtime, "spawn").unwrap();
        assert_eq!(spawn.route.app_id, "so.tilo.agent");
        assert_eq!(spawn.route.app_peer, "71");
        assert_eq!(
            spawn.route.resource,
            "workspace:00000000-0000-0000-0000-000000000001:agents"
        );
        assert_eq!(spawn.descriptor["contract"]["confirmation"]["mode"], "none");
        assert_eq!(spawn.descriptor["contract"]["retry"], "at_most_once");
        assert_eq!(
            spawn.descriptor["contract"]["execution"]["cancellable"],
            false
        );
        assert_eq!(spawn.descriptor["operation_kind"], "effect");
        assert_eq!(spawn.descriptor["revision"], spawn.route.revision);
        assert_ne!(
            spawn.route.revision,
            mutation_binding(&state, &runtime, "message")
                .unwrap()
                .route
                .revision
        );
        for field in ["governance", "publications", "peer", "host_device", "grant"] {
            let mut changed = state.clone();
            match field {
                "governance" => changed.governance_revision = "b".repeat(64),
                "publications" => changed.publication_revision = "c".repeat(64),
                "peer" => changed.app_peer = 72,
                "host_device" => changed.host_device = [9; 32],
                _ => changed.grant_offers[0].actions[0].record_id = "note-2".into(),
            }
            assert_ne!(
                spawn.route.revision,
                mutation_binding(&changed, &runtime, "spawn")
                    .unwrap()
                    .route
                    .revision,
                "{field}"
            );
        }
        let mut changed = runtime.clone();
        changed.runtime_epoch = Uuid::from_u128(4).to_string();
        assert_ne!(
            spawn.route.revision,
            mutation_binding(&state, &changed, "spawn")
                .unwrap()
                .route
                .revision
        );
        assert!(mutation_binding(&state, &runtime, "pause").is_err());
    }
    #[test]
    fn agent_control_no_mutation_without_one_current_offer_and_services() {
        let (mut state, runtime) = provider_data();
        state.private_v2_ready = false;
        assert!(mutation_binding(&state, &runtime, "spawn").is_err());
        state.private_v2_ready = true;
        state.required_services_ready = false;
        assert!(mutation_binding(&state, &runtime, "spawn").is_err());
        state.required_services_ready = true;
        state.grant_offers.push(state.grant_offers[0].clone());
        assert!(mutation_binding(&state, &runtime, "spawn").is_err());
        state.grant_offers.clear();
        assert!(mutation_binding(&state, &runtime, "spawn").is_err());
    }

    #[test]
    fn agent_control_rejects_action_kind_disagreement() {
        assert!(
            decode_control_request("spawn", br#"{"kind":"agent_capabilities","version":1}"#)
                .is_err()
        );
        assert!(decode_control_request("message", br#"{"kind":"agent_spawn","version":1,"name":"A","task":"Task","grant_set_id":"grant"}"#).is_err());
    }
    #[test]
    fn agent_control_rejects_unknown_fields() {
        assert!(decode_control_request("spawn", br#"{"kind":"agent_spawn","version":1,"name":"A","task":"Task","grant_set_id":"grant","extra":true}"#).is_err());
    }

    #[test]
    fn agent_control_message_preserves_parent_grants_when_spawn_offer_expands() {
        let (mut state, _) = provider_data();
        let parent = state.grant_offers[0].actions.clone();
        let mut added = parent[0].clone();
        added.action = "delete".into();
        state.grant_offers[0].actions.push(added);
        let request = AgentRequest::Message {
            version: 1,
            agent_id: "agent-existing".into(),
            text: "Continue".into(),
        };
        assert!(
            validate_control_grants(&request, &state.grant_offers[0], &parent, Some(&parent))
                .is_ok(),
            "current [A,B] offer must not prevent unchanged parent [A]"
        );
        assert!(
            validate_control_grants(&request, &state.grant_offers[0], &[], Some(&parent)).is_err(),
            "revoked A cannot be silently removed"
        );
        assert!(
            validate_control_grants(
                &request,
                &state.grant_offers[0],
                &state.grant_offers[0].actions,
                Some(&parent)
            )
            .is_err(),
            "new offer cannot broaden immutable parent grants"
        );
        assert!(validate_control_grants(&request, &state.grant_offers[0], &parent, None).is_err());
    }
}
