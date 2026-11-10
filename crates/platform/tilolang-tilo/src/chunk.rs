//! Chunks: the platform's split of an app's resources over several documents.
//!
//! One document per app and workspace is the unit of sync and storage, so a workspace of a
//! hundred thousand items loads and moves wholesale. A chunk is a smaller document holding some
//! of the app's resources (or a run of one collection's items), and a manifest says which. The
//! manifest is a map at the root of the app's record, [`CHUNKS_KEY`]: which resource is in which
//! chunk ([`Placement::chunk`]), and which chunks carry which run of a collection
//! ([`Placement::runs`]). A chunk is a record of its own (`derived:chunk.<n>@<base>`), sealed and
//! synced like any record of the workspace, so a write reaches only the chunk it changed.
//!
//! **Chunk 0 is today's record.** Its resources are read in place: nothing is copied, no id
//! changes, and a workspace written before chunks opens as one chunk with an empty manifest.
//! Everything a record holds that the manifest does not name is in chunk 0.
//!
//! **The platform picks the chunk, not the app.** A new resource or item is placed by the action
//! that makes it, recorded as [`DynEdit::PlaceChunk`](crate::action::DynEdit::PlaceChunk) and
//! [`DynEdit::RunChunk`](crate::action::DynEdit::RunChunk) in the same commit, so every replica
//! stores it where its maker did and concurrent makers converge like any other change. Ids never
//! change with placement: a resource's id is its own, a chunk's is internal.
//!
//! **Size follows measured cost.** An Automerge document costs, measured by
//! `cargo bench -p tilo-app-state --bench record_cost` over one map per item, one change per
//! item: load 3.6 ms / 31 ms / 325 ms and save 0.34 ms / 2.1 ms / 28 ms at one thousand, ten
//! thousand and a hundred thousand items, held as 276 KiB / 3.1 MiB / 27 MiB; one appended
//! change costs 0.08 ms / 0.18 ms / 4.7 ms and about 124 bytes at any size. Cost is linear in
//! the document and has no knee, so the only thing to choose is where a chunk stops: [`ITEMS`]
//! items (about 6 ms to load, 0.6 MiB held, a tenth of a millisecond per change) is a few
//! thousand, which keeps a workspace of a hundred thousand items at tens of chunks of that size
//! rather than one document of a third of a second.
//!
//! **Runs.** The items of one collection are a run per chunk, in order: a new item goes to the
//! collection's last run while it has room ([`Placement::for_new`]), else to the next chunk. A
//! run is a chunk plus the last order key it holds, so a reader orders the runs without reading
//! them. A split moves items from a full run to the next chunk as an action of its own; their
//! ids stay, and their history is the workspace's action log, which names resources, never
//! documents.
//!
//! **[`Chunks`]** is the set a store holds: the record and the chunks above it it was sent, each
//! a document of its own, and the heads it has applied of each. A read or a write of a resource
//! is routed by the record's manifest to the chunk that holds it.

use std::collections::BTreeMap;
use std::sync::Arc;

use automerge::ChangeHash;
use automerge::transaction::Transactable as _;
use automerge::{ReadDoc, Value as AmValue};

use tilo_actions::ActionError;

use crate::reads::map_at;

type Doc = tilo_resource::Doc;

/// A chunk's number. Chunk [`FIRST`] is the app's record itself.
pub type ChunkNo = u64;

/// The chunk every record written before chunks is: the app's record, read in place.
pub const FIRST: ChunkNo = 0;

/// The map at the root of the app's record that says where its resources are.
pub const CHUNKS_KEY: &str = "$chunks";

/// Items a chunk holds before a new run starts.
///
/// Chosen from the measured curve above: at two thousand items a chunk loads in a few
/// milliseconds and holds well under a megabyte, and one change in it costs a tenth of a
/// millisecond, which is where the cost of a document stops being worth splitting further
/// against the fixed cost of a record (about 200 us to make, 36 us to open, 19 KiB held).
pub const ITEMS: u64 = 2048;

/// What a chunk may hold: [`ITEMS`] items. Bytes are not a second limit to keep: a chunk's size
/// follows its item count on the measured curve, and a change too large for the transport is
/// refused by the workspace and reported (`Instance::unshared`), never silently kept here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub items: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self { items: ITEMS }
    }
}

/// The record id of chunk `chunk` of the record `base`: `derived:chunk.<n>@<base>`, the id
/// [`record_sync`](crate::chunk) syncs on demand and the channel its changes travel on. Chunk
/// [`FIRST`] is the record itself, which has its own id (`base`).
#[must_use]
pub fn record_id(base: &str, chunk: ChunkNo) -> String {
    format!("derived:chunk.{chunk}@{base}")
}

/// One run of a collection in one chunk: its last order key, and how many items it holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    /// The greatest order key stored in the run (`$at`'s key), so the runs of a collection
    /// order without being read.
    pub last: String,
    /// How many items of the collection the run holds, as its writers counted them.
    pub len: u64,
}

/// Where an app's resources are, as its record's manifest says: read once per change of the
/// record, and consulted for every placement.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Placement {
    /// The chunk a resource is in, when it is not chunk [`FIRST`].
    of: std::collections::BTreeMap<String, ChunkNo>,
    /// Per holder and collection field, its runs by chunk.
    runs: std::collections::BTreeMap<(String, String), std::collections::BTreeMap<ChunkNo, Run>>,
    /// The number of the next chunk to start.
    next: ChunkNo,
}

impl Placement {
    /// The placement `doc` (an app's record, chunk [`FIRST`]) holds. A record written before
    /// chunks holds nothing: every resource is in chunk [`FIRST`].
    #[must_use]
    pub fn read(doc: &Doc) -> Self {
        let mut placement = Self::default();
        // Every copy of the manifest, and of each map in it, a replica made before it met the
        // others: the record keeps one of each, and what the others place is placed just the
        // same ([`crate::containers`]). The last copy read is the one the record keeps, which
        // is what a write goes to.
        for manifest in crate::containers::maps(doc, &automerge::ROOT, CHUNKS_KEY) {
            if let Some((AmValue::Scalar(scalar), _)) = doc.get(&manifest, "next").ok().flatten()
                && let automerge::ScalarValue::Uint(next) = scalar.as_ref()
            {
                placement.next = placement.next.max(*next);
            }
            for of in crate::containers::maps(doc, &manifest, "of") {
                for id in doc.keys(&of) {
                    if let Some((AmValue::Scalar(scalar), _)) = doc.get(&of, &id).ok().flatten()
                        && let automerge::ScalarValue::Uint(chunk) = scalar.as_ref()
                    {
                        placement.of.insert(id, *chunk);
                    }
                }
            }
            for runs in crate::containers::maps(doc, &manifest, "runs") {
                for holder in doc.keys(&runs) {
                    for fields in crate::containers::maps(doc, &runs, &holder) {
                        for field in doc.keys(&fields) {
                            for chunks in crate::containers::maps(doc, &fields, &field) {
                                for name in doc.keys(&chunks) {
                                    let Ok(chunk) = name.parse::<ChunkNo>() else {
                                        continue;
                                    };
                                    let Some(run) = read_run(doc, &chunks, &name) else {
                                        continue;
                                    };
                                    placement
                                        .runs
                                        .entry((holder.clone(), field.clone()))
                                        .or_default()
                                        .insert(chunk, run);
                                }
                            }
                        }
                    }
                }
            }
        }
        placement
    }

    /// The chunk the resource `id` is stored in.
    #[must_use]
    pub fn chunk(&self, id: &str) -> ChunkNo {
        self.of.get(id).copied().unwrap_or(FIRST)
    }

    /// Where the manifest places each resource that is not in chunk [`FIRST`], by its id.
    #[must_use]
    pub fn placed(&self) -> &std::collections::BTreeMap<String, ChunkNo> {
        &self.of
    }

    /// The runs the manifest names, per holder and collection field.
    #[must_use]
    pub fn all_runs(
        &self,
    ) -> &std::collections::BTreeMap<(String, String), std::collections::BTreeMap<ChunkNo, Run>>
    {
        &self.runs
    }

    /// Whether the manifest names any chunk at all: a record written before chunks names none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.of.is_empty() && self.runs.is_empty()
    }

    /// Every chunk the manifest names, above the record: the resources it places, and the runs
    /// it keeps. What a replica holds of a record is these, chunk [`FIRST`] being the record
    /// itself.
    #[must_use]
    pub fn chunks(&self) -> std::collections::BTreeSet<ChunkNo> {
        self.of
            .values()
            .copied()
            .chain(self.runs.values().flat_map(|chunks| chunks.keys().copied()))
            .filter(|chunk| *chunk != FIRST)
            .collect()
    }

    /// The number of the next chunk a run starts in.
    #[must_use]
    pub fn next(&self) -> ChunkNo {
        self.next.max(FIRST.saturating_add(1))
    }

    /// The runs of the collection `field` of `holder`, in order: their chunks and the last order
    /// key each holds.
    #[must_use]
    pub fn runs(&self, holder: &str, field: &str) -> Vec<(ChunkNo, &Run)> {
        let mut runs: Vec<(ChunkNo, &Run)> = self
            .runs
            .get(&(holder.to_owned(), field.to_owned()))
            .map(|chunks| chunks.iter().map(|(chunk, run)| (*chunk, run)).collect())
            .unwrap_or_default();
        runs.sort_by(|(a_chunk, a), (b_chunk, b)| {
            (a.last.as_str(), *a_chunk).cmp(&(b.last.as_str(), *b_chunk))
        });
        runs
    }

    /// The chunk the newest item of the collection `field` of `holder` goes to, and whether that
    /// starts a new run: the collection's last run while it holds fewer than `limits.items`, else
    /// the next chunk. A collection with no run yet is in `holder_chunk`, where its resource is.
    #[must_use]
    pub fn for_new(
        &self,
        holder: &str,
        field: &str,
        holder_chunk: ChunkNo,
        limits: Limits,
    ) -> (ChunkNo, bool) {
        let Some((chunk, run)) = self.runs(holder, field).pop() else {
            return (holder_chunk, false);
        };
        if run.len < limits.items {
            (chunk, false)
        } else {
            (self.next(), true)
        }
    }

    /// The chunk whose run of `field` of `holder` holds the order key `last`, or the chunk that
    /// would continue the collection.
    #[must_use]
    pub fn run_of(&self, holder: &str, field: &str, last: &str) -> Option<ChunkNo> {
        self.runs(holder, field)
            .iter()
            .find(|(_, run)| run.last == last)
            .map(|(chunk, _)| *chunk)
    }
}

/// The heads a reader has applied of each chunk it holds, by chunk: what an index stamped with
/// them reflects.
pub type Stamps = BTreeMap<ChunkNo, Vec<ChangeHash>>;

/// The documents a store holds: chunk [`FIRST`], the record itself, and the chunks above it the
/// store was sent, each a copy of what its holder has with the store's own writes as its open
/// transaction. Every resource is read and written in the chunk the record's manifest places it
/// in ([`Placement`]); a chunk the store was not sent is not held, and nothing is read of it
/// until it arrives.
#[derive(Clone, Default)]
pub struct Chunks {
    first: Option<Arc<Doc>>,
    more: BTreeMap<ChunkNo, Arc<Doc>>,
    /// Where the record's manifest places its resources, as of its last change.
    placement: Placement,
    /// The heads the store has applied of each chunk it holds.
    seen: Stamps,
}

impl Chunks {
    /// Whether the store holds the record (chunk [`FIRST`]).
    #[must_use]
    pub fn has_record(&self) -> bool {
        self.first.is_some()
    }

    /// Chunk [`FIRST`], the record, when the store holds it.
    #[must_use]
    pub fn first(&self) -> Option<&Doc> {
        self.first.as_deref()
    }

    /// Chunk [`FIRST`], to change: a copy of it when a reader still holds it.
    pub fn first_mut(&mut self) -> Option<&mut Doc> {
        self.first.as_mut().map(Arc::make_mut)
    }

    /// The chunk `chunk`, when the store holds it.
    #[must_use]
    pub fn mirror(&self, chunk: ChunkNo) -> Option<&Doc> {
        if chunk == FIRST {
            self.first()
        } else {
            self.more.get(&chunk).map(AsRef::as_ref)
        }
    }

    /// The chunk `chunk`, to change: a copy of it when a reader still holds it.
    pub fn mirror_mut(&mut self, chunk: ChunkNo) -> Option<&mut Doc> {
        if chunk == FIRST {
            self.first_mut()
        } else {
            self.more.get_mut(&chunk).map(Arc::make_mut)
        }
    }

    /// The chunk `chunk`, to write into: made when the store was not sent it yet, as a write
    /// into a run that starts there is made before that chunk arrives.
    pub fn open(&mut self, chunk: ChunkNo) -> &mut Doc {
        if chunk == FIRST {
            Arc::make_mut(self.first.get_or_insert_with(|| Arc::new(Doc::new())))
        } else {
            Arc::make_mut(
                self.more
                    .entry(chunk)
                    .or_insert_with(|| Arc::new(Doc::new())),
            )
        }
    }

    /// The set holding `doc` as the record, chunk [`FIRST`], and nothing above it.
    #[must_use]
    pub fn of(doc: Doc) -> Self {
        let mut chunks = Self::default();
        chunks.insert(FIRST, doc);
        // What the record's manifest places is where this set routes its reads and writes.
        chunks.reread_placement();
        chunks
    }

    /// Hold `doc` as chunk `chunk`, in place of what was held of it.
    pub fn insert(&mut self, chunk: ChunkNo, doc: Doc) {
        if chunk == FIRST {
            self.first = Some(Arc::new(doc));
        } else {
            self.more.insert(chunk, Arc::new(doc));
        }
    }

    /// The number of the chunk the resource `id` is stored in, as the record's manifest places
    /// it: [`FIRST`] for a resource the manifest does not name.
    #[must_use]
    pub fn chunk(&self, id: &str) -> ChunkNo {
        self.placement.chunk(id)
    }

    /// The document the resource `id` is stored in, when the store holds its chunk.
    #[must_use]
    pub fn doc(&self, id: &str) -> Option<&Doc> {
        self.mirror(self.chunk(id))
    }

    /// The document the resource `id` is stored in, to change, when the store holds its chunk.
    pub fn doc_mut(&mut self, id: &str) -> Option<&mut Doc> {
        let chunk = self.chunk(id);
        self.mirror_mut(chunk)
    }

    /// The numbers of the chunks the store holds, the record first and the rest by number.
    pub fn numbers(&self) -> impl Iterator<Item = ChunkNo> + '_ {
        self.docs().map(|(chunk, _)| chunk)
    }

    /// The chunks the store holds, the record first and the rest by number.
    pub fn docs(&self) -> impl Iterator<Item = (ChunkNo, &Doc)> {
        let first = self.first.as_deref().map(|doc| (FIRST, doc));
        first
            .into_iter()
            .chain(self.more.iter().map(|(chunk, doc)| (*chunk, doc.as_ref())))
    }

    /// Where the record's manifest places its resources, as of its last change.
    #[must_use]
    pub fn placement(&self) -> &Placement {
        &self.placement
    }

    /// Read the record's manifest again: after the record's changes reached it.
    pub fn reread_placement(&mut self) {
        self.placement = self
            .first
            .as_deref()
            .map_or_else(Placement::default, Placement::read);
    }

    /// The heads the store has applied of `chunk`.
    #[must_use]
    pub fn seen(&self, chunk: ChunkNo) -> &[ChangeHash] {
        self.seen.get(&chunk).map_or(&[], Vec::as_slice)
    }

    /// Note that the store applied `heads` of `chunk`.
    pub fn set_seen(&mut self, chunk: ChunkNo, heads: Vec<ChangeHash>) {
        self.seen.insert(chunk, heads);
    }

    /// The heads the store has applied of each chunk it holds: what an index stamped with them
    /// reflects.
    #[must_use]
    pub fn heads(&self) -> Stamps {
        self.docs()
            .map(|(chunk, doc)| (chunk, doc.committed_document().get_heads()))
            .collect()
    }

    /// Whether the store holds every change of `heads`, in one chunk or another.
    #[must_use]
    pub fn holds_heads(&self, heads: &[ChangeHash]) -> bool {
        heads.iter().all(|head| {
            self.docs()
                .any(|(_, doc)| doc.committed_document().get_change_by_hash(head).is_some())
        })
    }

    /// Whether the store holds every change the stamps name, in the chunk each is stamped for:
    /// whether an index stamped with them reflects what the store holds.
    #[must_use]
    pub fn holds(&self, stamps: &Stamps) -> bool {
        stamps.iter().all(|(chunk, heads)| {
            self.mirror(*chunk).is_some_and(|doc| {
                heads
                    .iter()
                    .all(|head| doc.committed_document().get_change_by_hash(head).is_some())
            })
        })
    }
}

/// The run stored at `chunks/<number>` of the manifest.
fn read_run(doc: &Doc, chunks: &automerge::ObjId, number: &str) -> Option<Run> {
    let run = map_at(doc, chunks, number)?;
    let text = |key: &str| match doc.get(&run, key).ok().flatten() {
        Some((AmValue::Scalar(scalar), _)) => match scalar.as_ref() {
            automerge::ScalarValue::Str(text) => Some(text.to_string()),
            _ => None,
        },
        _ => None,
    };
    let len = match doc.get(&run, "len").ok().flatten() {
        Some((AmValue::Scalar(scalar), _)) => match scalar.as_ref() {
            automerge::ScalarValue::Uint(len) => Some(*len),
            _ => None,
        },
        _ => None,
    };
    Some(Run {
        last: text("last")?,
        len: len.unwrap_or(0),
    })
}

/// The map at the root of the record that holds the manifest, made when absent; the copies of
/// it a replica made are folded into the one the record keeps.
fn manifest(doc: &mut Doc) -> Result<automerge::ObjId, ActionError> {
    crate::containers::single(doc, &automerge::ROOT, CHUNKS_KEY)
}

/// Write in the manifest that chunk `chunk` exists: the number the next new chunk takes is past
/// it.
fn note_next(
    doc: &mut Doc,
    manifest: &automerge::ObjId,
    chunk: ChunkNo,
) -> Result<(), ActionError> {
    let next = doc
        .get(manifest, "next")
        .ok()
        .flatten()
        .and_then(|(value, _)| match value {
            AmValue::Scalar(scalar) => match scalar.as_ref() {
                automerge::ScalarValue::Uint(next) => Some(*next),
                _ => None,
            },
            AmValue::Object(_) => None,
        })
        .unwrap_or(FIRST);
    if chunk >= next {
        doc.put(manifest, "next", chunk.saturating_add(1))?;
    }
    Ok(())
}

/// Write that the resource `id` is stored in `chunk`, in the manifest of `doc` (the app's
/// record). The same write on every replica: what the action that placed it recorded.
pub fn place(doc: &mut Doc, id: &str, chunk: ChunkNo) -> Result<(), ActionError> {
    let manifest = manifest(doc)?;
    note_next(doc, &manifest, chunk)?;
    let of = crate::containers::single(doc, &manifest, "of")?;
    doc.put(&of, id, chunk)?;
    Ok(())
}

/// Write that the collection `field` of `holder` has a run in `chunk` whose last order key is
/// `last` and which holds `len` items: what the action that appended to it counted.
pub fn run(
    doc: &mut Doc,
    holder: &str,
    field: &str,
    chunk: ChunkNo,
    last: &str,
    len: u64,
) -> Result<(), ActionError> {
    let manifest = manifest(doc)?;
    note_next(doc, &manifest, chunk)?;
    let runs = crate::containers::single(doc, &manifest, "runs")?;
    let fields = crate::containers::single(doc, &runs, holder)?;
    let chunks = crate::containers::single(doc, &fields, field)?;
    let held = crate::containers::single(doc, &chunks, &chunk.to_string())?;
    doc.put(&held, "last", last)?;
    doc.put(&held, "len", len)?;
    Ok(())
}

/// The chunk a resource goes to when it is made: the chunk of the resource it is made in (its
/// parent), else [`FIRST`]. App code never names a chunk; the platform answers with the parent's.
#[must_use]
pub fn for_resource(placement: &Placement, parent: Option<&str>) -> ChunkNo {
    parent.map_or(FIRST, |parent| placement.chunk(parent))
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::{AutoCommit, ObjType};

    fn record() -> Doc {
        AutoCommit::new()
    }

    #[test]
    fn a_record_written_before_chunks_opens_as_one_chunk() {
        // A workspace written before chunks: its app's resources in the record's own map, and no
        // manifest. It reads as one chunk with everything in it, in place.
        let mut doc = record();
        {
            use automerge::transaction::Transactable as _;
            let records = doc
                .put_object(&automerge::ROOT, crate::action::RECORDS_KEY, ObjType::Map)
                .expect("records");
            let app = doc
                .put_object(&records, "so.tilo.tasks", ObjType::Map)
                .expect("app");
            let issue = doc
                .put_object(&app, "issue-1", ObjType::Map)
                .expect("issue");
            doc.put(&issue, "title", "Ship it").expect("title");
        }
        let placement = Placement::read(&doc);
        assert!(placement.is_empty());
        assert_eq!(placement.chunk("issue-1"), FIRST);
        assert_eq!(placement.next(), 1);
        assert!(placement.runs("tracker", "issues").is_empty());
        assert_eq!(
            placement.for_new("tracker", "issues", FIRST, Limits::default()),
            (FIRST, false)
        );
    }

    #[test]
    fn a_placement_says_which_chunk_holds_a_resource() {
        let mut doc = record();
        place(&mut doc, "issue-7", 2).expect("write");
        let placement = Placement::read(&doc);
        assert_eq!(placement.chunk("issue-7"), 2);
        assert_eq!(placement.chunk("issue-8"), FIRST);
        assert_eq!(placement.next(), 3);
    }

    #[test]
    fn runs_order_by_their_last_item_and_fill_before_a_new_chunk_starts() {
        let limits = Limits { items: 2 };
        let mut doc = record();
        // The first run stays in the record itself, where the collection is; the second is a chunk.
        run(&mut doc, "tracker", "issues", FIRST, "a2", 2).expect("write");
        run(&mut doc, "tracker", "issues", 3, "a5", 1).expect("write");
        run(&mut doc, "tracker", "issues", 2, "a4", 1).expect("write");
        let placement = Placement::read(&doc);
        let runs: Vec<(ChunkNo, &str)> = placement
            .runs("tracker", "issues")
            .into_iter()
            .map(|(chunk, run)| (chunk, run.last.as_str()))
            .collect();
        assert_eq!(runs, vec![(FIRST, "a2"), (2, "a4"), (3, "a5")]);
        assert_eq!(placement.run_of("tracker", "issues", "a4"), Some(2));
        // The last run still has room.
        assert_eq!(
            placement.for_new("tracker", "issues", FIRST, limits),
            (3, false)
        );
        run(&mut doc, "tracker", "issues", 3, "a6", 2).expect("write");
        assert_eq!(
            Placement::read(&doc).for_new("tracker", "issues", FIRST, limits),
            (4, true)
        );
    }

    #[test]
    fn a_resource_made_in_a_chunk_is_placed_beside_its_parent() {
        let mut doc = record();
        place(&mut doc, "channel-9", 4).expect("write");
        let placement = Placement::read(&doc);
        assert_eq!(for_resource(&placement, Some("channel-9")), 4);
        assert_eq!(for_resource(&placement, Some("tracker")), FIRST);
        assert_eq!(for_resource(&placement, None), FIRST);
    }
}
