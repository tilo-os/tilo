//! What search finds a package's resources by: the texts of every record and item, kept in a
//! [`SearchIndex`] of Tilo's search layer, which every surface that finds things by typed text
//! searches through. Text is folded as everywhere in Tilo (case, diacritics, compatibility
//! forms), found by substring and prefix as it is typed, words spread over a resource's texts,
//! and hits rank by [`MatchTier`](tilo_search::MatchTier) × [`FieldWeight`]: a resource's
//! naming text (its first text field, in id order) is its title, its other texts weigh as body
//! text.
//!
//! The search index is derived from the index's fields, as the lists are from the record: it is
//! taken in from the index's last commit, and from then on follows every commit of the index,
//! resource by resource. Each commit reads the resources whose texts it changed back from the
//! commit itself, so the search index holds what the index holds, at the heads it is stamped
//! with.
//!
//! It is taken in when something first searches: opening a window costs what it shows, not the
//! room's texts (their search index is most of what a room of many items holds in memory). For
//! an index kept on disk (a live window's) it is taken in off the frame, as the index itself is
//! built ([`crate::background`]): the first search starts it on a thread of its own and finds
//! nothing of the package, as its lists show nothing until its index is built, and a search
//! after the build ends takes it in and answers. The commits that land meanwhile are read back
//! when it is taken in, so it holds what the index holds from its first answer. One kept in
//! memory (a past version, a capture) is taken in at once when first searched, as its index was
//! built at once.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock, RwLockWriteGuard};

use redb::{Database, ReadTransaction, ReadableDatabase as _, ReadableTable as _};
use tilo_search::{Document, FieldWeight, Matching, Page, SearchIndex};
use tilolang_runtime::store::FieldValue;

use super::{FIELDS, IndexError, IndexStats, TYPE, codec};
use crate::background::{Pending, Poll};
use crate::schema::{FieldSchema, MergeKind, TypeSchema, lookup};

/// The weight of the text field `field` of `schema` in search: the text that names a resource
/// (its first text field, in id order) is its title, any other is body text.
#[must_use]
pub fn weight_of(schema: &TypeSchema, field: &FieldSchema) -> FieldWeight {
    let first = schema
        .fields
        .iter()
        .filter(|f| f.kind == MergeKind::Text)
        .map(|f| f.id.as_str())
        .min();
    if first == Some(field.id.as_str()) {
        FieldWeight::TITLE
    } else {
        FieldWeight::BODY
    }
}

/// The texts of a resource of the type `ty` search finds it by, with their weights, in field
/// order, read with `field`: none when it holds no text but blanks.
fn texts_of(
    ty: &str,
    mut field: impl FnMut(&str) -> Result<Option<FieldValue>, IndexError>,
) -> Result<Vec<(String, FieldWeight)>, IndexError> {
    let Some(schema) = lookup(ty) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for spec in schema.fields.iter().filter(|f| f.kind == MergeKind::Text) {
        if let Some(FieldValue::Text(text)) = field(&spec.id)?
            && !text.trim().is_empty()
        {
            out.push((text, weight_of(schema, spec)));
        }
    }
    Ok(out)
}

/// The search index of one package, and which resource each of its keys stands for.
#[derive(Default)]
struct Texts {
    index: SearchIndex,
    keys: HashMap<String, u64>,
    paths: HashMap<u64, String>,
    next: u64,
}

impl Texts {
    fn empty() -> Self {
        Self {
            index: SearchIndex::new(Matching::LOOSE),
            ..Self::default()
        }
    }

    fn key(&mut self, path: &str) -> u64 {
        if let Some(key) = self.keys.get(path) {
            return *key;
        }
        let key = self.next;
        self.next = self.next.saturating_add(1);
        self.keys.insert(path.to_owned(), key);
        self.paths.insert(key, path.to_owned());
        key
    }

    fn put(&mut self, path: &str, texts: Vec<(String, FieldWeight)>) {
        if texts.is_empty() {
            if let Some(key) = self.keys.remove(path) {
                self.paths.remove(&key);
                self.index.remove(key);
            }
            return;
        }
        let key = self.key(path);
        let document = texts
            .into_iter()
            .fold(Document::new(key), |document, (text, weight)| {
                document.field(text, weight)
            });
        self.index.upsert(document);
    }

    fn commit(&mut self) {
        if let Err(error) = self.index.commit() {
            tracing::error!(%error, "could not index the texts of a tilolang app");
        }
    }

    /// Every resource's texts, taken in from the commit `txn` reads.
    fn build(txn: &ReadTransaction) -> Result<Self, IndexError> {
        let fields = txn.open_table(FIELDS)?;
        // Per resource, in path order, its type and the values of its fields.
        let mut resource: Option<String> = None;
        let mut ty: Option<String> = None;
        let mut values: BTreeMap<String, FieldValue> = BTreeMap::new();
        let mut documents: Vec<(String, Vec<(String, FieldWeight)>)> = Vec::new();
        let mut finish = |resource: Option<String>,
                          ty: Option<String>,
                          values: &mut BTreeMap<String, FieldValue>|
         -> Result<(), IndexError> {
            if let (Some(resource), Some(ty)) = (resource, ty) {
                let held = texts_of(&ty, |field| Ok(values.remove(field)))?;
                if !held.is_empty() {
                    documents.push((resource, held));
                }
            }
            values.clear();
            Ok(())
        };
        for entry in fields.iter()? {
            let (key, value) = entry?;
            let (at, field) = key.value();
            if resource.as_deref() != Some(at) {
                finish(resource.take(), ty.take(), &mut values)?;
                resource = Some(at.to_owned());
            }
            if field == TYPE {
                ty = Some(codec::get_text(value.value())?);
            } else if !field.starts_with('$')
                && let FieldValue::Text(text) = codec::get_value(value.value())?
            {
                values.insert(field.to_owned(), FieldValue::Text(text));
            }
        }
        finish(resource, ty, &mut values)?;
        let mut texts = Self::empty();
        for (path, held) in documents {
            texts.put(&path, held);
        }
        texts.commit();
        Ok(texts)
    }

    /// Read the texts of `resources` back from the index's last commit.
    fn read_back(&mut self, db: &Database, resources: &BTreeSet<String>) -> Result<(), IndexError> {
        if resources.is_empty() {
            return Ok(());
        }
        let txn = db.begin_read()?;
        let fields = txn.open_table(FIELDS)?;
        for path in resources {
            let held = match fields.get((path.as_str(), TYPE))? {
                Some(ty) => {
                    let ty = codec::get_text(ty.value())?;
                    texts_of(&ty, |field| {
                        fields
                            .get((path.as_str(), field))?
                            .map(|value| codec::get_value(value.value()))
                            .transpose()
                            .map_err(Into::into)
                    })?
                }
                None => Vec::new(),
            };
            self.put(path, held);
        }
        self.commit();
        Ok(())
    }
}

/// Where a package's search index is in being had.
enum State {
    /// Not taken in: commits leave it be.
    Idle,
    /// Being taken in off the frame, from the commit the build reads.
    Building {
        built: Pending<Result<Texts, IndexError>>,
        /// The resources whose texts commits changed since the build started: read back once
        /// it is taken in, as the build may have read a commit before theirs.
        changed: BTreeSet<String>,
    },
    /// Taken in, and following every commit.
    Built(Box<Texts>),
    /// The last build failed: not started again until something searches.
    Failed(IndexError),
}

/// One resource search found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub resource: String,
    /// Its rank: what orders hits across packages.
    pub rank: i32,
}

/// The search index of one package's index: shared by the index and its snapshots.
pub struct Search {
    db: Arc<Database>,
    stats: Arc<IndexStats>,
    /// Whether it is taken in off the frame: for an index kept on disk, as the index itself is
    /// built off the frame. One kept in memory (a past version, a capture) is built at once,
    /// and so is its search index.
    off_frame: bool,
    state: Mutex<State>,
    /// Held by [`Self::hold`]: a build reads nothing until it is let go.
    gate: Arc<RwLock<()>>,
}

impl std::fmt::Debug for Search {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Search").finish_non_exhaustive()
    }
}

impl Search {
    pub(super) fn new(db: Arc<Database>, stats: Arc<IndexStats>, off_frame: bool) -> Self {
        Self {
            db,
            stats,
            off_frame,
            state: Mutex::new(State::Idle),
            gate: Arc::default(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn failed(&self, error: IndexError) -> State {
        if error == IndexError::Corrupt {
            self.stats.corrupt();
        }
        State::Failed(error)
    }

    /// Forget what it took in: the index was emptied for a build. A build under way is
    /// dropped with what it reads.
    pub(super) fn clear(&self) {
        *self.lock() = State::Idle;
    }

    /// The texts of `resources` changed in a commit of the index that is in: read them back
    /// from the index as it stands now, if search took it in already; if it is being taken
    /// in, once it is.
    pub(super) fn changed(&self, resources: &BTreeSet<String>) {
        if resources.is_empty() {
            return;
        }
        let mut state = self.lock();
        match &mut *state {
            State::Idle | State::Failed(_) => {}
            State::Building { changed, .. } => changed.extend(resources.iter().cloned()),
            State::Built(texts) => {
                if let Err(error) = texts.read_back(&self.db, resources) {
                    // Taken in anew on the next search.
                    *state = self.failed(error);
                }
            }
        }
    }

    /// Start taking it in, if nothing did: off the frame for an index kept on disk, else here.
    fn start(&self, state: &mut State) {
        if !matches!(state, State::Idle | State::Failed(_)) {
            return;
        }
        // The commit the build reads, had here under the lock: a commit after it reaches
        // `changed`, which waits for the lock, and is read back when the build is taken in.
        let txn = match self.db.begin_read() {
            Ok(txn) => txn,
            Err(error) => {
                *state = self.failed(error.into());
                return;
            }
        };
        if !self.off_frame {
            *state = match Texts::build(&txn) {
                Ok(texts) => State::Built(Box::new(texts)),
                Err(error) => self.failed(error),
            };
            return;
        }
        let gate = Arc::clone(&self.gate);
        let built = Pending::start(
            move || {
                let _let_go = gate.read().unwrap_or_else(PoisonError::into_inner);
                Texts::build(&txn)
            },
            None,
        );
        *state = State::Building {
            built,
            changed: BTreeSet::new(),
        };
    }

    /// Take in a build that ended (waiting for it with `wait`), with what commits changed while
    /// it ran. Whether one was taken in.
    fn take_in(&self, state: &mut State, wait: bool) -> bool {
        let State::Building { built, changed } = state else {
            return false;
        };
        let polled = if wait { built.wait() } else { built.poll() };
        let built = match polled {
            Poll::Running => return false,
            Poll::Ready(built) => built,
            Poll::Lost => Err(IndexError::Io(
                "the search build ended without a result".into(),
            )),
        };
        let changed = std::mem::take(changed);
        *state = match built.and_then(|mut texts| {
            texts.read_back(&self.db, &changed)?;
            Ok(texts)
        }) {
            Ok(texts) => State::Built(Box::new(texts)),
            Err(error) => self.failed(error),
        };
        matches!(state, State::Built(_))
    }

    /// What a frame does: take in a build that ended. With `wait` (a host without frames), wait
    /// for a build under way to end. Whether one was taken in. Nothing here waits on a build
    /// but `wait`.
    pub(crate) fn poll(&self, wait: bool) -> bool {
        let mut state = self.lock();
        let mut taken = self.take_in(&mut state, false);
        if wait {
            taken |= self.take_in(&mut state, true);
        }
        drop(state);
        taken
    }

    /// `read` of the search index as it stands, if it is had: `None` while it is being taken in
    /// off the frame (started now if it was not), as an index being built lists nothing.
    fn read<T>(&self, read: impl FnOnce(&Texts) -> T) -> Result<Option<T>, IndexError> {
        let mut state = self.lock();
        self.take_in(&mut state, false);
        let failed = match &*state {
            State::Failed(error) => Some(error.clone()),
            _ => None,
        };
        self.start(&mut state);
        match &*state {
            State::Built(texts) => Ok(Some(read(texts))),
            State::Failed(error) => Err(error.clone()),
            State::Idle | State::Building { .. } => failed.map_or(Ok(None), Err),
        }
    }

    /// Hold every build of this search index, once it has the commit it reads, until the guard
    /// is dropped: what tests prove with that nothing on the frame path waits for one, and that
    /// the commits landing meanwhile are read back.
    #[doc(hidden)]
    pub fn hold(&self) -> RwLockWriteGuard<'_, ()> {
        self.gate.write().unwrap_or_else(PoisonError::into_inner)
    }

    /// The resources whose texts hold `query` (its words spread over them, the last as typed so
    /// far), best first, then by path: the first `limit` of them. `None` while the search index
    /// is being taken in off the frame: the next search after it is answers.
    ///
    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn find(&self, query: &str, limit: usize) -> Result<Option<Vec<Found>>, IndexError> {
        let found = self.read(|texts| {
            if query.trim().is_empty() {
                return Vec::new();
            }
            let hits = texts.index.search(query, Page::first(limit)).hits;
            hits.into_iter()
                .filter_map(|hit| {
                    Some(Found {
                        resource: texts.paths.get(&hit.key)?.clone(),
                        rank: hit.score,
                    })
                })
                .collect::<Vec<_>>()
        })?;
        Ok(found.map(|mut out| {
            out.sort_by(|a, b| {
                b.rank
                    .cmp(&a.rank)
                    .then_with(|| a.resource.cmp(&b.resource))
            });
            out
        }))
    }

    /// Every resource search finds, by path: those holding a text. `None` while the search
    /// index is being taken in off the frame.
    ///
    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn resources(&self) -> Result<Option<Vec<String>>, IndexError> {
        Ok(self
            .read(|texts| texts.keys.keys().cloned().collect::<Vec<String>>())?
            .map(|mut out| {
                out.sort();
                out
            }))
    }

    /// Every resource's texts and their weights, by path: what tests compare a search index
    /// kept commit by commit with one taken in whole. Waits for a build under way.
    ///
    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn contents(&self) -> Result<BTreeMap<String, Vec<(String, u8)>>, IndexError> {
        let mut state = self.lock();
        self.take_in(&mut state, false);
        self.start(&mut state);
        self.take_in(&mut state, true);
        let State::Built(texts) = &*state else {
            return Err(match &*state {
                State::Failed(error) => error.clone(),
                _ => IndexError::Io("the search index was not taken in".into()),
            });
        };
        let out = texts
            .index
            .documents()
            .filter_map(|document| {
                let path = texts.paths.get(&document.key())?.clone();
                let held = document
                    .fields()
                    .iter()
                    .map(|(text, weight)| (text.clone(), weight.percent()))
                    .collect();
                Some((path, held))
            })
            .collect();
        drop(state);
        Ok(out)
    }
}
