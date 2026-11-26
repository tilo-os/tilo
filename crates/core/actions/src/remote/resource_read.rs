//! Record reads outside their app: how an agent reads a workspace's records
//! (`docs/designs/app-language/agents.md`, "Reading records outside their app").
//!
//! A workspace's records are sealed per app: a connection holding that app's launch ownership opens
//! them. An agent is not an app. It is admitted acting for the person whose session it works in,
//! never session-delegated and holding no app's route, and reads one record at a time through
//! this API. Each read is decided by the check every reader of a record goes through (the lesser
//! of the agent's grants and the person's), again for every read, so a grant revoked or a member
//! removed in the middle of a session cuts the reads that follow. A read carries the record's
//! committed content and nothing that writes to it: the agent changes a record only by invoking
//! actions.
//!
//! A provider publishes resources itself too, which no app's record holds: a desktop's device,
//! its windows and its styles, Settings' sections. Those are answered on this same API as a
//! reading, the JSON the service reads, so a program, an agent and a view read one resource one
//! way.

use super::{ActionCallError, Caller};

/// What a provider without record reads answers.
pub const RESOURCE_READS_UNAVAILABLE: &str = "this provider reads no records outside their apps";

/// What a caller is told about a record it may not read.
pub const RESOURCE_READ_REFUSED: &str = "not allowed to read this record";

/// What a caller is told about a record it may not change.
pub const RESOURCE_WRITE_REFUSED: &str = "not allowed to change this record";

/// What a caller that is neither a person nor an agent acting for its person is told.
pub const RESOURCE_READS_FOR_AGENTS: &str = "only a person, an app acting for its person, or an agent \
     acting for the person who admitted it, reads records outside their app; a session a person's \
     device handed to an agent reads nothing here";

/// One record of a workspace, by the workspace it is in, its app (the id whose records it is) and
/// its id there (empty for the app's record of the workspace).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ResourceAddress {
    pub workspace: String,
    pub app: String,
    pub record: String,
}

/// A record a listing names: where it is, and the kind the workspace's store keeps it as. A
/// document that is no record of the store (the workspace's tree, a device's settings) names no
/// kind.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ListedResource {
    pub address: ResourceAddress,
    pub kind: Option<String>,
}

impl ListedResource {
    /// `address`, a document that is no record of the workspace's store.
    #[must_use]
    pub fn document(address: ResourceAddress) -> Self {
        Self {
            address,
            kind: None,
        }
    }

    /// The records a listing of `workspace` answered ([`answer_read`]), each of `app` where it
    /// names none.
    #[must_use]
    pub fn read_listing(workspace: &str, app: &str, listing: &serde_json::Value) -> Vec<Self> {
        let text = |record: &serde_json::Value, key: &str| {
            record
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        };
        listing
            .get("records")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .map(|record| Self {
                address: ResourceAddress {
                    workspace: workspace.to_owned(),
                    app: text(record, "app").unwrap_or_else(|| app.to_owned()),
                    record: text(record, "record").unwrap_or_default(),
                },
                kind: text(record, "kind"),
            })
            .collect()
    }
}

/// The app a device's own settings are read under ([`device_record_space`]): the record
/// `keymap` there is `devices/<device>/settings/keymap`.
pub const DEVICE_SETTINGS: &str = "settings";

/// The record space a device that no person is founded on keeps its own records in, as the
/// `workspace` of a [`ResourceAddress`]: `devices/<device hex>`. No workspace has this id (a
/// workspace's is a UUID), so a read naming it is a read of the device's own records, decided by
/// the device alone rather than by a workspace's governance.
///
/// This is the device case of the one record API: the same [`super::RemoteActionMessage::ResourceRead`],
/// follow and wake answer the records as any workspace's, so the greeter and the launcher read
/// the settings the device kept before anyone claimed it the way their views read any record.
#[must_use]
pub fn device_record_space(device: &[u8; 32]) -> String {
    format!("devices/{}", hex::encode(device))
}

/// What a caller that may not read a device's own settings is told: who may.
pub const DEVICE_SETTINGS_REFUSED: &str = "Only this device's person, from any of their devices, and whoever they granted there, read \
     its settings";

/// The app a device's engine jobs are read under ([`device_record_space`]): the record `job`
/// there is the job the device runs, at `devices/<device>/jobs/<job>`. A job is the device's,
/// never a workspace's: it lives only as long as the process that runs it.
pub const DEVICE_JOBS: &str = "jobs";

/// The address of the engine job `job` the device `device` runs, in the space
/// [`device_record_space`] names.
#[must_use]
pub fn device_job_address(device: &[u8; 32], job: &str) -> ResourceAddress {
    ResourceAddress {
        workspace: device_record_space(device),
        app: DEVICE_JOBS.to_owned(),
        record: job.to_owned(),
    }
}

/// The text form of a job's address, `devices/<device hex>/jobs/<job>`: the one form a view, a
/// script or a link writes a job in, and [`parse_job_path`] reads back.
#[must_use]
pub fn device_job_path(device: &[u8; 32], job: &str) -> String {
    format!("devices/{}/jobs/{job}", hex::encode(device))
}

/// Read the text form of a job's address ([`device_job_path`]) back: the device that runs it and
/// the job's id there. `None` for any other text, including a path whose job is empty or names
/// more than one part.
#[must_use]
pub fn parse_job_path(path: &str) -> Option<([u8; 32], String)> {
    let (device, job) = path.strip_prefix("devices/")?.split_once("/jobs/")?;
    if job.is_empty() || job.contains('/') || device.contains('/') {
        return None;
    }
    let mut key = [0; 32];
    hex::decode_to_slice(device, &mut key).ok()?;
    Some((key, job.to_owned()))
}

/// The device key `space` names, when it is a device record space
/// ([`device_record_space`]): `devices/<device hex>`.
#[must_use]
pub fn parse_device_record_space(space: &str) -> Option<[u8; 32]> {
    let hex = space.strip_prefix("devices/")?;
    if hex.len() != 64 || hex.contains('/') {
        return None;
    }
    let mut key = [0; 32];
    hex::decode_to_slice(hex, &mut key).ok()?;
    Some(key)
}

/// The address of the device record `record` the device `device` keeps: its settings, in the
/// space [`device_record_space`] names.
#[must_use]
pub fn device_record_address(device: &[u8; 32], record: &str) -> ResourceAddress {
    ResourceAddress {
        workspace: device_record_space(device),
        app: DEVICE_SETTINGS.to_owned(),
        record: record.to_owned(),
    }
}

/// What a read of one record answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResourceCopy {
    /// The record's committed content: an Automerge document, whole.
    Document(Vec<u8>),
    /// The resource as its app's package reads it: what the core read answers of it, who
    /// made each item and when included. A tilolang app's records are read this way, never as
    /// the documents that keep them.
    Read(serde_json::Value),
    /// A resource the provider publishes itself rather than from an app's record (device state, a
    /// desktop's windows and styles): its reading as the JSON its service reads, and the revision
    /// it stands at when the provider names one. A provider with no revision of its own leaves it
    /// `None`, and a reader's revision is the hash of the JSON then.
    Reading {
        value: serde_json::Value,
        revision: Option<String>,
    },
    /// The workspace holds no such record: nothing was ever written to it.
    Absent,
}

impl ResourceCopy {
    /// A reading of a resource the provider publishes, at the revision it names (or none).
    #[must_use]
    pub const fn reading(value: serde_json::Value, revision: Option<String>) -> Self {
        Self::Reading { value, revision }
    }
}

/// Whether a caller may read or change a record: allowed, or why not
/// ([`ResourceReads::may_read`], [`ResourceReads::may_write`]).
pub type Decided = Result<(), ActionCallError>;

/// What a reader answers, once it has it: a read may wait on a lock, a store or another device
/// over its link, so it is awaited where it is asked and never holds the thread that asked.
pub type ReadsAnswer<'a, T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, ActionCallError>> + Send + 'a>>;

/// A read asked over the API, answered once its reader has it ([`ResourceReads::answer_read`]).
pub type ReadingAnswer =
    std::pin::Pin<Box<dyn std::future::Future<Output = super::RemoteActionMessage> + Send>>;

/// `answer`, as a reader answers: its result's type is the one the reader answers, so a body
/// that answers at once is written as an `async move` block, with `?` and `return` as usual.
pub fn reads_answer<'a, T>(
    answer: impl std::future::Future<Output = Result<T, ActionCallError>> + Send + 'a,
) -> ReadsAnswer<'a, T> {
    Box::pin(answer)
}

/// What a provider without record reads answers any of them.
fn unavailable<'a, T: Send + 'a>() -> ReadsAnswer<'a, T> {
    Box::pin(std::future::ready(Err(ActionCallError::Unavailable {
        reason: RESOURCE_READS_UNAVAILABLE.into(),
    })))
}

/// A provider's API for reading records outside their app.
pub trait ResourceReads: Send + Sync {
    /// The record `address`, in the workspace it names, as `caller` may read it now: decided at
    /// that workspace's record node with the caller's actor, whichever workspace the caller's
    /// connection was admitted to. A provider that publishes a resource itself answers its
    /// reading here, as it answers an app's record.
    ///
    /// # Errors
    ///
    /// [`ActionCallError::NotPermitted`] for a caller that is not an agent task acting for its
    /// person, [`ActionCallError::Refused`] for a record the caller may not read,
    /// [`ActionCallError::Unavailable`] when the workspace's records cannot be read now (still
    /// arriving, or held nowhere on this device), or by a reader that answers only
    /// [`Self::answer_read`], the device on the other end of a session.
    fn read<'a>(
        &'a self,
        caller: &'a Caller,
        address: &'a ResourceAddress,
    ) -> ReadsAnswer<'a, ResourceCopy> {
        let _ = (caller, address);
        unavailable()
    }

    /// Whether `caller` may read `address` now, decided as [`Self::read`] is, without the copy:
    /// what a reader that keeps the record another way (an app's live route to it) asks first.
    /// [`ActionCallError::ConfirmationRequired`] where the person is to be asked first.
    fn may_read<'a>(
        &'a self,
        caller: &'a Caller,
        address: &'a ResourceAddress,
    ) -> ReadsAnswer<'a, ()> {
        Box::pin(async move { self.read(caller, address).await.map(|_| ()) })
    }

    /// Whether `caller` may change `address` now, decided there as a write is: what a reader that
    /// keeps the record another way (an app's live route to it) asks before it changes it. An app
    /// changes its own records; another app's record it changes only where the caller's grants
    /// there allow a change, so a grant that reads alone answers [`ActionCallError::Refused`].
    /// [`ActionCallError::ConfirmationRequired`] where the person is to be asked first.
    fn may_write<'a>(
        &'a self,
        caller: &'a Caller,
        address: &'a ResourceAddress,
    ) -> ReadsAnswer<'a, ()> {
        Box::pin(async move { self.read(caller, address).await.map(|_| ()) })
    }

    /// Answer a read of one of this provider's records, asked as `correlation` over the API
    /// ([`super::RemoteActionMessage::ResourceRead`]), with what every reader of it is answered:
    /// [`answer_read`], from this provider's own reads, unless the provider answers reads another
    /// way (the desktop answers its windows' reads on its own turn).
    ///
    /// The API posts a read here rather than to code of its own, so what a reading is made of —
    /// every copy as JSON, and the record format behind it — is reached only from the reader's
    /// code. A provider that serves an app's windows and reads no record through the API leaves
    /// [`super::ActionProvider::resource_reads`] `None` and carries none of it.
    fn answer_read(
        self: std::sync::Arc<Self>,
        caller: &Caller,
        correlation: u64,
        read: super::ResourceReadRequest,
    ) -> ReadingAnswer
    where
        Self: 'static,
    {
        Box::pin(answer_read(self, caller.clone(), correlation, read))
    }

    /// Wake `wake` once the next write to `address` lands on this provider, or what it publishes
    /// of the resource moves, decided as a read is: what a reader holding a copy waits on before
    /// it reads again. Watched before the read it guards, so no write between the two is missed.
    ///
    /// # Errors
    ///
    /// As [`Self::read`]; a provider that cannot tell when a record changes answers
    /// [`ActionCallError::Unavailable`].
    fn watch<'a>(
        &'a self,
        caller: &'a Caller,
        address: &'a ResourceAddress,
        wake: &'a ResourceWake,
    ) -> ReadsAnswer<'a, ()> {
        let _ = (caller, address, wake);
        unavailable()
    }

    /// The resources of `workspace` that `caller` may read now: `app`'s, or every app's where
    /// `app` is empty, each decided as its read would be. What a reader that keeps every resource
    /// of a workspace (an index, search) reads before it reads each.
    ///
    /// # Errors
    ///
    /// As [`Self::read`]; a provider that lists nothing answers [`ActionCallError::Unavailable`].
    fn list<'a>(
        &'a self,
        caller: &'a Caller,
        workspace: &'a str,
        app: &'a str,
    ) -> ReadsAnswer<'a, Vec<ListedResource>> {
        let _ = (caller, workspace, app);
        unavailable()
    }

    /// The device's store's answer to `read`, a read command of the store's wire (a resource, a
    /// page of a query, a count, a content), asked of the resources the app `app` keeps in
    /// `address`'s workspace as `caller` may read them now, decided as [`Self::read`] is. The command
    /// and its answer travel as JSON, so a reader of resources needs no store of its own.
    ///
    /// # Errors
    ///
    /// As [`Self::read`]; [`ActionCallError::InvalidRequest`] for a command that writes or does
    /// not parse. A provider whose resources are in no store answers
    /// [`ActionCallError::Unavailable`].
    fn store_read<'a>(
        &'a self,
        caller: &'a Caller,
        address: &'a ResourceAddress,
        read: serde_json::Value,
    ) -> ReadsAnswer<'a, serde_json::Value> {
        let _ = (caller, address, read);
        unavailable()
    }
}

/// Copies of records already in hand, by id, read as they are: what a reader planning with what
/// it holds reads. Any other record is absent, and nothing wakes a wait for a write.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HeldCopies(pub std::collections::HashMap<String, Vec<u8>>);

impl HeldCopies {
    /// `asks` read from these copies by `caller` now, as every reader is answered
    /// ([`ResourceReadings::readable`]).
    ///
    /// # Errors
    ///
    /// As [`read_resources`] and [`ResourceReadings::readable`].
    pub fn readable(
        &self,
        _caller: &Caller,
        asks: &[ResourceAsk],
    ) -> Result<serde_json::Value, String> {
        readings_of(asks, |address| Ok(self.held(address)))
            .map_err(|error| error.to_string())?
            .readable()
    }
}

impl HeldCopies {
    /// The copy of `address` in hand, or absent.
    fn held(&self, address: &ResourceAddress) -> ResourceCopy {
        self.0
            .get(&address.record)
            .map_or(ResourceCopy::Absent, |bytes| {
                ResourceCopy::Document(bytes.clone())
            })
    }
}

impl ResourceReads for HeldCopies {
    fn read<'a>(
        &'a self,
        _: &'a Caller,
        address: &'a ResourceAddress,
    ) -> ReadsAnswer<'a, ResourceCopy> {
        Box::pin(std::future::ready(Ok(self.held(address))))
    }

    fn watch<'a>(
        &'a self,
        _: &'a Caller,
        _: &'a ResourceAddress,
        _: &'a ResourceWake,
    ) -> ReadsAnswer<'a, ()> {
        Box::pin(std::future::ready(Ok(())))
    }
}

/// One record a reader asks for: alone, or `joined`, the chain of parts whose base it is
/// ([`crate::document_sync`]): part `0`, `1`, … up to the first the workspace holds none of. A
/// chain's head ([`crate::document_sync::chain_head`]) asked alone reads its newest part, under
/// that part's id: what a reader that follows a chain reads, whatever its length.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceAsk {
    pub address: ResourceAddress,
    pub joined: bool,
}

/// What a read of several records answered: each record read (a joined one as its parts), its
/// copy or `None` while it is still arriving, and the revision of them all, which changes with
/// any write to any of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceReadings {
    pub revision: String,
    pub records: Vec<(String, Option<ResourceCopy>)>,
}

/// One pass over `asks`: each record `read` answers (a joined one as its parts) and the revision of
/// them all, which changes with any write to any of them. A record `read` answers
/// [`ActionCallError::Unavailable`] for is still arriving and reads as `None`. A provider that waits on
/// writes watches a record inside `read`, before it reads it, so no write between the two is missed.
///
/// This walks records already in hand, which answer at once: the desktop's, which it reads on its
/// own turn. A reader that awaits its records walks them with [`readings_from`].
///
/// # Errors
///
/// What `read` refuses a record with, other than [`ActionCallError::Unavailable`].
pub fn readings_of(
    asks: &[ResourceAsk],
    mut read: impl FnMut(&ResourceAddress) -> Result<ResourceCopy, ActionCallError>,
) -> Result<ResourceReadings, ActionCallError> {
    let mut walk = Walk::default();
    for ask in asks {
        let mut asking = Asking::of(ask);
        while let Some(address) = asking.next() {
            let copy = held(read(&address))?;
            asking.took(&mut walk.revision, address, copy);
        }
        walk.records.extend(asking.answered());
    }
    Ok(walk.readings())
}

/// [`readings_of`], over records `read` answers once it has them.
///
/// # Errors
///
/// What `read` refuses a record with, other than [`ActionCallError::Unavailable`].
pub async fn readings_from<Read, Answer>(
    asks: &[ResourceAsk],
    mut read: Read,
) -> Result<ResourceReadings, ActionCallError>
where
    Read: FnMut(ResourceAddress) -> Answer,
    Answer: std::future::Future<Output = Result<ResourceCopy, ActionCallError>>,
{
    let mut walk = Walk::default();
    for ask in asks {
        let mut asking = Asking::of(ask);
        while let Some(address) = asking.next() {
            let copy = held(read(address.clone()).await)?;
            asking.took(&mut walk.revision, address, copy);
        }
        walk.records.extend(asking.answered());
    }
    Ok(walk.readings())
}

/// A record's copy as a reading holds it: `None` while it is still arriving, whose write wakes
/// the reader.
fn held(
    read: Result<ResourceCopy, ActionCallError>,
) -> Result<Option<ResourceCopy>, ActionCallError> {
    match read {
        Ok(copy) => Ok(Some(copy)),
        Err(ActionCallError::Unavailable { .. }) => Ok(None),
        Err(error) => Err(error),
    }
}

/// The records a walk over several asks read so far, and the revision of them all.
#[derive(Default)]
struct Walk {
    revision: blake3::Hasher,
    records: Vec<(String, Option<ResourceCopy>)>,
}

impl Walk {
    fn readings(self) -> ResourceReadings {
        ResourceReadings {
            revision: self.revision.finalize().to_hex().to_string(),
            records: self.records,
        }
    }
}

/// One ask's records as they are read: the record alone, or the parts of its chain one after
/// another up to the first that holds no document.
struct Asking<'a> {
    ask: &'a ResourceAsk,
    /// The chain's base, for an ask that reads one.
    base: Option<&'a str>,
    /// Whether the ask is a chain's head read alone: it answers the newest part.
    newest_only: bool,
    part: u64,
    done: bool,
    held: Vec<(String, Option<ResourceCopy>)>,
}

impl<'a> Asking<'a> {
    fn of(ask: &'a ResourceAsk) -> Self {
        let newest = match crate::document_sync::document_sync(&ask.address.record) {
            crate::document_sync::DocumentSync::ChainHead { base } if !ask.joined => Some(base),
            _ => None,
        };
        let base = newest.or_else(|| ask.joined.then_some(ask.address.record.as_str()));
        Self {
            ask,
            base,
            newest_only: newest.is_some(),
            part: 0,
            done: false,
            held: Vec::new(),
        }
    }

    /// The record to read next, `None` once the ask is read.
    fn next(&self) -> Option<ResourceAddress> {
        if self.done {
            return None;
        }
        let record = match self.base {
            Some(base) => crate::document_sync::document_part(base, self.part),
            None => self.ask.address.record.clone(),
        };
        Some(ResourceAddress {
            workspace: self.ask.address.workspace.clone(),
            app: self.ask.address.app.clone(),
            record,
        })
    }

    /// `address`, the record [`Self::next`] named, read as `copy`.
    fn took(
        &mut self,
        revision: &mut blake3::Hasher,
        address: ResourceAddress,
        copy: Option<ResourceCopy>,
    ) {
        let chained = self.base.is_some();
        // A reading's revision is the one its provider names; one naming none, and a record its
        // app reads for itself, are hashed by their JSON, which is all a reader can tell about
        // them.
        let hashed;
        let bytes: &[u8] = match &copy {
            Some(ResourceCopy::Document(bytes)) => bytes,
            Some(ResourceCopy::Reading {
                revision: Some(revision),
                ..
            }) => revision.as_bytes(),
            Some(
                ResourceCopy::Read(value)
                | ResourceCopy::Reading {
                    value,
                    revision: None,
                },
            ) => {
                hashed = value.to_string();
                hashed.as_bytes()
            }
            Some(ResourceCopy::Absent) => b"",
            None => b"unavailable",
        };
        for bytes in [address.record.as_bytes(), bytes] {
            revision.update(&(bytes.len() as u64).to_le_bytes());
            revision.update(bytes);
        }
        let last = !chained || !matches!(copy, Some(ResourceCopy::Document(_)));
        // The part after a chain's last is watched, not answered: its first write wakes.
        if !(chained && self.part > 0 && matches!(copy, Some(ResourceCopy::Absent))) {
            self.held.push((address.record, copy));
        }
        if last {
            self.done = true;
        } else {
            self.part = self.part.saturating_add(1);
        }
    }

    /// What the ask answers: a head the newest part alone, every part read staying in the
    /// revision, so a write to any of them, or a new part, wakes its reader.
    fn answered(mut self) -> Vec<(String, Option<ResourceCopy>)> {
        if self.newest_only
            && let Some(newest) = self
                .held
                .iter()
                .rposition(|(_, copy)| matches!(copy, Some(ResourceCopy::Document(_))))
                .or_else(|| self.held.len().checked_sub(1))
        {
            self.held = vec![self.held.swap_remove(newest)];
        }
        self.held
    }
}

/// Read `asks` for `caller` once they differ from the revision `after` or `deadline` passed:
/// each record is watched before it is read, so the next write to any of them wakes the wait.
///
/// # Errors
///
/// As [`ResourceReads::read`], but a record still arriving answers `None` rather than failing.
pub async fn read_resources<Reads: ResourceReads + ?Sized>(
    reads: &Reads,
    caller: &Caller,
    asks: &[ResourceAsk],
    after: Option<&str>,
    deadline: tilo_clock::time::Instant,
) -> Result<ResourceReadings, ActionCallError> {
    loop {
        let (wake, woken) = ResourceWake::new();
        let wake = &wake;
        let readings = readings_from(asks, |address| async move {
            reads.watch(caller, &address, wake).await?;
            reads.read(caller, &address).await
        })
        .await?;
        let now = tilo_clock::now();
        if after != Some(readings.revision.as_str()) || now >= deadline {
            return Ok(readings);
        }
        let mut woken = std::pin::pin!(woken);
        #[cfg(all(feature = "answer-reads", not(tilo_no_js)))]
        let mut passed = std::pin::pin!(tilo_rt::sleep(deadline.saturating_duration_since(now)));
        // A build without a runtime (a model module, or one that answers no reads) has no timer: it
        // only waits for the write.
        #[cfg(not(all(feature = "answer-reads", not(tilo_no_js))))]
        let mut passed = std::pin::pin!(std::future::pending::<()>());
        std::future::poll_fn(|cx| {
            if woken.as_mut().poll(cx).is_ready() || passed.as_mut().poll(cx).is_ready() {
                std::task::Poll::Ready(())
            } else {
                std::task::Poll::Pending
            }
        })
        .await;
    }
}

/// Answer `read`, asked as `correlation`, for `caller` from `reads`, the provider's record API:
/// awaited where it is asked, through the wait for a write past `read.after` and the record's
/// capture.
///
/// Every provider that reads records answers [`ResourceReads::answer_read`] with this, and the API
/// posts a read through that method rather than here: the reading, the JSON every reader is
/// answered and the record format behind it are reached only from a reader's own code, so a provider
/// that serves an app's windows and reads no record does not carry them.
pub async fn answer_read<Reads: ResourceReads + ?Sized>(
    reads: std::sync::Arc<Reads>,
    caller: Caller,
    correlation: u64,
    read: super::ResourceReadRequest,
) -> super::RemoteActionMessage {
    super::RemoteActionMessage::ResourceReading {
        correlation,
        result: serve_readings(reads.as_ref(), &caller, read).await,
    }
}

/// What `reads` answers `read` for `caller` through the record API's read message, as JSON
/// ([`ResourceReadings::readable`], or a listing): wherever the record is, and with `after`
/// once it moves past that revision or the wait went by.
///
/// # Errors
///
/// The reader's refusal, or an answer that is no reading.
pub async fn answered(
    reads: std::sync::Arc<dyn ResourceReads>,
    caller: &Caller,
    read: super::ResourceReadRequest,
) -> Result<serde_json::Value, ActionCallError> {
    match reads.answer_read(caller, 0, read).await {
        super::RemoteActionMessage::ResourceReading { result, .. } => {
            serde_json::from_str(&result?).map_err(|error| ActionCallError::Unavailable {
                reason: format!("the reading does not read: {error}"),
            })
        }
        _ => Err(ActionCallError::Unanswered),
    }
}

/// `reading`, answered where it is completed: polled first in the call that asked, so a reader
/// that has the record answers it there, and again on the thread each wait of it ends on, so a
/// reader that answers on its own turn — the desktop reading its windows' records — is delivered
/// inside that turn, and a follow asks again in it. No thread is parked on the read: whoever ends
/// a wait runs the rest of it.
///
/// A read is polled within a runtime's context ([`tilo_rt::Handle`]), so one that waits on a timer
/// has one wherever it is resumed.
#[cfg(all(feature = "answer-reads", not(tilo_no_js)))]
pub(crate) fn answered_where_it_ends(
    reading: ReadingAnswer,
) -> super::Pending<super::RemoteActionMessage> {
    let (answer, answered) = super::pending();
    let driven = std::sync::Arc::new(Driven {
        slot: std::sync::Mutex::new(Slot::Held(Held { reading, answer })),
        again: std::sync::atomic::AtomicBool::new(false),
    });
    driven.drive();
    answered
}

/// What a [`Driven`] holds between polls, if anything.
#[cfg(all(feature = "answer-reads", not(tilo_no_js)))]
enum Slot {
    /// The reading and the answer it completes.
    Held(Held),
    /// This thread polls it now: a wake meanwhile asks for one more poll.
    Polling,
    /// It answered, or the client went.
    Done,
}

#[cfg(all(feature = "answer-reads", not(tilo_no_js)))]
struct Held {
    reading: ReadingAnswer,
    answer: super::Completer<super::RemoteActionMessage>,
}

/// A read answered on the thread that ends its wait: [`Driven::drive`] polls it, and its waker —
/// given to whatever the read awaits — drives it again on the thread that completes that wait.
#[cfg(all(feature = "answer-reads", not(tilo_no_js)))]
struct Driven {
    slot: std::sync::Mutex<Slot>,
    /// A wake that came while the reading was polled on another thread.
    again: std::sync::atomic::AtomicBool,
}

#[cfg(all(feature = "answer-reads", not(tilo_no_js)))]
impl Driven {
    /// Poll the held reading as far as it goes. Called in the call that asked and on every wake,
    /// on whichever thread that is.
    fn drive(self: &std::sync::Arc<Self>) {
        use std::sync::atomic::Ordering;
        use tilo_lock::MutexRecover as _;

        loop {
            let mut slot = self.slot.lock_recover();
            let taken = std::mem::replace(&mut *slot, Slot::Polling);
            drop(slot);
            let held = match taken {
                Slot::Held(held) => held,
                Slot::Polling => {
                    // Another thread polls it: that poll ends with one more.
                    self.again.store(true, Ordering::Release);
                    return;
                }
                Slot::Done => {
                    *self.slot.lock_recover() = Slot::Done;
                    return;
                }
            };
            self.again.store(false, Ordering::Release);
            let Held {
                mut reading,
                answer,
            } = held;
            let waker = std::task::Waker::from(std::sync::Arc::clone(self));
            let mut context = std::task::Context::from_waker(&waker);
            // A read may wait on a timer, and resumed on a thread that runs no runtime it would
            // fail to find one: poll it in the context of this thread's runtime, or the app's.
            #[cfg(not(target_family = "wasm"))]
            let polled = {
                let runtime = tilo_rt::Handle::try_current()
                    .unwrap_or_else(|_| tilo_rt::app().handle().clone());
                let _entered = runtime.enter();
                reading.as_mut().poll(&mut context)
            };
            #[cfg(target_family = "wasm")]
            let polled = reading.as_mut().poll(&mut context);
            if let std::task::Poll::Ready(message) = polled {
                *self.slot.lock_recover() = Slot::Done;
                answer.complete(message);
                return;
            }
            let mut slot = self.slot.lock_recover();
            *slot = Slot::Held(Held { reading, answer });
            let again = self.again.swap(false, Ordering::Acquire);
            drop(slot);
            if !again {
                return;
            }
        }
    }
}

#[cfg(all(feature = "answer-reads", not(tilo_no_js)))]
impl std::task::Wake for Driven {
    fn wake(self: std::sync::Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &std::sync::Arc<Self>) {
        self.drive();
    }
}

/// What a provider that reads no records answers a read of one: the API posts every read through
/// [`ResourceReads::answer_read`], and a provider whose [`super::ActionProvider::resource_reads`] is `None`
/// answers as this one does.
pub(crate) fn unanswered(correlation: u64) -> super::Pending<super::RemoteActionMessage> {
    super::Pending::ready(super::RemoteActionMessage::ResourceReading {
        correlation,
        result: Err(ActionCallError::Unavailable {
            reason: RESOURCE_READS_UNAVAILABLE.into(),
        }),
    })
}

async fn serve_readings<Reads: ResourceReads + ?Sized>(
    reads: &Reads,
    caller: &Caller,
    read: super::ResourceReadRequest,
) -> Result<String, ActionCallError> {
    let write = read.answer == super::ReadAnswer::WriteDecision;
    if write || read.answer == super::ReadAnswer::Decision {
        let address = ResourceAddress {
            workspace: read.workspace,
            app: read.app,
            record: read.record,
        };
        let decided = if write {
            reads.may_write(caller, &address).await
        } else {
            reads.may_read(caller, &address).await
        };
        return decided.map(|()| "null".to_owned());
    }
    if read.answer == super::ReadAnswer::Listing {
        return listing(reads, caller, &read.workspace, &read.app).await;
    }
    let document = read.answer == super::ReadAnswer::Document;
    let deadline = super::wait::deadline(read.wait_ms.unwrap_or(0));
    let ask = [ResourceAsk {
        address: ResourceAddress {
            workspace: read.workspace,
            app: read.app,
            record: read.record,
        },
        joined: read.joined,
    }];
    let readings = read_resources(reads, caller, &ask, read.after.as_deref(), deadline).await?;
    if document {
        return Ok(readings.documents().to_string());
    }
    readings
        .readable()
        .map(|answer| answer.to_string())
        .map_err(|reason| ActionCallError::Refused { reason })
}

/// The resources of `workspace` `caller` may read of `app` (every app's, empty), as a listing
/// answers them: `{"records": [{"app", "record", "kind"}]}`, `kind` only for a record the
/// workspace's store keeps.
async fn listing<Reads: ResourceReads + ?Sized>(
    reads: &Reads,
    caller: &Caller,
    workspace: &str,
    app: &str,
) -> Result<String, ActionCallError> {
    let listed: Vec<serde_json::Value> = reads
        .list(caller, workspace, app)
        .await?
        .into_iter()
        .map(|listed| {
            let mut entry = serde_json::Map::new();
            entry.insert("app".into(), listed.address.app.into());
            entry.insert("record".into(), listed.address.record.into());
            if let Some(kind) = listed.kind {
                entry.insert("kind".into(), kind.into());
            }
            serde_json::Value::Object(entry)
        })
        .collect();
    Ok(serde_json::json!({ "records": listed }).to_string())
}

impl ResourceReadings {
    /// The readings as a reader that keeps the documents is answered: as `readable`, with
    /// each document `{"record", "document"}`, whole, in hex. A reading a provider publishes
    /// itself, which is no document, is answered as `readable` answers it.
    ///
    /// # Panics
    /// Never: a reading's JSON always encodes.
    #[must_use]
    pub fn documents(self) -> serde_json::Value {
        let records: Vec<serde_json::Value> = self
            .records
            .into_iter()
            .map(|(record, copy)| match copy {
                None => serde_json::json!({"record": record, "arriving": true}),
                Some(ResourceCopy::Absent) => serde_json::json!({"record": record, "absent": true}),
                Some(ResourceCopy::Document(bytes)) => {
                    serde_json::json!({"record": record, "document": hex::encode(bytes)})
                }
                Some(ResourceCopy::Read(value) | ResourceCopy::Reading { value, .. }) => {
                    serde_json::json!({"record": record, "value": value})
                }
            })
            .collect();
        serde_json::json!({"revision": self.revision, "records": records})
    }

    /// The readings as every reader is answered: `{"revision", "records": [...]}`, each record
    /// `{"record", "value"}` ([`ResourceCopy::readable`], a document or a reading alike),
    /// `{"record", "absent": true}` for one the workspace holds none of, or `{"record",
    /// "arriving": true}` for one still arriving.
    ///
    /// # Errors
    ///
    /// When a copy is a document this build reads none of (feature `document-read`).
    pub fn readable(self) -> Result<serde_json::Value, String> {
        let mut records = Vec::with_capacity(self.records.len());
        for (record, copy) in self.records {
            records.push(match copy {
                None => serde_json::json!({"record": record, "arriving": true}),
                Some(ResourceCopy::Absent) => serde_json::json!({"record": record, "absent": true}),
                Some(copy) => serde_json::json!({"record": record, "value": copy.readable()?}),
            });
        }
        Ok(serde_json::json!({"revision": self.revision, "records": records}))
    }
}

impl ResourceCopy {
    /// The record as any reader reads it, whatever app keeps it: its maps as objects, lists as
    /// arrays, text as strings, counters and scalars as numbers, strings and booleans, bytes as
    /// their count, a data enum as its `$variant` beside its fields by name; a reading as the JSON
    /// its provider publishes; `null` for a record the workspace holds none of.
    ///
    /// A rich text reads with its paragraphs as lists in their order, each paragraph with its
    /// id, its kind, its words and its attributes, and without the key of bytes that places
    /// it: its body, its page header and footer, a note's and a comment's lines, and what a
    /// paragraph holds (`tilo_automerge::rich_text`). So every reader shows a text in order
    /// and none orders it again.
    ///
    /// # Errors
    ///
    /// When the copy is no Automerge document, or is one this build reads none of (feature
    /// `document-read` carries the reader).
    pub fn readable(&self) -> Result<serde_json::Value, String> {
        match self {
            Self::Absent => Ok(serde_json::Value::Null),
            Self::Read(value) | Self::Reading { value, .. } => Ok(value.clone()),
            Self::Document(bytes) => {
                #[cfg(feature = "document-read")]
                {
                    let doc =
                        automerge::Automerge::load(bytes).map_err(|error| error.to_string())?;
                    let mut value = tilo_automerge::json::read_document(&doc)
                        .map_err(|error| error.to_string())?;
                    name_variant_fields(&mut value);
                    Ok(value)
                }
                #[cfg(not(feature = "document-read"))]
                {
                    let _ = bytes;
                    Err("this build reads no record as a document".to_owned())
                }
            }
        }
    }
}

/// Each data enum in `value` with its fields by name: a record keeps a variant's fields under
/// `$field/<variant>/<field>` (`%` and `/` escaped), so no two variants' fields share a key.
#[cfg(feature = "document-read")]
fn name_variant_fields(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Array(items) => items.iter_mut().for_each(name_variant_fields),
        serde_json::Value::Object(map) => {
            map.values_mut().for_each(name_variant_fields);
            let Some(variant) = map.get("$variant").and_then(serde_json::Value::as_str) else {
                return;
            };
            let prefix = format!(
                "$field/{}/",
                variant.replace('%', "%25").replace('/', "%2F")
            );
            let stored: Vec<String> = map
                .keys()
                .filter(|key| key.starts_with(&prefix))
                .cloned()
                .collect();
            for key in stored {
                if let Some(field) = map.remove(&key) {
                    let name = key
                        .get(prefix.len()..)
                        .unwrap_or_default()
                        .replace("%2F", "/")
                        .replace("%25", "%");
                    map.insert(name, field);
                }
            }
        }
        _ => {}
    }
}

/// Woken once, by the first write to any record it watches ([`ResourceReads::watch`]).
#[derive(Clone)]
pub struct ResourceWake(std::sync::Arc<std::sync::Mutex<Option<super::Completer<()>>>>);

impl ResourceWake {
    /// A wake, and what its reader waits on.
    #[must_use]
    pub fn new() -> (Self, super::Pending<()>) {
        let (woken, waiting) = super::pending();
        (
            Self(std::sync::Arc::new(std::sync::Mutex::new(Some(woken)))),
            waiting,
        )
    }

    /// A write landed: wake the reader, once.
    pub fn wake(&self) {
        if let Some(woken) = self.0.lock().ok().and_then(|mut woken| woken.take()) {
            woken.complete(());
        }
    }

    /// Whether this wake can still wake anyone: not woken yet, and its reader still waits.
    #[must_use]
    pub fn waiting(&self) -> bool {
        self.0
            .lock()
            .is_ok_and(|woken| woken.as_ref().is_some_and(|woken| !woken.is_abandoned()))
    }
}

#[cfg(test)]
mod tests {
    use automerge::transaction::Transactable as _;
    use automerge::{AutoCommit, ObjType, ROOT};

    use super::*;

    fn message(text: &str) -> Vec<u8> {
        let mut doc = AutoCommit::new();
        let messages = doc.put_object(ROOT, "messages", ObjType::List).unwrap();
        let first = doc.insert_object(&messages, 0, ObjType::Map).unwrap();
        let body = doc.put_object(&first, "text", ObjType::Text).unwrap();
        doc.splice_text(&body, 0, 0, text).unwrap();
        doc.put(&first, "likes", automerge::ScalarValue::counter(2))
            .unwrap();
        doc.put(&first, "avatar", vec![1u8, 2, 3]).unwrap();
        doc.save()
    }

    fn ask(record: &str, joined: bool) -> ResourceAsk {
        ResourceAsk {
            address: ResourceAddress {
                workspace: "w".into(),
                app: "so.tilo.chat".into(),
                record: record.into(),
            },
            joined,
        }
    }

    /// [`super::read_resources`], waited for on a runtime of its own.
    fn read_resources(
        reads: &(impl ResourceReads + ?Sized),
        caller: &Caller,
        asks: &[ResourceAsk],
        after: Option<&str>,
        deadline: tilo_clock::time::Instant,
    ) -> Result<ResourceReadings, ActionCallError> {
        tilo_rt::block_on(super::read_resources(reads, caller, asks, after, deadline)).unwrap()
    }

    fn values(readings: &ResourceReadings) -> Vec<(String, serde_json::Value)> {
        readings
            .records
            .iter()
            .map(|(record, copy)| (record.clone(), copy.as_ref().unwrap().readable().unwrap()))
            .collect()
    }

    tilo_test_env::scenario! {
        fn a_record_reads_as_plain_data;

        phase a_record_reads_as_plain_data_with_its_text_counters_and_bytes;
        {
            let mut held = HeldCopies::default();
            held.0.insert("note".into(), message("hello"));
            let caller = Caller::local_person("me");
            let now = tilo_clock::now();
            let read = read_resources(
                &held,
                &caller,
                &[ask("note", false), ask("gone", false)],
                None,
                now,
            )
            .unwrap();
            assert_eq!(
                values(&read),
                [
                    (
                        "note".to_owned(),
                        serde_json::json!({"messages": [
                            {"text": "hello", "likes": 2, "avatar": {"bytes": 3}}
                        ]})
                    ),
                    ("gone".to_owned(), serde_json::Value::Null),
                ]
            );
        }

        phase a_data_enum_reads_with_its_fields_by_name;
        {
            let mut doc = AutoCommit::new();
            let attached = doc.put_object(ROOT, "attached", ObjType::Map).unwrap();
            doc.put(&attached, "$variant", "file").unwrap();
            doc.put(&attached, "$field/file/blob", "ab").unwrap();
            doc.put(&attached, "$field/file/a%2Fb", 3).unwrap();
            assert_eq!(
                ResourceCopy::Document(doc.save()).readable().unwrap(),
                serde_json::json!({"attached": {"$variant": "file", "blob": "ab", "a/b": 3}})
            );
        }

        phase a_rich_text_reads_as_its_paragraphs_in_their_order;
        {
            // A rich text under a field, as the word processor keeps a document's body: three
            // paragraphs, written in another order than they stand in.
            let mut doc = AutoCommit::new();
            doc.put(ROOT, "title", "A poem").unwrap();
            let body = doc.put_object(ROOT, "body", ObjType::Map).unwrap();
            doc.put(&body, "schema", "tilo.richtext").unwrap();
            let blocks = doc.put_object(&body, "blocks", ObjType::Map).unwrap();
            for (id, place, kind, words) in [
                ("p1", vec![192_u8], "list_item", "and so are you"),
                ("p2", vec![64], "heading", "A poem"),
                ("p3", vec![128], "paragraph", "Roses are red"),
            ] {
                let block = doc.put_object(&blocks, id, ObjType::Map).unwrap();
                doc.put(&block, "order", place).unwrap();
                doc.put(&block, "kind", kind).unwrap();
                let text = doc.put_object(&block, "text", ObjType::Text).unwrap();
                doc.splice_text(&text, 0, 0, words).unwrap();
            }
            assert_eq!(
                ResourceCopy::Document(doc.save()).readable().unwrap(),
                serde_json::json!({"title": "A poem", "body": {
                    "schema": "tilo.richtext",
                    "blocks": [
                        {"id": "p2", "kind": "heading", "text": "A poem"},
                        {"id": "p3", "kind": "paragraph", "text": "Roses are red"},
                        {"id": "p1", "kind": "list_item", "text": "and so are you"},
                    ],
                }})
            );
        }

        phase a_chain_reads_joined_up_to_its_last_part_and_a_new_part_changes_its_revision;
        {
            let mut held = HeldCopies::default();
            let part = |index| crate::document_sync::document_part("chain", index);
            held.0.insert(part(0), message("first"));
            held.0.insert(part(1), message("second"));
            let caller = Caller::local_person("me");
            let now = tilo_clock::now();
            let read = read_resources(&held, &caller, &[ask("chain", true)], None, now).unwrap();
            let texts: Vec<_> = values(&read)
                .into_iter()
                .map(|(record, value)| (record, value["messages"][0]["text"].clone()))
                .collect();
            assert_eq!(
                texts,
                [
                    (part(0), serde_json::json!("first")),
                    (part(1), serde_json::json!("second")),
                ]
            );
            let same = read_resources(&held, &caller, &[ask("chain", true)], None, now).unwrap();
            assert_eq!(same.revision, read.revision);
            held.0.insert(part(2), message("third"));
            let grown = read_resources(
                &held,
                &caller,
                &[ask("chain", true)],
                Some(&read.revision),
                now,
            )
            .unwrap();
            assert_ne!(grown.revision, read.revision);
            assert_eq!(grown.records.len(), 3);
            // The head is the newest part alone, its revision moved by a new part.
            let newest_part = crate::document_sync::chain_head("chain");
            let newest = read_resources(&held, &caller, &[ask(&newest_part, false)], None, now).unwrap();
            assert_eq!(
                values(&newest),
                [(
                    part(2),
                    serde_json::json!({"messages": [
                        {"text": "third", "likes": 2, "avatar": {"bytes": 3}}
                    ]})
                )]
            );
            held.0.insert(part(3), message("fourth"));
            let moved = read_resources(&held, &caller, &[ask(&newest_part, false)], None, now).unwrap();
            assert_ne!(moved.revision, newest.revision);
            assert_eq!(moved.records[0].0, part(3));
        }

        phase a_reading_reads_as_its_json_and_a_change_wakes_the_follow;
        {
            /// A resource a provider publishes itself: its JSON, the revision it names, and the
            /// follows watching it, woken when it moves.
            struct Published {
                value: std::sync::Mutex<serde_json::Value>,
                revision: std::sync::Mutex<String>,
                wakers: std::sync::Mutex<Vec<ResourceWake>>,
            }

            impl Published {
                fn move_to(&self, value: serde_json::Value, revision: &str) {
                    *self.value.lock().unwrap() = value;
                    *self.revision.lock().unwrap() = revision.to_owned();
                    let woken = std::mem::take(&mut *self.wakers.lock().unwrap());
                    for wake in woken {
                        wake.wake();
                    }
                }
            }

            impl ResourceReads for Published {
                fn read<'a>(
                    &'a self,
                    _: &'a Caller,
                    _: &'a ResourceAddress,
                ) -> ReadsAnswer<'a, ResourceCopy> {
                    Box::pin(async move {
                    Ok(ResourceCopy::reading(
                        self.value.lock().unwrap().clone(),
                        Some(self.revision.lock().unwrap().clone()),
                    ))
                    })
                }

                fn watch<'a>(
                    &'a self,
                    _: &'a Caller,
                    _: &'a ResourceAddress,
                    wake: &'a ResourceWake,
                ) -> ReadsAnswer<'a, ()> {
                    Box::pin(async move {
                    self.wakers.lock().unwrap().push(wake.clone());
                    Ok(())
                    })
                }
            }

            let published = std::sync::Arc::new(Published {
                value: std::sync::Mutex::new(serde_json::json!({"playing": false})),
                revision: std::sync::Mutex::new("a".into()),
                wakers: std::sync::Mutex::default(),
            });
            let caller = Caller::local_person("me");
            let asks = [ResourceAsk {
                address: ResourceAddress {
                    workspace: "w".into(),
                    app: "shell".into(),
                    record: "session:media".into(),
                },
                joined: false,
            }];
            let now = tilo_clock::now();
            let read = read_resources(&*published, &caller, &asks, None, now).unwrap();
            assert_eq!(
                values(&read),
                [(
                    "session:media".to_owned(),
                    serde_json::json!({"playing": false})
                )],
                "a reading reads as the JSON its provider publishes"
            );
            assert!(!read.revision.is_empty(), "the reading has a revision");
            // The first read watched too; only what the follow watches is woken here.
            published.wakers.lock().unwrap().clear();
            let changed = std::thread::scope(|scope| {
                let waiting = scope.spawn(|| {
                    read_resources(
                        &*published,
                        &caller,
                        &asks,
                        Some(&read.revision),
                        tilo_clock::now()
                            .checked_add(std::time::Duration::from_secs(30))
                            .unwrap(),
                    )
                });
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                loop {
                    if published.wakers.lock().unwrap().is_empty() {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "the follow watched nothing"
                        );
                        std::thread::yield_now();
                        continue;
                    }
                    break;
                }
                published.move_to(serde_json::json!({"playing": true}), "b");
                waiting.join().expect("the follow")
            })
            .unwrap();
            assert_eq!(
                values(&changed),
                [(
                    "session:media".to_owned(),
                    serde_json::json!({"playing": true})
                )],
                "the change woke the follow, which read the new reading"
            );
            assert_ne!(changed.revision, read.revision);
        }

        phase a_reading_its_provider_names_no_revision_for_is_hashed_by_its_json;
        {
            struct Unversioned(std::sync::Mutex<serde_json::Value>);

            impl ResourceReads for Unversioned {
                fn read<'a>(
                    &'a self,
                    _: &'a Caller,
                    _: &'a ResourceAddress,
                ) -> ReadsAnswer<'a, ResourceCopy> {
                    Box::pin(async move {
                    Ok(ResourceCopy::reading(
                        self.0.lock().unwrap().clone(),
                        None,
                    ))
                    })
                }

                fn watch<'a>(
                    &'a self,
                    _: &'a Caller,
                    _: &'a ResourceAddress,
                    _: &'a ResourceWake,
                ) -> ReadsAnswer<'a, ()> {
                    Box::pin(async move {
                    Ok(())
                    })
                }
            }

            let published = Unversioned(std::sync::Mutex::new(serde_json::json!({"level": 1})));
            let caller = Caller::local_person("me");
            let asks = [ask("session:volume", false)];
            let now = tilo_clock::now();
            let first = read_resources(&published, &caller, &asks, None, now).unwrap();
            let again = read_resources(&published, &caller, &asks, None, now).unwrap();
            assert_eq!(first.revision, again.revision, "the same JSON is one revision");
            *published.0.lock().unwrap() = serde_json::json!({"level": 2});
            let moved = read_resources(&published, &caller, &asks, None, now).unwrap();
            assert_ne!(moved.revision, first.revision, "another JSON is another");
        }
    }
}
