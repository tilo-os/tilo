//! The providers registered on this device, as its dispatch runs a call of one made here: the
//! steps the host runs itself (pairing a nearby device, administering the person's identities and
//! the devices trusted under them, changing their settings) and every provider the device's
//! embedder registered ([`tilo_workspace_service::workspaces::providers`]).
//!
//! Each is admitted against the workspace's policy under the application its registration names,
//! asks the person first where its contract says so, and is journalled as the person's own call
//! before the provider carries it out ([`ActionProvider::run`]). A provider that reaches this
//! device alone is offered only to a caller on this machine confined to a workspace.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::AppBrokerState;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Admitted, Caller, InvocationReceipt, InvokeRequest, Outcome,
    Pending, PendingPoll, ReceiptStatus, Reservation, Serving,
};
use tilo_workspace_service::workspaces::providers::Registered;

/// How a call of a registered provider stands once admitted, before it is taken.
enum RegisteredAdmission {
    /// The same call was taken before: its receipt.
    Taken(InvocationReceipt),
    /// A call to take.
    Call(Box<RegisteredCall>),
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
    /// Whose authority decides it, and in which workspace.
    decided: Decided,
}

/// What a call of a registered provider is decided as: the caller, the workspace whose policy
/// decides it, and the original admission it is decided against.
#[derive(Clone)]
struct Decided {
    caller: Caller,
    workspace: String,
    origin: Option<super::published_invocation::InvocationCaller>,
}

/// The workspace a call of `descriptor` on `registered` is decided in, when it is one of the
/// person's device settings: their own workspace keeps them, and their shell never shows it.
/// `None` for every other call, which is decided where the caller is confined.
pub(super) fn device_settings_workspace(
    caller: &Caller,
    registered: &Registered,
    descriptor: &tilo_actions::ActionDescriptor,
) -> Option<String> {
    if !caller.acting().is_person() || caller.is_foreign_device() {
        return None;
    }
    let node = tilo_actions::resource_node(
        &tilo_action_policy::TreePath::root(),
        &registered.app,
        &descriptor.resource_kind,
        descriptor.address.resource.as_str(),
    );
    if !tilo_action_policy::roles::is_device_settings(&node) {
        return None;
    }
    let own = caller
        .acting()
        .person_id()
        .and_then(tilo_action_policy::roles::workspace_of_person)?;
    (caller.workspace() != Some(own.as_str())).then_some(own)
}

/// [`Decided`] for the call of `descriptor` on `registered`: one of the person's device
/// settings is the person's own in their own workspace, whichever workspace stands in front;
/// every other call is decided where the caller is confined.
fn decided(
    caller: &Caller,
    origin: Option<&super::published_invocation::InvocationCaller>,
    registered: &Registered,
    descriptor: &tilo_actions::ActionDescriptor,
    invocation: tilo_actions::InvocationId,
) -> Decided {
    let confined = || Decided {
        caller: caller.clone(),
        workspace: caller.workspace().unwrap_or_default().to_owned(),
        origin: origin.cloned(),
    };
    let Some(own) = device_settings_workspace(caller, registered, descriptor) else {
        return confined();
    };
    // The certificate the caller was admitted with is the person's device's, and the device is
    // current for their own workspace too: what the person's own workspace decides is theirs.
    let kept = origin.and_then(|origin| match origin {
        super::published_invocation::InvocationCaller::Certified {
            caller: original, ..
        } => Some((
            original.admitted.certificate.clone()?,
            original.policy_time.clone()?,
        )),
        super::published_invocation::InvocationCaller::NativePerson {
            certificate, time, ..
        } => Some(((**certificate).clone(), time.clone())),
        super::published_invocation::InvocationCaller::Local { admitted, time, .. } => {
            Some((admitted.certificate.clone()?, time.clone()?))
        }
        // A shortcut's approval is no person's device: their device settings are not its.
        super::published_invocation::InvocationCaller::Shortcut(_) => None,
    });
    let Some((certificate, time)) = kept else {
        return confined();
    };
    Decided {
        caller: caller.clone().in_workspace(own.clone()),
        origin: Some(
            super::published_invocation::InvocationCaller::NativePerson {
                invocation,
                workspace: own.clone(),
                certificate: Box::new(certificate),
                time,
            },
        ),
        workspace: own,
    }
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
    // A shortcut's run this host started on its saver's approval, in the workspace it was
    // approved for, by a saver whose own devices are not foreign here.
    if let Some(super::published_invocation::InvocationCaller::Shortcut(approval)) = origin {
        return !approval.certificate().capabilities.foreign
            && caller.workspace() == Some(approval.workspace());
    }
    // An agent of the person whose own session the socket admitted acts under its own key: its
    // run is that session's, made for it once the workspace listed the agent, so it is local as
    // the person is, from a device of theirs.
    if let Some(super::published_invocation::InvocationCaller::Local { admitted, .. }) = origin
        && caller.device_public_key().is_some()
        && caller.acting().is_agent()
    {
        return admitted.certificate.as_ref().is_some_and(|cert| {
            !cert.capabilities.foreign && caller.user_public_key() == Some(&cert.user_public_key)
        });
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
            Some(
                super::published_invocation::InvocationCaller::Certified { .. }
                | super::published_invocation::InvocationCaller::Shortcut(_),
            ) => false,
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

impl AppBrokerState {
    /// What the workspace decides of `request`, when it names an action of the caller's own
    /// device that the registered provider `registered` answers: the same verdict the workspace's
    /// admission makes of it, with their own connection answering what the workspace asks of
    /// them, and nothing of it run or approved. `Ok` where it runs unasked, the ask refusal where
    /// the person is asked first, and the refusal where the workspace denies it. `None` for an
    /// action that is no own-device one, which the admission decides as any other.
    pub fn own_device_verdict(
        &self,
        caller: &Caller,
        workspace: &str,
        registered: &Registered,
        request: &InvokeRequest,
    ) -> Option<Result<(), ActionCallError>> {
        let argument = request.argument.to_slot();
        let descriptor = self.services.providers.as_ref()?.descriptor(
            registered,
            caller,
            &request.target.address,
            argument.as_deref(),
        )?;
        if !descriptor.contract.is_own_device() {
            return None;
        }
        Some((|| {
            let (workspace, policy, now) =
                self.current_framework_authority(caller, None, workspace)?;
            // The call's own confirmation is never read here: an ask is answered only by the
            // person's approve of the asked record the admission keeps, which this call is not.
            // An action whose contract asks its person every time asks here too, before anything
            // runs.
            if !Self::confirmed_as_asked(&policy, &descriptor, argument.as_deref(), None, false) {
                return Err(ActionCallError::confirmation_required());
            }
            self.admit_answer(
                (&workspace, &policy, now),
                deciding_app(registered, None),
                &descriptor,
                (argument.as_deref(), None),
            )
        })())
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
            Ok(RegisteredAdmission::Call(admitted)) => *admitted,
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
                self.take_registered(caller, serving, registered, request, admitted),
            );
        };
        let (state, caller, registered, request) = (
            Arc::clone(self),
            caller.clone(),
            registered.clone(),
            request.clone(),
        );
        asking.map(move |asked| match asked {
            Ok(()) => state.take_registered(&caller, serving, &registered, &request, admitted),
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
                provider: Some(registered.provider.to_string()),
                target: Some(request.target.address.clone()),
                max_results: tilo_actions::remote::MAX_CATALOGUE_PAGE,
                ..tilo_actions::remote::CatalogueQuery::default()
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
        let Some(confined) = caller.workspace() else {
            return Err(abandon(ActionCallError::NotPermitted));
        };
        let argument = request.argument.to_slot();
        let Some(descriptor) = described.or_else(|| {
            self.services.providers.as_ref()?.descriptor(
                registered,
                caller,
                &request.target.address,
                argument.as_deref(),
            )
        }) else {
            // Its listing has not answered yet: asked again once it does, nothing reserved.
            if let Ok(mut log) = self.remote_invocations.lock() {
                log.abandon(&request.invocation);
            }
            return Ok(RegisteredAdmission::Undescribed);
        };
        let app_id = deciding_app(registered, serving);
        let decided = decided(caller, origin, registered, &descriptor, request.invocation);
        let (workspace, policy, now) = self
            .current_framework_authority(
                &decided.caller,
                decided.origin.as_ref(),
                &decided.workspace,
            )
            .map_err(abandon)?;
        // One answer covers both checks of this exact invocation.
        let approved = super::approval::approved_by_person(caller, request.confirmation.as_ref());
        // An agent takes a step as any of its steps: on the workspace's grants, in an agent
        // session that still goes. A step of the session's own, or of an agent session's, is no
        // agent's outside one.
        let session = self.agent_session_of(Some(caller), confined);
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
        Ok(RegisteredAdmission::Call(Box::new(RegisteredCall {
            descriptor,
            approved,
            through,
            decided,
        })))
    }

    /// Take one call [`Self::admit_registered`] admitted, and hand it to its provider.
    #[allow(
        clippy::too_many_lines,
        reason = "one call's admission, effect and answer in a single auditable sequence"
    )]
    fn take_registered(
        self: &Arc<Self>,
        caller: &Caller,
        serving: Option<Arc<dyn Serving>>,
        registered: &Registered,
        request: &InvokeRequest,
        call: RegisteredCall,
    ) -> Result<InvocationReceipt, ActionCallError> {
        let RegisteredCall {
            descriptor,
            approved,
            through,
            decided,
        } = call;
        let abandon = |error: ActionCallError| {
            if let Ok(mut log) = self.remote_invocations.lock() {
                log.abandon(&request.invocation);
            }
            error
        };
        if caller.workspace().is_none() {
            return Err(abandon(ActionCallError::NotPermitted));
        }
        let argument = request.argument.to_slot();
        let app_id = deciding_app(registered, serving.as_ref());
        let admitted = self
            .admit_host_action(
                &decided.caller,
                decided.origin.as_ref(),
                super::host_action_admission::HostActionAdmission {
                    workspace: &decided.workspace,
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
            let (workspace, policy, now) = self.current_framework_authority(
                &decided.caller,
                decided.origin.as_ref(),
                &decided.workspace,
            )?;
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
                caller: decided.caller.clone(),
                origin: decided.origin.clone(),
                workspace: decided.workspace,
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
    use crate::collab_app_broker::tests::presentation;
    use crate::services::ProcessServices;
    use crate::test_support::{Workspace, WorkspaceTree};
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };
    use tilo_actions::remote::{ActionCallError, Caller, InvocationReceipt, InvokeRequest};

    struct NativeOwnerReceipts {
        owner: Arc<tilo_peer_state::owner::DeviceWorkspaces>,
        workspace: String,
    }

    impl tilo_broker_core::workspace_commits::WorkspaceCommits for NativeOwnerReceipts {
        fn projects_workspace(&self, _: tilo_desktop_model::WorkspaceId) -> bool {
            true
        }

        fn health(&self) -> Result<(), String> {
            let workspace = tilo_desktop_model::WorkspaceId::parse(&self.workspace).unwrap();
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
                    let receipt: tilo_actions::receipt::WorkspaceEffectReceipt =
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
        time: Mutex<crate::policy_clock::PolicyTime>,
        replace_time_on_authorize: Mutex<Option<crate::policy_clock::PolicyTime>>,
    }

    impl Authority {
        pub fn replace_time_on_authorize(&self, time: crate::policy_clock::PolicyTime) {
            *self.replace_time_on_authorize.lock().unwrap() = Some(time);
        }
    }

    impl crate::certificate_authority::CertificateAuthority for Authority {
        fn policy_time(&self) -> Result<Option<crate::policy_clock::PolicyTime>, String> {
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
        pub tree: crate::test_support::WorkspaceTreeHandle,
        certified_host: Option<Arc<dyn tilo_actions::remote::ActionProvider>>,
        _fanout: Arc<FanoutState>,
        /// The device's key and environment, kept for as long as this lives.
        home: crate::test_support::ProvisionedHome,
        clock: crate::policy_clock::OwnerPolicyClock,
        policy_origin: crate::policy_clock::PolicyOrigin,
    }

    impl Fixture {
        pub fn new_policy_origin(&self) -> crate::policy_clock::PolicyOrigin {
            self.clock.new_origin()
        }

        pub fn retire_policy_origin(&self) {
            self.policy_origin.invalidate();
        }

        /// The fixture in the process's shared test workspace, or, when `fresh`, in a
        /// workspace of its own that no other fixture in the process opened.
        pub fn new(services: ProcessServices, fresh: bool) -> Self {
            let home = crate::test_support::ProvisionedHome::new();
            let (key, certificate) =
                tilo_identity::trusted_device::current(tilo_peer_state::owner::testing::WORKSPACE)
                    .unwrap();
            let owner = Arc::new(
                tilo_peer_state::owner::open_file(&home.root().join("native.json")).unwrap(),
            );
            // A fixture in a workspace of its own founds one: a workspace no other fixture in
            // this process opened, which its host therefore signs nothing for.
            let named = if fresh {
                tilo_desktop_model::WorkspaceId::new().to_string()
            } else {
                tilo_peer_state::owner::testing::WORKSPACE.to_owned()
            };
            tilo_peer_state::owner::testing::add_interactive_device_in(
                &owner,
                &key,
                &certificate,
                &named,
            )
            .unwrap();
            let mut workspace = Workspace::new(
                tilo_desktop_model::WorkspaceId::parse(&named).unwrap(),
                "Host actions",
            );
            let workspace_id = workspace.id().to_string();
            let publisher = PeerPublisher::with_commits(Arc::new(NativeOwnerReceipts {
                owner,
                workspace: workspace_id.clone(),
            }));
            let person = person_id_for_root(certificate.user_public_key);
            workspace.policy.set_owner(person.clone());
            let mut tree = WorkspaceTree::default();
            tree.workspaces.push(workspace);
            let tree = tree.shared();
            let clock = crate::policy_clock::OwnerPolicyClock::new().unwrap();
            let policy_origin = clock.new_origin();
            let authority = Arc::new(Authority {
                certificate: certificate.clone(),
                revoked: AtomicBool::new(false),
                time: Mutex::new(policy_origin.capture().unwrap()),
                replace_time_on_authorize: Mutex::new(None),
            });
            let config = crate::NativePeerBrokerConfig {
                journal_root: home.root().join("journal"),
                certificate_authority: authority.clone(),
            };
            let fanout = Arc::new(FanoutState::new());
            let (state, _) = AppBrokerState::new_configured(
                &fanout,
                Arc::new(Mutex::new(BrokerChannels::default())),
                publisher,
                // A fresh fixture's host has opened nothing: its tree is handed as it is, never
                // through the authority that serves (and so opens) a fixture's workspaces.
                Some(if fresh {
                    Arc::new(tree.clone()) as crate::services::WorkspaceAuthorityHandle
                } else {
                    tree.authority()
                }),
                None,
                Some(&config),
                services,
            );
            let verified = state
                .verify_action_caller(
                    &workspace_id,
                    &presentation(&workspace_id, &certificate, &key),
                )
                .unwrap();
            let origin = InvocationCaller::Local {
                route: super::super::CrdtRoute::cluster(&workspace_id),
                admitted: Box::new(verified.admitted),
                time: state.registered_policy_time().unwrap(),
                pressed: false,
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
                home,
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

        /// The certified fixture, its caller foreign when `foreign`: what
        /// [`Self::new_certified_foreign`] and [`Self::new_certified`] make for a test that
        /// iterates both.
        pub(crate) fn certified(services: ProcessServices, foreign: bool) -> Self {
            let mut fixture = Self::new(services, false);
            let (key, certificate) =
                tilo_identity::trusted_device::current(&fixture.workspace).unwrap();
            // The evidence this fixture's broker keeps is signed as the device it acts as.
            tilo_invocation_journal::serve_as(certificate.clone(), Arc::new(key.clone()));
            tilo_invocation_journal::opened(&fixture.workspace);
            let presentation = presentation(&fixture.workspace, &certificate, &key);
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
                InvocationCaller::NativePerson { .. } | InvocationCaller::Shortcut(_) => {
                    panic!("authenticated ownership")
                }
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
                .fail_at = Some(tilo_invocation_journal::BarrierFailure::File);
        }

        pub fn reopen_journal(&self) {
            let mut guard = self.state.journal.lock().unwrap();
            drop(guard.as_mut().unwrap().take());
            *guard = Ok(Some(
                super::super::InvocationJournal::open(&self.home.root().join("journal")).unwrap(),
            ));
        }

        pub fn assert_signed(&self, request: &InvokeRequest, terminal: bool) -> DurableInvocation {
            assert!(
                tilo_invocation_journal::serves_a_signer(),
                "no host names what signs this process's receipts"
            );
            let record = self.record(request);
            let admission = record.admission.as_ref().expect("signed admission");
            let receipt = admission.public_receipt();
            tilo_invocation_journal::action_evidence::verify(receipt, &receipt.facts).unwrap();
            assert_eq!(receipt.facts.invocation, request.invocation);
            assert_eq!(receipt.facts.workspace, self.workspace);
            assert_eq!(receipt.facts.resource, request.target.address.resource);
            assert_eq!(receipt.facts.action, request.target.address.action);
            assert!(record.admitted_request_is(&serde_json::to_value(request).unwrap()));
            if terminal {
                tilo_invocation_journal::action_evidence::verify_terminal(
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
    use crate::test_support::{Grants as _, Workspace, WorkspaceId, WorkspaceTree};
    use std::sync::{Arc, Mutex};
    use tilo_actions::remote::admitted::top_level;
    use tilo_actions::remote::{
        ActionCallError, ActionProvider, Admitted, Caller, Completer, Confirmation, InvokeArgument,
        InvokeRequest, Outcome, Pending, RemoteActionMessage, device_tickets, pending,
    };
    use tilo_workspace_service::workspaces::providers::{Providers, Reach};

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
            top_level(Host::descriptors())
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
        fn descriptors() -> Vec<tilo_actions::ActionDescriptor> {
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
            crate::services::ProcessServices {
                providers: Some(providers(host)),
                ..Default::default()
            },
        );
        (state, owner, workspace_id)
    }

    /// A native host's device, which never certifies anyone here.
    struct NoDevice;

    impl crate::services::LocalPersonServices for NoDevice {
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
            workspace.allow_agent("planner", crate::test_support::KindSet::READ);
            let workspace_id = workspace.id().to_string();
            let mut tree = WorkspaceTree::default();
            tree.workspaces.push(workspace);
            let (state, _bridge_rx) = AppBrokerState::test_state(
                Some(tree.shared().authority()),
                crate::services::ProcessServices {
                    providers: Some(providers(host.clone())),
                    local_person: Some(Arc::new(NoDevice)),
                    ..Default::default()
                },
            );
            let agent = Caller::local_agent("planner", owner.clone()).in_workspace(workspace_id.clone());
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
                let services = crate::services::ProcessServices {
                    providers: Some(providers(service.clone())),
                    ..Default::default()
                };
                let f = super::signed_test_support::Fixture::certified(services, foreign);
                let mut asked = request(device_tickets::REMOVE_IDENTITY, Some(crate::collab_app_broker::person_approved("workspace", tilo_actions::InvocationId::new())));
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
                    let services = crate::services::ProcessServices {
                        providers: Some(providers(host.clone())),
                        ..Default::default()
                    };
                    let mut f = if certified {
                        super::signed_test_support::Fixture::new_certified(services)
                    } else {
                        super::signed_test_support::Fixture::new(services, false)
                    };
                    f.caller = f.caller.clone().in_new_execution();
                    let asked = request(device_tickets::REMOVE_IDENTITY, Some(crate::collab_app_broker::person_approved("workspace", tilo_actions::InvocationId::new())));
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
                            let registered = tilo_workspace_service::workspaces::providers::Registered {
                                provider: device_tickets::PROVIDER.into(),
                                app: "so.tilo.settings".into(),
                                reach: Reach::ThisDevice,
                                answers: steps as Arc<dyn ActionProvider + Send + Sync>,
                                published: Vec::new(),
                                placed_for: None,
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
            let f = super::signed_test_support::Fixture::new(crate::services::ProcessServices {
                providers: Some(providers(host.clone())),
                ..Default::default()
            }, false);
            let asked = request(device_tickets::REMOVE_IDENTITY, Some(crate::collab_app_broker::person_approved("workspace", tilo_actions::InvocationId::new())));
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
                let f = super::signed_test_support::Fixture::new(crate::services::ProcessServices {
                    providers: Some(providers(host.clone())),
                    ..Default::default()
                }, false);
                let asked = request(device_tickets::REMOVE_IDENTITY, Some(crate::collab_app_broker::person_approved("workspace", tilo_actions::InvocationId::new())));
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
                        tilo_invocation_journal::action_evidence::TerminalCategory::Uncertain
                    } else {
                        tilo_invocation_journal::action_evidence::TerminalCategory::Failed
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
                let f = super::signed_test_support::Fixture::new(crate::services::ProcessServices {
                    providers: Some(providers(host.clone())),
                    ..Default::default()
                }, false);
                let asked = request(device_tickets::REMOVE_IDENTITY, Some(crate::collab_app_broker::person_approved("workspace", tilo_actions::InvocationId::new())));
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
                // A workspace of this fixture's own: no host opened it, so it signs nothing.
                let f = super::signed_test_support::Fixture::new(
                    crate::services::ProcessServices {
                        providers: Some(providers(host.clone())),
                        ..Default::default()
                    },
                    true,
                );
                if revoked {
                    f.authority
                        .revoked
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                }
                let asked = request(device_tickets::REMOVE_IDENTITY, Some(crate::collab_app_broker::person_approved("workspace", tilo_actions::InvocationId::new())));
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
                &request(device_tickets::REMOVE_IDENTITY, Some(crate::collab_app_broker::person_approved("workspace", tilo_actions::InvocationId::new()))),
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
                        &request(step, Some(crate::collab_app_broker::person_approved("workspace", tilo_actions::InvocationId::new())))
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
    use crate::test_support::{
        Grantee, Grants as _, KindSet, Workspace, WorkspaceId, WorkspaceTree,
    };
    use std::sync::{Arc, Mutex};
    use tilo_action_policy::edits::{FIELD_EDIT_TAG, Weaker, weakening_tag};
    use tilo_actions::remote::admitted::top_level;
    use tilo_actions::remote::{
        ActionCallError, ActionProvider, Admitted, Caller, CatalogueQuery, Confirmation,
        InvokeArgument, InvokeRequest, Outcome, Pending, RemoteActionTarget, settings_resources,
    };
    use tilo_workspace_service::workspaces::providers::{Providers, Reach};

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

    /// The settings provider as a device's section arrives from it.
    fn device_providers(host: Arc<DeviceHost>) -> Providers {
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
        workspace.allow_app(Grantee::agent("scribe", owner.clone()), APP, KindSet::ALL);
        let workspace_id = workspace.id().to_string();
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let (state, _bridge_rx) = AppBrokerState::test_state(
            Some(tree.shared().authority()),
            crate::services::ProcessServices {
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
            &lock_after(
                10,
                Some(crate::collab_app_broker::person_approved(
                    "workspace",
                    tilo_actions::InvocationId::new(),
                )),
            ),
        )
        .expect("the person confirmed it");
        assert_eq!(minutes(), 10);

        invoked(&state, &agent, &lock_after(5, None)).expect("the agent locks sooner");
        assert_eq!(minutes(), 5);
        for confirmation in [None, Some(Confirmation::Operated)] {
            assert!(
                invoked(&state, &agent, &lock_after(0, confirmation)).is_err(),
                "an agent's own confirmation is no answer of the person's"
            );
        }
        assert_eq!(minutes(), 5, "nothing weakened");
    }

    /// A device's setting, as the service publishes it: its own section address.
    const DEVICE: &str = "abababababababababababababababababababababababababababababababab";

    fn device_section() -> String {
        format!("devices/{DEVICE}/settings/power")
    }

    /// The settings provider as a device's section arrives from it: one setter.
    struct DeviceHost;

    impl tilo_actions::remote::Admits for DeviceHost {
        const PROVIDER: &'static str = settings_resources::PROVIDER;
        const APP: &'static str = APP;

        fn rows(&self) -> Vec<tilo_actions::api::PlacedAction> {
            top_level(vec![tilo_actions::ActionDescriptor {
                did: None,
                state: tilo_actions::ActionState::default(),
                semantic_identity: None,
                address: tilo_actions::ActionAddress::new(device_section(), "set-lock-minutes"),
                resource_kind: "settings.power".into(),
                resource_label: "Power".into(),
                action_label: "Lock sooner".into(),
                keywords: Vec::new(),
                icon: None,
                description: None,
                locus: tilo_actions::ExecutionLocus::default(),
                kind: tilo_actions::OperationKind::Document,
                contract: tilo_actions::ActionContract {
                    kind: tilo_actions::ActionKind::Undoable,
                    ..tilo_actions::ActionContract::default()
                },
                input: tilo_actions::InputSpec::None,
                params: Vec::new(),
                view: None,
            }])
        }

        fn start(
            &self,
            _caller: &Caller,
            _request: &InvokeRequest,
            _admitted: &Admitted,
        ) -> Result<Pending<Outcome>, String> {
            Ok(Pending::ready(Ok(tilo_actions::ActionOutput::default())))
        }
    }

    /// A workspace with an owner, and `person` a member: where a person's shell stands.
    fn studio(person: &str) -> Workspace {
        let mut workspace = Workspace::new(WorkspaceId::from_u128(9), "Studio");
        workspace.policy.set_owner(hex::encode([42; 32]));
        workspace
            .policy
            .add_member(person, tilo_action_policy::Role::Editor);
        workspace
    }

    /// The person's own workspace, which keeps their device's sections.
    fn theirs(person: &str) -> Workspace {
        let own = tilo_action_policy::roles::workspace_of_person(person)
            .expect("a person's own workspace");
        let mut workspace =
            Workspace::new(WorkspaceId::parse(&own).expect("a workspace id"), "Mine");
        workspace.policy.set_owner(person.to_owned());
        workspace
    }

    /// A device's setting is offered in its person's own workspace, never by the workspace in
    /// front: the same row is listed once the workspace that keeps it is held here.
    #[test]
    fn a_devices_settings_are_listed_where_their_person_keeps_them() {
        let person = hex::encode([41; 32]);
        let query = CatalogueQuery {
            provider: Some(settings_resources::PROVIDER.into()),
            max_results: 1024,
            ..CatalogueQuery::default()
        };
        let offered = |workspaces: Vec<Workspace>| {
            let tree = WorkspaceTree {
                desktop: tilo_desktop_model::Layout {
                    workspaces,
                    ..tilo_desktop_model::Layout::default()
                },
                ..WorkspaceTree::default()
            };
            let (state, _bridge_rx) = AppBrokerState::test_state(
                Some(tree.shared().authority()),
                crate::services::ProcessServices {
                    providers: Some(device_providers(Arc::new(DeviceHost))),
                    ..Default::default()
                },
            );
            let caller =
                Caller::local_person(person.clone()).in_workspace(studio(&person).id().to_string());
            let page = state
                .action_query(&caller, &query)
                .wait_timeout(tilo_test_env::hang_bound())
                .ok()
                .flatten()
                .expect("the catalogue answers")
                .expect("the catalogue lists the settings");
            page.rows
                .iter()
                .filter_map(|row| row.action.as_ref())
                .any(|action| action.address.resource == device_section())
        };
        assert!(
            !offered(vec![studio(&person)]),
            "the workspace in front cannot offer a device's settings"
        );
        assert!(
            offered(vec![studio(&person), theirs(&person)]),
            "the person's own workspace offers their device's settings"
        );
    }
}

#[cfg(test)]
mod own_device_verdict_tests {
    use super::super::AppBrokerState;
    use crate::test_support::{
        Answer, Grantee, KindSet, Role, Workspace, WorkspaceId, WorkspaceTree,
    };
    use std::sync::Arc;
    use tilo_actions::remote::{
        ActionCallError, Admits, Admitted, Caller, InvokeArgument, InvokeRequest, Outcome, Pending,
        RemoteActionTarget, admitted::top_level,
    };
    use tilo_workspace_service::workspaces::providers::{Providers, Reach, Registered};

    /// The provider the device's package cache answers as.
    const PROVIDER: &str = "so.tilo.packages";
    /// A member of the workspace, on a connection that is their own.
    const MEMBER: &str = "member-person";

    /// The resource `install-dev` is published on for a device.
    fn resource() -> String {
        format!("devices/{}/packages", hex::encode([7u8; 32]))
    }

    fn descriptor() -> tilo_actions::ActionDescriptor {
        let mut contract = tilo_actions::ActionContract {
            kind: tilo_actions::ActionKind::Outside,
            ..tilo_actions::ActionContract::default()
        };
        contract
            .tags
            .push(tilo_actions::OWN_DEVICE_SCOPE.to_owned());
        tilo_actions::ActionDescriptor {
            did: None,
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            address: tilo_actions::ActionAddress::new(resource(), "install-dev"),
            resource_kind: "devices.packages".into(),
            resource_label: "Packages".into(),
            action_label: "install-dev".into(),
            keywords: Vec::new(),
            icon: None,
            description: None,
            locus: tilo_actions::ExecutionLocus::default(),
            kind: tilo_actions::OperationKind::Effect,
            contract,
            input: tilo_actions::InputSpec::None,
            params: Vec::new(),
            view: None,
        }
    }

    /// A device action that nothing runs in these tests: only its verdict is asked.
    struct Cache;

    impl Admits for Cache {
        const PROVIDER: &'static str = PROVIDER;
        const APP: &'static str = PROVIDER;

        fn rows(&self) -> Vec<tilo_actions::api::PlacedAction> {
            top_level(vec![descriptor()])
        }

        fn described(
            &self,
            address: &tilo_actions::ActionAddress,
            _argument: Option<&str>,
        ) -> Option<tilo_actions::ActionDescriptor> {
            (address == &descriptor().address).then(descriptor)
        }

        fn start(
            &self,
            _caller: &Caller,
            _request: &InvokeRequest,
            _admitted: &Admitted,
        ) -> Result<Pending<Outcome>, String> {
            Err("nothing runs in this test".into())
        }
    }

    fn registered() -> Registered {
        Registered {
            provider: Arc::from(PROVIDER),
            app: Arc::from(PROVIDER),
            reach: Reach::ThisDevice,
            answers: Arc::new(Cache),
            published: Vec::new(),
            placed_for: None,
        }
    }

    /// A workspace `owner` owns, `MEMBER` belongs to, where `rule` answers for the member at the
    /// device action.
    fn broker_workspace(
        rule: Option<Answer>,
    ) -> (Arc<AppBrokerState>, WorkspaceId, String, String) {
        let owner = hex::encode([61u8; 32]);
        let mut workspace = Workspace::new(WorkspaceId::from_u128(61), "Packages");
        workspace.policy.set_owner(owner.clone());
        workspace.policy.add_member(MEMBER, Role::Editor);
        let workspace_id = workspace.id().to_string();
        // A path below the workspace, as the fixture's grants take it.
        let node = tilo_actions::action_node(
            &crate::test_support::TreePath::root(),
            PROVIDER,
            "devices.packages",
            &resource(),
            None,
            "install-dev",
        );
        if let Some(rule) = rule {
            use crate::test_support::Grants as _;
            workspace.answer(Grantee::person(MEMBER), &node, KindSet::ALL, rule);
        }
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let providers = Providers::default();
        let cache: Arc<dyn tilo_actions::remote::ActionProvider + Send + Sync> = Arc::new(Cache);
        providers.provide(PROVIDER, PROVIDER, Reach::ThisDevice, cache);
        let (state, _bridge_rx) = AppBrokerState::test_state(
            Some(tree.shared().authority()),
            crate::services::ProcessServices {
                providers: Some(providers),
                ..Default::default()
            },
        );
        (state, WorkspaceId::from_u128(61), workspace_id, owner)
    }

    fn request() -> InvokeRequest {
        InvokeRequest {
            on_behalf_of: None,
            invocation: tilo_actions::InvocationId::new(),
            target: RemoteActionTarget {
                provider: PROVIDER.to_owned(),
                address: tilo_actions::ActionAddress::new(resource(), "install-dev"),
            },
            argument: InvokeArgument::Json(serde_json::Map::new()),
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        }
    }

    /// The person on their own device's connection asks the device to install a development
    /// build: the workspace's rules decide it as they decide it at admission, and a rule set for
    /// the person is consulted.
    #[test]
    fn an_own_device_action_is_asked_or_denied_by_the_workspace_rules() {
        // No rule for the workspace's owner: it runs.
        let (state, id, workspace, owner) = broker_workspace(None);
        let caller = Caller::own_person(owner).in_workspace(&workspace);
        assert!(
            state
                .own_device_verdict(&caller, &workspace, &registered(), &request())
                .expect("an own-device action")
                .is_ok(),
            "with no rule set, an own-device action of the person's runs"
        );

        // The member, on a connection of their own: their rule decides.
        for (rule, expected) in [
            (Answer::Ask, "confirmation_required"),
            (Answer::Deny, "not_permitted"),
        ] {
            let (state, same, workspace, _) = broker_workspace(Some(rule));
            assert_eq!(same, id, "the same workspace");
            let caller = Caller::own_person(MEMBER).in_workspace(&workspace);
            let refused = state
                .own_device_verdict(&caller, &workspace, &registered(), &request())
                .expect("an own-device action")
                .expect_err("a rule the person set decides their action");
            let answered = if matches!(&refused, ActionCallError::ConfirmationRequired(_)) {
                "confirmation_required"
            } else if matches!(&refused, ActionCallError::NotPermitted) {
                "not_permitted"
            } else {
                panic!("{rule:?} answered {refused:?}, expected {expected}")
            };
            assert_eq!(answered, expected, "{rule:?} answered {refused:?}");
        }
    }
}
