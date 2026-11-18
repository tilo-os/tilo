//! A turn's steps: each one action the agent asks the host to run on its own connection, which the
//! host serves as the turn's, followed by asks the host holds until the step moves on.
use super::{
    Asked, CollabEvent, CollabRequest, Duration, Following, Instant, Result, Session, StepOutcome,
    TaskAction, TaskActionStop, Uuid,
};
use tilo_actions::remote::{
    ActionCallError, ConfirmedInvocation, InvocationReceipt, InvokeArgument, InvokeRequest,
    ReceiptStatus, RemoteActionMessage, RemoteActionTarget, UpdateMark,
};

/// How long the host may hold one ask about a running step before it answers with the step as it
/// is. While a step runs and nothing happens, the agent wakes about once per hold.
const REASK_HOLD: Duration = Duration::from_secs(1);
/// How soon a step is asked about again when its host answered at once with nothing new: a host
/// that does not hold asks is asked at a steady pace rather than in a loop.
pub(super) const REASK_PACE: Duration = Duration::from_millis(50);
/// While the model is still writing the turn, a wait looks at it this often.
const TURN_PUMP: Duration = Duration::from_millis(100);
/// How long the host may take to take a step in.
const ADMISSION_WAIT: Duration = Duration::from_secs(10);
/// How long a step may run in its application before the turn gives up on it.
const STEP_WAIT: Duration = Duration::from_secs(30);

/// How a step is observed: by asking the host about it by its stable id, at most one ask out at a
/// time, which its host holds until the step moves on.
#[derive(Debug, Default)]
pub(super) struct Reask {
    /// When the ask out was sent.
    out: Option<Instant>,
    /// Not asked again before this: a host that answers at once is paced.
    not_before: Option<Instant>,
}

/// What the host answered about one step: its invocation's state, or why it would not say.
type Heard = (
    tilo_actions::InvocationId,
    Result<tilo_actions::InvocationUpdate, String>,
);

/// What the host's receipt says of the step it took or refused.
fn receipt_update(
    receipt: InvocationReceipt,
) -> Result<tilo_actions::InvocationUpdate, ActionCallError> {
    let invocation = receipt.invocation;
    Ok(match receipt.status {
        ReceiptStatus::Accepted => tilo_actions::InvocationUpdate::Accepted { invocation },
        ReceiptStatus::Completed { output } => {
            tilo_actions::InvocationUpdate::Succeeded { invocation, output }
        }
        ReceiptStatus::Failed { error } => {
            tilo_actions::InvocationUpdate::Failed { invocation, error }
        }
        ReceiptStatus::Cancelled { message } => tilo_actions::InvocationUpdate::Cancelled {
            invocation,
            message,
        },
        ReceiptStatus::Rejected { error } => return Err(error),
    })
}

/// The request that runs `action` with `arguments` as the step `invocation` of `run`.
fn invoke_request(
    run: &Asked,
    invocation: tilo_actions::InvocationId,
    action: &TaskAction,
    arguments: &serde_json::Map<String, serde_json::Value>,
) -> InvokeRequest {
    let provider = if action.provider.is_empty() {
        action.peer_id.to_string()
    } else {
        action.provider.clone()
    };
    InvokeRequest {
        invocation,
        target: RemoteActionTarget {
            provider,
            address: tilo_actions::ActionAddress::new(&action.resource, &action.action),
        },
        argument: InvokeArgument::Json(arguments.clone()),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: Some(run.invocation),
        appearance: None,
        wait_ms: None,
    }
}

impl Session {
    /// Ask the host to start one step of the turn: authorized and accepted by the broker on its
    /// own, exactly as when it is followed alone. The host answers with the accepted state at
    /// once; the action then runs in its application while this agent starts the next one or
    /// follows it ([`Self::follow_native`]). A step the person is asked about first waits for
    /// their answer on the system's sheet, in their own time.
    pub(super) fn start_task_invocation(
        &mut self,
        run: &Asked,
        action: &TaskAction,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Result<Following, TaskActionStop>> {
        let invocation = self.task_child_id(run, action, arguments)?;
        let mut request = invoke_request(run, invocation, action, arguments);
        let mut asked_person = false;
        let update = loop {
            self.link.send(&CollabRequest::RemoteAction {
                message: RemoteActionMessage::Invoke(request.clone()),
            })?;
            let Some(receipt) = self.await_receipt(invocation)? else {
                return Ok(Err(TaskActionStop::Failed(
                    "the broker did not answer the step".into(),
                )));
            };
            match receipt {
                Ok(update) => break update,
                Err(ActionCallError::ConfirmationRequired { .. }) if !asked_person => {
                    // The step asks the person before it runs: the host puts the question on the
                    // system's sheet, and the step goes on once they allow it.
                    asked_person = true;
                    match self.confirm_step(run, &request)? {
                        Ok(confirmed) => confirmed.apply(&mut request),
                        Err(stop) => return Ok(Err(stop)),
                    }
                }
                Err(error) => {
                    return Ok(Err(TaskActionStop::Failed(
                        super::confirmation::step_failure(error.to_string()),
                    )));
                }
            }
        };
        Ok(Ok(Following {
            action: action.clone(),
            invocation,
            reask: Reask::default(),
            update,
            deadline: Some(Session::task_wait_deadline(STEP_WAIT)),
            outcome: None,
        }))
    }

    /// The host's receipt for the step `invocation`, within [`ADMISSION_WAIT`]; `None` when it
    /// gave none in time. A step the host took is followed even when the person ended the turn
    /// meanwhile: it is stopped as one of the turn's running steps.
    fn await_receipt(
        &mut self,
        invocation: tilo_actions::InvocationId,
    ) -> Result<Option<Result<tilo_actions::InvocationUpdate, ActionCallError>>> {
        let until = Session::task_wait_deadline(ADMISSION_WAIT);
        let mut deferred = Vec::new();
        let answer = loop {
            self.pump_turn();
            let now = ui::time::now();
            if now >= until {
                break None;
            }
            match self.link.recv((until - now).min(TURN_PUMP))? {
                Some(CollabEvent::RemoteAction {
                    message: RemoteActionMessage::Receipt(receipt),
                }) if receipt.invocation == invocation => break Some(receipt_update(receipt)),
                Some(CollabEvent::ActionCatalogue { change }) => {
                    self.apply_broker_catalogue(&change);
                }
                Some(CollabEvent::RemoteAction { .. } | CollabEvent::Document { .. }) | None => {}
                Some(event) => deferred.push(event),
            }
        };
        self.link.defer(deferred);
        Ok(answer)
    }

    /// Ask the person about `request` on the system's sheet and wait for their answer, in their
    /// own time: what they allowed, or why the step does not run.
    fn confirm_step(
        &mut self,
        run: &Asked,
        request: &InvokeRequest,
    ) -> Result<Result<Box<ConfirmedInvocation>, TaskActionStop>> {
        let correlation = self.next_request_id();
        self.link.send(&CollabRequest::RemoteAction {
            message: RemoteActionMessage::Confirm {
                correlation,
                request: Box::new(request.clone()),
            },
        })?;
        let mut deferred = Vec::new();
        let answer = loop {
            self.pump_turn();
            if self.task_cancelled(run) {
                break Err(TaskActionStop::Cancelled(Some(crate::i18n::t(
                    "agent-cancel-task-center",
                ))));
            }
            match self.link.recv(TURN_PUMP)? {
                Some(CollabEvent::RemoteAction {
                    message:
                        RemoteActionMessage::ConfirmResponse {
                            correlation: answered,
                            result,
                        },
                }) if answered == correlation => {
                    // A decline reaches the model in the person's words, with what to do next.
                    break result.map_err(|error| {
                        TaskActionStop::Failed(super::confirmation::step_failure(error.to_string()))
                    });
                }
                Some(CollabEvent::ActionCatalogue { change }) => {
                    self.apply_broker_catalogue(&change);
                }
                Some(CollabEvent::RemoteAction { .. } | CollabEvent::Document { .. }) | None => {}
                Some(event) => deferred.push(event),
            }
        };
        self.link.defer(deferred);
        Ok(answer)
    }

    /// Follow started steps, together, until each has its outcome. Each running step has one ask
    /// out, which its host holds until the step moves past what was seen or settles; the agent
    /// sleeps until one of them answers. A turn the person ended stops every step.
    pub(super) fn follow_native(&mut self, run: &Asked, followed: &mut [Following]) -> Result<()> {
        loop {
            for following in followed.iter_mut() {
                conclude_native(following);
            }
            if followed.iter().all(|following| following.outcome.is_some()) {
                return Ok(());
            }
            if self.task_cancelled(run) {
                self.cancel_running(followed)?;
                return Ok(());
            }
            let now = ui::time::now();
            for following in followed
                .iter_mut()
                .filter(|following| following.outcome.is_none())
            {
                self.reask_native(following, now)?;
            }
            let asked: Vec<tilo_actions::InvocationId> = followed
                .iter()
                .filter(|following| following.reask.out.is_some())
                .map(|following| following.invocation)
                .collect();
            // The next thing to look at: the earliest deadline still running, or the pace a step
            // is held at.
            let until = followed
                .iter()
                .filter(|following| following.outcome.is_none())
                .flat_map(|following| {
                    let reask = &following.reask;
                    let paced = reask.out.is_none().then_some(reask.not_before).flatten();
                    [following.deadline, paced]
                })
                .flatten()
                .min()
                .unwrap_or_else(|| now.checked_add(REASK_HOLD).unwrap_or(now));
            if let Some((invocation, result)) = self.await_task_replies(run, &asked, until)? {
                take_reask_answer(followed, invocation, result);
            }
        }
    }

    /// Ask the host about `following`'s step, held until it moves past its latest state; nothing
    /// when an ask is already out or the step is being paced.
    fn reask_native(&mut self, following: &mut Following, now: Instant) -> Result<()> {
        let reask = &mut following.reask;
        if reask.out.is_some() || reask.not_before.is_some_and(|at| now < at) {
            return Ok(());
        }
        let hold = u32::try_from(REASK_HOLD.as_millis()).unwrap_or(u32::MAX);
        self.link.send(&CollabRequest::RemoteAction {
            message: RemoteActionMessage::Observe {
                invocation: following.invocation,
                wait_ms: Some(hold),
                seen: Some(UpdateMark::of(&following.update)),
            },
        })?;
        reask.out = Some(now);
        reask.not_before = None;
        Ok(())
    }

    /// Wait until `until` for the host's answer about any of the steps `asked`; `None` when none
    /// arrived or the person ended the turn. A model turn still being written is kept moving
    /// meanwhile; catalogue updates are applied and every other event keeps its place.
    fn await_task_replies(
        &mut self,
        run: &Asked,
        asked: &[tilo_actions::InvocationId],
        until: Instant,
    ) -> Result<Option<Heard>> {
        let mut deferred = Vec::new();
        let answer = loop {
            self.pump_turn();
            let now = ui::time::now();
            if now >= until || self.task_cancelled(run) {
                break None;
            }
            let left = until - now;
            let wait = if self.turn_streaming() {
                left.min(TURN_PUMP)
            } else {
                left
            };
            match self.link.recv(wait)? {
                Some(CollabEvent::RemoteAction {
                    message: RemoteActionMessage::Update { invocation, state },
                }) if asked.contains(&invocation) => {
                    break Some((
                        invocation,
                        state
                            .map(|state| state.update)
                            .ok_or_else(|| "the host no longer knows the step".to_owned()),
                    ));
                }
                Some(CollabEvent::ActionCatalogue { change }) => {
                    self.apply_broker_catalogue(&change);
                }
                // A late answer to an ask no longer out, the broker's word on a stop, or document
                // traffic: nothing waits for them here.
                Some(CollabEvent::RemoteAction { .. } | CollabEvent::Document { .. }) | None => {}
                Some(event) => deferred.push(event),
            }
        };
        self.link.defer(deferred);
        Ok(answer)
    }

    /// Stop every step still running, and settle each as cancelled: the person ended the turn, or
    /// it ended before the steps settled.
    pub(super) fn cancel_running(&mut self, followed: &mut [Following]) -> Result<()> {
        for following in followed
            .iter_mut()
            .filter(|following| following.outcome.is_none())
        {
            self.link.send(&CollabRequest::RemoteAction {
                message: RemoteActionMessage::Cancel {
                    invocation: following.invocation,
                },
            })?;
            following.outcome = Some(Err(TaskActionStop::Cancelled(Some(crate::i18n::t(
                "agent-cancel-task-center",
            )))));
        }
        Ok(())
    }

    /// The stable id of the step that performs `action` with `arguments` in the turn.
    ///
    /// The id is derived from the turn's answer, the request itself, and how many identical
    /// requests the turn made before it: a step asked again finds its earlier invocation (and its
    /// outcome) instead of running twice, and a different one is a new invocation rather than "an
    /// identity reused with another request" that the host must refuse.
    pub(super) fn task_child_id(
        &mut self,
        run: &Asked,
        action: &TaskAction,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<tilo_actions::InvocationId> {
        let request = child_request_digest(action, arguments);
        let occurrence = self.children.entry(request).or_default();
        let bytes = blake3::Hasher::new_derive_key("Tilo turn step v1")
            .update(&request)
            .update(&occurrence.to_le_bytes())
            .update(run.invocation.0.as_bytes())
            .finalize();
        *occurrence = occurrence
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("turn step index exhausted"))?;
        let mut id = [0; 16];
        id.copy_from_slice(&bytes.as_bytes()[..16]);
        Ok(tilo_actions::InvocationId(Uuid::from_bytes(id)))
    }
}

/// What names one request of a turn: the exact target and the arguments (digested canonically, so
/// key order does not matter).
fn child_request_digest(
    action: &TaskAction,
    arguments: &serde_json::Map<String, serde_json::Value>,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_derive_key("Tilo turn step request v1");
    hasher.update(&action.peer_id.to_le_bytes());
    for part in [&action.provider, &action.resource, &action.action] {
        hasher.update(&(part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    hasher.update(&tilo_actions::draft::arguments_digest(arguments));
    *hasher.finalize().as_bytes()
}

/// Take in the host's answer about `invocation`: the step's newer state, or the host's refusal
/// to say, which settles it as failed. A host that answered at once with nothing new does not
/// hold asks: its step is asked about again at a steady pace.
fn take_reask_answer(
    followed: &mut [Following],
    invocation: tilo_actions::InvocationId,
    result: Result<tilo_actions::InvocationUpdate, String>,
) {
    let Some(following) = followed
        .iter_mut()
        .find(|following| following.invocation == invocation && following.reask.out.is_some())
    else {
        return;
    };
    let reask = &mut following.reask;
    let Some(sent) = reask.out.take() else {
        return;
    };
    match result {
        Ok(update) => {
            let news = update.is_terminal()
                || UpdateMark::of(&update) != UpdateMark::of(&following.update);
            if !news && sent.elapsed() < REASK_HOLD / 2 {
                reask.not_before = sent.checked_add(REASK_PACE);
            }
            following.update = update;
        }
        Err(error) => following.outcome = Some(Err(TaskActionStop::Failed(error))),
    }
}

/// Settle `following` once its step is terminal or its time is up.
fn conclude_native(following: &mut Following) {
    if following.outcome.is_some() {
        return;
    }
    following.outcome = match &following.update {
        tilo_actions::InvocationUpdate::Succeeded { output, .. } => Some(Ok(StepOutcome {
            output: output.clone(),
        })),
        tilo_actions::InvocationUpdate::Cancelled { message, .. } => {
            Some(Err(TaskActionStop::Cancelled(message.clone())))
        }
        tilo_actions::InvocationUpdate::Failed { error, .. } => {
            Some(Err(TaskActionStop::Failed(error.message.clone())))
        }
        _ if following
            .deadline
            .is_some_and(|deadline| ui::time::now() >= deadline) =>
        {
            Some(Err(TaskActionStop::Failed(crate::i18n::t(
                "agent-fail-native-outcome",
            ))))
        }
        _ => None,
    };
}
