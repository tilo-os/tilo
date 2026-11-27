//! What the desktop keeps of its sessions for the next one, as records of the device it runs on:
//! the workspace in front, the wallpapers, the notification centre, what could not be written to a
//! workspace yet, and the like. Each is one of the device's own records, which its daemon keeps
//! (`settings/<name>` in the person's own workspace): read through the one record API and written
//! with the device's `settings#set` action, both on the shell link the daemon handed the desktop.
//!
//! A device certified for one record alone, a workspace's VM, has no workspace of the person's to
//! keep them in and never writes their settings: its session's records are kept in that one record
//! instead ([`Place::Environment`]), each under the same name, written with the same action. The
//! host seals them there, so the VM's disk keeps nothing its next session needs.
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
    ResourceAddress, ResourceReadRequest,
};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_lock::MutexRecover as _;
use tokio::sync::mpsc;

use crate::daemon_documents::DaemonShell;

/// The action that writes one of the device's records.
const SET: &str = "set";

/// The device record what this device's session shows: its workspaces in its own order and the
/// one in front, under `active`. It is the device's alone, and what any program the person runs
/// reads to know the workspace their session runs in.
pub const SHOWN: &str = "so.tilo.session-shown";

/// The workspace [`SHOWN`]'s value has in front; `None` when it names none.
#[must_use]
pub fn shown_workspace(shown: &Value) -> Option<&str> {
    shown
        .get("active")
        .and_then(Value::as_str)
        .filter(|workspace| !workspace.is_empty())
}

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

/// Where the device's daemon keeps the session's records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    /// The person's own workspace, by its id: each record is `settings/<name>` there.
    Own(String),
    /// The one record a device certified for that record alone may write (a workspace's VM): each
    /// record is the field `settings.<name>` of it.
    Environment(ResourceAddress),
}

impl Place {
    /// The workspace a read or a write of a record here names.
    fn workspace(&self) -> &str {
        match self {
            Self::Own(workspace) => workspace,
            Self::Environment(address) => &address.workspace,
        }
    }

    /// The read of the record `name`: what it holds now, or with `after` once it moves past that
    /// revision or the follower's wait went by.
    fn read_request(&self, name: &str, after: Option<String>) -> ResourceReadRequest {
        let wait_ms = after
            .is_some()
            .then(|| u32::try_from(FOLLOW_WAIT.as_millis()).unwrap_or(u32::MAX));
        let (app, record) = match self {
            Self::Own(_) => (DEVICE_SETTINGS.to_owned(), name.to_owned()),
            Self::Environment(address) => (address.app.clone(), address.record.clone()),
        };
        ResourceReadRequest {
            workspace: self.workspace().to_owned(),
            app,
            record,
            joined: false,
            after,
            wait_ms,
            answer: ReadAnswer::Reading,
        }
    }

    /// What a reading of the record `name` holds: `None` for one absent or emptied.
    fn value_of(&self, name: &str, readings: &Value) -> Option<Value> {
        let record = readings.get("records")?.get(0)?;
        if record.get("absent").and_then(Value::as_bool) == Some(true) {
            return None;
        }
        let value = record.get("value")?;
        match self {
            Self::Own(_) => Some(value),
            Self::Environment(_) => value.get(DEVICE_SETTINGS)?.get(name),
        }
        .filter(|value| !value.is_null())
        .cloned()
    }
}

enum Keeper {
    /// The device's daemon, which keeps the records where `place` says.
    Daemon {
        shell: DaemonShell,
        place: Place,
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
    /// The records of the device `shell` reaches, read and written where `place` says.
    #[must_use]
    pub fn on(shell: DaemonShell, place: Place) -> Self {
        let (writes, mut pending) = mpsc::unbounded_channel();
        let (daemon, space) = (shell.clone(), place.workspace().to_owned());
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
                place,
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
            held.followers
                .retain(|(_, follower)| follower.strong_count() > 0);
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

    /// Wait up to `within` until every write handed before this was answered: what a session that
    /// ends waits for. True once they all were; false when the daemon did not answer them in time,
    /// which a session that ends says and leaves, as it leaves a layout write that did not answer:
    /// a daemon that never answers would otherwise hold the session's end for ever. Called from a
    /// thread that is not the shell's runtime.
    pub fn flush(&self, within: Duration) -> bool {
        match &*self.keeper {
            Keeper::Daemon { writes, .. } => {
                let (done, flushed) = std::sync::mpsc::sync_channel(1);
                // A queue whose writer ended has nothing left that could still be answered.
                if writes.send(Queued::Flushed(done)).is_err() {
                    return true;
                }
                match flushed.recv_timeout(within) {
                    Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => true,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        tracing::warn!(
                            waited_s = within.as_secs(),
                            "what the session keeps was not all written: the device's daemon did not answer in time"
                        );
                        false
                    }
                }
            }
            #[cfg(any(test, feature = "test-support"))]
            Keeper::Held(_) => true,
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
            Keeper::Daemon { shell, place, .. } => shell
                .read_blocking(place.read_request(&self.name, None))
                .map(|readings| place.value_of(&self.name, &readings)),
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
            Keeper::Daemon { shell, place, .. } => {
                // What it holds now, and the revision the follower waits past: a reading of the
                // record before this session's own writes is never heard as news.
                let readings = shell.read_blocking(place.read_request(&self.name, None))?;
                let followed = from_now(place.value_of(&self.name, &readings));
                let after = readings
                    .get("revision")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let (shell, place, name) = (shell.clone(), place.clone(), self.name.clone());
                let watching = Arc::downgrade(&followed);
                shell
                    .runtime()
                    .clone()
                    .spawn(follow(shell, place, name, after, watching));
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
            Keeper::Daemon { shell, place, .. } => {
                if let Some(followed) = &self.followed {
                    followed.wrote(Some(value.clone()).filter(|value| !value.is_null()));
                }
                // After every write handed before it.
                if !self.records.flush(crate::daemon_documents::ASK_DEADLINE) {
                    return Err(format!(
                        "the device's daemon did not answer the session's earlier writes, so {} was not written",
                        self.name
                    ));
                }
                shell
                    .dispatch_blocking(
                        Some(place.workspace()),
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
    place: Place,
    name: String,
    mut after: Option<String>,
    follower: Weak<Followed>,
) {
    while follower.strong_count() > 0 {
        match shell.read(place.read_request(&name, after.clone())).await {
            Ok(readings) => {
                after = readings
                    .get("revision")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let Some(follower) = follower.upgrade() else {
                    return;
                };
                follower.arrived(place.value_of(&name, &readings));
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

/// The request writing the record `name` as `value`.
fn set_request(name: &str, value: Value) -> InvokeRequest {
    let mut arguments = Map::new();
    arguments.insert("name".to_owned(), Value::String(name.to_owned()));
    arguments.insert("value".to_owned(), value);
    InvokeRequest {
        asks: false,
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A session that ends while the device's daemon leaves its writes unanswered waits for them
    /// as long as it was given, says they were not written, and ends: a daemon that holds an ask
    /// for ever never holds the session's end with it.
    #[test]
    fn an_end_waits_for_unanswered_writes_no_longer_than_it_was_given() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("a runtime");
        // The daemon's end of the link takes every ask and answers none.
        let (link, daemon) = std::os::unix::net::UnixStream::pair().expect("a link");
        link.set_nonblocking(true)
            .expect("a link the runtime reads");
        let shell = runtime.block_on(async {
            let link = tokio::net::UnixStream::from_std(link).expect("the link on the runtime");
            DaemonShell::new(
                Ok(runtime_collab::ShellClient::open(link).0),
                tokio::runtime::Handle::current(),
            )
        });
        let records = DeviceRecords::on(shell, Place::Own("person".to_owned()));
        records
            .record("so.tilo.session-shown")
            .set(json!({"front": "Writer"}));
        let asked = std::time::Instant::now();
        assert!(
            !records.flush(Duration::from_millis(200)),
            "a write the daemon never answered is not taken for written",
        );
        let waited = asked.elapsed();
        assert!(
            waited < Duration::from_secs(5),
            "the end waited {waited:?} for a daemon that never answers",
        );
        drop(daemon);
    }

    /// A session in a workspace's VM reads each of its records from the one record its device may
    /// write, under the name it has everywhere, and asks for that record, never the person's
    /// settings; a session elsewhere reads the person's own settings record of that name.
    #[test]
    fn a_vm_session_reads_its_records_from_its_one_record() {
        let environment = Place::Environment(ResourceAddress {
            workspace: "hosted".to_owned(),
            app: "browser".to_owned(),
            record: "environment".to_owned(),
        });
        let asked = environment.read_request("so.tilo.session-wallpapers", None);
        assert_eq!(
            (
                asked.workspace.as_str(),
                asked.app.as_str(),
                asked.record.as_str()
            ),
            ("hosted", "browser", "environment"),
        );
        let reading = json!({"records": [{"value": {"settings": {
            "so.tilo.session-wallpapers": {"default": "dunes"},
        }}}]});
        assert_eq!(
            environment.value_of("so.tilo.session-wallpapers", &reading),
            Some(json!({"default": "dunes"})),
        );
        assert_eq!(
            environment.value_of("so.tilo.session-shown", &reading),
            None,
            "a record the session never kept there reads as nothing",
        );

        let own = Place::Own("person".to_owned());
        let asked = own.read_request("so.tilo.session-wallpapers", None);
        assert_eq!(
            (
                asked.workspace.as_str(),
                asked.app.as_str(),
                asked.record.as_str()
            ),
            ("person", DEVICE_SETTINGS, "so.tilo.session-wallpapers"),
        );
        let reading = json!({"records": [{"value": {"default": "dunes"}}]});
        assert_eq!(
            own.value_of("so.tilo.session-wallpapers", &reading),
            Some(json!({"default": "dunes"})),
        );
        assert_eq!(
            own.value_of("x", &json!({"records": [{"absent": true, "value": {}}]})),
            None,
        );
    }
}
