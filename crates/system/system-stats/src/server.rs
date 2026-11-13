//! The service's socket: one thread accepts, one thread per connection
//! reads its requests, and one writes what the hub pushes.

use std::collections::HashMap;
use std::io::BufReader;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};

use rustix::event::{PollFd, PollFlags, poll};

use crate::hub::{Hub, Spec, Subscription};
use crate::protocol::{
    Grant, Grants, Handoff, Refusal, Reply, Request, read_handoff, read_message, write_message,
};

const REPLY_QUEUE: usize = 4;

pub trait Prompter: Send + Sync {
    /// Whether the person already allowed `grant` for this app in this room
    /// (a choice made earlier, kept as they asked).
    fn granted(&self, handoff: &Handoff, grant: Grant) -> bool;

    /// Put the permission prompt in front of the person: whether they allowed
    /// it, once they answered. The asking connection's thread waits for it,
    /// never the service, nor whoever puts the prompt up.
    fn ask(&self, handoff: &Handoff, grant: Grant) -> mpsc::Receiver<bool>;
}

#[must_use]
pub fn answered(allowed: bool) -> mpsc::Receiver<bool> {
    let (answer, answered) = mpsc::channel();
    let _ = answer.send(allowed);
    answered
}

pub struct NoPrompter;

impl Prompter for NoPrompter {
    fn granted(&self, _handoff: &Handoff, _grant: Grant) -> bool {
        false
    }

    fn ask(&self, _handoff: &Handoff, _grant: Grant) -> mpsc::Receiver<bool> {
        answered(false)
    }
}

pub struct Server {
    path: PathBuf,
    /// The socket as this server left it: another server may have replaced
    /// it since, and its socket is not this one's to unlink.
    socket: (u64, u64),
    wake: Option<UnixStream>,
    acceptor: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    pub fn bind(path: &Path, hub: Hub, prompter: Arc<dyn Prompter>) -> std::io::Result<Self> {
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        let listener = UnixListener::bind(path)?;
        let socket = socket_of(path)?;
        // The acceptor waits on its own socket and this pair, never on the
        // path: a server that took the path over must not keep this one's
        // acceptor waiting.
        let (wake, waking) = UnixStream::pair()?;
        let acceptor = std::thread::Builder::new()
            .name("tilo-system-stats-accept".into())
            .spawn(move || {
                let mut ready = [
                    PollFd::new(&listener, PollFlags::IN),
                    PollFd::new(&waking, PollFlags::IN),
                ];
                loop {
                    match poll(&mut ready, None) {
                        Ok(0) | Err(rustix::io::Errno::INTR) => continue,
                        Ok(_) => {}
                        Err(error) => {
                            tracing::warn!(%error, "system information service stopped accepting");
                            break;
                        }
                    }
                    if ready[1]
                        .revents()
                        .intersects(PollFlags::IN | PollFlags::HUP | PollFlags::ERR)
                    {
                        break;
                    }
                    if !ready[0].revents().intersects(PollFlags::IN) {
                        continue;
                    }
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let hub = hub.clone();
                            let prompter = Arc::clone(&prompter);
                            let spawned = std::thread::Builder::new()
                                .name("tilo-system-stats-conn".into())
                                .spawn(move || serve_handed_off(stream, &hub, prompter.as_ref()));
                            if let Err(error) = spawned {
                                tracing::warn!(%error, "system information connection dropped");
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(error) => {
                            tracing::warn!(%error, "system information service stopped accepting");
                            break;
                        }
                    }
                }
            })?;
        Ok(Self {
            path: path.to_path_buf(),
            socket,
            wake: Some(wake),
            acceptor: Some(acceptor),
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        drop(self.wake.take());
        if let Some(acceptor) = self.acceptor.take() {
            let _ = acceptor.join();
        }
        if socket_of(&self.path).is_ok_and(|socket| socket == self.socket) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn socket_of(path: &Path) -> std::io::Result<(u64, u64)> {
    let metadata = std::fs::metadata(path)?;
    Ok((metadata.dev(), metadata.ino()))
}

pub fn serve_handed_off(stream: UnixStream, hub: &Hub, prompter: &dyn Prompter) {
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(read) => read,
        Err(error) => {
            tracing::warn!(%error, "system information connection unusable");
            return;
        }
    });
    let handoff = match read_handoff(&mut reader) {
        Ok(handoff) => handoff,
        Err(error) => {
            tracing::warn!(%error, "system information connection without a launcher handoff");
            return;
        }
    };
    serve(reader, stream, &handoff, hub, prompter);
}

/// Serve requests from `reader`, answering on `writer`, for the app the
/// handoff names.
pub fn serve<R: std::io::Read>(
    mut reader: R,
    writer: UnixStream,
    handoff: &Handoff,
    hub: &Hub,
    prompter: &dyn Prompter,
) {
    // What the launcher granted, plus what the person allowed this app
    // earlier.
    let mut grants = handoff.grants.within(handoff.ceiling);
    for grant in Grant::ALL {
        if !grants.holds(grant)
            && handoff.ceiling.with(grant) == handoff.ceiling
            && prompter.granted(handoff, grant)
        {
            grants = grants.with(grant);
        }
    }
    // A few replies in flight. A snapshot that finds the queue full is
    // dropped rather than queued: the next one supersedes it, and an app
    // that stops reading must not make the session hold every reading it
    // missed.
    let (replies, outgoing) = mpsc::sync_channel::<Reply>(REPLY_QUEUE);
    let writer_thread = std::thread::Builder::new()
        .name("tilo-system-stats-write".into())
        .spawn(move || {
            let mut writer = writer;
            for reply in outgoing {
                if write_message(&mut writer, &reply).is_err() {
                    break;
                }
            }
            let _ = writer.shutdown(std::net::Shutdown::Both);
        });
    if let Err(error) = writer_thread {
        tracing::warn!(%error, "system information connection dropped");
        return;
    }
    let mut connection = Connection {
        handoff,
        grants,
        subscriptions: HashMap::new(),
        replies,
        hub,
    };
    loop {
        match read_message::<_, Request>(&mut reader) {
            Ok(Some(request)) => connection.answer(request, prompter),
            Ok(None) => break,
            Err(error) => {
                tracing::debug!(%error, app = %handoff.app_id, "system information request unreadable");
                break;
            }
        }
    }
}

struct Connection<'a> {
    handoff: &'a Handoff,
    grants: Grants,
    subscriptions: HashMap<u64, Subscription>,
    replies: mpsc::SyncSender<Reply>,
    hub: &'a Hub,
}

impl Connection<'_> {
    fn reply(&self, reply: Reply) {
        let _ = self.replies.send(reply);
    }

    fn answer(&mut self, request: Request, prompter: &dyn Prompter) {
        match request {
            Request::Subscribe {
                subscription,
                interval_ms,
                processes,
            } => self.subscribe(subscription, interval_ms, processes),
            Request::Unsubscribe { subscription } => {
                self.subscriptions.remove(&subscription);
            }
            Request::AskGrant { request, grant } => {
                let declared = self.handoff.ceiling.with(grant) == self.handoff.ceiling;
                if declared
                    && !self.grants.holds(grant)
                    && prompter.ask(self.handoff, grant).recv().unwrap_or(false)
                {
                    // A command line is part of a process row: allowing it
                    // allows the list it belongs to.
                    self.grants = self.grants.with(grant);
                    if grant == Grant::CommandLines {
                        self.grants = self.grants.with(Grant::Processes);
                    }
                    self.grants = self.grants.within(self.handoff.ceiling);
                    for subscription in self.subscriptions.values() {
                        subscription.set_grants(self.grants);
                    }
                }
                if self.grants.holds(grant) {
                    self.reply(Reply::Grants {
                        request,
                        grants: self.grants,
                    });
                } else {
                    self.reply(Reply::Refused {
                        request,
                        refusal: Refusal::Declined,
                    });
                }
            }
            Request::EndApp {
                request,
                app,
                force,
            } => {
                let result = self
                    .require(Grant::Processes)
                    .and_then(|()| self.hub.end_app(&app, force));
                self.reply(match result {
                    Ok(ended) => Reply::Ended { request, ended },
                    Err(refusal) => Reply::Refused { request, refusal },
                });
            }
            Request::EndProcess {
                request,
                pid,
                started_at,
                force,
            } => {
                let result = self
                    .require(Grant::Processes)
                    .and_then(|()| self.hub.end_process(pid, started_at, force));
                self.reply(match result {
                    Ok(()) => Reply::Ended { request, ended: 1 },
                    Err(refusal) => Reply::Refused { request, refusal },
                });
            }
            Request::Storage { request } => {
                if let Err(refusal) = self.require(Grant::SystemInfo) {
                    self.reply(Reply::Refused { request, refusal });
                    return;
                }
                // Read on the hub's storage thread; this connection goes on
                // serving the app's other requests meanwhile.
                let replies = self.replies.clone();
                self.hub.read_storage(Box::new(move |result| {
                    let reply = match result {
                        Ok(storage) => Reply::Storage {
                            request,
                            storage: Box::new(storage),
                        },
                        Err(refusal) => Reply::Refused { request, refusal },
                    };
                    answer_without_waiting(&replies, reply);
                }));
            }
        }
    }

    fn require(&self, grant: Grant) -> Result<(), Refusal> {
        if self.grants.holds(grant) {
            Ok(())
        } else {
            Err(Refusal::NotGranted { grant })
        }
    }

    fn subscribe(&mut self, subscription: u64, interval_ms: u32, processes: bool) {
        if !self.grants.any() {
            self.reply(Reply::SubscriptionRefused {
                subscription,
                refusal: Refusal::NotGranted {
                    grant: Grant::SystemInfo,
                },
            });
            return;
        }
        let replies = self.replies.clone();
        let sink = Box::new(move |snapshot| {
            let _ = replies.try_send(Reply::Snapshot {
                subscription,
                snapshot: Box::new(snapshot),
            });
        });
        let live = self
            .hub
            .subscribe(Spec::every(interval_ms, processes), self.grants, sink);
        self.subscriptions.insert(subscription, live);
    }
}

/// Queue `reply` for a connection's writer without waiting on it: an app
/// that stops reading fills its queue, and the thread answering must not
/// wait for that app. A full queue hands the reply to a thread of its own,
/// which waits instead (and ends with the connection).
fn answer_without_waiting(replies: &mpsc::SyncSender<Reply>, reply: Reply) {
    match replies.try_send(reply) {
        Ok(()) | Err(mpsc::TrySendError::Disconnected(_)) => {}
        Err(mpsc::TrySendError::Full(reply)) => {
            let replies = replies.clone();
            let spawned = std::thread::Builder::new()
                .name("tilo-system-stats-answer".into())
                .spawn(move || {
                    let _ = replies.send(reply);
                });
            if let Err(error) = spawned {
                tracing::warn!(%error, "system information answer dropped");
            }
        }
    }
}
