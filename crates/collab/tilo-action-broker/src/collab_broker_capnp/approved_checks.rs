//! Host-only check execution under the actual accepted target action's custody.
use super::{Arc, CapnpBrokerState, CapnpChannelHandle, OriginalInvocation};
use runtime_collab::approved_checks::{Receipt, Request};
use std::collections::{HashMap, HashSet};
use std::sync::Weak;

/// No public constructor or deserializer: only this broker mints custody.
#[derive(Clone)]
pub struct ApprovedCheckContext {
    original: Arc<OriginalInvocation>,
    state: Weak<CapnpBrokerState>,
    check_id: String,
}
impl ApprovedCheckContext {
    pub fn app_id(&self) -> &str {
        &self.original.route().app_id
    }
    pub fn workspace(&self) -> &str {
        &self.original.route().cluster_id
    }
    pub fn record(&self) -> &str {
        &self.original.route().record_id
    }
    pub fn resource(&self) -> &str {
        &self.original.request().address.resource
    }
    /// The stable identity retained from the authenticated target publication.
    ///
    /// # Errors
    /// Refuses absent or malformed identity; never derives one from caller strings.
    pub fn resource_identity(&self) -> Result<&tilo_actions::ResourceSemanticIdentity, String> {
        self.original.resource_identity()
    }
    pub fn check_id(&self) -> &str {
        &self.check_id
    }
    pub fn invocation(&self) -> String {
        self.original.request().invocation.0.to_string()
    }
    /// Recheck original attribution, current target custody and active cancellation.
    ///
    /// # Errors
    /// Refuses retired workers, cancelled/terminal invocations or changed policy.
    pub fn recheck(&self) -> Result<(), String> {
        let state = self
            .state
            .upgrade()
            .ok_or("approved check broker retired")?;
        self.original.check_execution_frontier(&state)?;
        self.original.check_resource_publication(&state)?;
        self.original.check_lifecycle()?;
        self.original.check_execution_frontier(&state)
    }
}

/// Trusted host adapter; it resolves the exact resource binding, command and bounds.
/// Implementations run on the host effect worker, never the broker's `LocalSet`.
pub trait ApprovedCheckServices: Send + Sync {
    /// Execute this exact approved ID and recheck custody before and during work.
    ///
    /// # Errors
    /// Refuses unsupported registrations, changed authority or unavailable enforcement.
    fn run(&self, context: &ApprovedCheckContext) -> Result<Receipt, String>;
}

pub(super) enum Entry {
    Running,
    Complete(Box<Result<Receipt, String>>),
}
#[derive(Default)]
pub(super) struct Registry(HashMap<u64, Entry>);

fn matched_reply(context: &ApprovedCheckContext, reply: Receipt) -> Result<Receipt, String> {
    reply.encode()?;
    if reply.invocation != context.invocation()
        || reply.check_id != context.check_id()
        || reply.binding.app_id != context.app_id()
        || reply.binding.workspace != context.workspace()
        || reply.binding.record != context.record()
        || reply.binding.resource != context.resource()
    {
        return Err("approved check host reply changed its original resource binding".into());
    }
    context.recheck()?;
    Ok(reply)
}

pub(super) fn context(
    channel: &CapnpChannelHandle,
    request: Request,
) -> Result<ApprovedCheckContext, String> {
    Ok(ApprovedCheckContext {
        original: OriginalInvocation::for_approved_check(channel, &request)?,
        state: Arc::downgrade(&channel.state),
        check_id: request.check_id,
    })
}

pub(super) async fn execute(context: ApprovedCheckContext) -> Result<Vec<u8>, String> {
    let state = context
        .state
        .upgrade()
        .ok_or("approved check broker retired")?;
    let id = context.original.request().wire_id;
    context.recheck()?;
    let service = state
        .services
        .approved_checks
        .clone()
        .ok_or("approved project checks unsupported by this host")?;
    // Bound the replay cache to the same retained invocation frontier. Never
    // evict active work or execute a retry after uncertain response delivery.
    let retained: HashSet<_> = state
        .pending_invocations
        .lock()
        .map_err(|_| "invocation registry unavailable")?
        .keys()
        .copied()
        .collect();
    let cached = {
        let mut registry = state
            .approved_check_jobs
            .lock()
            .map_err(|_| "check registry unavailable")?;
        registry
            .0
            .retain(|id, entry| retained.contains(id) || matches!(entry, Entry::Running));
        match registry.0.get(&id) {
            Some(Entry::Running) => {
                return Err(
                    "approved check is already executing; observe the original action".into(),
                );
            }
            Some(Entry::Complete(reply)) => Some(reply.as_ref().clone()),
            None => {
                if registry.0.len() >= 256 {
                    return Err("approved check retained execution limit reached".into());
                }
                registry.0.insert(id, Entry::Running);
                None
            }
        }
    };
    if let Some(reply) = cached {
        return matched_reply(&context, reply?).and_then(|reply| reply.encode());
    }
    let (send, receive) = tokio::sync::oneshot::channel();
    let worker = context.clone();
    let weak_state = Arc::downgrade(&state);
    let spawned = std::thread::Builder::new()
        .name("tilo-approved-check".into())
        .spawn(move || {
            // A host adapter panic must retain a terminal refusal, never leave
            // an uncertain execution retryable or permanently marked Running.
            let reply = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                worker
                    .recheck()
                    .and_then(|()| service.run(&worker))
                    .and_then(|reply| matched_reply(&worker, reply))
            }))
            .unwrap_or_else(|_| {
                Err("approved-check host worker panicked; outcome unobserved".into())
            });
            if let Some(state) = weak_state.upgrade()
                && let Ok(mut registry) = state.approved_check_jobs.lock()
            {
                registry
                    .0
                    .insert(id, Entry::Complete(Box::new(reply.clone())));
            }
            let _ = send.send(reply);
        });
    if let Err(error) = spawned {
        if let Ok(mut registry) = state.approved_check_jobs.lock() {
            registry
                .0
                .insert(id, Entry::Complete(Box::new(Err(error.to_string()))));
        }
        return Err(error.to_string());
    }
    drop(state);
    let reply = receive
        .await
        .map_err(|_| "approved-check worker ended without an observed result")??;
    matched_reply(&context, reply)?.encode()
}
