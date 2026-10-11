//! A record held in a confined app worker, as the supervisor uses it.

use std::sync::{Mutex, PoisonError};

use automerge::AutoCommit;
use collab_types::DocumentMutation;
use tilo_actions::ActionOutput;
use tilo_actions::door::{ActionAddress, ActionDescriptor, DoorError};
use tilo_automerge_isolate::{Applied, Dependencies};
use tilo_relay_protocol::live::LiveAnchor;
use ui::record::RecordId;

use super::process::Process;
use super::proto::{Mode, Reply, Request};
use crate::apps::{AppCatalogue, AppError, Seed};

/// How often a worker that died is replaced, from what the supervisor holds
/// of its record, before the record is given up on: a document that crashes
/// every worker must not keep starting new ones.
const MAX_REVIVALS: u32 = 2;

/// One record of one app, held as a replica of its resource in a confined
/// app worker ([`crate::worker`]): [`crate::Replica`]'s operations, with the
/// app's code in the worker and everything this process signs admitted here
/// first.
///
/// The supervisor keeps its own copy of the record (the *mirror*), built
/// only through `tilo-automerge-isolate` from the same changes the worker
/// receives. Every change the worker reports is admitted to the mirror the
/// same way before it is handed back: a whole document must load, and an
/// increment must apply on top of what the record holds, with nothing
/// missing. A worker that died is replaced from the mirror.
pub struct ConfinedReplica {
    app_id: String,
    room: String,
    participant: String,
    agent: bool,
    record: RecordId,
    inner: Mutex<Inner>,
}

struct Inner {
    process: Option<Process>,
    revivals: u32,
    mirror: AutoCommit,
    /// The record is new here: the whole document is still to be sent.
    fresh: bool,
    /// An operation ran since the last take; the worker may hold changes.
    dirty: bool,
}

fn worker_error(reason: impl Into<String>) -> AppError {
    AppError::Confinement(reason.into())
}

impl ConfinedReplica {
    /// Hold a record of `app_id` in `room` as `participant`, in a new
    /// confined worker: [`crate::Replica::open`].
    ///
    /// # Errors
    ///
    /// Returns why the app is unknown, the record's changes were not
    /// admitted, or the worker did not start, confine itself or hold the
    /// record.
    pub fn open(
        catalogue: &AppCatalogue,
        app_id: &str,
        room: &str,
        participant: &str,
        agent: bool,
        seed: Seed,
    ) -> Result<Self, AppError> {
        let definition = catalogue
            .get(app_id)
            .ok_or_else(|| AppError::UnknownApp(app_id.to_owned()))?;
        let (mirror, seed, fresh) = match seed {
            Seed::New => (AutoCommit::new(), None, true),
            Seed::Existing { record, changes } => {
                let mut mirror = AutoCommit::new();
                admit_history(&mut mirror, &changes)?;
                (mirror, Some((record.as_str().to_owned(), changes)), false)
            }
        };
        let expected = seed.as_ref().map(|(record, _)| record.clone());
        let mut process = Process::start(Mode::Replica).map_err(worker_error)?;
        let reply = process
            .call(&Request::Open {
                app_id: definition.app_id.to_owned(),
                room: room.to_owned(),
                participant: participant.to_owned(),
                agent,
                seed,
            })
            .map_err(worker_error)?;
        let record = match reply {
            Reply::Opened { record } => super::serve::parse_record(&record)
                .filter(|record| expected.as_deref().is_none_or(|id| id == record.as_str()))
                .ok_or_else(|| worker_error("the worker named another record"))?,
            Reply::Failed(reason) => return Err(open_error(&reason, app_id)),
            _ => return Err(worker_error("the worker answered out of turn")),
        };
        let replica = Self {
            app_id: definition.app_id.to_owned(),
            room: room.to_owned(),
            participant: participant.to_owned(),
            agent,
            record,
            inner: Mutex::new(Inner {
                process: Some(process),
                revivals: 0,
                mirror,
                fresh,
                dirty: fresh,
            }),
        };
        if fresh {
            // The genesis is admitted now, so the mirror holds the record
            // before anything else is asked of it.
            let mut inner = replica.lock();
            replica.collect(&mut inner)?;
            drop(inner);
        }
        Ok(replica)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The app's id.
    #[must_use]
    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    /// The record's id, as its durable route names it.
    #[must_use]
    pub fn record(&self) -> &str {
        self.record.as_str()
    }

    /// Collaboration route of this record: [`crate::Replica::wire_record`].
    #[must_use]
    pub fn wire_record(&self) -> &str {
        if self.record.is_package_primary() {
            ""
        } else {
            self.record.as_str()
        }
    }

    /// The record's id.
    #[must_use]
    pub const fn record_id(&self) -> &RecordId {
        &self.record
    }

    /// The worker's process id, while one runs.
    #[must_use]
    pub fn worker_id(&self) -> Option<u32> {
        self.lock().process.as_ref().map(Process::id)
    }

    /// One request to the worker, replacing a dead one first. A failed call
    /// ends the worker; the next call starts another.
    fn call(&self, inner: &mut Inner, request: &Request) -> Result<Reply, AppError> {
        if inner.process.is_none() {
            self.revive(inner)?;
        }
        let process = inner
            .process
            .as_mut()
            .ok_or_else(|| worker_error("no app worker is running"))?;
        process.call(request).map_err(|reason| {
            inner.process = None;
            tracing::warn!(app = %self.app_id, record = %self.record.as_str(), %reason,
                "app worker: a request failed; the worker was stopped");
            worker_error(reason)
        })
    }

    /// Start a new worker on what the mirror holds of the record.
    fn revive(&self, inner: &mut Inner) -> Result<(), AppError> {
        if inner.revivals >= MAX_REVIVALS {
            return Err(worker_error(
                "the record's app worker kept stopping; the record is no longer held",
            ));
        }
        inner.revivals = inner.revivals.saturating_add(1);
        let mut process = Process::start(Mode::Replica).map_err(worker_error)?;
        match process
            .call(&Request::Open {
                app_id: self.app_id.clone(),
                room: self.room.clone(),
                participant: self.participant.clone(),
                agent: self.agent,
                seed: Some((self.record.as_str().to_owned(), vec![inner.mirror.save()])),
            })
            .map_err(worker_error)?
        {
            Reply::Opened { record } if record == self.record.as_str() => {
                inner.process = Some(process);
                inner.dirty = false;
                Ok(())
            }
            Reply::Failed(reason) => Err(worker_error(reason)),
            _ => Err(worker_error("the worker named another record")),
        }
    }

    /// Where the agent's last operation left it: [`crate::Replica::presence`].
    #[must_use]
    pub fn presence(&self) -> Option<(LiveAnchor, LiveAnchor)> {
        let reply = self.call(&mut self.lock(), &Request::Presence).ok()?;
        match reply {
            Reply::Presence(Some(text)) => serde_json::from_str(&text).ok(),
            _ => None,
        }
    }

    /// Every document operation the participant may perform now:
    /// [`crate::Replica::survey`]. Empty when the worker cannot answer.
    #[must_use]
    pub fn survey(&self) -> Vec<ActionDescriptor> {
        let reply = self.call(&mut self.lock(), &Request::Survey);
        match reply {
            Ok(Reply::Descriptors(text)) => serde_json::from_str(&text).unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    /// Perform one published operation through the resource's own door:
    /// [`crate::Replica::invoke`].
    ///
    /// # Errors
    ///
    /// Returns the door's refusal or the action's failure; a worker that
    /// could not run it is reported as unavailable.
    pub fn invoke(
        &self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirm: bool,
    ) -> Result<ActionOutput, DoorError> {
        let unavailable = |reason: String| DoorError::Unavailable {
            code: "app_worker".into(),
            reason,
        };
        let request = Request::Invoke {
            address: serde_json::to_string(address).map_err(|e| unavailable(e.to_string()))?,
            arguments: serde_json::to_string(arguments).map_err(|e| unavailable(e.to_string()))?,
            confirm,
        };
        let mut inner = self.lock();
        inner.dirty = true;
        let reply = self.call(&mut inner, &request);
        drop(inner);
        match reply {
            Ok(Reply::Output(text)) => serde_json::from_str(&text)
                .map_err(|error| DoorError::Failed(format!("the output did not decode: {error}"))),
            Ok(Reply::Refused(error)) => Err(error.into_door(address)),
            Ok(Reply::Failed(reason)) => Err(unavailable(reason)),
            Ok(_) => Err(unavailable("the worker answered out of turn".into())),
            Err(error) => Err(unavailable(error.to_string())),
        }
    }

    /// Read the record as the supervisor holds it: everything taken from the
    /// worker and everything applied, as admitted here.
    pub fn read<R>(&self, read: impl FnOnce(&AutoCommit) -> R) -> Option<R> {
        Some(read(&self.lock().mirror))
    }

    /// This participant's changes since the last call, or `None`:
    /// [`crate::Replica::take_mutation`]. The first call on a new record
    /// returns the whole document.
    ///
    /// # Errors
    ///
    /// The worker's changes were not admitted (they would not load or apply
    /// on top of the record), or the worker could not be asked. Nothing is
    /// returned to sign then.
    pub fn take_mutation(&self) -> Result<Option<DocumentMutation>, AppError> {
        let mut inner = self.lock();
        let own = self.collect(&mut inner)?;
        let whole = std::mem::take(&mut inner.fresh).then(|| inner.mirror.save());
        drop(inner);
        Ok(whole
            .map(DocumentMutation::Snapshot)
            .or_else(|| own.map(DocumentMutation::Incremental)))
    }

    /// Ask the worker for its changes and admit them to the mirror.
    fn collect(&self, inner: &mut Inner) -> Result<Option<Vec<u8>>, AppError> {
        if !std::mem::take(&mut inner.dirty) {
            return Ok(None);
        }
        match self.call(inner, &Request::TakeMutation)? {
            Reply::Mutation(None) => Ok(None),
            Reply::Mutation(Some((true, bytes))) if inner.fresh => {
                inner.mirror = tilo_automerge_isolate::load(&bytes)
                    .map_err(|error| AppError::Merge(error.to_string()))?;
                Ok(None)
            }
            Reply::Mutation(Some((false, bytes))) => {
                match tilo_automerge_isolate::apply(
                    &mut inner.mirror,
                    &bytes,
                    Dependencies::RequireComplete,
                ) {
                    Ok(Applied::Changed(_)) => Ok(Some(bytes)),
                    Ok(Applied::MissingDependencies) => {
                        inner.process = None;
                        Err(AppError::Merge(
                            "the worker's change builds on nothing the record holds".into(),
                        ))
                    }
                    Err(error) => {
                        inner.process = None;
                        Err(AppError::Merge(error.to_string()))
                    }
                }
            }
            Reply::Mutation(Some((true, _))) => {
                inner.process = None;
                Err(worker_error(
                    "the worker replaced a record it did not create",
                ))
            }
            Reply::Failed(reason) => Err(worker_error(reason)),
            _ => {
                inner.process = None;
                Err(worker_error("the worker answered out of turn"))
            }
        }
    }

    /// Merge another participant's durable change: [`crate::Replica::apply`].
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Merge`] when the change is not admitted or the
    /// record refuses it, and [`AppError::Confinement`] when the worker
    /// could not be asked.
    pub fn apply(&self, mutation: &DocumentMutation) -> Result<(), AppError> {
        let (snapshot, bytes) = match mutation {
            DocumentMutation::Snapshot(bytes) => (true, bytes),
            DocumentMutation::Incremental(bytes) => (false, bytes),
        };
        let mut inner = self.lock();
        // Admitted here before the worker sees it. A whole document may be
        // another record's, which the record refuses below, so it is merged
        // into a copy kept only if the record takes it. An increment that
        // builds on nothing here only waits in the mirror's queue: the
        // mirror then holds more than the worker, never less, so the
        // worker's own changes still apply on top of it.
        let candidate = if snapshot {
            let mut candidate = inner.mirror.clone();
            admit_incoming(&mut candidate, bytes)?;
            Some(candidate)
        } else {
            admit_incoming(&mut inner.mirror, bytes)?;
            None
        };
        let reply = self.call(
            &mut inner,
            &Request::Apply {
                snapshot,
                bytes: bytes.clone(),
            },
        );
        let result = match reply {
            Ok(Reply::Done) => {
                if let Some(candidate) = candidate {
                    inner.mirror = candidate;
                }
                Ok(())
            }
            Ok(Reply::Failed(reason)) => Err(AppError::Merge(reason)),
            Ok(_) => {
                inner.process = None;
                Err(worker_error("the worker answered out of turn"))
            }
            Err(error) => Err(error),
        };
        drop(inner);
        result
    }
}

/// Admit untrusted `bytes` (a document, changes, or both) to `doc` through
/// the isolate, queueing changes whose dependencies have not arrived, as
/// `load_incoming` does.
fn admit_incoming(doc: &mut AutoCommit, bytes: &[u8]) -> Result<(), AppError> {
    if bytes.is_empty() {
        return Ok(());
    }
    tilo_automerge_isolate::apply(doc, bytes, Dependencies::RetainOrphans)
        .map(|_| ())
        .map_err(|error| AppError::Merge(error.to_string()))
}

/// Most bytes of a record's history admitted in one request: well under the
/// isolate's input ceiling, so a long history is admitted in parts rather
/// than refused for its length.
const HISTORY_BATCH_BYTES: usize = 8 * 1024 * 1024;

/// Admit a record's history, in order, in batches of whole changes.
fn admit_history(doc: &mut AutoCommit, changes: &[Vec<u8>]) -> Result<(), AppError> {
    let mut batch = Vec::new();
    for change in changes {
        if !batch.is_empty() && batch.len().saturating_add(change.len()) > HISTORY_BATCH_BYTES {
            admit_incoming(doc, &std::mem::take(&mut batch))?;
        }
        batch.extend_from_slice(change);
    }
    admit_incoming(doc, &batch)
}

/// What a worker's refusal to open says, as [`crate::Replica::open`] says it.
fn open_error(reason: &str, app_id: &str) -> AppError {
    if reason.starts_with("no app ") {
        AppError::UnknownApp(app_id.to_owned())
    } else {
        AppError::Launch(reason.to_owned())
    }
}
