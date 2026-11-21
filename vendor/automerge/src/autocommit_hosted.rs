//! TILO(model-device): [`AutoCommit`] in a model module (`--cfg tilo_model_guest`).
//!
//! The document is the host's: this is a handle on it, and every read and
//! write is one call to the host ([`crate::model_device`]), so the module
//! carries none of Automerge's storage. The API is the native one, so a verb
//! written against `AutoCommit` runs unchanged in a module. What only a
//! device does with a document — sync it, diff it, take its changes apart —
//! is not a verb's to do, and calling it traps the module.

use std::ops::{Bound, RangeBounds};

use crate::author::Author;
use crate::automerge::SaveOptions;
use crate::cursor::{CursorPosition, MoveCursor};
use crate::exid::ExId;
use crate::iter::{DocIter, Keys, ListRange, ListRangeItem, MapRange, MapRangeItem, Span, Spans, Values};
use crate::marks::{ExpandMark, Mark, MarkSet, UpdateSpansConfig};
use crate::model_device::wire::{Reader, Wire, Writer};
use crate::model_device::{op, outside_device, Hosted, ANSWERED};
use crate::op_set2::{ChangeMetadata, Parent, Parents};
use crate::patches::PatchLog;
use crate::sync::{self, SyncDoc};
use crate::transaction::{CommitOptions, Transactable};
use crate::{hydrate, AnonymizeError, Bundle, Fragment, OnPartialLoad, TextEncoding};
use crate::{
    ActorId, Automerge, AutomergeError, Change, ChangeHash, Cursor, ObjType, Patch, Prop, ReadDoc,
    ScalarValue, Value, ROOT,
};
use crate::{LoadOptions, VerificationMode};

#[link(wasm_import_module = "tilo_model")]
extern "C" {
    /// Make the call of `length` bytes at `offset` on a document the host
    /// keeps, answering how long the host's answer is.
    fn document(offset: i32, length: i32) -> i32;
    /// Copy the host's answer to the last call to `offset`.
    fn document_answer(offset: i32);
}

/// One call to the host, and its answer as the host wrote it.
fn exchange(call: &Writer) -> Vec<u8> {
    let length = unsafe { document(call.0.as_ptr() as i32, call.0.len() as i32) };
    let mut answer = vec![0_u8; usize::try_from(length).unwrap_or(0)];
    unsafe { document_answer(answer.as_mut_ptr() as i32) };
    answer
}

/// One call to the host: what it answered, or why it refused.
fn ask(call: Writer) -> Result<Vec<u8>, AutomergeError> {
    let mut answer = exchange(&call);
    match answer.first() {
        Some(&ANSWERED) => {
            answer.remove(0);
            Ok(answer)
        }
        Some(_) => {
            let reason = Reader::new(&answer[1..])
                .take::<String>()
                .unwrap_or_else(|_| "the host refused".to_owned());
            Err(AutomergeError::Device(reason))
        }
        None => Err(AutomergeError::Device("the host answered nothing".to_owned())),
    }
}

fn read<T: Wire>(answer: &[u8]) -> T {
    Reader::new(answer)
        .take()
        .unwrap_or_else(|_| panic!("the host's answer is malformed"))
}

/// A document the host keeps, open in this module.
#[derive(Debug)]
pub struct AutoCommit {
    handle: u32,
    actor: ActorId,
    encoding: TextEncoding,
}

impl Clone for AutoCommit {
    fn clone(&self) -> Self {
        Self::opened(self.must(op::CLONE, |_| {}))
    }
}

impl Default for AutoCommit {
    fn default() -> Self {
        Self::new_with_encoding(TextEncoding::platform_default())
    }
}

impl Drop for AutoCommit {
    fn drop(&mut self) {
        // The host lets go of the document, or never had it: either way
        // there is nothing to answer.
        let mut w = Writer::new();
        w.byte(op::RELEASE);
        w.put(&self.handle);
        exchange(&w);
    }
}

impl AutoCommit {
    /// The document the host's request is about: what a verb applies to and
    /// a genesis initializes.
    pub fn of_device() -> AutoCommit {
        Self::opened(
            ask({
                let mut w = Writer::new();
                w.byte(op::DESCRIBE);
                w.put(&0_u32);
                w
            })
            .unwrap_or_else(|error| panic!("{error}")),
        )
    }

    fn opened(answer: Vec<u8>) -> Self {
        let (handle, actor, encoding) = read(&answer);
        Self {
            handle,
            actor,
            encoding,
        }
    }

    fn call(&self, call: u8, args: impl FnOnce(&mut Writer)) -> Result<Vec<u8>, AutomergeError> {
        let mut w = Writer::new();
        w.byte(call);
        w.put(&self.handle);
        args(&mut w);
        ask(w)
    }

    /// A call the host answers whenever the document is open.
    fn must(&self, call: u8, args: impl FnOnce(&mut Writer)) -> Vec<u8> {
        self.call(call, args)
            .unwrap_or_else(|error| panic!("{error}"))
    }

    fn get_with<T: Wire>(&self, call: u8, args: impl FnOnce(&mut Writer)) -> Result<T, AutomergeError> {
        Ok(read(&self.call(call, args)?))
    }

    fn must_get<T: Wire>(&self, call: u8, args: impl FnOnce(&mut Writer)) -> T {
        read(&self.must(call, args))
    }

    pub fn new() -> AutoCommit {
        AutoCommit::default()
    }

    #[inline(always)]
    pub fn diff_opset(&self, _other: &AutoCommit) -> Result<(), AutomergeError> {
        outside_device("comparing op sets")
    }

    pub fn new_with_encoding(encoding: TextEncoding) -> AutoCommit {
        let mut w = Writer::new();
        w.byte(op::NEW);
        w.put(&encoding);
        Self::opened(ask(w).unwrap_or_else(|error| panic!("{error}")))
    }

    #[inline(always)]
    pub fn anonymize(&mut self) -> Result<Self, AnonymizeError> {
        outside_device("anonymizing a document")
    }

    pub fn load(data: &[u8]) -> Result<Self, AutomergeError> {
        let mut w = Writer::new();
        w.byte(op::LOAD);
        w.put(data);
        Ok(Self::opened(ask(w)?))
    }

    pub fn load_unverified_heads(data: &[u8]) -> Result<Self, AutomergeError> {
        Self::load(data)
    }

    pub fn load_with(
        data: &[u8],
        _on_error: OnPartialLoad,
        _mode: VerificationMode,
    ) -> Result<Self, AutomergeError> {
        Self::load(data)
    }

    #[inline(always)]
    pub fn load_with_options(
        _data: &[u8],
        _options: LoadOptions<'_>,
    ) -> Result<Self, AutomergeError> {
        outside_device("loading with options")
    }

    #[inline(always)]
    pub fn from_document(_doc: Automerge) -> Self {
        outside_device("a document of the module's own")
    }

    pub fn reset_diff_cursor(&mut self) {
        self.must(op::RESET_DIFF_CURSOR, |_| {});
    }

    pub fn update_diff_cursor(&mut self) {
        self.must(op::UPDATE_DIFF_CURSOR, |_| {});
    }

    pub fn diff_cursor(&self) -> Vec<ChangeHash> {
        self.must_get(op::DIFF_CURSOR, |_| {})
    }

    #[inline(always)]
    pub fn make_patches(&self, _patch_log: &mut PatchLog) -> Vec<Patch> {
        outside_device("patches")
    }

    #[inline(always)]
    pub fn diff(&mut self, _before: &[ChangeHash], _after: &[ChangeHash]) -> Vec<Patch> {
        outside_device("diffing")
    }

    #[inline(always)]
    pub fn diff_obj(
        &mut self,
        _obj: &ExId,
        _before: &[ChangeHash],
        _after: &[ChangeHash],
        _recursive: bool,
    ) -> Result<Vec<Patch>, AutomergeError> {
        outside_device("diffing")
    }

    #[inline(always)]
    pub fn diff_incremental(&mut self) -> Vec<Patch> {
        outside_device("diffing")
    }

    pub fn fork(&mut self) -> Self {
        Self::opened(self.must(op::FORK, |_| {}))
    }

    pub fn fork_at(&mut self, heads: &[ChangeHash]) -> Result<Self, AutomergeError> {
        Ok(Self::opened(self.call(op::FORK_AT, |w| {
            w.put(heads);
        })?))
    }

    #[inline(always)]
    pub fn committed_document(&self) -> &Automerge {
        outside_device("the committed document")
    }

    pub fn committed_change_meta(&self, hash: &ChangeHash) -> Option<ChangeMetadata<'_>> {
        let meta: Option<ChangeMetadata<'static>> = self.must_get(op::CHANGE_META, |w| {
            w.put(hash);
        });
        meta
    }

    pub fn committed_hash_of_actor_seq(&self, actor: &ActorId, seq: u64) -> Option<ChangeHash> {
        self.must_get(op::HASH_OF_ACTOR_SEQ, |w| {
            w.put(actor).put(&seq);
        })
    }

    pub fn committed_applied_seq(&self, actor: &ActorId) -> u64 {
        self.must_get(op::APPLIED_SEQ, |w| {
            w.put(actor);
        })
    }

    pub fn committed_heads(&self) -> Vec<ChangeHash> {
        self.must_get(op::COMMITTED_HEADS, |_| {})
    }

    pub fn save_committed(&self) -> Vec<u8> {
        self.must_get(op::SAVE_COMMITTED, |_| {})
    }

    pub fn committed_fork_at(&self, heads: &[ChangeHash]) -> Result<Self, AutomergeError> {
        Ok(Self::opened(self.call(op::COMMITTED_FORK_AT, |w| {
            w.put(heads);
        })?))
    }

    #[inline(always)]
    pub fn committed_diff(&self, _before: &[ChangeHash], _after: &[ChangeHash]) -> Vec<Patch> {
        outside_device("diffing")
    }

    #[inline(always)]
    pub fn document(&mut self) -> &Automerge {
        outside_device("the committed document")
    }

    pub fn with_actor(mut self, actor: ActorId) -> Self {
        self.set_actor(actor);
        self
    }

    pub fn set_actor(&mut self, actor: ActorId) -> &mut Self {
        self.must(op::SET_ACTOR, |w| {
            w.put(&actor);
        });
        self.actor = actor;
        self
    }

    #[inline(always)]
    pub fn with_author(self, _author: Option<Author<'static>>) -> Self {
        outside_device("authors")
    }

    #[inline(always)]
    pub fn set_author(&mut self, _author: Option<Author<'static>>) -> &mut Self {
        outside_device("authors")
    }

    pub fn get_actor(&self) -> &ActorId {
        &self.actor
    }

    #[inline(always)]
    pub fn get_actors_for_author(&self, _author: &Author<'_>) -> Vec<ActorId> {
        outside_device("authors")
    }

    #[inline(always)]
    pub fn get_author_for_actor(&self, _actor: &ActorId) -> Option<Author<'_>> {
        outside_device("authors")
    }

    #[inline(always)]
    pub fn get_author(&self) -> Option<&Author<'static>> {
        outside_device("authors")
    }

    #[inline(always)]
    pub fn get_authors(&self) -> &[Author<'static>] {
        outside_device("authors")
    }

    pub fn isolate(&mut self, heads: &[ChangeHash]) {
        self.must(op::ISOLATE, |w| {
            w.put(heads);
        });
    }

    pub fn read_at<R>(&mut self, heads: &[ChangeHash], read_view: impl FnOnce(&Self) -> R) -> R {
        let view: Option<Vec<ChangeHash>> = self.must_get(op::REPLACE_VIEW, |w| {
            w.put(&Some(heads.to_vec()));
        });
        let answer = read_view(self);
        self.must(op::REPLACE_VIEW, |w| {
            w.put(&view);
        });
        answer
    }

    pub fn integrate(&mut self) {
        self.must(op::INTEGRATE, |_| {});
    }

    pub fn load_incremental(&mut self, data: &[u8]) -> Result<usize, AutomergeError> {
        self.get_with(op::LOAD_INCREMENTAL, |w| {
            w.put(data);
        })
    }

    #[inline(always)]
    pub fn apply_changes(
        &mut self,
        _changes: impl IntoIterator<Item = Change> + Clone,
    ) -> Result<(), AutomergeError> {
        outside_device("applying changes")
    }

    #[inline(always)]
    pub fn apply_changes_batch(
        &mut self,
        _changes: impl IntoIterator<Item = Change> + Clone,
    ) -> Result<(), AutomergeError> {
        outside_device("applying changes")
    }

    pub fn merge(&mut self, other: &mut AutoCommit) -> Result<Vec<ChangeHash>, AutomergeError> {
        self.get_with(op::MERGE, |w| {
            w.put(&other.handle);
        })
    }

    pub fn save(&mut self) -> Vec<u8> {
        self.must_get(op::SAVE, |_| {})
    }

    #[inline(always)]
    pub fn save_with_options(&mut self, _options: SaveOptions) -> Vec<u8> {
        outside_device("saving with options")
    }

    pub fn save_and_verify(&mut self) -> Result<Vec<u8>, AutomergeError> {
        Ok(self.save())
    }

    #[inline(always)]
    pub fn bundle<I>(&self, _hashes: I) -> Result<Bundle, AutomergeError>
    where
        I: IntoIterator<Item = ChangeHash>,
    {
        outside_device("bundles")
    }

    #[inline(always)]
    pub fn debug_cmp(&self, _other: &Self) {
        outside_device("comparing documents")
    }

    pub fn save_nocompress(&mut self) -> Vec<u8> {
        self.must_get(op::SAVE_NOCOMPRESS, |_| {})
    }

    pub fn save_incremental(&mut self) -> Vec<u8> {
        self.must_get(op::SAVE_INCREMENTAL, |_| {})
    }

    pub fn is_empty(&self) -> bool {
        self.must_get(op::IS_EMPTY, |_| {})
    }

    pub fn save_after(&mut self, heads: &[ChangeHash]) -> Vec<u8> {
        self.must_get(op::SAVE_AFTER, |w| {
            w.put(heads);
        })
    }

    #[inline(always)]
    pub fn discard_queued(&mut self, _hashes: impl IntoIterator<Item = ChangeHash>) {
        outside_device("the change queue")
    }

    pub fn get_missing_deps(&mut self, heads: &[ChangeHash]) -> Vec<ChangeHash> {
        self.must_get(op::MISSING_DEPS, |w| {
            w.put(heads);
        })
    }

    #[inline(always)]
    pub fn get_last_local_change(&mut self) -> Option<Change> {
        outside_device("changes")
    }

    #[inline(always)]
    pub fn get_changes(&mut self, _have_deps: &[ChangeHash]) -> Vec<Change> {
        outside_device("changes")
    }

    #[inline(always)]
    pub fn get_changes_meta(&mut self, _have_deps: &[ChangeHash]) -> Vec<ChangeMetadata<'_>> {
        outside_device("changes")
    }

    #[inline(always)]
    pub fn get_change_by_hash(&mut self, _hash: &ChangeHash) -> Option<Change> {
        outside_device("changes")
    }

    #[inline(always)]
    pub fn has_change(&self, _hash: &ChangeHash) -> bool {
        outside_device("changes")
    }

    pub fn get_change_meta_by_hash(&mut self, hash: &ChangeHash) -> Option<ChangeMetadata<'_>> {
        self.committed_change_meta(hash)
    }

    #[inline(always)]
    pub fn get_changes_added(&mut self, _other: &mut Self) -> Vec<Change> {
        outside_device("changes")
    }

    pub fn import(&self, s: &str) -> Result<(ExId, ObjType), AutomergeError> {
        self.get_with(op::IMPORT, |w| {
            w.put(s);
        })
    }

    pub fn import_obj(&self, s: &str) -> Result<ExId, AutomergeError> {
        let (obj, _) = self.import(s)?;
        Ok(obj)
    }

    #[inline(always)]
    pub fn dump(&mut self) {
        outside_device("dumping a document")
    }

    #[inline(always)]
    pub fn fragments<R: RangeBounds<usize>>(&self, _levels: R) -> Vec<Fragment> {
        outside_device("fragments")
    }

    #[inline(always)]
    pub fn get_fragment(&self, _head: ChangeHash) -> Option<Fragment> {
        outside_device("fragments")
    }

    #[inline(always)]
    pub fn bundle_fragments<I: IntoIterator<Item = Fragment>>(&self, _fragments: I) -> Vec<Vec<u8>> {
        outside_device("fragments")
    }

    pub fn get_heads(&mut self) -> Vec<ChangeHash> {
        self.must_get(op::GET_HEADS, |_| {})
    }

    pub fn commit(&mut self) -> Option<ChangeHash> {
        self.commit_with(CommitOptions::default())
    }

    pub fn commit_with(&mut self, options: CommitOptions) -> Option<ChangeHash> {
        self.must_get(op::COMMIT_WITH, |w| {
            w.put(&options.message).put(&options.time);
        })
    }

    pub fn rollback(&mut self) -> usize {
        self.must_get(op::ROLLBACK, |_| {})
    }

    /// The objects [`crate::AutoCommit::batch_create_objects`] makes, each
    /// made by the host as [`Transactable::batch_create_object`] makes it:
    /// the same document, without the native op set's side-by-side insert.
    pub fn batch_create_objects<O: AsRef<ExId>>(
        &mut self,
        obj: O,
        values: &[(&str, &hydrate::Value)],
    ) -> Result<Vec<ExId>, AutomergeError> {
        let obj = obj.as_ref();
        values
            .iter()
            .map(|(key, value)| self.batch_create_object(obj, *key, value, false))
            .collect()
    }

    pub fn empty_change(&mut self, options: CommitOptions) -> ChangeHash {
        self.must_get(op::EMPTY_CHANGE, |w| {
            w.put(&options.message).put(&options.time);
        })
    }

    pub fn sync(&mut self) -> impl SyncDoc + '_ {
        NoSync(self)
    }

    pub fn hash_for_opid(&self, opid: &ExId) -> Option<ChangeHash> {
        self.must_get(op::HASH_FOR_OPID, |w| {
            w.put(opid);
        })
    }

    #[inline(always)]
    pub fn has_our_changes(&mut self, _state: &sync::State) -> bool {
        outside_device("sync")
    }

    fn read_obj<T: Wire>(
        &self,
        call: u8,
        obj: &ExId,
        args: impl FnOnce(&mut Writer),
    ) -> Result<T, AutomergeError> {
        self.get_with(call, |w| {
            w.put(obj);
            args(w);
        })
    }

    fn must_read_obj<T: Wire>(&self, call: u8, obj: &ExId, args: impl FnOnce(&mut Writer)) -> T {
        self.read_obj(call, obj, args)
            .unwrap_or_else(|error| panic!("{error}"))
    }

    fn write_obj(
        &mut self,
        call: u8,
        obj: &ExId,
        args: impl FnOnce(&mut Writer),
    ) -> Result<(), AutomergeError> {
        self.call(call, |w| {
            w.put(obj);
            args(w);
        })
        .map(|_| ())
    }

    fn keys_of(&self, obj: &ExId, heads: Option<&[ChangeHash]>) -> Keys<'_> {
        let keys: Vec<String> = self.must_read_obj(op::KEYS, obj, |w| {
            w.put(&heads.map(<[ChangeHash]>::to_vec));
        });
        Keys {
            hosted: Hosted::new(keys),
        }
    }

    fn map_range_of<R: RangeBounds<String>>(
        &self,
        obj: &ExId,
        range: R,
        heads: Option<&[ChangeHash]>,
    ) -> MapRange<'_> {
        let bounds: (Bound<String>, Bound<String>) =
            (range.start_bound().cloned(), range.end_bound().cloned());
        let items: Vec<(String, Value<'static>, bool, ExId)> =
            self.must_read_obj(op::MAP_RANGE, obj, |w| {
                w.put(&bounds).put(&heads.map(<[ChangeHash]>::to_vec));
            });
        MapRange {
            hosted: Hosted::new(
                items
                    .into_iter()
                    .map(|(key, value, conflict, id)| MapRangeItem::hosted(key, value, conflict, id))
                    .collect(),
            ),
        }
    }

    fn list_range_of<R: RangeBounds<usize>>(
        &self,
        obj: &ExId,
        range: R,
        heads: Option<&[ChangeHash]>,
    ) -> ListRange<'_> {
        let bounds: (Bound<usize>, Bound<usize>) =
            (range.start_bound().cloned(), range.end_bound().cloned());
        let items: Vec<(usize, Value<'static>, bool, ExId)> =
            self.must_read_obj(op::LIST_RANGE, obj, |w| {
                w.put(&bounds).put(&heads.map(<[ChangeHash]>::to_vec));
            });
        ListRange {
            hosted: Hosted::new(
                items
                    .into_iter()
                    .map(|(index, value, conflict, id)| {
                        ListRangeItem::hosted(index, value, conflict, id)
                    })
                    .collect(),
            ),
        }
    }

    fn values_of(&self, obj: &ExId, heads: Option<&[ChangeHash]>) -> Values<'_> {
        let values: Vec<(Value<'static>, ExId)> = self.must_read_obj(op::VALUES, obj, |w| {
            w.put(&heads.map(<[ChangeHash]>::to_vec));
        });
        Values {
            hosted: Hosted::new(values),
        }
    }

    fn parents_of(
        &self,
        obj: &ExId,
        heads: Option<&[ChangeHash]>,
    ) -> Result<Parents<'_>, AutomergeError> {
        let parents: Vec<(ExId, Prop, bool, ObjType)> = self.read_obj(op::PARENTS, obj, |w| {
            w.put(&heads.map(<[ChangeHash]>::to_vec));
        })?;
        Ok(Parents {
            hosted: Hosted::new(
                parents
                    .into_iter()
                    .map(|(obj, prop, visible, typ)| Parent {
                        obj,
                        typ,
                        prop,
                        visible,
                    })
                    .collect(),
            ),
        })
    }

    fn spans_of(
        &self,
        obj: &ExId,
        heads: Option<&[ChangeHash]>,
    ) -> Result<Spans<'_>, AutomergeError> {
        let spans: Vec<Span> = self.read_obj(op::SPANS, obj, |w| {
            w.put(&heads.map(<[ChangeHash]>::to_vec));
        })?;
        Ok(Spans {
            hosted: Hosted::new(spans),
        })
    }

    fn cursor_of(
        &self,
        obj: &ExId,
        position: CursorPosition,
        at: Option<&[ChangeHash]>,
        moving: Option<MoveCursor>,
    ) -> Result<Cursor, AutomergeError> {
        self.read_obj(op::GET_CURSOR, obj, |w| {
            w.put(&position)
                .put(&at.map(<[ChangeHash]>::to_vec))
                .put(&moving);
        })
    }
}

/// A module's documents are not synced by the module: the host syncs its own.
struct NoSync<'a>(&'a mut AutoCommit);

impl SyncDoc for NoSync<'_> {
    fn generate_sync_message(&self, _sync_state: &mut sync::State) -> Option<sync::Message> {
        let _ = &self.0;
        outside_device("sync")
    }

    #[inline(always)]
    fn receive_sync_message(
        &mut self,
        _sync_state: &mut sync::State,
        _message: sync::Message,
    ) -> Result<(), AutomergeError> {
        outside_device("sync")
    }

    #[inline(always)]
    fn receive_sync_message_log_patches(
        &mut self,
        _sync_state: &mut sync::State,
        _message: sync::Message,
        _patch_log: &mut PatchLog,
    ) -> Result<(), AutomergeError> {
        outside_device("sync")
    }
}

fn heads_of(heads: &[ChangeHash]) -> Option<Vec<ChangeHash>> {
    Some(heads.to_vec())
}

impl ReadDoc for AutoCommit {
    fn parents<O: AsRef<ExId>>(&self, obj: O) -> Result<Parents<'_>, AutomergeError> {
        self.parents_of(obj.as_ref(), None)
    }

    fn parents_at<O: AsRef<ExId>>(
        &self,
        obj: O,
        heads: &[ChangeHash],
    ) -> Result<Parents<'_>, AutomergeError> {
        self.parents_of(obj.as_ref(), Some(heads))
    }

    fn keys<O: AsRef<ExId>>(&self, obj: O) -> Keys<'_> {
        self.keys_of(obj.as_ref(), None)
    }

    fn keys_at<O: AsRef<ExId>>(&self, obj: O, heads: &[ChangeHash]) -> Keys<'_> {
        self.keys_of(obj.as_ref(), Some(heads))
    }

    #[inline(always)]
    fn iter_at<O: AsRef<ExId>>(&self, _obj: O, _heads: Option<&[ChangeHash]>) -> DocIter<'_> {
        outside_device("iterating a whole document")
    }

    fn map_range<'a, O: AsRef<ExId>, R: RangeBounds<String> + 'a>(
        &'a self,
        obj: O,
        range: R,
    ) -> MapRange<'a> {
        self.map_range_of(obj.as_ref(), range, None)
    }

    fn map_range_at<'a, O: AsRef<ExId>, R: RangeBounds<String> + 'a>(
        &'a self,
        obj: O,
        range: R,
        heads: &[ChangeHash],
    ) -> MapRange<'a> {
        self.map_range_of(obj.as_ref(), range, Some(heads))
    }

    fn list_range<O: AsRef<ExId>, R: RangeBounds<usize>>(&self, obj: O, range: R) -> ListRange<'_> {
        self.list_range_of(obj.as_ref(), range, None)
    }

    fn list_range_at<O: AsRef<ExId>, R: RangeBounds<usize>>(
        &self,
        obj: O,
        range: R,
        heads: &[ChangeHash],
    ) -> ListRange<'_> {
        self.list_range_of(obj.as_ref(), range, Some(heads))
    }

    fn values<O: AsRef<ExId>>(&self, obj: O) -> Values<'_> {
        self.values_of(obj.as_ref(), None)
    }

    fn values_at<O: AsRef<ExId>>(&self, obj: O, heads: &[ChangeHash]) -> Values<'_> {
        self.values_of(obj.as_ref(), Some(heads))
    }

    fn length<O: AsRef<ExId>>(&self, obj: O) -> usize {
        self.must_read_obj(op::LENGTH, obj.as_ref(), |w| {
            w.put(&None::<Vec<ChangeHash>>);
        })
    }

    fn length_at<O: AsRef<ExId>>(&self, obj: O, heads: &[ChangeHash]) -> usize {
        self.must_read_obj(op::LENGTH, obj.as_ref(), |w| {
            w.put(&heads_of(heads));
        })
    }

    fn object_type<O: AsRef<ExId>>(&self, obj: O) -> Result<ObjType, AutomergeError> {
        self.read_obj(op::OBJECT_TYPE, obj.as_ref(), |_| {})
    }

    fn marks<O: AsRef<ExId>>(&self, obj: O) -> Result<Vec<Mark>, AutomergeError> {
        self.read_obj(op::MARKS, obj.as_ref(), |w| {
            w.put(&None::<Vec<ChangeHash>>);
        })
    }

    fn marks_at<O: AsRef<ExId>>(
        &self,
        obj: O,
        heads: &[ChangeHash],
    ) -> Result<Vec<Mark>, AutomergeError> {
        self.read_obj(op::MARKS, obj.as_ref(), |w| {
            w.put(&heads_of(heads));
        })
    }

    fn get_marks<O: AsRef<ExId>>(
        &self,
        obj: O,
        index: usize,
        heads: Option<&[ChangeHash]>,
    ) -> Result<MarkSet, AutomergeError> {
        self.read_obj(op::GET_MARKS, obj.as_ref(), |w| {
            w.put(&index).put(&heads.map(<[ChangeHash]>::to_vec));
        })
    }

    fn text<O: AsRef<ExId>>(&self, obj: O) -> Result<String, AutomergeError> {
        self.read_obj(op::TEXT, obj.as_ref(), |w| {
            w.put(&None::<Vec<ChangeHash>>);
        })
    }

    fn text_at<O: AsRef<ExId>>(
        &self,
        obj: O,
        heads: &[ChangeHash],
    ) -> Result<String, AutomergeError> {
        self.read_obj(op::TEXT, obj.as_ref(), |w| {
            w.put(&heads_of(heads));
        })
    }

    fn spans<O: AsRef<ExId>>(&self, obj: O) -> Result<Spans<'_>, AutomergeError> {
        self.spans_of(obj.as_ref(), None)
    }

    fn spans_at<O: AsRef<ExId>>(
        &self,
        obj: O,
        heads: &[ChangeHash],
    ) -> Result<Spans<'_>, AutomergeError> {
        self.spans_of(obj.as_ref(), Some(heads))
    }

    fn get_cursor<O: AsRef<ExId>, I: Into<CursorPosition>>(
        &self,
        obj: O,
        position: I,
        at: Option<&[ChangeHash]>,
    ) -> Result<Cursor, AutomergeError> {
        self.cursor_of(obj.as_ref(), position.into(), at, None)
    }

    fn get_cursor_moving<O: AsRef<ExId>, I: Into<CursorPosition>>(
        &self,
        obj: O,
        position: I,
        at: Option<&[ChangeHash]>,
        move_cursor: MoveCursor,
    ) -> Result<Cursor, AutomergeError> {
        self.cursor_of(obj.as_ref(), position.into(), at, Some(move_cursor))
    }

    fn get_cursor_position<O: AsRef<ExId>>(
        &self,
        obj: O,
        cursor: &Cursor,
        at: Option<&[ChangeHash]>,
    ) -> Result<usize, AutomergeError> {
        self.read_obj(op::GET_CURSOR_POSITION, obj.as_ref(), |w| {
            w.put(cursor).put(&at.map(<[ChangeHash]>::to_vec));
        })
    }

    fn hydrate<O: AsRef<ExId>>(
        &self,
        obj: O,
        heads: Option<&[ChangeHash]>,
    ) -> Result<hydrate::Value, AutomergeError> {
        self.read_obj(op::HYDRATE, obj.as_ref(), |w| {
            w.put(&heads.map(<[ChangeHash]>::to_vec));
        })
    }

    fn get<O: AsRef<ExId>, P: Into<Prop>>(
        &self,
        obj: O,
        prop: P,
    ) -> Result<Option<(Value<'_>, ExId)>, AutomergeError> {
        let prop = prop.into();
        self.read_obj(op::GET, obj.as_ref(), |w| {
            w.put(&prop).put(&None::<Vec<ChangeHash>>);
        })
    }

    fn get_at<O: AsRef<ExId>, P: Into<Prop>>(
        &self,
        obj: O,
        prop: P,
        heads: &[ChangeHash],
    ) -> Result<Option<(Value<'_>, ExId)>, AutomergeError> {
        let prop = prop.into();
        self.read_obj(op::GET, obj.as_ref(), |w| {
            w.put(&prop).put(&heads_of(heads));
        })
    }

    fn get_all<O: AsRef<ExId>, P: Into<Prop>>(
        &self,
        obj: O,
        prop: P,
    ) -> Result<Vec<(Value<'_>, ExId)>, AutomergeError> {
        let prop = prop.into();
        self.read_obj(op::GET_ALL, obj.as_ref(), |w| {
            w.put(&prop).put(&None::<Vec<ChangeHash>>);
        })
    }

    fn get_all_at<O: AsRef<ExId>, P: Into<Prop>>(
        &self,
        obj: O,
        prop: P,
        heads: &[ChangeHash],
    ) -> Result<Vec<(Value<'_>, ExId)>, AutomergeError> {
        let prop = prop.into();
        self.read_obj(op::GET_ALL, obj.as_ref(), |w| {
            w.put(&prop).put(&heads_of(heads));
        })
    }

    fn get_missing_deps(&self, heads: &[ChangeHash]) -> Vec<ChangeHash> {
        self.must_get(op::MISSING_DEPS, |w| {
            w.put(heads);
        })
    }

    #[inline(always)]
    fn get_change_by_hash(&self, _hash: &ChangeHash) -> Option<Change> {
        outside_device("changes")
    }

    #[inline(always)]
    fn has_change(&self, _hash: &ChangeHash) -> bool {
        outside_device("changes")
    }

    #[inline(always)]
    fn stats(&self) -> crate::read::Stats {
        outside_device("document statistics")
    }

    fn text_encoding(&self) -> TextEncoding {
        self.encoding
    }
}

impl Transactable for AutoCommit {
    fn pending_ops(&self) -> usize {
        self.must_get(op::PENDING_OPS, |_| {})
    }

    fn put<O: AsRef<ExId>, P: Into<Prop>, V: Into<ScalarValue>>(
        &mut self,
        obj: O,
        prop: P,
        value: V,
    ) -> Result<(), AutomergeError> {
        let (prop, value) = (prop.into(), value.into());
        self.write_obj(op::PUT, obj.as_ref(), |w| {
            w.put(&prop).put(&value);
        })
    }

    fn put_object<O: AsRef<ExId>, P: Into<Prop>>(
        &mut self,
        obj: O,
        prop: P,
        value: ObjType,
    ) -> Result<ExId, AutomergeError> {
        let prop = prop.into();
        self.read_obj(op::PUT_OBJECT, obj.as_ref(), |w| {
            w.put(&prop).put(&value);
        })
    }

    fn insert<O: AsRef<ExId>, V: Into<ScalarValue>>(
        &mut self,
        obj: O,
        index: usize,
        value: V,
    ) -> Result<(), AutomergeError> {
        let value = value.into();
        self.write_obj(op::INSERT, obj.as_ref(), |w| {
            w.put(&index).put(&value);
        })
    }

    fn insert_object<O: AsRef<ExId>>(
        &mut self,
        obj: O,
        index: usize,
        value: ObjType,
    ) -> Result<ExId, AutomergeError> {
        self.read_obj(op::INSERT_OBJECT, obj.as_ref(), |w| {
            w.put(&index).put(&value);
        })
    }

    fn increment<O: AsRef<ExId>, P: Into<Prop>>(
        &mut self,
        obj: O,
        prop: P,
        value: i64,
    ) -> Result<(), AutomergeError> {
        let prop = prop.into();
        self.write_obj(op::INCREMENT, obj.as_ref(), |w| {
            w.put(&prop).put(&value);
        })
    }

    fn delete<O: AsRef<ExId>, P: Into<Prop>>(
        &mut self,
        obj: O,
        prop: P,
    ) -> Result<(), AutomergeError> {
        let prop = prop.into();
        self.write_obj(op::DELETE, obj.as_ref(), |w| {
            w.put(&prop);
        })
    }

    fn splice<O: AsRef<ExId>, V: Into<hydrate::Value>, I: IntoIterator<Item = V>>(
        &mut self,
        obj: O,
        pos: usize,
        del: isize,
        vals: I,
    ) -> Result<(), AutomergeError> {
        let values: Vec<hydrate::Value> = vals.into_iter().map(Into::into).collect();
        self.write_obj(op::SPLICE, obj.as_ref(), |w| {
            w.put(&pos).put(&del).put(&values);
        })
    }

    fn splice_text<O: AsRef<ExId>>(
        &mut self,
        obj: O,
        pos: usize,
        del: isize,
        text: &str,
    ) -> Result<(), AutomergeError> {
        self.write_obj(op::SPLICE_TEXT, obj.as_ref(), |w| {
            w.put(&pos).put(&del).put(text);
        })
    }

    fn mark<O: AsRef<ExId>>(
        &mut self,
        obj: O,
        mark: Mark,
        expand: ExpandMark,
    ) -> Result<(), AutomergeError> {
        self.write_obj(op::MARK, obj.as_ref(), |w| {
            w.put(&mark).put(&expand);
        })
    }

    fn unmark<O: AsRef<ExId>>(
        &mut self,
        obj: O,
        key: &str,
        start: usize,
        end: usize,
        expand: ExpandMark,
    ) -> Result<(), AutomergeError> {
        self.write_obj(op::UNMARK, obj.as_ref(), |w| {
            w.put(key).put(&start).put(&end).put(&expand);
        })
    }

    fn split_block<'p, O>(&mut self, obj: O, index: usize) -> Result<ExId, AutomergeError>
    where
        O: AsRef<ExId>,
    {
        self.read_obj(op::SPLIT_BLOCK, obj.as_ref(), |w| {
            w.put(&index);
        })
    }

    fn join_block<O: AsRef<ExId>>(&mut self, text: O, index: usize) -> Result<(), AutomergeError> {
        self.write_obj(op::JOIN_BLOCK, text.as_ref(), |w| {
            w.put(&index);
        })
    }

    fn replace_block<'p, O>(&mut self, text: O, index: usize) -> Result<ExId, AutomergeError>
    where
        O: AsRef<ExId>,
    {
        self.read_obj(op::REPLACE_BLOCK, text.as_ref(), |w| {
            w.put(&index);
        })
    }

    fn base_heads(&self) -> Vec<ChangeHash> {
        self.must_get(op::BASE_HEADS, |_| {})
    }

    fn update_text<S: AsRef<str>>(
        &mut self,
        obj: &ExId,
        new_text: S,
    ) -> Result<(), AutomergeError> {
        self.write_obj(op::UPDATE_TEXT, obj, |w| {
            w.put(new_text.as_ref());
        })
    }

    fn update_spans<O: AsRef<ExId>, I: IntoIterator<Item = Span>>(
        &mut self,
        text: O,
        config: UpdateSpansConfig,
        new_text: I,
    ) -> Result<(), AutomergeError> {
        let mut per_mark: Vec<(String, ExpandMark)> = config.per_mark_expands.into_iter().collect();
        per_mark.sort_by(|a, b| a.0.cmp(&b.0));
        let spans: Vec<Span> = new_text.into_iter().collect();
        self.write_obj(op::UPDATE_SPANS, text.as_ref(), |w| {
            w.put(&config.default_expand).put(&per_mark).put(&spans);
        })
    }

    fn update_object<O: AsRef<ExId>>(
        &mut self,
        obj: O,
        new_value: &hydrate::Value,
    ) -> Result<(), crate::error::UpdateObjectError> {
        Ok(self.write_obj(op::UPDATE_OBJECT, obj.as_ref(), |w| {
            w.put(new_value);
        })?)
    }

    fn batch_create_object<O: AsRef<ExId>, P: Into<Prop>>(
        &mut self,
        obj: O,
        prop: P,
        value: &hydrate::Value,
        insert: bool,
    ) -> Result<ExId, AutomergeError> {
        let prop = prop.into();
        self.read_obj(op::BATCH_CREATE_OBJECT, obj.as_ref(), |w| {
            w.put(&prop).put(value).put(&insert);
        })
    }

    fn init_root_from_hydrate(&mut self, value: &hydrate::Map) -> Result<(), AutomergeError> {
        self.write_obj(op::INIT_ROOT_FROM_HYDRATE, &ROOT, |w| {
            w.put(value);
        })
    }
}
