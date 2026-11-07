//! A package's index in the store: opening it, building it from the record whole (off the
//! frame whenever a frame can be woken to take it in), catching it up from the heads it
//! reflects, and putting a fresh one in place of a broken one. What is built here is what the
//! store's reads answer from ([`Inner::view`]).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use tilolang_runtime::ids::{AppId, DocId, TypeId};

use super::reading::{build, everything, mirror_of, scan};
use super::{Doc, Inner, TiloStore};
use crate::background::{Pending, Poll};
use crate::chunk::{self, ChunkNo, Stamps};
use crate::index::derive::{Derive, DocReads, Reached};
use crate::index::view::View;
use crate::index::{self as idx, Index, IndexError, Opened};
use crate::reads::changed_by;

/// Where a package's index is in being had.
pub(super) enum IndexState {
    /// Read from: it reflects `heads`, with what the store wrote over it in the overlay.
    Ready,
    /// Being built from the record off the frame: reflecting the heads given, once done.
    Building(Pending<Result<(Stamps, DocReads), IndexError>>),
    /// Nothing to read, and no record yet to build it from.
    Missing,
}

/// One package's index, as the store holds it.
pub(super) struct AppIndex {
    pub(super) index: Arc<Index>,
    /// The index as its last commit left it.
    pub(super) snapshot: Arc<idx::Snapshot>,
    /// The heads that commit reflects of each chunk it was built from.
    pub(super) heads: Stamps,
    schema: String,
    state: IndexState,
}

impl AppIndex {
    pub(super) fn ready(&self) -> bool {
        matches!(self.state, IndexState::Ready)
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
            None => Index::in_memory(app, schema).map(Opened::Empty),
        };
        // A file that cannot be had (another window holds it) leaves an index in memory.
        let opened = opened.or_else(|_| Index::in_memory(app, schema).map(Opened::Empty));
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
            index: Arc::new(index),
            snapshot,
            heads: heads.clone().unwrap_or_default(),
            schema: schema.to_owned(),
            state: if heads.is_some() {
                IndexState::Ready
            } else {
                IndexState::Missing
            },
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

    /// Whether every package's index is read from: none is being built, or waits for the
    /// record. Until then its lists show nothing, as the platform's loading state.
    #[must_use]
    pub fn index_ready(&self) -> bool {
        self.held().apps.values().all(AppIndex::ready)
    }

    /// Take in the indexes built off the frame since the last call. Returns whether one was:
    /// what every view reads may have changed.
    pub fn poll_index(&self) -> bool {
        self.take_in_builds(false)
    }

    /// [`Self::poll_index`], waiting for the indexes being built: for a host without frames.
    pub fn wait_index(&self) -> bool {
        self.take_in_builds(true)
    }

    fn take_in_builds(&self, wait: bool) -> bool {
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
            let IndexState::Building(pending) = &held.state else {
                continue;
            };
            let poll = if wait { pending.wait() } else { pending.poll() };
            let built = match poll {
                Poll::Running => continue,
                Poll::Ready(built) => built,
                Poll::Lost => Err(IndexError::Io("the build ended without a result".into())),
            };
            any = true;
            if let Ok((heads, reads)) = built {
                inner.count(reads);
                inner.stats.builds = inner.stats.builds.saturating_add(1);
                if let Some(held) = inner.apps.get_mut(&app) {
                    held.state = IndexState::Ready;
                    held.heads = heads;
                    if let Ok(snapshot) = held.index.snapshot() {
                        held.snapshot = snapshot;
                    }
                }
                // What the record gained while it was built.
                inner.catch_up(&app);
            } else {
                // Built in memory instead, on the frame: an index is owed either way.
                let schema = inner
                    .apps
                    .get(&app)
                    .map(|held| held.schema.clone())
                    .unwrap_or_default();
                if let Ok(index) = Index::in_memory(&app, &schema)
                    && let Ok(snapshot) = index.snapshot()
                {
                    inner.apps.insert(
                        app.clone(),
                        AppIndex {
                            index: Arc::new(index),
                            snapshot,
                            heads: Stamps::new(),
                            schema,
                            state: IndexState::Missing,
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
        // it does not, and a window would show a list shorter than the workspace's: a store
        // that can load them loads every chunk, as one that was sent them opens. An index kept
        // from an open over every chunk needs none of this.
        drop(inner);
        let short: Vec<ChunkNo> = {
            let held = self.held();
            let reflected: BTreeSet<ChunkNo> = held
                .apps
                .values()
                .filter(|app| app.ready())
                .flat_map(|app| app.heads.keys().copied())
                .collect();
            if held.chunk_loader.is_none() || !held.chunks.has_record() {
                Vec::new()
            } else {
                held.chunks
                    .placement()
                    .chunks()
                    .iter()
                    .copied()
                    .filter(|chunk| !reflected.contains(chunk))
                    .collect()
            }
        };
        for chunk in short {
            if let Err(error) = self.load_chunk(chunk) {
                tracing::warn!(%error, chunk, "a chunk the index does not reflect is not loaded");
            }
        }
        any
    }

    /// Everything the index of `app` holds now (the store's open writes apart): what tests
    /// compare with an index built from the record whole.
    #[must_use]
    pub fn index_contents(&self, app: &str) -> Option<idx::Contents> {
        let inner = self.held();
        let held = inner.apps.get(app).filter(|held| held.ready())?;
        held.snapshot.contents().ok()
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
    /// The index of `app` as the store's writes leave it, when it is read from.
    pub(super) fn view(&self, app: &str) -> Option<View<'_>> {
        let held = self.apps.get(app).filter(|held| held.ready())?;
        Some(View {
            snapshot: &held.snapshot,
            overlay: &self.overlay,
        })
    }

    /// The records every index read from holds: what a store without the record knows of
    /// the workspace.
    fn index_docs(&mut self) {
        let mut index = BTreeMap::new();
        for (app, held) in self.apps.iter().filter(|(_, held)| held.ready()) {
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
        self.versions = None;
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

    /// Have the index of `app` reflect the mirror: caught up from its heads when the mirror
    /// holds them, else built anew (off the frame whenever a frame can be woken to take it
    /// in).
    fn bring_up(&mut self, app: &str) {
        let Some(held) = self.apps.get(app) else {
            return;
        };
        match held.state {
            IndexState::Building(_) => {}
            // Caught up, or built anew when the record lacks its heads. Finding a change by
            // its hash reads the record's whole history: an index at the record's heads, the
            // usual open, asks for none.
            IndexState::Ready => self.catch_up(app),
            IndexState::Missing => self.rebuild(app),
        }
    }

    /// Put a fresh index in place of the broken one of `app`, built from the record (once it
    /// is had).
    fn reopen_broken(&mut self, app: &str) {
        let Some(held) = self.apps.remove(app) else {
            return;
        };
        let path = held.index.path().map(Path::to_path_buf);
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
            .map_or_else(|| Index::in_memory(app, &schema), Ok);
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
                index: Arc::new(index),
                snapshot,
                heads: Stamps::new(),
                schema,
                state: IndexState::Missing,
            },
        );
        self.rebuild(app);
        if !self.chunks.has_record() {
            self.index_docs();
        }
    }

    /// Build the index of `app` anew from the documents the store holds.
    pub(super) fn rebuild(&mut self, app: &str) {
        if !self.chunks.has_record() {
            if let Some(held) = self.apps.get_mut(app) {
                held.state = IndexState::Missing;
            }
            return;
        }
        let Some(held) = self.apps.get(app) else {
            return;
        };
        // Built off the frame whenever a frame can be woken to take it in, or when it is kept
        // on disk; a store nothing wakes (a capture, a past version read once) builds it here.
        let off_frame = held.index.clear_for_build().is_ok()
            && (self.wake.is_some() || held.index.path().is_some());
        if !off_frame {
            self.build_now(app);
            return;
        }
        let index = Arc::clone(&held.index);
        let docs = self.index.clone();
        let chunks = self.chunks.clone();
        let heads = chunks.heads();
        let app_id = app.to_owned();
        let schemas = Arc::clone(&self.schemas);
        let pending = Pending::start(
            move || {
                let reads = build(&index, &chunks, &docs, &app_id, &heads, &schemas)?;
                Ok((heads, reads))
            },
            self.wake.clone(),
        );
        if let Some(held) = self.apps.get_mut(app) {
            held.state = IndexState::Building(pending);
        }
    }

    /// Build the index of `app` from the documents the store holds, here: one kept in memory.
    fn build_now(&mut self, app: &str) {
        if !self.chunks.has_record() {
            return;
        }
        let heads = self.chunks.heads();
        let Some(index) = self.apps.get(app).map(|held| Arc::clone(&held.index)) else {
            return;
        };
        let schemas = Arc::clone(&self.schemas);
        let built = index
            .clear_for_build()
            .and_then(|()| build(&index, &self.chunks, &self.index, app, &heads, &schemas));
        if let Ok(reads) = built {
            self.count(reads);
            self.stats.builds = self.stats.builds.saturating_add(1);
        }
        let snapshot = index.snapshot();
        let Some(held) = self.apps.get_mut(app) else {
            return;
        };
        match (built, snapshot) {
            (Ok(_), Ok(snapshot)) => {
                held.state = IndexState::Ready;
                held.heads = heads;
                held.snapshot = snapshot;
            }
            _ => held.state = IndexState::Missing,
        }
    }

    /// Apply to the index of `app` what the chunks the store holds gained since the heads it
    /// reflects: the changes of each chunk the index does not reflect yet, and only those.
    /// Nothing when it reflects them all — the open over the index an earlier run left. A chunk
    /// the index reflects and the store was not sent asks for nothing here: what reads its rows
    /// loads it.
    fn catch_up(&mut self, app: &str) {
        if !self.chunks.has_record() {
            return;
        }
        let Some(held) = self.apps.get(app) else {
            return;
        };
        let from = held.heads.clone();
        let moved: Vec<ChunkNo> = self
            .chunks
            .docs()
            .filter(|(chunk, doc)| from.get(chunk) != Some(&doc.committed_document().get_heads()))
            .map(|(chunk, _)| chunk)
            .collect();
        if moved.is_empty() {
            return;
        }
        let now = stamps_after(&self.chunks, &from);
        // Named against the records the index held: a record made since is derived whole.
        let mut before: BTreeMap<DocId, (AppId, TypeId)> = BTreeMap::new();
        for (doc, ty, _chunk) in held.snapshot.docs().unwrap_or_default() {
            before.insert(doc, (app.to_owned(), ty));
        }
        let mut ends: BTreeMap<String, idx::reflect::Ends> = BTreeMap::new();
        let mut placed = idx::reflect::Placed::new();
        let mut changed = crate::reads::Changed::default();
        for chunk in &moved {
            let Some(doc) = self.chunks.mirror(*chunk) else {
                continue;
            };
            let since = doc
                .committed_document()
                .get_changes(from.get(chunk).map_or(&[], Vec::as_slice));
            ends.extend(idx::reflect::ends_of(&since, doc, &self.index));
            placed.extend(idx::reflect::placed_by(&since));
            changed.take_in(changed_by(&since, doc, &before));
        }
        for doc in &changed.nested {
            self.nested.forget(doc);
        }
        // A change that laid the record out again may have read anything: every record the
        // store holds is derived again, the ones it was not sent keeping what the index has
        // ([`Derive::record`] leaves them as they are).
        let reached = if changed.unplaced || changed.layout {
            everything(&before, &self.index)
        } else {
            Reached {
                docs: changed.docs,
                placed: changed.placed,
            }
        };
        let index = Arc::clone(&held.index);
        let docs = self.index.clone();
        let nested = &self.nested;
        let written = index.write(|writer| {
            let mut derive = Derive::new(&self.chunks, &docs, nested);
            derive.removed_before(writer, app, &ends, &placed)?;
            derive.changes(writer, app, &reached)?;
            derive.versions(writer, app, &ends)?;
            derive.removed(writer, app, &ends)?;
            writer.set_heads(&now)?;
            Ok(derive.reads)
        });
        match written {
            Ok(reads) => {
                self.count(reads);
                self.stats.catch_ups = self.stats.catch_ups.saturating_add(1);
                if let Some(held) = self.apps.get_mut(app) {
                    held.heads = now;
                    if let Ok(snapshot) = held.index.snapshot() {
                        held.snapshot = snapshot;
                    }
                }
            }
            Err(_) => self.rebuild(app),
        }
    }
}

/// The stamps `from` brought up to what the store holds now: the heads of each chunk it holds,
/// and the stamps of the chunks it was not sent, which its index still reflects.
fn stamps_after(chunks: &crate::chunk::Chunks, from: &Stamps) -> Stamps {
    let mut now = from.clone();
    for (chunk, doc) in chunks.docs() {
        now.insert(chunk, doc.committed_document().get_heads());
    }
    now
}
