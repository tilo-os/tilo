#[allow(
    clippy::wildcard_imports,
    reason = "one part of the broker module: the shared state, its imports and the other parts reach this through the parent glob"
)]
use super::*;

pub(super) struct PreparedEffectCommit {
    pub(super) update: tilo_actions::InvocationUpdate,
    pub(super) request: tilo_peer_state::owner_commit::OwnerCommitRequest,
}

impl AppBrokerState {
    #[cfg(test)]
    pub(super) fn authorize_durable_result(
        &self,
        caller: &VerifiedActionCaller,
        record: &DurableInvocation,
    ) -> Result<(), String> {
        let current = self.current_action_caller(caller)?;
        let actor = serde_json::to_value(current.broker_actor().member())
            .map_err(|error| error.to_string())?;
        if record.prepared_effect.is_some() && record.intent.get("actor") != Some(&actor) {
            return Err("prepared invocation actor differs from current caller".into());
        }
        let now = self.checked_policy_time(caller.policy_time.as_ref())?.1 / 1_000_000_000;
        let contract: tilo_actions::ActionContract =
            serde_json::from_value(record.intent.get("contract").cloned().unwrap_or_default())
                .map_err(|_| "journal action contract invalid")?;
        let app = record
            .intent
            .get("app_id")
            .and_then(serde_json::Value::as_str)
            .ok_or("journal app invalid")?;
        let resource = record
            .intent
            .get("resource")
            .and_then(serde_json::Value::as_str)
            .ok_or("journal resource invalid")?;
        let resource_kind = record
            .intent
            .get("resource_kind")
            .and_then(serde_json::Value::as_str)
            .ok_or("journal resource kind invalid")?;
        let action = record
            .intent
            .get("action")
            .and_then(serde_json::Value::as_str)
            .ok_or("journal action invalid")?;
        self.authorize_invocation_permission(
            &caller.workspace,
            (app, resource_kind, resource, action),
            &contract.need(),
            &current.broker_actor(),
            now,
        )?;
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn verified_invocation_status(
        &self,
        caller: &VerifiedActionCaller,
        invocation: tilo_actions::InvocationId,
    ) -> Result<Option<DurableInvocation>, String> {
        self.current_action_caller(caller)?;
        self.expire_stale_invocations();
        let key = self.caller_key(caller, invocation)?;
        let record = self
            .journal
            .lock()
            .map_err(|_| "invocation journal unavailable")?
            .as_ref()
            .map_err(Clone::clone)?
            .as_ref()
            .ok_or("durable admission unavailable")?
            .lookup(&key)?;
        if let Some(record) = &record {
            self.authorize_durable_result(caller, record)?;
        }
        Ok(record)
    }
    #[cfg(test)]
    pub(crate) fn start_verified_invocation(
        &self,
        caller: &VerifiedActionCaller,
        request: &CallerInvocation,
        confirmation: Option<&str>,
    ) -> Result<DurableInvocation, String> {
        let _admission = self
            .admission_lock
            .lock()
            .map_err(|_| "invocation admission unavailable")?;
        let admitted = self.current_action_caller(caller)?;
        if request.host != self.invocation_host()?
            || request.invocation.0.is_nil()
            || request.peer_id == collaboration::CURRENT_CONNECTION_PEER_ID
            || [
                &request.app_id,
                &request.document_id,
                &request.resource,
                &request.action,
            ]
            .iter()
            .any(|s| s.len() > 512)
            || request
                .argument
                .as_ref()
                .is_some_and(|s| s.len() > 256 * 1024)
        {
            return Err("invalid invocation target or bounds".into());
        }
        if let Some(record) = self.verified_invocation_status(caller, request.invocation)? {
            if !record.admitted_request_is(&request.intent()) {
                return Err("invocation identity reused with different intent".into());
            }
            return Ok(record);
        }
        let offered = self
            .publishers
            .surfaces
            .lock()
            .map_err(|_| "action catalogue unavailable")?
            .get(&request.peer_id)
            .cloned()
            .filter(|surface| {
                surface.live
                    && surface.route.cluster_id == caller.workspace
                    && surface.app_id == request.app_id
                    && surface.route.document_id == request.document_id
            })
            .and_then(|surface| {
                let action = surface
                    .actions
                    .iter()
                    .find(|a| a.resource == request.resource && a.action == request.action)?
                    .clone();
                Some((surface, action))
            })
            .ok_or("action no longer offered in this exact workspace/app/record")?;
        self.dispatch_invocation_from_offered(
            request.peer_id,
            offered,
            InvocationInputs {
                original_request: None,
                requested: None,
                caller: None,
                execution: None,
                argument: request.argument.as_deref(),
                actor: &admitted.broker_actor(),
                confirmation_token: confirmation,
                context: None,
            },
            Some((caller, request)),
            None,
        )?
        .start_blocking(self)?;
        self.verified_invocation_status(caller, request.invocation)?
            .ok_or_else(|| "admitted invocation missing".into())
    }
    #[cfg(test)]
    pub(crate) async fn cancel_verified_invocation(
        &self,
        caller: &VerifiedActionCaller,
        invocation: tilo_actions::InvocationId,
    ) -> Result<DurableInvocation, String> {
        let record = self
            .verified_invocation_status(caller, invocation)?
            .ok_or("unknown invocation")?;
        self.cancel_durable_document(&record)?.wait().await?;
        self.verified_invocation_status(caller, invocation)?
            .ok_or_else(|| "invocation missing".into())
    }
    /// Ask `record`'s application to stop. Answer the asker only once the
    /// returned write resolves; the application hears of it no sooner
    /// either (see `invocation_cancellation`).
    #[cfg(test)]
    pub(super) fn cancel_durable_document(
        &self,
        record: &DurableInvocation,
    ) -> Result<invocation_journal::Durable, String> {
        let mut registry = self
            .pending_invocations
            .lock()
            .map_err(|_| "invocation registry unavailable")?;
        let pending = registry
            .get_mut(&record.wire_id)
            .filter(|p| p.journal_key.as_ref() == Some(&record.key))
            .ok_or("invocation is not running")?;
        if !pending.cancellable {
            return Err("action did not declare cancellation".into());
        }
        let durable = self.persist_cancellation(pending)?;
        pending
            .cancellation_requested_at
            .get_or_insert_with(std::time::Instant::now);
        self.cancellation_requests
            .lock()
            .map_err(|_| "cancellation registry unavailable")?
            .insert(record.wire_id);
        drop(registry);
        Ok(durable)
    }
    /// Stage `pending`'s cancellation request and fence its effects off at
    /// once. The request is durable once the returned write resolves.
    pub(super) fn persist_cancellation(
        &self,
        pending: &PendingInvocation,
    ) -> Result<invocation_journal::Durable, String> {
        let durable = match &pending.journal_key {
            Some(key) => self
                .journal
                .lock()
                .map_err(|_| "invocation journal unavailable")?
                .as_mut()
                .map_err(|e| e.clone())?
                .as_mut()
                .ok_or("durable admission unavailable")?
                .record_cancellation(key)?,
            None => invocation_journal::Durable::settled(),
        };
        pending.cancellation_fence.store(true, Ordering::Release);
        Ok(durable)
    }
    /// The journal write invocation `id` must wait for before anyone hears
    /// of its last transition.
    pub(super) fn invocation_in_flight(&self, id: u64) -> invocation_journal::Durable {
        let key = self
            .pending_invocations
            .lock()
            .ok()
            .and_then(|pending| pending.get(&id).and_then(|job| job.journal_key.clone()));
        key.map_or_else(invocation_journal::Durable::settled, |key| {
            self.journal_in_flight(&key.invocation)
        })
    }
    /// Stage `pending`'s definite outcome `update`, to be heard only once
    /// it is durable ([`DefiniteOutcome::settle`]). Until then expiry and a
    /// disconnect leave the invocation alone: its outcome is being written.
    pub(super) fn stage_definite_outcome(
        &self,
        id: u64,
        pending: &PendingInvocation,
        update: &tilo_actions::InvocationUpdate,
    ) -> Result<DefiniteOutcome<'_>, String> {
        let Some(key) = &pending.journal_key else {
            return Ok(DefiniteOutcome {
                state: self,
                id: None,
                durable: invocation_journal::Durable::settled(),
            });
        };
        let durable = self
            .journal
            .lock()
            .map_err(|_| "invocation journal unavailable")?
            .as_mut()
            .map_err(|e| e.clone())?
            .as_mut()
            .ok_or("durable admission unavailable")?
            .record_update(key, update.clone(), false)?;
        // A progress update stages nothing of its own (it waits for what is
        // in flight), so only a terminal outcome holds expiry off.
        let id = update.is_terminal().then_some(id);
        if let Some(id) = id {
            self.committing_invocations
                .lock()
                .map_err(|_| "invocation registry unavailable")?
                .insert(id);
        }
        Ok(DefiniteOutcome {
            state: self,
            id,
            durable,
        })
    }
    /// Record `pending`'s uncertain outcome `update`. It claims nothing a
    /// restart would not answer anyway (recovery turns every unfinished
    /// invocation uncertain), so it is heard at once, its write in flight.
    pub(super) fn persist_uncertain_outcome(
        &self,
        pending: &PendingInvocation,
        update: &tilo_actions::InvocationUpdate,
    ) -> Result<(), String> {
        if let Some(key) = &pending.journal_key {
            // Not waited for: see above. The write still finishes in order.
            drop(
                self.journal
                    .lock()
                    .map_err(|_| "invocation journal unavailable")?
                    .as_mut()
                    .map_err(|e| e.clone())?
                    .as_mut()
                    .ok_or("durable admission unavailable")?
                    .record_update(key, update.clone(), true)?,
            );
        }
        Ok(())
    }
    /// Whether invocation `id`'s definite outcome is being written.
    pub(super) fn committing(&self, id: u64) -> bool {
        self.committing_invocations
            .lock()
            .is_ok_and(|committing| committing.contains(&id))
    }
    /// Keep `update` as `id`'s outcome, whoever ended it: its app, the deadline, or its connection
    /// going away.
    pub(super) fn record_completion(
        &self,
        id: u64,
        actor: InvocationOwner,
        update: tilo_actions::InvocationUpdate,
    ) {
        if update.is_terminal() {
            self.offload_answered(id, &update);
        }
        if let Ok(mut completed) = self.completed_invocations.lock() {
            completed.insert(id, (actor, update));
            self.invocation_changed();
            while completed.len() > Self::MAX_RETAINED_INVOCATIONS {
                let Some(oldest) = completed.keys().min().copied() else {
                    break;
                };
                completed.remove(&oldest);
                if let Ok(mut untrusted) = self.untrusted_jobs.lock() {
                    untrusted.remove(&oldest);
                }
            }
        }
    }
    pub(super) fn owner_health(&self) -> Result<(), String> {
        if self.durable_owner_required {
            // A probe: the committed authority must be readable; nothing is copied.
            self.membership
                .as_ref()
                .ok_or("current workspace authority unavailable")?
                .with_tree(&mut |_| {})?;
        }

        if let Some(reason) = self
            .owner_commit_failure
            .lock()
            .map_err(|_| "owner health unavailable")?
            .as_ref()
        {
            return Err(reason.clone());
        }
        self.publisher.owner_commit_health()
    }
    pub(super) async fn commit_owner_request(
        &self,
        request: tilo_peer_state::owner_commit::OwnerCommitRequest,
    ) -> Result<(), BrokerError> {
        self.owner_health().map_err(BrokerError::failed)?;
        let waiting = OwnerWait::on(&self.owner_commit_failure);
        let result = self.publisher.commit_owner(request, None).await;
        waiting.answered();
        result.map(drop).map_err(|reason| {
            // The owner answered: retain its actual refusal/failure for
            // later clients instead of labeling this as a dropped RPC.
            self.retain_owner_failure(&reason);
            BrokerError::failed(reason)
        })
    }
    /// Commit a document write of this device through the owner: the signed
    /// write as the record took it. A write the owner refuses is its writer's alone: it
    /// is answered the refusal and takes the write back out, and nothing else
    /// waits on it. An answer that does not prove the write leaves the owner
    /// unknown, as a dropped wait does.
    pub(super) async fn commit_owner_document(
        &self,
        request: tilo_peer_state::owner_commit::OwnerCommitRequest,
        resource_type: Option<String>,
    ) -> Result<SignedOp, BrokerError> {
        self.owner_health().map_err(BrokerError::failed)?;
        let waiting = OwnerWait::on(&self.owner_commit_failure);
        let answered = self
            .publisher
            .commit_owner_document(request.clone(), resource_type)
            .await;
        waiting.answered();
        let (committed, receipt) = answered.map_err(BrokerError::failed)?;
        receipt
            .validate_local_document(&request, &committed)
            .map_err(|reason| {
                self.retain_owner_failure(&reason);
                BrokerError::failed(reason)
            })?;
        match committed.operation {
            tilo_peer_state::owner_commit::CommitOperation::Document { envelope, .. } => {
                Ok(*envelope)
            }
            _ => Err(BrokerError::failed(
                "the owner committed no document".into(),
            )),
        }
    }
    pub(super) fn retain_owner_failure(&self, reason: &str) {
        if let Ok(mut failure) = self.owner_commit_failure.lock() {
            failure.get_or_insert_with(|| format!("owner commit failed: {reason}"));
        }
    }
    pub(super) async fn commit_effect_before_terminal(
        &self,
        pending: &PendingInvocation,
        update: &tilo_actions::InvocationUpdate,
    ) -> Result<(), BrokerError> {
        if !(self.durable_owner_required || self.publisher.records_effects_for(&pending.cluster_id))
            || !update.is_terminal()
        {
            return Ok(());
        }
        self.owner_health().map_err(BrokerError::failed)?;
        self.checked_policy_time(pending.policy_time.as_ref())
            .map_err(BrokerError::failed)?;
        if pending.operation_kind != tilo_actions::OperationKind::Effect {
            return Ok(());
        }
        let status = tilo_actions::EffectReceiptStatus::from_terminal(update)
            .ok_or_else(|| BrokerError::failed("nonterminal effect receipt".into()))?;
        let request = {
            let mut prepared = pending.prepared_effect.lock().map_err(|_| {
                BrokerError::failed("effect receipt preparation unavailable".into())
            })?;
            if let Some(previous) = prepared.as_ref() {
                if previous.update != *update {
                    return Err(BrokerError::failed(
                        "effect terminal outcome changed".into(),
                    ));
                }
                previous.request.clone()
            } else {
                let receipt = tilo_desktop_model::WorkspaceEffectReceipt::redacted(
                    update.invocation(),
                    &pending.actor,
                    pending.context.clone(),
                    status,
                    tilo_clock::unix_now_i64(),
                );
                let request = tilo_peer_state::owner_commit::OwnerCommitRequest {
                    workspace: pending.cluster_id.clone(),
                    operation: tilo_peer_state::owner_commit::CommitOperation::EffectReceipt {
                        invocation: update.invocation().0,
                        redacted: serde_json::to_vec(&receipt)
                            .map_err(|error| BrokerError::failed(error.to_string()))?,
                    },
                };
                *prepared = Some(PreparedEffectCommit {
                    update: update.clone(),
                    request: request.clone(),
                });
                request
            }
        };
        if let Some(key) = &pending.journal_key {
            let (_, durable) = self
                .journal
                .lock()
                .map_err(|_| BrokerError::failed("invocation journal unavailable".into()))?
                .as_mut()
                .map_err(|error| BrokerError::failed(error.clone()))?
                .as_mut()
                .ok_or_else(|| BrokerError::failed("durable admission unavailable".into()))?
                .stage_prepared_effect(key, update.clone(), &request)
                .map_err(BrokerError::failed)?;
            durable.wait().await.map_err(BrokerError::failed)?;
        }
        self.commit_owner_request(request).await
    }
    /// Reconcile an inert saved receipt, never dispatch or reconstruct action authority.
    #[cfg(test)]
    pub(super) async fn reconcile_prepared_effect(
        &self,
        record: DurableInvocation,
        authorize: impl Fn() -> Result<(), String>,
    ) -> Result<DurableInvocation, String> {
        authorize()?;
        let Some(prepared) = record.prepared_effect.as_ref() else {
            return Ok(record);
        };
        if !record.uncertain || record.update.is_terminal() {
            return Ok(record);
        }
        self.owner_health()?;
        // The normal service supplies actual current governor ownership and exact owner retry.
        // A saved request grants no authority, and no unavailable service fallback is allowed.
        if let Err(error) = self.publisher.commit_owner(prepared.request(), None).await {
            tracing::warn!(%error, "prepared effect remains uncertain");
            authorize()?;
            return Ok(record);
        }
        authorize()?;
        let (finished, durable) = self
            .journal
            .lock()
            .map_err(|_| "invocation journal unavailable")?
            .as_mut()
            .map_err(|error| error.clone())?
            .as_mut()
            .ok_or("durable admission unavailable")?
            .stage_finished_effect(&record.key, prepared)?;
        durable.wait().await?;
        Ok(finished)
    }
}
