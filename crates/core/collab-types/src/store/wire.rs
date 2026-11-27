//! What an app asks of the store, and what it answers: the commands a process sends the device's
//! workspace service over its port, and their replies. The store carries them out
//! (`tilo_resource_store::wire`).

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use super::{Filter, Indexed, Page, PastChild, Query, Resource, Review, Value};
use crate::history::Version;
use crate::stamp::Stamp;

/// The most a command or its answer takes on the wire.
pub const MAX_WIRE_BYTES: usize = 4 << 20;

/// One thing an app asks of the store.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Command {
    /// Write the resource `id` of `kind`, as the action `stamp` names.
    Put {
        kind: String,
        id: String,
        resource: Resource,
        stamp: Stamp,
    },
    /// Remove the resource `id` of `kind`, as `stamp` names.
    Remove {
        kind: String,
        id: String,
        stamp: Stamp,
    },
    /// Take back the invocation `invocation`, as `stamp` names.
    Undo { invocation: String, stamp: Stamp },
    /// The resource `id` of `kind`.
    Get { kind: String, id: String },
    /// One page of `query`.
    Query(Query),
    /// How many resources of `kind` hold to `filters`.
    Count { kind: String, filters: Vec<Filter> },
    /// Edit the content of the resource `id` of `kind` (its texts, counters, lists and maps), in
    /// one change, as `stamp` names: two writers' edits of it merge.
    Edit {
        kind: String,
        id: String,
        edits: Vec<Edit>,
        stamp: Stamp,
    },
    /// The content of the resource `id` of `kind`, as JSON: texts as strings, counters as
    /// numbers.
    Content { kind: String, id: String },
    /// Several writes, as one: each is made in turn, and the first that fails stops them.
    Batch(Vec<Command>),
    /// The versions of the resource `id` of `kind`, oldest first.
    Versions { kind: String, id: String },
    /// The resource `id` of `kind` as it stood just before the change `change` (a version's
    /// change, its hash in hex), or just after it with `after`.
    At {
        kind: String,
        id: String,
        change: String,
        after: bool,
    },
    /// Every version stamped as a step of the group `group`, of every resource it touched.
    Grouped { group: String },
    /// The ids of the resources of `kind` removed, nested in `parent` when it names one.
    Removed {
        kind: String,
        parent: Option<String>,
    },
    /// The resources made by a change stamped as made from each resource `ids` names, wherever
    /// they were placed, answered by id in the order asked: what a reader taking a page of
    /// resources reads in one command rather than one of each. A single resource is a list of
    /// one.
    MadeFrom { ids: Vec<String> },
    /// The newest version of each resource `within` names (it, and those whose ids are under
    /// it), of at most `limit` of them, most recent first.
    Newest { within: String, limit: usize },
    /// Where each resource `ids` names stands in the listing of `query`, in their order, read
    /// from the index's order rather than by paging to it: where a reader showing the listing
    /// puts the rows that came, and finds the ones that left, in one read however many moved.
    Positions { query: Query, ids: Vec<String> },
    /// The page of `query` that starts at `at` in its listing, its cursor ignored: the rows a
    /// reader shows far down a listing, read from the index's counts rather than by paging to
    /// them.
    PageAt { query: Query, at: usize },
    /// The first version of the resource `id` of `kind`, or its last with `last`: what a view
    /// showing when it was made or last changed reads, without the rest of its history.
    VersionEnd {
        kind: String,
        id: String,
        last: bool,
    },
    /// The content of each resource `ids` names, of `kind`, in their order: what [`Self::Content`]
    /// answers of each, in one read, for a reader taking the contents of a page of resources.
    Contents { kind: String, ids: Vec<String> },
    /// One page of `query`, each resource holding only the indexed fields `fields` names: what
    /// a reader taking a few fields of every resource of a kind (search, its titles) asks for,
    /// rather than each resource whole.
    Fields { query: Query, fields: Vec<String> },
    /// Take the turn to run the action `address` (`<app>/<resource>#<action>`) on `inputs`,
    /// which runs once in the workspace whichever device runs it; or, with an `answer`, keep that
    /// this device ran it and what it answered, and let the turn go. Answered as [`Reply::Turn`].
    /// No store holds turns: the device's workspace service places them, never a store.
    Turn {
        address: String,
        inputs: Json,
        answer: Option<Json>,
    },
    /// This device's changes of the record `record` that no other device keeping the workspace
    /// has taken in yet, and those the workspace refused. Answered as [`Reply::Reach`]. The
    /// device's sync keeps them and its workspace service answers; a store answers none, as a
    /// store on one device alone has no other device to reach.
    Reach { record: String },
    /// The changes of a record of the asking app, as Automerge encodes them, past the heads
    /// `after` (hex; none asks for every change): what the process merges into its own copy of
    /// the record. `record` names it; `None` is the record the process's route opened. Answered
    /// as [`Reply::RecordChanges`] by the device's workspace service, which keeps the record and
    /// decides the read for the caller; the store's `read` answers none.
    ///
    /// The answer comes in pages: `from` is where in it this page starts, `0` first, and each
    /// page names where the next one starts until the last.
    ///
    /// It goes on the wire as `RecordChanges/2`, which a service older than `record` refuses as
    /// a command it does not know rather than answering another record; the earlier name, which
    /// names no record, is still read.
    #[serde(rename = "RecordChanges/2", alias = "RecordChanges")]
    RecordChanges {
        #[serde(default)]
        record: Option<String>,
        after: Vec<String>,
        from: u64,
    },
    /// The asking app's own changes of a record, as Automerge encodes them: `record` names it,
    /// `None` being the record the process's route opened. The device's workspace service reads
    /// them with the confined parser, decides the write for the caller, and records them as its
    /// own change of the record; the record's first changes found it. Answered as
    /// [`Reply::Written`]; a store makes none (the store's `write`). On the wire as
    /// `RecordChange/2`, as [`Self::RecordChanges`] is.
    ///
    /// `changes` is a run of the changes, concatenated as Automerge saves them incrementally; a
    /// write too large for one command goes as several runs in order, each but the last with
    /// `more`, and the service takes them as one write once the last arrives. `commit` names the
    /// action this write is one part of, when it wrote more than one record: every part carries
    /// it, and every device applies the parts together or not at all.
    #[serde(rename = "RecordChange/2", alias = "RecordChange")]
    RecordChange {
        #[serde(default)]
        record: Option<String>,
        changes: Bytes,
        more: bool,
        commit: Option<crate::WorkspaceCommit>,
    },
    /// The ids of the asking app's records the workspace holds, answered as [`Reply::Ids`] by the
    /// device's workspace service; the store's `read` answers none.
    Records,
    /// The values the resources of `kind` holding to `filters` hold in `field`, each once, in
    /// order (each member of a set): what a view listing the labels in use reads, answered from
    /// the counts the index keeps rather than from every resource. Answered as
    /// [`Reply::Values`].
    Values {
        kind: String,
        filters: Vec<Filter>,
        field: String,
    },
    /// The resources nested in `parent` as they stood at the version the change `change` (its
    /// hash in hex) made, each with its fields and content then: what a reader comparing a
    /// parent's past states reads its collections from. Answered as [`Reply::ChildrenAt`], in
    /// pages: `from` is where in the answer this page starts, `0` first.
    ///
    /// A service older than this read refuses it as a command it does not know, which the
    /// reader says plainly; nothing it already answers changes.
    ChildrenAt {
        parent: String,
        change: String,
        from: u64,
    },
    /// Name the version the resource `id` of `kind` stands at now `name`, as `stamp`: a change
    /// with no operations, which its history lists.
    Checkpoint {
        kind: String,
        id: String,
        name: String,
        stamp: Stamp,
    },
    /// Bring `resources` (by kind and id), and what is nested in them, back to the version
    /// `change`, as `stamp`; with `only`, just the resources it names, and with `live`, only
    /// while their documents stand where a review saw them.
    Restore {
        resources: Vec<(String, String)>,
        change: String,
        only: Option<Vec<String>>,
        live: Option<Vec<String>>,
        stamp: Stamp,
    },
    /// What restoring `resources` to the version `change` would change, answered as
    /// [`Reply::Review`] of the resources the person may read.
    ReviewRestore {
        resources: Vec<(String, String)>,
        change: String,
    },
    /// The invocations of `invoker`'s last `steps` steps still in effect, newest first, read from
    /// the store's own change graph rather than from a list kept by whoever wrote them: what an
    /// app's `undo` of its last steps takes back, also where the store was opened for the call
    /// that asked and holds nothing it wrote. A step is one invocation; a change an undo took
    /// back is no step until a redo brings it back. `invoker` names the caller as a stamp writes
    /// it ([`Stamp::by`]'s text); `kinds` names the kinds whose records are read, so an app takes
    /// back its own steps and no other app's (empty for every kind). With `redo`, the undos of
    /// theirs still in effect instead, which [`Self::UndoMany`] brings back. Answered as
    /// [`Reply::Ids`].
    Steps {
        steps: u32,
        invoker: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        kinds: Vec<String>,
        #[serde(default)]
        redo: bool,
    },
    /// Take back the invocations `invocations`, as `stamp` names: each change it writes names the
    /// invocation it takes back in its stamp, so a redo is this of the undo's own invocation.
    /// Answered as [`Reply::Written`].
    UndoMany {
        invocations: Vec<String>,
        stamp: Stamp,
    },
    /// The kind of the resource `id`, held now or removed, as the store's index files it:
    /// what a reader holding an id alone (a published address, a reference in a text) reads it
    /// by, in one read however many kinds it knows. Answered as [`Reply::Kind`].
    KindOf { id: String },
}

/// One edit of a resource's content.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Edit {
    /// `field` holds `value` whole: a scalar, or maps and lists of them.
    Set { field: String, value: Json },
    /// In the text `field`, made when it holds none, `delete` characters at `at` give way to
    /// `insert`.
    Splice {
        field: String,
        at: usize,
        delete: usize,
        insert: String,
    },
    /// The counter `field`, made at zero when it holds none, grows by `by`.
    Add { field: String, by: i64 },
    /// In the map `field`, made when it holds none, `key` holds `value`.
    MapPut {
        field: String,
        key: String,
        value: Json,
    },
    /// In the map `field`, `key` holds nothing.
    MapDelete { field: String, key: String },
    /// `field` holds nothing.
    Remove { field: String },
}

/// What the store answered.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Reply {
    /// The write was made.
    Written,
    /// The resource, `None` where there is none the person may read.
    Resource(Option<Indexed>),
    Page(Page),
    Count(usize),
    /// The content, `None` where there is none the person may read or none was written.
    Content(Option<Json>),
    Versions(Vec<Version>),
    /// The resource and its content as they stood at a change, `None` where the store holds no
    /// such change of a resource the person may read.
    At(Option<(Option<Indexed>, Option<Json>)>),
    /// Versions, each with the kind and id of the resource it is a version of.
    Grouped(Vec<(String, String, Version)>),
    /// Ids of resources.
    Ids(Vec<String>),
    /// Resources, by kind and id. Retired: [`Command::MadeFrom`] answers [`Self::MadeFrom`],
    /// which says which asked resource each of them was made from.
    RetiredResources(Vec<(String, String)>),
    /// Versions, each with the kind and id of the resource it is the newest version of, and
    /// whether there were more than were asked for.
    Newest {
        versions: Vec<(String, String, Version)>,
        more: bool,
    },
    /// Where each resource asked for stands in a listing, in the order asked, `None` for one
    /// the listing holds none of that the person may read.
    Positions(Vec<Option<usize>>),
    /// The content of each resource asked for, in the order asked: as [`Self::Content`] answers
    /// one.
    Contents(Vec<Option<Json>>),
    /// Whether this device runs an action that runs once ([`Command::Turn`]).
    Turn(Turn),
    /// How far this device's changes of a record have gone ([`Command::Reach`]).
    Reach(Reach),
    /// The changes of a record past what the asker holds, and the heads they bring it to
    /// ([`Command::RecordChanges`]).
    ///
    /// `changes` is this page's part of the changes, concatenated as Automerge saves them
    /// incrementally; the asker joins the pages and loads them once `next` is `None`. `next` is
    /// where the next page starts. `heads` is where the whole answer brings the record, the same
    /// on every page of one answer: a page naming other heads belongs to a newer answer, and the
    /// asker starts again from its first page.
    ///
    /// `pending` says the record is on its way to this device (the workspace is still opening,
    /// or the keys or checkpoint that open the record have not come): it holds nothing to answer
    /// yet, and is asked again.
    RecordChanges {
        changes: Bytes,
        heads: Vec<String>,
        next: Option<u64>,
        #[serde(default)]
        pending: bool,
    },
    /// The values a field holds, each once, in order ([`Command::Values`]).
    Values(Vec<Value>),
    /// The resources nested in a parent at a version, a page of them, and where the next page
    /// starts ([`Command::ChildrenAt`]); `None` where the store holds no such change.
    ChildrenAt(Option<PastChildren>),
    /// What a restore would change ([`Command::ReviewRestore`]), or why there is nothing to review.
    Review(Result<Review, String>),
    /// The resources made from each resource [`Command::MadeFrom`] asked of, by kind and id, in
    /// the order asked: one list per asked resource, each as [`Self::RetiredResources`] answered
    /// one.
    MadeFrom(Vec<Vec<(String, String)>>),
    /// The kind of a resource ([`Command::KindOf`]), `None` where the store holds none the
    /// person may read, now or in its history.
    Kind(Option<String>),
}

impl Reply {
    /// The page of a record's `changes` from byte `from` on, of at most [`RECORD_PAGE_BYTES`],
    /// with the `heads` the whole answer brings the record to: what answers
    /// [`Command::RecordChanges`]. `None` when it starts past them.
    #[must_use]
    pub fn record_page(changes: &[u8], heads: Vec<String>, from: u64) -> Option<Self> {
        let start = usize::try_from(from).ok()?;
        let end = changes.len().min(start.saturating_add(RECORD_PAGE_BYTES));
        Some(Self::RecordChanges {
            changes: Bytes(changes.get(start..end)?.to_vec()),
            heads,
            next: (end < changes.len()).then(|| u64::try_from(end).unwrap_or(u64::MAX)),
            pending: false,
        })
    }
}

/// One answer to [`Command::RecordChanges`], joined page by page as its reader asks for them.
#[derive(Debug, Default)]
pub struct RecordPages {
    changes: Vec<u8>,
    heads: Option<Vec<String>>,
}

/// What a reader joining [`RecordPages`] asks next.
#[derive(Debug)]
pub enum RecordPage {
    /// The page that starts here: the next one, or the first of a newer answer.
    From(u64),
    /// Nothing yet: the record is on its way to the device.
    Pending,
    /// The whole answer: the changes, and the heads they bring the record to.
    Done {
        changes: Vec<u8>,
        heads: Vec<String>,
    },
}

impl RecordPages {
    /// Join `reply`, the page asked for last. A page naming other heads than the first belongs
    /// to a newer answer, which is read again from its start.
    ///
    /// # Errors
    /// `reply` is no page of a record's changes; it is handed back.
    pub fn join(&mut self, reply: Reply) -> Result<RecordPage, Box<Reply>> {
        let Reply::RecordChanges {
            changes,
            heads,
            next,
            pending,
        } = reply
        else {
            return Err(Box::new(reply));
        };
        if pending {
            return Ok(RecordPage::Pending);
        }
        match &self.heads {
            Some(first) if *first != heads => {
                *self = Self::default();
                return Ok(RecordPage::From(0));
            }
            Some(_) => {}
            None => self.heads = Some(heads),
        }
        self.changes.extend(changes.0);
        Ok(match next {
            Some(next) => RecordPage::From(next),
            None => RecordPage::Done {
                changes: std::mem::take(&mut self.changes),
                heads: self.heads.take().unwrap_or_default(),
            },
        })
    }
}

/// A page of the resources nested in a parent at a version ([`Reply::ChildrenAt`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PastChildren {
    pub children: Vec<PastChild>,
    /// Where the next page starts; `None` on the last.
    pub next: Option<u64>,
}

impl PastChildren {
    /// The page of `children` from `from` on, of at most [`RECORD_PAGE_BYTES`] encoded.
    #[must_use]
    pub fn page(children: Vec<PastChild>, from: u64) -> Self {
        let start = usize::try_from(from).unwrap_or(usize::MAX);
        let mut bytes = 0usize;
        let mut page = Vec::new();
        let mut next = None;
        for (at, child) in children.into_iter().enumerate().skip(start) {
            let size = serde_json::to_vec(&child).map_or(0, |encoded| encoded.len());
            if !page.is_empty() && bytes.saturating_add(size) > RECORD_PAGE_BYTES {
                next = u64::try_from(at).ok();
                break;
            }
            bytes = bytes.saturating_add(size);
            page.push(child);
        }
        Self {
            children: page,
            next,
        }
    }
}

/// How many bytes of changes one page or run carries: under [`MAX_WIRE_BYTES`] once encoded, and
/// under the megabyte one frame of an app's connection to its device carries, as base64.
pub const RECORD_PAGE_BYTES: usize = 512 << 10;

/// Bytes carried in a command or reply, encoded as base64 text rather than a list of numbers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bytes(pub Vec<u8>);

/// Standard base64, written padded and read with or without its padding.
const BASE64: base64::engine::GeneralPurpose = base64::engine::GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    base64::engine::GeneralPurposeConfig::new()
        .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
);

impl Serialize for Bytes {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&base64::Engine::encode(&BASE64, &self.0))
    }
}

impl<'de> Deserialize<'de> for Bytes {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        base64::Engine::decode(&BASE64, text)
            .map(Self)
            .map_err(|error| serde::de::Error::custom(format!("bytes are not base64: {error}")))
    }
}

/// This device's changes of one record that no other device keeping the workspace has taken in
/// yet, and those the workspace refused for good, each by its hash in hex. Any other change of
/// the record reached another device, or came from one.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reach {
    pub pending: Vec<String>,
    pub refused: Vec<String>,
}

/// Whether this device runs an action that runs once in the workspace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Turn {
    /// It is this device's turn: run it, then say it is done.
    Take,
    /// Another device runs it, or none may yet: ask again later.
    Later,
    /// A device ran it already, and answered this.
    Done(Json),
}

impl Command {
    /// Whether it writes: what the store's `write` carries out, and the store's `read` does not.
    #[must_use]
    pub const fn writes(&self) -> bool {
        matches!(
            self,
            Self::Put { .. }
                | Self::Remove { .. }
                | Self::Undo { .. }
                | Self::Edit { .. }
                | Self::Batch(_)
                | Self::RecordChange { .. }
                | Self::Checkpoint { .. }
                | Self::Restore { .. }
                | Self::UndoMany { .. }
        )
    }

    /// The resources whose readings this write moves: each resource it writes, what it puts one
    /// under, and what it says the resources it made were made from. A reader forgets what it
    /// read of each, of what is nested under it and of what holds it ([`super::MovedRecord`]).
    /// `None` for a write that may move any resource (an undo, a restore, a record's own
    /// changes); none for a read.
    #[must_use]
    pub fn moves(&self) -> Option<Vec<super::MovedItem>> {
        let item = |id: &str, kind: Option<&str>| super::MovedItem {
            id: id.to_owned(),
            kind: kind.map(str::to_owned),
        };
        let mut moved = Vec::new();
        let mut commands = vec![self];
        while let Some(command) = commands.pop() {
            let (kind, id, stamp) = match command {
                Self::Put {
                    kind,
                    id,
                    resource,
                    stamp,
                } => {
                    moved.extend(resource.parent.iter().map(|parent| item(parent, None)));
                    (kind, id, stamp)
                }
                Self::Remove { kind, id, stamp }
                | Self::Edit {
                    kind, id, stamp, ..
                }
                | Self::Checkpoint {
                    kind, id, stamp, ..
                } => (kind, id, stamp),
                Self::Batch(inner) => {
                    commands.extend(inner);
                    continue;
                }
                Self::Undo { .. }
                | Self::UndoMany { .. }
                | Self::Restore { .. }
                | Self::RecordChange { .. } => return None,
                _ => continue,
            };
            moved.push(item(id, Some(kind)));
            moved.extend(
                stamp
                    .from
                    .as_ref()
                    .map(|from| item(&from.id, Some(&from.kind))),
            );
        }
        moved.sort_unstable();
        moved.dedup();
        Some(moved)
    }
}

#[cfg(test)]
mod bytes_tests {
    use super::Bytes;

    /// Every length's bytes come back from their text whole, and the text is base64's.
    #[test]
    fn bytes_travel_as_base64_text() {
        assert_eq!(
            serde_json::to_string(&Bytes(b"Man".to_vec())).unwrap(),
            "\"TWFu\""
        );
        assert_eq!(
            serde_json::to_string(&Bytes(b"Ma".to_vec())).unwrap(),
            "\"TWE=\""
        );
        for len in 0..300 {
            let bytes = Bytes(
                (0..len)
                    .map(|at: usize| u8::try_from(at * 7 % 256).unwrap())
                    .collect(),
            );
            let text = serde_json::to_string(&bytes).unwrap();
            assert_eq!(serde_json::from_str::<Bytes>(&text).unwrap(), bytes);
        }
        assert!(serde_json::from_str::<Bytes>("\"T!==\"").is_err());
    }
}

#[cfg(test)]
mod record_wire_tests {
    use super::{Bytes, Command, Reply};

    /// The record commands as a service from before they named their record reads them.
    #[derive(Debug, serde::Deserialize)]
    #[allow(dead_code, reason = "only what the earlier service decodes is read")]
    enum Earlier {
        RecordChanges { after: Vec<String>, from: u64 },
        RecordChange { changes: Bytes, more: bool },
    }

    /// A command naming its record reaches an earlier service as one it refuses, never as a read
    /// or write of the record the route opened; what an earlier app sends still reads, as the
    /// route's record; and an earlier app reads an answer that says the record is on its way.
    #[test]
    fn a_record_command_crosses_versions_or_is_refused() {
        /// A reply as a reader from before `pending` reads it.
        #[derive(Debug, serde::Deserialize)]
        enum EarlierReply {
            RecordChanges {
                heads: Vec<String>,
                next: Option<u64>,
            },
        }
        for command in [
            Command::RecordChanges {
                record: Some("note-1".into()),
                after: Vec::new(),
                from: 0,
            },
            Command::RecordChange {
                record: Some("note-1".into()),
                changes: Bytes(vec![1, 2]),
                more: false,
                commit: None,
            },
        ] {
            let text = serde_json::to_string(&command).unwrap();
            let refused = serde_json::from_str::<Earlier>(&text).unwrap_err();
            assert!(
                refused.to_string().contains("unknown variant"),
                "{text}: {refused}"
            );
        }

        let earlier = r#"{"RecordChanges":{"after":["ab"],"from":3}}"#;
        assert!(matches!(
            serde_json::from_str::<Command>(earlier).unwrap(),
            Command::RecordChanges {
                record: None,
                from: 3,
                ..
            }
        ));
        let earlier = r#"{"RecordChange":{"changes":"AQI=","more":false,"commit":null}}"#;
        assert!(matches!(
            serde_json::from_str::<Command>(earlier).unwrap(),
            Command::RecordChange { record: None, .. }
        ));

        let earlier = r#"{"RecordChanges":{"changes":"","heads":[],"next":null}}"#;
        assert!(matches!(
            serde_json::from_str::<Reply>(earlier).unwrap(),
            Reply::RecordChanges { pending: false, .. }
        ));
        let pending = serde_json::to_string(&Reply::RecordChanges {
            changes: Bytes::default(),
            heads: Vec::new(),
            next: None,
            pending: true,
        })
        .unwrap();
        assert!(matches!(
            serde_json::from_str::<EarlierReply>(&pending).unwrap(),
            EarlierReply::RecordChanges { heads, next: None } if heads.is_empty()
        ));
    }
}
