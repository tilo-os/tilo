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
}

pub(super) fn exercise(
    fd: RawFd,
    context: &serde_json::Value,
    mut model: UnixStream,
    ordinary: bool,
) -> Result<()> {
    let text = |key| {
        context
            .get(key)
            .and_then(serde_json::Value::as_str)
            .ok_or("managed fixture field absent")
    };
    let room = text("room")?.parse()?;
    let agent = text("agent")?;
    if ordinary {
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
    frame(
        &mut model,
        &serde_json::json!({"stage":"accepted", "acceptance":accepted}),
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
    gate(&mut model, b'F')?;
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
    frame(&mut model, &serde_json::json!({"stage":"finished"}))?;
    gate(&mut model, b'E')?;
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
