#[allow(
    clippy::wildcard_imports,
    reason = "one part of the broker module: the shared state, its imports and the other parts reach this through the parent glob"
)]
use super::*;

mod admit;
mod asks;

pub(crate) use asks::Refusal;

#[derive(Clone, Debug)]
pub struct InvocationEnvelope {
    pub(super) id: u64,
    pub(super) invocation: tilo_actions::InvocationId,
    pub resource: String,
    pub action: String,
    pub(super) argument: Option<String>,
    pub actor: tilo_actions::Actor,
    pub(super) context: Option<tilo_actions::ActionContext>,
}

impl InvocationEnvelope {
    /// The context the host set for the invocation: who invoked it, of what
    /// kind, for whom, in which run.
    #[cfg(test)]
    #[must_use]
    pub fn context(&self) -> Option<&tilo_actions::ActionContext> {
        self.context.as_ref()
    }
}

/// The application's terminal answer to a directed invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvocationCompletion {
    pub code: String,
    pub message: String,
}

pub struct InvocationJob {
    pub id: u64,
    pub completion: std::sync::mpsc::Receiver<InvocationCompletion>,
}

/// `answer`, waited for here: a test's view of a host's answer.
#[cfg(test)]
#[track_caller]
pub(crate) fn answered<T>(answer: tilo_actions::remote::Pending<T>) -> T {
    answer
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten()
        .expect("the host answers")
}

impl AppBrokerState {
    /// Who an invoke an agent's connection makes continuing a running
    /// invocation is admitted for: that invocation's own caller, with their
    /// grants, reaching no further than it ([`Self::child_caller`]). `None`
    /// for any other request; the refusal to answer with when the connection
    /// runs no such invocation. An application's connection is decided by
    /// [`Self::app_request_caller`].
    pub(super) fn continued_caller(
        &self,
        peer_id: u64,
        message: &tilo_actions::remote::RemoteActionMessage,
    ) -> Option<Result<tilo_actions::remote::Caller, Box<tilo_actions::remote::RemoteActionMessage>>>
    {
        let tilo_actions::remote::RemoteActionMessage::Invoke(invoke) = message else {
            return None;
        };
        let running = invoke.on_behalf_of?;
        Some(self.child_caller(peer_id, running).ok_or_else(|| {
            Box::new(tilo_actions::remote::RemoteActionMessage::Receipt(
                tilo_actions::remote::InvocationReceipt {
                    invocation: invoke.invocation,
                    status: tilo_actions::remote::ReceiptStatus::Rejected {
                        error: tilo_actions::remote::ActionCallError::NotPermitted,
                    },
                },
            ))
        }))
    }

    /// The caller of the invocation `running` that this broker delivered to
    /// the connection `peer_id` and that still runs. `None` when the
    /// connection runs no such invocation, two callers share its id, or its
    /// caller no longer stands.
    pub(crate) fn continuing_caller(
        &self,
        peer_id: u64,
        running: tilo_actions::InvocationId,
    ) -> Option<tilo_actions::remote::Caller> {
        self.running_for(peer_id, running).map(|(caller, _)| caller)
    }

    /// Who a request continuing the invocation `running`, which this broker
    /// delivered to the connection `peer_id` and that still runs, is decided
    /// as: its caller, reaching no further than it was admitted to without
    /// being asked for. `None` as [`Self::continuing_caller`].
    pub(crate) fn child_caller(
        &self,
        peer_id: u64,
        running: tilo_actions::InvocationId,
    ) -> Option<tilo_actions::remote::Caller> {
        self.running_for(peer_id, running)
            .map(|(caller, reach)| caller.within(reach))
    }

    /// The current caller of `running` on `peer_id`, and how far it was
    /// admitted to reach.
    pub(super) fn running_for(
        &self,
        peer_id: u64,
        running: tilo_actions::InvocationId,
    ) -> Option<(tilo_actions::remote::Caller, tilo_action_policy::Reach)> {
        // Apis that take the caller's own invocation id can admit two
        // callers' invocations under one id; such an id names nobody.
        let mut matching = self
            .pending_invocations
            .lock()
            .ok()?
            .values()
            .filter(|pending| pending.peer_id == peer_id && pending.update.invocation() == running)
            .map(|pending| (pending.caller.clone(), pending.reach))
            .take(2)
            .collect::<Vec<_>>();
        if matching.len() != 1 {
            return None;
        }
        let (caller, reach) = matching.pop()?;
        Some((caller?.current(self)?, reach))
    }
}

/// Who a running invocation was admitted for, however the API knew them:
/// the caller an edit its effect submits is admitted as.
#[derive(Clone)]
pub(super) enum ContinuingCaller {
    /// Host-admitted native person, revalidated for every continuation.
    NativePerson {
        workspace: String,
        certificate: Box<tilo_user_identity::DeviceCert>,
        time: crate::apps::policy_clock::PolicyTime,
        direct: tilo_actions::remote::Caller,
    },
    /// A caller the API knew directly.
    Caller(tilo_actions::remote::Caller),
    /// A certified caller, its certificate checked again each time.
    Certified {
        caller: Box<VerifiedActionCaller>,
        /// A stricter foreign-device restriction the host kept for them.
        foreign_restriction: bool,
        /// The exact caller the API admitted, with its execution, when it
        /// knew one; it continues only while that execution is live.
        direct: Option<tilo_actions::remote::Caller>,
    },
    /// A person or agent on a connection this host admitted, in a workspace.
    Admitted {
        workspace: String,
        admitted: Box<AdmittedJoinIdentity>,
    },
}

impl ContinuingCaller {
    pub(super) fn from_origin(origin: &InvocationCaller) -> Self {
        match origin {
            InvocationCaller::NativePerson {
                workspace,
                certificate,
                time,
                ..
            } => Self::NativePerson {
                workspace: workspace.clone(),
                certificate: certificate.clone(),
                time: time.clone(),
                direct: tilo_actions::remote::Caller::local_person(person_id_for_root(
                    certificate.user_public_key,
                ))
                .in_workspace(workspace.clone()),
            },
            InvocationCaller::Certified {
                caller,
                foreign_restriction,
            } => Self::Certified {
                caller: caller.clone(),
                foreign_restriction: *foreign_restriction,
                direct: None,
            },
            InvocationCaller::Local {
                route, admitted, ..
            } => Self::Admitted {
                workspace: route.cluster_id.clone(),
                admitted: admitted.clone(),
            },
        }
    }

    /// Who `caller` was admitted as: certified, with the ownership its api
    /// holds, when it holds one.
    pub(super) fn admitted(
        caller: Option<&tilo_actions::remote::Caller>,
        origin: Option<&InvocationCaller>,
    ) -> Option<Self> {
        match (caller, origin) {
            (
                Some(direct),
                Some(InvocationCaller::NativePerson {
                    workspace,
                    certificate,
                    time,
                    ..
                }),
            ) => Some(Self::NativePerson {
                workspace: workspace.clone(),
                certificate: certificate.clone(),
                time: time.clone(),
                direct: direct.clone(),
            }),
            (
                Some(direct),
                Some(InvocationCaller::Certified {
                    caller,
                    foreign_restriction,
                }),
            ) => Some(Self::Certified {
                caller: caller.clone(),
                foreign_restriction: *foreign_restriction,
                direct: Some(direct.clone()),
            }),
            (Some(caller), _) => Some(Self::Caller(caller.clone())),
            (None, _) => origin.map(Self::from_origin),
        }
    }

    /// The caller as they stand now: a certified caller whose certificate
    /// no longer verifies, who changed, or whose execution ended, is nobody.
    /// A certified caller keeps their own device, so they stay held to the
    /// foreign-device cap wherever that device does not govern the workspace.
    pub(super) fn current(&self, state: &AppBrokerState) -> Option<tilo_actions::remote::Caller> {
        match self {
            Self::NativePerson {
                workspace,
                certificate,
                time,
                direct,
            } => {
                state.checked_policy_time(Some(time)).ok()?;
                state.current_native_person(workspace, certificate).ok()?;
                (direct.execution_id().is_none() || direct.execution_is_live())
                    .then(|| direct.clone())
            }
            Self::Caller(caller) => Some(caller.clone()),
            Self::Certified {
                caller,
                foreign_restriction,
                direct,
            } => {
                let admitted = state.current_action_caller(caller).ok()?;
                match direct {
                    Some(direct) => (direct.execution_id().is_none() || direct.execution_is_live())
                        .then(|| direct.clone()),
                    None => {
                        Some(admitted.certified_caller(&caller.workspace, *foreign_restriction))
                    }
                }
            }
            Self::Admitted {
                workspace,
                admitted,
            } => Some(admitted.caller(workspace)),
        }
    }
}

impl InvocationEnvelope {
    /// The invocation as the app receives it on the collaboration wire.
    pub(super) fn on_wire(self) -> Result<runtime_collab::wire::Invocation, BrokerError> {
        Ok(runtime_collab::wire::Invocation {
            id: self.id,
            invocation: *self.invocation.0.as_bytes(),
            resource: self.resource,
            action: self.action,
            argument: self.argument,
            actor: self.actor,
            context: self
                .context
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|error| BrokerError::failed(error.to_string()))?,
            // The context's JSON leaves out what the person typed: it goes beside it, to this
            // app alone, from the approve the asked record handed back in memory.
            typed: runtime_collab::wire::Typed(
                self.context
                    .as_ref()
                    .and_then(|context| context.approval.as_ref())
                    .map(|approval| approval.typed.clone())
                    .unwrap_or_default(),
            ),
        })
    }
}

impl AppBrokerState {
    pub(super) fn publish_pending_activity(
        &self,
        pending: &PendingInvocation,
        update: &tilo_actions::InvocationUpdate,
    ) {
        if pending.cancellation_requested_at.is_some() && !update.is_terminal() {
            self.execution_activity.publish_status(
                pending.activity.as_ref(),
                ExecutionStatus::CancellationRequested,
            );
        } else {
            self.publish_activity(pending.activity.as_ref(), update);
        }
    }

    #[expect(
        clippy::significant_drop_tightening,
        reason = "the pending entry borrows the registry until it is staged; the lint misreads the borrow"
    )]
    pub(super) async fn apply_invocation_update(
        self: &Arc<Self>,
        peer_id: u64,
        id: u64,
        update: tilo_actions::InvocationUpdate,
        identity: Option<(CrdtRoute, AdmittedJoinIdentity)>,
    ) -> Result<(), BrokerError> {
        let state = self;
        // Copy only the local routing key before awaiting; never hold the registry lock across the owner turn.
        let owner_workspace = state
            .pending_invocations
            .lock()
            .map_err(|_| BrokerError::failed("invocation registry unavailable".into()))?
            .get(&id)
            .map(|pending| pending.cluster_id.clone());
        let requires_owner = state.durable_owner_required
            || owner_workspace
                .as_ref()
                .is_some_and(|workspace| state.publisher.records_effects_for(workspace));
        let _owner_order = if requires_owner {
            Some(state.owner_commit_order.lock().await)
        } else {
            None
        };
        if requires_owner && let Some((route, identity)) = &identity {
            state.revalidate_admission(route, identity)?;
        }
        let (staged, next_sequence) = {
            let registry = state
                .pending_invocations
                .lock()
                .map_err(|_| BrokerError::failed("invocation registry unavailable".into()))?;
            let pending = registry
                .get(&id)
                .filter(|pending| pending.peer_id == peer_id)
                .ok_or_else(|| BrokerError::failed("unknown invocation id".into()))?;
            let next_sequence = invocation_update::validate_invocation_update(pending, &update)?;
            (pending.clone(), next_sequence)
        };
        state
            .commit_effect_before_terminal(&staged, &update)
            .await?;
        if requires_owner && let Some((route, identity)) = &identity {
            state.revalidate_admission(route, identity)?;
        }
        state
            .settle_definite_outcome(peer_id, id, &staged.update, &update)
            .await?;
        let mut registry = state
            .pending_invocations
            .lock()
            .map_err(|_| BrokerError::failed("invocation registry unavailable".into()))?;
        let pending = registry
            .get_mut(&id)
            .filter(|pending| pending.peer_id == peer_id && pending.update == staged.update)
            .ok_or_else(|| {
                BrokerError::failed("invocation changed while committing; outcome unknown".into())
            })?;
        pending.last_activity = tilo_clock::now();
        pending.last_sequence = next_sequence;
        pending.update = update.clone();
        state.publish_pending_activity(pending, &update);
        state.invocation_changed();
        if update.is_terminal() {
            let completion = match &update {
                tilo_actions::InvocationUpdate::Succeeded { .. } => InvocationCompletion {
                    code: String::new(),
                    message: String::new(),
                },
                tilo_actions::InvocationUpdate::Failed { error, .. } => InvocationCompletion {
                    code: error.code.clone(),
                    message: error.message.clone(),
                },
                tilo_actions::InvocationUpdate::Cancelled { message, .. } => InvocationCompletion {
                    code: "action_cancelled".into(),
                    message: message.clone().unwrap_or_default(),
                },
                _ => {
                    return Err(BrokerError::failed(
                        "nonterminal invocation reached terminal handling".into(),
                    ));
                }
            };
            let pending = registry
                .remove(&id)
                .ok_or_else(|| BrokerError::failed("unknown invocation id".into()))?;
            // Retire it before releasing the registry: a waiter woken by the
            // change above must find it pending or completed, never neither.
            state.record_completion(id, pending.completion_owner(), update.clone());
            drop(registry);
            if let Ok(mut requests) = state.cancellation_requests.lock() {
                requests.remove(&id);
            }
            let _ = pending.tx.send(completion);
        } else {
            drop(registry);
        }
        Ok(())
    }
}

impl AppBrokerState {
    pub(super) fn start_invocation_confirmed_for_origin(
        &self,
        peer_id: u64,
        resource: &str,
        action: &str,
        inputs: InvocationInputs<'_>,
        origin: Option<InvocationCaller>,
    ) -> Result<StagedJob, Refusal> {
        let offered = self
            .publishers
            .surfaces
            .lock()
            .ok()
            .and_then(|surfaces| surfaces.get(&peer_id).cloned())
            .and_then(|surface| {
                let action = surface
                    .actions
                    .iter()
                    .find(|candidate| candidate.resource == resource && candidate.action == action)
                    .cloned()?;
                Some((surface, action))
            })
            .ok_or_else(|| "the application no longer publishes that action".to_string())?;
        let _admission = self
            .admission_lock
            .lock()
            .map_err(|_| "invocation admission unavailable")?;
        self.dispatch_invocation_from_offered_owned(
            peer_id,
            offered,
            inputs,
            AdmissionProvenance {
                revision: None,
                origin,
                retained_time: None,
            },
        )
    }
    pub(super) fn dispatch_invocation_from_offered_owned(
        &self,
        peer_id: u64,
        offered: (PublishedSurface, PublishedAction),
        inputs: InvocationInputs<'_>,
        provenance: AdmissionProvenance<'_>,
    ) -> Result<StagedJob, Refusal> {
        self.admit_invocation_from_offered_owned(peer_id, offered, inputs, provenance, None)
    }
    /// The intent an admission of `offered` is recorded with: the exact
    /// request the caller asked for, and everything that decides it. Built
    /// once, so a repeated invocation is compared with what was admitted.
    pub(super) fn invocation_intent(
        offered: &(PublishedSurface, PublishedAction),
        inputs: IntentInputs<'_>,
    ) -> serde_json::Value {
        let IntentInputs {
            peer_id,
            actor,
            argument,
            revision,
            original_request,
            local_report,
            typed_local_arguments,
            receipt_context,
        } = inputs;
        let request = serde_json::json!({
            "peer_id": peer_id, "app_id": offered.0.app_id, "document_id": offered.0.route.document_id,
            "resource": offered.1.resource, "action": offered.1.action, "argument": argument,
            "original_request": original_request,
        });
        let request = match local_report {
            Some(report) => serde_json::json!({
                "kind": "local-document-action-report-v1",
                "claim": "semantic-admission-and-app-reported-input-and-hashes-not-execution",
                "resolved_request": {
                    "app_id": offered.0.app_id,
                    "document_id": offered.0.route.document_id,
                    "resource": offered.1.resource,
                    "action": offered.1.action,
                    "argument": argument,
                    "original_request": original_request,
                },
                "report": report,
                "typed_arguments": typed_local_arguments,
            }),
            None => request,
        };
        serde_json::json!({ "request": request,
            "contract": offered.1.contract, "operation_kind": offered.1.operation_kind,
            "context": receipt_context, "actor": actor.member(), "app_id": offered.0.app_id,
            "document_id": offered.0.route.document_id,
            "resource": offered.1.resource, "resource_kind": offered.1.resource_kind,
            "action": offered.1.action, "revision": revision })
    }
    /// What `record`'s invocation is answered with when it is asked for
    /// again: its retained outcome, on a fresh local handle, with nothing
    /// delivered and nothing admitted.
    pub(super) fn retained_answer(&self, record: &DurableInvocation) -> StagedJob {
        self.record_completion(
            record.wire_id,
            record.key.owner.clone(),
            record.update.clone(),
        );
        let (_sent, completion) = std::sync::mpsc::channel();
        StagedJob {
            admitted: invocation_journal::Durable::settled(),
            start: Admitted::Reported(InvocationJob {
                id: record.wire_id,
                completion,
            }),
        }
    }
    /// Retire only this run's transient presence, preserving its pending jobs.
    pub fn retire_execution(&self, caller: &tilo_actions::remote::Caller) {
        caller.retire_execution();
        self.live_views.retire(caller);
        if let Ok(mut pending) = self.pending_invocations.lock() {
            for invocation in pending.values_mut() {
                if invocation
                    .activity
                    .as_ref()
                    .is_some_and(|target| target.belongs_to(caller))
                {
                    self.execution_activity
                        .publish_status(invocation.activity.as_ref(), ExecutionStatus::Ended);
                    if let Some(target) = &mut invocation.activity {
                        target.retire_scope();
                    }
                }
            }
        }
    }
    /// Observe subsequent broker execution events without scheduling app work.
    /// Report an invocation's progress on the observation bus, and in the
    /// view it runs in for an actor that view does not already show: an
    /// agent, an app, someone acting for a person, a person on another
    /// device. Nothing is performed out of sight; the view names who acts.
    pub(super) fn publish_activity(
        &self,
        target: Option<&ActivityTarget>,
        update: &tilo_actions::InvocationUpdate,
    ) {
        self.execution_activity.publish(target, update);
        let Some((target, shown)) = target.and_then(|target| Some((target, target.shown()?)))
        else {
            return;
        };
        let phase = match update {
            tilo_actions::InvocationUpdate::Accepted { .. }
            | tilo_actions::InvocationUpdate::Progress { .. }
            | tilo_actions::InvocationUpdate::Item { .. } => {
                tilo_actions::InteractionPhase::Applying
            }
            tilo_actions::InvocationUpdate::Succeeded { .. } => {
                tilo_actions::InteractionPhase::Committed
            }
            tilo_actions::InvocationUpdate::Failed { error, .. } => {
                tilo_actions::InteractionPhase::Failed {
                    code: Some(error.code.clone()),
                    message: error.message.clone(),
                }
            }
            tilo_actions::InvocationUpdate::Cancelled { .. } => {
                tilo_actions::InteractionPhase::Cancelled { message: None }
            }
        };
        // What it wrote is known once it applied: the characters, or the item it made, where
        // they landed, rather than the place it named.
        let landed = match update {
            tilo_actions::InvocationUpdate::Succeeded { output, .. } => shown.landed(output),
            _ => None,
        };
        let shown = landed.as_ref().unwrap_or(shown);
        self.publish_interaction(target.route(), shown, update.invocation(), phase);
    }
    /// Show `shown` in the view `route` names, in `phase` of `invocation`.
    pub(super) fn publish_interaction(
        &self,
        route: &CrdtRoute,
        shown: &execution_activity::ShownActor,
        invocation: tilo_actions::InvocationId,
        phase: tilo_actions::InteractionPhase,
    ) {
        let sequence = shown
            .sequence
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .saturating_add(1);
        let interaction = tilo_actions::SemanticInteraction {
            version: tilo_actions::INTERACTION_VERSION,
            invocation,
            sequence,
            actor: shown.actor.clone(),
            phase,
            action: shown.action.clone(),
            resource: shown.resource.clone(),
            anchor: shown.anchor.clone(),
            group: shown.group.clone(),
        };
        if interaction.validate().is_err() {
            return;
        }
        let Some(member) = shown
            .actor
            .members()
            .first()
            .map(|member| (*member).clone())
        else {
            return;
        };
        let key = PresenceKey::resource(&route.app_id, &shown.key.0, &shown.key.1);
        let payload = PresencePayload::Interaction(Box::new(interaction));
        let event = CollabEvent::RemotePresenceUpdate {
            member,
            key,
            payload,
        };
        if let Some(fanout) = self.fanout.upgrade() {
            // No connection is peer 0: the view the action runs in sees it too.
            fanout.fanout_to_route(&event, 0, &route.to_cluster_scope());
        }
    }
    /// The fixed 256-event buffer reports `Lagged` rather than concealing loss;
    /// consumers must clear stale presence on lag/closure. Dropping a receiver
    /// changes neither sessions nor app state. This is host-local provenance,
    /// not a substitute for authenticated transport or current workspace authority.
    #[must_use]
    pub fn subscribe_execution_activity(
        &self,
    ) -> tokio::sync::broadcast::Receiver<Arc<ExecutionActivity>> {
        self.execution_activity.subscribe()
    }
    /// Changes whenever a pending or completed invocation changes.
    #[must_use]
    pub fn invocations_generation(&self) -> u64 {
        *self.invocation_generation.borrow()
    }
    /// Record that a pending or completed invocation changed, waking every
    /// [`Self::invocation_changes`] waiter.
    #[track_caller]
    pub(super) fn invocation_changed(&self) {
        self.invocation_generation
            .send_modify(|generation| *generation = generation.wrapping_add(1));
        tracing::debug!(
            target: "tilo::publications",
            generation = *self.invocation_generation.borrow(),
            cause = %std::panic::Location::caller(),
            "invocation statuses changed"
        );
    }
    /// A receiver that sees every later invocation change; what it has seen
    /// so far counts as seen.
    pub(crate) fn invocation_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.invocation_generation.subscribe()
    }
    /// A receiver that sees every later published-surface change; what it
    /// has seen so far counts as seen. What a session holding a window reads
    /// to notice the app publishing what it now offers — a page holding a
    /// request that nothing has answered yet — without asking again.
    pub fn surface_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.publishers.changes.subscribe()
    }
    /// Whether job `id`'s action declares untrusted content.
    pub fn untrusted_job(&self, id: u64) -> bool {
        self.untrusted_jobs
            .lock()
            .is_ok_and(|untrusted| untrusted.contains(&id))
    }
    pub fn invocation_status(&self, id: u64) -> Option<tilo_actions::InvocationUpdate> {
        self.expire_stale_invocations();
        if let Some(update) = self.live_views.status(id, None) {
            return Some(update);
        }
        self.pending_invocations
            .lock()
            .ok()
            .and_then(|pending| pending.get(&id).map(|job| job.update.clone()))
            .or_else(|| {
                self.completed_invocations
                    .lock()
                    .ok()
                    .and_then(|jobs| jobs.get(&id).map(|(_, update)| update.clone()))
            })
    }
    pub(super) fn invocation_status_for_owner(
        &self,
        id: u64,
        owner: &InvocationOwner,
    ) -> Option<tilo_actions::InvocationUpdate> {
        self.expire_stale_invocations();
        if let Some(update) = self.live_views.status(id, Some(owner)) {
            return Some(update);
        }
        self.pending_invocations
            .lock()
            .ok()
            .and_then(|jobs| {
                jobs.get(&id)
                    .filter(|job| &job.owner == owner)
                    .map(|job| job.update.clone())
            })
            .or_else(|| {
                self.completed_invocations.lock().ok().and_then(|jobs| {
                    jobs.get(&id)
                        .filter(|(retained, _)| retained == owner)
                        .map(|(_, update)| update.clone())
                })
            })
    }
    pub fn invocation_statuses(
        &self,
        actor: &MemberId,
    ) -> Vec<(u64, tilo_actions::InvocationUpdate)> {
        self.invocation_statuses_for_owner(&InvocationOwner::Local {
            actor: actor.clone(),
        })
    }
    pub(super) fn invocation_statuses_for_owner(
        &self,
        owner: &InvocationOwner,
    ) -> Vec<(u64, tilo_actions::InvocationUpdate)> {
        self.expire_stale_invocations();
        let mut jobs = self
            .pending_invocations
            .lock()
            .map(|pending| {
                pending
                    .iter()
                    .filter(|(_, job)| &job.owner == owner)
                    .map(|(id, job)| (*id, job.update.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if let Ok(completed) = self.completed_invocations.lock() {
            jobs.extend(
                completed
                    .iter()
                    .filter(|(_, (retained, _))| retained == owner)
                    .map(|(id, (_, update))| (*id, update.clone())),
            );
        }
        jobs.sort_by_key(|(id, _)| *id);
        jobs
    }
    /// Request cooperative cancellation for an actor-owned invocation.
    pub(crate) fn request_cancellation(
        &self,
        id: u64,
        actor: &MemberId,
    ) -> Result<invocation_journal::Durable, String> {
        self.request_cancellation_for_owner(
            id,
            &InvocationOwner::Local {
                actor: actor.clone(),
            },
        )
    }
    /// Ask invocation `id`'s application to stop, for `owner`. The asker is
    /// answered once the returned write resolves.
    pub(super) fn request_cancellation_for_owner(
        &self,
        id: u64,
        owner: &InvocationOwner,
    ) -> Result<invocation_journal::Durable, String> {
        self.expire_stale_invocations();
        if let Some(cancelled) = self.live_views.cancel(id, Some(owner)) {
            return cancelled.map(|()| invocation_journal::Durable::settled());
        }
        // A host step that declared itself cancellable stops on its own flag, and answers as
        // cancelled when it does. Such a step keeps no pending invocation of its own.
        if self.stop_host_step(id) {
            return Ok(invocation_journal::Durable::settled());
        }
        let mut pending = self
            .pending_invocations
            .lock()
            .map_err(|_| "the invocation registry is unavailable".to_string())?;
        let job = pending
            .get(&id)
            .ok_or_else(|| "the invocation is not running".to_string())?;
        if &job.owner != owner {
            return Err("only the invocation owner may cancel it".into());
        }
        if !job.cancellable {
            return Err("the action did not declare cooperative cancellation".into());
        }
        let job = pending
            .get_mut(&id)
            .ok_or_else(|| "the invocation is not running".to_string())?;
        let durable = self.persist_cancellation(job)?;
        if job.cancellation_requested_at.is_none() {
            job.cancellation_requested_at = Some(tilo_clock::now());
            self.execution_activity.publish_status(
                job.activity.as_ref(),
                ExecutionStatus::CancellationRequested,
            );
            self.invocation_changed();
        }
        drop(pending);
        self.cancellation_requests
            .lock()
            .map_err(|_| "the cancellation registry is unavailable".to_string())?
            .insert(id);
        Ok(durable)
    }
    pub fn can_cancel(&self, id: u64, actor: &MemberId) -> bool {
        self.can_cancel_for_owner(
            id,
            &InvocationOwner::Local {
                actor: actor.clone(),
            },
        )
    }
    pub(super) fn can_cancel_for_owner(&self, id: u64, owner: &InvocationOwner) -> bool {
        self.expire_stale_invocations();
        self.pending_invocations
            .lock()
            .ok()
            .and_then(|pending| {
                pending
                    .get(&id)
                    .map(|job| job.cancellable && &job.owner == owner)
            })
            .unwrap_or(false)
    }
    pub(super) fn disconnect_device_media(&self, peer_id: u64) {
        if let Some(media) = self.services.native_media.as_ref() {
            media.disconnect(peer_id);
        }
        if let Some(output) = self.services.audio_output.as_ref() {
            output.disconnect(peer_id);
        }
    }
    /// A connection ended: every route it held is withdrawn, then what the
    /// connection itself owned.
    pub(super) fn remove_connection(&self, peer_id: u64) {
        for hold in self.connection_holds(peer_id) {
            self.withdraw_route_hold(hold);
        }
        self.launch_disconnected(peer_id);
        self.forget_windows_of(peer_id);
        self.disconnect_device_media(peer_id);
        if let Some(realtime) = self.services.realtime.as_ref() {
            realtime.disconnect(peer_id);
        }
        self.disconnect_file_transfers(peer_id);
        if let Ok(mut subscribers) = self.action_catalogue_subscribers.lock() {
            subscribers.remove(&peer_id);
        }
    }
    /// One route hold ended ([`route_holds`]): withdraw what it published
    /// and worked as there, and fail what was still pending on it.
    pub(super) fn withdraw_route_hold(&self, peer_id: u64) {
        self.release_route_hold(peer_id);
        if let Ok(mut directories) = self.canonical_directories.lock() {
            directories.remove(peer_id);
        }
        // Withdraw provenance before any stale surface can be observed during cleanup.
        match self.host_workers.lock() {
            Ok(mut workers) => workers.remove(peer_id),
            Err(poisoned) => poisoned.into_inner().remove(peer_id),
        }
        if let Ok(mut tables) = self.surface_tables.lock() {
            tables.remove(&peer_id);
        }
        if let Ok(mut surfaces) = self.publishers.surfaces.lock()
            && surfaces.remove(&peer_id).is_some()
        {
            self.surfaces_changed();
        }
        if let Ok(mut roots) = self.action_directory_roots.lock() {
            roots.remove(&peer_id);
        }
        if let Ok(mut checkouts) = self.native_checkouts.lock() {
            checkouts.remove(&peer_id);
        }
        self.refresh_action_directory_root_snapshot();
        self.end_invocations(peer_id);
        self.catalogue_changed();
    }
    /// `peer_id` went away: its invocation stream ends, and each invocation
    /// still pending on it fails, or its outcome is unknown where it was
    /// journaled.
    pub(super) fn end_invocations(&self, peer_id: u64) {
        if let Ok(mut capabilities) = self.stable_invocation_subscribers.lock() {
            capabilities.remove(&peer_id);
        }
        // Gone already when its app ended the stream as it closed.
        let ended_by_app = !self
            .publishers
            .streams
            .lock()
            .is_ok_and(|mut subscribers| subscribers.remove(&peer_id).is_some());
        if !ended_by_app {
            self.publishers.streams_changed();
        }
        let unfinished: Vec<(u64, PendingInvocation)> = {
            let Ok(mut pending) = self.pending_invocations.lock() else {
                return;
            };
            let ids = pending
                .iter()
                .filter_map(|(id, invocation)| {
                    (invocation.peer_id == peer_id && !self.committing(*id)).then_some(*id)
                })
                .collect::<Vec<_>>();
            ids.into_iter()
                .filter_map(|id| pending.remove(&id).map(|invocation| (id, invocation)))
                .collect()
        };
        for (id, invocation) in unfinished {
            tracing::warn!(
                peer_id,
                id,
                ended_by_app,
                update = ?invocation.update,
                "collaboration broker: an application went away holding an invocation it never answered"
            );
            self.answer_unfinished(
                id,
                &invocation,
                "application disconnected before completing the action",
                "application outcome unknown after disconnect",
            );
        }
    }
    /// Answer invocation `id` of `peer_id`, whose terminal update could not
    /// be recorded (`error`), with that reason: its asker hears why, never
    /// that its peer went away. The peer goes on serving the others.
    pub(super) fn fail_unrecorded_invocation(&self, peer_id: u64, id: u64, error: &str) {
        let Some(invocation) = self
            .pending_invocations
            .lock()
            .ok()
            .and_then(|mut pending| {
                pending
                    .get(&id)
                    .is_some_and(|invocation| invocation.peer_id == peer_id)
                    .then(|| pending.remove(&id))
                    .flatten()
            })
        else {
            return;
        };
        let failed = format!("the action's outcome could not be recorded: {error}");
        self.answer_unfinished(id, &invocation, &failed, &failed);
    }
    /// Answer `invocation`, taken out of the pending registry unfinished:
    /// an unjournaled one fails with `failed`, a journaled one's outcome is
    /// unknown, said as `unknown`.
    pub(super) fn answer_unfinished(
        &self,
        id: u64,
        invocation: &PendingInvocation,
        failed: &str,
        unknown: &str,
    ) {
        invocation.cancellation_fence.store(true, Ordering::Release);
        let (update, completion) = if invocation.journal_key.is_some() {
            (
                invocation_journal::unknown(invocation.update.invocation()),
                InvocationCompletion {
                    code: "action_outcome_unknown".into(),
                    message: unknown.into(),
                },
            )
        } else {
            (
                tilo_actions::InvocationUpdate::Failed {
                    invocation: invocation.update.invocation(),
                    error: tilo_actions::ActionFailure {
                        code: "action_failed".into(),
                        message: failed.into(),
                        details: None,
                        retryable: invocation.retryable_on_disconnect,
                    },
                },
                InvocationCompletion {
                    code: "action_failed".into(),
                    message: failed.into(),
                },
            )
        };
        if self.persist_uncertain_outcome(invocation, &update).is_err() {
            // Its uncertainty is not durable either: its asker still hears
            // that no outcome comes, rather than waiting on nobody.
            self.execution_activity.publish_status(
                invocation.activity.as_ref(),
                ExecutionStatus::OutcomeUnknown,
            );
            let _ = invocation.tx.send(InvocationCompletion {
                code: "action_outcome_unknown".into(),
                message: unknown.into(),
            });
            return;
        }
        self.publish_activity(invocation.activity.as_ref(), &update);
        self.record_completion(id, invocation.completion_owner(), update);
        if let Ok(mut requests) = self.cancellation_requests.lock() {
            requests.remove(&id);
        }
        let _ = invocation.tx.send(completion);
    }
    pub(super) fn disconnect_file_transfers(&self, peer_id: u64) {
        if let Some(service) = self.services.browser_service_sessions.as_ref() {
            service.disconnect(peer_id);
        }
        if let Some(nearby) = self.services.nearby_transfer.as_ref() {
            nearby.disconnect(peer_id);
        }
        if let Some(clipboard) = self.services.file_clipboard.as_ref() {
            clipboard.disconnect(peer_id);
        }
    }
}
