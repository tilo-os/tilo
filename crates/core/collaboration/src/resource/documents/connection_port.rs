//! The workspace's one store as a native process reaches it: the record commands
//! ([`super::commands`]) on the route its app joined in the workspace ([`crate::resource_command`]).
//!
//! A read never holds a window's thread for the workspace. It waits a few milliseconds, which a
//! healthy service on the device answers in, so the window's first frame has its record. A read
//! the workspace has not answered by then (its service still starting, a large workspace loading)
//! answers its records as on their way ([`DocumentRead::Pending`]) and goes on off the thread,
//! asked again while the workspace says the record is on its way. Once the answer comes the next
//! read of the same records takes it, and what was waiting for it is woken
//! ([`DocumentPort::when_answered`]).

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{LazyLock, Mutex};

use collab_types::store::wire::{Command, Reply};
use tilo_lock::MutexRecover as _;

use crate::time::Duration;

use super::{DocumentAsk, DocumentPort, DocumentRead, ReadyWake, Written, commands};

/// How long a read waits for the workspace before it answers its records as on their way: a few
/// round trips to the service on this device, so a healthy one answers within the frame and a
/// window never shows its record arriving only to show it a moment later.
const FIRST_WAIT: Duration = Duration::from_millis(4);

/// How long a read the workspace answered as on its way waits before it asks again, at first and
/// at most: a workspace that is opening answers within the first few.
const ASK_AGAIN: (Duration, Duration) = (Duration::from_millis(50), Duration::from_secs(2));

/// The store over this process's workspace connection.
#[derive(Clone, Copy, Debug, Default)]
pub struct ConnectionDocuments;

impl ConnectionDocuments {
    /// The port, when this process has a workspace connection to send on.
    #[must_use]
    pub fn connected() -> Option<Self> {
        crate::has_resource_connection().then_some(Self)
    }
}

type Answer = Result<Vec<(String, DocumentRead)>, String>;

/// A read by an app in a workspace: the reads it asked, `None` for its listing.
type Asked = (String, String, Option<Vec<DocumentAsk>>);

/// What this process's connection has on its way, the one connection every store of the process
/// sends on.
#[derive(Default)]
struct Reads {
    /// Each read the workspace has not answered (`None`), or whose answer no read has taken yet.
    /// A listing's answer stays as what it last listed.
    asked: HashMap<Asked, Option<Answer>>,
    /// What is woken once something on its way comes in.
    wakes: Vec<ReadyWake>,
}

static READS: LazyLock<Mutex<Reads>> = LazyLock::new(Mutex::default);

/// `command` to `app`'s store in `workspace`, answered once the workspace answers.
fn ask(app: &str, workspace: &str, command: &Command) -> Result<Reply, String> {
    let cluster = uuid::Uuid::parse_str(workspace)
        .map_err(|_| format!("{workspace} does not name a workspace"))?;
    let command = serde_json::to_vec(command).map_err(|error| error.to_string())?;
    let answer = crate::resource_command(app, cluster, command)?
        .recv()
        .unwrap_or_else(|_| Err(closed()))?;
    serde_json::from_slice(&answer).map_err(|error| error.to_string())
}

fn closed() -> String {
    "the workspace connection closed before it answered".to_owned()
}

/// `asked`, read off the window's thread: asked again while the workspace says a record of it is
/// on its way, then answered on the returned receiver.
fn reading(asked: Asked) -> Result<Receiver<Answer>, String> {
    let (answered, receiver) = std::sync::mpsc::sync_channel(1);
    tilo_rt::background::Background::current()
        .spawn("record-read", move || {
            let (app, workspace, reads) = asked;
            let mut wait = ASK_AGAIN.0;
            let answer = loop {
                let answer =
                    commands::answer(&|command| ask(&app, &workspace, &command), reads.clone());
                let arriving = answer.as_ref().is_ok_and(|read| {
                    read.iter()
                        .any(|(_, read)| matches!(read, DocumentRead::Pending))
                });
                if !arriving {
                    break answer;
                }
                std::thread::park_timeout(wait);
                wait = (wait * 2).min(ASK_AGAIN.1);
            };
            let _ = answered.send(answer);
        })
        .map_err(|error| format!("the workspace's answer cannot be awaited: {error}"))?;
    Ok(receiver)
}

/// What a read answers while its answer is on its way: each record on its way; a listing what it
/// last listed (nothing before its first), since every list of the app's records reads it on each
/// pass, and one that answered nothing while asked would empty every list for a moment.
fn on_its_way(asked: &Asked, last: Option<&Answer>) -> Answer {
    match &asked.2 {
        Some(reads) => Ok(reads
            .iter()
            .map(|wanted| (wanted.id.clone(), DocumentRead::Pending))
            .collect()),
        None => Ok(last.and_then(|last| last.clone().ok()).unwrap_or_default()),
    }
}

fn read(asked: Asked) -> Answer {
    let listing = asked.2.is_none();
    {
        let mut held = READS.lock_recover();
        if let Some(answer) = held.asked.get_mut(&asked) {
            return match answer.take() {
                Some(answer) => {
                    held.asked.remove(&asked);
                    if listing {
                        // The listing is asked again; this answer stands until the next one comes.
                        drop(held);
                        return start(asked, Some(answer));
                    }
                    answer
                }
                None => on_its_way(&asked, None),
            };
        }
    }
    start(asked, None)
}

/// Ask `asked` now, answering it within [`FIRST_WAIT`] or as on its way (`last` standing for a
/// listing), and keep its answer for the next read once it comes.
fn start(asked: Asked, last: Option<Answer>) -> Answer {
    let receiver = reading(asked.clone())?;
    match receiver.recv_timeout(FIRST_WAIT) {
        Ok(answer) => return answer,
        Err(RecvTimeoutError::Disconnected) => return Err(closed()),
        Err(RecvTimeoutError::Timeout) => {}
    }
    let answer = on_its_way(&asked, last.as_ref());
    READS.lock_recover().asked.insert(asked.clone(), None);
    let kept = tilo_rt::background::Background::current().spawn("record-answer", move || {
        let answer = receiver.recv().unwrap_or_else(|_| Err(closed()));
        let wakes = {
            let mut reads = READS.lock_recover();
            let changed = last.as_ref() != Some(&answer);
            reads.asked.insert(asked, Some(answer));
            if changed {
                std::mem::take(&mut reads.wakes)
            } else {
                Vec::new()
            }
        };
        for wake in wakes {
            wake();
        }
    });
    kept.map_err(|error| format!("the workspace's answer cannot be awaited: {error}"))?;
    answer
}

impl DocumentPort for ConnectionDocuments {
    fn read(
        &self,
        app: &str,
        workspace: &str,
        reads: Option<Vec<DocumentAsk>>,
    ) -> Result<Vec<(String, DocumentRead)>, String> {
        read((app.to_owned(), workspace.to_owned(), reads))
    }

    fn when_answered(&self, wake: ReadyWake) -> bool {
        let mut held = READS.lock_recover();
        let on_its_way = held.asked.values().any(Option::is_none);
        if on_its_way {
            held.wakes.push(wake);
        }
        on_its_way
    }

    fn write(
        &self,
        app: &str,
        workspace: &str,
        id: &str,
        _kind: &str,
        changes: Vec<u8>,
    ) -> Written {
        let (answered, receiver) = std::sync::mpsc::sync_channel(1);
        let (app, workspace, id) = (app.to_owned(), workspace.to_owned(), id.to_owned());
        let sent = tilo_rt::background::Background::current()
            .spawn("record-write", move || {
                let _ = answered.send(commands::write(
                    &|command| ask(&app, &workspace, &command),
                    &id,
                    &changes,
                ));
            })
            .map_err(|error| format!("the write cannot be sent: {error}"));
        let mut waiting = match sent {
            Ok(()) => Some(receiver),
            Err(error) => {
                let mut failed = Some(Err(error));
                return Box::new(move |_| failed.take());
            }
        };
        Box::new(move |wait| {
            let answer = match waiting.as_ref()?.recv_timeout(wait) {
                Ok(answer) => answer,
                Err(RecvTimeoutError::Timeout) => return None,
                Err(RecvTimeoutError::Disconnected) => Err(closed()),
            };
            waiting = None;
            Some(answer)
        })
    }
}
