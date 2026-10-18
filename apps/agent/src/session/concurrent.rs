//! Independent native task sessions sharing one authenticated broker connection.
//!
//! Only the connection owner touches the real transport. Task threads own their
//! planner, frontier, cancellation polls and confirmation waits. In particular a
//! parent waiting for its child cannot occupy the thread admitting that child.
use std::collections::HashMap;
use std::sync::mpsc;

use super::{CollabEvent, CollabRequest, Duration, Session};
use crate::wire::Link;

#[cfg(test)]
mod tests;

struct Mailbox {
    worker: u64,
    requests: mpsc::Sender<Message>,
    events: mpsc::Receiver<CollabEvent>,
    connected: bool,
    owner_wake: Option<std::sync::Arc<ui::wake::WakeHandle>>,
}

impl ui::collaboration::PeerTransport for Mailbox {
    fn is_connected(&self) -> bool {
        self.connected
    }

    fn send(&mut self, request: CollabRequest) -> Result<(), String> {
        self.requests
            .send(Message::Request(self.worker, Box::new(request)))
            .map_err(|_| "task connection owner stopped".to_string())?;
        notify(&self.owner_wake);
        Ok(())
    }

    fn poll(&mut self) -> Vec<CollabEvent> {
        let mut events = Vec::new();
        loop {
            match self.events.try_recv() {
                Ok(event) => events.push(event),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.connected = false;
                    break;
                }
            }
        }
        events
    }
}

enum Message {
    Model(u64, ModelCall),
    Request(u64, Box<CollabRequest>),
    Finished(u64),
}

type ModelReply = mpsc::SyncSender<Result<serde_json::Value, String>>;

struct ModelCall {
    invocation_id: Option<u64>,
    run_id: u64,
    lease: String,
    request: serde_json::Value,
    timeout: Duration,
    reply: ModelReply,
}

/// Release routing entries even if an unexpected planner panic unwinds.
struct Completion(
    u64,
    mpsc::Sender<Message>,
    Option<std::sync::Arc<ui::wake::WakeHandle>>,
);

fn notify(wake: &Option<std::sync::Arc<ui::wake::WakeHandle>>) {
    if let Some(wake) = wake {
        wake.wake();
    }
}

impl Drop for Completion {
    fn drop(&mut self) {
        let _ = self.1.send(Message::Finished(self.0));
        notify(&self.2);
    }
}

struct Worker {
    run_id: u64,
    task_id: String,
    events: mpsc::Sender<CollabEvent>,
    wake: std::sync::Arc<std::sync::OnceLock<ui::wake::WakeHandle>>,
}

impl Worker {
    fn deliver(&self, event: CollabEvent) {
        if self.events.send(event).is_ok()
            && let Some(wake) = self.wake.get()
        {
            wake.wake();
        }
    }
}

pub(super) struct Tasks {
    tx: mpsc::Sender<Message>,
    rx: mpsc::Receiver<Message>,
    workers: HashMap<u64, Worker>,
    /// Broker ids are connection-wide; each session starts its own local ids.
    replies: HashMap<u64, (u64, u64)>,
    invocations: HashMap<tilo_actions::InvocationId, u64>,
    next_worker: u64,
    model_replies: HashMap<u64, (u64, ModelReply)>,
    owner_wake: Option<std::sync::Arc<ui::wake::WakeHandle>>,
}

impl Tasks {
    pub(super) fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            tx,
            rx,
            workers: HashMap::new(),
            replies: HashMap::new(),
            invocations: HashMap::new(),
            next_worker: 0,
            model_replies: HashMap::new(),
            owner_wake: None,
        }
    }

    /// Outer Transform workers share the owner transport but not a planner's
    /// lease. Only the host can resolve this incoming wire id to native custody.
    pub(super) fn native_transform_scope(&self, invocation_id: u64) -> crate::model::native::Scope {
        let tx = self.tx.clone();
        let owner_wake = self.owner_wake.clone();
        crate::model::native::Scope::for_invocation(move |request, timeout| {
            let (reply, rx) = mpsc::sync_channel(1);
            tx.send(Message::Model(
                0,
                ModelCall {
                    invocation_id: Some(invocation_id),
                    run_id: 0,
                    lease: String::new(),
                    request,
                    timeout,
                    reply,
                },
            ))
            .map_err(|_| "task connection owner stopped".to_string())?;
            notify(&owner_wake);
            rx.recv_timeout(timeout)
                .map_err(|_| "native transform reply unavailable or timed out".to_string())?
        })
    }

    pub(super) fn start(
        &mut self,
        owner: &Session,
        run: runtime_collab::TaskRunEnvelope,
    ) -> anyhow::Result<()> {
        // Re-delivery of a live lease must not start a second planner. A later
        // delivery after completion still uses the normal durable resume path.
        if self
            .workers
            .values()
            .any(|worker| worker.run_id == run.run_id || worker.task_id == run.task_id)
        {
            return Ok(());
        }
        self.next_worker = self
            .next_worker
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("task worker ids exhausted"))?;
        let worker = self.next_worker;
        self.owner_wake = owner.link.wake_handle().map(std::sync::Arc::new);
        let owner_wake = self.owner_wake.clone();
        let model_wake = self.owner_wake.clone();
        let (events, rx) = mpsc::channel();
        let wake = std::sync::Arc::new(std::sync::OnceLock::new());
        let worker_wake = std::sync::Arc::clone(&wake);
        let mailbox = Mailbox {
            worker,
            requests: self.tx.clone(),
            events: rx,
            connected: true,
            owner_wake: self.owner_wake.clone(),
        };
        let cfg = owner.cfg.clone();
        let selection = owner.chooser.selection().cloned();
        let managed = owner.managed;
        let model_enabled = matches!(run.planning, runtime_collab::TaskRunPlanning::Model);
        let surfaces = owner.broker_surfaces.clone();
        let awaiting_companions = owner.awaiting_companions.clone();
        let tx = self.tx.clone();
        let run_id = run.run_id;
        let model_tx = self.tx.clone();
        let scope = crate::model::native::Scope::new(
            run_id,
            run.lease.clone(),
            move |run_id, lease, request, timeout| {
                let (reply, rx) = mpsc::sync_channel(1);
                model_tx
                    .send(Message::Model(
                        worker,
                        ModelCall {
                            invocation_id: None,
                            run_id,
                            lease,
                            request,
                            timeout,
                            reply,
                        },
                    ))
                    .map_err(|_| "task connection owner stopped".to_string())?;
                notify(&model_wake);
                rx.recv_timeout(timeout)
                    .map_err(|_| "native model reply unavailable or timed out".to_string())?
            },
        );
        let task_id = run.task_id.clone();
        std::thread::Builder::new()
            .name(format!("agent-task-{run_id}"))
            .spawn(move || {
                let _completion = Completion(worker, tx, owner_wake);
                // Construct thread-bound collaboration and model facades here,
                // not on the owner thread and then moved across their bindings.
                let chooser = match selection {
                    Some(selection) => crate::model::Chooser::with_provider(
                        model_enabled, cfg.model_timeout, selection,
                    ),
                    None => crate::model::Chooser::new(false, cfg.model_timeout),
                };
                let chooser = chooser.with_native_model(scope.clone());
                let mut session = Session {
                    native_model: Some(scope),
                    cfg,
                    link: Link::for_task_waking(mailbox, |handle| {
                        let _ = worker_wake.set(handle);
                    }),
                    chooser,
                    managed,
                    protected_model: true,
                    task_deadline: None,
                    native_task: None,
                    broker_surfaces: surfaces,
                    next_broker_request: 1,
                    awaiting_companions,
                    broker_revision: 0,
                    catalogue_cache: None,
                    turn: None,
                    memory: super::agent_loop::Memory::default(),
                    outstanding: super::outstanding::Outstanding::default(),
                };
                if let Err(error) = session.run_task(&run) {
                    tracing::warn!(run_id, %error, "native task stopped before completion");
                    if let Err(report_error) = session.fail_task(&run, &error.to_string()) {
                        tracing::warn!(run_id, %report_error, "native task review report was refused");
                    }
                }
            })?;
        self.workers.insert(
            worker,
            Worker {
                run_id,
                task_id,
                events,
                wake,
            },
        );
        Ok(())
    }

    /// Forward queued requests without ever waiting for one task's reply.
    pub(super) fn pump(&mut self, owner: &mut Session) -> anyhow::Result<()> {
        if self.owner_wake.is_none() {
            self.owner_wake = owner.link.wake_handle().map(std::sync::Arc::new);
        }
        while let Ok(message) = self.rx.try_recv() {
            match message {
                Message::Finished(worker) => {
                    self.workers.remove(&worker);
                    self.replies.retain(|_, (id, _)| *id != worker);
                    self.model_replies.retain(|_, (id, _)| *id != worker);
                    self.invocations.retain(|_, id| *id != worker);
                }
                Message::Model(worker, call) => {
                    if call.invocation_id.is_none()
                        && !self
                            .workers
                            .get(&worker)
                            .is_some_and(|state| state.run_id == call.run_id)
                    {
                        let _ = call
                            .reply
                            .send(Err("native model task is no longer active".into()));
                        continue;
                    }
                    let request_id = owner.next_request_id();
                    let request = CollabRequest::NativeModel {
                        request_id,
                        invocation_id: call.invocation_id,
                        run_id: call.run_id,
                        lease: call.lease,
                        request: call.request,
                        timeout_ms: u64::try_from(call.timeout.as_millis()).unwrap_or(u64::MAX),
                    };
                    match owner.link.send(&request) {
                        Ok(()) => {
                            self.model_replies.insert(request_id, (worker, call.reply));
                        }
                        Err(error) => {
                            let _ = call.reply.send(Err(error.to_string()));
                        }
                    }
                }
                Message::Request(worker, mut request) => {
                    if !self.workers.contains_key(&worker) {
                        continue;
                    }
                    if let Some(local) = request_id(&mut request) {
                        let original = *local;
                        *local = owner.next_request_id();
                        self.replies.insert(*local, (worker, original));
                    }
                    if let CollabRequest::InvokeTaskAction { invocation, .. } = &*request {
                        self.invocations.insert(*invocation, worker);
                    }
                    owner.link.send(&request)?;
                }
            }
        }
        Ok(())
    }

    /// Deliver correlated events only to their task, never to another planner.
    pub(super) fn route(&mut self, event: &CollabEvent) -> bool {
        if let CollabEvent::NativeModelResult { request_id, result } = event {
            if let Some((_, reply)) = self.model_replies.remove(request_id) {
                let _ = reply.send(result.clone());
                return true;
            }
        }
        // A control-stream notification is task-addressed, not an RPC reply.
        if let CollabEvent::TaskRunControl {
            request_id: 0,
            control,
        } = event
        {
            if let Some(worker) = self
                .workers
                .values()
                .find(|worker| worker.task_id == control.task_id)
            {
                worker.deliver(event.clone());
                return true;
            }
            return false;
        }
        let mut event = event.clone();
        let control = matches!(event, CollabEvent::TaskRunControl { .. });
        if let Some(id) = reply_id(&mut event) {
            let Some(&(worker, original)) = self.replies.get(id) else {
                return false;
            };
            // A cancellation poll can yield control followed by its boolean
            // answer on the same request id, so control does not consume it.
            if !control {
                self.replies.remove(id);
            }
            *id = original;
            if let Some(worker) = self.workers.get(&worker) {
                worker.deliver(event);
            }
            return true;
        }
        match &event {
            CollabEvent::ActionCatalogue { .. } => {
                for worker in self.workers.values() {
                    worker.deliver(event.clone());
                }
                false // the owner retains the current catalogue for new tasks
            }
            CollabEvent::RemoteAction {
                message: tilo_actions::remote::RemoteActionMessage::Update { invocation, .. },
            } => {
                let Some(worker) = self
                    .invocations
                    .get(invocation)
                    .and_then(|id| self.workers.get(id))
                else {
                    return false;
                };
                worker.deliver(event);
                true
            }
            _ => false,
        }
    }

    pub(super) fn receive_wait(&self) -> Duration {
        // Both broker events and worker requests wake the connection owner.
        // A waiting child must not turn the dispatcher into a 1 ms poll loop.
        Duration::from_millis(100)
    }
}

fn request_id(request: &mut CollabRequest) -> Option<&mut u64> {
    match request {
        CollabRequest::NativeModel { request_id, .. }
        | CollabRequest::InvokeTaskAction { request_id, .. }
        | CollabRequest::TaskRunCancellation { request_id, .. } => Some(request_id),
        CollabRequest::ReportTaskRun { request_id, .. } => request_id.as_mut(),
        _ => None,
    }
}

fn reply_id(event: &mut CollabEvent) -> Option<&mut u64> {
    match event {
        CollabEvent::NativeModelResult { request_id, .. }
        | CollabEvent::TaskActionInvocationResult { request_id, .. }
        | CollabEvent::TaskRunCancellation { request_id, .. }
        | CollabEvent::TaskRunControl { request_id, .. }
        | CollabEvent::TaskRunReportResult { request_id, .. } => Some(request_id),
        _ => None,
    }
}
