//! Closed-app task actions use the ordinary bounded launch-and-deliver path.
//! Host activation is a separate durable admission, never a publisher-zero action.
use super::action_host::LaunchDestination;
use super::{
    CapnpBrokerState, InvocationKey, InvocationOwner, PendingTaskRun, PublishedAction,
    durable_local_invocation::LocalCaller,
};
use crate::services::NativeTaskBindingActor;
use crate::services::{LaunchPlace, NativeTaskActionRequirement};
use std::sync::{Arc, atomic::Ordering};
use tilo_actions::remote::{
    Caller, InvokeArgument, InvokeRequest, RemoteActionTarget, application_provider, pending,
    workspace_provider_room,
};

/// Invocation-scoped injection at the actual durable/effect boundary.
#[cfg(test)]
pub(super) mod test_barrier {
    use std::sync::Mutex;
    use tilo_actions::InvocationId;

    type Hook = Box<dyn FnOnce() + Send>;
    static HOOKS: Mutex<Vec<(InvocationId, Hook)>> = Mutex::new(Vec::new());

    pub(super) fn run(invocation: InvocationId) {
        let hook = {
            let mut hooks = HOOKS.lock().unwrap();
            hooks
                .iter()
                .position(|(id, _)| *id == invocation)
                .map(|index| hooks.remove(index).1)
        };
        if let Some(hook) = hook {
            hook();
        }
    }

    pub(crate) fn install(invocation: InvocationId, hook: Hook) {
        HOOKS.lock().unwrap().push((invocation, hook));
    }
}

#[derive(Clone)]
pub(super) struct NativeLaunch {
    peer: u64,
    run_id: u64,
    lease: String,
    run: PendingTaskRun,
    caller: LocalCaller,
    invocation: tilo_actions::InvocationId,
    provider: String,
    resource: String,
    action: String,
    argument: Option<String>,
    original_argument: Option<String>,
    app: String,
    record: Option<String>,
    retained: Option<PublishedAction>,
    bootstrap: bool,
    /// The person confirmed this retained action as asked first; `argument`
    /// is then what they confirmed.
    confirmation: Option<String>,
    witness: Option<String>,
}

impl NativeLaunch {
    fn binding(&self) -> Result<crate::services::NativeTaskBinding, String> {
        let mut binding = self
            .run
            .native
            .clone()
            .ok_or("native launch binding absent")?;
        binding.revision = runtime_collab::native_task_revision(&self.lease)
            .ok_or("invalid native launch lease")?
            .1;
        Ok(binding)
    }

    fn requirement(&self, state: &CapnpBrokerState) -> Result<NativeTaskActionRequirement, String> {
        let binding = self.binding()?;
        let private_run_digest = state.native_private_run_digest(&self.run, &binding)?;
        // Match the existing durable journal's activation identity exactly.
        let digest =
            blake3::hash(format!("native-host-activation:{}", self.invocation.0).as_bytes());
        let id = uuid::Uuid::from_bytes(
            *digest
                .as_bytes()
                .first_chunk::<16>()
                .ok_or("activation digest invalid")?,
        );
        let need = tilo_actions::ActionNeed::of(tilo_actions::ActionKind::Undoable);
        let mut invocation = crate::services::NativeTaskInvocation::new(
            id,
            &self.provider,
            self.argument.as_deref(),
            &tilo_actions::ActionContract::default(),
            &self.lease,
        )?;
        invocation.request = self.invocation.0;
        invocation.private_run_digest = private_run_digest;
        if let Some(retained) = &self.retained {
            let mut child = crate::services::NativeTaskInvocation::new(
                self.invocation.0,
                &self.provider,
                self.argument.as_deref(),
                &retained.contract,
                &self.lease,
            )?;
            child.private_run_digest = private_run_digest;
            invocation.activation_for = Some(Box::new(NativeTaskActionRequirement {
                binding: Some(self.binding()?),
                app: self.app.clone(),
                resource_kind: retained.resource_kind.clone(),
                resource: tilo_actions::selection::logical_resource_id(&self.resource).into_owned(),
                action: self.action.clone(),
                need: retained.contract.need(),
                consequential: false,
                confirmation: self.confirmation.clone(),
                invocation: Some(child),
            }));
        }
        Ok(NativeTaskActionRequirement {
            binding: Some(self.binding()?),
            app: self.app.clone(),
            resource_kind: "applications.app".into(),
            resource: format!("app.{}", self.app),
            action: "open".into(),
            need,
            invocation: Some(invocation),
            consequential: false,
            // A child's approval is not an independent approval to open apps.
            // The owner resolves the exact activation_for workflow above.
            confirmation: None,
        })
    }

    pub(super) fn check(&self, state: &CapnpBrokerState) -> Result<(), String> {
        self.caller.current(state)?;
        state.task_run_for(self.peer, &self.run.agent_id, self.run_id, &self.lease)?;
        if state.task_run_cancelled(self.peer, &self.run.agent_id, self.run_id, &self.lease)? {
            return Err("task stopped while waiting for its application".into());
        }
        self.check_action_policy(state)
    }

    // Read-only policy checks are safe inside the owner's task admission turn.
    // Do not use `check` there: cancellation handling can mutate the owner.
    fn check_action_policy(&self, state: &CapnpBrokerState) -> Result<(), String> {
        if let Some(retained) = &self.retained {
            if self.confirmation.is_none()
                && !matches!(
                    retained
                        .contract
                        .confirmation_for_argument(self.argument.as_deref()),
                    tilo_actions::ConfirmationPolicy::None
                )
            {
                return Err("closed task action requires person confirmation".into());
            }
            let mut invocation = crate::services::NativeTaskInvocation::new(
                self.invocation.0,
                &self.provider,
                self.argument.as_deref(),
                &retained.contract,
                &self.lease,
            )?;
            invocation.private_run_digest =
                state.native_private_run_digest(&self.run, &self.binding()?)?;
            state
                .native_tasks(&self.run.cluster_id)
                .ok_or("native launch owner unavailable")?
                .authorize_action(
                    &self.run.cluster_id,
                    &self.run.agent_id,
                    &NativeTaskActionRequirement {
                        binding: Some(self.binding()?),
                        app: self.app.clone(),
                        resource_kind: retained.resource_kind.clone(),
                        resource: tilo_actions::selection::logical_resource_id(&self.resource)
                            .into_owned(),
                        action: self.action.clone(),
                        need: retained.contract.need(),
                        consequential: false,
                        confirmation: self.confirmation.clone(),
                        invocation: Some(invocation),
                    },
                )?;
        }
        // A ready app does not need launch authority. Its action still takes
        // the exact current task admission below, including starter revocation.
        if self.bootstrap || self.ready(state).is_none() {
            state
                .native_tasks(&self.run.cluster_id)
                .ok_or("native launch owner unavailable")?
                .authorize_action(
                    &self.run.cluster_id,
                    &self.run.agent_id,
                    &self.requirement(state)?,
                )?;
        }
        Ok(())
    }

    pub(super) fn destination(
        &self,
        state: &CapnpBrokerState,
    ) -> Result<LaunchDestination, String> {
        self.check(state)?;
        if state.services.launcher.as_ref().is_some_and(|launcher| {
            launcher
                .applications()
                .iter()
                .any(|app| app.package_id == self.app && app.unconfined)
        }) {
            return Err(
                "native task activation requires a sandboxed resource-publishing application"
                    .into(),
            );
        }
        let workspace = state
            .membership
            .as_ref()
            .ok_or("native launch membership absent")?
            .decide_in_room(&self.run.cluster_id, |workspace| {
                workspace
                    .cloned()
                    .ok_or_else(|| "native launch workspace absent".to_string())
            })??;
        Ok(LaunchDestination {
            workspace,
            app_id: self.app.clone(),
            record: self.record.clone(),
        })
    }

    pub(super) fn ready(&self, state: &CapnpBrokerState) -> Option<u64> {
        if !self.bootstrap {
            return state.ready_publisher(
                &self.app,
                &self.run.cluster_id,
                self.record.as_deref(),
                &tilo_actions::ActionAddress::new(&self.resource, &self.action),
            );
        }
        let subscribers = state.invocation_subscribers.lock().ok()?;
        state
            .surfaces
            .lock()
            .ok()?
            .values()
            .filter(|surface| {
                surface.live
                    && surface.app_id == self.app
                    && surface.route.cluster_id == self.run.cluster_id
                    && subscribers.contains_key(&surface.peer_id)
                    && !surface.actions.is_empty()
            })
            .map(|surface| surface.peer_id)
            .min()
    }

    fn launch_key(&self, state: &CapnpBrokerState) -> Result<InvocationKey, String> {
        host_activation_key(state, &self.run, self.invocation)
    }

    pub(super) async fn launch(
        &self,
        state: &Arc<CapnpBrokerState>,
        _destination: &LaunchDestination,
        place: &LaunchPlace,
        permit: Option<tokio::sync::OwnedSemaphorePermit>,
    ) -> Result<(), String> {
        let worker_state = Arc::clone(state);
        let launch = self.clone();
        let place = place.clone();
        let runtime = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            launch.launch_worker(&worker_state, &place, &runtime)
        })
        .await
        .map_err(|_| "native host launch interrupted; outcome unknown")?
    }

    /// The blocking half of [`Self::launch`]: durable admission, the exact
    /// owner turn and its recorded outcome. The caller holds `permit` for the
    /// whole worker.
    fn launch_worker(
        &self,
        state: &Arc<CapnpBrokerState>,
        place: &LaunchPlace,
        runtime: &tokio::runtime::Handle,
    ) -> Result<(), String> {
        self.check(state)?;
        let binding = self.binding()?;
        let requirement = self.requirement(state)?;
        self.caller
            .authorize_host_action(state, &self.run.cluster_id, &requirement)?;
        let key = self.launch_key(state)?;
        let intent = self.launch_intent(&binding, &requirement)?;
        // Capture the original signed identity before entering an owner turn.
        let evidence = state.native_host_action_evidence(&self.caller, &key, &intent)?;
        if admit_launch(state, &key, intent, evidence)? {
            return Ok(());
        }
        #[cfg(test)]
        test_barrier::run(self.invocation);
        // Disk custody is not authority. Acquire the exact owner turn only
        // AFTER that barrier, so its policy and original caller checks are
        // fresh. No owner RPC is made while Ready holds the guarded turn.
        let result = self.run_launch_turn(state, place, runtime, binding);
        record_launch_outcome(state, &key, result)
    }

    /// The durable journal identity of this host activation.
    fn launch_intent(
        &self,
        binding: &crate::services::NativeTaskBinding,
        requirement: &NativeTaskActionRequirement,
    ) -> Result<serde_json::Value, String> {
        Ok(serde_json::json!({
            "host_activation":true, "app_id":self.app,
            "actor":binding.actor()?.id(), "record_id":self.record.as_deref().unwrap_or(""),
            "resource":format!("app.{}", self.app), "resource_kind":"applications.app",
            "action":"open", "contract":tilo_actions::ActionContract::default(),
            "bootstrap":self.bootstrap, "confirmed":self.confirmation.is_some(),
            "task":binding.task_id, "epoch":binding.epoch, "revision":binding.revision,
            "caller":self.caller.owner(), "native_requirement":requirement,
            "effect_request_digest":hex::encode(super::invocation_journal::admission_request_digest(
                &serde_json::json!({"provider":self.provider, "resource":self.resource,
                    "action":self.action, "argument":self.argument}))),
            "retained_contract":self.retained.as_ref().map(|action| &action.contract),
            "retained_params":self.retained.as_ref().map(|action| &action.params),
            "request":{"provider":self.provider,
                "resource":tilo_actions::selection::logical_resource_id(&self.resource),
                "action":self.action, "argument":self.original_argument}
        }))
    }

    /// The owner turn the barrier guards: freeze the admission, launch, and
    /// spend the grant only once that turn released successfully.
    fn run_launch_turn(
        &self,
        state: &Arc<CapnpBrokerState>,
        place: &LaunchPlace,
        runtime: &tokio::runtime::Handle,
        binding: crate::services::NativeTaskBinding,
    ) -> Result<(), String> {
        self.check(state)?;
        let requirement = self.requirement(state)?;
        let spent = self
            .caller
            .authorize_host_action(state, &self.run.cluster_id, &requirement)?;
        let destination = self.destination(state)?;
        let service = state
            .native_tasks(&self.run.cluster_id)
            .ok_or("native launch owner absent")?;
        let ready = runtime.block_on(service.prepare_admission(
            self.run.cluster_id.clone(),
            binding,
            requirement,
            self.caller.admission_caller()?,
        ))?;
        let effect = (|| {
            self.caller.current_task_connection(state)?;
            state.task_run_for(self.peer, &self.run.agent_id, self.run_id, &self.lease)?;
            if self.ready(state).is_some() {
                return Ok(());
            }
            state.launch_retained(
                &self.app,
                &destination.workspace,
                self.record.as_deref().filter(|record| !record.is_empty()),
                place,
            )
        })();
        let completed = runtime.block_on(ready.complete(effect.clone()));
        // Grant mutation can call the owner, so it follows release.
        if effect.is_ok() && completed.is_ok() {
            state.spend_once_grants(&self.run.cluster_id, &spent);
        }
        completed?;
        effect
    }

    fn check_retained_contract(
        &self,
        state: &CapnpBrokerState,
        fresh: &PublishedAction,
    ) -> Result<(), String> {
        let key = self.launch_key(state)?;
        let guard = state
            .journal
            .lock()
            .map_err(|_| "launch journal unavailable")?;
        let journal = guard
            .as_ref()
            .map_err(Clone::clone)?
            .as_ref()
            .ok_or("launch journal absent")?;
        let record = journal.lookup(&key)?;
        drop(guard);
        if let Some(record) = record {
            let request = serde_json::json!({"provider":self.provider,
                "resource":tilo_actions::selection::logical_resource_id(&self.resource),
                "action":self.action,"argument":self.original_argument});
            if !record.admitted_request_is(&request)
                || record.intent.get("caller")
                    != Some(
                        &serde_json::to_value(self.caller.owner())
                            .map_err(|error| error.to_string())?,
                    )
            {
                return Err(
                    "native activation identity was reused with another request or caller".into(),
                );
            }
            if record.uncertain {
                return Err("host activation outcome uncertain; review before retrying".into());
            }
            let contract = record
                .intent
                .get("retained_contract")
                .filter(|value| !value.is_null());
            let params = record
                .intent
                .get("retained_params")
                .filter(|value| !value.is_null());
            if contract.is_some_and(|contract| {
                serde_json::to_value(&fresh.contract).ok().as_ref() != Some(contract)
            }) || params.is_some_and(|params| {
                serde_json::to_value(&fresh.params).ok().as_ref() != Some(params)
            }) {
                return Err("closed application's action contract changed; refresh the catalogue with a new invocation".into());
            }
        }
        Ok(())
    }

    pub(super) async fn deliver(
        &self,
        state: &Arc<CapnpBrokerState>,
        peer: u64,
    ) -> Result<u64, String> {
        self.check(state)?;
        if self.bootstrap {
            return self.deliver_bootstrap(state).await;
        }
        let offered = state
            .surfaces
            .lock()
            .map_err(|_| "fresh task surface unavailable")?
            .get(&peer)
            .and_then(|surface| surface.action_projection(&self.resource, &self.action))
            .ok_or("fresh task action unavailable")?;
        if self.retained.as_ref().is_some_and(|retained| {
            retained.contract != offered.1.contract || retained.params != offered.1.params
        }) {
            return Err(
                "closed application's action contract changed; refresh the catalogue".into(),
            );
        }
        self.check_retained_contract(state, &offered.1)?;
        let mut admitted = self.clone();
        let confirmation = self
            .admit_store_witness(state, &offered, &mut admitted)
            .await?;
        let job = state
            .admit_task_action(
                admitted.run.clone(),
                &admitted.lease,
                runtime_collab::TaskAction {
                    peer_id: peer,
                    provider: &self.provider,
                    resource: &self.resource,
                    action: &self.action,
                    argument: admitted.argument.as_deref(),
                },
                (
                    Some(&(
                        admitted.caller.clone(),
                        admitted.invocation,
                        admitted.original_argument.as_deref(),
                    )),
                    (
                        admitted.confirmation.as_deref(),
                        admitted.witness.as_deref(),
                    ),
                ),
                offered,
            )
            .await?;
        admitted
            .settle_started_confirmation(state, confirmation.as_deref())
            .await;
        Ok(job.id)
    }

    /// Bootstrap activation: observe the wait loop's owner-authenticated
    /// receipt, launching only when it never completed, and return its wire id.
    async fn deliver_bootstrap(&self, state: &Arc<CapnpBrokerState>) -> Result<u64, String> {
        let caller = Caller::local_agent(&self.run.agent_id).in_room(&self.run.cluster_id);
        // The wait loop may already have completed this exact activation
        // before its publisher appeared. Observe its owner-authenticated
        // receipt instead of reserving the spent invocation a second time.
        let observed = state.native_host_observation(
            &self.run,
            runtime_collab::TaskAction {
                peer_id: self.peer,
                provider: &self.provider,
                resource: &self.resource,
                action: &self.action,
                argument: self.original_argument.as_deref(),
            },
            &self.caller,
            self.invocation,
        )?;
        if let Some((_, update)) = observed {
            if !matches!(update, tilo_actions::InvocationUpdate::Succeeded { .. }) {
                return Err("host activation has no successful retained receipt".into());
            }
        } else {
            self.launch(
                state,
                &self.destination(state)?,
                &state.launch_place(&caller, &self.run.cluster_id, None),
                None,
            )
            .await?;
        }
        let key = self.launch_key(state)?;
        let guard = state
            .journal
            .lock()
            .map_err(|_| "launch journal unavailable")?;
        let journal = guard
            .as_ref()
            .map_err(Clone::clone)?
            .as_ref()
            .ok_or("launch journal absent")?;
        let id = journal
            .lookup(&key)?
            .map(|record| record.wire_id)
            .ok_or("host activation receipt absent")?;
        drop(guard);
        state.launchable.note_view(
            &self.run.cluster_id,
            &format!("agent:{}", self.run.agent_id),
            &self.app,
        );
        Ok(id)
    }

    /// Application-directory providers can name an action that was never
    /// retained. Once published, it needs the same exact disclosure gate.
    /// Returns the confirmation the started child still has to settle.
    async fn admit_store_witness(
        &self,
        state: &Arc<CapnpBrokerState>,
        offered: &(super::PublishedSurface, PublishedAction),
        admitted: &mut Self,
    ) -> Result<Option<String>, String> {
        if !super::requires_store_witness(&self.app, &self.action) || self.witness.is_some() {
            return Ok(None);
        }
        let mut confirmation = None;
        if admitted.confirmation.is_none()
            && let super::native_task_confirmation::NativeGate::Confirmed {
                invocation,
                argument,
                reference,
                confirmation: id,
            } = state
                .native_confirmation_for(
                    &self.run,
                    super::native_task_confirmation::ConfirmationContext {
                        lease: &self.lease,
                        invocation: self.invocation.0,
                        provider: &self.provider,
                    },
                    (&self.app, &offered.1),
                    (&self.resource, &self.action),
                    self.argument.as_deref(),
                )
                .await?
        {
            admitted.argument = Some(argument);
            admitted.invocation = tilo_actions::InvocationId(invocation);
            admitted.confirmation = Some(reference);
            confirmation = Some(id);
        }
        admitted.witness = state
            .native_store_witness(
                (
                    &admitted.run,
                    &admitted.lease,
                    admitted.confirmation.as_deref(),
                    &admitted.provider,
                ),
                offered,
                admitted.argument.as_deref(),
                &admitted.caller,
                admitted.invocation,
            )
            .await?;
        admitted.check(state)?;
        state.check_native_store_offer(offered)?;
        Ok(confirmation)
    }

    async fn settle_started_confirmation(
        &self,
        state: &CapnpBrokerState,
        confirmation: Option<&str>,
    ) {
        if let Some(id) = confirmation
            && let Err(error) = state.settle_native_confirmation(&self.run, id).await
        {
            tracing::warn!(%error, "confirmed Store action started but its confirmation stayed");
        }
    }
}

/// Admit the activation in the journal, or confirm the retained record of the
/// same request. `true` means it already succeeded and nothing may run.
fn admit_launch(
    state: &CapnpBrokerState,
    key: &InvocationKey,
    intent: serde_json::Value,
    evidence: crate::action_evidence::AdmissionEvidence,
) -> Result<bool, String> {
    let mut guard = state
        .journal
        .lock()
        .map_err(|_| "launch journal unavailable")?;
    let journal = guard
        .as_mut()
        .map_err(|error| error.clone())?
        .as_mut()
        .ok_or("durable launch journal absent")?;
    let outcome = (|| {
        if let Some(retained) = journal.lookup(key)? {
            return if retained
                .admitted_request_is(intent.get("request").ok_or("launch request absent")?)
                && retained.intent.get("caller") == intent.get("caller")
                && !retained.uncertain
                && matches!(
                    retained.update,
                    tilo_actions::InvocationUpdate::Succeeded { .. }
                ) {
                Ok(true)
            } else {
                Err(
                    "host activation changed or its outcome is uncertain; review before retrying"
                        .into(),
                )
            };
        }
        let wire = state.invocation_id.fetch_add(1, Ordering::Relaxed);
        if !journal.admit_with_evidence(key.clone(), intent, wire, Some(evidence))? {
            return Err("native host activation already admitted".into());
        }
        Ok(false)
    })();
    drop(guard);
    outcome
}

/// Write the activation's outcome into the durable journal and return it.
fn record_launch_outcome(
    state: &CapnpBrokerState,
    key: &InvocationKey,
    result: Result<(), String>,
) -> Result<(), String> {
    let update = match &result {
        Ok(()) => tilo_actions::InvocationUpdate::Succeeded {
            invocation: key.invocation,
            output: tilo_actions::ActionOutput::default(),
        },
        Err(message) => tilo_actions::InvocationUpdate::Failed {
            invocation: key.invocation,
            error: tilo_actions::ActionFailure {
                code: "host_activation_failed".into(),
                message: message.clone(),
                details: None,
                retryable: false,
            },
        },
    };
    let mut guard = state
        .journal
        .lock()
        .map_err(|_| "launch journal unavailable")?;
    guard
        .as_mut()
        .map_err(|error| error.clone())?
        .as_mut()
        .ok_or("durable launch journal absent")?
        .update(key, update, false)?;
    drop(guard);
    result
}

/// The ordinary remote invocation a closed-app task action delivers.
fn native_task_invoke_request(
    native: &NativeLaunch,
    target: runtime_collab::TaskAction<'_>,
) -> InvokeRequest {
    InvokeRequest {
        invocation: native.invocation,
        target: RemoteActionTarget {
            provider: native.provider.clone(),
            address: tilo_actions::ActionAddress::new(target.resource, target.action),
        },
        argument: native
            .argument
            .as_deref()
            .map_or(InvokeArgument::None, |text| {
                InvokeArgument::Text(text.into())
            }),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        appearance: None,
        wait_ms: None,
    }
}

fn host_activation_key(
    state: &CapnpBrokerState,
    run: &PendingTaskRun,
    invocation: tilo_actions::InvocationId,
) -> Result<InvocationKey, String> {
    // Host activation and resource action have distinct durable identities.
    let digest = blake3::hash(format!("native-host-activation:{}", invocation.0).as_bytes());
    let bytes = *digest
        .as_bytes()
        .first_chunk::<16>()
        .ok_or("launch digest invalid")?;
    let binding = run.native.as_ref().ok_or("native launch binding absent")?;
    Ok(InvocationKey {
        room: run.cluster_id.clone(),
        host: state.invocation_host()?,
        owner: InvocationOwner::NativeTask {
            agent: binding.agent_id.clone(),
            task: binding.task_id.clone(),
            epoch: binding.epoch,
        },
        invocation: tilo_actions::InvocationId(uuid::Uuid::from_bytes(bytes)),
    })
}

impl CapnpBrokerState {
    pub(super) fn native_host_observation(
        &self,
        run: &PendingTaskRun,
        target: runtime_collab::TaskAction<'_>,
        caller: &LocalCaller,
        invocation: tilo_actions::InvocationId,
    ) -> Result<Option<(u64, tilo_actions::InvocationUpdate)>, String> {
        if workspace_provider_room(target.provider) != Some(run.cluster_id.as_str())
            || target.action != "open"
        {
            return Ok(None);
        }
        if !target.resource.starts_with("app.") {
            return Ok(None);
        }
        caller.current(self)?;
        let key = host_activation_key(self, run, invocation)?;
        let guard = self
            .journal
            .lock()
            .map_err(|_| "launch journal unavailable")?;
        let journal = guard
            .as_ref()
            .map_err(Clone::clone)?
            .as_ref()
            .ok_or("launch journal absent")?;
        let record = journal.lookup(&key)?;
        drop(guard);
        let Some(record) = record else {
            return Ok(None);
        };
        let request = serde_json::json!({"provider":target.provider,
            "resource":tilo_actions::selection::logical_resource_id(target.resource),
            "action":target.action,"argument":target.argument});
        if !record.admitted_request_is(&request)
            || record.uncertain
            || record.intent.get("caller")
                != Some(&serde_json::to_value(caller.owner()).map_err(|error| error.to_string())?)
        {
            return Err("host activation request changed or its outcome is uncertain".into());
        }
        let service = self
            .native_tasks(&run.cluster_id)
            .ok_or("native launch owner absent")?;
        let requirement = caller.authorize_retained_task(
            self,
            (
                &record,
                &run.native
                    .as_ref()
                    .ok_or("native launch binding absent")?
                    .actor()?,
            ),
        )?;
        // Observing a retained activation never reserves or dispatches again.
        service.authorize_retained_action(
            &run.cluster_id,
            &run.agent_id,
            &requirement,
            record.key.invocation.0,
        )?;
        Ok(Some((
            record.wire_id,
            record.update.with_invocation(invocation),
        )))
    }

    fn native_launch_for(
        &self,
        reask: &super::native_task_execution::ChildReask<'_>,
    ) -> Result<NativeLaunch, String> {
        let run = self.task_run_for(reask.peer_id, reask.agent_id, reask.run_id, reask.lease)?;
        let target = reask.target;
        let bootstrap = workspace_provider_room(target.provider)
            .is_some_and(|room| room == run.cluster_id)
            && target.action == "open"
            && target.resource.starts_with("app.");
        let (app, record, retained) = if bootstrap {
            (
                target
                    .resource
                    .strip_prefix("app.")
                    .ok_or("native application resource absent")?
                    .to_owned(),
                None,
                None,
            )
        } else if let Some((room, app)) = application_provider(target.provider) {
            if room != run.cluster_id {
                return Err("native launch changed workspace".into());
            }
            (app.to_owned(), None, None)
        } else {
            let surface = self
                .launchable
                .surface_for(target.provider)
                .ok_or("native launch provider absent")?;
            if surface.route.cluster_id != run.cluster_id {
                return Err("native launch changed workspace".into());
            }
            let action = surface
                .action_projection(target.resource, target.action)
                .ok_or("retained task action absent")?
                .1;
            (surface.app_id, Some(surface.route.record_id), Some(action))
        };
        Ok(NativeLaunch {
            peer: reask.peer_id,
            run_id: reask.run_id,
            lease: reask.lease.into(),
            run,
            caller: reask.caller.clone(),
            invocation: reask.invocation,
            provider: target.provider.into(),
            resource: target.resource.into(),
            action: target.action.into(),
            argument: target.argument.map(str::to_owned),
            original_argument: target.argument.map(str::to_owned),
            app,
            record,
            retained,
            bootstrap,
            confirmation: None,
            witness: None,
        })
    }

    pub(super) async fn launch_native_task_action(
        self: &Arc<Self>,
        reask: &super::native_task_execution::ChildReask<'_>,
    ) -> Result<(u64, tilo_actions::InvocationUpdate), String> {
        let target = reask.target;
        let mut native = self.native_launch_for(reask)?;
        // A retained action that asks first pauses at its draft before the
        // application is launched for it, as a running application's does.
        let mut confirmation = None;
        if let Some(retained) = native.retained.clone()
            && let super::native_task_confirmation::NativeGate::Confirmed {
                invocation,
                argument,
                reference,
                confirmation: id,
            } = self
                .native_confirmation_for(
                    &native.run,
                    super::native_task_confirmation::ConfirmationContext {
                        lease: reask.lease,
                        invocation: reask.invocation.0,
                        provider: &native.provider,
                    },
                    (&native.app, &retained),
                    (target.resource, target.action),
                    target.argument,
                )
                .await?
        {
            native.invocation = tilo_actions::InvocationId(invocation);
            // Resolve a previously started child before checking activation or
            // attempting to consume this still-held answer again.
            if let Some(observed) = self.native_task_observation(
                &native.run,
                native.invocation,
                target,
                &native.caller,
            )? {
                return Ok(observed);
            }
            native.argument = Some(argument);
            native.confirmation = Some(reference);
            confirmation = Some(id);
        }
        let preparation_state = Arc::clone(self);
        let preparation = native.clone();
        let destination =
            tokio::task::spawn_blocking(move || preparation.destination(&preparation_state))
                .await
                .map_err(|_| "native task destination preparation interrupted".to_owned())??;
        // The ordinary delivery loop uses this label only for actor-view placement.
        // Authority comes from the captured signed LocalCaller and native owner admission.
        let caller = Caller::local_agent(&native.run.agent_id).in_room(&native.run.cluster_id);
        let request = native_task_invoke_request(&native, target);
        let launch_key = native.launch_key(self)?;
        let invocation = native.invocation;
        let (completer, answer) = pending::<Result<u64, tilo_actions::remote::ActionHostError>>();
        let run = native.run.clone();
        let state = Arc::clone(self);
        super::launchable::delivery_runtime()?.spawn(async move {
            let result = Box::pin(state.relaunch_and_deliver(
                &completer,
                &caller,
                &destination,
                None,
                &request,
                Some(&native),
            ))
            .await;
            completer.complete(result);
        });
        let id = answer
            .await
            .ok_or("native activation delivery ended")?
            .map_err(|error| error.to_string())?;
        if let Some(confirmation) = confirmation {
            // The child started; a failure to clear the answer only leaves
            // the card until the run ends.
            if let Err(error) = self.settle_native_confirmation(&run, &confirmation).await {
                tracing::warn!(%error, "confirmed task action started but its confirmation stayed");
            }
        }
        let update = self
            .invocation_status(id)
            .or_else(|| {
                self.journal
                    .lock()
                    .ok()?
                    .as_ref()
                    .ok()?
                    .as_ref()?
                    .lookup(&launch_key)
                    .ok()?
                    .map(|record| record.update.with_invocation(invocation))
            })
            .ok_or("native activation state absent")?;
        Ok((id, update))
    }
}
