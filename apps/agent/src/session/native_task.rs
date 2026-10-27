//! Steering on the existing native task lease. Receipt alone never acknowledges it.
use super::{
    CollabEvent, CollabRequest, Duration, Following, Instant, Result, Session, StepOutcome,
    TaskActionStop, Uuid,
};
use tilo_actions::remote::UpdateMark;

/// How long the host may hold one re-ask of a running child before it
/// answers with the child as it is. While a step runs and nothing happens,
/// the agent wakes about once per hold.
const REASK_HOLD: Duration = Duration::from_secs(1);
/// How soon a child is re-asked when its host answered at once with nothing
/// new: a host that does not hold re-asks is asked at the old cadence rather
/// than in a loop.
pub(super) const REASK_PACE: Duration = Duration::from_millis(50);
/// While the model is still writing the turn, a wait looks at it this often.
const TURN_PUMP: Duration = Duration::from_millis(100);

#[derive(Default)]
pub(super) struct NativeTaskState {
    pub run_id: u64,
    pub task_id: String,
    pub base: String,
    pub revision: u64,
    /// How many children each request (by [`child_request_digest`]) has
    /// started in this run so far.
    pub children: std::collections::HashMap<[u8; 32], u32>,
    pub epoch: Option<u64>,
    pub pending: Option<runtime_collab::TaskRunControl>,
}

/// How a native task's child is observed: by re-asking for it with its
/// stable id, at most one re-ask out at a time, which its host holds until
/// the child moves on.
#[derive(Debug, Default)]
pub(super) struct Reask {
    /// The re-ask out: its request id and when it was sent.
    out: Option<(u64, Instant)>,
    /// Not re-asked before this: a host that answers at once is paced.
    not_before: Option<Instant>,
}

/// One answer to a task action request: its request id and result.
type TaskActionAnswer = (u64, Result<(u64, tilo_actions::InvocationUpdate), String>);

impl Session {
    /// Follow started native children, together, until each has its
    /// outcome. Each running child has one re-ask out, which its host holds
    /// until the child moves past what was seen, settles, or the run is
    /// cancelled; the agent sleeps until one of them answers. Every answer
    /// is the host's observation of the child, authorized when it is given.
    /// A cancelled run stops every child: the host cancels them when it
    /// reads the run cancelled, and each is reported cancelled here.
    pub(super) fn follow_native(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        followed: &mut [Following],
    ) -> Result<()> {
        loop {
            for following in followed.iter_mut() {
                conclude_native(following);
            }
            if followed.iter().all(|following| following.outcome.is_some()) {
                return Ok(());
            }
            // One poll out at a time, read without waiting: it is answered
            // ahead of the re-asks sent after it.
            self.poll_cancellation(run)?;
            if self.cancellation_known()? {
                cancel_running(followed);
                return Ok(());
            }
            let now = ui::time::now();
            for following in followed
                .iter_mut()
                .filter(|following| following.outcome.is_none())
            {
                self.reask_native(run, following, now)?;
            }
            let asked: Vec<u64> = followed
                .iter()
                .filter_map(|following| following.reask.out.map(|(id, _)| id))
                .collect();
            let until = followed
                .iter()
                .filter(|following| following.outcome.is_none())
                .flat_map(|following| {
                    let reask = &following.reask;
                    let paced = reask.out.is_none().then_some(reask.not_before).flatten();
                    [Some(following.deadline), paced]
                })
                .flatten()
                .min()
                .unwrap_or(now);
            if let Some((request_id, result)) = self.await_task_replies(&asked, until)? {
                // The host refuses to observe a child of a run it has just
                // read cancelled: that is the run's cancellation, not the
                // step's failure.
                if result.is_err() && self.task_cancelled(run)? {
                    cancel_running(followed);
                    return Ok(());
                }
                take_reask_answer(followed, request_id, result);
            }
        }
    }

    /// Re-ask for `following`'s child with the identical request, held by
    /// the host until the child moves past its latest state; nothing when a
    /// re-ask is already out or the child is being paced.
    fn reask_native(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        following: &mut Following,
        now: Instant,
    ) -> Result<()> {
        let reask = &mut following.reask;
        if reask.out.is_some() || reask.not_before.is_some_and(|at| now < at) {
            return Ok(());
        }
        let request_id = self.next_request_id();
        let hold = u32::try_from(REASK_HOLD.as_millis()).unwrap_or(u32::MAX);
        self.link.send(&CollabRequest::InvokeTaskAction {
            request_id,
            invocation: following.invocation,
            run_id: run.run_id,
            lease: self.task_lease(run),
            peer_id: following.action.peer_id,
            provider: following.action.provider.clone(),
            resource: following.action.resource.clone(),
            action: following.action.action.clone(),
            arguments: following.arguments.clone(),
            wait: Some(runtime_collab::TaskChildWait {
                wait_ms: hold,
                seen: UpdateMark::of(&following.update),
            }),
        })?;
        reask.out = Some((request_id, now));
        reask.not_before = None;
        Ok(())
    }

    /// Wait until `until` for the answer to any of the task action requests
    /// `requests`; `None` when none arrived. A model turn still being
    /// written is kept moving meanwhile; catalogue updates are applied and
    /// every other event keeps its place.
    fn await_task_replies(
        &mut self,
        requests: &[u64],
        until: Instant,
    ) -> Result<Option<TaskActionAnswer>> {
        let mut deferred = Vec::new();
        let answer = loop {
            self.pump_turn();
            let now = ui::time::now();
            if now >= until {
                break None;
            }
            let left = until - now;
            let wait = if self.turn_streaming() {
                left.min(TURN_PUMP)
            } else {
                left
            };
            match self.link.recv(wait)? {
                Some(CollabEvent::TaskActionInvocationResult {
                    request_id, result, ..
                }) if requests.contains(&request_id) => break Some((request_id, result)),
                // A late answer to a re-ask no longer out, or document
                // traffic: nothing waits for either.
                Some(
                    CollabEvent::TaskActionInvocationResult { .. } | CollabEvent::Document { .. },
                )
                | None => {}
                Some(CollabEvent::ActionCatalogue { surfaces }) => {
                    self.set_broker_surfaces(surfaces);
                }
                Some(event) => deferred.push(event),
            }
        };
        self.link.defer(deferred);
        Ok(answer)
    }

    /// Wait for the host's receipt of report `request_id`. A report is how
    /// the run ends, so the run's own time budget does not cut it short.
    pub(super) fn wait_for_native_report(&mut self, request_id: u64) -> Result<()> {
        let deadline = ui::time::now() + Duration::from_secs(5);
        let mut deferred = Vec::new();
        while ui::time::now() < deadline {
            match self.link.recv(Duration::from_millis(50))? {
                Some(CollabEvent::TaskRunReportResult {
                    request_id: received,
                    result,
                }) if received == request_id => {
                    self.link.defer(deferred);
                    return result.map_err(anyhow::Error::msg);
                }
                Some(event) => deferred.push(event),
                None => {}
            }
        }
        self.link.defer(deferred);
        anyhow::bail!("native task report has no durable acknowledgement")
    }
    pub(super) fn task_lease(&self, run: &runtime_collab::TaskRunEnvelope) -> String {
        match self
            .native_task
            .as_ref()
            .filter(|state| state.run_id == run.run_id)
        {
            Some(state) => format!("{}:{}", state.base, state.revision),
            None => run.lease.clone(),
        }
    }

    pub(super) fn observe_native_control(
        &mut self,
        control: runtime_collab::TaskRunControl,
    ) -> Result<()> {
        control.validate().map_err(anyhow::Error::msg)?;
        let state = self
            .native_task
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("unexpected native task control"))?;
        if control.task_id != state.task_id
            || state.epoch.is_some_and(|epoch| epoch != control.run_epoch)
        {
            anyhow::bail!("native task control belongs to another run or frontier");
        }
        if control.from_revision != state.revision {
            anyhow::bail!("native task control belongs to another instruction frontier");
        }
        state.epoch = Some(control.run_epoch);
        if control.has_more {
            anyhow::bail!(
                "More than one page of task directions needs person review; none were acknowledged"
            );
        }
        if !control.directions.is_empty() {
            state.pending = Some(control);
        }
        Ok(())
    }

    pub(super) fn incorporate_native_directions(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        conversation: &mut crate::model::Conversation,
    ) -> Result<()> {
        let Some(control) = self
            .native_task
            .as_mut()
            .and_then(|state| state.pending.take())
        else {
            return Ok(());
        };
        // The attributed, exact source is now in the next model request. A
        // concurrent newer direction still fences every child at the owner.
        let text = serde_json::to_string(&control.directions)?;
        conversation.push_user(&format!(
            "Authenticated workspace task directions (apply before further operations):\n{text}"
        ));
        self.report_task(
            run,
            runtime_collab::TaskRunUpdate::DirectionsApplied {
                revision: control.latest_revision,
            },
        )?;
        if let Some(state) = self.native_task.as_mut() {
            state.revision = control.latest_revision;
        }
        self.step_settled();
        // This round trip observes durable acknowledgement; logging a failed
        // report in the SDK cannot silently mint a usable revised lease.
        if self.task_cancelled(run)? {
            anyhow::bail!("task cancelled while incorporating directions");
        }
        Ok(())
    }

    /// The stable id of the native child that performs `action` with
    /// `arguments` in `run`, at the run's instruction revision.
    ///
    /// The id is derived from the request itself, plus how many identical
    /// requests this run made before it. A run delivered again after its
    /// agent restarted makes the same id only for the same request: an
    /// identical step finds its earlier child (and its outcome) instead of
    /// running twice, and a different one, as a model that plans afresh
    /// asks, is a new child rather than "an identity reused with another
    /// request" that the host must refuse.
    pub(super) fn task_child_id(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        action: &super::TaskAction,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<tilo_actions::InvocationId> {
        let state = self
            .native_task
            .as_mut()
            .filter(|state| state.run_id == run.run_id)
            .ok_or_else(|| anyhow::anyhow!("a step outside the run it belongs to"))?;
        let request = child_request_digest(run.run_id, state.revision, action, arguments);
        let occurrence = state.children.entry(request).or_default();
        let bytes = blake3::Hasher::new_derive_key("Tilo native task child v2")
            .update(&request)
            .update(&occurrence.to_le_bytes())
            .finalize();
        *occurrence = occurrence
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("native task step index exhausted"))?;
        let mut id = [0; 16];
        id.copy_from_slice(&bytes.as_bytes()[..16]);
        Ok(tilo_actions::InvocationId(Uuid::from_bytes(id)))
    }
}

/// What names one request of a native task: its run, the instruction
/// revision, the exact target and the arguments (digested canonically, so
/// key order does not matter).
fn child_request_digest(
    run_id: u64,
    revision: u64,
    action: &super::TaskAction,
    arguments: &serde_json::Map<String, serde_json::Value>,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_derive_key("Tilo native task child request v1");
    hasher.update(&run_id.to_le_bytes());
    hasher.update(&revision.to_le_bytes());
    hasher.update(&action.peer_id.to_le_bytes());
    for part in [&action.provider, &action.resource, &action.action] {
        hasher.update(&(part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    hasher.update(&tilo_actions::draft::arguments_digest(arguments));
    *hasher.finalize().as_bytes()
}

/// Take in the answer to the re-ask `request_id`: the child's newer state,
/// or the host's refusal to observe it, which settles it as failed. A host
/// that answered at once with nothing new does not hold re-asks: its child
/// is re-asked at the old cadence.
fn take_reask_answer(
    followed: &mut [Following],
    request_id: u64,
    result: Result<(u64, tilo_actions::InvocationUpdate), String>,
) {
    let Some(following) = followed
        .iter_mut()
        .find(|following| following.reask.out.is_some_and(|(id, _)| id == request_id))
    else {
        return;
    };
    let reask = &mut following.reask;
    let Some((_, sent)) = reask.out.take() else {
        return;
    };
    match result {
        Ok((_, update)) => {
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

/// Settle every child still running as cancelled from Task Center.
fn cancel_running(followed: &mut [Following]) {
    for following in followed
        .iter_mut()
        .filter(|following| following.outcome.is_none())
    {
        following.outcome = Some(Err(TaskActionStop::Cancelled(Some(
            "Cancelled from Task Center".into(),
        ))));
    }
}

/// Settle `following` once its child is terminal or its time is up.
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
        _ if ui::time::now() >= following.deadline => Some(Err(TaskActionStop::Failed(
            "Native action outcome remains unknown; review history before retrying".into(),
        ))),
        _ => None,
    };
}
