//! The private action lane to a governed room's governor, on the same relay
//! session that carries the room's state. The shell only asks on it: it
//! invokes actions the governor publishes and reads their answers, exactly
//! as any certified member does. Answers are accepted only from the
//! governor the room was joined under, for a request this lane sent.
use std::collections::HashMap;
use std::sync::Arc;

use tilo_delivery_protocol::peer_actions::{ActionMessage, DirectedAction};
use tilo_delivery_protocol::transport_v2::EnvelopeV2;
use tilo_relay_session::RelayCoeditSession;
use tokio::sync::{mpsc, oneshot};

/// Most requests one lane keeps waiting for an answer.
const MAX_WAITING: usize = 32;

/// One request to the governor and where its answer goes.
#[derive(Debug)]
pub struct Request {
    pub action: DirectedAction,
    /// The governor's user key, which the relay attests on its answers.
    pub governor_user: [u8; 32],
    pub reply: oneshot::Sender<Result<ActionMessage, String>>,
}

pub(super) struct Lane {
    requests: mpsc::Sender<Request>,
    task: tokio::task::JoinHandle<()>,
}

impl Lane {
    pub(super) fn open(session: Arc<RelayCoeditSession>, room: uuid::Uuid) -> Self {
        let (requests, receiver) = mpsc::channel(MAX_WAITING);
        let task = tokio::spawn(run(session, room, receiver));
        Self { requests, task }
    }

    pub(super) fn send(&self, request: Request) {
        if let Err(error) = self.requests.try_send(request) {
            let request = match error {
                mpsc::error::TrySendError::Full(request)
                | mpsc::error::TrySendError::Closed(request) => request,
            };
            let _ = request
                .reply
                .send(Err("the governor lane is unavailable".into()));
        }
    }
}

impl Drop for Lane {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn answer(request: &Request, action: &DirectedAction, source_user: [u8; 32]) -> bool {
    source_user == request.governor_user
        && action.room == request.action.room
        && action.source == request.action.target
        && action.target == request.action.source
        && action.validate()
}

async fn run(
    session: Arc<RelayCoeditSession>,
    room: uuid::Uuid,
    mut requests: mpsc::Receiver<Request>,
) {
    let mut ready = false;
    let mut queued: Vec<Request> = Vec::new();
    let mut waiting: HashMap<uuid::Uuid, Request> = HashMap::new();
    let send = |request: Request, waiting: &mut HashMap<uuid::Uuid, Request>| {
        if waiting.len() >= MAX_WAITING || waiting.contains_key(&request.action.request_id) {
            let _ = request
                .reply
                .send(Err("too many requests to the governor".into()));
            return;
        }
        if session.send_action(request.action.clone()).is_err() {
            let _ = request
                .reply
                .send(Err("the governor lane is closed".into()));
            return;
        }
        waiting.insert(request.action.request_id, request);
    };
    // Without the lane, requests are dropped and their callers told it closed.
    if session.enable_actions(room).is_err() {
        return;
    }
    loop {
        tokio::select! {
            request = requests.recv() => match request {
                None => break,
                Some(request) if ready => send(request, &mut waiting),
                Some(request) => queued.push(request),
            },
            event = session.recv_action() => match event {
                None => break,
                Some(EnvelopeV2::ActionsReady { room: ready_room, .. }) if ready_room == room => {
                    if !ready && session.enable_broker_client(room).is_ok() {
                        ready = true;
                        for request in std::mem::take(&mut queued) {
                            send(request, &mut waiting);
                        }
                    }
                }
                Some(EnvelopeV2::Delivered { action, source_user }) => {
                    if let Some(request) = waiting.remove(&action.request_id) {
                        let reply = if answer(&request, &action, source_user) {
                            Ok(action.message)
                        } else {
                            Err("the answer did not come from the room's governor".into())
                        };
                        let _ = request.reply.send(reply);
                    }
                }
                Some(EnvelopeV2::Unavailable { request_id, reason, .. }) => {
                    if let Some(request) = waiting.remove(&request_id) {
                        let _ = request
                            .reply
                            .send(Err(format!("the governor is unavailable: {reason:?}")));
                    }
                }
                Some(_) => {}
            },
        }
    }
    for request in queued.into_iter().chain(waiting.into_values()) {
        let _ = request.reply.send(Err("the governor lane closed".into()));
    }
}
