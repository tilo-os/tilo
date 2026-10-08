//! Actual SDK consumer on the existing signed, kernel-admitted fixture descriptor.
use super::{Result, frame, gate};
use std::{
    collections::VecDeque,
    os::fd::RawFd,
    os::unix::net::UnixStream,
    time::{Duration, Instant},
};
use ui::collaboration::{CollabEvent, CollabRequest, CollaborationManager};

struct Recipient {
    manager: CollaborationManager,
    pending: VecDeque<CollabEvent>,
    catalogue_seen: bool,
}

impl Recipient {
    fn take(&mut self, matches: impl Fn(&CollabEvent) -> bool) -> Result<CollabEvent> {
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(10))
            .ok_or("deadline overflow")?;
        loop {
            if let Some(index) = self.pending.iter().position(&matches) {
                return self
                    .pending
                    .remove(index)
                    .ok_or_else(|| "pending SDK event disappeared".into());
            }
            for event in self.manager.poll() {
                if matches!(event, CollabEvent::ActionCatalogue { .. }) {
                    self.catalogue_seen = true;
                } else {
                    if self.pending.len() >= 128 {
                        return Err("SDK fixture event bound exceeded".into());
                    }
                    self.pending.push_back(event);
                }
            }
            if !self.manager.is_connected() || Instant::now() >= deadline {
                return Err("actual enrolled SDK reply unavailable".into());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn request(&mut self, request_id: u64, request: CollabRequest) -> Result<()> {
        if !self.manager.send(request) {
            return Err("actual SDK refused enrolled request".into());
        }
        let reply = self.take(|event| matches!(event, CollabEvent::EnrolledTaskReply { request_id: id, .. } if *id == request_id))?;
        let CollabEvent::EnrolledTaskReply { result, .. } = reply else {
            return Err("SDK reply changed".into());
        };
        result.map_err(Into::into)
    }
    fn cancellation(
        &mut self,
        id: u64,
        run: &runtime_collab::TaskRunEnvelope,
        lease: &str,
    ) -> Result<std::result::Result<bool, String>> {
        if !self.manager.send(CollabRequest::TaskRunCancellation {
            request_id: id,
            run_id: run.run_id,
            lease: lease.into(),
        }) {
            return Err("SDK cancellation request refused".into());
        }
        let reply = self.take(|event| matches!(event, CollabEvent::TaskRunCancellation { request_id, .. } if *request_id == id))?;
        let CollabEvent::TaskRunCancellation { result, .. } = reply else {
            return Err("cancellation reply changed".into());
        };
        Ok(result)
    }
    fn unavailable_action(
        &mut self,
        id: u64,
        run: &runtime_collab::TaskRunEnvelope,
        lease: &str,
    ) -> Result<String> {
        if !self.manager.send(CollabRequest::InvokeTaskAction {
            request_id: id,
            invocation: tilo_actions::InvocationId::new(),
            run_id: run.run_id,
            lease: lease.into(),
            step: None,
            peer_id: u64::MAX,
            resource: "missing-resource".into(),
            action: "inspect".into(),
            arguments: serde_json::Map::default(),
        }) {
            return Err("SDK task action request refused".into());
        }
        let event = self.take(|event| matches!(event, CollabEvent::TaskActionInvocationResult { request_id, .. } if *request_id == id))?;
        let CollabEvent::TaskActionInvocationResult { result, .. } = event else {
            return Err("task action reply changed".into());
        };
        result
            .err()
            .ok_or_else(|| "unpublished resource action was admitted".into())
    }
}

pub(super) fn exercise(
    fd: RawFd,
    context: &serde_json::Value,
    mut model: UnixStream,
    mode: &str,
) -> Result<()> {
    let text = |key| {
        context
            .get(key)
            .and_then(serde_json::Value::as_str)
            .ok_or("managed fixture field absent")
    };
    let room = text("room")?.parse()?;
    let agent = text("agent")?;
    if mode == "ordinary" {
        return refuse_ordinary_sdk(fd, room, agent, &mut model);
    }
    let manager =
        CollaborationManager::for_enrolled_task_route_fd(fd, "so.tilo.agent", room, agent)?;
    let mut recipient = Recipient {
        manager,
        pending: VecDeque::new(),
        catalogue_seen: false,
    };
    check_document_refusals(&mut recipient.manager)?;
    recipient.request(1, CollabRequest::SubscribeEnrolledTasks { request_id: 1 })?;
    let event = recipient.take(|event| matches!(event, CollabEvent::EnrolledTask(_)))?;
    let CollabEvent::EnrolledTask(envelope) = event else {
        return Err("SDK envelope changed".into());
    };
    if !recipient.catalogue_seen {
        return Err("actual permission-filtered catalogue was not observed".into());
    }
    frame(
        &mut model,
        &serde_json::json!({"stage":"offered", "envelope":envelope, "sdk_task_only":true}),
    )?;
    gate(&mut model, b'A')?;
    if recipient
        .cancellation(10, &envelope.task, &envelope.task.lease)?
        .is_ok()
    {
        return Err("unaccepted enrolled lease acquired cancellation authority".into());
    }
    let mut custody = runtime_collab::EnrolledRecipientCustody::new(
        agent,
        text("launch")?,
        text("runtime_epoch")?,
        text("channel_id")?,
    )?;
    let decision = custody.accept(&envelope)?;
    if !decision.execute {
        return Err("fresh SDK recipient custody did not execute".into());
    }
    let accepted = decision.acceptance;
    recipient.request(
        2,
        CollabRequest::AcceptEnrolledTask {
            request_id: 2,
            acceptance: accepted.clone(),
        },
    )?;
    check_accepted_lease(&mut recipient, &envelope.task)?;
    frame(
        &mut model,
        &serde_json::json!({"stage":"accepted", "acceptance":accepted, "native_lease_observed":true,
            "unaccepted_lease_refused":true,"wrong_lease_refused":true,"wrong_numeric_run_refused":true}),
    )?;
    gate(&mut model, b'P')?;
    recipient.request(
        3,
        CollabRequest::ReportEnrolledTask {
            request_id: 3,
            acceptance: accepted.clone(),
            sequence: 1,
            update: runtime_collab::TaskRunUpdate::Progress {
                message: "Private SDK progress".into(),
            },
        },
    )?;
    frame(&mut model, &serde_json::json!({"stage":"progress"}))?;
    if mode.starts_with("steps") {
        steps(&mut recipient, &mut model, &envelope.task, mode)?;
    }
    finish(&mut recipient, &mut model, &envelope.task, accepted, mode)
}

fn finish(
    recipient: &mut Recipient,
    model: &mut UnixStream,
    task: &runtime_collab::TaskRunEnvelope,
    accepted: runtime_collab::EnrolledTaskAcceptance,
    mode: &str,
) -> Result<()> {
    gate(model, b'F')?;
    if mode.ends_with("cancelled") {
        if recipient.cancellation(15, task, &task.lease)? != Ok(true) {
            return Err("exact accepted native cancellation was not observed".into());
        }
        frame(
            model,
            &serde_json::json!({"stage":"cancelled", "owner_cancelled_observed":true}),
        )?;
        gate(model, b'E')?;
        return Ok(());
    }
    recipient.request(
        4,
        CollabRequest::ReportEnrolledTask {
            request_id: 4,
            acceptance: accepted,
            sequence: 2,
            update: if mode == "steps-prepare-ack-lost" {
                runtime_collab::TaskRunUpdate::Failed {
                    message: "Uncertain prepared step".into(),
                }
            } else {
                runtime_collab::TaskRunUpdate::Succeeded {
                    message: "Fixture complete".into(),
                }
            },
        },
    )?;
    if recipient.cancellation(14, task, &task.lease)?.is_ok() {
        return Err("terminal enrolled lease retained cancellation authority".into());
    }
    frame(
        model,
        &serde_json::json!({"stage":"finished", "terminal_lease_refused":true}),
    )?;
    gate(model, b'E')?;
    Ok(())
}

fn check_accepted_lease(
    recipient: &mut Recipient,
    task: &runtime_collab::TaskRunEnvelope,
) -> Result<()> {
    if recipient.cancellation(11, task, &task.lease)? != Ok(false) {
        return Err("accepted enrolled lease cannot observe current native cancellation".into());
    }
    if recipient.cancellation(12, task, "wrong-lease")?.is_ok() {
        return Err("wrong lease acquired native cancellation authority".into());
    }
    let mut foreign = task.clone();
    foreign.run_id = foreign
        .run_id
        .checked_add(1)
        .ok_or("numeric run overflow")?;
    if recipient.cancellation(13, &foreign, &task.lease)?.is_ok() {
        return Err("wrong numeric run acquired native cancellation authority".into());
    }
    let unpublished = recipient.unavailable_action(16, task, &task.lease)?;
    if !unpublished.contains("no longer published in this workspace") {
        return Err(format!(
            "accepted task action failed before current publication check: {unpublished}"
        )
        .into());
    }
    let wrong_action_lease = recipient.unavailable_action(17, task, "wrong-lease")?;
    if !wrong_action_lease.contains("accepted enrolled lease not retained") {
        return Err("wrong task action lease reached resource admission".into());
    }
    Ok(())
}

fn refuse_ordinary_sdk(
    fd: RawFd,
    room: uuid::Uuid,
    agent: &str,
    model: &mut UnixStream,
) -> Result<()> {
    let refused = CollaborationManager::for_agent_route_fd(fd, "so.tilo.agent", room, None, agent);
    let Err(error) = refused else {
        return Err("ordinary SDK document mode acquired managed document custody".into());
    };
    frame(
        model,
        &serde_json::json!({"stage":"sdk-ordinary-refused", "error":error.to_string()}),
    )?;
    gate(model, b'E')?;
    Ok(())
}

fn check_document_refusals(manager: &mut CollaborationManager) -> Result<()> {
    if manager.has_identity()
        || manager.send(CollabRequest::document_snapshot(vec![1]))
        || manager.send(CollabRequest::RequestDocumentSnapshot)
        || manager.send(CollabRequest::PublishActions {
            app_id: "so.tilo.agent".into(),
            actions: vec![],
        })
        || manager.begin_document_publication_barrier().is_ok()
    {
        return Err("task-only SDK claimed document or resource publication authority".into());
    }
    manager.publish_action_directory("so.tilo.agent");
    if !manager.is_connected() {
        return Err("refused publication retired legitimate task custody".into());
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct StepCall<'a> {
    id: u64,
    lease: &'a str,
    index: u32,
    changed: bool,
}
impl Recipient {
    fn step(
        &mut self,
        task: &runtime_collab::TaskRunEnvelope,
        peer: u64,
        call: StepCall<'_>,
    ) -> Result<CollabEvent> {
        if !self.manager.send(CollabRequest::InvokeTaskAction {
            request_id: call.id,
            invocation: tilo_actions::InvocationId::new(),
            run_id: task.run_id,
            lease: call.lease.into(),
            step: Some(call.index),
            peer_id: peer,
            resource: "reader:fixture".into(),
            action: "inspect".into(),
            arguments: if call.changed {
                serde_json::from_value(serde_json::json!({"changed":true}))?
            } else {
                serde_json::Map::default()
            },
        }) {
            return Err("actual SDK step request refused".into());
        }
        self.take(|event| matches!(event, CollabEvent::TaskActionInvocationResult { request_id, .. } if *request_id == call.id))
    }
}
fn step_terminal(event: CollabEvent) -> Result<(tilo_actions::InvocationUpdate, Vec<u8>)> {
    let CollabEvent::TaskActionInvocationResult {
        result,
        continuation,
        retry_step,
        ..
    } = event
    else {
        return Err("step reply changed".into());
    };
    let (_, update) = result.map_err(|error| format!("actual step failed: {error}"))?;
    if retry_step || !matches!(update, tilo_actions::InvocationUpdate::Succeeded { .. }) {
        return Err("step did not commit a successful canonical terminal".into());
    }
    Ok((update, continuation.ok_or("committed continuation absent")?))
}
fn step_refusal(event: CollabEvent) -> Result<String> {
    let CollabEvent::TaskActionInvocationResult {
        result: Err(error),
        continuation: None,
        ..
    } = event
    else {
        return Err("altered or unauthorized step was accepted".into());
    };
    Ok(error)
}
fn step_control(model: &mut UnixStream) -> Result<serde_json::Value> {
    use std::io::Read;
    let mut length = [0; 4];
    model.read_exact(&mut length)?;
    let length = usize::try_from(u32::from_le_bytes(length))?;
    if length > 4096 {
        return Err("step control frame exceeds bound".into());
    }
    let mut bytes = vec![0; length];
    model.read_exact(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}
fn first_step(
    recipient: &mut Recipient,
    model: &mut UnixStream,
    task: &runtime_collab::TaskRunEnvelope,
    peer: u64,
    mode: &str,
) -> Result<Option<CollabEvent>> {
    let mut first = recipient.step(
        task,
        peer,
        StepCall {
            id: 20,
            lease: &task.lease,
            index: 0,
            changed: false,
        },
    )?;
    if mode == "steps-prepare-ack-lost" {
        let error = step_refusal(first)?;
        if !error.contains("injected lost acknowledgement") {
            return Err(error.into());
        }
        let replay = step_refusal(recipient.step(
            task,
            peer,
            StepCall {
                id: 21,
                lease: &task.lease,
                index: 0,
                changed: false,
            },
        )?)?;
        if !replay.contains("prepared without a retained dispatch record") {
            return Err(replay.into());
        }
        frame(
            model,
            &serde_json::json!({"stage":"step-prepare-ack-lost","prepared_without_journal_refused":true}),
        )?;
        return Ok(None);
    }
    if mode == "steps-commit-ack-lost" {
        let error = step_refusal(first)?;
        if !error.contains("injected lost acknowledgement") {
            return Err(error.into());
        }
        first = recipient.step(
            task,
            peer,
            StepCall {
                id: 27,
                lease: &task.lease,
                index: 0,
                changed: false,
            },
        )?;
    }
    if mode == "steps-final-admission-cancelled" {
        let refusal = step_refusal(first)?;
        if !refusal.contains("cancelled") {
            return Err(refusal.into());
        }
        frame(
            model,
            &serde_json::json!({"stage":"step-final-admission-cancelled","final_admission_refused":true}),
        )?;
        return Ok(None);
    }
    if mode == "steps-denied" {
        let refusal = step_refusal(first)?;
        if !refusal.contains("caller no longer authorized") {
            return Err(refusal.into());
        }
        frame(
            model,
            &serde_json::json!({"stage":"step-denied","scoped_read_refused":true}),
        )?;
        return Ok(None);
    }
    Ok(Some(first))
}

fn steps(
    recipient: &mut Recipient,
    model: &mut UnixStream,
    task: &runtime_collab::TaskRunEnvelope,
    mode: &str,
) -> Result<()> {
    let target = step_control(model)?;
    let peer = target
        .get("peer")
        .and_then(serde_json::Value::as_u64)
        .ok_or("step target peer absent")?;
    let Some(first) = first_step(recipient, model, task, peer, mode)? else {
        return Ok(());
    };
    let replay = recipient.step(
        task,
        peer,
        StepCall {
            id: 21,
            lease: &task.lease,
            index: 0,
            changed: false,
        },
    )?;
    if step_terminal(first)? != step_terminal(replay)? {
        return Err("step replay changed committed private output".into());
    }
    for call in [
        StepCall {
            id: 22,
            lease: &task.lease,
            index: 0,
            changed: true,
        },
        StepCall {
            id: 23,
            lease: "wrong-lease",
            index: 0,
            changed: false,
        },
        StepCall {
            id: 24,
            lease: &task.lease,
            index: 2,
            changed: false,
        },
    ] {
        step_refusal(recipient.step(task, peer, call)?)?;
    }
    frame(
        model,
        &serde_json::json!({"stage":"steps", "replay_same_terminal":true,
        "changed_arguments_refused":true,"wrong_lease_refused":true,"gap_index_refused":true,"lost_terminal_ack_recovered":mode == "steps-commit-ack-lost"}),
    )?;
    if mode == "steps-permission-revoked" {
        step_control(model)?;
        let refusal = step_refusal(recipient.step(
            task,
            peer,
            StepCall {
                id: 25,
                lease: &task.lease,
                index: 1,
                changed: false,
            },
        )?)?;
        if !refusal.contains("caller no longer authorized") {
            return Err(refusal.into());
        }
        frame(
            model,
            &serde_json::json!({"stage":"step-revoked","current_scoped_read_refused":true}),
        )?;
    }
    if mode == "steps-cancelled" {
        let refusal = step_refusal(recipient.step(
            task,
            peer,
            StepCall {
                id: 26,
                lease: &task.lease,
                index: 1,
                changed: false,
            },
        )?)?;
        if !refusal.contains("cancelled") {
            return Err(refusal.into());
        }
        frame(
            model,
            &serde_json::json!({"stage":"step-cancelled","new_admission_refused":true}),
        )?;
    }
    Ok(())
}
