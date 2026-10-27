//! Native runs are recoverable from the canonical approved task, not a queue grant.
pub(super) use super::native_task_launch::NativeLaunch;
use super::{
    AdmissionProvenance, CapnpBrokerState, DurableContext, InvocationInputs, InvocationKey,
    InvocationOwner, PendingTaskRun, StagedJob, TaskRunEnvelope, durable_local_invocation,
};
use crate::services::NativeTaskBinding;
use std::sync::Arc;
use tokio::sync::mpsc;
#[cfg(test)]
#[path = "native_task_tests.rs"]
mod tests;

/// What a native task's re-ask of its child asked to wait for: the child
/// moving past `seen` (with no `seen`, settling) until `until`, or a child
/// that waits for its person being decided.
#[derive(Clone, Copy)]
pub(super) struct ChildWait {
    seen: Option<tilo_actions::remote::UpdateMark>,
    until: tokio::time::Instant,
}

impl ChildWait {
    /// The wait a re-ask asked for, capped at
    /// [`tilo_actions::remote::MAX_REMOTE_WAIT`]; `None` for an answer now.
    pub(super) fn read(
        request: &super::channel_handle::invoke_task_action_params::Reader<'_>,
    ) -> Result<Option<Self>, capnp::Error> {
        let wait_ms = request.get_wait_ms();
        if wait_ms == 0 {
            return Ok(None);
        }
        let seen = request.get_seen_json()?.to_str()?.trim();
        let seen = if seen.is_empty() {
            None
        } else {
            Some(serde_json::from_str(seen).map_err(|_| {
                capnp::Error::failed("task child wait names an invalid update mark".into())
            })?)
        };
        let wait = std::time::Duration::from_millis(u64::from(wait_ms))
            .min(tilo_actions::remote::MAX_REMOTE_WAIT);
        let now = tokio::time::Instant::now();
        Ok(Some(Self {
            seen,
            until: now.checked_add(wait).unwrap_or(now),
        }))
    }

    fn arrived(&self, update: &tilo_actions::InvocationUpdate) -> bool {
        update.is_terminal()
            || self
                .seen
                .is_some_and(|seen| tilo_actions::remote::UpdateMark::of(update) != seen)
    }
}

/// A watch that notifies the returned wake on the next
/// [`tilo_identity_host::policy_change`] announcement after each arming.
fn decision_watch() -> (
    Arc<tokio::sync::Notify>,
    tilo_identity_host::policy_change::Watch,
) {
    let wake = Arc::new(tokio::sync::Notify::new());
    let notify = Arc::clone(&wake);
    let watch = tilo_identity_host::policy_change::Watch::new(move || notify.notify_one());
    (wake, watch)
}

/// A native task's request about its child: the run, the child's stable
/// id, the action it names and the authenticated caller.
pub(super) struct ChildReask<'a> {
    pub peer_id: u64,
    pub agent_id: &'a str,
    pub run_id: u64,
    pub lease: &'a str,
    pub invocation: tilo_actions::InvocationId,
    pub target: runtime_collab::TaskAction<'a>,
    pub caller: &'a durable_local_invocation::LocalCaller,
}

pub(super) fn child_provenance(
    lease: &str,
    invocation: &str,
    caller: Option<durable_local_invocation::LocalCaller>,
) -> Result<
    Option<(
        durable_local_invocation::LocalCaller,
        tilo_actions::InvocationId,
    )>,
    capnp::Error,
> {
    if runtime_collab::native_task_revision(lease).is_none() {
        return Ok(None);
    }
    let id = uuid::Uuid::parse_str(invocation).map_err(|_| {
        capnp::Error::failed("native task child requires stable invocation UUID".into())
    })?;
    if id.is_nil() {
        return Err(capnp::Error::failed("nil task invocation".into()));
    }
    Ok(Some((
        caller.ok_or_else(|| {
            capnp::Error::failed("native task channel custody unavailable".into())
        })?,
        tilo_actions::InvocationId(id),
    )))
}

fn native_request(
    run: &PendingTaskRun,
    target: runtime_collab::TaskAction<'_>,
    app: &str,
    record: &str,
) -> serde_json::Value {
    let mut request = serde_json::json!({"task":run.task_id,
        "epoch":run.native.epoch,
        "peer_id":target.peer_id,"app":app,"record":record,
        "resource":target.resource,"action":target.action,"argument":target.argument});
    if !target.provider.is_empty()
        && let Some(request) = request.as_object_mut()
    {
        request.remove("peer_id");
        request.insert("provider".into(), target.provider.into());
    }
    request
}

impl CapnpBrokerState {
    pub(super) async fn invoke_native_task_child(
        self: &std::sync::Arc<Self>,
        reask: &ChildReask<'_>,
        wait: Option<ChildWait>,
        admission: (&super::CrdtRoute, &super::AdmittedJoinIdentity),
    ) -> Result<(u64, tilo_actions::InvocationUpdate), capnp::Error> {
        if let Some(observed) = self.observe_native_child(reask, wait, admission).await? {
            return Ok(observed);
        }
        // A child that waits for its person is held, when the re-ask asked
        // to be, until it is decided: the person answered the run's
        // question, a grant was given or revoked, or the run stopped. The
        // hold is this connection's call, so it also ends with the
        // connection; nothing ends it on a clock.
        if wait.is_none() {
            return self.start_native_child(reask).await;
        }
        let (wake, watch) = decision_watch();
        loop {
            let _ = watch.arm();
            match self.start_native_child(reask).await {
                Err(error)
                    if crate::services::NativeTaskActionRequirement::awaits_confirmation(
                        &error.to_string(),
                    ) => {}
                started => return started,
            }
            if self
                .task_run_cancelled(reask.peer_id, reask.agent_id, reask.run_id, reask.lease)
                .map_err(capnp::Error::failed)?
            {
                return Err(capnp::Error::failed(
                    "the task was cancelled while its step waited for the person".into(),
                ));
            }
            self.decision_may_change(&wake).await;
            let (route, identity) = admission;
            self.revalidate_admission(route, identity)?;
        }
    }

    /// Until what decides a held re-ask may have changed: announced through
    /// [`tilo_identity_host::policy_change`], which `wake` hears from a
    /// [`decision_watch`] armed before the last look, or, on a host that
    /// does not announce every such change, its next unannounced re-check.
    async fn decision_may_change(&self, wake: &tokio::sync::Notify) {
        if self.announces_decisions() {
            wake.notified().await;
        } else {
            let recheck = super::committed_document::next_unannounced_recheck();
            let _ = tokio::time::timeout_at(recheck, wake.notified()).await;
        }
    }

    /// Whether every change a run's admission reads is announced through
    /// [`tilo_identity_host::policy_change`].
    fn announces_decisions(&self) -> bool {
        self.membership
            .as_ref()
            .is_some_and(|membership| membership.announces_policy_changes())
            && tilo_identity_host::policy_change::timers_reliable()
    }

    /// Start `reask`'s child, once its person answered where it asks first.
    async fn start_native_child(
        self: &std::sync::Arc<Self>,
        reask: &ChildReask<'_>,
    ) -> Result<(u64, tilo_actions::InvocationUpdate), capnp::Error> {
        if !reask.target.provider.is_empty() {
            return self
                .launch_native_task_action(reask)
                .await
                .map_err(capnp::Error::failed);
        }
        let gate = self
            .native_confirmation_gate(reask)
            .map_err(capnp::Error::failed)?;
        let (argument, confirmation) = match gate {
            super::native_task_confirmation::NativeGate::Proceed => (None, None),
            super::native_task_confirmation::NativeGate::Confirmed {
                argument,
                confirmation,
            } => (Some(argument), Some(confirmation)),
        };
        let mut target = reask.target;
        if let Some(argument) = argument.as_deref() {
            target.argument = Some(argument);
        }
        let run = self
            .task_run_for(reask.peer_id, reask.agent_id, reask.run_id, reask.lease)
            .map_err(capnp::Error::failed)?;
        let offered = self
            .surfaces
            .lock()
            .map_err(|_| capnp::Error::failed("task surface unavailable".into()))?
            .get(&target.peer_id)
            .and_then(|surface| surface.action_projection(target.resource, target.action))
            .ok_or_else(|| capnp::Error::failed("task action unavailable".into()))?;
        let witness = self
            .native_store_witness(
                (&run, reask.lease, confirmation.is_some()),
                &offered,
                target.argument,
                reask.caller,
                reask.invocation,
            )
            .await
            .map_err(capnp::Error::failed)?;
        reask.caller.current(self).map_err(capnp::Error::failed)?;
        if witness.is_some() {
            self.check_native_store_offer(&offered)
                .map_err(capnp::Error::failed)?;
        }
        let provenance = (reask.caller.clone(), reask.invocation);
        let job = self
            .start_task_action(
                reask.peer_id,
                reask.agent_id,
                reask.run_id,
                reask.lease,
                target,
                (
                    Some(&provenance),
                    (confirmation.is_some(), witness.as_deref()),
                ),
            )
            .map_err(capnp::Error::failed)?
            .start(self)
            .await
            .map_err(capnp::Error::failed)?;
        let update = self.invocation_status(job.id).ok_or_else(|| {
            capnp::Error::failed("accepted task action has no broker state".into())
        })?;
        Ok((job.id, update))
    }

    /// What a native task's request finds of its child: `None` when it has
    /// not started (the request starts it), else the child's state as
    /// [`Self::native_task_observation`] reports it. With `wait`, a running
    /// child is held ([`Self::hold_native_observation`]) and then observed
    /// and authorized afresh, with the connection's admission revalidated
    /// (`admission`); a held re-ask never starts the child anew.
    pub(super) async fn observe_native_child(
        &self,
        reask: &ChildReask<'_>,
        wait: Option<ChildWait>,
        admission: (&super::CrdtRoute, &super::AdmittedJoinIdentity),
    ) -> Result<Option<(u64, tilo_actions::InvocationUpdate)>, capnp::Error> {
        let observe = || {
            let run =
                self.task_run_for(reask.peer_id, reask.agent_id, reask.run_id, reask.lease)?;
            self.native_task_observation(&run, reask.invocation, reask.target, reask.caller)
        };
        let (observed, wait) = match (observe().map_err(capnp::Error::failed)?, wait) {
            (Some(observed), Some(wait)) => (observed, wait),
            (observed, _) => return Ok(observed),
        };
        self.hold_native_observation(reask, observed, wait).await;
        let (route, identity) = admission;
        self.revalidate_admission(route, identity)?;
        observe()
            .map_err(capnp::Error::failed)?
            .ok_or_else(|| capnp::Error::failed("native task child is no longer recorded".into()))
            .map(Some)
    }

    /// Hold a native task's re-ask of its child, observed as `observed`,
    /// until the child moves past what the caller has seen or settles, the
    /// run is read cancelled (or can no longer be read), or the wait ends.
    /// It answers nothing itself: the caller observes and authorizes the
    /// child afresh once it returns. Between wakes it only looks at the
    /// child's status; it never starts, admits or authorizes anything.
    pub(super) async fn hold_native_observation(
        &self,
        reask: &ChildReask<'_>,
        observed: (u64, tilo_actions::InvocationUpdate),
        wait: ChildWait,
    ) {
        let (wire_id, mut update) = observed;
        // Subscribed before looking again, so a change between the
        // observation and this point still wakes the hold.
        let mut changes = self.invocation_changes();
        if let Some(current) = self.invocation_status(wire_id) {
            update = current;
        }
        // The run stopping is announced with the publication that records
        // it; reading it cancelled cancels the children, which ends the hold.
        let (wake, watch) = decision_watch();
        let mut signalled = true;
        while !wait.arrived(&update) {
            let _ = watch.arm();
            match self.task_run_cancelled(reask.peer_id, reask.agent_id, reask.run_id, reask.lease)
            {
                Ok(false) => {}
                Ok(true) | Err(_) => return,
            }
            tokio::select! {
                signal = changes.changed(), if signalled => {
                    // A broker going away stops signalling.
                    signalled = signal.is_ok();
                }
                () = self.decision_may_change(&wake) => {}
                () = tokio::time::sleep_until(wait.until) => return,
            }
            match self.invocation_status(wire_id) {
                Some(current) => update = current,
                None => return,
            }
        }
        tracing::trace!(invocation = %reask.invocation.0, "held native re-ask answered");
    }

    pub(super) fn native_task_observation(
        &self,
        run: &PendingTaskRun,
        invocation: tilo_actions::InvocationId,
        target: runtime_collab::TaskAction<'_>,
        caller: &durable_local_invocation::LocalCaller,
    ) -> Result<Option<(u64, tilo_actions::InvocationUpdate)>, String> {
        let binding = &run.native;
        if self.tracker_run_of(run)?.epoch != binding.epoch {
            return Err("native task observation source was replaced".into());
        }
        let record = {
            let guard = self
                .journal
                .lock()
                .map_err(|_| "native task journal unavailable")?;
            let journal = guard
                .as_ref()
                .map_err(Clone::clone)?
                .as_ref()
                .ok_or("native task journal unavailable")?;
            let found = journal.lookup(&InvocationKey {
                room: run.cluster_id.clone(),
                host: journal.host(),
                invocation,
                owner: InvocationOwner::NativeTask {
                    agent: run.agent_id.clone(),
                    task: run.task_id.clone(),
                    epoch: binding.epoch,
                },
            })?;
            drop(guard);
            found
        };
        let Some(record) = record else {
            return self.native_host_observation(run, target, caller, invocation);
        };
        let requirement = caller.authorize_retained_task(self, (&record, &run.native.actor()?))?;
        self.authorize_task_action(run, &requirement)?;
        let (app, record_id) = if target.provider.is_empty() {
            let offered = self
                .surfaces
                .lock()
                .map_err(|_| "native task target surface unavailable")?
                .get(&target.peer_id)
                .and_then(|surface| surface.action_projection(target.resource, target.action))
                .ok_or("native task target was removed")?;
            (offered.0.app_id, offered.0.route.record_id)
        } else {
            let app = record
                .intent
                .get("app_id")
                .and_then(serde_json::Value::as_str)
                .ok_or("retained task app absent")?
                .to_owned();
            let record_id = record
                .intent
                .get("record_id")
                .and_then(serde_json::Value::as_str)
                .ok_or("retained task record absent")?
                .to_owned();
            (app, record_id)
        };
        let request = native_request(run, target, &app, &record_id);
        if !record.admitted_request_is(&request) {
            return Err("native task child identity was reused with another request".into());
        }
        if record.uncertain {
            return Err(
                "native task action outcome is uncertain; review history before retrying".into(),
            );
        }
        let update = self
            .invocation_status_for_owner(record.wire_id, &record.key.owner)
            .unwrap_or(record.update);
        Ok(Some((record.wire_id, update)))
    }

    pub(super) fn cancel_native_children(&self, run: &PendingTaskRun) -> Result<(), String> {
        let binding = &run.native;
        let owner = InvocationOwner::NativeTask {
            agent: run.agent_id.clone(),
            task: run.task_id.clone(),
            epoch: binding.epoch,
        };
        let keys = self
            .pending_invocations
            .lock()
            .map_err(|_| "native child registry unavailable")?
            .values()
            .filter(|pending| {
                pending.owner == owner
                    && pending.cluster_id == run.cluster_id
                    && pending.cancellable
            })
            .filter_map(|pending| pending.journal_key.clone())
            .collect::<Vec<_>>();
        for key in keys {
            let record = {
                let guard = self
                    .journal
                    .lock()
                    .map_err(|_| "native child journal unavailable")?;
                guard
                    .as_ref()
                    .map_err(Clone::clone)?
                    .as_ref()
                    .ok_or("native child journal unavailable")?
                    .lookup(&key)?
            };
            if let Some(record) = record {
                // The run has ended; nobody waits on its children's stop.
                // Each hears of it only once durable (`invocation_cancellation`).
                drop(self.cancel_durable_record(&record)?);
            }
        }
        Ok(())
    }
    pub(super) fn start_native_task_invocation(
        &self,
        run: &PendingTaskRun,
        target: runtime_collab::TaskAction<'_>,
        caller: &durable_local_invocation::LocalCaller,
        invocation: tilo_actions::InvocationId,
        offered: (super::PublishedSurface, super::PublishedAction),
        (confirmed, witness): (bool, Option<&str>),
    ) -> Result<StagedJob, String> {
        // Dispatch the exact contract checked by native task admission. A
        // concurrent publication must not replace it with a broader need.
        if offered.0.route.cluster_id != run.cluster_id {
            return Err("task target changed workspace".into());
        }
        let intent = native_request(run, target, &offered.0.app_id, &offered.0.route.record_id);
        let actor = run.native.actor()?;
        // Admission checked the person's answer for this exact invocation;
        // the token carries it to the dispatch, bound to these arguments.
        let token = if super::requires_store_witness(&offered.0.app_id, &offered.1.action) {
            self.confirmation_token(
                target.peer_id,
                &offered.0.app_id,
                &offered.1,
                target.argument,
                &actor,
                witness
                    .map(|token| tilo_actions::remote::Confirmation::Witnessed(token.into()))
                    .as_ref(),
                &self.native_witness_binding(actor.id(), &offered, target.argument, invocation),
            )
            .map_err(|error| error.to_string())?
        } else {
            confirmed
                .then(|| {
                    self.mint_confirmation_grant(
                        target.peer_id,
                        &offered.1,
                        target.argument,
                        &actor,
                    )
                })
                .transpose()?
        };
        let app = offered.0.app_id.clone();
        let _admission = self
            .admission_lock
            .lock()
            .map_err(|_| "task action admission unavailable")?;
        self.dispatch_invocation_from_offered_owned(
            target.peer_id,
            offered,
            InvocationInputs {
                original_request: None,
                requested: Some(invocation),
                caller: None,
                execution: None,
                argument: target.argument,
                actor: &actor,
                confirmation_token: token.as_deref(),
                context: Some(run.action_context.clone()),
            },
            AdmissionProvenance {
                durable: Some(DurableContext::Task {
                    caller,
                    binding: &run.native,
                    invocation,
                    intent: &intent,
                }),
                revision: None,
                origin: None,
                retained_time: None,
            },
        )
        .inspect(|_| {
            self.launchable.note_view(&run.cluster_id, &actor.id, &app);
        })
    }

    pub(super) fn offer_native_run(
        &self,
        room: &str,
        binding: NativeTaskBinding,
        envelope: TaskRunEnvelope,
        subscriber: (u64, mpsc::UnboundedSender<TaskRunEnvelope>),
    ) -> Result<u64, String> {
        let (peer_id, tx) = subscriber;
        let run_id = envelope.run_id;
        let mut pending = self
            .pending_task_runs
            .lock()
            .map_err(|_| "task run registry unavailable")?;
        if let Some(prior) = pending.get(&run_id) {
            if prior.cluster_id != room
                || prior.task_id != binding.task_id
                || prior.agent_id != binding.agent_id
            {
                return Err("native task run identity collision".into());
            }
            if prior.peer_id == peer_id {
                return Ok(run_id);
            }
        }
        if pending.len() >= 256 && !pending.contains_key(&run_id) {
            return Err("native task delivery capacity reached; review running tasks".into());
        }
        pending.insert(
            run_id,
            PendingTaskRun {
                peer_id,
                agent_id: binding.agent_id.clone(),
                cluster_id: room.into(),
                task_id: binding.task_id.clone(),
                lease: envelope.lease.clone(),
                action_context: envelope.action_context.clone(),
                terminal: false,
                native: binding,
            },
        );
        drop(pending);
        if tx.send(envelope).is_err() {
            self.pending_task_runs
                .lock()
                .map_err(|_| "task run registry unavailable")?
                .remove(&run_id);
            return Err("agent task stream closed".into());
        }
        Ok(run_id)
    }

    pub(super) fn has_native_task_subscribers(&self) -> bool {
        self.task_subscribers.lock().is_ok_and(|subscribers| {
            subscribers
                .values()
                .any(|subscriber| !subscriber.tx.is_closed())
        })
    }

    pub(super) fn refresh_native_task_delivery(self: &Arc<Self>) {
        let rooms = match self.task_subscribers.lock() {
            Ok(subscribers) => {
                let rooms = subscribers
                    .values()
                    .map(|subscriber| subscriber.cluster_id.clone())
                    .collect::<std::collections::BTreeSet<_>>();
                drop(subscribers);
                rooms
            }
            Err(_) => return,
        };
        self.refresh_tracker_runs(&rooms);
    }

    pub(super) fn native_task_status(
        &self,
        run: &PendingTaskRun,
        lease: &str,
    ) -> Result<runtime_collab::TaskRunStatus, String> {
        self.tracker_status(run, lease)
    }
}
