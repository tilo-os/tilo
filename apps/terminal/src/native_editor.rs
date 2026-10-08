//! Ephemeral editor metadata and acknowledged literal edits. Never process stdin.
use runtime_portal::editor::{self, Lease, Request, Status};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use ui::framework::EffectReply;
use ui::webview::text_input::{Failure, Receipt, Resolver};

#[derive(Clone)]
pub(crate) struct Snapshot {
    pub lease: Lease,
    pub can_undo: bool,
    pub can_redo: bool,
    pub transaction: u64,
}

pub(crate) struct Capture {
    pub lease: Lease,
    epoch: u64,
    active: Receipt,
}

enum Answer {
    Ime {
        receipt: Receipt,
        resolver: Resolver,
        capture: Receipt,
    },
    Effect(Box<EffectReply>),
    Observe,
}
impl Answer {
    fn send(&self, socket: &editor::Socket, bytes: &[u8]) -> Result<(), Failure> {
        match self {
            Self::Ime {
                receipt, capture, ..
            } => {
                if receipt.was_canceled() || capture.was_canceled() || receipt.poll().is_some() {
                    return Err(Failure::Rejected);
                }
                socket.send(bytes).map_err(|_| Failure::Unknown)
            }
            Self::Effect(reply) => reply
                .progress()
                .with_effect(|| Ok(socket.send(bytes)))
                .map_err(|_| Failure::Rejected)?
                .map_err(|_| Failure::Unknown),
            Self::Observe => socket.send(bytes).map_err(|_| Failure::Retired),
        }
    }
    fn finish(self, result: Result<(), Failure>) {
        match self {
            Self::Ime { resolver, .. } => resolver.complete(result.map(|()| None)),
            Self::Effect(reply) => {
                (*reply).answer(
                    result
                        .map(|()| ui::tilo_actions::ActionOutput::default())
                        .map_err(|failure| ui::tilo_actions::ActionFailure {
                            code: if failure == Failure::Unknown {
                                "native_edit_uncertain"
                            } else {
                                "native_edit_refused"
                            }
                            .into(),
                            message: if failure == Failure::Unknown {
                                "The original editor did not acknowledge this edit"
                            } else {
                                "The original editor lease is no longer available"
                            }
                            .into(),
                            details: None,
                            retryable: false,
                        }),
                );
            }
            Self::Observe => {}
        }
    }
}
struct Command {
    epoch: u64,
    operation: Request,
    expected: Status,
    answer: Answer,
}
struct Pending {
    request: u64,
    transaction: u64,
    expected: Status,
    answer: Answer,
    deadline: Instant,
}

pub(crate) struct Client {
    socket: Arc<editor::Socket>,
    queue: SyncSender<Command>,
    wake: Mutex<UnixStream>,
    epoch: Arc<AtomicU64>,
    snapshot: Arc<Mutex<Option<Snapshot>>>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Client {
    fn drop(&mut self) {
        self.socket.retire();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
impl Client {
    pub fn new(socket: editor::Socket, waker: Arc<Mutex<ui::WakeHandle>>) -> io::Result<Self> {
        let socket = Arc::new(socket);
        let epoch = Arc::new(AtomicU64::new(1));
        let snapshot = Arc::new(Mutex::new(None));
        let (queue, receiver) = mpsc::sync_channel(4);
        let (wake, input) = UnixStream::pair()?;
        wake.set_nonblocking(true)?;
        input.set_nonblocking(true)?;
        let worker_socket = Arc::clone(&socket);
        let worker_epoch = Arc::clone(&epoch);
        let worker_snapshot = Arc::clone(&snapshot);
        let worker = std::thread::Builder::new()
            .name("terminal-native-editor".into())
            .spawn(move || {
                let mut worker = Worker {
                    socket: worker_socket,
                    epoch: worker_epoch,
                    snapshot: worker_snapshot,
                    receiver,
                    input,
                    waker,
                    pending: None,
                    next: 1,
                    transaction: 0,
                };
                let _ = worker.run();
                if let Some(pending) = worker.pending.take() {
                    pending.answer.finish(Err(Failure::Unknown));
                }
                for command in worker.receiver.try_iter() {
                    command.answer.finish(Err(Failure::Retired));
                }
                worker.clear();
                worker.socket.retire();
            })?;
        Ok(Self {
            socket,
            queue,
            wake: Mutex::new(wake),
            epoch,
            snapshot,
            worker: Some(worker),
        })
    }
    pub fn snapshot(&self) -> Option<Snapshot> {
        if self.epoch.load(Ordering::Acquire) == u64::MAX {
            return None;
        }
        let snapshot = self.snapshot.lock().ok()?.clone()?;
        (monotonic_ns()? < snapshot.lease.expires_ns).then_some(snapshot)
    }
    fn enqueue(&self, command: Command) {
        if let Err(error) = self.queue.try_send(command) {
            let (command, failure) = match error {
                mpsc::TrySendError::Full(command) => (command, Failure::Busy),
                mpsc::TrySendError::Disconnected(command) => (command, Failure::Retired),
            };
            command.answer.finish(Err(failure));
            return;
        }
        if let Ok(mut wake) = self.wake.lock() {
            if let Err(error) = wake.write_all(&[1])
                && error.kind() != io::ErrorKind::WouldBlock
            {
                self.socket.retire();
            }
        } else {
            self.socket.retire();
        }
    }
    pub fn revoke(&self) {
        let next = self
            .epoch
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |epoch| {
                Some(epoch.saturating_add(1))
            })
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        if let Ok(mut snapshot) = self.snapshot.lock() {
            *snapshot = None;
        }
        self.enqueue(Command {
            epoch: next,
            operation: Request::Revoke,
            expected: Status::Revoked,
            answer: Answer::Observe,
        });
    }
    pub fn capture(&self) -> Option<(Capture, Receipt)> {
        let snapshot = self.snapshot()?;
        let (active, resolver) = Receipt::channel();
        let capture = Capture {
            lease: snapshot.lease,
            epoch: self.epoch.load(Ordering::Acquire),
            active: active.clone(),
        };
        resolver.complete(Ok(None));
        Some((capture, active))
    }
    pub fn commit(&self, capture: Capture, text: String) -> Receipt {
        let (receipt, resolver) = Receipt::channel();
        self.enqueue(Command {
            epoch: capture.epoch,
            operation: Request::Replace {
                lease: capture.lease,
                transaction: 1,
                text,
            },
            expected: Status::Applied,
            answer: Answer::Ime {
                receipt: receipt.clone(),
                resolver,
                capture: capture.active,
            },
        });
        receipt
    }
    pub fn effect(&self, operation: Request, expected: Status, reply: EffectReply) {
        self.enqueue(Command {
            epoch: self.epoch.load(Ordering::Acquire),
            operation,
            expected,
            answer: Answer::Effect(Box::new(reply)),
        });
    }
    pub fn history(&self, direction: ui::undo::UndoRequest) -> Receipt {
        let Some(snapshot) = self.snapshot() else {
            return Receipt::failed(Failure::Rejected);
        };
        let allowed = match direction {
            ui::undo::UndoRequest::Undo => snapshot.can_undo,
            ui::undo::UndoRequest::Redo => snapshot.can_redo,
        };
        if !allowed {
            return Receipt::failed(Failure::Rejected);
        }
        let operation = match direction {
            ui::undo::UndoRequest::Undo => Request::Undo {
                lease: snapshot.lease,
                transaction: snapshot.transaction,
            },
            ui::undo::UndoRequest::Redo => Request::Redo {
                lease: snapshot.lease,
                transaction: snapshot.transaction,
            },
        };
        let (receipt, resolver) = Receipt::channel();
        let (capture, acknowledged) = Receipt::channel();
        acknowledged.complete(Ok(None));
        self.enqueue(Command {
            epoch: self.epoch.load(Ordering::Acquire),
            operation,
            expected: match direction {
                ui::undo::UndoRequest::Undo => Status::Undone,
                ui::undo::UndoRequest::Redo => Status::Redone,
            },
            answer: Answer::Ime {
                receipt: receipt.clone(),
                resolver,
                capture,
            },
        });
        receipt
    }
}

fn monotonic_ns() -> Option<u64> {
    let mut clock = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: properly sized initialized output and a supported clock identifier.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &raw mut clock) } != 0 {
        return None;
    }
    u64::try_from(clock.tv_sec)
        .ok()?
        .checked_mul(1_000_000_000)?
        .checked_add(u64::try_from(clock.tv_nsec).ok()?)
}
struct Worker {
    socket: Arc<editor::Socket>,
    epoch: Arc<AtomicU64>,
    snapshot: Arc<Mutex<Option<Snapshot>>>,
    receiver: Receiver<Command>,
    input: UnixStream,
    waker: Arc<Mutex<ui::WakeHandle>>,
    pending: Option<Pending>,
    next: u64,
    transaction: u64,
}
impl Worker {
    fn clear(&self) {
        if let Ok(mut snapshot) = self.snapshot.lock() {
            *snapshot = None;
        }
        crate::inner::request_terminal_wake(&self.waker);
    }
    fn start_next(&mut self) -> io::Result<()> {
        while self.pending.is_none() {
            let Ok(mut command) = self.receiver.try_recv() else {
                break;
            };
            if command.epoch != self.epoch.load(Ordering::Acquire) || command.epoch == u64::MAX {
                command.answer.finish(Err(Failure::Rejected));
                continue;
            }
            let request = self.next;
            self.next = self
                .next
                .checked_add(1)
                .ok_or_else(|| io::Error::other("native request identity exhausted"))?;
            let transaction = match &mut command.operation {
                Request::Replace { transaction, .. } => {
                    *transaction = request;
                    request
                }
                Request::Undo { transaction, .. }
                | Request::Redo { transaction, .. }
                | Request::Query { transaction, .. } => *transaction,
                Request::Inspect | Request::Revoke => 0,
            };
            let Ok(bytes) = editor::encode(request, &command.operation) else {
                command.answer.finish(Err(Failure::Rejected));
                continue;
            };
            if let Err(failure) = command.answer.send(&self.socket, &bytes) {
                command.answer.finish(Err(failure));
                if failure == Failure::Unknown {
                    return Err(io::Error::other("uncertain native edit delivery"));
                }
                continue;
            }
            self.clear();
            self.pending = Some(Pending {
                request,
                transaction,
                expected: command.expected,
                answer: command.answer,
                deadline: Instant::now()
                    .checked_add(Duration::from_secs(10))
                    .ok_or_else(|| io::Error::other("native deadline overflow"))?,
            });
        }
        Ok(())
    }
    fn receive(&mut self) -> io::Result<()> {
        let mut bytes = [0; editor::FRAME_BYTES];
        let size = self.socket.receive(&mut bytes)?;
        let reply = editor::decode(
            bytes
                .get(..size)
                .ok_or_else(|| io::Error::other("native packet bound"))?,
        )?;
        if reply.request != 0 {
            let pending = self
                .pending
                .take()
                .ok_or_else(|| io::Error::other("unsolicited native receipt"))?;
            if reply.request != pending.request || reply.transaction != pending.transaction {
                pending.answer.finish(Err(Failure::Unknown));
                return Err(io::Error::other("wrong native receipt"));
            }
            let result = if reply.status == pending.expected {
                Ok(())
            } else if reply.status == Status::Unknown {
                Err(Failure::Unknown)
            } else {
                Err(Failure::Rejected)
            };
            if result.is_ok() && pending.transaction != 0 {
                self.transaction = pending.transaction;
            }
            pending.answer.finish(result);
            if reply.status == Status::Ready {
                self.observe(reply);
                return Ok(());
            }
        } else if self.pending.is_none() {
            self.observe(reply);
            return Ok(());
        }
        crate::inner::request_terminal_wake(&self.waker);
        Ok(())
    }
    fn observe(&self, reply: editor::Reply) {
        if let Ok(mut snapshot) = self.snapshot.lock() {
            *snapshot = reply.lease.map(|lease| Snapshot {
                lease,
                can_undo: reply.can_undo && self.transaction != 0,
                can_redo: reply.can_redo && self.transaction != 0,
                transaction: self.transaction,
            });
        }
        crate::inner::request_terminal_wake(&self.waker);
    }
    fn timeout(&self) -> i32 {
        if let Some(pending) = &self.pending {
            return i32::try_from(
                pending
                    .deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis(),
            )
            .unwrap_or(10000);
        }
        self.snapshot
            .lock()
            .ok()
            .and_then(|snapshot| snapshot.as_ref().map(|snapshot| snapshot.lease.expires_ns))
            .and_then(|expiry| monotonic_ns().map(|now| expiry.saturating_sub(now) / 1_000_000))
            .and_then(|millis| i32::try_from(millis).ok())
            .unwrap_or(-1)
    }
    fn refresh(&mut self) -> io::Result<()> {
        let request = self.next;
        self.next = self
            .next
            .checked_add(1)
            .ok_or_else(|| io::Error::other("native request identity exhausted"))?;
        self.socket
            .send(&editor::encode(request, &Request::Inspect)?)?;
        self.clear();
        self.pending = Some(Pending {
            request,
            transaction: 0,
            expected: Status::Ready,
            answer: Answer::Observe,
            deadline: Instant::now()
                .checked_add(Duration::from_secs(10))
                .ok_or_else(|| io::Error::other("native deadline overflow"))?,
        });
        Ok(())
    }
    fn run(&mut self) -> io::Result<()> {
        loop {
            self.start_next()?;
            if self
                .pending
                .as_ref()
                .is_some_and(|pending| Instant::now() >= pending.deadline)
            {
                return Err(io::Error::other("native acknowledgement deadline"));
            }
            let timeout = self.timeout();
            let mut descriptors = [
                libc::pollfd {
                    fd: self.socket.as_fd().as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: self.input.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            // SAFETY: two initialized owned descriptors and a bounded pending deadline.
            let ready = unsafe { libc::poll(descriptors.as_mut_ptr(), 2, timeout) };
            if ready == 0 {
                if self.pending.is_some() {
                    return Err(io::Error::other("native acknowledgement deadline"));
                }
                self.refresh()?;
                continue;
            }
            if ready < 0 {
                if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(io::Error::last_os_error());
            }
            let [editor, wake] = descriptors;
            if (editor.revents | wake.revents) & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL)
                != 0
            {
                return Ok(());
            }
            if editor.revents & libc::POLLIN != 0 {
                self.receive()?;
            }
            if wake.revents & libc::POLLIN != 0 {
                let mut bytes = [0; 64];
                let _ = self.input.read(&mut bytes);
            }
        }
    }
}
