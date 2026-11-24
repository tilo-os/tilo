//! What the desktop keeps of its sessions for the next one, as records of the device it runs on:
//! the workspace in front, the wallpapers, the notification centre, what could not be written to a
//! workspace yet, and the like. Each is one of the device's own records, which its daemon keeps
//! (`settings/<name>` in the person's own workspace): read through the one record API and written
//! with the device's `settings#set` action, both on the shell link the daemon handed the desktop.
//!
//! A write is handed to the daemon in the order the session made it, away from the thread that made
//! it; [`DeviceRecords::flush`] is what a session that ends waits for. A record the session shows
//! while it runs is followed ([`DeviceRecord::follow`]): a change Settings, an agent or another of
//! the person's tools writes reaches the session as it lands.

#![cfg(unix)]

use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use serde_json::{Map, Value};
use tilo_actions::remote::{
    DEVICE_SETTINGS, InvokeArgument, InvokeRequest, ReadAnswer, RemoteActionTarget,
    ResourceReadRequest,
};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_lock::MutexRecover as _;
use tokio::sync::mpsc;

use crate::daemon_documents::DaemonShell;

/// The action that writes one of the device's records.
const SET: &str = "set";

/// How long a follower's read waits for the record to move before it asks again.
const FOLLOW_WAIT: Duration = tilo_actions::remote::MAX_REMOTE_WAIT;

/// How long a follower waits after the daemon could not be asked before it asks again.
const ASK_AGAIN: Duration = Duration::from_secs(2);

/// How many of its own writes a follower remembers, so a reading of one of them is not taken for a
/// change from elsewhere.
const OWN_WRITES: usize = 16;

/// The device's records, as the desktop reads and writes them.
#[derive(Clone)]
pub struct DeviceRecords {
    keeper: Arc<Keeper>,
}

impl std::fmt::Debug for DeviceRecords {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceRecords").finish_non_exhaustive()
    }
}

enum Keeper {
    /// The device's daemon, in the person's own workspace, which its records are carried into.
    Daemon {
        shell: DaemonShell,
        workspace: String,
        writes: mpsc::UnboundedSender<Queued>,
    },
    /// Records held in this process alone, as the daemon would keep them: a test's.
    #[cfg(any(test, feature = "test-support"))]
    Held(Mutex<Held>),
}

/// Records held in this process, and who follows each.
#[cfg(any(test, feature = "test-support"))]
#[derive(Default)]
struct Held {
    values: std::collections::BTreeMap<String, Value>,
    followers: Vec<(String, Weak<Followed>)>,
}

/// What waits its turn to reach the daemon.
enum Queued {
    Set(String, Value),
    /// Answered once everything handed before it was answered.
    Flushed(std::sync::mpsc::SyncSender<()>),
}

impl DeviceRecords {
    /// The records of the device `shell` reaches, read and written in `workspace`, the person's
    /// own, which the daemon carries them into.
    #[must_use]
    pub fn on(shell: DaemonShell, workspace: String) -> Self {
        let (writes, mut pending) = mpsc::unbounded_channel();
        let (daemon, space) = (shell.clone(), workspace.clone());
        shell.runtime().spawn(async move {
            while let Some(next) = pending.recv().await {
                match next {
                    Queued::Set(name, value) => {
                        let request = set_request(&name, value);
                        if let Err(error) = daemon
                            .dispatch(
                                Some(&space),
                                &tilo_actions::remote::RemoteActionMessage::Invoke(request),
                            )
                            .await
                        {
                            tracing::warn!(%error, record = %name, "what the session keeps was not written");
                        }
                    }
                    Queued::Flushed(done) => {
                        let _ = done.send(());
                    }
                }
            }
        });
        Self {
            keeper: Arc::new(Keeper::Daemon {
                shell,
                workspace,
                writes,
            }),
        }
    }

    /// Records held in this process alone, as the device's daemon keeps them: what a test of a
    /// session that keeps something for the next one hands both sessions.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn held() -> Self {
        Self {
            keeper: Arc::new(Keeper::Held(Mutex::default())),
        }
    }

    /// Write `value` as the record `name` from outside this session, as Settings or an agent does:
    /// what a test of a session that follows the record hands it. Only a record held in this
    /// process is written this way: one the daemon keeps is written by the daemon alone.
    #[cfg(any(test, feature = "test-support"))]
    pub fn write_elsewhere(&self, name: &str, value: Value) {
        let Keeper::Held(held) = &*self.keeper else {
            tracing::warn!(record = %name, "only a held record is written from here");
            return;
        };
        let followers: Vec<_> = {
            let mut held = held.lock_recover();
            held.values.insert(name.to_owned(), value.clone());
            held.followers.retain(|(_, follower)| follower.strong_count() > 0);
            held.followers
                .iter()
                .filter(|(followed, _)| followed == name)
                .filter_map(|(_, follower)| follower.upgrade())
                .collect()
        };
        let announced = Some(value).filter(|value| !value.is_null());
        for follower in followers {
            follower.arrived(announced.clone());
        }
    }

    /// The record `name`.
    #[must_use]
    pub fn record(&self, name: &str) -> DeviceRecord {
        DeviceRecord {
            records: self.clone(),
            name: name.to_owned(),
            followed: None,
        }
    }

    /// Wait until every write handed before this was answered: what a session that ends waits for.
    /// Called from a thread that is not the shell's runtime.
    pub fn flush(&self) {
        match &*self.keeper {
            Keeper::Daemon { writes, .. } => {
                let (done, flushed) = std::sync::mpsc::sync_channel(1);
                if writes.send(Queued::Flushed(done)).is_ok() {
                    let _ = flushed.recv();
                }
            }
            #[cfg(any(test, feature = "test-support"))]
            Keeper::Held(_) => {}
        }
    }
}

/// One record of the device's, by its name.
#[derive(Clone)]
pub struct DeviceRecord {
    records: DeviceRecords,
    name: String,
    /// What the record holds as its follower last heard, once it is followed.
    followed: Option<Arc<Followed>>,
}

impl std::fmt::Debug for DeviceRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceRecord")
            .field("name", &self.name)
            .field("followed", &self.followed.is_some())
            .finish_non_exhaustive()
    }
}

/// A followed record: what it holds as last heard, the writes of this session a reading may still
/// bring back, and who is told a change from elsewhere.
struct Followed {
    known: Mutex<Known>,
    changed: Box<dyn Fn(Option<Value>) + Send + Sync>,
}

#[derive(Default)]
struct Known {
    value: Option<Value>,
    written: Vec<Option<Value>>,
}

impl Followed {
    /// A reading of the record landed: told on when it is neither what was known nor one of this
    /// session's own writes coming back.
    fn arrived(&self, value: Option<Value>) {
        {
            let mut known = self.known.lock_recover();
            if known.value == value {
                return;
            }
            if let Some(own) = known.written.iter().position(|written| *written == value) {
                known.written.drain(..=own);
                return;
            }
            known.value.clone_from(&value);
            known.written.clear();
        }
        (self.changed)(value);
    }

    /// This session wrote `value`.
    fn wrote(&self, value: Option<Value>) {
        let mut known = self.known.lock_recover();
        if known.written.len() == OWN_WRITES {
            known.written.remove(0);
        }
        known.written.push(value.clone());
        known.value = value;
    }
}

/// A change a followed record arrived at, kept until the session's loop takes it.
#[derive(Clone, Debug)]
enum Arrival {
    /// What the record holds now.
    Held(Value),
    /// The record was emptied.
    Emptied,
}

/// What a followed record holds now, when it changed since it was last taken: for a session that
/// takes a change up on its own loop.
#[derive(Clone, Default)]
pub struct Arrived(Arc<Mutex<Option<Arrival>>>);

impl Arrived {
    /// The record as it changed to since the last take, `Some(None)` when it was emptied.
    #[must_use]
    pub fn take(&self) -> Option<Option<Value>> {
        self.0.lock_recover().take().map(|arrival| match arrival {
            Arrival::Held(value) => Some(value),
            Arrival::Emptied => None,
        })
    }
}

impl DeviceRecord {
    /// What the record holds, `None` when nothing was kept yet: as last heard once it is followed,
    /// else read from the daemon. Called from a thread that is not the shell's runtime.
    ///
    /// # Errors
    /// The daemon could not be asked, or refused the read.
    pub fn read(&self) -> Result<Option<Value>, String> {
        if let Some(followed) = &self.followed {
            return Ok(followed.known.lock_recover().value.clone());
        }
        match &*self.records.keeper {
            Keeper::Daemon {
                shell, workspace, ..
            } => shell
                .read_blocking(read_request(workspace, &self.name, None))
                .map(|readings| value_of(&readings)),
            #[cfg(any(test, feature = "test-support"))]
            Keeper::Held(held) => Ok(held.lock_recover().values.get(&self.name).cloned()),
        }
    }

    /// The record, followed from now on: [`Self::read`] answers it as last heard, and `changed` is
    /// told each change written elsewhere (never one of this session's own), on a thread that is
    /// not the session's. The follower stops once every copy of the record is dropped.
    ///
    /// # Errors
    /// The daemon could not be asked what the record holds now, or refused the read.
    pub fn follow(
        &self,
        changed: impl Fn(Option<Value>) + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let from_now = |now| {
            Arc::new(Followed {
                known: Mutex::new(Known {
                    value: now,
                    written: Vec::new(),
                }),
                changed: Box::new(changed),
            })
        };
        let followed = match &*self.records.keeper {
            Keeper::Daemon {
                shell, workspace, ..
            } => {
                // What it holds now, and the revision the follower waits past: a reading of the
                // record before this session's own writes is never heard as news.
                let readings = shell.read_blocking(read_request(workspace, &self.name, None))?;
                let followed = from_now(value_of(&readings));
                let after = readings
                    .get("revision")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let (shell, workspace, name) =
                    (shell.clone(), workspace.clone(), self.name.clone());
                let watching = Arc::downgrade(&followed);
                shell
                    .runtime()
                    .clone()
                    .spawn(follow(shell, workspace, name, after, watching));
                followed
            }
            #[cfg(any(test, feature = "test-support"))]
            Keeper::Held(held) => {
                let mut held = held.lock_recover();
                let followed = from_now(held.values.get(&self.name).cloned());
                held.followers
                    .push((self.name.clone(), Arc::downgrade(&followed)));
                followed
            }
        };
        Ok(Self {
            records: self.records.clone(),
            name: self.name.clone(),
            followed: Some(followed),
        })
    }

    /// [`Self::follow`], each change kept in what it answers for the session's loop to take, and
    /// `wake` called to run it.
    ///
    /// # Errors
    /// As [`Self::follow`].
    pub fn follow_waking(
        &self,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<(Self, Arrived), String> {
        let arrived = Arrived::default();
        let keeping = arrived.clone();
        let record = self.follow(move |value| {
            *keeping.0.lock_recover() = Some(match value {
                Some(value) => Arrival::Held(value),
                None => Arrival::Emptied,
            });
            wake();
        })?;
        Ok((record, arrived))
    }

    /// Keep `value` as the record, replacing what it held: handed to the daemon after every write
    /// before it, without waiting for it.
    pub fn set(&self, value: Value) {
        if let Some(followed) = &self.followed {
            followed.wrote(Some(value.clone()).filter(|value| !value.is_null()));
        }
        match &*self.records.keeper {
            Keeper::Daemon { writes, .. } => {
                if writes.send(Queued::Set(self.name.clone(), value)).is_err() {
                    tracing::warn!(record = %self.name, "the daemon's connection is gone; what the session keeps was not written");
                }
            }
            #[cfg(any(test, feature = "test-support"))]
            Keeper::Held(held) => {
                held.lock_recover().values.insert(self.name.clone(), value);
            }
        }
    }

    /// [`Self::set`], answered once the daemon kept it.
    ///
    /// # Errors
    /// The daemon could not be asked, or did not keep it.
    pub fn set_now(&self, value: Value) -> Result<(), String> {
        match &*self.records.keeper {
            Keeper::Daemon {
                shell, workspace, ..
            } => {
                if let Some(followed) = &self.followed {
                    followed.wrote(Some(value.clone()).filter(|value| !value.is_null()));
                }
                // After every write handed before it.
                self.records.flush();
                shell
                    .dispatch_blocking(
                        Some(workspace),
                        &tilo_actions::remote::RemoteActionMessage::Invoke(set_request(
                            &self.name, value,
                        )),
                    )
                    .map(|_| ())
            }
            #[cfg(any(test, feature = "test-support"))]
            Keeper::Held(_) => {
                self.set(value);
                Ok(())
            }
        }
    }
}

/// Follow the record `name` past the revision `after` for `follower`, with one held read at a time,
/// until the follower is dropped.
async fn follow(
    shell: DaemonShell,
    workspace: String,
    name: String,
    mut after: Option<String>,
    follower: Weak<Followed>,
) {
    while follower.strong_count() > 0 {
        match shell
            .read(read_request(&workspace, &name, after.clone()))
            .await
        {
            Ok(readings) => {
                after = readings
                    .get("revision")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let Some(follower) = follower.upgrade() else {
                    return;
                };
                follower.arrived(value_of(&readings));
                if after.is_none() {
                    // A reading with no revision gives no wait to hold: asked again shortly.
                    tokio::time::sleep(ASK_AGAIN).await;
                }
            }
            Err(error) => {
                tracing::debug!(%error, record = %name, "a kept record is followed again shortly");
                tokio::time::sleep(ASK_AGAIN).await;
            }
        }
    }
}

/// The read of the record `name`: what it holds now, or with `after` once it moves past that
/// revision or the follower's wait went by.
fn read_request(workspace: &str, name: &str, after: Option<String>) -> ResourceReadRequest {
    let wait_ms = after
        .is_some()
        .then(|| u32::try_from(FOLLOW_WAIT.as_millis()).unwrap_or(u32::MAX));
    ResourceReadRequest {
        workspace: workspace.to_owned(),
        app: DEVICE_SETTINGS.to_owned(),
        record: name.to_owned(),
        joined: false,
        after,
        wait_ms,
        answer: ReadAnswer::Reading,
    }
}

/// What a reading of one record holds: `None` for one absent or emptied.
fn value_of(readings: &Value) -> Option<Value> {
    let record = readings.get("records")?.get(0)?;
    let absent = record.get("absent").and_then(Value::as_bool) == Some(true);
    record
        .get("value")
        .filter(|_| !absent)
        .filter(|value| !value.is_null())
        .cloned()
}

/// The request writing the record `name` as `value`.
fn set_request(name: &str, value: Value) -> InvokeRequest {
    let mut arguments = Map::new();
    arguments.insert("name".to_owned(), Value::String(name.to_owned()));
    arguments.insert("value".to_owned(), value);
    InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: tilo_device_identity::login::PROVIDER.to_owned(),
            address: ActionAddress::new(DEVICE_SETTINGS, SET),
        },
        argument: InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}
