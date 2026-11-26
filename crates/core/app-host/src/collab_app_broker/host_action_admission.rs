//! Host-side admission barriers, shared by services that do not dispatch to an app.
//!
//! Call only after resolving and authorizing the current descriptor. The returned
//! permit is created before any service is started; terminal observations pass
//! through the same journal transition as application completions.

use super::{
    AppBrokerState, BrokerActor, InvocationCaller, InvocationKey, InvocationOwner,
    person_id_for_root,
};
use tilo_action_policy::MemberId;
use tilo_actions::remote::{ActionCallError, Caller, InvokeRequest};
use tilo_actions::{ActionDescriptor, InvocationId, InvocationUpdate};

#[cfg(test)]
#[path = "host_action_admission_tests.rs"]
mod tests;

#[derive(Clone, Copy)]
pub(super) struct HostActionAdmission<'a> {
    pub workspace: &'a str,
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

impl AppBrokerState {
    /// Preserve original caller ownership and UUID, and durably sign admission
    /// before allowing a host service's first side effect.
    pub(super) fn admit_host_action(
        &self,
        caller: &Caller,
        origin: Option<&InvocationCaller>,
        admission: HostActionAdmission<'_>,
    ) -> Result<AdmittedHostAction, ActionCallError> {
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
    ) -> Result<AdmittedHostAction, ActionCallError> {
        let HostActionAdmission {
            workspace,
            descriptor,
            request,
            ..
        } = &admission;
        let _admission = self
            .admission_lock
            .lock()
            .map_err(|_| ActionCallError::unavailable("invocation admission unavailable"))?;
        if !self.is_accepting()
            || (caller.execution_id().is_some() && !caller.execution_is_live())
            || request.invocation.0.is_nil()
            || descriptor.address != request.target.address
        {
            tracing::warn!(
                accepting = self.is_accepting(),
                run_ended = caller.execution_id().is_some() && !caller.execution_is_live(),
                "an action of the person's own was refused at its admission"
            );
            return Err(ActionCallError::NotPermitted);
        }
        if self.durable_owner_required || self.publisher.has_commits() {
            self.owner_health().map_err(ActionCallError::unavailable)?;
        }
        let actor = BrokerActor::for_caller(caller);
        let owner = match (origin, Self::forwarded_owner(caller, request)) {
            (None, Some(owner)) => owner,
            // A shortcut's approval owns only the run and the steps it covers.
            (Some(InvocationCaller::Shortcut(approval)), _) => self.shortcut_call_owner(
                approval,
                workspace,
                actor.actor(),
                (
                    (
                        admission.app_id,
                        &descriptor.resource_kind,
                        &descriptor.address.resource,
                        &descriptor.address.action,
                    ),
                    &request.argument,
                ),
            )?,
            _ => self.host_action_owner(caller, origin, workspace, request.invocation)?,
        };
        owner.validate().map_err(ActionCallError::unavailable)?;
        self.retain_host_action(&actor, owner, admission, start)
    }

    /// Who owns `request` when another device brought it for its caller, or a turn this device's
    /// agent engine runs here made it, and its caller signed it: that caller, as the signature it
    /// carries proves and its admission checked (an agent's turn's by `signed_by_agent`), their
    /// own or the one an app's call serves. Nothing of it is kept but its owner. `None` for any
    /// other call this device's own connections brought.
    fn forwarded_owner(caller: &Caller, request: &InvokeRequest) -> Option<InvocationOwner> {
        let engine_turn = caller.is_engine_turn_here() && caller.roster_created_at().is_some();
        if caller.forwarder().is_none() && !engine_turn {
            return None;
        }
        let (Some(root), Some(device), Some(signed)) = (
            caller.user_public_key(),
            caller.device_public_key(),
            request.signed.as_deref(),
        ) else {
            return None;
        };
        (signed.statement().signer == *device).then(|| InvocationOwner::AuthenticatedLocal {
            root: *root,
            device: *device,
            agent: caller.acting().agent_id().map(str::to_owned),
        })
    }

    #[expect(
        clippy::too_many_lines,
        reason = "each origin's owner is revalidated against current policy, certificate and workspace in one ordered match"
    )]
    fn host_action_owner(
        &self,
        caller: &Caller,
        origin: Option<&InvocationCaller>,
        workspace: &str,
        invocation: InvocationId,
    ) -> Result<InvocationOwner, ActionCallError> {
        let actor = BrokerActor::for_caller(caller);
        let owner = match origin {
            Some(InvocationCaller::Certified {
                caller: original, ..
            }) => {
                self.checked_policy_time(original.policy_time.as_ref())
                    .map_err(ActionCallError::unavailable)?;
                let current = self
                    .current_action_caller(original)
                    .map_err(ActionCallError::unavailable)?;
                let expected = current.broker_actor();
                if original.workspace != workspace || expected.actor() != actor.actor() {
                    return Err(ActionCallError::NotPermitted);
                }
                // Revalidation checks the full certificate, not an actor supplied
                // by the transport or an actor reconstructed from a display name.
                let certificate = current.certificate.as_ref().ok_or_else(|| {
                    ActionCallError::unavailable(
                        "action_evidence_unavailable: caller certificate missing",
                    )
                })?;
                if caller.device_public_key().is_none() {
                    // Only the native facade combines a physical projection
                    // with retained signed admission. Recheck its protected device
                    // at the effect barrier, not only when it queued the request.
                    self.current_native_person(workspace, certificate)
                        .map_err(ActionCallError::unavailable)?;
                }
                original.owner().map_err(ActionCallError::unavailable)?
            }
            Some(InvocationCaller::Local {
                route,
                admitted,
                time,
                ..
            }) => {
                self.checked_policy_time(time.as_ref())
                    .map_err(ActionCallError::unavailable)?;
                let current = self
                    .revalidate_admission(route, admitted)
                    .map_err(|error| ActionCallError::unavailable(error.to_string()))?;
                self.checked_policy_time(time.as_ref())
                    .map_err(ActionCallError::unavailable)?;
                let Some(certificate) = current.certificate.as_ref() else {
                    if self.services.local_person.is_some()
                        || self.services.native_certificates.is_some()
                    {
                        return Err(ActionCallError::unavailable(
                            "action_evidence_unavailable: original certificate missing",
                        ));
                    }
                    // Only an explicitly identity-free host retains connection
                    // compatibility. A missing device service is not bootstrap.
                    // Retain pre-identity connection compatibility without
                    // upgrading its displayed actor into signed user evidence.
                    let expected = current.broker_actor();
                    if route.cluster_id != workspace || expected.actor() != actor.actor() {
                        return Err(ActionCallError::NotPermitted);
                    }
                    return Ok(InvocationOwner::Local {
                        actor: actor.actor().clone(),
                    });
                };
                let expected: tilo_action_policy::Actor = current
                    .agent_id
                    .as_ref()
                    .map_or_else(
                        || MemberId::person(person_id_for_root(certificate.user_public_key)),
                        |agent| {
                            MemberId::agent(
                                super::delegated_agent_name(agent),
                                person_id_for_root(certificate.user_public_key),
                            )
                        },
                    )
                    .into();
                // The connection's app acting under its own identity, by what
                // the workspace grants it: decided as that app and owned by it,
                // never attested as the connection's person
                // (`AppBrokerState::invocation_owner`).
                if route.cluster_id == workspace
                    && actor
                        .app
                        .as_ref()
                        .is_some_and(|app| app.as_str() == route.app_id)
                {
                    return Ok(InvocationOwner::Local {
                        actor: actor.actor().clone(),
                    });
                }
                if route.cluster_id != workspace || expected != *actor.actor() {
                    return Err(ActionCallError::NotPermitted);
                }
                InvocationOwner::AuthenticatedLocal {
                    root: certificate.user_public_key,
                    device: certificate.device_public_key,
                    agent: current.agent_id.clone(),
                }
            }
            Some(InvocationCaller::NativePerson {
                invocation: original_invocation,
                workspace: original_workspace,
                certificate,
                time,
            }) => {
                self.checked_policy_time(Some(time))
                    .map_err(ActionCallError::unavailable)?;
                self.current_native_person(workspace, certificate)
                    .map_err(ActionCallError::unavailable)?;
                if original_workspace != workspace
                    || *original_invocation != invocation
                    || actor.actor().person_id()
                        != Some(person_id_for_root(certificate.user_public_key).as_str())
                {
                    tracing::warn!(
                        %workspace,
                        original_workspace,
                        caller = %actor.actor(),
                        "an action of the person's own was refused: its admission names another device"
                    );
                    return Err(ActionCallError::NotPermitted);
                }
                InvocationOwner::AuthenticatedLocal {
                    root: certificate.user_public_key,
                    device: certificate.device_public_key,
                    agent: None,
                }
            }
            Some(InvocationCaller::Shortcut(approval)) => self
                .current_shortcut_approval(approval, workspace, actor.actor())
                .map_err(|reason| ActionCallError::Refused { reason })?,
            None => {
                // A transport key is not retained certificate ownership. Do not
                // silently downgrade a certified caller to actor-only ownership.
                // A host with a device or native certificates serves everyone
                // through retained ownership, so a caller that arrives with no
                // original admission is refused rather than downgraded. An app
                // this device's daemon launched, acting by itself, has no
                // certificate to downgrade: it is decided by the grants that name
                // it, and what it starts is its own. Having no keys is not being
                // that app: only the daemon's attestation of the process it
                // launched is, so a caller restored, carried or crafted to name an
                // app never owns as it.
                let keyed =
                    caller.device_public_key().is_some() || caller.user_public_key().is_some();
                let app_itself = caller
                    .launched_app()
                    .is_some_and(|launched| actor.actor().app() == Some(launched));
                if keyed
                    || (actor.actor().app().is_some() && !app_itself)
                    || (!app_itself
                        && (self.services.local_person.is_some()
                            || self.services.native_certificates.is_some()))
                {
                    return Err(ActionCallError::unavailable(
                        "action_evidence_unavailable: original caller missing",
                    ));
                }
                InvocationOwner::Local {
                    actor: actor.actor().clone(),
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
    ) -> Result<AdmittedHostAction, ActionCallError> {
        let HostActionAdmission {
            workspace,
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
            .map_err(|_| ActionCallError::unavailable("invocation handle space exhausted"))?;
        let mut guard = self
            .journal
            .lock()
            .map_err(|_| ActionCallError::unavailable("invocation journal unavailable"))?;
        let journal = guard
            .as_mut()
            .map_err(|error| ActionCallError::unavailable(error.clone()))?;
        if owner.verified_actor().is_some() && journal.is_none() {
            return Err(ActionCallError::unavailable(
                "action_evidence_unavailable: admission journal unavailable",
            ));
        }
        let mut admitted = tilo_invocation_journal::Durable::settled();
        let key = if let Some(journal) = journal {
            let key = InvocationKey {
                workspace: workspace.into(),
                host: journal.host(),
                owner: owner.clone(),
                invocation: request.invocation,
            };
            // Host services expose no resolved revision identifier. Bind the
            // descriptor actually authorized by the host instead; the caller's
            // revision remains part of the exact request, not an authority claim.
            let resolved_descriptor = serde_json::to_value(descriptor)
                .map_err(|error| ActionCallError::unavailable(error.to_string()))?;
            let descriptor_digest = hex::encode(tilo_invocation_journal::admission_request_digest(
                &resolved_descriptor,
            ));
            let mut intent = serde_json::json!({
                "host_action": true,
                "host_descriptor_digest": descriptor_digest,
                "request": request,
                "contract": descriptor.contract,
                "operation_kind": descriptor.kind,
                "context": request.context,
                "actor": actor.actor(),
                "app_id": app_id,
                "document_id": "",
                "resource": descriptor.address.resource,
                "resource_kind": descriptor.resource_kind,
                "action": descriptor.address.action,
            });
            if let (Some(start), Some(fields)) = (start, intent.as_object_mut()) {
                fields.insert("host_start".into(), start);
            }
            let evidence = if owner.verified_actor().is_some() {
                let facts = tilo_invocation_journal::evidence_facts(&key, &intent)
                    .map_err(ActionCallError::unavailable)?;
                Some(
                    tilo_invocation_journal::action_evidence::mint(facts, &intent).map_err(|why| {
                        ActionCallError::unavailable(format!(
                            "action_evidence_unavailable: host signing identity unavailable: {why}"
                        ))
                    })?,
                )
            } else {
                None
            };
            let (fresh, durable) = journal
                .admit_with_evidence(key.clone(), intent, id, evidence)
                .map_err(ActionCallError::unavailable)?;
            if !fresh {
                return Err(ActionCallError::Refused {
                    reason: "invocation already admitted".into(),
                });
            }
            admitted = durable;
            Some(key)
        } else {
            None
        };
        drop(guard);
        admitted
            .wait_blocking()
            .map_err(ActionCallError::unavailable)?;
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
    /// interchangeable, even when their actor happens to match.
    pub(super) fn host_action_replay(
        &self,
        caller: &Caller,
        origin: &InvocationCaller,
        request: &InvokeRequest,
    ) -> Result<Option<tilo_actions::remote::InvocationReceipt>, ActionCallError> {
        // Application providers cannot admit host-service records. Do not take
        // the durable admission lock for their UI-side replay probe: another
        // application's signer may be stalled while this request enqueues.
        if request.target.provider.parse::<u64>().is_ok()
            || request.target.provider == tilo_actions::remote::OWN_PROVIDER
            || request
                .target
                .provider
                .starts_with(super::canonical_route::PROVIDER_PREFIX)
        {
            return Ok(None);
        }
        let _admission = self
            .admission_lock
            .lock()
            .map_err(|_| ActionCallError::unavailable("invocation admission unavailable"))?;
        if !self.is_accepting() || (caller.execution_id().is_some() && !caller.execution_is_live())
        {
            tracing::warn!(
                accepting = self.is_accepting(),
                run_ended = caller.execution_id().is_some() && !caller.execution_is_live(),
                "an action of the person's own was refused before its admission was read"
            );
            return Err(ActionCallError::NotPermitted);
        }
        // Legacy app-local ownership cannot have admitted a signed host action.
        // Leave its ordinary application dispatch path unchanged.
        if matches!(origin, InvocationCaller::Local { admitted, .. } if admitted.certificate.is_none())
        {
            return Ok(None);
        }
        let Some(workspace) = caller.workspace() else {
            tracing::warn!(
                "an action of the person's own was refused: the caller names no workspace"
            );
            return Err(ActionCallError::NotPermitted);
        };
        let owner = self.host_action_owner(caller, Some(origin), workspace, request.invocation)?;
        let record = {
            let guard = self
                .journal
                .lock()
                .map_err(|_| ActionCallError::unavailable("invocation journal unavailable"))?;
            let journal = guard
                .as_ref()
                .map_err(|e| ActionCallError::unavailable(e.clone()))?;
            let Some(journal) = journal.as_ref() else {
                return Ok(None);
            };
            let looked = journal.lookup(&InvocationKey {
                workspace: workspace.into(),
                host: journal.host(),
                owner,
                invocation: request.invocation,
            });
            drop(guard);
            looked.map_err(ActionCallError::unavailable)?
        };
        let Some(record) = record.filter(|record| {
            record.intent.get("host_action") == Some(&serde_json::Value::Bool(true))
        }) else {
            return Ok(None);
        };
        let intent = serde_json::to_value(request)
            .map_err(|error| ActionCallError::unavailable(error.to_string()))?;
        if !record.admitted_request_is(&intent) {
            return Err(ActionCallError::Refused {
                reason: "invocation identity reused with different intent".into(),
            });
        }
        let field = |name| {
            record
                .intent
                .get(name)
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| ActionCallError::unavailable("host invocation intent invalid"))
        };
        let contract: tilo_actions::ActionContract =
            serde_json::from_value(record.intent.get("contract").cloned().unwrap_or_default())
                .map_err(|_| ActionCallError::unavailable("host invocation contract invalid"))?;
        let actor = self.host_replay_actor(origin)?;
        self.authorize_invocation_permission(
            workspace,
            (
                field("app_id")?,
                field("resource_kind")?,
                field("resource")?,
                field("action")?,
            ),
            &contract.need(),
            &actor,
            self.policy_now_ns().map_err(ActionCallError::unavailable)? / 1_000_000_000,
        )
        .map_err(ActionCallError::unavailable)?;
        let mut log = self
            .remote_invocations
            .lock()
            .map_err(|_| ActionCallError::unavailable("invocation log unavailable"))?;
        log.reserve(caller, &Self::invocation_identity(request))?;
        log.complete(&request.invocation, record.wire_id);
        drop(log);
        self.record_completion(record.wire_id, record.key.owner, record.update.clone());
        Ok(Some(
            tilo_actions::remote::InvocationState::new(request.invocation, record.update).receipt(),
        ))
    }

    fn host_replay_actor(&self, origin: &InvocationCaller) -> Result<BrokerActor, ActionCallError> {
        Ok(match origin {
            InvocationCaller::Certified {
                caller: original,
                foreign_restriction,
            } => {
                let mut admitted = self
                    .current_action_caller(original)
                    .map_err(ActionCallError::unavailable)?
                    .broker_actor();
                admitted.capped |= *foreign_restriction;
                admitted
            }
            InvocationCaller::Local {
                route,
                admitted,
                time,
                ..
            } => {
                self.checked_policy_time(time.as_ref())
                    .map_err(ActionCallError::unavailable)?;
                self.revalidate_admission(route, admitted)
                    .map_err(|error| ActionCallError::unavailable(error.to_string()))?
                    .broker_actor()
            }
            InvocationCaller::NativePerson { certificate, .. } => {
                AppBrokerState::certified_person(certificate)
            }
            InvocationCaller::Shortcut(approval) => {
                let actor = approval.decided_as();
                self.current_shortcut_approval(approval, approval.workspace(), actor.actor())
                    .map_err(|reason| ActionCallError::Refused { reason })?;
                actor
            }
        })
    }

    /// A failed durable transition must not publish a successful completion.
    pub(super) fn complete_host_action(
        &self,
        admitted: &AdmittedHostAction,
        update: InvocationUpdate,
    ) -> Result<(), ActionCallError> {
        self.complete_host_action_outcome(admitted, update, false)
    }

    /// A lost service answer cannot establish whether its effect completed.
    pub(super) fn complete_uncertain_host_action(
        &self,
        admitted: &AdmittedHostAction,
        update: InvocationUpdate,
    ) -> Result<(), ActionCallError> {
        self.complete_host_action_outcome(admitted, update, true)
    }

    fn complete_host_action_outcome(
        &self,
        admitted: &AdmittedHostAction,
        update: InvocationUpdate,
        uncertain: bool,
    ) -> Result<(), ActionCallError> {
        let _admission = self
            .admission_lock
            .lock()
            .map_err(|_| ActionCallError::unavailable("invocation admission unavailable"))?;
        if !self.is_accepting()
            || update.invocation() != admitted.invocation
            || !update.is_terminal()
        {
            return Err(ActionCallError::NotPermitted);
        }
        // Host services end through this barrier; a cancellable one is no longer running, so
        // its stop flag is forgotten with it.
        if let Ok(mut stops) = self.host_step_stops.lock() {
            stops.remove(&admitted.id);
        }
        // A host service's `Accepted` observation is not an application dispatch: what ends it
        // is either its own answer or, for a step that declared itself cancellable, its
        // cancellation, which `complete_host_action` records like any other terminal update.
        if self
            .completed_invocations
            .lock()
            .map_err(|_| ActionCallError::unavailable("invocation registry unavailable"))?
            .get(&admitted.id)
            .is_some_and(|(owner, update)| owner != &admitted.owner || update.is_terminal())
        {
            return Err(ActionCallError::unavailable(
                "host invocation is no longer running",
            ));
        }
        if let Some(key) = &admitted.key {
            let durable = self
                .journal
                .lock()
                .map_err(|_| ActionCallError::unavailable("invocation journal unavailable"))?
                .as_mut()
                .map_err(|error| ActionCallError::unavailable(error.clone()))?
                .as_mut()
                .ok_or_else(|| ActionCallError::unavailable("durable admission unavailable"))?
                .record_update(key, update.clone(), uncertain)
                .map_err(ActionCallError::unavailable)?;
            durable
                .wait_blocking()
                .map_err(ActionCallError::unavailable)?;
        }
        self.record_completion(admitted.id, admitted.owner.clone(), update);
        Ok(())
    }
}
