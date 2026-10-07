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
    if mode == "cancelled" {
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
            update: runtime_collab::TaskRunUpdate::Succeeded {
                message: "Fixture complete".into(),
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
