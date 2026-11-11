//! Record reads outside their app: how an agent task's host reads a workspace's records
//! (`docs/designs/app-language/agents.md`, "Reading records outside their app").
//!
//! A workspace's records are sealed per app: a connection holding that app's launch custody opens
//! them. An agent task is not an app. Its host is admitted as the agent acting for the person
//! who started it, never session-delegated and holding no app's route, and reads one record at
//! a time through this door. Each read is decided by the check every reader of a record goes
//! through (the lesser of the agent's grants and the person's), again for every read, so a
//! grant revoked or a member removed in the middle of a task cuts the reads that follow. A read
//! carries the record's committed content and nothing that writes to it: the task changes a
//! record only by invoking actions.

use super::{ActionHostError, Caller};

/// What a host without record reads answers.
pub const RECORD_READS_UNAVAILABLE: &str = "this host reads no records outside their apps";

/// What a caller is told about a record it may not read.
pub const RECORD_READ_REFUSED: &str = "not allowed to read this record";

/// What a caller that is neither a person nor an agent task acting for its person is told.
pub const RECORD_READS_FOR_TASKS: &str = "only a person, an app acting for its person, or an agent \
     acting for the person who admitted it, reads records outside their app; a person's device \
     hands its agents nothing here";

/// One record of a workspace, by its app (the id whose records it is) and its id there (empty for
/// the app's record of the workspace).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RecordAddress {
    pub app: String,
    pub record: String,
}

/// What a read of one record answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordCopy {
    /// The record's committed content: an Automerge document, whole.
    Document(Vec<u8>),
    /// The workspace holds no such record: nothing was ever written to it.
    Absent,
}

/// A host's door for reading records outside their app.
pub trait RecordReads: Send + Sync {
    /// The record `address` of `caller`'s workspace, as `caller` may read it now.
    ///
    /// # Errors
    ///
    /// [`ActionHostError::NotPermitted`] for a caller that is not an agent task acting for its
    /// person, [`ActionHostError::Refused`] for a record the caller may not read,
    /// [`ActionHostError::Unavailable`] when the workspace's records cannot be read now (still
    /// arriving, or held nowhere on this device).
    fn read(&self, caller: &Caller, address: &RecordAddress)
    -> Result<RecordCopy, ActionHostError>;

    /// Wake `wake` once the next write to `address` lands on this host, decided as a read is:
    /// what a reader holding a copy waits on before it reads again. Watched before the read it
    /// guards, so no write between the two is missed.
    ///
    /// # Errors
    ///
    /// As [`Self::read`]; a host that cannot tell when a record changes answers
    /// [`ActionHostError::Unavailable`].
    fn watch(
        &self,
        caller: &Caller,
        address: &RecordAddress,
        wake: &RecordWake,
    ) -> Result<(), ActionHostError> {
        let _ = (caller, address, wake);
        Err(ActionHostError::Unavailable {
            reason: RECORD_READS_UNAVAILABLE.into(),
        })
    }
}

/// Copies of records already in hand, by id, read as they are: what a reader planning with what
/// it holds reads. Any other record is absent, and nothing wakes a wait for a write.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HeldCopies(pub std::collections::HashMap<String, Vec<u8>>);

impl HeldCopies {
    /// `asks` read from these copies by `caller` now, as every reader is answered
    /// ([`RecordReadings::readable`]).
    ///
    /// # Errors
    ///
    /// As [`read_records`] and [`RecordReadings::readable`].
    pub fn readable(
        &self,
        caller: &Caller,
        asks: &[RecordAsk],
    ) -> Result<serde_json::Value, String> {
        read_records(self, caller, asks, None, tilo_clock::Instant::now())
            .map_err(|error| error.to_string())?
            .readable()
    }
}

impl RecordReads for HeldCopies {
    fn read(&self, _: &Caller, address: &RecordAddress) -> Result<RecordCopy, ActionHostError> {
        Ok(self
            .0
            .get(&address.record)
            .map_or(RecordCopy::Absent, |bytes| {
                RecordCopy::Document(bytes.clone())
            }))
    }

    fn watch(&self, _: &Caller, _: &RecordAddress, _: &RecordWake) -> Result<(), ActionHostError> {
        Ok(())
    }
}

/// One record a reader asks for: alone, or `joined`, the chain of parts whose base it is
/// ([`crate::record_sync`]): part `0`, `1`, … up to the first the workspace holds none of. A
/// chain's head ([`crate::record_sync::chain_head`]) asked alone reads its newest part, under
/// that part's id: what a reader that follows a chain reads, whatever its length.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordAsk {
    pub address: RecordAddress,
    pub joined: bool,
}

/// What a read of several records answered: each record read (a joined one as its parts), its
/// copy or `None` while it is still arriving, and the revision of them all, which changes with
/// any write to any of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordReadings {
    pub revision: String,
    pub records: Vec<(String, Option<RecordCopy>)>,
}

/// Read `asks` for `caller` once they differ from the revision `after` or `deadline` passed:
/// each record is watched before it is read, so the next write to any of them wakes the wait.
///
/// # Errors
///
/// As [`RecordReads::read`], but a record still arriving answers `None` rather than failing.
pub fn read_records(
    reads: &dyn RecordReads,
    caller: &Caller,
    asks: &[RecordAsk],
    after: Option<&str>,
    deadline: tilo_clock::Instant,
) -> Result<RecordReadings, ActionHostError> {
    loop {
        let (wake, woken) = RecordWake::new();
        let mut revision = blake3::Hasher::new();
        let mut records = Vec::new();
        for ask in asks {
            let newest_only = match crate::record_sync::record_sync(&ask.address.record) {
                crate::record_sync::RecordSync::ChainHead { base } if !ask.joined => Some(base),
                _ => None,
            };
            let chained = ask.joined || newest_only.is_some();
            let base = newest_only.unwrap_or(&ask.address.record);
            let mut read = Vec::new();
            let mut part = 0;
            loop {
                let record = if chained {
                    crate::record_sync::record_part(base, part)
                } else {
                    ask.address.record.clone()
                };
                let address = RecordAddress {
                    app: ask.address.app.clone(),
                    record,
                };
                let copy = match reads
                    .watch(caller, &address, &wake)
                    .and_then(|()| reads.read(caller, &address))
                {
                    Ok(copy) => Some(copy),
                    // A record still arriving is not held yet: its write wakes the reader.
                    Err(ActionHostError::Unavailable { .. }) => None,
                    Err(error) => return Err(error),
                };
                let held: &[u8] = match &copy {
                    Some(RecordCopy::Document(bytes)) => bytes,
                    Some(RecordCopy::Absent) => b"",
                    None => b"unavailable",
                };
                for bytes in [address.record.as_bytes(), held] {
                    revision.update(&(bytes.len() as u64).to_le_bytes());
                    revision.update(bytes);
                }
                let last = !chained || !matches!(copy, Some(RecordCopy::Document(_)));
                // The part after a chain's last is watched, not answered: its first write wakes.
                if !(chained && part > 0 && matches!(copy, Some(RecordCopy::Absent))) {
                    read.push((address.record, copy));
                }
                if last {
                    break;
                }
                part = part.saturating_add(1);
            }
            // A head answers the newest part alone; every part read stays in the revision, so
            // a write to any of them, or a new part, wakes its reader.
            if newest_only.is_some()
                && let Some(newest) = read
                    .iter()
                    .rposition(|(_, copy)| matches!(copy, Some(RecordCopy::Document(_))))
                    .or_else(|| read.len().checked_sub(1))
            {
                read = vec![read.swap_remove(newest)];
            }
            records.extend(read);
        }
        let revision = revision.finalize().to_hex().to_string();
        let now = tilo_clock::Instant::now();
        if after != Some(revision.as_str()) || now >= deadline {
            return Ok(RecordReadings { revision, records });
        }
        let _ = woken.wait_timeout(deadline.saturating_duration_since(now));
    }
}

/// Answer `read`, asked as `correlation`, for `caller` from `reads`, the host's record door: on
/// a thread of its own, which the wait for a write past `read.after` and the record's capture hold.
pub(super) fn serve_read(
    reads: Option<std::sync::Arc<dyn RecordReads>>,
    caller: &Caller,
    correlation: u64,
    read: super::RecordReadRequest,
) -> super::Pending<super::RemoteActionMessage> {
    serve_readings(reads, caller, read).map(move |result| {
        super::RemoteActionMessage::RecordReading {
            correlation,
            result,
        }
    })
}

fn serve_readings(
    reads: Option<std::sync::Arc<dyn RecordReads>>,
    caller: &Caller,
    read: super::RecordReadRequest,
) -> super::Pending<Result<String, ActionHostError>> {
    let (answer, answered) = super::pending();
    let Some(reads) = reads else {
        answer.complete(Err(ActionHostError::Unavailable {
            reason: RECORD_READS_UNAVAILABLE.into(),
        }));
        return answered;
    };
    let caller = caller.clone();
    let deadline = super::wait::deadline(read.wait_ms.unwrap_or(0));
    let ask = [RecordAsk {
        address: RecordAddress {
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
                read_records(
                    reads.as_ref(),
                    &caller,
                    &ask,
                    read.after.as_deref(),
                    deadline,
                )
                .and_then(|readings| {
                    readings
                        .readable()
                        .map(|answer| answer.to_string())
                        .map_err(|reason| ActionHostError::Refused { reason })
                }),
            );
        });
    answered
}

impl RecordReadings {
    /// The readings as every reader is answered: `{"revision", "records": [...]}`, each record
    /// `{"record", "value"}` ([`RecordCopy::readable`]), `{"record", "absent": true}` for one the
    /// workspace holds none of, or `{"record", "arriving": true}` for one still arriving.
    ///
    /// # Errors
    ///
    /// When a copy is no Automerge document.
    pub fn readable(self) -> Result<serde_json::Value, String> {
        let mut records = Vec::with_capacity(self.records.len());
        for (record, copy) in self.records {
            records.push(match copy {
                None => serde_json::json!({"record": record, "arriving": true}),
                Some(RecordCopy::Absent) => serde_json::json!({"record": record, "absent": true}),
                Some(copy) => serde_json::json!({"record": record, "value": copy.readable()?}),
            });
        }
        Ok(serde_json::json!({"revision": self.revision, "records": records}))
    }
}

impl RecordCopy {
    /// The record as any reader reads it, whatever app keeps it: its maps as objects, lists as
    /// arrays, text as strings, counters and scalars as numbers, strings and booleans, bytes as
    /// their count, a data enum as its `$variant` beside its fields by name; `null` for a record
    /// the workspace holds none of.
    ///
    /// # Errors
    ///
    /// When the copy is no Automerge document.
    pub fn readable(&self) -> Result<serde_json::Value, String> {
        match self {
            Self::Absent => Ok(serde_json::Value::Null),
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

/// Woken once, by the first write to any record it watches ([`RecordReads::watch`]).
#[derive(Clone)]
pub struct RecordWake(std::sync::Arc<std::sync::Mutex<Option<super::Completer<()>>>>);

impl RecordWake {
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
pub struct RecordChange {
    pub app: String,
    pub record: String,
    pub resource: String,
    pub action: String,
    pub arguments: serde_json::Map<String, serde_json::Value>,
    pub preview: Option<serde_json::Value>,
}

/// A host's door for an agent task's changes to records outside their apps. A change goes to
/// the record's origin, which runs the action against the record as it is then, applies what it
/// wrote as the task's step and answers with its JSON: `{"ok": {value, resources}, "differs":
/// bool}`, or `{"failed": why}` when the action did not run.
pub trait RecordChanges: Send + Sync {
    /// Apply `change` for `caller`, as `caller` may change the record now.
    ///
    /// # Errors
    ///
    /// [`ActionHostError::Refused`] for a caller that is not an agent task acting for its
    /// person or a change the workspace does not allow it, [`ActionHostError::ConfirmationRequired`]
    /// for one that asks its person first, [`ActionHostError::Unavailable`] when the origin
    /// cannot be reached.
    fn apply(
        &self,
        caller: &Caller,
        change: &RecordChange,
    ) -> Result<serde_json::Value, ActionHostError>;
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

    fn ask(record: &str, joined: bool) -> RecordAsk {
        RecordAsk {
            address: RecordAddress {
                app: "so.tilo.chat".into(),
                record: record.into(),
            },
            joined,
        }
    }

    fn values(readings: &RecordReadings) -> Vec<(String, serde_json::Value)> {
        readings
            .records
            .iter()
            .map(|(record, copy)| (record.clone(), copy.as_ref().unwrap().readable().unwrap()))
            .collect()
    }

    #[test]
    fn a_record_reads_as_plain_data_with_its_text_counters_and_bytes() {
        let mut held = HeldCopies::default();
        held.0.insert("note".into(), message("hello"));
        let caller = Caller::local_person("me");
        let now = tilo_clock::Instant::now();
        let read = read_records(
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

    #[test]
    fn a_data_enum_reads_with_its_fields_by_name() {
        let mut doc = AutoCommit::new();
        let attached = doc.put_object(ROOT, "attached", ObjType::Map).unwrap();
        doc.put(&attached, "$variant", "file").unwrap();
        doc.put(&attached, "$field/file/blob", "ab").unwrap();
        doc.put(&attached, "$field/file/a%2Fb", 3).unwrap();
        assert_eq!(
            RecordCopy::Document(doc.save()).readable().unwrap(),
            serde_json::json!({"attached": {"$variant": "file", "blob": "ab", "a/b": 3}})
        );
    }

    #[test]
    fn a_chain_reads_joined_up_to_its_last_part_and_a_new_part_changes_its_revision() {
        let mut held = HeldCopies::default();
        let part = |index| crate::record_sync::record_part("chain", index);
        held.0.insert(part(0), message("first"));
        held.0.insert(part(1), message("second"));
        let caller = Caller::local_person("me");
        let now = tilo_clock::Instant::now();
        let read = read_records(&held, &caller, &[ask("chain", true)], None, now).unwrap();
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
        let same = read_records(&held, &caller, &[ask("chain", true)], None, now).unwrap();
        assert_eq!(same.revision, read.revision);
        held.0.insert(part(2), message("third"));
        let grown = read_records(
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
        let newest_part = crate::record_sync::chain_head("chain");
        let newest = read_records(&held, &caller, &[ask(&newest_part, false)], None, now).unwrap();
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
        let moved = read_records(&held, &caller, &[ask(&newest_part, false)], None, now).unwrap();
        assert_ne!(moved.revision, newest.revision);
        assert_eq!(moved.records[0].0, part(3));
    }
}
