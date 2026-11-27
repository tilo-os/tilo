//! Saved shortcut admission and execution through the host action api.

use super::super::InvocationCaller;
use super::super::host_action_admission::{AdmittedHostAction, HostActionAdmission};
use super::{
    ActionCallError, AppBrokerState, Arc, BrokerActor, BrokerApi, Caller, InvocationReceipt,
    InvokeRequest, Reservation, RowScope, SHORTCUTS_APP_ID, automation, workspace_provider,
};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy)]
struct ShortcutAdmission<'a> {
    origin: Option<&'a InvocationCaller>,
    request: &'a InvokeRequest,
}

impl AppBrokerState {
    /// Admit a run of saved shortcut `shortcut_id` in `workspace` for `caller`
    /// and start it. The run invokes every step through this broker as
    /// `caller`, so each step meets `caller`'s own authority as well as the
    /// shortcut's grant; the admission answers at once and the run reports
    /// its outcome on the invocation when it ends. The run's id is the
    /// invocation's, so the history it leaves is found from the receipt.
    pub(super) fn invoke_shortcut_run(
        self: &Arc<Self>,
        caller: &Caller,
        origin: Option<&InvocationCaller>,
        workspace: &str,
        shortcut_id: &str,
        request: &InvokeRequest,
    ) -> Result<InvocationReceipt, ActionCallError> {
        let abandon = |error: ActionCallError| {
            if let Ok(mut log) = self.remote_invocations.lock() {
                log.abandon(&request.invocation);
            }
            error
        };
        let reservation = self.reserve_invocation(caller, request)?;
        if matches!(reservation, Reservation::Admitted) {
            return self
                .remote_job(caller, &request.invocation)
                .map(|job| self.receipt_for(request.invocation, job))
                .ok_or(ActionCallError::UnknownInvocation);
        }
        let shortcut = match self.saved_shortcut(caller, workspace, shortcut_id) {
            Ok(shortcut) => shortcut,
            Err(refused) => return Err(abandon(refused)),
        };
        // A step that would need the person's yes is asked about before anything runs: the
        // call waits in its asked record, and their approve brings it back to run.
        let asking = tilo_automation::asks_before_running(
            &shortcut,
            &tilo_automation::ActionCatalog::new(self.eager_catalogue(caller, RowScope::All).0),
            Some(workspace),
        );
        if !asking.is_empty() {
            tilo_workspace_service::workspaces::asking::approved_or_asks(request, || {
                run_asks(caller, &shortcut, &asking)
            })
            .map_err(abandon)?;
        }
        let inputs = match &request.argument {
            tilo_actions::remote::InvokeArgument::None => serde_json::Map::new(),
            tilo_actions::remote::InvokeArgument::Json(inputs) => inputs.clone(),
            tilo_actions::remote::InvokeArgument::Text(_) => {
                return Err(abandon(ActionCallError::InvalidRequest {
                    reason: "a shortcut takes its inputs as JSON arguments".into(),
                }));
            }
        };
        // A key the person pressed says so, and its history shows it with the chord.
        let trigger = automation::run_trigger(request.context.as_ref(), &shortcut);
        let job = self
            .start_shortcut_run(
                caller,
                workspace,
                shortcut,
                automation::InvocationStart {
                    invocation: tilo_automation::AutomationInvocationId(request.invocation.0),
                    trigger,
                    mode: automation::run_mode(request.approval()),
                    inputs,
                    // A task that invoked this run is its parent: the caller's
                    // context names that task's group, so the run's steps are
                    // stamped with it and the run reads as part of that task.
                    parent: request
                        .context
                        .as_ref()
                        .and_then(|context| context.group.clone())
                        .filter(|group| group.kind != "shortcut"),
                    approval: request.approval().cloned(),
                },
                |runtime, run| {
                    automation::ApiHooks::new(
                        runtime,
                        self.ended_invocations.open.running.clone(),
                        run,
                    )
                },
                Some(ShortcutAdmission { origin, request }),
                None,
            )
            .map_err(abandon)?;
        if let Ok(mut log) = self.remote_invocations.lock() {
            log.complete(&request.invocation, job);
        }
        Ok(self.receipt_for(request.invocation, job))
    }

    /// Run saved shortcut `shortcut_id` in `workspace` for `caller`, who is at
    /// the screen. Nothing asks them on the way: a step that needs their yes
    /// stops the run, as it does in a run nobody approved; a run that asks
    /// before it starts is one invoked through the dispatch, as its asked record.
    ///
    /// # Errors
    ///
    /// Why the shortcut cannot run for `caller`.
    pub(crate) fn start_interactive_shortcut_run(
        self: &Arc<Self>,
        caller: &Caller,
        workspace: &str,
        shortcut_id: &str,
        inputs: serde_json::Map<String, serde_json::Value>,
    ) -> Result<u64, ActionCallError> {
        self.start_interactive_shortcut_run_continuing(caller, workspace, shortcut_id, inputs, None)
    }

    /// [`Self::start_interactive_shortcut_run`], continuing the run `from`
    /// names: its steps before `from` are taken as they happened and the run
    /// goes on from `from`, as it stands in the shortcut now. The origin run
    /// has to be one of this shortcut's runs in `workspace`.
    ///
    /// # Errors
    ///
    /// Why the shortcut cannot run for `caller`, or why the run it says to go
    /// on from cannot be continued.
    pub(crate) fn start_interactive_shortcut_run_continuing(
        self: &Arc<Self>,
        caller: &Caller,
        workspace: &str,
        shortcut_id: &str,
        inputs: serde_json::Map<String, serde_json::Value>,
        from: Option<tilo_automation::Continuation>,
    ) -> Result<u64, ActionCallError> {
        let shortcut = self.saved_shortcut(caller, workspace, shortcut_id)?;
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
        let runs = self.ended_invocations.open.running.clone();
        let hooks = move |runtime, run| automation::ApiHooks::new(runtime, runs, run);
        self.start_shortcut_run(
            caller,
            workspace,
            shortcut,
            automation::InvocationStart {
                invocation: tilo_automation::AutomationInvocationId::new(),
                trigger: tilo_automation::InvocationTrigger::Manual,
                mode: tilo_automation::InvocationMode::Interactive,
                inputs,
                parent: None,
                approval: None,
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
    ) -> Result<tilo_automation::InvocationReport, ActionCallError> {
        if continuation.from.as_str().is_empty() {
            return Err(ActionCallError::InvalidRequest {
                reason: "a continued run says which step it goes on from".into(),
            });
        }
        self.ended_invocations
            .finished
            .report(continuation.invocation, shortcut_id)
            .ok_or_else(|| ActionCallError::Refused {
                reason: "that run is not recorded for this shortcut".into(),
            })
    }

    /// Start one of saved shortcut `shortcut_id`'s triggers, `trigger`, in
    /// `workspace` on behalf of `caller`, who must be the person who saved it.
    ///
    /// Nobody is present at a triggered invocation: a step asking for confirmation proceeds
    /// only when its saver consented to exactly that action in advance, and a question stops
    /// the run. A key the person pressed is not started here: it runs the shortcut through the
    /// device's dispatch as theirs, where a step that needs their yes asks them. Before anything runs the
    /// shortcut is checked again against what `caller` is offered now, so a
    /// shortcut that was deleted, lost the trigger, or reaches past a grant
    /// that has since been revoked does not start.
    pub fn start_triggered_shortcut_run(
        self: &Arc<Self>,
        caller: &Caller,
        workspace: &str,
        shortcut_id: &str,
        trigger: &tilo_automation::TriggerSpec,
        invocation: tilo_automation::AutomationInvocationId,
        inputs: serde_json::Map<String, serde_json::Value>,
    ) -> Result<u64, ActionCallError> {
        if matches!(trigger, tilo_automation::TriggerSpec::Keyboard { .. }) {
            return Err(ActionCallError::InvalidRequest {
                reason: "a pressed key runs its shortcut through the device's dispatch".into(),
            });
        }
        let shortcut = self.saved_shortcut(caller, workspace, shortcut_id)?;
        // The one rule arming holds too: started for its saver only, the
        // person a device of theirs proved saved it in this workspace.
        let actor = BrokerActor::for_caller(caller);
        let revocations = self
            .membership
            .as_ref()
            .and_then(|membership| membership.workspace_revocations(workspace))
            .ok_or(ActionCallError::NotPermitted)?;
        // An agent that saved it with its own key runs it as itself, for its person; a device's
        // proof runs it as the person.
        let name = &shortcut.name;
        let Some(saver) = tilo_peer_state::workspace::proven(
            workspace,
            &shortcut,
            &revocations,
            tilo_clock::unix_now_ns(),
        ) else {
            return Err(ActionCallError::Refused {
                reason: format!(
                    "\u{201c}{name}\u{201d} does not say which device of its person saved it, so it does not run unattended"
                ),
            });
        };
        let runs_as_saver = match &saver.agent {
            Some(agent) => {
                caller.acting().member_id()
                    == Some(&tilo_member_types::MemberId::agent(
                        agent.as_str(),
                        saver.person.clone(),
                    ))
                    && caller.acting_for_person() == Some(saver.person.as_str())
            }
            None => actor.actor().person_id() == Some(saver.person.as_str()),
        };
        if !runs_as_saver {
            return Err(ActionCallError::Refused {
                reason: format!("\u{201c}{name}\u{201d} runs only as the person who saved it"),
            });
        }
        if !shortcut.triggers.contains(trigger) {
            return Err(ActionCallError::Refused {
                reason: "the shortcut no longer has that trigger".into(),
            });
        }
        let catalog =
            tilo_automation::ActionCatalog::new(self.eager_catalogue(caller, RowScope::All).0);
        let others = self
            .decide_for(caller, workspace, |workspace, _, _| {
                tilo_automation::decode_saved(&workspace.shortcuts)
            })
            .unwrap_or_default();
        let resolver = |id: &str| others.iter().find(|other| other.id == id).cloned();
        tilo_automation::check(&shortcut, &catalog, &resolver).map_err(|errors| {
            ActionCallError::Refused {
                reason: errors
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; "),
            }
        })?;
        self.start_shortcut_run(
            caller,
            workspace,
            shortcut,
            automation::InvocationStart {
                invocation,
                trigger: tilo_automation::InvocationTrigger::Fired(trigger.clone()),
                mode: tilo_automation::InvocationMode::Unattended,
                inputs,
                parent: None,
                approval: None,
            },
            |runtime, run| {
                automation::ApiHooks::new(runtime, self.ended_invocations.open.running.clone(), run)
            },
            None,
            None,
        )
    }

    /// Shortcut `shortcut_id` as saved in `workspace`, when `caller` is offered
    /// that workspace.
    ///
    /// # Errors
    /// [`ActionCallError::UnknownAction`] for a shortcut not saved there (or
    /// a workspace not offered); [`ActionCallError::Unavailable`] with the
    /// reason for a saved shortcut whose record does not decode.
    fn saved_shortcut(
        &self,
        caller: &Caller,
        workspace_id: &str,
        shortcut_id: &str,
    ) -> Result<tilo_automation::Shortcut, ActionCallError> {
        self.decide_for(caller, workspace_id, |workspace, _, _| {
            workspace
                .shortcuts
                .iter()
                .find(|shortcut| shortcut.id() == shortcut_id)
                .map(|shortcut| {
                    shortcut.decode().map_err(|error| {
                        tracing::warn!(workspace_id, shortcut = shortcut_id, %error, "saved shortcut does not decode");
                        ActionCallError::Unavailable {
                            reason: format!("the saved shortcut cannot be read: {error}"),
                        }
                    })
                })
        })
        .flatten()
        .unwrap_or(Err(ActionCallError::UnknownAction))
    }

    /// Run `shortcut` as `caller` through this broker, reporting on a new
    /// broker job whose id is returned at once. The saver's advance consents
    /// go only to a run the saver started.
    fn start_shortcut_run<K>(
        self: &Arc<Self>,
        caller: &Caller,
        workspace: &str,
        shortcut: tilo_automation::Shortcut,
        start: automation::InvocationStart,
        hooks: impl FnOnce(tokio::runtime::Handle, tilo_automation::AutomationInvocationId) -> K,
        admission: Option<ShortcutAdmission<'_>>,
        carried: Option<automation::Continued>,
    ) -> Result<u64, ActionCallError>
    where
        K: tilo_automation::InvocationHooks + Send + 'static,
    {
        let runtime = self
            .services
            .shortcut_library
            .as_ref()
            .ok_or(ActionCallError::UnknownAction)?
            .runtime()
            .map_err(|reason| ActionCallError::Unavailable { reason })?;
        let hooks = hooks(runtime.clone(), start.invocation);
        let actor = BrokerActor::for_caller(caller).actor().clone();
        let _lifecycle = self
            .lifecycle
            .read()
            .map_err(|_| ActionCallError::Unavailable {
                reason: "broker lifecycle unavailable".into(),
            })?;
        let (admitted, origin) =
            self.prepare_shortcut_run(caller, workspace, &shortcut, &start, admission)?;
        let job = admitted.id;
        let invocation = admitted.invocation;
        let host = BrokerApi::for_run(Arc::clone(self), origin);
        let state = Arc::clone(self);
        let caller = caller.clone().running_shortcut(shortcut.id.clone());
        let workspace = workspace.to_string();
        let parent = start.parent.clone();
        let approval = start.approval.clone();
        // Registered before the first step, so a stop can find it; it leaves
        // as it ends, whether or not it was stopped.
        let running = automation::RunningInvocation {
            workspace: workspace.clone(),
            shortcut: shortcut.id.clone(),
            name: shortcut.name.clone(),
            invocation: start.invocation,
            at: None,
            stopped: None,
        };
        // What this run's steps say they are part of: the work unit that asked
        // for it, or its own group. Read before the steps run, so it names the
        // shortcut that asked for it among the ones above it.
        let group = start.parent.as_ref().map_or_else(
            || start.invocation.0.to_string(),
            |parent| parent.id.clone(),
        );
        let chain = self.ended_invocations.open.running.above(
            &workspace,
            Some(group.as_str()),
            &shortcut.id,
        );
        self.ended_invocations.open.running.register(running);
        let registered = self.ended_invocations.open.running.clone();
        runtime.spawn(async move {
            let report = tilo_automation::run(
                &host,
                &hooks,
                tilo_automation::InvocationRequest {
                    shortcut: &shortcut,
                    invocation: start.invocation,
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
                    approval,
                },
            )
            .await;
            registered.leave(start.invocation);
            let update = automation::completion(invocation, &report);
            if let Err(error) = state.complete_host_action(&admitted, update) {
                tracing::warn!(%error, "shortcut completion could not be recorded");
                return;
            }
            state
                .ended_invocations
                .finished
                .push(automation::FinishedInvocation {
                    workspace,
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
        workspace: &str,
        shortcut: &tilo_automation::Shortcut,
        start: &automation::InvocationStart,
        admission: Option<ShortcutAdmission<'_>>,
    ) -> Result<(AdmittedHostAction, Option<InvocationCaller>), ActionCallError> {
        // Palette and trigger entry points do not carry a wire request. Resolve
        // their ownership from the installed native device, never from actor text.
        // Keep the run's trigger unchanged: a scheduled run is still unattended
        // and history attributes it to the system rather than a manual gesture.
        let request;
        let origin;
        let admission = if let Some(admission) = admission {
            admission
        } else {
            if caller.workspace() != Some(workspace) {
                return Err(ActionCallError::NotPermitted);
            }
            request = InvokeRequest {
                invocation: tilo_actions::InvocationId(start.invocation.0),
                target: tilo_actions::remote::RemoteActionTarget {
                    provider: workspace_provider(workspace),
                    address: automation::run_descriptor(shortcut).address,
                },
                argument: tilo_actions::remote::InvokeArgument::Json(start.inputs.clone()),
                on_behalf_of: None,
                context: None,
                confirmation: None,
                revision: None,
                wait_ms: None,
                appearance: None,
                signed: None,
            };
            let native = self.native_local_person_certificate(caller);
            // A trigger fires with nobody at a screen, on whichever device of the workspace
            // takes it: unless the saver is this device's own person, the run's proof is the
            // approval they signed when they saved the shortcut.
            let fired = matches!(start.trigger, tilo_automation::InvocationTrigger::Fired(_));
            if fired && !matches!(native, Ok(Some(_))) {
                origin = Some(self.shortcut_approval(caller, workspace, shortcut)?);
                return self.admit_shortcut_run(
                    caller,
                    workspace,
                    shortcut,
                    start,
                    ShortcutAdmission {
                        origin: origin.as_ref(),
                        request: &request,
                    },
                );
            }
            origin = native
                .map_err(|reason| ActionCallError::Unavailable { reason })?
                .map(|certificate| -> Result<_, ActionCallError> {
                    let time = self
                        .registered_policy_time()
                        .map_err(|reason| ActionCallError::Unavailable { reason })?
                        .ok_or(ActionCallError::NotPermitted)?;
                    Ok(InvocationCaller::NativePerson {
                        invocation: request.invocation,
                        workspace: workspace.into(),
                        certificate: Box::new(certificate),
                        time,
                    })
                })
                .transpose()?;
            ShortcutAdmission {
                origin: origin.as_ref(),
                request: &request,
            }
        };
        self.admit_shortcut_run(caller, workspace, shortcut, start, admission)
    }

    /// Admit the run of `shortcut` `admission` asks for, owned as its origin says.
    fn admit_shortcut_run(
        &self,
        caller: &Caller,
        workspace: &str,
        shortcut: &tilo_automation::Shortcut,
        start: &automation::InvocationStart,
        admission: ShortcutAdmission<'_>,
    ) -> Result<(AdmittedHostAction, Option<InvocationCaller>), ActionCallError> {
        // No runtime task may run until admission crosses the journal barrier.
        let trigger = match &start.trigger {
            tilo_automation::InvocationTrigger::Manual => serde_json::json!({"trigger": "manual"}),
            tilo_automation::InvocationTrigger::Fired(trigger) => serde_json::to_value(trigger)
                .map_err(|error| ActionCallError::Unavailable {
                    reason: error.to_string(),
                })?,
        };
        let admitted = self.admit_host_action_with_start(
            caller,
            admission.origin,
            HostActionAdmission {
                workspace,
                app_id: SHORTCUTS_APP_ID,
                descriptor: &automation::run_descriptor(shortcut),
                request: admission.request,
            },
            Some(serde_json::json!({
                "run": start.invocation,
                "shortcut": shortcut,
                "trigger": trigger,
                "mode": start.mode,
                "inputs": start.inputs,
            })),
        )?;
        Ok((admitted, admission.origin.cloned()))
    }

    /// Shortcut runs finished since the last call, oldest first.
    pub fn take_finished_invocations(&self) -> Vec<automation::FinishedInvocation> {
        self.ended_invocations.finished.take()
    }

    /// How the run `invocation` of `shortcut` ended, when it is one this broker kept.
    pub fn shortcut_run_report(
        &self,
        invocation: tilo_automation::AutomationInvocationId,
        shortcut: &str,
    ) -> Option<tilo_automation::InvocationReport> {
        self.ended_invocations.finished.report(invocation, shortcut)
    }

    /// Whether a finished shortcut run is waiting to be taken.
    pub fn has_finished_shortcut_runs(&self) -> bool {
        self.ended_invocations.finished.has_finished()
    }

    /// The runs going in `workspace`, so it can offer to stop one.
    pub fn running_shortcut_runs(&self, workspace: &str) -> Vec<automation::RunningInvocation> {
        self.ended_invocations.open.running.going(workspace)
    }

    /// Stop the runs `request` names before their next step.
    pub fn stop_shortcut_runs(
        &self,
        workspace: &str,
        request: &automation::StopRequest,
        reason: &str,
    ) -> Vec<automation::RunningInvocation> {
        self.ended_invocations
            .open
            .running
            .stop(workspace, request, reason)
    }
}

/// What the person is shown about a run of `shortcut` by `caller` before it starts: the
/// shortcut, and each of its steps that needs their yes, with what it acts on and the arguments
/// the shortcut gives it.
fn run_asks(
    caller: &Caller,
    shortcut: &tilo_automation::Shortcut,
    asking: &[tilo_automation::AskingStep],
) -> tilo_actions::remote::asked::AskedFacts {
    use tilo_actions::remote::asked::{AskedBy, AskedFacts, Fact};
    let by = match caller.acting().agent_id() {
        Some(_) => tilo_workspace_service::workspaces::asking::asked_by(caller),
        None => AskedBy::App {
            app: SHORTCUTS_APP_ID.to_owned(),
            title: shortcut.name.clone(),
            author: None,
        },
    };
    let mut facts = AskedFacts {
        on: Some(shortcut.name.clone()),
        verb: automation::run_descriptor(shortcut).action_label,
        destructive: asking
            .iter()
            .any(|step| step.descriptor.contract.risk == tilo_actions::RiskLevel::Destructive),
        ..AskedFacts::by(by)
    };
    for step in asking {
        // The step as the call it is: what it acts on, then the arguments the shortcut gives
        // it, drawn the way any waiting call's are.
        let mut shown = super::super::approval::described(
            AskedFacts::default(),
            step.descriptor.clone(),
            &tilo_actions::remote::InvokeArgument::Json(step.arguments.clone()),
        );
        if !step.resource.is_empty() {
            shown.facts.insert(
                0,
                Fact::new(step.descriptor.action_label.clone(), step.resource.clone()),
            );
        }
        facts.facts.extend(shown.facts);
    }
    facts
}
