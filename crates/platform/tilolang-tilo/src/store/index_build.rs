//! A package's index in the store: opening it, building it from the record whole (off the
//! frame whenever a frame can be woken to take it in), catching it up from the heads it
//! reflects, and putting a fresh one in place of a broken one. What is built here is what the
//! store's reads answer from ([`Inner::view`]).
//!
//! A fresh index is built in two steps: the first holds the record's own chunk with the first
//! chunk it places, which is where a window's first rows are, and is read from while the rest
//! of the record is derived off the frame. The chunks that arrive while it is built are what
//! the next step derives. A count read before the index covers the record is the part of the
//! workspace read so far, which the runtime shows as not yet known rather than as the count
//! ([`TiloStore::index_whole`]).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use tilolang_runtime::ids::{AppId, DocId, TypeId};
use tilolang_runtime::store::doc_of;

use super::reading::{build, everything, mirror_of, scan};
use super::{Doc, Inner, Nested, TiloStore};
use crate::background::{Pending, Poll};
use crate::chunk::{self, ChunkNo, Stamps};
use crate::index::derive::{Derive, DocReads, Reached};
use crate::index::view::View;
use crate::index::{self as idx, Held, Index, IndexError, Opened};
use crate::reads::Places;

/// Whose the things a chunk the record's manifest names are: the apps the store knows them by, or
/// every app's, where it holds no record of them ([`Inner::chunks_of_apps`]).
enum Theirs<'a> {
    /// The apps whose runs or placed resources the record has in the chunk.
    Apps(BTreeSet<&'a str>),
    /// Any app's: the manifest names something there the store cannot say the app of.
    Any,
}

/// Where a package's index is in being had.
pub(super) enum IndexState {
    /// Read from: it reflects `heads`, with what the store wrote over it in the overlay.
    Ready,
    /// A step is being derived from the record off the frame, reflecting the heads given once
    /// it is taken in; `true` while it is a build's step, `false` while it catches the index up
    /// to what the record holds.
    Building(Pending<Result<(Stamps, DocReads), IndexError>>, bool),
    /// Nothing to read, and no record yet to build it from.
    Missing,
}

/// Where an index is in being had, without what is being built for it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Ready,
    Building,
    Missing,
}

/// One package's index, as the store holds it.
pub(super) struct AppIndex {
    pub(super) index: Held,
    /// The index as its last commit left it.
    pub(super) snapshot: Arc<idx::Snapshot>,
    /// The heads that commit reflects of each chunk it was built from.
    pub(super) heads: Stamps,
    schema: String,
    state: IndexState,
    /// Whether the index holds rows to read, though they may not be the whole record's yet: a
    /// kept index does, and so does one after a step of its build was taken in — the window
    /// opens on those rows while the rest is derived off the frame.
    readable: bool,
}

impl AppIndex {
    /// Whether the index covers the record: it is neither being built nor waiting for it.
    pub(super) fn ready(&self) -> bool {
        matches!(self.state, IndexState::Ready)
    }

    /// Whether the index holds rows to read (see [`Self::readable`]).
    pub(super) fn answers(&self) -> bool {
        self.readable
    }

    fn stage(&self) -> Stage {
        match self.state {
            IndexState::Ready => Stage::Ready,
            IndexState::Building(..) => Stage::Building,
            IndexState::Missing => Stage::Missing,
        }
    }
}

impl TiloStore {
    /// Open (or build) the index of the package `app`, whose record schemas are `schema` (as
    /// [`crate::meta::MetaExt::schema_text`] writes them). An index of another schema is built
    /// anew; one already open with this schema is kept.
    pub fn open_index(&self, app: &str, schema: &str) {
        let mut inner = self.mutate();
        if inner
            .apps
            .get(app)
            .is_some_and(|held| held.schema == schema)
        {
            return;
        }
        inner.apps.remove(app);
        let opened = match inner.index_dir.as_deref() {
            Some(dir) => Index::open(&idx::file_of(dir, app), app, schema),
            None => in_memory(app, schema).map(Opened::Empty),
        };
        // A file that cannot be had (another process holds it, or a window of another version of
        // the package reads it) leaves an index in memory.
        let opened = opened.or_else(|_| in_memory(app, schema).map(Opened::Empty));
        let Ok(opened) = opened else {
            return;
        };
        let (index, heads) = match opened {
            Opened::Ready(index, heads) => (index, Some(heads)),
            Opened::Empty(index) => (index, None),
        };
        let Ok(snapshot) = index.snapshot() else {
            return;
        };
        let held = AppIndex {
            index,
            snapshot,
            heads: heads.clone().unwrap_or_default(),
            schema: schema.to_owned(),
            state: if heads.is_some() {
                IndexState::Ready
            } else {
                IndexState::Missing
            },
            readable: heads.is_some(),
        };
        inner.apps.insert(app.to_owned(), held);
        if inner.chunks.has_record() {
            // The records under ids the app had before are its own from now on.
            if !crate::schema::previously(app).is_empty() {
                inner.rescan();
            }
            inner.bring_up(app);
        } else {
            inner.index_docs();
        }
        inner.changed();
        drop(inner);
        self.changing().notify_all();
    }

    /// Whether every package's index covers the record: none is being built, or waits for the
    /// record. A window that opened over the rows an index holds while the rest of it is still
    /// being derived shows what it reads; this is what an index read in full, to publish it,
    /// waits for.
    #[must_use]
    pub fn index_ready(&self) -> bool {
        self.held().apps.values().all(AppIndex::ready)
    }

    /// Whether every package's index holds rows to read, though they may not be the whole
    /// record's yet: a kept index does, and so does one after the first step of its build was
    /// taken in, off the frame. Until then its lists would show nothing, and the window shows
    /// the platform's loading state.
    #[must_use]
    pub fn index_answers(&self) -> bool {
        self.held().apps.values().all(AppIndex::answers)
    }

    /// Whether a sync would take in work that changes what a view reads: an index being derived
    /// off the frame, an index waiting for the record it is built from, an index a read found
    /// corrupt that is built anew, or a chunk of the record no index read from reflects, which
    /// the store reads before its rows can be listed. Until it has them, the rows, counts and
    /// lists a window reads are of the part of the workspace read so far, so a frame taken then
    /// is not the frame of the workspace the record holds.
    ///
    /// A window whose store owes one syncs before it rests, whatever else is still
    /// ([`crate::host::Host::index_pending`]). An index derived off the frame wakes the window
    /// when it lands (`TiloStore::set_waker`), and what a frame reads in is read within it, so
    /// nothing polls for either.
    ///
    /// A chunk the source cannot give is not one this owes: the store asked for it once and waits
    /// for the sync that brings it ([`Inner::waited`]), so a window rests over the rows it has
    /// while the chunk is away rather than reading what is not here at every frame.
    #[must_use]
    pub fn index_pending(&self) -> bool {
        let held = self.held();
        if !held.chunks.has_record() {
            return false;
        }
        // An index that is not read from is built, or waits for the chunks it is built from,
        // and an index a read found corrupt is built anew off the frame.
        if held
            .apps
            .values()
            .any(|held| !held.ready() || held.index.stats().is_corrupt())
        {
            return true;
        }
        // Every index answers rows, so a chunk the manifest names that none reflects is read
        // now, in this frame ([`TiloStore::load_short`]).
        held.answers_all() && !held.chunks_to_read().is_empty()
    }

    /// Whether every index read from covers the record: it is read from, every chunk the
    /// manifest names that has something of an app the store reads from is either held and
    /// derived or, not held, stamped by the index an earlier run kept, and no chunk the store
    /// holds is left to derive. Until it does, a length or count the store answers is the part of
    /// the workspace read so far, not the workspace's (`Store::settled`). A window shows what has
    /// no final answer yet as not yet known, rather than as a count that changes under the person
    /// reading it.
    ///
    /// A chunk the record names nothing of any app the store reads from in is another app's, and
    /// is no part of what these indexes answer: a window that never reads it holds every row of
    /// its own all the same ([`Inner::chunks_of_apps`]).
    #[must_use]
    pub fn index_whole(&self) -> bool {
        self.held().whole()
    }

    /// Take in the indexes built off the frame since the last call. Returns whether one was:
    /// what every view reads may have changed.
    pub fn poll_index(&self) -> bool {
        self.take_in_builds(false)
    }

    /// [`Self::poll_index`], waiting for the indexes being built: for a host without frames.
    /// What a wait is for is every index the store reads from: a chunk read for an index that
    /// waits for its record starts the build of that index ([`Inner::bring_up`]), and that
    /// build is waited for too. A build is taken in a step at a time, so this waits through
    /// every step of it: what it returns leaves the index covering the record.
    pub fn wait_index(&self) -> bool {
        self.take_in_builds(true)
    }

    /// Whether an index is being derived off the frame.
    fn building(&self) -> bool {
        self.held()
            .apps
            .values()
            .any(|held| matches!(held.state, IndexState::Building(..)))
    }

    fn take_in_builds(&self, wait: bool) -> bool {
        let mut any = false;
        loop {
            any |= self.take_in_round(wait);
            if !wait || !self.building() || self.index_whole() {
                return any;
            }
        }
    }

    /// One pass of [`Self::take_in_builds`]: the builds that ended, the chunks no index read
    /// from reflects, and the indexes still waiting for their record.
    fn take_in_round(&self, wait: bool) -> bool {
        let mut inner = self.mutate();
        let apps: Vec<AppId> = inner.apps.keys().cloned().collect();
        let mut any = false;
        // An index a read found corrupt is not read again: it is built anew from the record.
        let broken: Vec<AppId> = inner
            .apps
            .iter()
            .filter(|(_, held)| held.ready() && held.index.stats().is_corrupt())
            .map(|(app, _)| app.clone())
            .collect();
        for app in broken {
            any = true;
            inner.reopen_broken(&app);
        }
        for app in apps {
            let Some(held) = inner.apps.get_mut(&app) else {
                continue;
            };
            let IndexState::Building(pending, step) = &held.state else {
                continue;
            };
            let step = *step;
            let poll = if wait { pending.wait() } else { pending.poll() };
            let ended = match poll {
                Poll::Running => continue,
                Poll::Ready(ended) => ended,
                Poll::Lost => Err(IndexError::Io("the build ended without a result".into())),
            };
            any = true;
            if let Ok((heads, reads)) = ended {
                inner.count(reads);
                if inner.after_step(&app, heads, step) {
                    // The rest of the record, derived off the frame while the window shows
                    // what the step it has read holds.
                    continue;
                }
                if step {
                    inner.stats.builds = inner.stats.builds.saturating_add(1);
                }
            } else {
                // Built in memory instead, on the frame: an index is owed either way.
                let schema = inner
                    .apps
                    .get(&app)
                    .map(|held| held.schema.clone())
                    .unwrap_or_default();
                if let Ok(index) = in_memory(&app, &schema)
                    && let Ok(snapshot) = index.snapshot()
                {
                    inner.apps.insert(
                        app.clone(),
                        AppIndex {
                            index,
                            snapshot,
                            heads: Stamps::new(),
                            schema,
                            state: IndexState::Missing,
                            readable: false,
                        },
                    );
                    inner.build_now(&app);
                }
            }
        }
        if any {
            inner.rebuild_overlay();
            inner.changed();
            inner.notify_all();
        }
        // An index built over fewer chunks than the manifest names answers no row in the ones
        // it does not, and a window would show a list shorter than the workspace's.
        drop(inner);
        let loaded = self.load_short();
        // What the store was not sent and could read is read: the indexes still waiting for their
        // record are built now, over every chunk of it ([`Inner::bring_up`] waits for this
        // rather than building over the chunk that arrived first), or over what the store has
        // when the chunks cannot be read.
        let mut inner = self.mutate();
        let built = inner.build_missing();
        let any = any || loaded || built;
        if built {
            inner.changed();
            inner.notify_all();
        }
        any
    }

    /// Read every chunk the record's manifest names that no index read from reflects: an index
    /// built over fewer chunks answers no row in the missing ones, and a window would show a
    /// list shorter than the workspace's. A store that can load them loads them, as one that
    /// was sent every chunk opens; an index kept from an open over all of them needs none.
    /// Returns whether a chunk was read.
    ///
    /// An index that answers no row yet is building its first step: the chunk that step derives
    /// is read first, and a window shows its rows once the step is in. The rest of the record is
    /// read after them, so a workspace of fifty chunks opens on its rows as one of one does.
    pub fn load_short(&self) -> bool {
        let short: Vec<ChunkNo> = {
            let held = self.held();
            if held.answers_all() {
                held.chunks_to_read()
            } else {
                // An index that answers no row yet is building its first step: the chunk that
                // step derives is read first, and the rest of the record after them.
                held.chunks_to_read().into_iter().take(1).collect()
            }
        };
        let mut loaded = false;
        for chunk in short {
            match self.load_chunk(chunk) {
                Ok(()) => loaded = true,
                Err(error) => {
                    tracing::warn!(%error, chunk, "a chunk the index does not reflect is not loaded");
                }
            }
        }
        loaded
    }

    /// Everything the index of `app` holds now (the store's open writes apart): what tests
    /// compare with an index built from the record whole.
    #[must_use]
    pub fn index_contents(&self, app: &str) -> Option<idx::Contents> {
        let inner = self.held();
        let held = inner.apps.get(app).filter(|held| held.ready())?;
        held.snapshot.contents().ok()
    }

    /// Whether the store reads nothing over its indexes: no write of its own is pending, and the
    /// index each app reads from reflects every write the record took. What tests check once
    /// the record holds every write and the indexes caught up.
    #[must_use]
    pub fn overlay_empty(&self) -> bool {
        let inner = self.held();
        inner.overlay.is_empty() && inner.accepted.is_empty()
    }

    /// Wait for the commits to the indexes so far to reach the disk: when a window closes.
    pub fn persist_index(&self) {
        for held in self.held().apps.values() {
            if held.index.path().is_some()
                && let Err(error) = held.index.persist()
            {
                // The index is rebuilt from the record where the disk lost it.
                tracing::warn!(%error, "an index did not reach the disk");
            }
        }
    }
}

impl Inner {
    /// The index of `app` as the store's writes leave it, when it is read from: one being
    /// built answers from what its steps have derived so far, so a window opened over them
    /// reads the rows it shows while the rest of the record is derived under them.
    pub(super) fn view(&self, app: &str) -> Option<View<'_>> {
        let held = self.apps.get(app).filter(|held| held.answers())?;
        Some(View {
            snapshot: &held.snapshot,
            overlay: &self.overlay,
            clock: self.clock.read(),
        })
    }

    /// The records every index read from holds: what a store without the record knows of
    /// the workspace.
    fn index_docs(&mut self) {
        let mut index = BTreeMap::new();
        for (app, held) in self.apps.iter().filter(|(_, held)| held.answers()) {
            for (doc, ty, _chunk) in held.snapshot.docs().unwrap_or_default() {
                index.insert(doc, (app.clone(), ty));
            }
        }
        self.index = index;
    }

    /// Take the document `doc` as chunk `chunk`: the store's mirror of it is a copy of it, the
    /// resources it holds are read again, and each index catches up with what that chunk's
    /// changes made — nothing when the index already reflects them, which is how a workspace
    /// opens over the index an earlier run left. The chunks the store holds beside it are not
    /// read again: opening a workspace of many chunks costs what those chunks hold, once.
    pub(super) fn attach_document(&mut self, chunk: ChunkNo, doc: &Doc) {
        self.stats.attaches = self.stats.attaches.saturating_add(1);
        // The chunk the store waited on is here: what reads the record covers it from now on.
        self.waited.remove(&chunk);
        self.versions.remove(&chunk);
        self.nested.clear();
        let mirror = mirror_of(doc);
        let seen = mirror.committed_document().get_heads();
        let now = scan(&mirror);
        let before: Vec<DocId> = self
            .chunks
            .mirror(chunk)
            .map(|held| scan(held).into_keys().collect())
            .unwrap_or_default();
        self.chunks.set_seen(chunk, seen);
        self.chunks.insert(chunk, mirror);
        if chunk == chunk::FIRST {
            self.chunks.reread_placement();
        }
        self.scan_chunk(chunk, now, before);
        let apps: Vec<AppId> = self.apps.keys().cloned().collect();
        for app in apps {
            self.bring_up(&app);
        }
        self.rebuild_overlay();
        self.changed();
    }

    /// Have the index of `app` cover the chunks the store holds: the rest of the record derived
    /// off the frame while the window shows what is already there, else caught up on the frame
    /// in a store nothing wakes, else built anew when the record lacks its heads.
    fn bring_up(&mut self, app: &str) {
        let Some(held) = self.apps.get(app) else {
            return;
        };
        let stage = held.stage();
        match stage {
            // The build derives the rest itself when its step ends ([`Inner::after_step`]).
            Stage::Building => {}
            // The record names chunks the store does not hold yet, and it can read them: an
            // index built now would cover the record's own chunk alone, with no row of what the
            // window's first screen shows. The chunks are read first, and the build starts over
            // them ([`TiloStore::take_in_builds`]).
            Stage::Missing if self.waits_for_rows() => {}
            Stage::Missing => self.rebuild(app),
            // Caught up, or built anew when the record lacks its heads. Finding a change by
            // its hash reads the record's whole history: an index at the record's heads, the
            // usual open, asks for none.
            Stage::Ready => self.catch_up(app),
        }
    }

    /// The chunks the store holds that the index of `app` does not reflect: what the record
    /// gained since the heads the index carries, and the whole of a record it has not derived
    /// yet. The chunks the store was not sent are not here to derive.
    fn unreflected(&self, app: &str) -> Vec<ChunkNo> {
        let Some(held) = self.apps.get(app) else {
            return Vec::new();
        };
        let mut chunks: Vec<ChunkNo> = self
            .chunks
            .docs()
            .filter(|(chunk, doc)| {
                held.heads.get(chunk) != Some(&doc.committed_document().get_heads())
            })
            .map(|(chunk, _)| chunk)
            .collect();
        chunks.sort_unstable();
        chunks
    }

    /// Whether every index read from answers rows.
    fn answers_all(&self) -> bool {
        self.apps.values().all(AppIndex::answers)
    }

    /// The chunks the record's manifest names that no index read from reflects, the store does
    /// not hold, and that hold something of an app the store reads from: what it reads to cover
    /// the record ([`TiloStore::load_short`]). Empty when it cannot read chunks: its indexes are
    /// what the record's manifest was read into.
    ///
    /// A chunk the source could not give is not here to read: it is waited on rather than asked
    /// for again ([`Inner::waited`]), so the store owes no sync for it until it arrives.
    fn chunks_to_read(&self) -> Vec<ChunkNo> {
        if self.chunk_loader.is_none() || !self.chunks.has_record() {
            return Vec::new();
        }
        let reflected: BTreeSet<ChunkNo> = self
            .apps
            .values()
            .filter(|held| held.answers())
            .flat_map(|held| held.heads.keys().copied())
            .collect();
        let whose = self.chunks_of_apps();
        self.chunks
            .placement()
            .chunks()
            .iter()
            .copied()
            .filter(|chunk| !reflected.contains(chunk) && self.chunks.mirror(*chunk).is_none())
            .filter(|chunk| !self.waited.contains(chunk))
            .filter(|chunk| self.read_for_an_app(&whose, *chunk))
            .collect()
    }

    /// Whether every index read from covers the record: none is being built, every chunk the
    /// manifest names that has something of an app the store reads from is covered by that app's
    /// index, and no chunk the store holds is left to derive ([`TiloStore::index_whole`]).
    fn whole(&self) -> bool {
        if !self.chunks.has_record() {
            return true;
        }
        if self.apps.values().any(|held| !held.ready()) {
            return false;
        }
        // A chunk the store holds is the index's once it derived it ([`Self::unreflected`]), and
        // one it does not hold is the index's when the index was read stamped with it: a window
        // opens over the index a run kept without reading the whole record, and counts the whole
        // of it from that index. A chunk the record names nothing of an app in is another app's:
        // an index that never read it holds every row of its own all the same, and waits for no
        // stamp it can never write ([`Self::chunks_of_apps`]).
        let unheld: Vec<ChunkNo> = self
            .chunks
            .placement()
            .chunks()
            .into_iter()
            .filter(|chunk| self.chunks.mirror(*chunk).is_none())
            .collect();
        if !unheld.is_empty() {
            let whose = self.chunks_of_apps();
            for chunk in unheld {
                if !self.stamped(&whose, chunk) {
                    return false;
                }
            }
        }
        self.apps.keys().all(|app| self.unreflected(app).is_empty())
    }

    /// The apps the record's manifest says have something in each chunk it names: the runs of a
    /// collection of one of their resources, and the resources it places there. A chunk the
    /// manifest names nothing of any app in is missing here, and is no app's.
    fn chunks_of_apps(&self) -> BTreeMap<ChunkNo, Theirs<'_>> {
        let placement = self.chunks.placement();
        let mut whose: BTreeMap<ChunkNo, Theirs<'_>> = BTreeMap::new();
        let mut note = |chunk: ChunkNo, doc: &str| {
            match self.app_of(doc) {
                Some(app) => match whose.entry(chunk) {
                    std::collections::btree_map::Entry::Vacant(slot) => {
                        slot.insert(Theirs::Apps(BTreeSet::from([app])));
                    }
                    std::collections::btree_map::Entry::Occupied(mut held) => {
                        if let Theirs::Apps(apps) = held.get_mut() {
                            apps.insert(app);
                        }
                    }
                },
                // What the store holds no record of may be any app's: it is read before the
                // index that would count it is said to cover the record.
                None => {
                    whose.insert(chunk, Theirs::Any);
                }
            }
        };
        for ((holder, _field), runs) in placement.all_runs() {
            for chunk in runs.keys() {
                note(*chunk, doc_of(holder));
            }
        }
        for (id, chunk) in placement.placed() {
            note(*chunk, doc_of(id));
        }
        whose
    }

    /// The app the store knows the resource `doc` by, from the chunks it holds.
    fn app_of(&self, doc: &str) -> Option<&str> {
        self.index.get(doc).map(|(app, _)| app.as_str())
    }

    /// Whether every app with something in `chunk` has its index stamped with it: a chunk no app
    /// has anything in needs no stamp from any of them.
    fn stamped(&self, whose: &BTreeMap<ChunkNo, Theirs<'_>>, chunk: ChunkNo) -> bool {
        let stamped = |app: &str| {
            self.apps
                .get(app)
                .is_none_or(|held| held.heads.contains_key(&chunk))
        };
        match whose.get(&chunk) {
            None => true,
            Some(Theirs::Apps(apps)) => apps.iter().all(|app| stamped(app)),
            Some(Theirs::Any) => self.apps.keys().all(|app| stamped(app)),
        }
    }

    /// Whether the record names something of an app the store reads from in `chunk`: what it
    /// reads, as no index reflects it ([`Self::chunks_to_read`]). A chunk that is no loaded
    /// app's holds nothing a window can show, and is not read for one.
    fn read_for_an_app(&self, whose: &BTreeMap<ChunkNo, Theirs<'_>>, chunk: ChunkNo) -> bool {
        match whose.get(&chunk) {
            None => false,
            Some(Theirs::Apps(apps)) => apps.iter().any(|app| self.apps.contains_key(*app)),
            Some(Theirs::Any) => !self.apps.is_empty(),
        }
    }

    /// Whether the store still expects the chunks beside the record's own that the manifest
    /// names, none of which it holds: a workspace opens over the chunks its window shows and
    /// reads the rest as it reaches for them ([`TiloStore::load_short`]), so the build starts
    /// once one of them is read, rather than over a record with no rows. A store that cannot
    /// read them builds over what it has, once the round that would have read them has passed
    /// ([`Inner::build_missing`]).
    fn waits_for_rows(&self) -> bool {
        if !self.chunks.has_record() {
            return false;
        }
        let chunks = self.chunks.placement().chunks();
        !chunks.is_empty()
            && chunks
                .iter()
                .all(|chunk| self.chunks.mirror(*chunk).is_none())
    }

    /// Build the index of each app still waiting for its record: what a cold open does once the
    /// chunks of the record are read, once over all of them, and what an open over the record
    /// alone does over what the store has when the chunks cannot be read.
    fn build_missing(&mut self) -> bool {
        if !self.chunks.has_record() {
            return false;
        }
        let missing: Vec<AppId> = self
            .apps
            .iter()
            .filter(|(_, held)| matches!(held.state, IndexState::Missing))
            .map(|(app, _)| app.clone())
            .collect();
        let built = !missing.is_empty();
        for app in missing {
            self.rebuild(&app);
        }
        built
    }

    /// Put a fresh index in place of the broken one of `app`, built from the record (once it
    /// is had).
    fn reopen_broken(&mut self, app: &str) {
        let Some(held) = self.apps.remove(app) else {
            return;
        };
        // Another window of the record reads the file: it is not this one's to remove.
        let path = (!held.index.shared())
            .then(|| held.index.path().map(Path::to_path_buf))
            .flatten();
        let schema = held.schema.clone();
        drop(held);
        let fresh = path
            .as_deref()
            .and_then(|path| {
                // Absent already is as good as removed: the open below says what is wrong.
                let _ = std::fs::remove_file(path);
                match Index::open(path, app, &schema) {
                    Ok(opened) => Some(opened),
                    Err(error) => {
                        tracing::warn!(
                            app,
                            path = %path.display(),
                            %error,
                            "a broken index could not be made anew on disk: it is kept in memory"
                        );
                        None
                    }
                }
            })
            .map(|opened| match opened {
                Opened::Ready(index, _) | Opened::Empty(index) => index,
            })
            .map_or_else(|| in_memory(app, &schema), Ok);
        let index = match fresh {
            Ok(index) => index,
            Err(error) => {
                tracing::error!(app, %error, "a broken index could not be made anew");
                return;
            }
        };
        let snapshot = match index.snapshot() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                tracing::error!(app, %error, "a fresh index could not be read");
                return;
            }
        };
        self.apps.insert(
            app.to_owned(),
            AppIndex {
                index,
                snapshot,
                heads: Stamps::new(),
                schema,
                state: IndexState::Missing,
                readable: false,
            },
        );
        self.rebuild(app);
        if !self.chunks.has_record() {
            self.index_docs();
        }
    }

    /// Build the index of `app` anew from the documents the store holds: its first step now,
    /// the rest of the record once the window shows what that step holds
    /// ([`Inner::after_step`]).
    pub(super) fn rebuild(&mut self, app: &str) {
        if !self.chunks.has_record() {
            if let Some(held) = self.apps.get_mut(app) {
                held.state = IndexState::Missing;
            }
            return;
        }
        let Some(held) = self.apps.get_mut(app) else {
            return;
        };
        if held.index.shared() {
            // Another window of the record reads the index: emptied, it would read nothing while
            // this one builds. This one builds its own, in memory.
            match in_memory(app, &held.schema) {
                Ok(index) => held.index = index,
                Err(error) => {
                    tracing::error!(app, %error, "an index could not be made in memory");
                    return;
                }
            }
        }
        let cleared = held.index.clear_for_build();
        if cleared.is_ok() {
            // What a fresh build derives is read once its first step is taken in: until then
            // the index holds nothing, and the lists would show nothing.
            held.readable = false;
        }
        // Built off the frame whenever a frame can be woken to take it in, or when it is kept
        // on disk; a store nothing wakes (a capture, a past version read once) builds it here.
        let off_frame = cleared.is_ok() && (self.wake.is_some() || held.index.path().is_some());
        if !off_frame {
            self.build_now(app);
            return;
        }
        self.derive_off_frame(app, self.first_step(), Stamps::new(), true, true);
    }

    /// The chunks a fresh index is first derived from: the first chunk the record places that
    /// the store holds — where a window's first rows are, and what its first screen shows. The
    /// rest of the record, the record's own chunk among it, follows once the window shows them.
    ///
    /// A record that places nothing keeps everything in its own chunk, which is then the step:
    /// that chunk holds a change an item of a collection wrote of the manifest, and reading it
    /// costs what the record holds. Read only with the rest, it would hold up the first rows of
    /// a workspace of a hundred thousand tasks for the manifest's changes, which name no
    /// resource and show no row.
    fn first_step(&self) -> Vec<ChunkNo> {
        let placed = self.chunks.placement();
        let held = placed
            .chunks()
            .into_iter()
            .find(|chunk| self.chunks.mirror(*chunk).is_some());
        match held {
            Some(chunk) => vec![chunk],
            None => vec![chunk::FIRST],
        }
    }

    /// Take in the step of `app`'s build that ended, reflecting `heads`: the index answers the
    /// rows it holds from now on, and the rest of the record — every chunk the store holds that
    /// it does not reflect — is derived off the frame, still as the build's `build` says.
    /// Whether there was one.
    fn after_step(&mut self, app: &str, heads: Stamps, build: bool) -> bool {
        let Some(held) = self.apps.get_mut(app) else {
            return false;
        };
        held.heads = heads.clone();
        if let Ok(snapshot) = held.index.snapshot() {
            held.snapshot = snapshot;
        }
        held.state = IndexState::Ready;
        held.readable = true;
        if !build {
            self.stats.catch_ups = self.stats.catch_ups.saturating_add(1);
        }
        let rest = self.unreflected(app);
        if !self.chunks.has_record() {
            // What the step derived names records the store without the record finds by their
            // index: a window opened over its rows routes reads to them as they are derived.
            self.index_docs();
        }
        if rest.is_empty() {
            return false;
        }
        self.derive_off_frame(app, rest, heads, build, false);
        true
    }

    /// Derive the chunks `part` of the record into the index of `app`, since the heads `from`,
    /// off the frame, as a step of the build `build` says. A step of a build leaves the index
    /// readable while the rest are derived: what ended here is taken in on a frame
    /// ([`TiloStore::poll_index`]).
    fn derive_off_frame(
        &mut self,
        app: &str,
        part: Vec<ChunkNo>,
        from: Stamps,
        build: bool,
        opening: bool,
    ) {
        let Some(held) = self.apps.get(app) else {
            return;
        };
        let index = Arc::clone(&*held.index);
        let before: BTreeMap<DocId, (AppId, TypeId)> = held
            .snapshot
            .docs()
            .unwrap_or_default()
            .into_iter()
            .map(|(doc, ty, _chunk)| (doc, (app.to_owned(), ty)))
            .collect();
        let docs = self.index.clone();
        let chunks = self.chunks.clone();
        let app_id = app.to_owned();
        let clock = self.clock.read();
        let pending = Pending::start(
            move || {
                // A cold build's index is its own: what a record written before items were flat
                // keeps is read from the chunks as it goes.
                let nested = Nested::default();
                let source = Source {
                    index: &index,
                    chunks: &chunks,
                    docs: &docs,
                    nested: &nested,
                };
                let (heads, reads) =
                    derive(&source, &app_id, &from, &part, &before, clock, opening)?;
                Ok((heads, reads))
            },
            self.wake.clone(),
        );
        if let Some(held) = self.apps.get_mut(app) {
            held.state = IndexState::Building(pending, build);
        }
    }

    /// Build the index of `app` from the documents the store holds, here: one kept in memory.
    fn build_now(&mut self, app: &str) {
        if !self.chunks.has_record() {
            return;
        }
        let heads = self.chunks.heads();
        let Some(index) = self.apps.get(app).map(|held| Arc::clone(&*held.index)) else {
            return;
        };
        let schemas = Arc::clone(&self.schemas);
        let clock = self.clock.read();
        let built = index.clear_for_build().and_then(|()| {
            build(
                &index,
                &self.chunks,
                &self.index,
                app,
                &heads,
                &schemas,
                clock,
            )
        });
        if let Ok(reads) = built {
            self.count(reads);
            self.stats.builds = self.stats.builds.saturating_add(1);
        }
        let snapshot = index.snapshot();
        let Some(held) = self.apps.get_mut(app) else {
            return;
        };
        if let (true, Ok(snapshot)) = (built.is_ok(), snapshot) {
            held.state = IndexState::Ready;
            held.readable = true;
            held.heads = heads;
            held.snapshot = snapshot;
        } else {
            held.state = IndexState::Missing;
            held.readable = false;
        }
    }

    /// Apply to the index of `app` what the chunks the store holds gained since the heads it
    /// reflects: the changes of each chunk the index does not reflect yet, and only those.
    /// Nothing when it reflects them all — the open over the index an earlier run left. A chunk
    /// the index reflects and the store was not sent asks for nothing here: what reads its rows
    /// loads it.
    ///
    /// The changes are derived off the frame whenever a frame can be woken to take them in: a
    /// workspace that arrives whole reads its rows while the index catches up to what the record
    /// holds, rather than the frame the chunks arrived in reading all of it.
    fn catch_up(&mut self, app: &str) {
        if !self.chunks.has_record() {
            return;
        }
        let Some(held) = self.apps.get(app) else {
            return;
        };
        let from = held.heads.clone();
        // Off the frame wherever a frame can be woken to take the step in: a workspace that
        // arrives whole reads the rows the store already holds while the index catches up to
        // the rest, rather than the frame the chunks arrived in reading all of it. A store
        // nothing wakes (a capture, a host without frames) catches up here, in its own time.
        let off_frame = self.wake.is_some();
        let moved = self.unreflected(app);
        if moved.is_empty() {
            return;
        }
        if off_frame {
            self.derive_off_frame(app, moved, from, false, false);
            return;
        }
        let Some(held) = self.apps.get(app) else {
            return;
        };
        let now = stamps_after(&self.chunks, &from, &moved);
        // Named against the records the index held: a record made since is derived whole.
        let mut before: BTreeMap<DocId, (AppId, TypeId)> = BTreeMap::new();
        for (doc, ty, _chunk) in held.snapshot.docs().unwrap_or_default() {
            before.insert(doc, (app.to_owned(), ty));
        }
        let index = Arc::clone(&*held.index);
        let docs = self.index.clone();
        let chunks = self.chunks.clone();
        let clock = self.clock.read();
        let source = Source {
            index: &index,
            chunks: &chunks,
            docs: &docs,
            nested: &self.nested,
        };
        let written = derive(&source, app, &from, &moved, &before, clock, false);
        match written {
            Ok((_, reads)) => {
                if let Some(held) = self.apps.get_mut(app) {
                    held.heads = now;
                    if let Ok(snapshot) = held.index.snapshot() {
                        held.snapshot = snapshot;
                    }
                }
                self.stats.catch_ups = self.stats.catch_ups.saturating_add(1);
                self.count(reads);
            }
            Err(_) => self.rebuild(app),
        }
    }
}

/// What a derivation reads: the index it derives into, and the record it derives from.
struct Source<'a> {
    /// The index the step is derived into.
    index: &'a Index,
    /// The chunks the store holds, where the step's changes are read.
    chunks: &'a crate::chunk::Chunks,
    /// Every resource of the record, by document.
    docs: &'a BTreeMap<DocId, (AppId, TypeId)>,
    /// Where the items of records written before items were flat are.
    nested: &'a Nested,
}

/// Derive into the source's index what the chunks `part` of the record hold since the heads
/// `from`: the changes of each chunk the index does not reflect, and only those. `before` is
/// what the index held of each resource. The heads it ends at, and what it read.
fn derive(
    source: &Source<'_>,
    app: &str,
    from: &Stamps,
    part: &[ChunkNo],
    before: &BTreeMap<DocId, (AppId, TypeId)>,
    clock: crate::index::Clock,
    opening: bool,
) -> Result<(Stamps, DocReads), IndexError> {
    let &Source {
        index,
        chunks,
        docs,
        nested,
    } = source;
    let mut ends: BTreeMap<String, idx::reflect::Ends> = BTreeMap::new();
    let mut placed = idx::reflect::Placed::new();
    let mut changed = crate::reads::Changed::default();
    for chunk in part {
        let Some(doc) = chunks.mirror(*chunk) else {
            continue;
        };
        let since = doc
            .committed_document()
            .get_changes(from.get(chunk).map_or(&[], Vec::as_slice));
        // What wrote a resource of the record, as the run tails a collection's every item
        // writes in its manifest write none: a workspace of a hundred thousand items holds a
        // hundred thousand changes that name no resource, and their messages are not read.
        let mut naming = Vec::new();
        let mut walked = Places::of(doc);
        for change in &since {
            let written = walked.written(change, before);
            if written.wrote() {
                naming.push(change);
            }
            changed.take_in(written);
        }
        ends.extend(idx::reflect::ends_of(naming.iter().copied(), doc, docs));
        placed.extend(idx::reflect::placed_by(naming));
    }
    for doc in &changed.nested {
        nested.forget(doc);
    }
    // A change that laid the record out again may have read anything: every record the step
    // brings is derived again, and every one the index already held. The chunks the step does
    // not derive are left to their own, which is what makes the first step small.
    let reached = if changed.unplaced || changed.layout {
        let mut here: BTreeMap<DocId, (AppId, TypeId)> = BTreeMap::new();
        for chunk in part {
            if let Some(doc) = chunks.mirror(*chunk) {
                here.extend(scan(doc));
            }
        }
        everything(before, &here)
    } else {
        Reached {
            docs: changed.docs,
            placed: changed.placed,
        }
    };
    let now = stamps_after(chunks, from, part);
    // An index with nothing of the changes in it derives no row: a change one app made is that
    // app's, whatever else the store holds. What it has taken in is not a row but the heads, and
    // they are written: an index that kept the heads it had catches up from these chunks again
    // at every open.
    if !reached_app(docs, app, &reached, &ends) {
        index.write(|writer| writer.set_heads(&now))?;
        return Ok((now, DocReads::default()));
    }
    // The chunks the step derives from their first change: a step of a build, or a chunk read
    // since the index was. Their items are read once, and each change the step takes in reads
    // the one it names from them. A chunk the index reflects up to a few changes ago is not:
    // the items those name are looked up, as an edit of one task costs one task.
    let whole: Vec<ChunkNo> = part
        .iter()
        .copied()
        .filter(|chunk| from.get(chunk).is_none_or(Vec::is_empty))
        .collect();
    let written = index.write(|writer| {
        // The first step of a build reads only the chunks it derives, and its items are its
        // rows.
        let mut derive = if opening {
            Derive::opening(chunks, docs, nested, clock, part)
        } else {
            Derive::step(chunks, docs, nested, clock, (part, &whole))
        };
        derive.removed_before(writer, app, &ends, &placed)?;
        derive.changes(writer, app, &reached)?;
        derive.versions(writer, app, &ends)?;
        derive.removed(writer, app, &ends)?;
        writer.set_heads(&now)?;
        Ok(derive.reads)
    })?;
    Ok((now, written))
}

/// Whether a change gave `app`'s index anything to take in: a document of it the change
/// reached, or a resource of it the change made a version of. A version is not always of the
/// app that made the change: what an action was made from changed too, so a comment made from a
/// task is a version of the task, and the tasks' index takes it in. A document no index names —
/// one just removed — may be any app's, so every app is derived for it.
pub(crate) fn reached_app(
    docs: &BTreeMap<DocId, (AppId, TypeId)>,
    app: &str,
    reached: &Reached,
    made: &BTreeMap<String, idx::reflect::Ends>,
) -> bool {
    let mut unknown = false;
    for doc in reached.docs.keys().chain(reached.placed.keys()) {
        match docs.get(doc) {
            Some((held, _)) if held == app => return true,
            Some(_) => {}
            None => unknown = true,
        }
    }
    for resource in made.keys() {
        match docs.get(doc_of(resource)) {
            Some((held, _)) if held == app => return true,
            Some(_) => {}
            None => unknown = true,
        }
    }
    unknown
}

/// The stamps `from` brought up to what the chunks `part` hold now, and the stamps of the chunks
/// it was not sent, which the index still reflects.
fn stamps_after(chunks: &crate::chunk::Chunks, from: &Stamps, part: &[ChunkNo]) -> Stamps {
    let mut now = from.clone();
    for chunk in part {
        if let Some(doc) = chunks.mirror(*chunk) {
            now.insert(*chunk, doc.committed_document().get_heads());
        }
    }
    now
}

/// An empty index of `app` in memory, held by the store alone.
fn in_memory(app: &str, schema: &str) -> Result<Held, IndexError> {
    Index::in_memory(app, schema).map(|index| Held::new(Arc::new(index)))
}
