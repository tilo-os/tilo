//! Certificate-authenticated MODEL RPC, separate from application actions.
use super::{
    AdmittedJoinIdentity, CapnpBrokerState, InvocationOwner, PendingTaskRun, native_media,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak, atomic::Ordering};
#[path = "native_model_receipt.rs"]
mod receipt;
use crate::native_model::{
    NativeModelCloseCallback, NativeModelConnection, NativeModelContext, NativeModelContract,
    NativeModelLease,
};
use tilo_model_gateway::{GatewaySource, NativeGatewaySource, NativeModelDispatch, Request};

#[derive(serde::Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Command {
    Claim { run_id: u64, lease: String },
    ClaimInvocation { invocation_id: u64 },
    Execute { request: Request },
}

pub(super) struct Claim {
    pub(super) peer: u64,
    agent: String,
    lease: Arc<Lease>,
    dispatch: Arc<dyn NativeModelDispatch>,
}

/// Delivered authority outlives individual MODEL attempts. Retain each claim's
/// listeners until the delivered run or connection ends, not until Execute ends.
/// Both retained claims and connection identities are bounded.
#[derive(Default)]
pub(super) struct Leases {
    identities: HashMap<(u64, u64, u64), NativeModelConnection>,
    active: Vec<Arc<Lease>>,
    receipts: HashMap<(u64, String, u64, u64), receipt::Receipt>,
}

impl CapnpBrokerState {
    fn close_model_leases(&self, predicate: impl Fn(&Lease) -> bool) {
        let leases = {
            let registry = self
                .native_model_leases
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // Keep clones until after unlocking: the last Arc's drop can
            // deliver callbacks and must never run under the registry lock.
            registry.active.clone()
        };
        for lease in &leases {
            if predicate(lease) {
                lease.close();
            }
        }
        {
            let mut registry = self
                .native_model_leases
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry.active.retain(|lease| !lease.closed());
            let Leases {
                identities,
                active,
                receipts,
            } = &mut *registry;
            receipts.retain(|(peer, task, epoch, _), _| {
                active.iter().any(|lease| {
                    lease.peer == *peer
                        && lease.binding.task_id == *task
                        && lease.binding.epoch == *epoch
                })
            });
            identities.retain(|key, _| {
                active
                    .iter()
                    .any(|lease| *key == (lease.peer, lease.run_id, lease.binding.revision))
            });
        }
        // Removed leases can only drop here, outside the registry lock.
        drop(leases);
    }

    /// Fence a terminal/replaced owner run, including in-flight HTTP turns.
    /// Call after publishing the owner mutation, outside its mutation lock.
    pub fn revoke_native_model_run(&self, room: &str, task: &str, epoch: u64) {
        self.close_model_leases(|lease| {
            lease.room() == room && lease.binding.task_id == task && lease.binding.epoch == epoch
        });
    }

    pub(super) fn close_native_model_peer(&self, peer: u64) {
        self.close_model_leases(|lease| lease.peer == peer);
        self.native_model_leases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .identities
            .retain(|(owner, _, _), _| *owner != peer);
    }

    pub(super) fn close_native_models(&self) {
        self.close_model_leases(|_| true);
        self.native_model_leases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .identities
            .clear();
    }
}

struct Lease {
    peer: u64,
    connection: NativeModelConnection,
    local: Arc<native_media::LocalAppConnection>,
    callbacks: Mutex<Option<Vec<NativeModelCloseCallback>>>,
    state: Weak<CapnpBrokerState>,
    run_id: u64,
    run: PendingTaskRun,
    binding: crate::services::NativeTaskBinding,
    identity: AdmittedJoinIdentity,
    directed: Option<(u64, u64)>,
}
impl Lease {
    fn closed(&self) -> bool {
        self.callbacks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none()
    }

    fn close(&self) {
        let callbacks = self
            .callbacks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        for callback in callbacks.into_iter().flatten() {
            callback();
        }
    }

    fn live_with_view(
        &self,
        current: Option<(
            &tilo_joined_room::NativeCommittedView,
            std::time::SystemTime,
        )>,
    ) -> bool {
        if !self.local.current() || self.closed() {
            return false;
        }
        self.live_registered(current)
    }

    fn live_registered(
        &self,
        current: Option<(
            &tilo_joined_room::NativeCommittedView,
            std::time::SystemTime,
        )>,
    ) -> bool {
        let Some(state) = self.state.upgrade() else {
            return false;
        };
        if let Some((peer, invocation)) = self.directed {
            let Ok((id, run, binding)) = directed_run(&state, peer, invocation, current) else {
                return false;
            };
            if id != self.run_id
                || run.lease != self.run.lease
                || binding.revision != self.binding.revision
            {
                return false;
            }
        }
        state.accepting.load(Ordering::Acquire)
            && state.task_subscribers.lock().is_ok_and(|subscribers| {
                subscribers
                    .get(&self.run.peer_id)
                    .is_some_and(|subscription| {
                        subscription.agent_id == self.run.agent_id
                            && subscription.cluster_id == self.run.cluster_id
                    })
            })
            && state.pending_task_runs.lock().is_ok_and(|runs| {
                runs.get(&self.run_id).is_some_and(|run| {
                    !run.terminal
                        && run.peer_id == self.run.peer_id
                        && run.agent_id == self.run.agent_id
                        && run.lease == self.run.lease
                        && run.native.as_ref().is_some_and(|binding| {
                            binding.task_id == self.binding.task_id
                                && binding.agent_id == self.binding.agent_id
                                && binding.epoch == self.binding.epoch
                                && binding.starter == self.binding.starter
                        })
                })
            })
    }
}
// The registry retains a strong reference after an attempt finishes. Drop is
// only a final teardown fence, never an inference-completion notification.
impl Drop for Lease {
    fn drop(&mut self) {
        self.close();
    }
}

impl NativeModelLease for Lease {
    fn with_current(
        &self,
        view: &tilo_joined_room::NativeCommittedView,
        contract: NativeModelContract,
        now: std::time::SystemTime,
        enqueue: &mut dyn FnMut(NativeModelContext) -> Result<(), String>,
    ) -> Result<(), String> {
        // Owner -> connection -> delivered lease. close() releases its lease
        // gate before notifying listeners; neither path reenters the owner.
        self.local.with_current(|| {
            let callbacks = self
                .callbacks
                .lock()
                .map_err(|_| "MODEL lease gate unavailable")?;
            if callbacks.is_none() || !self.live_registered(Some((view, now))) {
                return Err("native MODEL connection or delivered lease ended".into());
            }
            let result = enqueue(self.certified_context(
                view,
                contract,
                now.max(std::time::SystemTime::now()),
            )?);
            drop(callbacks);
            result
        })
    }
    fn connection(&self) -> Option<NativeModelConnection> {
        Some(self.connection.clone())
    }
    fn on_close(&self, callback: NativeModelCloseCallback) -> Result<(), String> {
        let callback = {
            let mut callbacks = self
                .callbacks
                .lock()
                .map_err(|_| "native MODEL listeners unavailable")?;
            match callbacks.as_mut() {
                Some(callbacks) => {
                    callbacks.push(callback);
                    None
                }
                None => Some(callback),
            }
        };
        if let Some(callback) = callback {
            callback();
        }
        Ok(())
    }
    fn room(&self) -> &str {
        &self.run.cluster_id
    }
    fn binding(&self) -> &crate::services::NativeTaskBinding {
        &self.binding
    }
    fn live(&self) -> bool {
        self.live_with_view(None)
    }
    fn current(
        &self,
        view: &tilo_joined_room::NativeCommittedView,
        contract: NativeModelContract,
        now: std::time::SystemTime,
    ) -> Result<NativeModelContext, String> {
        if !self.live_with_view(Some((view, now))) {
            return Err("native MODEL connection or delivered lease ended".into());
        }
        self.certified_context(view, contract, now)
    }
}

impl Lease {
    fn certified_context(
        &self,
        view: &tilo_joined_room::NativeCommittedView,
        contract: NativeModelContract,
        now: std::time::SystemTime,
    ) -> Result<NativeModelContext, String> {
        let certificate = self
            .identity
            .certificate
            .as_ref()
            .ok_or("native MODEL certified connection unavailable")?;
        view.peer()
            .permissions(certificate.device_public_key, now)?;
        if self.directed.is_none()
            && self.identity.agent_id.as_deref() != Some(self.binding.agent_id.as_str())
        {
            return Err("native MODEL certified actor changed".into());
        }
        crate::native_model::current_context(view, &self.binding, contract)
    }
}

/// No app-provided native flag exists: this session is created only after the
/// channel certificate's agent identity and broker-issued run have been checked.
struct Session {
    lease: Arc<Lease>,
    source: Option<NativeGatewaySource>,
}
impl tilo_model_gateway::caller::Session for Session {
    fn caller(&self) -> tilo_model_gateway::caller::Caller {
        tilo_model_gateway::caller::Caller::Agent
    }
    fn active(&self) -> bool {
        self.lease.live()
    }
    fn gateway_source(&self) -> GatewaySource<'_> {
        self.source
            .as_ref()
            .map_or(GatewaySource::NativeUnavailable, GatewaySource::Native)
    }
}

fn invocation(request: &Request) -> Result<uuid::Uuid, String> {
    let (Request::NativePlan {
        invocation: value, ..
    }
    | Request::NativeTransform {
        invocation: value, ..
    }
    | Request::NativeComplete {
        invocation: value, ..
    }) = request
    else {
        return Err("protected MODEL RPC refuses ordinary gateway requests".into());
    };
    serde_json::from_value(value.clone()).map_err(|_| "invalid native MODEL invocation".into())
}

/// Resolve a delivered child solely from the host's pending registry and durable
/// admission. The receiving connection cannot choose a task, actor or model grant.
fn directed_run(
    state: &CapnpBrokerState,
    peer: u64,
    invocation_id: u64,
    current: Option<(
        &tilo_joined_room::NativeCommittedView,
        std::time::SystemTime,
    )>,
) -> Result<(u64, PendingTaskRun, crate::services::NativeTaskBinding), String> {
    use crate::services::NativeTaskBindingActor as _;
    let pending = state
        .pending_invocations
        .lock()
        .map_err(|_| "native MODEL pending registry unavailable")?
        .get(&invocation_id)
        .cloned()
        .ok_or("native MODEL directed invocation absent")?;
    if pending.peer_id != peer
        || pending.cancellation_requested_at.is_some()
        || pending.cancellation_fence.load(Ordering::Acquire)
    {
        return Err("native MODEL directed invocation unavailable to this connection".into());
    }
    let InvocationOwner::NativeTask { agent, task, epoch } = &pending.owner else {
        return Err("native MODEL directed invocation has no native task custody".into());
    };
    let key = pending
        .journal_key
        .as_ref()
        .ok_or("native MODEL durable admission absent")?;
    let room = &pending.cluster_id;
    if key.owner != pending.owner || &key.room != room {
        return Err("native MODEL durable owner mismatch".into());
    }
    let requirement = directed_requirement(state, key, invocation_id)?;
    let binding = requirement
        .binding
        .clone()
        .ok_or("native MODEL task binding absent")?;
    if binding.agent_id != *agent
        || binding.task_id != *task
        || binding.epoch != *epoch
        || binding.actor()?.id() != pending.actor
        || requirement
            .invocation
            .as_ref()
            .is_none_or(|child| child.id != key.invocation.0)
    {
        return Err("native MODEL admitted actor or child mismatch".into());
    }
    let original = pending
        .original
        .as_ref()
        .ok_or("native MODEL original caller absent")?;
    if let Some((view, now)) = current {
        // Admission through membership would reenter the owner held by the
        // MODEL gate. Inspect this exact committed view instead.
        original.check_native_model_current_source(&requirement, view, now)?;
    } else {
        original.check_native_model_source(state, &requirement)?;
    }
    let runs = state
        .pending_task_runs
        .lock()
        .map_err(|_| "native MODEL runs unavailable")?;
    let mut matching = runs.iter().filter(|(_, run)| {
        !run.terminal
            && run.cluster_id == pending.cluster_id
            && run.native.as_ref().is_some_and(|native| {
                native.task_id == binding.task_id
                    && native.agent_id == binding.agent_id
                    && native.epoch == binding.epoch
                    && native.starter == binding.starter
            })
    });
    let (id, run) = matching
        .next()
        .ok_or("native MODEL source run no longer live")?;
    if matching.next().is_some() {
        return Err("native MODEL source run is ambiguous".into());
    }
    let result = (*id, run.clone(), binding);
    drop(runs);
    Ok(result)
}

fn directed_requirement(
    state: &CapnpBrokerState,
    key: &super::InvocationKey,
    invocation_id: u64,
) -> Result<crate::services::NativeTaskActionRequirement, String> {
    let guard = state
        .journal
        .lock()
        .map_err(|_| "native MODEL journal unavailable")?;
    let journal = guard
        .as_ref()
        .map_err(Clone::clone)?
        .as_ref()
        .ok_or("native MODEL journal absent")?;
    let record = journal
        .lookup(key)?
        .ok_or("native MODEL admission absent")?;
    if record.wire_id != invocation_id
        || record.uncertain
        || record.cancellation_requested
        || record.update.is_terminal()
    {
        return Err("native MODEL admission no longer live".into());
    }
    let requirement = serde_json::from_value(
        record
            .intent
            .get("native_requirement")
            .cloned()
            .ok_or("native MODEL exact child absent")?,
    )
    .map_err(|_| "native MODEL exact child invalid")?;
    drop(guard);
    Ok(requirement)
}

struct Delivery {
    run_id: u64,
    run: PendingTaskRun,
    binding: crate::services::NativeTaskBinding,
    directed: Option<(u64, u64)>,
}

pub(super) async fn command(
    state: Arc<CapnpBrokerState>,
    peer: u64,
    agent: String,
    identity: AdmittedJoinIdentity,
    local: Arc<native_media::LocalAppConnection>,
    command: Command,
) -> Result<Vec<u8>, String> {
    let protected = local
        .protected_model
        .as_ref()
        .ok_or("native MODEL protected launch absent")?;
    protected
        .validate(&local.app_id, Some(&local.workspace_id))
        .map_err(|error| error.to_string())?;
    if !local.current() || protected.agent != agent {
        return Err("native MODEL protected process or actor mismatch".into());
    }
    let delivery = match command {
        Command::Claim { run_id, lease } => {
            let run = state.task_run_for(peer, &agent, run_id, &lease)?;
            let mut binding = run
                .native
                .clone()
                .ok_or("ordinary run has no native MODEL custody")?;
            let (_, revision) = runtime_collab::native_task_revision(&lease)
                .ok_or("invalid native MODEL lease revision")?;
            binding.revision = revision;
            Delivery {
                run_id,
                run,
                binding,
                directed: None,
            }
        }
        Command::ClaimInvocation { invocation_id } => {
            let (run_id, run, binding) = directed_run(&state, peer, invocation_id, None)?;
            Delivery {
                run_id,
                run,
                binding,
                directed: Some((peer, invocation_id)),
            }
        }
        Command::Execute { request } => return execute_claim(&state, peer, &agent, request).await,
    };
    if protected.room != delivery.run.cluster_id {
        return Err("native MODEL protected launch room mismatch".into());
    }
    // Owner/journal reads and close callbacks must remain outside registry locks.
    state.close_model_leases(|lease| !lease.live());
    let lease = register_delivery(&state, peer, local, identity, delivery)?;
    claim_delivery(&state, peer, agent, lease).await
}

fn register_delivery(
    state: &Arc<CapnpBrokerState>,
    peer: u64,
    local: Arc<native_media::LocalAppConnection>,
    identity: AdmittedJoinIdentity,
    delivery: Delivery,
) -> Result<Arc<Lease>, String> {
    let mut registry = state
        .native_model_leases
        .lock()
        .map_err(|_| "native MODEL lease registry unavailable")?;
    let key = (peer, delivery.run_id, delivery.binding.revision);
    if registry.active.len() >= 1024
        || (!registry.identities.contains_key(&key) && registry.identities.len() >= 1024)
    {
        return Err("too many native MODEL delivered leases".into());
    }
    let connection = registry
        .identities
        .entry(key)
        .or_insert_with(|| NativeModelConnection {
            connection: local.model_connection,
            lease: uuid::Uuid::new_v4(),
        })
        .clone();
    let lease = Arc::new(Lease {
        peer,
        connection,
        local,
        callbacks: Mutex::new(Some(Vec::new())),
        state: Arc::downgrade(state),
        run_id: delivery.run_id,
        run: delivery.run,
        binding: delivery.binding,
        identity,
        directed: delivery.directed,
    });
    registry.active.push(lease.clone());
    drop(registry);
    Ok(lease)
}

async fn claim_delivery(
    state: &Arc<CapnpBrokerState>,
    peer: u64,
    agent: String,
    lease: Arc<Lease>,
) -> Result<Vec<u8>, String> {
    if !lease.live() {
        lease.close();
        state.close_model_leases(Lease::closed);
        return Err("native MODEL connection ended".into());
    }
    let host = state
        .services
        .native_model
        .clone()
        .ok_or("native MODEL host custody unavailable")?;
    let id = uuid::Uuid::new_v4();
    let claimed_lease = lease.clone();
    let dispatch = tokio::task::spawn_blocking(move || host.claim(claimed_lease, id))
        .await
        .map_err(|_| "native MODEL selection worker stopped")??;
    // Snapshot first: live() enters subscriber/run/journal and owner locks.
    let candidates = claim_candidates(state)?;
    let expired = candidates.into_iter().filter_map(|(id, lease)| {
        if lease.live() {
            None
        } else {
            lease.close();
            Some(id)
        }
    });
    // Eagerly finish every authority read before acquiring the claims registry.
    let mut expired_ids = Vec::new();
    expired_ids.extend(expired);
    let mut claims = state
        .native_model_claims
        .lock()
        .map_err(|_| "native MODEL registry unavailable")?;
    for expired in expired_ids {
        claims.remove(&expired);
    }
    if claims.len() >= 256 {
        return Err("too many pending native MODEL attempts".into());
    }
    claims.insert(
        id,
        Claim {
            peer,
            agent,
            lease,
            dispatch,
        },
    );
    drop(claims);
    encode_reply(&serde_json::to_value(id).map_err(|error| error.to_string())?)
}

fn claim_candidates(state: &CapnpBrokerState) -> Result<Vec<(uuid::Uuid, Arc<Lease>)>, String> {
    let claims = state
        .native_model_claims
        .lock()
        .map_err(|_| "native MODEL registry unavailable")?;
    let candidates = claims
        .iter()
        .map(|(id, claim)| (*id, claim.lease.clone()))
        .collect();
    drop(claims);
    Ok(candidates)
}

fn take_claim(
    state: &CapnpBrokerState,
    peer: u64,
    agent: &str,
    id: uuid::Uuid,
) -> Result<Claim, String> {
    let mut claims = state
        .native_model_claims
        .lock()
        .map_err(|_| "native MODEL registry unavailable")?;
    match claims.get(&id) {
        None => return Err("native MODEL invocation missing or consumed".into()),
        Some(claim) if claim.peer != peer || claim.agent != agent => {
            return Err("native MODEL invocation belongs to another connection".into());
        }
        Some(_) => {}
    }
    let claim = claims
        .remove(&id)
        .ok_or("native MODEL invocation disappeared")?;
    drop(claims);
    Ok(claim)
}

async fn execute_claim(
    state: &CapnpBrokerState,
    peer: u64,
    agent: &str,
    request: Request,
) -> Result<Vec<u8>, String> {
    let id = invocation(&request)?;
    let Claim {
        lease, dispatch, ..
    } = take_claim(state, peer, agent, id)?;
    if lease.directed.is_some() && !matches!(&request, Request::NativeTransform { .. }) {
        return Err("directed native MODEL custody permits only Transform".into());
    }
    // One attempt only. The LocalSet stays free for Stop while the owner/vault
    // worker waits; the actual HTTP transfer still requires its final fence.
    let session = Session {
        lease,
        source: Some(NativeGatewaySource::new(dispatch)?),
    };
    tokio::task::spawn_blocking(move || execute(session, request))
        .await
        .map_err(|_| "native MODEL host worker stopped; outcome unknown".to_string())?
}

fn execute(session: Session, request: Request) -> Result<Vec<u8>, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "native MODEL host runtime unavailable")?;
    runtime.block_on(async move {
        let gateway = tilo_model_gateway::service::Gateway::new()?;
        let receipt = receipt::Receipt::capture(&session.lease, &request)?;
        let mut events = Vec::new();
        let mut size = 0usize;
        let mut overflow = false;
        let response = gateway
            .execute(&session, request, &mut |event| {
                let Ok(value) = serde_json::to_value(event) else {
                    return false;
                };
                let Ok(bytes) = serde_json::to_vec(&value) else {
                    return false;
                };
                size = size.saturating_add(bytes.len());
                if size > 8 * 1024 * 1024 || events.len() >= 8192 {
                    overflow = true;
                    return false;
                }
                events.push(value);
                true
            })
            .await;
        if overflow {
            return Err("native MODEL event limit exceeded; outcome unknown".into());
        }
        if response.ok
            && let Some(receipt) = receipt
        {
            receipt.commit(&session.lease)?;
        }
        // The neutral adapter retains token counters when the provider
        // supplies them. Its historical all-zero default is not proof of
        // usage or price: retain unknown explicitly, never display $0.
        let usage = response
            .output
            .as_deref()
            .and_then(|output| serde_json::from_str::<serde_json::Value>(output).ok())
            .and_then(|output| output.get("usage").cloned())
            .filter(|usage| {
                usage.as_object().is_some_and(|fields| {
                    fields
                        .values()
                        .any(|value| value.as_u64().is_some_and(|n| n > 0))
                })
            });
        encode_reply(&serde_json::json!({
            "events": events,
            "response": response,
            "accounting": {"usage": usage, "cost_usd": null},
        }))
    })
}

fn encode_reply(reply: &serde_json::Value) -> Result<Vec<u8>, String> {
    let reply = serde_json::to_vec(reply).map_err(|error| error.to_string())?;
    if reply.len() > 16 * 1024 * 1024 {
        return Err("native MODEL reply limit exceeded".into());
    }
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::{Command, Request, invocation};
    #[test]
    fn ordinary_verbs_cannot_consume_native_custody() {
        assert!(invocation(&Request::CancellationSupported).is_err());
    }
    #[test]
    fn claim_does_not_accept_sender_profile_or_actor_authority() {
        assert!(
            serde_json::from_value::<Command>(serde_json::json!({
                "command":"claim", "run_id":1, "lease":"x", "profile":"forged"
            }))
            .is_err()
        );
    }
}
