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

/// Whether a caller may read a record: allowed, or why not ([`ResourceReads::may_read`]).
pub type Decided = Result<(), ActionCallError>;

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
    fn read(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
    ) -> Result<ResourceCopy, ActionCallError> {
        let _ = (caller, address);
        Err(ActionCallError::Unavailable {
            reason: RESOURCE_READS_UNAVAILABLE.into(),
        })
    }

    /// Whether `caller` may read `address` now, decided as [`Self::read`] is, without the copy:
    /// what a reader that keeps the record another way (an app's live route to it) asks first.
    /// [`ActionCallError::ConfirmationRequired`] where the person is to be asked first.
    fn may_read(&self, caller: &Caller, address: &ResourceAddress) -> super::Pending<Decided> {
        super::Pending::ready(self.read(caller, address).map(|_| ()))
    }

    /// Answer a read of one of this provider's records, asked as `correlation` over the API
    /// ([`super::RemoteActionMessage::ResourceRead`]), with what every reader of it is answered:
    /// [`answer_read`], which every provider that reads records through this trait answers with.
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
    ) -> super::Pending<super::RemoteActionMessage>;

    /// Wake `wake` once the next write to `address` lands on this provider, or what it publishes
    /// of the resource moves, decided as a read is: what a reader holding a copy waits on before
    /// it reads again. Watched before the read it guards, so no write between the two is missed.
    ///
    /// # Errors
    ///
    /// As [`Self::read`]; a provider that cannot tell when a record changes answers
    /// [`ActionCallError::Unavailable`].
    fn watch(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
        wake: &ResourceWake,
    ) -> Result<(), ActionCallError> {
        let _ = (caller, address, wake);
        Err(ActionCallError::Unavailable {
            reason: RESOURCE_READS_UNAVAILABLE.into(),
        })
    }

    /// The resources of `workspace` that `caller` may read now: `app`'s, or every app's where
    /// `app` is empty, each decided as its read would be. What a reader that keeps every resource
    /// of a workspace (an index, search) reads before it reads each.
    ///
    /// # Errors
    ///
    /// As [`Self::read`]; a provider that lists nothing answers [`ActionCallError::Unavailable`].
    fn list(
        &self,
        caller: &Caller,
        workspace: &str,
        app: &str,
    ) -> Result<Vec<ResourceAddress>, ActionCallError> {
        let _ = (caller, workspace, app);
        Err(ActionCallError::Unavailable {
            reason: RESOURCE_READS_UNAVAILABLE.into(),
        })
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
    fn store_read(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
        read: serde_json::Value,
    ) -> Result<serde_json::Value, ActionCallError> {
        let _ = (caller, address, read);
        Err(ActionCallError::Unavailable {
            reason: RESOURCE_READS_UNAVAILABLE.into(),
        })
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
        caller: &Caller,
        asks: &[ResourceAsk],
    ) -> Result<serde_json::Value, String> {
        read_resources(self, caller, asks, None, tilo_clock::now())
            .map_err(|error| error.to_string())?
            .readable()
    }
}

impl ResourceReads for HeldCopies {
    fn answer_read(
        self: std::sync::Arc<Self>,
        caller: &Caller,
        correlation: u64,
        read: super::ResourceReadRequest,
    ) -> super::Pending<super::RemoteActionMessage> {
        answer_read(self, caller, correlation, read)
    }

    fn read(&self, _: &Caller, address: &ResourceAddress) -> Result<ResourceCopy, ActionCallError> {
        Ok(self
            .0
            .get(&address.record)
            .map_or(ResourceCopy::Absent, |bytes| {
                ResourceCopy::Document(bytes.clone())
            }))
    }

    fn watch(
        &self,
        _: &Caller,
        _: &ResourceAddress,
        _: &ResourceWake,
    ) -> Result<(), ActionCallError> {
        Ok(())
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
/// Every provider answers a read with this pass, whatever it waits with: [`read_resources`] on a thread
/// of its own, a desktop that serves its windows in process when its records next change.
///
/// # Errors
///
/// What `read` refuses a record with, other than [`ActionCallError::Unavailable`].
pub fn readings_of(
    asks: &[ResourceAsk],
    mut read: impl FnMut(&ResourceAddress) -> Result<ResourceCopy, ActionCallError>,
) -> Result<ResourceReadings, ActionCallError> {
    let mut revision = blake3::Hasher::new();
    let mut records = Vec::new();
    for ask in asks {
        let newest_only = match crate::document_sync::document_sync(&ask.address.record) {
            crate::document_sync::DocumentSync::ChainHead { base } if !ask.joined => Some(base),
            _ => None,
        };
        let chained = ask.joined || newest_only.is_some();
        let base = newest_only.unwrap_or(&ask.address.record);
        let mut held = Vec::new();
        let mut part = 0;
        loop {
            let record = if chained {
                crate::document_sync::document_part(base, part)
            } else {
                ask.address.record.clone()
            };
            let address = ResourceAddress {
                workspace: ask.address.workspace.clone(),
                app: ask.address.app.clone(),
                record,
            };
            let copy = match read(&address) {
                Ok(copy) => Some(copy),
                // A record still arriving is not held yet: its write wakes the reader.
                Err(ActionCallError::Unavailable { .. }) => None,
                Err(error) => return Err(error),
            };
            // A reading's revision is the one its provider names; one naming none, and a record
            // its app reads for itself, are hashed by their JSON, which is all a reader can tell
            // about them.
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
            if !(chained && part > 0 && matches!(copy, Some(ResourceCopy::Absent))) {
                held.push((address.record, copy));
            }
            if last {
                break;
            }
            part = part.saturating_add(1);
        }
        // A head answers the newest part alone; every part read stays in the revision, so
        // a write to any of them, or a new part, wakes its reader.
        if newest_only.is_some()
            && let Some(newest) = held
                .iter()
                .rposition(|(_, copy)| matches!(copy, Some(ResourceCopy::Document(_))))
                .or_else(|| held.len().checked_sub(1))
        {
            held = vec![held.swap_remove(newest)];
        }
        records.extend(held);
    }
    Ok(ResourceReadings {
        revision: revision.finalize().to_hex().to_string(),
        records,
    })
}

/// Read `asks` for `caller` once they differ from the revision `after` or `deadline` passed:
/// each record is watched before it is read, so the next write to any of them wakes the wait.
///
/// # Errors
///
/// As [`ResourceReads::read`], but a record still arriving answers `None` rather than failing.
pub fn read_resources(
    reads: &dyn ResourceReads,
    caller: &Caller,
    asks: &[ResourceAsk],
    after: Option<&str>,
    deadline: tilo_clock::time::Instant,
) -> Result<ResourceReadings, ActionCallError> {
    loop {
        let (wake, woken) = ResourceWake::new();
        let readings = readings_of(asks, |address| {
            reads
                .watch(caller, address, &wake)
                .and_then(|()| reads.read(caller, address))
        })?;
        let now = tilo_clock::now();
        if after != Some(readings.revision.as_str()) || now >= deadline {
            return Ok(readings);
        }
        let _ = woken.wait_timeout(deadline.saturating_duration_since(now));
    }
}

/// Answer `read`, asked as `correlation`, for `caller` from `reads`, the provider's record API: on
/// a thread of its own, which the wait for a write past `read.after` and the record's capture hold.
///
/// Every provider that reads records answers [`ResourceReads::answer_read`] with this, and the API
/// posts a read through that method rather than here: the reading, the JSON every reader is
/// answered and the record format behind it are reached only from a reader's own code, so a provider
/// that serves an app's windows and reads no record does not carry them.
pub fn answer_read(
    reads: std::sync::Arc<dyn ResourceReads>,
    caller: &Caller,
    correlation: u64,
    read: super::ResourceReadRequest,
) -> super::Pending<super::RemoteActionMessage> {
    serve_readings(reads, caller, read).map(move |result| {
        super::RemoteActionMessage::ResourceReading {
            correlation,
            result,
        }
    })
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
        Some(super::RemoteActionMessage::ResourceReading { result, .. }) => {
            serde_json::from_str(&result?).map_err(|error| ActionCallError::Unavailable {
                reason: format!("the reading does not read: {error}"),
            })
        }
        _ => Err(ActionCallError::Unanswered),
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

fn serve_readings(
    reads: std::sync::Arc<dyn ResourceReads>,
    caller: &Caller,
    read: super::ResourceReadRequest,
) -> super::Pending<Result<String, ActionCallError>> {
    let (answer, answered) = super::pending();
    let caller = caller.clone();
    if read.answer == super::ReadAnswer::Decision {
        let address = ResourceAddress {
            workspace: read.workspace,
            app: read.app,
            record: read.record,
        };
        // A reader may block to decide; without a thread the caller hears the read dropped.
        let _ = std::thread::Builder::new()
            .name("resource-decision".into())
            .spawn(move || {
                reads.may_read(&caller, &address).then(move |decided| {
                    answer.complete(
                        decided
                            .unwrap_or(Err(ActionCallError::Unanswered))
                            .map(|()| "null".to_owned()),
                    );
                });
            });
        return answered;
    }
    if read.answer == super::ReadAnswer::Listing {
        // Without a thread the completer goes with the closure: the caller hears the read dropped.
        let _ = std::thread::Builder::new()
            .name("resource-listing".into())
            .spawn(move || {
                answer.complete(listing(reads.as_ref(), &caller, &read.workspace, &read.app));
            });
        return answered;
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
    // Without a thread the completer goes with the closure: the caller hears the read dropped.
    let _ = std::thread::Builder::new()
        .name("record-read".into())
        .spawn(move || {
            answer.complete(
                read_resources(
                    reads.as_ref(),
                    &caller,
                    &ask,
                    read.after.as_deref(),
                    deadline,
                )
                .and_then(|readings| {
                    if document {
                        return Ok(readings.documents().to_string());
                    }
                    readings
                        .readable()
                        .map(|answer| answer.to_string())
                        .map_err(|reason| ActionCallError::Refused { reason })
                }),
            );
        });
    answered
}

/// The resources of `workspace` `caller` may read of `app` (every app's, empty), as a listing
/// answers them: `{"records": [{"app", "record"}]}`.
fn listing(
    reads: &dyn ResourceReads,
    caller: &Caller,
    workspace: &str,
    app: &str,
) -> Result<String, ActionCallError> {
    let listed: Vec<serde_json::Value> = reads
        .list(caller, workspace, app)?
        .into_iter()
        .map(|address| serde_json::json!({"app": address.app, "record": address.record}))
        .collect();
    Ok(serde_json::json!({ "records": listed }).to_string())
}

impl ResourceReadings {
    /// The readings as a reader that keeps the documents is answered: as [`Self::readable`], with
    /// each document `{"record", "document"}`, whole, in hex. A reading a provider publishes
    /// itself, which is no document, is answered as [`Self::readable`] answers it.
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
                Some(copy) => serde_json::json!({
                    "record": record,
                    "value": copy.readable().unwrap_or(serde_json::Value::Null),
                }),
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
    /// When a copy is no Automerge document.
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
    /// paragraph holds ([`tilo_automerge::rich_text`]). So every reader shows a text in order
    /// and none orders it again.
    ///
    /// # Errors
    ///
    /// When the copy is no Automerge document.
    pub fn readable(&self) -> Result<serde_json::Value, String> {
        match self {
            Self::Absent => Ok(serde_json::Value::Null),
            Self::Read(value) | Self::Reading { value, .. } => Ok(value.clone()),
            Self::Document(bytes) => {
                let doc = automerge::Automerge::load(bytes).map_err(|error| error.to_string())?;
                let mut value =
                    tilo_automerge::json::read_document(&doc).map_err(|error| error.to_string())?;
                name_variant_fields(&mut value);
                Ok(value)
            }
        }
    }
}

/// Each data enum in `value` with its fields by name: a record keeps a variant's fields under
/// `$field/<variant>/<field>` (`%` and `/` escaped), so no two variants' fields share a key.
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

/// An action to run on one record at the record's origin, as an agent task invokes it: the
/// record (by its app and its id there), the action's subject resource and the action, its
/// arguments, and what the task's preview planned, when it planned.
#[derive(Clone, Debug, PartialEq)]
pub struct ResourceChange {
    pub app: String,
    pub record: String,
    pub resource: String,
    pub action: String,
    pub arguments: serde_json::Map<String, serde_json::Value>,
    pub preview: Option<serde_json::Value>,
}

/// A provider's API for an agent task's changes to records outside their apps. A change goes to
/// the record's origin, which runs the action against the record as it is then, applies what it
/// wrote as the task's step and answers with its JSON: `{"ok": {value, resources}, "differs":
/// bool}`, or `{"failed": why}` when the action did not run.
pub trait ResourceChanges: Send + Sync {
    /// Apply `change` for `caller`, as `caller` may change the record now.
    ///
    /// # Errors
    ///
    /// [`ActionCallError::Refused`] for a caller that is not an agent task acting for its
    /// person or a change the workspace does not allow it, [`ActionCallError::ConfirmationRequired`]
    /// for one that asks its person first, [`ActionCallError::Unavailable`] when the origin
    /// cannot be reached.
    fn apply(
        &self,
        caller: &Caller,
        change: &ResourceChange,
    ) -> Result<serde_json::Value, ActionCallError>;
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
                fn read(
                    &self,
                    _: &Caller,
                    _: &ResourceAddress,
                ) -> Result<ResourceCopy, ActionCallError> {
                    Ok(ResourceCopy::reading(
                        self.value.lock().unwrap().clone(),
                        Some(self.revision.lock().unwrap().clone()),
                    ))
                }

                fn watch(
                    &self,
                    _: &Caller,
                    _: &ResourceAddress,
                    wake: &ResourceWake,
                ) -> Result<(), ActionCallError> {
                    self.wakers.lock().unwrap().push(wake.clone());
                    Ok(())
                }

                fn answer_read(
                    self: std::sync::Arc<Self>,
                    caller: &Caller,
                    correlation: u64,
                    read: super::super::ResourceReadRequest,
                ) -> super::super::Pending<super::super::RemoteActionMessage> {
                    answer_read(self, caller, correlation, read)
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
                fn read(
                    &self,
                    _: &Caller,
                    _: &ResourceAddress,
                ) -> Result<ResourceCopy, ActionCallError> {
                    Ok(ResourceCopy::reading(
                        self.0.lock().unwrap().clone(),
                        None,
                    ))
                }

                fn watch(
                    &self,
                    _: &Caller,
                    _: &ResourceAddress,
                    _: &ResourceWake,
                ) -> Result<(), ActionCallError> {
                    Ok(())
                }

                fn answer_read(
                    self: std::sync::Arc<Self>,
                    caller: &Caller,
                    correlation: u64,
                    read: super::super::ResourceReadRequest,
                ) -> super::super::Pending<super::super::RemoteActionMessage> {
                    answer_read(self, caller, correlation, read)
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
