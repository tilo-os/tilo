//! What search finds a package's resources by: the texts of every record and item, kept in a
//! [`SearchIndex`] of Tilo's search layer, which every surface that finds things by typed text
//! searches through. Text is folded as everywhere in Tilo (case, diacritics, compatibility
//! forms), found by substring and prefix as it is typed, words spread over a resource's texts,
//! and hits rank by [`MatchTier`](tilo_search::MatchTier) × [`FieldWeight`]: a resource's
//! naming text (its first text field, in id order) is its title, its other texts weigh as body
//! text.
//!
//! The search index is derived from the index's fields, as the lists are from the record: it is
//! taken in from the index's last commit when something first searches (opening a window costs
//! what it shows, not the room's texts), and from then on follows every commit of the index,
//! resource by resource. Each commit reads the resources whose texts it changed back from the
//! commit itself, so the search index holds what the index holds, at the heads it is stamped
//! with.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, PoisonError};

use redb::{Database, ReadableDatabase as _, ReadableTable as _};
use tilo_search::{Document, FieldWeight, Matching, Page, SearchIndex};
use tilolang_runtime::store::FieldValue;

use super::{FIELDS, IndexError, IndexStats, TYPE, codec};
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
    /// Taken in from the index: until then commits leave it be.
    built: bool,
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
    texts: Mutex<Texts>,
}

impl std::fmt::Debug for Search {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Search").finish_non_exhaustive()
    }
}

impl Search {
    pub(super) fn new(db: Arc<Database>, stats: Arc<IndexStats>) -> Self {
        Self {
            db,
            stats,
            texts: Mutex::new(Texts::empty()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Texts> {
        self.texts.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Forget what it took in: the index was emptied for a build.
    pub(super) fn clear(&self) {
        *self.lock() = Texts::empty();
    }

    /// The texts of `resources` changed in a commit of the index that is in: read them back
    /// from the index as it stands now, if search took it in already.
    pub(super) fn changed(&self, resources: &BTreeSet<String>) {
        if resources.is_empty() {
            return;
        }
        let mut texts = self.lock();
        if !texts.built {
            return;
        }
        if let Err(error) = self.read_back(&mut texts, resources) {
            // Taken in anew on the next search.
            *texts = Texts::empty();
            if error == IndexError::Corrupt {
                self.stats.corrupt();
            }
            return;
        }
        texts.commit();
    }

    fn read_back(&self, texts: &mut Texts, resources: &BTreeSet<String>) -> Result<(), IndexError> {
        let txn = self.db.begin_read()?;
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
            texts.put(path, held);
        }
        Ok(())
    }

    /// Take in every resource's texts from the index's last commit.
    fn build(&self, texts: &mut Texts) -> Result<(), IndexError> {
        let txn = self.db.begin_read()?;
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
        *texts = Texts::empty();
        for (path, held) in documents {
            texts.put(&path, held);
        }
        texts.commit();
        texts.built = true;
        Ok(())
    }

    fn ready(&self) -> Result<std::sync::MutexGuard<'_, Texts>, IndexError> {
        let mut texts = self.lock();
        if !texts.built
            && let Err(error) = self.build(&mut texts)
        {
            *texts = Texts::empty();
            if error == IndexError::Corrupt {
                self.stats.corrupt();
            }
            return Err(error);
        }
        Ok(texts)
    }

    /// The resources whose texts hold `query` (its words spread over them, the last as typed so
    /// far), best first, then by path: the first `limit` of them.
    ///
    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn find(&self, query: &str, limit: usize) -> Result<Vec<Found>, IndexError> {
        let texts = self.ready()?;
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let hits = texts.index.search(query, Page::first(limit)).hits;
        let mut out: Vec<Found> = hits
            .into_iter()
            .filter_map(|hit| {
                Some(Found {
                    resource: texts.paths.get(&hit.key)?.clone(),
                    rank: hit.score,
                })
            })
            .collect();
        drop(texts);
        out.sort_by(|a, b| {
            b.rank
                .cmp(&a.rank)
                .then_with(|| a.resource.cmp(&b.resource))
        });
        Ok(out)
    }

    /// Every resource search finds, by path: those holding a text.
    ///
    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn resources(&self) -> Result<Vec<String>, IndexError> {
        let texts = self.ready()?;
        let mut out: Vec<String> = texts.keys.keys().cloned().collect();
        drop(texts);
        out.sort();
        Ok(out)
    }

    /// Every resource's texts and their weights, by path: what tests compare a search index
    /// kept commit by commit with one taken in whole.
    ///
    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn contents(&self) -> Result<BTreeMap<String, Vec<(String, u8)>>, IndexError> {
        let texts = self.ready()?;
        Ok(texts
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
            .collect())
    }
}
