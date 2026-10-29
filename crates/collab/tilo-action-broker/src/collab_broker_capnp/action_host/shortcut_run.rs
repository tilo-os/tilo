//! Saved shortcut admission and execution through the host action door.

use super::super::InvocationCaller;
use super::super::host_action_admission::{AdmittedHostAction, HostActionAdmission};
use super::{
    ActionHostError, Arc, BrokerActor, BrokerDoor, Caller, CapnpBrokerState, InvocationReceipt,
    InvokeRequest, Reservation, RowScope, SHORTCUTS_APP_ID, automation, workspace_provider,
};

#[cfg(test)]
mod tests;

struct ShortcutAdmission<'a> {
    origin: Option<&'a InvocationCaller>,
    request: &'a InvokeRequest,
}

impl CapnpBrokerState {
    /// Admit a run of saved shortcut `shortcut_id` in `room` for `caller`
    /// and start it. The run invokes every step through this broker as
    /// `caller`, so each step meets `caller`'s own authority as well as the
    /// shortcut's grant; the admission answers at once and the run reports
    /// its outcome on the invocation when it ends. The run's id is the
    /// invocation's, so the history it leaves is found from the receipt.
    pub(super) fn invoke_shortcut_run(
        self: &Arc<Self>,
        caller: &Caller,
        origin: Option<&InvocationCaller>,
        room: &str,
        shortcut_id: &str,
        request: &InvokeRequest,
    ) -> Result<InvocationReceipt, ActionHostError> {
        let abandon = |error: ActionHostError| {
            if let Ok(mut ledger) = self.remote_invocations.lock() {
                ledger.abandon(&request.invocation);
            }
            error
        };
        let reservation = self
            .remote_invocations
            .lock()
            .map_err(|_| ActionHostError::Unavailable {
                reason: "the invocation ledger is unavailable".into(),
            })?
            .reserve(caller, request)?;
        if matches!(reservation, Reservation::Admitted) {
            return self
                .remote_job(caller, &request.invocation)
                .map(|job| self.receipt_for(request.invocation, job))
                .ok_or(ActionHostError::UnknownInvocation);
        }
        let shortcut = match self.saved_shortcut(caller, room, shortcut_id) {
            Ok(shortcut) => shortcut,
            Err(refused) => return Err(abandon(refused)),
        };
        let inputs = match &request.argument {
            tilo_actions::remote::InvokeArgument::None => serde_json::Map::new(),
            tilo_actions::remote::InvokeArgument::Json(inputs) => inputs.clone(),
            tilo_actions::remote::InvokeArgument::Text(_) => {
                return Err(abandon(ActionHostError::InvalidRequest {
                    reason: "a shortcut takes its inputs as JSON arguments".into(),
                }));
            }
        };
        let job = self
            .start_shortcut_run(
                caller,
                room,
                shortcut,
                automation::RunStart {
                    run: tilo_automation::RunId(request.invocation.0),
                    trigger: tilo_automation::RunTrigger::Manual,
                    mode: automation::run_mode(request.confirmation.as_ref()),
                    inputs,
                    // A task that invoked this run is its parent: the caller's
                    // context names that task's group, so the run's steps are
                    // stamped with it and the run reads as part of that task.
                    parent: request
                        .context
                        .as_ref()
                        .and_then(|context| context.group.clone())
                        .filter(|group| group.kind != "shortcut"),
                },
                |runtime, run| {
                    automation::DoorHooks::new(
                        request.confirmation.clone(),
                        runtime,
                        self.ended_runs.running.running.clone(),
                        run,
                    )
                },
                Some(ShortcutAdmission { origin, request }),
                None,
            )
            .map_err(abandon)?;
        if let Ok(mut ledger) = self.remote_invocations.lock() {
            ledger.complete(&request.invocation, job);
        }
        Ok(self.receipt_for(request.invocation, job))
    }

    /// Run saved shortcut `shortcut_id` in `room` for `caller`, who is at
    /// the screen: every step that needs confirmation and every question
    /// waits in [`Self::take_step_prompt`] until the compositor's sheet
    /// answers it.
    ///
    /// # Errors
    ///
    /// Why the shortcut cannot run for `caller`.
    pub(crate) fn start_interactive_shortcut_run(
        self: &Arc<Self>,
        caller: &Caller,
        room: &str,
        shortcut_id: &str,
        inputs: serde_json::Map<String, serde_json::Value>,
    ) -> Result<u64, ActionHostError> {
        self.start_interactive_shortcut_run_continuing(caller, room, shortcut_id, inputs, None)
    }

    /// [`Self::start_interactive_shortcut_run`], continuing the run `from`
    /// names: its steps before `from` are taken as they happened and the run
    /// goes on from `from`, as it stands in the shortcut now. The origin run
    /// has to be one of this shortcut's runs in `room`.
    ///
    /// # Errors
    ///
    /// Why the shortcut cannot run for `caller`, or why the run it says to go
    /// on from cannot be continued.
    pub(crate) fn start_interactive_shortcut_run_continuing(
        self: &Arc<Self>,
        caller: &Caller,
        room: &str,
        shortcut_id: &str,
        inputs: serde_json::Map<String, serde_json::Value>,
        from: Option<tilo_automation::Continuation>,
    ) -> Result<u64, ActionHostError> {
        let shortcut = self.saved_shortcut(caller, room, shortcut_id)?;
        let origin = match &from {
            Some(continuation) => Some(self.earlier_run(continuation, shortcut_id)?),
            None => None,
        };
        let carried = origin
            .zip(from)
            .map(|(report, asked)| automation::Continued {
                report,
                from: asked.from,
            });
        let prompts = self.ended_runs.running.prompts.clone();
        let name = shortcut.name.clone();
        let runs = self.ended_runs.running.running.clone();
        let hooks =
            move |runtime, run| automation::SheetHooks::new(prompts, name, runtime, runs, run);
        self.start_shortcut_run(
            caller,
            room,
            shortcut,
            automation::RunStart {
                run: tilo_automation::RunId::new(),
                trigger: tilo_automation::RunTrigger::Manual,
                mode: tilo_automation::RunMode::Interactive,
                inputs,
                parent: None,
            },
            hooks,
            None,
            carried,
        )
    }

    /// Run `continuation` names, which has to be one of this broker's own
    /// runs of `shortcut_id`.
    fn earlier_run(
        &self,
        continuation: &tilo_automation::Continuation,
        shortcut_id: &str,
    ) -> Result<tilo_automation::RunReport, ActionHostError> {
        if continuation.from.as_str().is_empty() {
            return Err(ActionHostError::InvalidRequest {
                reason: "a continued run says which step it goes on from".into(),
            });
        }
        self.ended_runs
            .shortcuts
            .report(continuation.run, shortcut_id)
            .ok_or_else(|| ActionHostError::Refused {
                reason: "that run is not recorded for this shortcut".into(),
            })
    }

    /// The oldest step of an interactive run waiting for its person.
    pub(crate) fn take_step_prompt(&self) -> Option<automation::PromptedStep> {
        self.ended_runs.running.prompts.take_next()
    }

    /// Start one of saved shortcut `shortcut_id`'s triggers, `trigger`, in
    /// `room` on behalf of `caller`, who must be the person who saved it.
    ///
    /// A keyboard trigger means the person is at the screen: its run asks
    /// them through the compositor's sheet, like a palette run. Nobody is
    /// present at a scheduled run: a step asking for confirmation proceeds
    /// only when its saver consented to exactly that action in advance, and
    /// a question stops the run. Before anything runs the
    /// shortcut is checked again against what `caller` is offered now, so a
    /// shortcut that was deleted, lost the trigger, or reaches past a grant
    /// that has since been revoked does not start.
    ///
    /// # Errors
    ///
    /// Why the trigger may not start the shortcut.
    pub fn start_triggered_shortcut_run(
        self: &Arc<Self>,
        caller: &Caller,
        room: &str,
        shortcut_id: &str,
        trigger: &tilo_automation::TriggerSpec,
        run: tilo_automation::RunId,
        inputs: serde_json::Map<String, serde_json::Value>,
    ) -> Result<u64, ActionHostError> {
        let shortcut = self.saved_shortcut(caller, room, shortcut_id)?;
        // The one rule arming holds too: started for its saver only, the
        // person a device of theirs proved saved it in this room.
        let actor = BrokerActor::for_caller(caller).id;
        let revocations = self
            .membership
            .as_ref()
            .and_then(|membership| membership.room_revocations(room))
            .ok_or(ActionHostError::NotPermitted)?;
        if tilo_peer_state::room::proven_saver(
            room,
            &shortcut,
            &revocations,
            tilo_clock::unix_now_ns(),
        ) != Some(actor)
        {
            return Err(ActionHostError::NotPermitted);
        }
        if !shortcut.triggers.contains(trigger) {
            return Err(ActionHostError::Refused {
                reason: "the shortcut no longer has that trigger".into(),
            });
        }
        let catalog =
            tilo_automation::ActionCatalog::new(self.eager_catalogue(caller, RowScope::All).0);
        let others = self
            .decide_for(caller, room, |workspace, _, _| {
                tilo_automation::decode_saved(&workspace.shortcuts)
            })
            .unwrap_or_default();
        let resolver = |id: &str| others.iter().find(|other| other.id == id).cloned();
        tilo_automation::check(&shortcut, &catalog, &resolver).map_err(|errors| {
            ActionHostError::Refused {
                reason: errors
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; "),
            }
        })?;
        if matches!(trigger, tilo_automation::TriggerSpec::Keyboard { .. }) {
            let prompts = self.ended_runs.running.prompts.clone();
            let name = shortcut.name.clone();
            let runs = self.ended_runs.running.running.clone();
            let hooks =
                move |runtime, run| automation::SheetHooks::new(prompts, name, runtime, runs, run);
            return self.start_shortcut_run(
                caller,
                room,
                shortcut,
                automation::RunStart {
                    run,
                    trigger: tilo_automation::RunTrigger::Fired(trigger.clone()),
                    mode: tilo_automation::RunMode::Interactive,
                    inputs: serde_json::Map::new(),
                    parent: None,
                },
                hooks,
                None,
                None,
            );
        }
        self.start_shortcut_run(
            caller,
            room,
            shortcut,
            automation::RunStart {
                run,
                trigger: tilo_automation::RunTrigger::Fired(trigger.clone()),
                mode: tilo_automation::RunMode::Unattended,
                inputs,
                parent: None,
            },
            |runtime, run| {
                automation::DoorHooks::new(
                    None,
                    runtime,
                    self.ended_runs.running.running.clone(),
                    run,
                )
            },
            None,
            None,
        )
    }

    /// Shortcut `shortcut_id` as saved in `room`, when `caller` is offered
    /// that workspace.
    ///
    /// # Errors
    /// [`ActionHostError::UnknownAction`] for a shortcut not saved there (or
    /// a workspace not offered); [`ActionHostError::Unavailable`] with the
    /// reason for a saved shortcut whose record does not decode.
    fn saved_shortcut(
        &self,
        caller: &Caller,
        room: &str,
        shortcut_id: &str,
    ) -> Result<tilo_automation::Shortcut, ActionHostError> {
        self.decide_for(caller, room, |workspace, _, _| {
            workspace
                .shortcuts
                .iter()
                .find(|shortcut| shortcut.id() == shortcut_id)
                .map(|shortcut| {
                    shortcut.decode().map_err(|error| {
                        tracing::warn!(room, shortcut = shortcut_id, %error, "saved shortcut does not decode");
                        ActionHostError::Unavailable {
                            reason: format!("the saved shortcut cannot be read: {error}"),
                        }
                    })
                })
        })
        .flatten()
        .unwrap_or(Err(ActionHostError::UnknownAction))
    }

    /// Run `shortcut` as `caller` through this broker, reporting on a new
    /// broker job whose id is returned at once. The saver's advance consents
    /// go only to a run the saver started.
    fn start_shortcut_run<K>(
        self: &Arc<Self>,
        caller: &Caller,
        room: &str,
        shortcut: tilo_automation::Shortcut,
        start: automation::RunStart,
        hooks: impl FnOnce(tokio::runtime::Handle, tilo_automation::RunId) -> K,
        admission: Option<ShortcutAdmission<'_>>,
        carried: Option<automation::Continued>,
    ) -> Result<u64, ActionHostError>
    where
        K: tilo_automation::RunHooks + Send + 'static,
    {
        let runtime = self
            .services
            .shortcut_library
            .as_ref()
            .ok_or(ActionHostError::UnknownAction)?
            .runtime()
            .map_err(|reason| ActionHostError::Unavailable { reason })?;
        let hooks = hooks(runtime.clone(), start.run);
        let actor = BrokerActor::for_caller(caller).id;
        let _lifecycle = self
            .lifecycle
            .read()
            .map_err(|_| ActionHostError::Unavailable {
                reason: "broker lifecycle unavailable".into(),
            })?;
        let (admitted, origin) =
            self.prepare_shortcut_run(caller, room, &shortcut, &start, admission)?;
        let job = admitted.id;
        let invocation = admitted.invocation;
        let host = BrokerDoor::for_run(Arc::clone(self), origin);
        let state = Arc::clone(self);
        let caller = caller.clone().running_shortcut(shortcut.id.clone());
        let room = room.to_string();
        let parent = start.parent.clone();
        // Registered before the first step, so a stop can find it; it leaves
        // as it ends, whether or not it was stopped.
        let running = automation::RunningRun {
            room: room.clone(),
            shortcut: shortcut.id.clone(),
            name: shortcut.name.clone(),
            run: start.run,
            at: None,
            stopped: None,
        };
        // What this run's steps say they are part of: the work unit that asked
        // for it, or its own group. Read before the steps run, so it names the
        // shortcut that asked for it among the ones above it.
        let group = start
            .parent
            .as_ref()
            .map_or_else(|| start.run.0.to_string(), |parent| parent.id.clone());
        let chain =
            self.ended_runs
                .running
                .running
                .above(&room, Some(group.as_str()), &shortcut.id);
        self.ended_runs.running.running.register(running);
        let registered = self.ended_runs.running.running.clone();
        runtime.spawn(async move {
            let report = tilo_automation::run(
                &host,
                &hooks,
                tilo_automation::RunRequest {
                    shortcut: &shortcut,
                    run: start.run,
                    caller,
                    mode: start.mode,
                    trigger: start.trigger,
                    inputs: start.inputs,
                    caused_by: None,
                    // A run that goes on from an earlier one takes the steps
                    // before the step it goes on from from that run's record.
                    resume: carried.as_ref().map(|carried| &carried.report),
                    from: carried.as_ref().map(|carried| carried.from.clone()),
                    parent,
                    chain,
                },
            )
            .await;
            registered.leave(start.run);
            let update = automation::completion(invocation, &report);
            if let Err(error) = state.complete_host_action(&admitted, update) {
                tracing::warn!(%error, "shortcut completion could not be recorded");
                return;
            }
            state
                .ended_runs
                .shortcuts
                .push(automation::FinishedShortcutRun {
                    room,
                    shortcut,
                    invoker: actor,
                    report,
                });
        });
        Ok(job)
    }

    fn prepare_shortcut_run(
        &self,
        caller: &Caller,
        room: &str,
        shortcut: &tilo_automation::Shortcut,
        start: &automation::RunStart,
        admission: Option<ShortcutAdmission<'_>>,
    ) -> Result<(AdmittedHostAction, Option<InvocationCaller>), ActionHostError> {
        // Palette and trigger entry points do not carry a wire request. Resolve
        // their custody from the installed native seat, never from actor text.
        // Keep the run's trigger unchanged: a scheduled run is still unattended
        // and history attributes it to the system rather than a manual gesture.
        let request;
        let origin;
        let admission = if let Some(admission) = admission {
            admission
        } else {
            if caller.room() != Some(room) {
                return Err(ActionHostError::NotPermitted);
            }
            request = InvokeRequest {
                invocation: tilo_actions::InvocationId(start.run.0),
                target: tilo_actions::remote::RemoteActionTarget {
                    provider: workspace_provider(room),
                    address: automation::run_descriptor(shortcut).address,
                },
                argument: tilo_actions::remote::InvokeArgument::Json(start.inputs.clone()),
                on_behalf_of: None,
                context: None,
                confirmation: None,
                revision: None,
                wait_ms: None,
                appearance: None,
            };
            origin = self
                .native_local_person_certificate(caller)
                .map_err(|reason| ActionHostError::Unavailable { reason })?
                .map(|certificate| -> Result<_, ActionHostError> {
                    let time = self
                        .registered_policy_time()
                        .map_err(|reason| ActionHostError::Unavailable { reason })?
                        .ok_or(ActionHostError::NotPermitted)?;
                    Ok(InvocationCaller::NativePerson {
                        invocation: request.invocation,
                        room: room.into(),
                        certificate,
                        time,
                    })
                })
                .transpose()?;
            ShortcutAdmission {
                origin: origin.as_ref(),
                request: &request,
            }
        };
        // No runtime task may run until admission crosses the journal barrier.
        let trigger = match &start.trigger {
            tilo_automation::RunTrigger::Manual => serde_json::json!({"trigger": "manual"}),
            tilo_automation::RunTrigger::Fired(trigger) => {
                serde_json::to_value(trigger).map_err(|error| ActionHostError::Unavailable {
                    reason: error.to_string(),
                })?
            }
        };
        let admitted = self.admit_host_action_with_start(
            caller,
            admission.origin,
            HostActionAdmission {
                room,
                app_id: SHORTCUTS_APP_ID,
                descriptor: &automation::run_descriptor(shortcut),
                request: admission.request,
            },
            Some(serde_json::json!({
                "run": start.run,
                "shortcut": shortcut,
                "trigger": trigger,
                "mode": start.mode,
                "inputs": start.inputs,
            })),
        )?;
        Ok((admitted, admission.origin.cloned()))
    }

    /// Shortcut runs finished since the last call, oldest first.
    pub fn take_finished_shortcut_runs(&self) -> Vec<automation::FinishedShortcutRun> {
        self.ended_runs.shortcuts.take()
    }

    /// Whether a finished shortcut run is waiting to be taken.
    pub fn has_finished_shortcut_runs(&self) -> bool {
        self.ended_runs.shortcuts.has_finished()
    }

    /// The runs going in `room`, so it can offer to stop one.
    pub fn running_shortcut_runs(&self, room: &str) -> Vec<automation::RunningRun> {
        self.ended_runs.running.running.going(room)
    }

    /// Stop the runs `request` names before their next step.
    pub fn stop_shortcut_runs(
        &self,
        room: &str,
        request: &automation::StopRequest,
        reason: &str,
    ) -> Vec<automation::RunningRun> {
        self.ended_runs.running.running.stop(room, request, reason)
    }
}
