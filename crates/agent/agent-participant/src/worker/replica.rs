//! A record held in a confined app worker, as the supervisor uses it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use automerge::AutoCommit;
use collab_types::DocumentMutation;
use tilo_actions::ActionOutput;
use tilo_actions::door::{ActionAddress, ActionDescriptor, DoorError};
use tilo_automerge_isolate::{Applied, Dependencies};
use tilo_content::ContentStore;
use tilo_relay_protocol::live::LiveAnchor;
use ui::record::RecordId;

use super::process::Process;
use super::proto::{MAX_CONTENT_ITEM_BYTES, Mode, Reply, Request};
use crate::apps::{AppCatalogue, AppError, ReplicaActor, Seed};
use tilo_lock::MutexRecover as _;

/// How often a worker that died is replaced, from what the supervisor holds
/// of its record, before the record is given up on: a document that crashes
/// every worker must not keep starting new ones.
const MAX_REVIVALS: u32 = 2;

/// The most pieces of content the record refers to that are put into its
/// worker before one effect runs.
const REFERENCED_CONTENT_ITEMS: usize = 256;

/// The most bytes of such content put into the worker before one effect
/// runs; the piece that crosses it is the last.
const REFERENCED_CONTENT_BYTES: usize = 256 * 1024 * 1024;

/// The most values of a record's document read looking for the content it
/// refers to.
const MOST_SCANNED_VALUES: usize = 1 << 20;

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
///
/// Lent the room's content store ([`Self::lending`]), the replica carries
/// content between the room and the worker around each operation: what the
/// operation names, and before an effect what the record refers to, goes
/// in before it runs; what the app stored comes out after, hashed again
/// here before the room keeps it.
pub struct ConfinedReplica {
    app_id: String,
    room: String,
    participant: String,
    /// JSON [`ReplicaActor`], sent again to a revived worker.
    actor: String,
    record: RecordId,
    content: Option<ContentStore>,
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
    /// The records the app keeps beside this one that are held with it
    /// (the parts of its chain), each mirrored as this one is.
    beside: BTreeMap<RecordId, AutoCommit>,
    /// An operation ran since the last take of the records beside.
    beside_dirty: bool,
    /// Content the worker holds, put again into a worker that replaces it.
    held: BTreeSet<String>,
}

fn worker_error(reason: impl Into<String>) -> AppError {
    AppError::Confinement(reason.into())
}

impl ConfinedReplica {
    /// Hold a record of `app_id` in `room` as `participant`, in a new
    /// confined worker: [`crate::Replica::open`].
    pub fn open(
        catalogue: &AppCatalogue,
        app_id: &str,
        room: &str,
        participant: &str,
        actor: &ReplicaActor,
        seed: Seed,
    ) -> Result<Self, AppError> {
        let actor =
            serde_json::to_string(actor).map_err(|error| worker_error(error.to_string()))?;
        let definition = catalogue
            .get(app_id)
            .ok_or_else(|| AppError::UnknownApp(app_id.to_owned()))?;
        let (mirror, seed, fresh) = match seed {
            Seed::New => (AutoCommit::new(), None, true),
            // No changes asks the worker for a new record under that id.
            Seed::NewAt(record) => (
                AutoCommit::new(),
                Some((record.as_str().to_owned(), Vec::new())),
                true,
            ),
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
                actor: actor.clone(),
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
            actor,
            record,
            content: None,
            inner: Mutex::new(Inner {
                process: Some(process),
                revivals: 0,
                mirror,
                fresh,
                // An existing record's worker may have brought it up to
                // the app's storage on open: asked once, so the migration
                // reaches the mirror and the room.
                dirty: true,
                beside: BTreeMap::new(),
                beside_dirty: false,
                held: BTreeSet::new(),
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

    /// Lend the record the room's content store: content an operation names
    /// (and, before an effect, content the record refers to) is put into the
    /// worker from it, and content the app stores in the worker is kept in
    /// it, so an export or a look stays in the room.
    #[must_use]
    pub fn lending(mut self, content: ContentStore) -> Self {
        self.content = Some(content);
        self
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock_recover()
    }

    #[must_use]
    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    /// Whether an agent holds the record ([`ReplicaActor::Agent`]): only an
    /// agent's renders go to the host's render service.
    pub(crate) fn is_agents(&self) -> bool {
        serde_json::from_str::<ReplicaActor>(&self.actor)
            .is_ok_and(|actor| matches!(actor, ReplicaActor::Agent))
    }

    /// Collaboration route of this record: [`crate::Replica::wire_record`].
    #[must_use]
    pub fn wire_record(&self) -> &str {
        self.record.route()
    }

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
                actor: self.actor.clone(),
                seed: Some((self.record.as_str().to_owned(), vec![inner.mirror.save()])),
            })
            .map_err(worker_error)?
        {
            Reply::Opened { record } if record == self.record.as_str() => {
                // The records held beside it go back to the new worker as
                // the mirrors hold them.
                for (record, mirror) in &mut inner.beside {
                    match process
                        .call(&Request::ApplyRecord {
                            record: record.as_str().to_owned(),
                            snapshot: true,
                            bytes: mirror.save(),
                        })
                        .map_err(worker_error)?
                    {
                        Reply::Done => {}
                        Reply::Failed(reason) => return Err(worker_error(reason)),
                        _ => return Err(worker_error("the worker answered out of turn")),
                    }
                }
                // The content the last worker held went with it.
                let held = std::mem::take(&mut inner.held);
                if let Some(content) = &self.content {
                    for hash in held {
                        if put_content(&mut process, content, &hash)?.is_some() {
                            inner.held.insert(hash);
                        }
                    }
                }
                inner.process = Some(process);
                inner.dirty = false;
                inner.beside_dirty = false;
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
            Ok(Reply::Descriptors(text)) => {
                crate::render::offered(serde_json::from_str(&text).unwrap_or_default())
            }
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
        self.invoke_with(None, None, address, arguments, confirm)
    }

    /// [`Self::invoke`] showing the appearance the door admitted:
    /// [`crate::Replica::invoke_appearing`].
    ///
    /// # Errors
    ///
    /// As [`Self::invoke`].
    pub fn invoke_appearing(
        &self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirm: bool,
        appearance: Option<&tilo_actions::Appearance>,
    ) -> Result<ActionOutput, DoorError> {
        self.invoke_within(None, None, appearance, address, arguments, confirm)
    }

    /// [`Self::invoke`] as `invoker` within `context`:
    /// [`crate::Replica::invoke_with`].
    ///
    /// # Errors
    ///
    /// As [`Self::invoke`].
    pub fn invoke_with(
        &self,
        invoker: Option<&tilo_actions::ActionInvoker>,
        context: Option<&tilo_actions::ActionContext>,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirm: bool,
    ) -> Result<ActionOutput, DoorError> {
        self.invoke_within(invoker, context, None, address, arguments, confirm)
    }

    /// [`Self::invoke_with`] showing `appearance`, which only a door that
    /// admitted it passes.
    pub(crate) fn invoke_within(
        &self,
        invoker: Option<&tilo_actions::ActionInvoker>,
        context: Option<&tilo_actions::ActionContext>,
        appearance: Option<&tilo_actions::Appearance>,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirm: bool,
    ) -> Result<ActionOutput, DoorError> {
        // A render is made in the host's render service, never the worker.
        if let Some(rendered) =
            crate::render::route(self, self.content.as_ref(), address, arguments)
        {
            return rendered;
        }
        let unavailable = |reason: String| DoorError::Unavailable {
            code: "app_worker".into(),
            reason,
        };
        let request = Request::Invoke {
            address: serde_json::to_string(address).map_err(|e| unavailable(e.to_string()))?,
            arguments: serde_json::to_string(arguments).map_err(|e| unavailable(e.to_string()))?,
            confirm,
            invoker: invoker
                .map(serde_json::to_string)
                .transpose()
                .map_err(|e| unavailable(e.to_string()))?,
            context: context
                .map(serde_json::to_string)
                .transpose()
                .map_err(|e| unavailable(e.to_string()))?,
            appearance: appearance
                .map(serde_json::to_string)
                .transpose()
                .map_err(|e| unavailable(e.to_string()))?,
        };
        let mut inner = self.lock();
        inner.dirty = true;
        inner.beside_dirty = true;
        if let Some(content) = &self.content {
            self.supply(&mut inner, content, &named_content(arguments))
                .map_err(|error| unavailable(error.to_string()))?;
            // What the record refers to is the effect's best effort: the
            // operation still runs, and asks for what it lacks, without it.
            if let Err(error) = self.supply_referenced(&mut inner, content, address) {
                tracing::warn!(app = %self.app_id, %error,
                    "app worker: the content the record refers to was not put");
            }
        }
        let mut reply = self.call(&mut inner, &request);
        if let Some(content) = &self.content {
            let wanted = self.gather(&mut inner, content);
            let failed = !matches!(reply, Ok(Reply::Output(_)));
            let missing: Vec<String> = wanted
                .into_iter()
                .filter(|hash| content.has(hash))
                .collect();
            // What the app lacked and the room has: the operation runs once
            // more with it there.
            if failed && !missing.is_empty() {
                for hash in &missing {
                    inner.held.remove(hash);
                }
                if self.supply(&mut inner, content, &missing).is_ok() {
                    reply = self.call(&mut inner, &request);
                    self.gather(&mut inner, content);
                }
            }
        }
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

    /// Put the content at `hashes` the room has and the worker does not yet
    /// hold into the worker.
    fn supply(
        &self,
        inner: &mut Inner,
        content: &ContentStore,
        hashes: &[String],
    ) -> Result<(), AppError> {
        for hash in hashes {
            if inner.held.contains(hash) || !content.has(hash) {
                continue;
            }
            self.put_held(inner, content, hash)?;
        }
        Ok(())
    }

    /// Put the content at `hash` into the worker, starting one if none
    /// runs, and note that it holds it. Its size, when it was put.
    fn put_held(
        &self,
        inner: &mut Inner,
        content: &ContentStore,
        hash: &str,
    ) -> Result<Option<usize>, AppError> {
        if inner.process.is_none() {
            // A new worker is given everything held; this one is next.
            self.revive(inner)?;
        }
        let process = inner
            .process
            .as_mut()
            .ok_or_else(|| worker_error("no app worker is running"))?;
        match put_content(process, content, hash) {
            Ok(Some(size)) => {
                inner.held.insert(hash.to_owned());
                Ok(Some(size))
            }
            Ok(None) => Ok(None),
            Err(error) => {
                inner.process = None;
                Err(error)
            }
        }
    }

    /// Before an effect or view runs, put into the worker the content the
    /// record's document refers to that the room has and the worker does
    /// not hold: what an effect reads of the record (an export's pictures)
    /// may have been placed by another participant, in another worker.
    /// Document operations take nothing more than they name. Bounded by
    /// [`REFERENCED_CONTENT_ITEMS`] and [`REFERENCED_CONTENT_BYTES`]; what
    /// is left out is logged.
    fn supply_referenced(
        &self,
        inner: &mut Inner,
        content: &ContentStore,
        address: &ActionAddress,
    ) -> Result<(), AppError> {
        let missing: Vec<String> = referenced_content(&inner.mirror)
            .into_iter()
            .filter(|hash| !inner.held.contains(hash) && content.has(hash))
            .collect();
        if missing.is_empty() || !self.is_effect(inner, address)? {
            return Ok(());
        }
        let (mut items, mut bytes) = (0usize, 0usize);
        for (at, hash) in missing.iter().enumerate() {
            if items >= REFERENCED_CONTENT_ITEMS || bytes >= REFERENCED_CONTENT_BYTES {
                tracing::warn!(app = %self.app_id, record = %self.record.as_str(),
                    skipped = missing.len().saturating_sub(at),
                    "app worker: the record refers to more content than an effect is given");
                break;
            }
            if let Some(size) = self.put_held(inner, content, hash)? {
                items = items.saturating_add(1);
                bytes = bytes.saturating_add(size);
            }
        }
        Ok(())
    }

    /// Whether `address` names one of the record's effects or views now,
    /// rather than a document operation.
    fn is_effect(&self, inner: &mut Inner, address: &ActionAddress) -> Result<bool, AppError> {
        match self.call(inner, &Request::Survey)? {
            Reply::Descriptors(text) => {
                let offered: Vec<ActionDescriptor> =
                    serde_json::from_str(&text).map_err(|error| worker_error(error.to_string()))?;
                Ok(offered.iter().any(|descriptor| {
                    descriptor.address == *address
                        && descriptor.kind != tilo_actions::OperationKind::Document
                }))
            }
            Reply::Failed(reason) => Err(worker_error(reason)),
            _ => {
                inner.process = None;
                Err(worker_error("the worker answered out of turn"))
            }
        }
    }

    /// Keep what the app stored in the room's store under the hash its
    /// bytes have here, and answer the hashes it asked for and lacked.
    /// Content the room does not keep is reported and left in the worker.
    fn gather(&self, inner: &mut Inner, content: &ContentStore) -> Vec<String> {
        let mut wanted = Vec::new();
        while inner.process.is_some() {
            let (offered, asked) = match self.call(inner, &Request::TakeContent) {
                Ok(Reply::Content { offered, wanted }) => (offered, wanted),
                Ok(Reply::Failed(reason)) => {
                    tracing::warn!(app = %self.app_id, %reason,
                        "app worker: its content was not taken");
                    break;
                }
                Ok(_) => {
                    inner.process = None;
                    break;
                }
                Err(_) => break,
            };
            wanted.extend(
                asked
                    .into_iter()
                    .filter(|hash| tilo_content::valid_hash(hash))
                    .map(|hash| hash.to_ascii_lowercase()),
            );
            if offered.is_empty() {
                break;
            }
            for bytes in offered {
                match content.put(&bytes) {
                    Ok(hash) => {
                        inner.held.insert(hash);
                    }
                    Err(error) => {
                        tracing::warn!(app = %self.app_id, %error,
                            "app worker: stored content was not kept");
                    }
                }
            }
        }
        wanted
    }

    /// Read the record as the supervisor holds it: everything taken from the
    /// worker and everything applied, as admitted here.
    pub fn read<R>(&self, read: impl FnOnce(&AutoCommit) -> R) -> Option<R> {
        Some(read(&self.lock().mirror))
    }

    /// [`Self::read`] through the mirror's history: its heads and changes,
    /// which a reader of events after a cursor needs.
    pub fn read_history<R>(&self, read: impl FnOnce(&automerge::Automerge) -> R) -> R {
        read(self.lock().mirror.document())
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

    /// Merge another participant's durable change into `record`: this
    /// one ([`Self::apply`]), or one its app keeps beside it, a part of its
    /// chain ([`crate::Replica::apply_record`]), mirrored here as this one
    /// is. A change to a part not held yet waits for its whole document.
    ///
    /// # Errors
    ///
    /// As [`Self::apply`]; a record that is no part of a chain is refused.
    pub fn apply_record(
        &self,
        record: &RecordId,
        mutation: &DocumentMutation,
    ) -> Result<(), AppError> {
        if record == &self.record {
            return self.apply(mutation);
        }
        if record.chain_part().is_none() {
            return Err(AppError::Merge(format!(
                "{} is held on its own, not beside {}",
                record.as_str(),
                self.record.as_str()
            )));
        }
        let (snapshot, bytes) = match mutation {
            DocumentMutation::Snapshot(bytes) => (true, bytes),
            DocumentMutation::Incremental(bytes) => (false, bytes),
        };
        let mut inner = self.lock();
        // Admitted into a copy of its mirror, kept once the worker took it.
        let mut candidate = match inner.beside.get(record) {
            Some(mirror) => mirror.clone(),
            None if snapshot => AutoCommit::new(),
            None => return Ok(()),
        };
        admit_incoming(&mut candidate, bytes)?;
        let reply = self.call(
            &mut inner,
            &Request::ApplyRecord {
                record: record.as_str().to_owned(),
                snapshot,
                bytes: bytes.clone(),
            },
        );
        let result = match reply {
            Ok(Reply::Done) => {
                inner.beside.insert(record.clone(), candidate);
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

    /// This participant's changes to the records held beside this one since
    /// the last call, by record, admitted here first:
    /// [`crate::Replica::take_record_mutations`]. A part the worker started
    /// comes whole; it must be a part of a chain not held here yet.
    ///
    /// # Errors
    ///
    /// As [`Self::take_mutation`].
    pub fn take_record_mutations(&self) -> Result<Vec<(RecordId, DocumentMutation)>, AppError> {
        let mut inner = self.lock();
        if !std::mem::take(&mut inner.beside_dirty) {
            return Ok(Vec::new());
        }
        let taken = match self.call(&mut inner, &Request::TakeRecordMutations)? {
            Reply::RecordMutations(taken) => taken,
            Reply::Failed(reason) => return Err(worker_error(reason)),
            _ => {
                inner.process = None;
                return Err(worker_error("the worker answered out of turn"));
            }
        };
        let mut out = Vec::new();
        for (record, snapshot, bytes) in taken {
            match self.admit_beside(&mut inner.beside, &record, snapshot, bytes) {
                Ok(taken) => out.push(taken),
                Err(error) => {
                    // A worker that sends what is not admitted is not asked
                    // again.
                    inner.process = None;
                    return Err(error);
                }
            }
        }
        drop(inner);
        Ok(out)
    }

    /// Admit one change the worker made to a record held beside this one
    /// to its mirror: a part it started whole, a part held here as changes
    /// on top of it.
    fn admit_beside(
        &self,
        beside: &mut BTreeMap<RecordId, AutoCommit>,
        record: &str,
        snapshot: bool,
        bytes: Vec<u8>,
    ) -> Result<(RecordId, DocumentMutation), AppError> {
        let record = RecordId::parse(record)
            .filter(|record| record.chain_part().is_some() && record != &self.record)
            .ok_or_else(|| worker_error("the worker named a record it does not hold beside"))?;
        match (snapshot, beside.get_mut(&record)) {
            (true, None) => {
                let mirror = tilo_automerge_isolate::load(&bytes)
                    .map_err(|error| AppError::Merge(error.to_string()))?;
                beside.insert(record.clone(), mirror);
                Ok((record, DocumentMutation::Snapshot(bytes)))
            }
            (false, Some(mirror)) => {
                match tilo_automerge_isolate::apply(mirror, &bytes, Dependencies::RequireComplete) {
                    Ok(Applied::Changed(_)) => Ok((record, DocumentMutation::Incremental(bytes))),
                    Ok(Applied::MissingDependencies) => Err(AppError::Merge(
                        "the worker's change builds on nothing the part holds".into(),
                    )),
                    Err(error) => Err(AppError::Merge(error.to_string())),
                }
            }
            _ => Err(worker_error(
                "the worker replaced a part it did not start, or changed one it does not hold",
            )),
        }
    }

    /// The records held beside this one ([`Self::apply_record`]), by id.
    #[must_use]
    pub fn records_beside(&self) -> Vec<RecordId> {
        self.lock().beside.keys().cloned().collect()
    }

    /// Read `record` as the supervisor holds it: this one, or one held
    /// beside it. `None` for a record not held here.
    pub fn read_record<R>(
        &self,
        record: &RecordId,
        read: impl FnOnce(&AutoCommit) -> R,
    ) -> Option<R> {
        let inner = self.lock();
        if record == &self.record {
            return Some(read(&inner.mirror));
        }
        inner.beside.get(record).map(read)
    }

    /// [`Self::read_record`] through the record's history.
    pub fn read_record_history<R>(
        &self,
        record: &RecordId,
        read: impl FnOnce(&automerge::Automerge) -> R,
    ) -> Option<R> {
        let mut inner = self.lock();
        if record == &self.record {
            return Some(read(inner.mirror.document()));
        }
        inner
            .beside
            .get_mut(record)
            .map(|mirror| read(mirror.document()))
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

/// Put the content at `hash` from `content` into the worker, answering its
/// size. `None` when it is not there to put, or too large to cross.
fn put_content(
    process: &mut Process,
    content: &ContentStore,
    hash: &str,
) -> Result<Option<usize>, AppError> {
    let Some(bytes) = content.read(hash) else {
        return Ok(None);
    };
    if bytes.len() > MAX_CONTENT_ITEM_BYTES {
        tracing::warn!(%hash, "app worker: content is too large to hand over");
        return Ok(None);
    }
    let size = bytes.len();
    match process
        .call(&Request::PutContent(bytes))
        .map_err(worker_error)?
    {
        Reply::Stored(stored) if stored == hash => Ok(Some(size)),
        Reply::Stored(_) => Err(worker_error("the worker kept content under another hash")),
        Reply::Failed(reason) => {
            tracing::warn!(%hash, %reason, "app worker: content was not kept");
            Ok(None)
        }
        _ => Err(worker_error("the worker answered out of turn")),
    }
}

/// The content `text` names: itself, when it is a bare hash, and every
/// content address inside it.
fn content_in(text: &str, named: &mut BTreeSet<String>) {
    if tilo_content::valid_hash(text) {
        named.insert(text.to_ascii_lowercase());
    }
    for after in text.split(tilo_actions::CONTENT_RESOURCE_KIND).skip(1) {
        if let Some(hash) = after.strip_prefix(':').and_then(|rest| rest.get(..64))
            && tilo_content::valid_hash(hash)
        {
            named.insert(hash.to_ascii_lowercase());
        }
    }
}

/// The content a record's document refers to: every key, string and text
/// in it that is a hash or holds a content address, as [`content_in`]
/// reads one. At most [`MOST_SCANNED_VALUES`] values are read.
fn referenced_content(doc: &AutoCommit) -> BTreeSet<String> {
    use automerge::{ObjId, ObjType, ReadDoc as _, ScalarValueRef, ValueRef};
    fn visit(
        value: ValueRef<'_>,
        id: ObjId,
        named: &mut BTreeSet<String>,
        objects: &mut Vec<(ObjId, ObjType)>,
    ) {
        match value {
            ValueRef::Object(kind) => objects.push((id, kind)),
            ValueRef::Scalar(ScalarValueRef::Str(text)) => content_in(&text, named),
            ValueRef::Scalar(_) => {}
        }
    }
    let mut named = BTreeSet::new();
    let mut objects = vec![(automerge::ROOT, ObjType::Map)];
    let mut budget = MOST_SCANNED_VALUES;
    while let Some((object, kind)) = objects.pop() {
        match kind {
            ObjType::Map | ObjType::Table => {
                for item in doc.map_range(&object, ..) {
                    if budget == 0 {
                        break;
                    }
                    budget = budget.saturating_sub(1);
                    content_in(&item.key, &mut named);
                    let id = item.id();
                    visit(item.value, id, &mut named, &mut objects);
                }
            }
            ObjType::List => {
                for item in doc.list_range(&object, ..) {
                    if budget == 0 {
                        break;
                    }
                    budget = budget.saturating_sub(1);
                    let id = item.id();
                    visit(item.value, id, &mut named, &mut objects);
                }
            }
            ObjType::Text => {
                budget = budget.saturating_sub(1);
                if let Ok(text) = doc.text(&object) {
                    content_in(&text, &mut named);
                }
            }
        }
        if budget == 0 {
            tracing::warn!("app worker: the record is too large to read whole for its content");
            break;
        }
    }
    named
}

/// The content an operation's arguments name: every string that is a
/// content address or a bare hash, and every address inside a longer one.
fn named_content(arguments: &serde_json::Map<String, serde_json::Value>) -> Vec<String> {
    fn visit(value: &serde_json::Value, named: &mut BTreeSet<String>) {
        match value {
            serde_json::Value::String(text) => content_in(text, named),
            serde_json::Value::Array(items) => {
                for item in items {
                    visit(item, named);
                }
            }
            serde_json::Value::Object(fields) => {
                for field in fields.values() {
                    visit(field, named);
                }
            }
            _ => {}
        }
    }
    let mut named = BTreeSet::new();
    for value in arguments.values() {
        visit(value, &mut named);
    }
    named.into_iter().collect()
}

/// What a worker's refusal to open says, as [`crate::Replica::open`] says it.
fn open_error(reason: &str, app_id: &str) -> AppError {
    if reason.starts_with("no app ") {
        AppError::UnknownApp(app_id.to_owned())
    } else {
        AppError::Launch(reason.to_owned())
    }
}
