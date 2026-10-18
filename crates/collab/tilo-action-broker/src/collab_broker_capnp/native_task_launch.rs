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
        destination: &LaunchDestination,
        place: &LaunchPlace,
        permit: Option<tokio::sync::OwnedSemaphorePermit>,
    ) -> Result<(), String> {
        // All owner reads and caller authorization happen before transferring
        // the guarded turn. Only owned metadata crosses the blocking boundary.
        let preparation_state = Arc::clone(state);
        let launch = self.clone();
        let (binding, requirement, spent, key, intent) = tokio::task::spawn_blocking(move || {
            let binding = launch.binding()?;
            let requirement = launch.requirement(&preparation_state)?;
            let spent = launch.caller.authorize_host_action(
                &preparation_state,
                &launch.run.cluster_id,
                &requirement,
            )?;
            let key = launch.launch_key(&preparation_state)?;
            let intent = serde_json::json!({"host_activation":true,"app_id":launch.app,
                    "task":binding.task_id,"epoch":binding.epoch,"revision":binding.revision,
                    "caller":launch.caller.owner(),"native_requirement":requirement,
                    "retained_contract":launch.retained.as_ref().map(|action| &action.contract),
                    "retained_params":launch.retained.as_ref().map(|action| &action.params),
                    "request":{"provider":launch.provider,
                        "resource":tilo_actions::selection::logical_resource_id(&launch.resource),
                        "action":launch.action,"argument":launch.original_argument}});
            Ok::<_, String>((binding, requirement, spent, key, intent))
        })
        .await
        .map_err(|_| "native host launch preparation interrupted")??;
        let ready = state
            .native_tasks(&self.run.cluster_id)
            .ok_or("native launch owner absent")?
            .prepare_admission(
                self.run.cluster_id.clone(),
                binding,
                requirement,
                self.caller.admission_caller()?,
            )
            .await?;
        // Ready holds the canonical owner's final current policy check. This
        // fence samples broker memory only; never re-enter that owner here.
        if let Err(error) = self.caller.current_task_connection(state) {
            return ready.complete(Err(error)).await;
        }
        let already_ready = self.ready(state).is_some();
        let worker_state = Arc::clone(state);
        let app = self.app.clone();
        let workspace = destination.workspace.clone();
        let record = destination.record.clone();
        let place = place.clone();
        let runtime = tokio::runtime::Handle::current();
        let room = self.run.cluster_id.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            // Keep readiness with the worker even if the local future is
            // cancelled: custody and launch must finish before releasing it.
            let state = &worker_state;
            let mut admitted = false;
            let result = (|| {
                let mut guard = state
                    .journal
                    .lock()
                    .map_err(|_| "launch journal unavailable")?;
                let journal = guard
                    .as_mut()
                    .map_err(|error| error.clone())?
                    .as_mut()
                    .ok_or("durable launch journal absent")?;
                let wire = state.invocation_id.fetch_add(1, Ordering::Relaxed);
                if !journal.admit(key.clone(), intent, wire)? {
                    let retained = journal.lookup(&key)?.ok_or("launch custody disappeared")?;
                    return if !retained.uncertain
                        && matches!(
                            retained.update,
                            tilo_actions::InvocationUpdate::Succeeded { .. }
                        ) {
                        Ok(())
                    } else {
                        Err("host activation outcome uncertain; review before retrying".into())
                    };
                }
                drop(guard);
                admitted = true;
                // Custody is on disk before the existing on-system host path runs.
                let result = if already_ready {
                    Ok(())
                } else {
                    state.launch_retained(
                        &app,
                        &workspace,
                        record.as_deref().filter(|record| !record.is_empty()),
                        &place,
                    )
                };
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
                    .update(&key, update, false)?;
                result
            })();
            // Grant mutation can re-enter the owner. Finish the guarded turn
            // first, and keep cleanup on this worker even if the caller leaves.
            let completion = runtime.block_on(ready.complete(result.clone()));
            if admitted {
                state.spend_once_grants(&room, &spent);
            }
            completion?;
            result
        })
        .await
        .map_err(|_| "native host launch interrupted; outcome unknown")?
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
            return Ok(id);
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
        state
            .admit_task_action(
                self.run.clone(),
                &self.lease,
                runtime_collab::TaskAction {
                    peer_id: peer,
                    provider: &self.provider,
                    resource: &self.resource,
                    action: &self.action,
                    argument: self.argument.as_deref(),
                },
                (
                    Some(&(
                        self.caller.clone(),
                        self.invocation,
                        self.original_argument.as_deref(),
                    )),
                    self.confirmation.as_deref(),
                ),
                offered,
            )
            .await
            .map(|job| job.id)
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
        let request = InvokeRequest {
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
        };
        let launch_key = native.launch_key(self)?;
        let invocation = native.invocation;
        let (completer, answer) = pending::<Result<u64, tilo_actions::remote::ActionHostError>>();
        let run = native.run.clone();
        let state = Arc::clone(self);
        super::launchable::delivery_runtime()?.spawn(async move {
            let result = state
                .relaunch_and_deliver(
                    &completer,
                    &caller,
                    &destination,
                    None,
                    &request,
                    Some(&native),
                )
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
