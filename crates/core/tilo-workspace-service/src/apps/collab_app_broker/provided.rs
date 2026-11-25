//! The providers registered on this device, as its dispatch runs a call of one made here: the
//! steps the host runs itself (pairing a nearby device, administering the person's identities and
//! the devices trusted under them, changing their settings) and every provider the device's
//! embedder registered ([`crate::workspaces::providers`]).
//!
//! Each is admitted against the workspace's policy under the application its registration names,
//! asks the person first where its contract says so, and is journalled as the person's own call
//! before the provider carries it out ([`ActionProvider::run`]). A provider that reaches this
//! device alone is offered only to a caller on this machine confined to a workspace.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::AppBrokerState;
use crate::workspaces::providers::Registered;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Admitted, Caller, InvocationReceipt, InvokeRequest, Outcome,
    Pending, PendingPoll, ReceiptStatus, Reservation, Serving,
};

/// How a call of a registered provider stands once admitted, before it is taken.
enum RegisteredAdmission {
    /// The same call was taken before: its receipt.
    Taken(InvocationReceipt),
    /// A call to take.
    Call(RegisteredCall),
    /// The provider describes the action only once its listing answers, which it has not yet:
    /// nothing is reserved, and the call is admitted again with what the listing describes.
    Undescribed,
}

/// A call of a registered provider, admitted and not yet taken.
struct RegisteredCall {
    descriptor: tilo_actions::ActionDescriptor,
    /// The answer the person gave this exact call, consumed.
    approved: bool,
    /// What a step of an agent session does in turn goes through: the session's own api.
    through: Option<Arc<dyn ActionProvider + Send + Sync>>,
}

/// The app whose grants decide a call of `registered`: for a step an invocation's action
/// declares, the app that invocation was delivered to, whose yes covers it; else the one the
/// provider registered for.
fn deciding_app<'a>(registered: &'a Registered, serving: Option<&'a Arc<dyn Serving>>) -> &'a str {
    serving.map_or(&registered.app, |serving| serving.app())
}

/// How long a provider that answered a call as running is left before it is looked at again,
/// when it cannot say it changed.
const FOLLOW_LOOK: std::time::Duration = std::time::Duration::from_secs(1);

/// Transport keys alone never grant access to a service on this device.
/// The certified local socket retains its original admission, even when the
/// protected native device projects a keyless person. Network admissions,
/// including same-person devices, are not that socket.
pub(super) fn is_local_host_caller(
    caller: &Caller,
    origin: Option<&super::published_invocation::InvocationCaller>,
) -> bool {
    if caller.is_foreign_device() {
        return false;
    }
    if caller.device_public_key().is_none()
        && !matches!(
            origin,
            Some(super::published_invocation::InvocationCaller::Certified { .. })
        )
    {
        return match origin {
            Some(super::published_invocation::InvocationCaller::Local { admitted, .. }) => admitted
                .certificate
                .as_ref()
                .is_none_or(|cert| !cert.capabilities.foreign),
            Some(super::published_invocation::InvocationCaller::NativePerson {
                certificate,
                ..
            }) => !certificate.capabilities.foreign,
            Some(super::published_invocation::InvocationCaller::Certified { .. }) => false,
            None => true,
        };
    }
    let Some(super::published_invocation::InvocationCaller::Certified {
        caller: original,
        foreign_restriction: false,
    }) = origin
    else {
        return false;
    };
    !original.presentation.is_empty()
        && caller.workspace() == Some(original.workspace.as_str())
        && original
            .admitted
            .certificate
            .as_ref()
            .is_some_and(|certificate| {
                !certificate.capabilities.foreign
                    && if caller.device_public_key().is_none() {
                        original.native_projection_verified
                            && caller.user_public_key().is_none()
                            && caller.acting().person_id()
                                == Some(
                                    super::person_id_for_root(certificate.user_public_key).as_str(),
                                )
                    } else {
                        caller.user_public_key() == Some(&certificate.user_public_key)
                            && caller.device_public_key() == Some(&certificate.device_public_key)
                    }
            })
}

/// The descriptor of the action `request` calls, as `registered` offers it to `caller`: as its
/// argument makes it, where the provider says, else as its catalogue describes it.
fn descriptor_of(
    registered: &Registered,
    caller: &Caller,
    request: &InvokeRequest,
    argument: Option<&str>,
) -> Option<tilo_actions::ActionDescriptor> {
    let address = &request.target.address;
    if let Some(descriptor) = registered.answers.invocation(address, argument) {
        return Some(descriptor);
    }
    rows_of(registered, caller)
        .into_iter()
        .filter_map(|row| row.action)
        .find(|action| &action.address == address)
}

/// The query that lists everything `registered` offers, from its first page.
fn listing(registered: &Registered) -> tilo_actions::remote::CatalogueQuery {
    tilo_actions::remote::CatalogueQuery {
        provider: Some(registered.provider.to_string()),
        max_results: tilo_actions::remote::MAX_CATALOGUE_PAGE,
        ..tilo_actions::remote::CatalogueQuery::default()
    }
}

/// What `registered` lists for `caller`, as it answers at once: a registered provider lists what
/// it holds, never what it must first go and ask.
pub(super) fn rows_of(
    registered: &Registered,
    caller: &Caller,
) -> Vec<tilo_actions::remote::CatalogueRow> {
    let mut rows = Vec::new();
    let mut query = listing(registered);
    loop {
        let mut listed = registered.answers.query(caller, query.clone());
        let tilo_actions::remote::PendingPoll::Ready(Ok(page)) = listed.poll_ready() else {
            return rows;
        };
        rows.extend(page.rows);
        if page.next_cursor.is_empty() || page.exhausted {
            return rows;
        }
        query.cursor = page.next_cursor;
        query.revision = Some(page.revision);
    }
}

/// The outcome of a call `answers` carried out for `caller`: what its receipt says, followed
/// while it runs until it ends. A stopped call is cancelled at the provider.
fn outcome_of(
    answers: Arc<dyn ActionProvider + Send + Sync>,
    caller: Caller,
    invocation: tilo_actions::InvocationId,
    receipt: Pending<Result<InvocationReceipt, ActionCallError>>,
    stop: Arc<AtomicBool>,
) -> Pending<Outcome> {
    let (completer, outcome) = tilo_actions::remote::pending();
    receipt.then(move |receipt| {
        let status = match receipt {
            None => return,
            Some(Err(error)) => {
                completer.complete(Err(Box::new(refusal(&error))));
                return;
            }
            Some(Ok(receipt)) => receipt.status,
        };
        if let Some(ended) = ended(status) {
            completer.complete(ended);
            return;
        }
        let Ok(runtime) = tilo_rt::try_app() else {
            completer.complete(Err(Box::new(failure(
                "host_step_stopped",
                "nothing follows the call here",
            ))));
            return;
        };
        drop(runtime.spawn(async move {
            let mut cancelled = false;
            loop {
                if stop.load(Ordering::Acquire) && !cancelled {
                    cancelled = true;
                    drop(answers.cancel(&caller, &invocation).await);
                }
                let change = answers.next_change(&caller, &invocation);
                match answers.observe(&caller, &invocation) {
                    None => {
                        completer.complete(Err(Box::new(failure(
                            "host_step_stopped",
                            "the call is no longer known where it ran",
                        ))));
                        return;
                    }
                    Some(state) if state.update.is_terminal() => {
                        completer.complete(match state.update {
                            tilo_actions::InvocationUpdate::Succeeded { output, .. } => Ok(output),
                            tilo_actions::InvocationUpdate::Failed { error, .. } => {
                                Err(Box::new(error))
                            }
                            other => Err(Box::new(failure(
                                "cancelled",
                                &format!("the call ended as {other:?}"),
                            ))),
                        });
                        return;
                    }
                    Some(_) => {}
                }
                match change {
                    Some(change) => drop(change.await),
                    None => tilo_rt::sleep(FOLLOW_LOOK).await,
                }
            }
        }));
    });
    outcome
}

/// How a receipt that is already final ended; `None` for one still running.
fn ended(status: ReceiptStatus) -> Option<Outcome> {
    match status {
        ReceiptStatus::Accepted => None,
        ReceiptStatus::Completed { output } => Some(Ok(output)),
        ReceiptStatus::Failed { error } => Some(Err(Box::new(error))),
        ReceiptStatus::Rejected { error } => Some(Err(Box::new(refusal(&error)))),
        ReceiptStatus::Cancelled { message } => Some(Err(Box::new(failure(
            "cancelled",
            message.as_deref().unwrap_or("the call was cancelled"),
        )))),
    }
}

fn refusal(error: &ActionCallError) -> tilo_actions::ActionFailure {
    failure("host_step_refused", &error.to_string())
}

fn failure(code: &str, message: &str) -> tilo_actions::ActionFailure {
    tilo_actions::ActionFailure {
        code: code.into(),
        message: message.into(),
        details: None,
        retryable: true,
    }
}

impl AppBrokerState {
    /// The provider registered on this device under `provider`.
    pub(super) fn registered(&self, provider: &str) -> Option<Registered> {
        self.services.providers.as_ref()?.get(provider)
    }

    /// Admit one call of a provider registered on this device for `caller` and hand it to the
    /// provider.
    ///
    /// A provider that reaches this device alone is never offered a remote device, and the caller
    /// must be confined to the workspace its connection was admitted to; whatever its reach, the
    /// workspace's policy decides the call under the application its registration names. An
    /// action whose contract asks first needs the person's own confirmation, or the answer they
    /// gave on their device. A provider that knows whether a call waits for its person's yes only
    /// by looking at it ([`ActionProvider::asks_first`]) is asked before the call is taken, and a
    /// call that waits is refused with what its person is shown, for the dispatch to keep as its
    /// asked record. The invocation answers `Accepted` once taken and records the outcome when
    /// the provider's call ends, which for a pairing approval may be minutes later.
    pub(super) fn invoke_registered(
        self: &Arc<Self>,
        caller: &Caller,
        origin: Option<&super::published_invocation::InvocationCaller>,
        serving: Option<Arc<dyn Serving>>,
        registered: &Registered,
        request: &InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        self.invoke_described(caller, (origin, serving), registered, request, None)
    }

    /// [`Self::invoke_registered`], with the descriptor of the action it calls where the
    /// provider's listing described it after the call came ([`RegisteredAdmission::Undescribed`]).
    fn invoke_described(
        self: &Arc<Self>,
        caller: &Caller,
        (origin, serving): (
            Option<&super::published_invocation::InvocationCaller>,
            Option<Arc<dyn Serving>>,
        ),
        registered: &Registered,
        request: &InvokeRequest,
        described: Option<tilo_actions::ActionDescriptor>,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let admitted = match self.admit_registered(
            caller,
            (origin, serving.as_ref()),
            registered,
            request,
            described,
        ) {
            Ok(RegisteredAdmission::Call(admitted)) => admitted,
            Ok(RegisteredAdmission::Taken(receipt)) => return Pending::ready(Ok(receipt)),
            Ok(RegisteredAdmission::Undescribed) => {
                return self.invoke_once_described(caller, (origin, serving), registered, request);
            }
            Err(error) => return Pending::ready(Err(error)),
        };
        let asking = request.approval().is_none().then(|| {
            registered.answers.asks_first(
                caller,
                request,
                &Admitted {
                    through: admitted.through.clone(),
                    serving: serving.clone(),
                    ..Admitted::default()
                },
            )
        });
        let Some(Some(asking)) = asking else {
            return Pending::ready(
                self.take_registered(caller, origin, serving, registered, request, admitted),
            );
        };
        let (state, caller, origin, registered, request) = (
            Arc::clone(self),
            caller.clone(),
            origin.cloned(),
            registered.clone(),
            request.clone(),
        );
        asking.map(move |asked| match asked {
            Ok(()) => state.take_registered(
                &caller,
                origin.as_ref(),
                serving,
                &registered,
                &request,
                admitted,
            ),
            Err(waits) => {
                if let Ok(mut log) = state.remote_invocations.lock() {
                    log.abandon(&request.invocation);
                }
                Err(waits)
            }
        })
    }

    /// Admit `request` once `registered` describes the action it calls, asked for that one
    /// action wherever it publishes it (the check a call makes of its row before it is invoked):
    /// what a provider that reads its listing off the caller's thread answers (a built-in app's
    /// record, which makes its root before it lists it). Unknown where it has no such action.
    fn invoke_once_described(
        self: &Arc<Self>,
        caller: &Caller,
        (origin, serving): (
            Option<&super::published_invocation::InvocationCaller>,
            Option<Arc<dyn Serving>>,
        ),
        registered: &Registered,
        request: &InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let rows = registered.answers.query(
            caller,
            tilo_actions::remote::CatalogueQuery {
                target: Some(request.target.address.clone()),
                ..listing(registered)
            },
        );
        let (done, answer) = tilo_actions::remote::pending();
        let (state, caller, origin, registered, request) = (
            Arc::clone(self),
            caller.clone(),
            (origin.cloned(), serving),
            registered.clone(),
            request.clone(),
        );
        rows.then(move |page| {
            let address = &request.target.address;
            let Some(descriptor) = page
                .and_then(Result::ok)
                .into_iter()
                .flat_map(|page| page.rows)
                .filter_map(|row| row.action)
                .find(|action| &action.address == address)
            else {
                done.complete(Err(ActionCallError::UnknownAction));
                return;
            };
            state
                .invoke_described(
                    &caller,
                    (origin.0.as_ref(), origin.1),
                    &registered,
                    &request,
                    Some(descriptor),
                )
                .then(move |answered| {
                    if let Some(answered) = answered {
                        done.complete(answered);
                    }
                });
        });
        answer
    }

    /// Admit one call of a provider registered on this device for `caller`, before it is taken:
    /// what [`Self::take_registered`] goes on with. `described` is the action's descriptor where
    /// the provider's listing gave it after the call came; without it, a provider that describes
    /// the action only once its listing answers leaves the call
    /// [`RegisteredAdmission::Undescribed`].
    fn admit_registered(
        self: &Arc<Self>,
        caller: &Caller,
        (origin, serving): (
            Option<&super::published_invocation::InvocationCaller>,
            Option<&Arc<dyn Serving>>,
        ),
        registered: &Registered,
        request: &InvokeRequest,
        described: Option<tilo_actions::ActionDescriptor>,
    ) -> Result<RegisteredAdmission, ActionCallError> {
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
                .map(|job| RegisteredAdmission::Taken(self.receipt_for(request.invocation, job)))
                .ok_or(ActionCallError::UnknownInvocation);
        }
        if !registered.reach.devices() && !is_local_host_caller(caller, origin) {
            return Err(abandon(ActionCallError::UnknownAction));
        }
        // A step an invocation's action declares runs only while that invocation does: its
        // caller still may, its app still holds it, and nobody cancelled it.
        if let Some(serving) = serving {
            serving
                .recheck()
                .map_err(|reason| abandon(ActionCallError::Refused { reason }))?;
        }
        let Some(workspace_id) = caller.workspace() else {
            return Err(abandon(ActionCallError::NotPermitted));
        };
        let argument = request.argument.to_slot();
        let Some(descriptor) =
            described.or_else(|| descriptor_of(registered, caller, request, argument.as_deref()))
        else {
            // Its listing has not answered yet: asked again once it does, nothing reserved.
            if let Ok(mut log) = self.remote_invocations.lock() {
                log.abandon(&request.invocation);
            }
            return Ok(RegisteredAdmission::Undescribed);
        };
        let app_id = deciding_app(registered, serving);
        let (workspace, policy, now) = self
            .current_framework_authority(caller, origin, workspace_id)
            .map_err(abandon)?;
        // One answer covers both checks of this exact invocation.
        let approved = super::approval::approved_by_person(request.confirmation.as_ref());
        // An agent takes a step as any of its steps: on the workspace's grants, in an agent
        // session that still goes. A step of the session's own, or of an agent session's, is no
        // agent's outside one.
        let session = self.agent_session_of(Some(caller), workspace_id);
        if let Some(Err(reason)) = &session {
            return Err(abandon(ActionCallError::Refused {
                reason: reason.clone(),
            }));
        }
        if registered.reach.in_sessions() && policy.is_agent() && session.is_none() {
            return Err(abandon(ActionCallError::Refused {
                reason: "a session step is an agent session's: this agent works in none".into(),
            }));
        }
        self.framework_policy(
            (&workspace, &policy, now),
            (app_id, &descriptor),
            request.confirmation.as_ref(),
            approved,
            false,
        )
        .map_err(abandon)?;
        if !Self::confirmed_as_asked(
            &policy,
            &descriptor,
            argument.as_deref(),
            request.confirmation.as_ref(),
            approved,
        ) {
            return Err(abandon(ActionCallError::confirmation_required()));
        }
        // What a step of an agent session does in turn goes through the session's own api.
        let through = match (&session, origin) {
            (Some(Ok(_)), Some(origin)) => {
                self.agent_session_api(caller, origin.clone()).map(|api| {
                    Arc::new(api) as Arc<dyn tilo_actions::remote::ActionProvider + Send + Sync>
                })
            }
            _ => None,
        };
        Ok(RegisteredAdmission::Call(RegisteredCall {
            descriptor,
            approved,
            through,
        }))
    }

    /// Take one call [`Self::admit_registered`] admitted, and hand it to its provider.
    #[allow(
        clippy::too_many_lines,
        reason = "one call's admission, effect and answer in a single auditable sequence"
    )]
    fn take_registered(
        self: &Arc<Self>,
        caller: &Caller,
        origin: Option<&super::published_invocation::InvocationCaller>,
        serving: Option<Arc<dyn Serving>>,
        registered: &Registered,
        request: &InvokeRequest,
        call: RegisteredCall,
    ) -> Result<InvocationReceipt, ActionCallError> {
        let RegisteredCall {
            descriptor,
            approved,
            through,
        } = call;
        let abandon = |error: ActionCallError| {
            if let Ok(mut log) = self.remote_invocations.lock() {
                log.abandon(&request.invocation);
            }
            error
        };
        let Some(workspace_id) = caller.workspace() else {
            return Err(abandon(ActionCallError::NotPermitted));
        };
        let argument = request.argument.to_slot();
        let app_id = deciding_app(registered, serving.as_ref());
        let admitted = self
            .admit_host_action(
                caller,
                origin,
                super::host_action_admission::HostActionAdmission {
                    workspace: workspace_id,
                    app_id,
                    descriptor: &descriptor,
                    request,
                },
            )
            .map_err(abandon)?;
        let id = admitted.id;
        if let Ok(mut log) = self.remote_invocations.lock() {
            log.complete(&request.invocation, id);
        }
        // Signing and journalling may block. Resolve original ownership and the committed policy
        // again for the effect, not the pre-admission actor.
        self.framework_effect_ready(&admitted, || {
            let (workspace, policy, now) =
                self.current_framework_authority(caller, origin, workspace_id)?;
            if !Self::confirmed_as_asked(
                &policy,
                &descriptor,
                argument.as_deref(),
                request.confirmation.as_ref(),
                approved,
            ) {
                return Err(ActionCallError::confirmation_required());
            }
            self.admit_answer(
                (&workspace, &policy, now),
                app_id,
                &descriptor,
                (argument.as_deref(), request.confirmation.as_ref()),
            )?;
            Ok(())
        })?;
        // A call whose contract declares it cancellable may be asked to stop while it runs: its
        // caller's cancel sets this flag, and the call stops and answers as cancelled.
        let stop = Arc::new(AtomicBool::new(false));
        if descriptor.contract.execution.cancellable()
            && let Ok(mut stops) = self.host_step_stops.lock()
        {
            stops.insert(id, Arc::clone(&stop));
        }
        if descriptor.contract.is_live() {
            let view = match registered.answers.watch(caller, request) {
                Ok(view) => view,
                Err(reason) => {
                    self.complete_host_action(
                        &admitted,
                        host_step_failure(request.invocation, "host_step_refused", &reason),
                    )?;
                    return Err(ActionCallError::Refused { reason });
                }
            };
            let following = super::live_views::Following {
                caller: caller.clone(),
                origin: origin.cloned(),
                workspace: workspace_id.to_owned(),
                app_id: app_id.to_string(),
                descriptor,
                approved,
            };
            self.follow_live_view(admitted, following, view)
                .map_err(|reason| ActionCallError::Refused { reason })?;
            return Ok(self.receipt_for(request.invocation, id));
        }
        let progress = {
            let (state, id, invocation, owner) = (
                Arc::downgrade(self),
                admitted.id,
                admitted.invocation,
                admitted.owner.clone(),
            );
            Arc::new(move |phase: &str, done: Option<u64>, total: Option<u64>| {
                if let Some(state) = state.upgrade() {
                    state.host_step_progress((id, invocation, &owner), phase, done, total);
                }
            })
        };
        let mut receipt = registered.answers.run(
            caller,
            request.clone(),
            Admitted {
                through,
                stop: Arc::clone(&stop),
                serving,
                progress,
            },
        );
        // A call this provider runs on goes on after `Accepted`: its observation and
        // cancellation reach the provider itself, not the record this dispatch keeps of it.
        let receipt = match receipt.poll_ready() {
            PendingPoll::Ready(receipt) => {
                if receipt
                    .as_ref()
                    .is_ok_and(|receipt| matches!(&receipt.status, ReceiptStatus::Accepted))
                    && let Some(providers) = &self.services.providers
                {
                    providers.took(request.invocation, &registered.provider);
                }
                Pending::ready(receipt)
            }
            PendingPoll::Waiting => receipt,
            PendingPoll::Abandoned => Pending::ready(Err(ActionCallError::Unanswered)),
        };
        let started = outcome_of(
            Arc::clone(&registered.answers),
            caller.clone(),
            request.invocation,
            receipt,
            Arc::clone(&stop),
        );
        self.follow_admitted_host_step(admitted, started, stop);
        Ok(self.receipt_for(request.invocation, id))
    }

    /// How far host step `id`, which `owner` asked for and which still runs, got: whoever
    /// follows it reads `phase`, and `done` of `total`, until it ends.
    fn host_step_progress(
        &self,
        (id, invocation, owner): (u64, tilo_actions::InvocationId, &super::InvocationOwner),
        phase: &str,
        done: Option<u64>,
        total: Option<u64>,
    ) {
        let Ok(mut completed) = self.completed_invocations.lock() else {
            return;
        };
        let sequence = match completed.get(&id) {
            Some((kept, update)) if kept == owner && !update.is_terminal() => match update {
                tilo_actions::InvocationUpdate::Progress { sequence, .. } => {
                    sequence.saturating_add(1)
                }
                _ => 1,
            },
            Some(_) => return,
            None => 1,
        };
        completed.insert(
            id,
            (
                owner.clone(),
                tilo_actions::InvocationUpdate::Progress {
                    invocation,
                    sequence,
                    phase: phase.to_owned(),
                    completed: done,
                    total,
                    message: None,
                },
            ),
        );
        drop(completed);
        self.invocation_changed();
    }

    /// Ask cancellable host step `id` to stop, if it is one: whether it was. The step's caller
    /// cancelled it, and it answers as cancelled when it stops.
    pub(super) fn stop_host_step(&self, id: u64) -> bool {
        let stop = self
            .host_step_stops
            .lock()
            .ok()
            .and_then(|stops| stops.get(&id).cloned());
        stop.is_some_and(|stop| {
            stop.store(true, std::sync::atomic::Ordering::Release);
            true
        })
    }

    /// Follow an already durable admission without replacing its caller or UUID. `stop` is the
    /// flag its caller's cancel sets, for a cancellable step: a step that stopped answers as
    /// cancelled whatever its service said.
    pub(super) fn follow_admitted_host_step(
        self: &Arc<Self>,
        admitted: super::host_action_admission::AdmittedHostAction,
        started: Pending<Outcome>,
        stop: Arc<std::sync::atomic::AtomicBool>,
    ) {
        let state = Arc::clone(self);
        started.then(move |outcome| {
            let invocation = admitted.invocation;
            let uncertain = outcome.is_none();
            let stopped = stop.load(std::sync::atomic::Ordering::Acquire);
            // What a stopped step said of why, where it said anything.
            let said = outcome
                .as_ref()
                .and_then(|ended| ended.as_ref().err())
                .map(|error| error.message.clone());
            let update = match outcome {
                _ if stopped => tilo_actions::InvocationUpdate::Cancelled {
                    invocation,
                    message: said,
                },
                Some(Ok(output)) => {
                    tilo_actions::InvocationUpdate::Succeeded { invocation, output }
                }
                Some(Err(error)) => tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: *error,
                },
                None => host_step_failure(
                    invocation,
                    "host_step_stopped",
                    "the host step stopped without an answer",
                ),
            };
            let completed = if uncertain && !stopped {
                state.complete_uncertain_host_action(&admitted, update)
            } else {
                state.complete_host_action(&admitted, update)
            };
            if let Err(error) = completed {
                tracing::warn!(?error, "could not persist host step completion");
            }
        });
    }
}

fn host_step_failure(
    invocation: tilo_actions::InvocationId,
    code: &str,
    message: &str,
) -> tilo_actions::InvocationUpdate {
    tilo_actions::InvocationUpdate::Failed {
        invocation,
        error: tilo_actions::ActionFailure {
            code: code.into(),
            message: message.into(),
            details: None,
            retryable: true,
        },
    }
}

#[cfg(test)]
pub(crate) mod signed_test_support {
    use super::super::{
        AppBrokerState, BrokerChannels, DurableInvocation, FanoutState, InvocationKey,
        PeerPublisher, person_id_for_root, published_invocation::InvocationCaller,
    };
    use crate::apps::services::ProcessServices;
    use crate::apps::test_support::{Workspace, WorkspaceTree};
    use ed25519_dalek::Signer as _;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };
    use tilo_actions::remote::{ActionCallError, Caller, InvocationReceipt, InvokeRequest};

    struct NativeOwnerReceipts {
        owner: Arc<tilo_peer_state::owner::NativeWorkspaceOwner>,
    }

    impl tilo_broker_core::workspace_commits::WorkspaceCommits for NativeOwnerReceipts {
        fn projects_workspace(&self, _: tilo_desktop_model::WorkspaceId) -> bool {
            true
        }

        fn health(&self) -> Result<(), String> {
            let workspace =
                tilo_desktop_model::WorkspaceId::parse(tilo_peer_state::owner::testing::WORKSPACE)
                    .unwrap();
            self.owner.with_current(workspace, |_| Ok(()))
        }

        fn effect(
            &self,
            workspace: String,
            redacted: Vec<u8>,
        ) -> tilo_broker_core::workspace_commits::EffectFuture {
            let owner = self.owner.clone();
            Box::pin(async move {
                let workspace = tilo_desktop_model::WorkspaceId::parse(&workspace)
                    .ok_or_else(|| "invalid workspace".to_owned())?;
                let (device, receipt) = owner.with_current(workspace, |view| {
                    let receipt: tilo_desktop_model::WorkspaceEffectReceipt =
                        serde_json::from_slice(&redacted).map_err(|error| error.to_string())?;
                    Ok((view.device().clone(), receipt))
                })?;
                owner.record_effect(workspace, &device, &receipt, tilo_clock::wall())
            })
        }
    }

    pub(crate) struct Authority {
        certificate: tilo_user_identity::DeviceCert,
        pub revoked: AtomicBool,
        pub device_governs: AtomicBool,
        time: Mutex<crate::apps::policy_clock::PolicyTime>,
        replace_time_on_authorize: Mutex<Option<crate::apps::policy_clock::PolicyTime>>,
    }

    impl Authority {
        pub fn replace_time_on_authorize(&self, time: crate::apps::policy_clock::PolicyTime) {
            *self.replace_time_on_authorize.lock().unwrap() = Some(time);
        }
    }

    impl crate::apps::certificate_authority::CertificateAuthority for Authority {
        fn governs(&self, _: uuid::Uuid, device: Option<&[u8; 32]>) -> bool {
            device.is_none() || self.device_governs.load(Ordering::SeqCst)
        }

        fn policy_time(&self) -> Result<Option<crate::apps::policy_clock::PolicyTime>, String> {
            Ok(Some(self.time.lock().unwrap().clone()))
        }

        fn authorize(
            &self,
            _workspace: uuid::Uuid,
            certificate: &tilo_user_identity::DeviceCert,
        ) -> Result<(), String> {
            let replacement = self.replace_time_on_authorize.lock().unwrap().take();
            if let Some(time) = replacement {
                *self.time.lock().unwrap() = time;
            }
            if self.revoked.load(Ordering::SeqCst) || certificate != &self.certificate {
                return Err("device is no longer enrolled".into());
            }
            tilo_user_identity::verify_device_cert(certificate).map_err(|error| error.to_string())
        }
    }

    pub(crate) struct Fixture {
        pub state: Arc<AppBrokerState>,
        pub caller: Caller,
        pub origin: InvocationCaller,
        pub authority: Arc<Authority>,
        pub workspace: String,
        pub tree: crate::apps::test_support::WorkspaceTreeHandle,
        certified_host: Option<Arc<dyn tilo_actions::remote::ActionProvider>>,
        _fanout: Arc<FanoutState>,
        _environment: tilo_test_env::ScopedEnv,
        directory: tempfile::TempDir,
        clock: crate::apps::policy_clock::OwnerPolicyClock,
        policy_origin: crate::apps::policy_clock::PolicyOrigin,
    }

    impl Fixture {
        pub fn new_policy_origin(&self) -> crate::apps::policy_clock::PolicyOrigin {
            self.clock.new_origin()
        }

        pub fn retire_policy_origin(&self) {
            self.policy_origin.invalidate();
        }

        pub fn new(services: ProcessServices) -> Self {
            let directory = tempfile::tempdir().unwrap();
            let mut environment = tilo_test_env::ScopedEnv::new();
            environment
                .set("XDG_CONFIG_HOME", directory.path().join("config"))
                .set("TILO_IDENTITY_ROOT", directory.path().join("identity"))
                .set(tilo_identity::provisioning::FILE_SECRETS_ENV, "1");
            tilo_identity::sources::test_trust::install();
            tilo_identity::provisioning::ensure_collaboration_identity(
                &tilo_identity::store::account_identity_dir(
                    &directory.path().join("config/tilo/identity"),
                ),
                &tilo_device_identity::IdentityPaths::under(directory.path().join("identity")),
                tilo_identity::provisioning::IdentityStorage::Files,
            )
            .expect("provision the production receipt signer");
            let mut workspace = Workspace::new(
                tilo_desktop_model::WorkspaceId::parse(tilo_peer_state::owner::testing::WORKSPACE)
                    .unwrap(),
                "Host actions",
            );
            let workspace_id = workspace.id().to_string();
            let (key, certificate) = tilo_identity::trusted_device::current(&workspace_id).unwrap();
            let owner = Arc::new(
                tilo_peer_state::owner::open_file(&directory.path().join("native.json")).unwrap(),
            );
            tilo_peer_state::owner::testing::add_interactive_device(&owner, &key, &certificate)
                .unwrap();
            let publisher = PeerPublisher::with_commits(Arc::new(NativeOwnerReceipts { owner }));
            let person = person_id_for_root(certificate.user_public_key);
            workspace.policy.set_owner(person.clone());
            let mut tree = WorkspaceTree::default();
            tree.workspaces.push(workspace);
            let tree = tree.shared();
            let clock = crate::apps::policy_clock::OwnerPolicyClock::new().unwrap();
            let policy_origin = clock.new_origin();
            let authority = Arc::new(Authority {
                certificate: certificate.clone(),
                revoked: AtomicBool::new(false),
                device_governs: AtomicBool::new(true),
                time: Mutex::new(policy_origin.capture().unwrap()),
                replace_time_on_authorize: Mutex::new(None),
            });
            let config = crate::apps::NativePeerBrokerConfig {
                journal_root: directory.path().join("journal"),
                certificate_authority: authority.clone(),
            };
            let fanout = Arc::new(FanoutState::new());
            let (state, _) = AppBrokerState::new_configured(
                &fanout,
                Arc::new(Mutex::new(BrokerChannels::default())),
                publisher,
                Some(tree.authority()),
                None,
                Some(&config),
                services,
            );
            let presentation = runtime_collab::CertifiedJoinIdentity {
                workspace_id: workspace_id.clone(),
                device_certificate: serde_json::to_vec(&certificate).unwrap(),
                proof: key
                    .sign(&tilo_identity_client::collaboration_join_digest(&workspace_id).unwrap())
                    .to_bytes()
                    .to_vec(),
            }
            .encode()
            .unwrap();
            let verified = state
                .verify_action_caller(&workspace_id, &presentation)
                .unwrap();
            let origin = InvocationCaller::Local {
                route: super::super::CrdtRoute::cluster(&workspace_id),
                admitted: Box::new(verified.admitted),
                time: state.registered_policy_time().unwrap(),
            };
            Self {
                state,
                caller: Caller::local_person(person).in_workspace(workspace_id.clone()),
                origin,
                authority,
                workspace: workspace_id,
                tree,
                certified_host: None,
                _fanout: fanout,
                _environment: environment,
                directory,
                clock,
                policy_origin,
            }
        }

        pub fn new_certified(services: ProcessServices) -> Self {
            Self::certified(services, false)
        }

        pub fn new_certified_foreign(services: ProcessServices) -> Self {
            Self::certified(services, true)
        }

        fn certified(services: ProcessServices, foreign: bool) -> Self {
            let mut fixture = Self::new(services);
            let (key, certificate) =
                tilo_identity::trusted_device::current(&fixture.workspace).unwrap();
            let presentation = runtime_collab::CertifiedJoinIdentity {
                workspace_id: fixture.workspace.clone(),
                device_certificate: serde_json::to_vec(&certificate).unwrap(),
                proof: key
                    .sign(
                        &tilo_identity_client::collaboration_join_digest(&fixture.workspace)
                            .unwrap(),
                    )
                    .to_bytes()
                    .to_vec(),
            }
            .encode()
            .unwrap();
            fixture.caller =
                Caller::authenticated(certificate.user_public_key, certificate.device_public_key)
                    .in_workspace(&fixture.workspace)
                    .from_foreign_device(foreign)
                    .in_new_execution();
            fixture.certified_host = Some(
                fixture
                    .state
                    .certified_local_action_provider(&fixture.caller, &presentation, false)
                    .unwrap(),
            );
            fixture.origin = InvocationCaller::Certified {
                caller: Box::new(
                    fixture
                        .state
                        .verify_action_caller(&fixture.workspace, &presentation)
                        .unwrap(),
                ),
                foreign_restriction: foreign,
            };
            fixture
        }

        fn owner(&self) -> super::super::InvocationOwner {
            match &self.origin {
                InvocationCaller::Certified { caller, .. } => caller.owner().unwrap(),
                InvocationCaller::Local { admitted, .. } => {
                    let certificate = admitted.certificate.as_ref().unwrap();
                    super::super::InvocationOwner::AuthenticatedLocal {
                        root: certificate.user_public_key,
                        device: certificate.device_public_key,
                        agent: None,
                    }
                }
                InvocationCaller::NativePerson { .. } => panic!("authenticated ownership"),
            }
        }

        pub fn invoke(
            &self,
            request: &InvokeRequest,
        ) -> Result<InvocationReceipt, ActionCallError> {
            let pending = match &self.certified_host {
                Some(host) => host.invoke(&self.caller, request.clone()),
                None => self.state.action_invoke_with_origin(
                    &self.caller,
                    None,
                    None,
                    Some(&self.origin),
                    request,
                ),
            };
            pending
                .wait_timeout(tilo_test_env::hang_bound())
                .ok()
                .flatten()
                .expect("host admission answers immediately")
        }

        pub fn query(&self, provider: &str) -> tilo_actions::remote::CataloguePage {
            let query = tilo_actions::remote::CatalogueQuery {
                provider: Some(provider.into()),
                ..Default::default()
            };
            let pending = match &self.certified_host {
                Some(host) => host.query(&self.caller, query),
                None => {
                    self.state
                        .action_query_with_origin(&self.caller, Some(&self.origin), &query)
                }
            };
            pending
                .wait_timeout(tilo_test_env::hang_bound())
                .ok()
                .flatten()
                .expect("catalogue answers immediately")
                .expect("the certified caller may query")
        }

        #[allow(
            clippy::significant_drop_tightening,
            reason = "the test keeps its fixture and guards alive through its assertions"
        )]
        pub fn admission_check(&self, request: &InvokeRequest) -> Box<dyn Fn() + Send> {
            let key = InvocationKey {
                workspace: self.workspace.clone(),
                host: self.state.invocation_host().unwrap(),
                owner: self.owner(),
                invocation: request.invocation,
            };
            let state = Arc::downgrade(&self.state);
            Box::new(move || {
                let state = state.upgrade().expect("host is live at dispatch");
                let guard = state.journal.lock().unwrap();
                let journal = guard.as_ref().unwrap().as_ref().unwrap();
                let record = journal
                    .lookup(&key)
                    .unwrap()
                    .expect("durable before dispatch");
                assert!(record.admission.is_some(), "signed before dispatch");
                assert!(record.terminal.is_none(), "not yet completed");
                assert!(
                    matches!(record.update, tilo_actions::InvocationUpdate::Accepted { invocation } if invocation == key.invocation)
                );
            })
        }

        #[allow(
            clippy::significant_drop_tightening,
            reason = "the test keeps its fixture and guards alive through its assertions"
        )]
        pub fn record(&self, request: &InvokeRequest) -> DurableInvocation {
            let guard = self.state.journal.lock().unwrap();
            let journal = guard.as_ref().unwrap().as_ref().unwrap();
            journal
                .lookup(&InvocationKey {
                    workspace: self.workspace.clone(),
                    host: journal.host(),
                    owner: self.owner(),
                    invocation: request.invocation,
                })
                .unwrap()
                .expect("durable host admission")
        }

        pub fn fail_next_barrier(&self) {
            self.state
                .journal
                .lock()
                .unwrap()
                .as_mut()
                .unwrap()
                .as_mut()
                .unwrap()
                .fail_at = Some(super::super::invocation_journal::BarrierFailure::File);
        }

        pub fn reopen_journal(&self) {
            let mut guard = self.state.journal.lock().unwrap();
            drop(guard.as_mut().unwrap().take());
            *guard = Ok(Some(
                super::super::InvocationJournal::open(&self.directory.path().join("journal"))
                    .unwrap(),
            ));
        }

        pub fn assert_signed(&self, request: &InvokeRequest, terminal: bool) -> DurableInvocation {
            let record = self.record(request);
            let admission = record.admission.as_ref().expect("signed admission");
            let receipt = admission.public_receipt();
            crate::apps::action_evidence::verify(receipt, &receipt.facts).unwrap();
            assert_eq!(receipt.facts.invocation, request.invocation);
            assert_eq!(receipt.facts.workspace, self.workspace);
            assert_eq!(receipt.facts.resource, request.target.address.resource);
            assert_eq!(receipt.facts.action, request.target.address.action);
            assert!(record.admitted_request_is(&serde_json::to_value(request).unwrap()));
            if terminal {
                crate::apps::action_evidence::verify_terminal(
                    record.terminal.as_ref().expect("signed terminal"),
                    receipt,
                )
                .unwrap();
            } else {
                assert!(record.terminal.is_none(), "admission is not execution");
            }
            record
        }
    }
}

#[cfg(test)]
mod identity_admin_tests {
    use super::super::AppBrokerState;
    use super::super::action_provider::RowScope;
    use crate::apps::test_support::{Grants as _, Workspace, WorkspaceId, WorkspaceTree};
    use crate::workspaces::providers::{Providers, Reach};
    use std::sync::{Arc, Mutex};
    use tilo_actions::remote::admitted::top_level;
    use tilo_actions::remote::{
        ActionCallError, ActionProvider, Admitted, Caller, Completer, Confirmation, InvokeArgument,
        InvokeRequest, Outcome, Pending, RemoteActionMessage, device_tickets, pending,
    };

    /// The host's identity store, in its place: it records each step it was
    /// asked to start and answers when the test completes it.
    #[derive(Default)]
    struct Host {
        started: Mutex<Vec<(String, tilo_actions::Actor, String)>>,
        answers: Mutex<Vec<Completer<Outcome>>>,
        admission_check: Mutex<Option<Box<dyn Fn() + Send>>>,
    }

    /// `host` registered on this device as the identity store's provider.
    fn providers(host: Arc<Host>) -> Providers {
        let providers = Providers::default();
        providers.provide(
            device_tickets::PROVIDER,
            "so.tilo.settings",
            Reach::ThisDevice,
            host as Arc<dyn ActionProvider + Send + Sync>,
        );
        providers
    }

    fn contract(step: &str) -> tilo_actions::ActionContract {
        match step {
            device_tickets::IDENTITIES => tilo_actions::ActionContract::view(),
            _ => tilo_actions::ActionContract {
                kind: tilo_actions::ActionKind::Outside,
                risk: tilo_actions::RiskLevel::Privileged,
                confirmation: tilo_actions::ConfirmationPolicy::Confirm,
                ..tilo_actions::ActionContract::default()
            },
        }
    }

    impl tilo_actions::remote::Admits for Host {
        const PROVIDER: &'static str = device_tickets::PROVIDER;
        const APP: &'static str = "so.tilo.settings";

        fn rows(&self) -> Vec<tilo_actions::api::PlacedAction> {
            top_level(self.descriptors())
        }

        fn start(
            &self,
            caller: &Caller,
            request: &InvokeRequest,
            _admitted: &Admitted,
        ) -> Result<Pending<Outcome>, String> {
            if let Some(check) = self.admission_check.lock().unwrap().as_ref() {
                check();
            }
            let workspace = caller
                .workspace()
                .ok_or("the step was asked in no workspace")?;
            self.started.lock().unwrap().push((
                workspace.to_owned(),
                tilo_broker_core::BrokerActor::for_caller(caller)
                    .actor()
                    .clone(),
                request.target.address.action.clone(),
            ));
            let (answer, pending) = pending();
            self.answers.lock().unwrap().push(answer);
            Ok(pending)
        }
    }

    impl Host {
        fn descriptors(&self) -> Vec<tilo_actions::ActionDescriptor> {
            device_tickets::IDENTITY_STEPS
                .into_iter()
                .map(|step| tilo_actions::ActionDescriptor {
                    did: None,
                    state: tilo_actions::ActionState::default(),
                    semantic_identity: None,
                    address: tilo_actions::ActionAddress::new(device_tickets::RESOURCE, step),
                    resource_kind: device_tickets::RESOURCE.into(),
                    resource_label: "Identities".into(),
                    action_label: step.into(),
                    keywords: Vec::new(),
                    icon: None,
                    description: None,
                    locus: tilo_actions::ExecutionLocus::default(),
                    kind: tilo_actions::OperationKind::Effect,
                    contract: contract(step),
                    input: tilo_actions::InputSpec::None,
                    params: Vec::new(),
                    view: None,
                })
                .collect()
        }
    }

    fn broker(host: Arc<Host>) -> (Arc<AppBrokerState>, String, String) {
        let owner = hex::encode([41; 32]);
        let mut workspace = Workspace::new(WorkspaceId::from_u128(9), "Identities");
        workspace.policy.set_owner(owner.clone());
        let workspace_id = workspace.id().to_string();
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let (state, _bridge_rx) = AppBrokerState::test_state(
            Some(tree.shared().authority()),
            crate::apps::services::ProcessServices {
                providers: Some(providers(host)),
                ..Default::default()
            },
        );
        (state, owner, workspace_id)
    }

    /// A native host's device, which never certifies anyone here.
    struct NoDevice;

    impl crate::apps::services::LocalPersonServices for NoDevice {
        fn certificate(
            &self,
            _workspace: &str,
            _person: &str,
        ) -> Result<tilo_user_identity::DeviceCert, String> {
            Err("no device in this test".into())
        }
    }

    tilo_test_env::scenario! {
        #[allow(
            clippy::significant_drop_tightening,
            reason = "the test keeps its fixture and guards alive through its assertions"
        )]
        fn host_steps_admit_only_certified_local_ownership;

        phase a_keyless_agent_or_person_takes_no_host_step_on_a_native_host;
        // On a native host (one with the person's device), neither an agent nor a
        // person that carries no retained ownership takes a host step: an agent
        // takes one only inside an agent session, a person only through the
        // device. Neither is taken as an actor-only caller.
        {
            let host = Arc::new(Host::default());
            let owner = hex::encode([41; 32]);
            let mut workspace = Workspace::new(WorkspaceId::from_u128(9), "Identities");
            workspace.policy.set_owner(owner.clone());
            workspace.allow_agent("planner", crate::apps::test_support::KindSet::READ);
            let workspace_id = workspace.id().to_string();
            let mut tree = WorkspaceTree::default();
            tree.workspaces.push(workspace);
            let (state, _bridge_rx) = AppBrokerState::test_state(
                Some(tree.shared().authority()),
                crate::apps::services::ProcessServices {
                    providers: Some(providers(host.clone())),
                    local_person: Some(Arc::new(NoDevice)),
                    ..Default::default()
                },
            );
            let agent = Caller::local_agent("planner").in_workspace(workspace_id.clone());
            assert!(
                invoked(&state, &agent, &request(device_tickets::IDENTITIES, None)).is_err(),
                "an agent outside any run takes no host step"
            );
            let person = Caller::local_person(owner).in_workspace(workspace_id);
            assert!(
                invoked(&state, &person, &request(device_tickets::IDENTITIES, None)).is_err(),
                "a person reaches a native host's steps only with the device's ownership"
            );
            assert!(host.started.lock().unwrap().is_empty());
        }

        phase a_host_service_is_listed_when_only_publishers_are_asked;
        // Asking only who publishes (`max_results: 0`, how an agent's paths
        // find an app) lists the host's own services beside apps' windows, and
        // builds no row; asking a service for its rows still gets them.
        {
            let host = Arc::new(Host::default());
            let (state, owner, workspace) = broker(host);
            let caller = Caller::local_person(owner).in_workspace(workspace.clone());
            let ask = |query: tilo_actions::remote::CatalogueQuery| {
                state
                    .action_query(&caller, &query)
                    .wait_timeout(tilo_test_env::hang_bound())
                    .ok()
                    .flatten()
                    .expect("the catalogue answers at once")
                    .expect("the workspace's owner may ask")
            };
            let publishers = ask(tilo_actions::remote::CatalogueQuery {
                max_results: 0,
                ..Default::default()
            });
            assert!(publishers.rows.is_empty(), "{:?}", publishers.rows);
            assert!(
                publishers.providers.iter().any(|provider| {
                    provider.provider == device_tickets::PROVIDER
                        && provider.app_id == "so.tilo.settings"
                        && provider.workspace == workspace
                        && provider.browsable
                }),
                "{:?}",
                publishers.providers
            );
            let rows = ask(tilo_actions::remote::CatalogueQuery {
                provider: Some(device_tickets::PROVIDER.into()),
                ..Default::default()
            });
            assert!(
                rows.rows
                    .iter()
                    .any(|row| row.provider == device_tickets::PROVIDER),
                "{:?}",
                rows.rows
            );
        }

        phase certified_host_steps_admit_only_nonforeign_local_ownership;
        {
            for foreign in [false, true] {
                let service = Arc::new(Host::default());
                let services = crate::apps::services::ProcessServices {
                    providers: Some(providers(service.clone())),
                    ..Default::default()
                };
                let f = if foreign {
                    super::signed_test_support::Fixture::new_certified_foreign(services)
                } else {
                    super::signed_test_support::Fixture::new_certified(services)
                };
                let mut asked = request(device_tickets::REMOVE_IDENTITY, Some(Confirmation::Confirmed));
                assert_eq!(
                    f.query(&asked.target.provider).rows.is_empty(),
                    foreign,
                    "only the retained local api offers host steps"
                );
                assert!(
                    f.state
                        .eager_catalogue(
                            &f.caller,
                            super::super::action_provider::RowScope::Provider(&asked.target.provider),
                        )
                        .0
                        .is_empty(),
                    "transport keys alone do not offer local host steps"
                );
                if foreign {
                    assert!(f.invoke(&asked).is_err());
                    assert!(service.started.lock().unwrap().is_empty());
                    continue;
                }
                assert!(
                    invoked(&f.state, &f.caller, &asked).is_err(),
                    "device keys without certified local ownership are not a local api"
                );
                assert!(service.started.lock().unwrap().is_empty());
                *service.admission_check.lock().unwrap() = Some(f.admission_check(&asked));
                f.invoke(&asked).unwrap();
                f.assert_signed(&asked, false);
                assert_eq!(service.started.lock().unwrap().len(), 1);
                service
                    .answers
                    .lock()
                    .unwrap()
                    .pop()
                    .unwrap()
                    .complete(Ok(tilo_actions::ActionOutput::default()));
                f.assert_signed(&asked, true);
                f.invoke(&asked).unwrap();
                assert_eq!(service.started.lock().unwrap().len(), 1);
                f.authority
                    .revoked
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                asked.invocation = tilo_actions::InvocationId::new();
                assert!(f.invoke(&asked).is_err());
                drop(f);
                assert_eq!(service.started.lock().unwrap().len(), 1);
            }
        }

        phase host_steps_recheck_live_authority_after_blocked_durable_admission;
        {
            use std::sync::{atomic::Ordering, mpsc};
            use tilo_actions::InvocationUpdate;

            for certified in [false, true] {
                for change in [
                    "unchanged",
                    "certificate",
                    "execution",
                    "permission",
                    "time",
                ] {
                    let host = Arc::new(Host::default());
                    let services = crate::apps::services::ProcessServices {
                        providers: Some(providers(host.clone())),
                        ..Default::default()
                    };
                    let mut f = if certified {
                        super::signed_test_support::Fixture::new_certified(services)
                    } else {
                        super::signed_test_support::Fixture::new(services)
                    };
                    f.caller = f.caller.clone().in_new_execution();
                    let asked = request(device_tickets::REMOVE_IDENTITY, Some(Confirmation::Confirmed));
                    let (entered, blocked) = mpsc::sync_channel(0);
                    let (release, resume) = mpsc::sync_channel(0);
                    f.state
                        .journal
                        .lock()
                        .unwrap()
                        .as_mut()
                        .unwrap()
                        .as_mut()
                        .unwrap()
                        .after_admission = Some(Box::new(move || {
                        entered.send(()).unwrap();
                        resume.recv_timeout(tilo_test_env::hang_bound()).unwrap();
                    }));
                    let result = std::thread::scope(|scope| {
                        let state = f.state.clone();
                        let caller = f.caller.clone();
                        let origin = f.origin.clone();
                        let steps = host.clone();
                        let asked = &asked;
                        let worker = scope.spawn(move || {
                            let registered = crate::workspaces::providers::Registered {
                                provider: device_tickets::PROVIDER.into(),
                                app: "so.tilo.settings".into(),
                                reach: Reach::ThisDevice,
                                answers: steps as Arc<dyn ActionProvider + Send + Sync>,
                                published: Vec::new(),
                            };
                            state
                                .invoke_registered(&caller, Some(&origin), None, &registered, asked)
                                .wait_timeout(tilo_test_env::hang_bound())
                                .ok()
                                .flatten()
                                .expect("the call is answered")
                        });
                        blocked.recv_timeout(tilo_test_env::hang_bound()).unwrap();
                        // The journal writer holds a signed admission but has not
                        // returned a dispatch permit. Change authority in that gap.
                        match change {
                            "certificate" => f.authority.revoked.store(true, Ordering::SeqCst),
                            "execution" => f.caller.retire_execution(),
                            "time" => f.retire_policy_origin(),
                            "permission" => f.tree.write().workspaces[0].policy.set_owner("someone-else"),
                            _ => {}
                        }
                        release.send(()).unwrap();
                        worker.join().unwrap()
                    });
                    if change == "unchanged" {
                        result.expect("unchanged ownership must still start");
                        assert_eq!(host.started.lock().unwrap().len(), 1);
                        host.answers
                            .lock()
                            .unwrap()
                            .pop()
                            .unwrap()
                            .complete(Ok(tilo_actions::ActionOutput::default()));
                        let record = f.assert_signed(&asked, true);
                        assert!(matches!(record.update, InvocationUpdate::Succeeded { .. }));
                    } else {
                        assert!(result.is_err(), "{certified}/{change}");
                        assert!(
                            host.started.lock().unwrap().is_empty(),
                            "{certified}/{change}"
                        );
                        let record = f.assert_signed(&asked, true);
                        assert!(!record.uncertain);
                        assert!(
                            matches!(record.update, InvocationUpdate::Failed { ref error, .. }
                            if error.code == "host_action_refused")
                        );
                    }
                }
            }
        }
    }

    fn request(step: &str, confirmation: Option<Confirmation>) -> InvokeRequest {
        InvokeRequest {
            on_behalf_of: None,
            invocation: tilo_actions::InvocationId::new(),
            target: device_tickets::target(step),
            argument: InvokeArgument::None,
            context: None,
            confirmation,
            revision: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        }
    }

    fn invoked(
        state: &Arc<AppBrokerState>,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<tilo_actions::remote::InvocationReceipt, ActionCallError> {
        state
            .action_invoke(caller, None, None, request)
            .wait_timeout(tilo_test_env::hang_bound())
            .ok()
            .flatten()
            .expect("admission answers at once")
    }

    tilo_test_env::scenario! {
        #[allow(
            clippy::significant_drop_tightening,
            reason = "the test keeps its fixture and guards alive through its assertions"
        )]
        fn identity_admin_is_signed_journaled_and_fail_closed;

        phase identity_admin_signs_before_start_and_retains_the_original_uuid;
        {
            let host = Arc::new(Host::default());
            let f = super::signed_test_support::Fixture::new(crate::apps::services::ProcessServices {
                providers: Some(providers(host.clone())),
                ..Default::default()
            });
            let asked = request(device_tickets::REMOVE_IDENTITY, Some(Confirmation::Confirmed));
            *host.admission_check.lock().unwrap() = Some(f.admission_check(&asked));
            let receipt = f.invoke(&asked).unwrap();
            let accepted = f.assert_signed(&asked, false);
            assert_eq!(receipt.invocation, asked.invocation);
            assert!(
                matches!(accepted.update, tilo_actions::InvocationUpdate::Accepted { invocation } if invocation == asked.invocation)
            );
            let repeated = f.invoke(&asked).unwrap();
            assert_eq!(receipt, repeated);
            assert_eq!(host.started.lock().unwrap().len(), 1);
            let mut changed = asked.clone();
            changed.argument = InvokeArgument::Text("another identity".into());
            assert!(
                f.invoke(&changed).is_err(),
                "changed retry cannot start another step"
            );
            host.answers
                .lock()
                .unwrap()
                .pop()
                .unwrap()
                .complete(Ok(tilo_actions::ActionOutput::default()));
            let completed = f.assert_signed(&asked, true);
            assert!(
                matches!(completed.update, tilo_actions::InvocationUpdate::Succeeded { invocation, .. } if invocation == asked.invocation)
            );
            assert_eq!(completed.admission, accepted.admission);
            f.reopen_journal();
            assert_eq!(f.assert_signed(&asked, true), completed);
        }

        phase identity_admin_persists_failed_and_dropped_host_outcomes;
        {
            for dropped in [false, true] {
                let host = Arc::new(Host::default());
                let f = super::signed_test_support::Fixture::new(crate::apps::services::ProcessServices {
                    providers: Some(providers(host.clone())),
                    ..Default::default()
                });
                let asked = request(device_tickets::REMOVE_IDENTITY, Some(Confirmation::Confirmed));
                f.invoke(&asked).unwrap();
                // The service has accepted the effect; losing its answer does not
                // prove that the effect failed or make it safe to repeat.
                assert_eq!(host.started.lock().unwrap().len(), 1);
                let answer = host.answers.lock().unwrap().pop().unwrap();
                if dropped {
                    drop(answer);
                } else {
                    answer.complete(Err(Box::new(tilo_actions::ActionFailure {
                        code: "identity_busy".into(),
                        message: "the identity is in use".into(),
                        details: None,
                        retryable: true,
                    })));
                }
                let completed = f.assert_signed(&asked, true);
                assert!(
                    matches!(completed.update, tilo_actions::InvocationUpdate::Failed { invocation, .. } if invocation == asked.invocation)
                );
                assert_eq!(completed.uncertain, dropped);
                assert_eq!(
                    completed.terminal.as_ref().unwrap().category,
                    if dropped {
                        crate::apps::action_evidence::TerminalCategory::Uncertain
                    } else {
                        crate::apps::action_evidence::TerminalCategory::Failed
                    }
                );
                f.reopen_journal();
                assert_eq!(f.assert_signed(&asked, true), completed);
                f.invoke(&asked).unwrap();
                assert_eq!(host.started.lock().unwrap().len(), 1);
            }
        }

        phase identity_admin_admission_and_completion_barriers_are_fail_closed;
        {
            for admission_failure in [false, true] {
                let host = Arc::new(Host::default());
                let f = super::signed_test_support::Fixture::new(crate::apps::services::ProcessServices {
                    providers: Some(providers(host.clone())),
                    ..Default::default()
                });
                let asked = request(device_tickets::REMOVE_IDENTITY, Some(Confirmation::Confirmed));
                if admission_failure {
                    f.fail_next_barrier();
                    assert!(f.invoke(&asked).is_err());
                    assert!(host.started.lock().unwrap().is_empty());
                    assert!(f.state.remote_job(&f.caller, &asked.invocation).is_none());
                } else {
                    let receipt = f.invoke(&asked).unwrap();
                    let job = f.state.remote_job(&f.caller, &receipt.invocation).unwrap();
                    f.fail_next_barrier();
                    host.answers
                        .lock()
                        .unwrap()
                        .pop()
                        .unwrap()
                        .complete(Ok(tilo_actions::ActionOutput::default()));
                    assert!(
                        matches!(f.state.invocation_status(job), Some(
                        tilo_actions::InvocationUpdate::Accepted { invocation }
                    ) if invocation == asked.invocation),
                        "failed durability must not publish success"
                    );
                }
            }
        }

        phase identity_admin_refuses_revoked_ownership_and_missing_signer_before_start;
        {
            for revoked in [false, true] {
                let host = Arc::new(Host::default());
                let f = super::signed_test_support::Fixture::new(crate::apps::services::ProcessServices {
                    providers: Some(providers(host.clone())),
                    ..Default::default()
                });
                if revoked {
                    f.authority
                        .revoked
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                } else {
                    std::fs::remove_file(
                        tilo_identity::provisioning::collaboration_identity_paths().device_privkey,
                    )
                    .unwrap();
                }
                let asked = request(device_tickets::REMOVE_IDENTITY, Some(Confirmation::Confirmed));
                assert!(f.invoke(&asked).is_err());
                assert!(host.started.lock().unwrap().is_empty());
                assert!(f.state.remote_job(&f.caller, &asked.invocation).is_none());
            }
        }

        phase identity_admin_is_offered_to_settings_as_its_person_and_removal_asks_first;
        {
            let host = Arc::new(Host::default());
            let (state, owner, workspace) = broker(Arc::clone(&host));
            let person = Caller::local_person(owner.clone()).in_workspace(workspace.clone());

            let rows = state
                .eager_catalogue(&person, RowScope::Provider(device_tickets::PROVIDER))
                .0;
            assert_eq!(
                rows.len(),
                device_tickets::IDENTITY_STEPS.len(),
                "every step is offered to the person here"
            );
            assert!(rows.iter().all(
                |row| row.app_id == "so.tilo.settings" && row.provider == device_tickets::PROVIDER
            ));

            // Settings reaches the steps as the person using it, without
            // declaring `[actions] invoke`; another app does not.
            let list = RemoteActionMessage::Invoke(request(device_tickets::IDENTITIES, None));
            assert_eq!(
                state
                    .app_api(&person, "so.tilo.settings", &list)
                    .map(|caller| caller.acting().clone())
                    .ok(),
                Some(person.acting().clone())
            );
            assert!(state.app_api(&person, "so.tilo.notes", &list).is_err());

            for step in [
                device_tickets::USE_IDENTITY,
                device_tickets::REMOVE_IDENTITY,
                device_tickets::PROTECT_IDENTITY,
            ] {
                assert_eq!(
                    invoked(&state, &person, &request(step, None)),
                    Err(ActionCallError::asks(
                        tilo_actions::remote::asked::AskedFacts {
                            on: Some("Identities".into()),
                            verb: step.into(),
                            destructive: true,
                            plural: Some("Identities".into()),
                            kind: device_tickets::RESOURCE.into(),
                            effect: Some(tilo_actions::ActionKind::Outside),
                            ..tilo_actions::remote::asked::AskedFacts::default()
                        }
                    )),
                    "{step} needs the person's confirmation, and says what it would do"
                );
            }
            assert!(host.started.lock().unwrap().is_empty(), "nothing started");

            // Listing asks nothing: the person sees their own identities.
            invoked(&state, &person, &request(device_tickets::IDENTITIES, None)).expect("listed");
            host.started.lock().unwrap().clear();
            host.answers.lock().unwrap().clear();

            let receipt = invoked(
                &state,
                &person,
                &request(device_tickets::REMOVE_IDENTITY, Some(Confirmation::Confirmed)),
            )
            .expect("admitted");
            assert_eq!(
                *host.started.lock().unwrap(),
                [(
                    workspace,
                    tilo_actions::MemberId::person(owner).into(),
                    device_tickets::REMOVE_IDENTITY.to_owned()
                )]
            );
            let job = state
                .remote_job(&person, &receipt.invocation)
                .expect("the invocation has a job");
            assert!(matches!(
                state.invocation_status(job),
                Some(tilo_actions::InvocationUpdate::Accepted { .. })
            ));
            let answer = host.answers.lock().unwrap().pop().expect("started");
            answer.complete(Ok(tilo_actions::ActionOutput {
                value: Some(serde_json::json!({"removed": "work"})),
                resources: Vec::new(),
            }));
            assert!(matches!(
                state.invocation_status(job),
                Some(tilo_actions::InvocationUpdate::Succeeded { .. })
            ));
        }

        phase identity_admin_is_refused_to_strangers_remote_devices_and_unconfined_callers;
        {
            let host = Arc::new(Host::default());
            let (state, owner, workspace) = broker(Arc::clone(&host));

            // A person the workspace's policy grants nothing is offered no step that
            // changes the identity, and the broker refuses one before the host
            // hears of it.
            let stranger = Caller::local_person(hex::encode([7; 32])).in_workspace(workspace.clone());
            let offered = state
                .eager_catalogue(&stranger, RowScope::Provider(device_tickets::PROVIDER))
                .0;
            assert!(
                offered.iter().all(|row| row
                    .action
                    .as_ref()
                    .is_some_and(
                        |descriptor| descriptor.contract.risk != tilo_actions::RiskLevel::Privileged
                    )),
                "{offered:?}"
            );
            for step in [
                device_tickets::USE_IDENTITY,
                device_tickets::PROTECT_IDENTITY,
                device_tickets::REMOVE_IDENTITY,
            ] {
                assert!(
                    invoked(
                        &state,
                        &stranger,
                        &request(step, Some(Confirmation::Confirmed))
                    )
                    .is_err(),
                    "{step} is refused to a stranger"
                );
            }

            // A remote device, or a caller confined to no workspace, is never offered
            // this machine's identity store.
            let remote = Caller::authenticated([41; 32], [42; 32]).in_workspace(workspace);
            assert!(
                state
                    .eager_catalogue(&remote, RowScope::Provider(device_tickets::PROVIDER))
                    .0
                    .is_empty()
            );
            assert!(invoked(&state, &remote, &request(device_tickets::IDENTITIES, None)).is_err());
            let unconfined = Caller::local_person(owner);
            assert!(invoked(&state, &unconfined, &request(device_tickets::IDENTITIES, None)).is_err());
            assert!(host.started.lock().unwrap().is_empty(), "nothing started");
        }
    }
}

#[cfg(test)]
mod settings_documents_tests {
    use super::super::AppBrokerState;
    use crate::apps::test_support::{
        Grantee, Grants as _, KindSet, Workspace, WorkspaceId, WorkspaceTree,
    };
    use crate::workspaces::providers::{Providers, Reach};
    use std::sync::{Arc, Mutex};
    use tilo_action_policy::edits::{FIELD_EDIT_TAG, Weaker, weakening_tag};
    use tilo_actions::remote::admitted::top_level;
    use tilo_actions::remote::{
        ActionCallError, ActionProvider, Admitted, Caller, Confirmation, InvokeArgument,
        InvokeRequest, Outcome, Pending, RemoteActionTarget, settings_resources,
    };

    const APP: &str = tilo_app_id::apps::SETTINGS;
    const RESOURCE: &str = "settings:power";

    /// The session's settings, in its place: one section whose lock delay
    /// weakens the lock when longer or never.
    struct Host {
        minutes: Mutex<u32>,
    }

    impl Host {
        fn descriptor(&self, privileged: bool) -> tilo_actions::ActionDescriptor {
            let now = self.minutes.lock().unwrap().to_string();
            tilo_actions::ActionDescriptor {
                did: None,
                state: tilo_actions::ActionState::default(),
                semantic_identity: None,
                address: tilo_actions::ActionAddress::new(RESOURCE, "field-edit"),
                resource_kind: "settings.power".into(),
                resource_label: "Power".into(),
                action_label: "Edit".into(),
                keywords: Vec::new(),
                icon: None,
                description: None,
                locus: tilo_actions::ExecutionLocus::default(),
                kind: tilo_actions::OperationKind::Document,
                contract: tilo_actions::ActionContract {
                    kind: tilo_actions::ActionKind::Undoable,
                    risk: if privileged {
                        tilo_actions::RiskLevel::Privileged
                    } else {
                        tilo_actions::RiskLevel::Low
                    },
                    tags: vec![
                        FIELD_EDIT_TAG.into(),
                        tilo_actions::ARGUMENT_RISK_TAG.into(),
                        weakening_tag(
                            "lock_minutes",
                            Some(&now),
                            Weaker::Higher { weakest: Some(0) },
                        ),
                    ],
                    ..tilo_actions::ActionContract::default()
                },
                input: tilo_actions::InputSpec::None,
                params: Vec::new(),
                view: None,
            }
        }
    }

    impl tilo_actions::remote::Admits for Host {
        const PROVIDER: &'static str = settings_resources::PROVIDER;
        const APP: &'static str = APP;

        fn rows(&self) -> Vec<tilo_actions::api::PlacedAction> {
            top_level(self.descriptors())
        }

        fn start(
            &self,
            _caller: &Caller,
            request: &InvokeRequest,
            _admitted: &Admitted,
        ) -> Result<Pending<Outcome>, String> {
            let started = request.argument.to_slot();
            let json = started
                .as_deref()
                .and_then(|text| text.strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX))
                .ok_or("no edit")?;
            let arguments: serde_json::Value =
                serde_json::from_str(json).map_err(|error| error.to_string())?;
            let minutes = arguments["edit"]["value"].as_u64().ok_or("no value")?;
            *self.minutes.lock().unwrap() = u32::try_from(minutes).map_err(|e| e.to_string())?;
            Ok(Pending::ready(Ok(tilo_actions::ActionOutput::default())))
        }

        fn described(
            &self,
            address: &tilo_actions::ActionAddress,
            argument: Option<&str>,
        ) -> Option<tilo_actions::ActionDescriptor> {
            let weakens = self
                .descriptor(false)
                .contract
                .confirmation_for_argument(argument)
                != tilo_actions::ConfirmationPolicy::None;
            Some(self.descriptor(weakens)).filter(|descriptor| &descriptor.address == address)
        }
    }

    impl Host {
        fn descriptors(&self) -> Vec<tilo_actions::ActionDescriptor> {
            vec![self.descriptor(false)]
        }
    }

    /// `host` registered on this device as the person's settings provider.
    fn providers(host: Arc<Host>) -> Providers {
        let providers = Providers::default();
        providers.provide(
            settings_resources::PROVIDER,
            APP,
            Reach::ThisDevice,
            host as Arc<dyn ActionProvider + Send + Sync>,
        );
        providers
    }

    fn lock_after(minutes: u32, confirmation: Option<Confirmation>) -> InvokeRequest {
        InvokeRequest {
            on_behalf_of: None,
            invocation: tilo_actions::InvocationId::new(),
            target: RemoteActionTarget {
                provider: settings_resources::PROVIDER.into(),
                address: tilo_actions::ActionAddress::new(RESOURCE, "field-edit"),
            },
            argument: InvokeArgument::Json(
                serde_json::from_value(serde_json::json!({
                    "edit": { "path": [{ "field": "lock_minutes" }], "op": "set", "value": minutes }
                }))
                .unwrap(),
            ),
            context: None,
            confirmation,
            revision: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        }
    }

    fn invoked(
        state: &Arc<AppBrokerState>,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<tilo_actions::remote::InvocationReceipt, ActionCallError> {
        state
            .action_invoke(caller, None, None, request)
            .wait_timeout(tilo_test_env::hang_bound())
            .ok()
            .flatten()
            .expect("admission answers at once")
    }

    /// With no window of Settings, the session changes the person's
    /// settings: a change that weakens the lock asks the person first, and
    /// an agent's needs their own answer; a stricter one asks nothing.
    #[test]
    fn a_setting_that_weakens_asks_the_person_and_an_agent_needs_their_answer() {
        let host = Arc::new(Host {
            minutes: Mutex::new(5),
        });
        let owner = hex::encode([41; 32]);
        let mut workspace = Workspace::new(WorkspaceId::from_u128(9), "Settings");
        workspace.policy.set_owner(owner.clone());
        workspace.allow_agent("scribe", KindSet::READ);
        workspace.allow_app(Grantee::agent("scribe"), APP, KindSet::ALL);
        let workspace_id = workspace.id().to_string();
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let (state, _bridge_rx) = AppBrokerState::test_state(
            Some(tree.shared().authority()),
            crate::apps::services::ProcessServices {
                providers: Some(providers(host.clone())),
                ..Default::default()
            },
        );
        let person = Caller::local_person(owner.clone()).in_workspace(workspace_id.clone());
        let agent = Caller::local_person(owner)
            .delegated_to("scribe")
            .in_workspace(workspace_id);
        let minutes = || *host.minutes.lock().unwrap();

        invoked(&state, &person, &lock_after(1, None)).expect("locking sooner asks nothing");
        assert_eq!(minutes(), 1);
        assert_eq!(
            invoked(&state, &person, &lock_after(0, None)),
            Err(ActionCallError::asks(
                tilo_actions::remote::asked::AskedFacts {
                    on: Some("Power".into()),
                    verb: "Edit".into(),
                    plural: Some("Powers".into()),
                    kind: "settings.power".into(),
                    effect: Some(tilo_actions::ActionKind::Undoable),
                    ..tilo_actions::remote::asked::AskedFacts::default()
                }
            )),
            "never locking asks first, saying what it would change"
        );
        assert_eq!(minutes(), 1);
        invoked(
            &state,
            &person,
            &lock_after(10, Some(Confirmation::Confirmed)),
        )
        .expect("the person confirmed it");
        assert_eq!(minutes(), 10);

        invoked(&state, &agent, &lock_after(5, None)).expect("the agent locks sooner");
        assert_eq!(minutes(), 5);
        for confirmation in [None, Some(Confirmation::Confirmed)] {
            assert!(
                invoked(&state, &agent, &lock_after(0, confirmation)).is_err(),
                "an agent's own confirmation is no answer of the person's"
            );
        }
        assert_eq!(minutes(), 5, "nothing weakened");
    }
}
