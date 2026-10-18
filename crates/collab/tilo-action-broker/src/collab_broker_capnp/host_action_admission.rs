//! Host-side admission barriers, shared by services that do not dispatch to an app.
//!
//! Call only after resolving and authorizing the current descriptor. The returned
//! permit is created before any service is started; terminal observations pass
//! through the same journal transition as application completions.

use super::{
    BrokerActor, CapnpBrokerState, InvocationCaller, InvocationKey, InvocationOwner,
    invocation_journal, principal_id_for_root,
};
use tilo_actions::remote::{ActionHostError, Caller, InvokeRequest};
use tilo_actions::{ActionDescriptor, InvocationId, InvocationUpdate};

#[cfg(test)]
#[path = "host_action_admission_tests.rs"]
mod tests;

pub(super) struct HostActionAdmission<'a> {
    pub room: &'a str,
    pub app_id: &'a str,
    pub descriptor: &'a ActionDescriptor,
    pub request: &'a InvokeRequest,
}

/// A dispatch permit, never reconstructed from recovered journal data.
pub(super) struct AdmittedHostAction {
    pub id: u64,
    pub invocation: InvocationId,
    pub owner: InvocationOwner,
    key: Option<InvocationKey>,
}

fn unavailable(reason: impl Into<String>) -> ActionHostError {
    ActionHostError::Unavailable {
        reason: reason.into(),
    }
}

impl CapnpBrokerState {
    /// Preserve original caller custody and UUID, and durably sign admission
    /// before allowing a host service's first side effect.
    pub(super) fn admit_host_action(
        &self,
        caller: &Caller,
        origin: Option<&InvocationCaller>,
        admission: HostActionAdmission<'_>,
    ) -> Result<AdmittedHostAction, ActionHostError> {
        self.admit_host_action_with_start(caller, origin, admission, None)
    }

    /// Include host-owned execution semantics absent from the wire request,
    /// such as a shortcut run's trigger and attendance mode.
    pub(super) fn admit_host_action_with_start(
        &self,
        caller: &Caller,
        origin: Option<&InvocationCaller>,
        admission: HostActionAdmission<'_>,
        start: Option<serde_json::Value>,
    ) -> Result<AdmittedHostAction, ActionHostError> {
        let HostActionAdmission {
            room,
            descriptor,
            request,
            ..
        } = &admission;
        let _admission = self
            .admission_lock
            .lock()
            .map_err(|_| unavailable("invocation admission unavailable"))?;
        if !self.is_accepting()
            || (caller.execution_id().is_some() && !caller.execution_is_live())
            || request.invocation.0.is_nil()
            || descriptor.address != request.target.address
        {
            return Err(ActionHostError::NotPermitted);
        }
        if self.durable_owner_required || self.publisher.requires_owner_for(room) {
            self.owner_health().map_err(unavailable)?;
        }
        let actor = BrokerActor::for_caller(caller);
        let owner = self.host_action_owner(caller, origin, room, request.invocation)?;
        owner.validate().map_err(unavailable)?;
        self.retain_host_action(&actor, owner, admission, start)
    }

    fn host_action_owner(
        &self,
        caller: &Caller,
        origin: Option<&InvocationCaller>,
        room: &str,
        invocation: InvocationId,
    ) -> Result<InvocationOwner, ActionHostError> {
        let actor = BrokerActor::for_caller(caller);
        let owner = match origin {
            Some(InvocationCaller::Certified {
                caller: original, ..
            }) => {
                self.checked_policy_time(original.policy_time.as_ref())
                    .map_err(unavailable)?;
                let current = self.current_action_caller(original).map_err(unavailable)?;
                let expected = current.broker_actor();
                if original.room != room
                    || expected.id != actor.id
                    || expected.is_agent != actor.is_agent
                {
                    return Err(ActionHostError::NotPermitted);
                }
                // Revalidation checks the full certificate, not a principal supplied
                // by the transport or an actor reconstructed from a display name.
                current.certificate.as_ref().ok_or_else(|| {
                    unavailable("action_evidence_unavailable: caller certificate missing")
                })?;
                original.owner()
            }
            Some(InvocationCaller::Local { route, admitted }) => {
                let current = self
                    .revalidate_admission(route, admitted)
                    .map_err(|error| unavailable(error.to_string()))?;
                let Some(certificate) = current.certificate.as_ref() else {
                    // Retain pre-identity connection compatibility without
                    // upgrading its displayed actor into signed user evidence.
                    let expected = current.broker_actor();
                    if route.cluster_id != room
                        || expected.id != actor.id
                        || expected.is_agent != actor.is_agent
                    {
                        return Err(ActionHostError::NotPermitted);
                    }
                    return Ok(InvocationOwner::Local {
                        actor: actor.id.clone(),
                    });
                };
                let expected = current.agent_id.as_ref().map_or_else(
                    || principal_id_for_root(certificate.user_public_key),
                    |agent| format!("agent:{}", agent.strip_prefix("agent:").unwrap_or(agent)),
                );
                if route.cluster_id != room
                    || expected != actor.id
                    || current.agent_id.is_some() != actor.is_agent
                {
                    return Err(ActionHostError::NotPermitted);
                }
                InvocationOwner::AuthenticatedLocal {
                    root: certificate.user_public_key,
                    device: certificate.device_public_key,
                    agent: current.agent_id.clone(),
                }
            }
            Some(InvocationCaller::NativePerson {
                invocation: original_invocation,
                room: original_room,
                certificate,
                time,
            }) => {
                self.checked_policy_time(Some(time)).map_err(unavailable)?;
                self.current_native_person(room, certificate)
                    .map_err(unavailable)?;
                if original_room != room
                    || *original_invocation != invocation
                    || actor.is_agent
                    || actor.id != principal_id_for_root(certificate.user_public_key)
                {
                    return Err(ActionHostError::NotPermitted);
                }
                InvocationOwner::AuthenticatedLocal {
                    root: certificate.user_public_key,
                    device: certificate.device_public_key,
                    agent: None,
                }
            }
            None => {
                // A transport key is not retained certificate custody. Do not
                // silently downgrade a certified caller to actor-only ownership.
                if caller.device_public_key().is_some() || caller.user_public_key().is_some() {
                    return Err(unavailable(
                        "action_evidence_unavailable: original caller missing",
                    ));
                }
                InvocationOwner::Local {
                    actor: actor.id.clone(),
                }
            }
        };
        Ok(owner)
    }

    fn retain_host_action(
        &self,
        actor: &BrokerActor,
        owner: InvocationOwner,
        admission: HostActionAdmission<'_>,
        start: Option<serde_json::Value>,
    ) -> Result<AdmittedHostAction, ActionHostError> {
        let HostActionAdmission {
            room,
            app_id,
            descriptor,
            request,
        } = admission;
        let id = self
            .invocation_id
            .fetch_update(
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
                |id| id.checked_add(1),
            )
            .map_err(|_| unavailable("invocation handle space exhausted"))?;
        let mut guard = self
            .journal
            .lock()
            .map_err(|_| unavailable("invocation journal unavailable"))?;
        let journal = guard.as_mut().map_err(|error| unavailable(error.clone()))?;
        if owner.verified_actor().is_some() && journal.is_none() {
            return Err(unavailable(
                "action_evidence_unavailable: admission journal unavailable",
            ));
        }
        let key = if let Some(journal) = journal {
            let key = InvocationKey {
                room: room.into(),
                host: journal.host(),
                owner: owner.clone(),
                invocation: request.invocation,
            };
            // Host services expose no resolved revision identifier. Bind the
            // descriptor actually authorized by the host instead; the caller's
            // revision remains part of the exact request, not an authority claim.
            let resolved_descriptor =
                serde_json::to_value(descriptor).map_err(|error| unavailable(error.to_string()))?;
            let descriptor_digest = hex::encode(invocation_journal::admission_request_digest(
                &resolved_descriptor,
            ));
            let mut intent = serde_json::json!({
                "host_action": true,
                "host_descriptor_digest": descriptor_digest,
                "request": request,
                "contract": descriptor.contract,
                "operation_kind": descriptor.kind,
                "context": request.context,
                "actor": actor.id,
                "app_id": app_id,
                "record_id": "",
                "resource": descriptor.address.resource,
                "resource_kind": descriptor.resource_kind,
                "action": descriptor.address.action,
            });
            if let Some(start) = start {
                intent["host_start"] = start;
            }
            let evidence = if owner.verified_actor().is_some() {
                let facts =
                    invocation_journal::evidence_facts(&key, &intent).map_err(unavailable)?;
                Some(crate::action_evidence::mint(facts, &intent).map_err(|_| {
                    unavailable("action_evidence_unavailable: host signing identity unavailable")
                })?)
            } else {
                None
            };
            if !journal
                .admit_with_evidence(key.clone(), intent, id, evidence)
                .map_err(unavailable)?
            {
                return Err(ActionHostError::Refused {
                    reason: "invocation already admitted".into(),
                });
            }
            Some(key)
        } else {
            None
        };
        drop(guard);
        self.record_completion(
            id,
            owner.clone(),
            InvocationUpdate::Accepted {
                invocation: request.invocation,
            },
        );
        Ok(AdmittedHostAction {
            id,
            invocation: request.invocation,
            owner,
            key,
        })
    }

    /// Recover an observation, never a service dispatch permit. Ownership uses
    /// exactly the same key as admission; certificate-shaped owners are not
    /// interchangeable, even when their principal happens to match.
    pub(super) fn host_action_replay(
        &self,
        caller: &Caller,
        origin: &InvocationCaller,
        request: &InvokeRequest,
    ) -> Result<Option<tilo_actions::remote::InvocationReceipt>, ActionHostError> {
        let _admission = self
            .admission_lock
            .lock()
            .map_err(|_| unavailable("invocation admission unavailable"))?;
        if !self.is_accepting() || (caller.execution_id().is_some() && !caller.execution_is_live())
        {
            return Err(ActionHostError::NotPermitted);
        }
        // Legacy app-local custody cannot have admitted a signed host action.
        // Leave its ordinary application dispatch path unchanged.
        if matches!(origin, InvocationCaller::Local { admitted, .. } if admitted.certificate.is_none())
        {
            return Ok(None);
        }
        let room = caller.room().ok_or(ActionHostError::NotPermitted)?;
        let owner = self.host_action_owner(caller, Some(origin), room, request.invocation)?;
        let record = {
            let guard = self
                .journal
                .lock()
                .map_err(|_| unavailable("invocation journal unavailable"))?;
            let Some(journal) = guard.as_ref().map_err(|e| unavailable(e.clone()))?.as_ref() else {
                return Ok(None);
            };
            journal
                .lookup(&InvocationKey {
                    room: room.into(),
                    host: journal.host(),
                    owner,
                    invocation: request.invocation,
                })
                .map_err(unavailable)?
        };
        let Some(record) = record.filter(|record| {
            record.intent.get("host_action") == Some(&serde_json::Value::Bool(true))
        }) else {
            return Ok(None);
        };
        let intent =
            serde_json::to_value(request).map_err(|error| unavailable(error.to_string()))?;
        if !record.admitted_request_is(&intent) {
            return Err(ActionHostError::Refused {
                reason: "invocation identity reused with different intent".into(),
            });
        }
        let field = |name| {
            record
                .intent
                .get(name)
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| unavailable("host invocation intent invalid"))
        };
        let contract: tilo_actions::ActionContract =
            serde_json::from_value(record.intent.get("contract").cloned().unwrap_or_default())
                .map_err(|_| unavailable("host invocation contract invalid"))?;
        let actor = self.host_replay_actor(room, origin)?;
        self.authorize_invocation_permission(
            room,
            (
                field("app_id")?,
                field("resource_kind")?,
                field("resource")?,
                field("action")?,
            ),
            &contract.need(),
            &actor,
            self.policy_now_ns().map_err(unavailable)? / 1_000_000_000,
        )
        .map_err(unavailable)?;
        let mut ledger = self
            .remote_invocations
            .lock()
            .map_err(|_| unavailable("invocation ledger unavailable"))?;
        ledger.reserve(caller, request)?;
        ledger.complete(&request.invocation, record.wire_id);
        drop(ledger);
        self.record_completion(record.wire_id, record.key.owner, record.update.clone());
        Ok(Some(
            tilo_actions::remote::InvocationState::new(request.invocation, record.update).receipt(),
        ))
    }

    fn host_replay_actor(
        &self,
        room: &str,
        origin: &InvocationCaller,
    ) -> Result<BrokerActor, ActionHostError> {
        Ok(match origin {
            InvocationCaller::Certified {
                caller: original,
                foreign_restriction,
            } => {
                let mut admitted = self
                    .current_action_caller(original)
                    .map_err(unavailable)?
                    .broker_actor();
                admitted.capped |= super::cap_applies_in(
                    self.certificate_authority.get().map(AsRef::as_ref),
                    room,
                    *foreign_restriction,
                    original
                        .admitted
                        .certificate
                        .as_ref()
                        .map(|certificate| &certificate.device_public_key),
                );
                admitted
            }
            InvocationCaller::Local { route, admitted } => self
                .revalidate_admission(route, admitted)
                .map_err(|error| unavailable(error.to_string()))?
                .broker_actor(),
            InvocationCaller::NativePerson { certificate, .. } => {
                self.certified_person(room, certificate)
            }
        })
    }

    /// A failed durable transition must not publish a successful completion.
    pub(super) fn complete_host_action(
        &self,
        admitted: &AdmittedHostAction,
        update: InvocationUpdate,
    ) -> Result<(), ActionHostError> {
        let _admission = self
            .admission_lock
            .lock()
            .map_err(|_| unavailable("invocation admission unavailable"))?;
        if !self.is_accepting()
            || update.invocation() != admitted.invocation
            || !update.is_terminal()
        {
            return Err(ActionHostError::NotPermitted);
        }
        // Host services have no cooperative cancellation protocol. Their
        // Accepted observation is not a cancellable application dispatch.
        if self
            .completed_invocations
            .lock()
            .map_err(|_| unavailable("invocation registry unavailable"))?
            .get(&admitted.id)
            .is_some_and(|(owner, update)| owner != &admitted.owner || update.is_terminal())
        {
            return Err(unavailable("host invocation is no longer running"));
        }
        if let Some(key) = &admitted.key {
            self.journal
                .lock()
                .map_err(|_| unavailable("invocation journal unavailable"))?
                .as_mut()
                .map_err(|error| unavailable(error.clone()))?
                .as_mut()
                .ok_or_else(|| unavailable("durable admission unavailable"))?
                .update(key, update.clone(), false)
                .map_err(unavailable)?;
        }
        self.record_completion(admitted.id, admitted.owner.clone(), update);
        Ok(())
    }
}
