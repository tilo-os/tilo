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

    /// The records a listing of `workspace` answered (`tilo_action_calls::answer_read`), each of
    /// `app` where it names none.
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
    /// ([`super::RemoteActionMessage::ResourceRead`]), with what every reader of it is answered.
    /// A provider that reads records answers with `tilo_action_calls::answer_read`, from its own
    /// reads, unless it answers reads another way (the desktop answers its windows' reads on its
    /// own turn). Every implementor names how it answers.
    ///
    /// The API posts a read here rather than to code of its own, so what a reading is made of —
    /// every copy as JSON, and the record format behind it — is reached only from the code that
    /// answers calls. A provider that serves an app's windows and reads no record through the API
    /// leaves [`super::ActionProvider::resource_reads`] `None` and carries none of it.
    fn answer_read(
        self: std::sync::Arc<Self>,
        caller: &Caller,
        correlation: u64,
        read: super::ResourceReadRequest,
    ) -> ReadingAnswer
    where
        Self: 'static;

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
/// it holds reads. Any other record is absent, and nothing wakes a wait for a write. Its reading
/// for a set of asks is `tilo_action_calls::copies_readable`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HeldCopies(pub std::collections::HashMap<String, Vec<u8>>);

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

    /// Copies in hand answer a planning reader's asks, which `tilo_action_calls::read_resources`
    /// walks without a wait; a read posted over the API, which may list, follow or decide, is not
    /// one of them.
    fn answer_read(
        self: std::sync::Arc<Self>,
        _: &Caller,
        correlation: u64,
        _: super::ResourceReadRequest,
    ) -> ReadingAnswer
    where
        Self: 'static,
    {
        Box::pin(std::future::ready(
            super::RemoteActionMessage::ResourceReading {
                correlation,
                result: Err(ActionCallError::Unavailable {
                    reason: "these copies answer no read asked over the API".into(),
                }),
            },
        ))
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

/// What a provider that reads no records answers a read of one: the API posts every read through
/// [`ResourceReads::answer_read`], and a provider whose [`super::ActionProvider::resource_reads`] is `None`
/// answers as this one does.
pub fn unanswered(correlation: u64) -> super::Pending<super::RemoteActionMessage> {
    super::Pending::ready(super::RemoteActionMessage::ResourceReading {
        correlation,
        result: Err(ActionCallError::Unavailable {
            reason: RESOURCE_READS_UNAVAILABLE.into(),
        }),
    })
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
