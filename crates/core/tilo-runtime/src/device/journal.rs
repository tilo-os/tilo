//! The `journal` resource, as the daemon publishes it to the apps attached to this device: this
//! computer's systemd journal, read where no desktop session is there to read it (a server, or a
//! capture's disposable profile).
//!
//! The session's system service serves the same resource to the shell, from the same read
//! ([`tilo_system_resources::journal::read`]). A device with no session — the machine's daemon
//! alone, as `--run-app` starts an app — has nothing else to publish it, so the daemon does: the
//! reading every view follows, and the actions that read it again, tail it after a cursor, or
//! follow new records.
//!
//! A capture's profile names the journal it staged (`TILO_JOURNAL_FIXTURE`), so a picture of
//! Logs shows what the plan wrote and never this machine's own journal.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};
use tilo_actions::ActionOutput;
use tilo_app_host::services::{Caller, PublishedResources, Reading, ResourceWatch};
use tilo_lock::MutexRecover as _;
use tilo_system_resources::journal::{JOURNAL, Journal};
use tokio::sync::{mpsc, oneshot};

/// The most records the reading keeps: older ones leave as new ones arrive.
const KEPT: usize = 5000;
/// The records a read asks for when its action names none.
const DEFAULT_LIMIT: u32 = 500;
/// The records a follow reads each time it asks for new ones.
const FOLLOW_LIMIT: u32 = 200;

/// One app's watch of the journal: where its readings go, and the revision it was last sent.
struct Watch {
    id: ResourceWatch,
    sink: mpsc::UnboundedSender<runtime_collab::wire::Event>,
    revision: u64,
}

/// The journal as this device's daemon keeps it: what the last read found, and the revision every
/// watcher has been sent.
struct Served {
    reading: Journal,
    revision: u64,
}

/// What the daemon's provider shares with the threads that read the journal.
struct Inner {
    served: Mutex<Served>,
    watches: Mutex<Vec<Watch>>,
    next_watch: AtomicU64,
}

/// This device's journal, as the daemon publishes it to the apps attached to it.
pub struct Published {
    inner: Arc<Inner>,
}

impl Default for Published {
    fn default() -> Self {
        Self::new()
    }
}

impl Published {
    /// The provider, reading the journal once at its start so an app opened on this device finds
    /// it already there.
    #[must_use]
    pub fn new() -> Self {
        let inner = Arc::new(Inner {
            served: Mutex::new(Served {
                reading: Journal::default(),
                revision: 0,
            }),
            watches: Mutex::default(),
            next_watch: AtomicU64::new(1),
        });
        let reading = Arc::clone(&inner);
        let spawned = std::thread::Builder::new()
            .name("tilo-journal".into())
            .spawn(move || reading.read(None, DEFAULT_LIMIT, "refresh", true));
        if let Err(error) = spawned {
            tracing::warn!(%error, "the journal is not read at this device's start");
        }
        Self { inner }
    }
}

impl Inner {
    /// The reading as it stands, encoded for the wire.
    fn encoded(&self) -> (u64, Option<Vec<u8>>) {
        let served = self.served.lock_recover();
        let reading = serde_json::to_vec(&served.reading).ok();
        (served.revision, reading)
    }

    /// Tell every watcher the reading as it stands now, and move its revision.
    fn moved(&self) {
        let (revision, reading) = {
            let mut served = self.served.lock_recover();
            served.revision = served.revision.wrapping_add(1);
            let reading = serde_json::to_vec(&served.reading).ok();
            (served.revision, reading)
        };
        let mut watches = self.watches.lock_recover();
        watches.retain_mut(|watch| {
            if watch.revision == revision {
                return true;
            }
            watch.revision = revision;
            watch
                .sink
                .send(runtime_collab::wire::Event::Reading {
                    resource: JOURNAL.to_owned(),
                    revision,
                    reading: reading.clone(),
                })
                .is_ok()
        });
    }

    /// Start a read on a thread of its own, so the caller is answered at once and the reading
    /// moves when the journal answers: the records are replaced ([`replace`]) or continue them.
    fn start_read(
        self: &Arc<Self>,
        after: Option<String>,
        limit: u32,
        action: &str,
        replace: bool,
    ) {
        {
            let mut served = self.served.lock_recover();
            if !served.reading.pending.iter().any(|id| id == action) {
                served.reading.pending.push(action.to_owned());
            }
        }
        self.moved();
        let read = Arc::clone(self);
        let named = action.to_owned();
        let spawned = std::thread::Builder::new()
            .name("tilo-journal-read".into())
            .spawn(move || read.read(after, limit, &named, replace));
        if let Err(error) = spawned {
            tracing::warn!(%error, "the journal is not read");
            self.land(
                action,
                Err("The journal is not read on this device.".to_owned()),
                replace,
            );
            self.moved();
        }
    }

    /// Read the journal, then put what it answered into the reading.
    fn read(&self, after: Option<String>, limit: u32, action: &str, replace: bool) {
        let result = match after {
            Some(cursor) => tilo_system_resources::journal::read_after(&cursor, limit),
            None => tilo_system_resources::journal::read(limit),
        };
        self.land(action, result, replace);
        self.moved();
    }

    /// Put the answer to `action` into the reading: its records, or why there are none.
    fn land(
        &self,
        action: &str,
        result: Result<(Vec<tilo_system_resources::journal::JournalEntry>, String), String>,
        replace: bool,
    ) {
        let mut served = self.served.lock_recover();
        let journal = &mut served.reading;
        if let Some(at) = journal.pending.iter().position(|id| id == action) {
            journal.pending.remove(at);
        }
        match result {
            Ok((entries, cursor)) => {
                if replace {
                    journal.entries = entries;
                } else {
                    journal.entries.extend(entries);
                    let too_many = journal.entries.len().saturating_sub(KEPT);
                    if too_many > 0 {
                        journal.entries.drain(..too_many);
                    }
                }
                journal.cursor = cursor;
                journal.serving = true;
                journal.read_error = None;
                journal.read_at_ms = now_ms();
                journal.problem = None;
            }
            Err(error) => {
                journal.problem = Some(error.clone());
                if replace {
                    journal.serving = false;
                    journal.read_error = Some(error);
                }
            }
        }
        drop(served);
    }
}

/// The moment now, in milliseconds since the Unix epoch.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

/// The limit `arguments` names, when it names one.
fn limit_of(arguments: &Map<String, Value>, fallback: u32) -> u32 {
    arguments
        .get("limit")
        .and_then(Value::as_u64)
        .map_or(fallback, |limit| u32::try_from(limit).unwrap_or(u32::MAX))
}

/// Why a caller's action cannot run, in the receipt's words.
fn refused(code: &str, message: String) -> runtime_collab::wire::Receipt {
    runtime_collab::wire::Receipt::Refused {
        code: code.to_owned(),
        message,
    }
}

fn done() -> runtime_collab::wire::Receipt {
    runtime_collab::wire::Receipt::Done(
        serde_json::to_vec(&ActionOutput::default()).unwrap_or_default(),
    )
}

impl PublishedResources for Published {
    fn snapshot(
        &self,
        _caller: &Caller,
        resource: &str,
    ) -> oneshot::Receiver<Result<Reading, String>> {
        let (reply, answered) = oneshot::channel();
        let answer = if resource == JOURNAL {
            Ok(self.inner.encoded())
        } else {
            Ok((0, None))
        };
        let _ = reply.send(answer);
        answered
    }

    fn watch(
        &self,
        _caller: &Caller,
        resource: &str,
        sink: mpsc::UnboundedSender<runtime_collab::wire::Event>,
    ) -> Result<ResourceWatch, String> {
        if resource != JOURNAL {
            return Err(format!("this device publishes no `{resource}` resource"));
        }
        let (revision, reading) = self.inner.encoded();
        let _ = sink.send(runtime_collab::wire::Event::Reading {
            resource: JOURNAL.to_owned(),
            revision,
            reading,
        });
        let id = ResourceWatch::new(self.inner.next_watch.fetch_add(1, Ordering::Relaxed));
        self.inner
            .watches
            .lock_recover()
            .push(Watch { id, sink, revision });
        Ok(id)
    }

    fn unwatch(&self, watch: ResourceWatch) {
        self.inner
            .watches
            .lock_recover()
            .retain(|held| held.id != watch);
    }

    fn invoke(
        &self,
        _caller: &Caller,
        resource: &str,
        action: &str,
        arguments: Vec<u8>,
    ) -> oneshot::Receiver<runtime_collab::wire::Receipt> {
        let (reply, answered) = oneshot::channel();
        if resource != JOURNAL {
            let _ = reply.send(refused(
                "unknown_action",
                format!("this device publishes no `{resource}` resource"),
            ));
            return answered;
        }
        let arguments = match serde_json::from_slice::<Map<String, Value>>(&arguments) {
            Ok(arguments) => arguments,
            Err(error) => {
                let _ = reply.send(refused(
                    "invalid_arguments",
                    format!("the arguments are not a JSON object: {error}"),
                ));
                return answered;
            }
        };
        match action {
            "refresh" => {
                self.inner
                    .start_read(None, limit_of(&arguments, DEFAULT_LIMIT), action, true);
            }
            "read-after" => {
                let cursor = arguments
                    .get("cursor")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let after = (!cursor.is_empty()).then_some(cursor);
                self.inner
                    .start_read(after, limit_of(&arguments, FOLLOW_LIMIT), action, false);
            }
            "set-following" => {
                let on = arguments
                    .get("on")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                self.inner.served.lock_recover().reading.following = on;
                self.inner.moved();
            }
            other => {
                let _ = reply.send(refused(
                    "unknown_action",
                    format!("this device runs no `{other}` action on the journal"),
                ));
                return answered;
            }
        }
        let _ = reply.send(done());
        answered
    }
}

#[cfg(test)]
mod tests {
    use super::Published;
    use tilo_system_resources::journal::{FIXTURE_ENV, Journal};

    /// The provider reads the journal a capture's profile stages and publishes it as the
    /// reading the Logs window shows.
    #[test]
    fn the_staged_journal_is_the_reading_the_provider_publishes() {
        let dir = tempfile::tempdir().expect("a directory for the staged journal");
        let path = dir.path().join("journal.jsonl");
        std::fs::write(
            &path,
            "{\"__REALTIME_TIMESTAMP\":\"1700000000000000\",\"__CURSOR\":\"s=one\",\"SYSLOG_IDENTIFIER\":\"test\",\"PRIORITY\":\"3\",\"MESSAGE\":\"a record\"}\n",
        )
        .expect("the staged journal is written");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(FIXTURE_ENV, &path);
        let published = Published::new();
        published.inner.read(None, 500, "refresh", true);
        let (_, reading) = published.inner.encoded();
        let journal: Journal = serde_json::from_slice(&reading.expect("the reading is encoded"))
            .expect("the reading is the journal's own");
        assert!(journal.serving, "the journal was read");
        assert_eq!(journal.read_error, None);
        assert_eq!(journal.entries.len(), 1);
        assert_eq!(journal.entries[0].message, "a record");
        assert_eq!(journal.entries[0].priority, 3);
        assert_eq!(journal.cursor, "s=one");
    }
}
