//! The steps a write took in the listings it changed: where each item that joined a collection
//! stands now, and where each one that left it stood. A view showing the collection takes them
//! as they are, a row in and a row out, rather than reading the whole list again.
//!
//! The store says where resources stand in a listing from its index ([`Command::Positions`]):
//! one read for every item that moved, however many moved and however long the collection is.
//! What left is read before the write reaches the store and what came after it.

use collab_types::store::wire::{Command, Reply};
use collab_types::store::{Filter, PARENT, Query, Value};
use std::collections::{BTreeMap, BTreeSet};

use tilolang_runtime::ids::{Actor, DocId};
use tilolang_runtime::store::{ChangeSummary, FieldKey, ListingChange, Splice};

use super::{ResourceStore, item_of};

/// One collection a write changed the items of, between reading where what left stood and
/// reading where what came stands.
pub(super) struct Stepping {
    doc: DocId,
    key: FieldKey,
    query: Query,
    /// What left, where it stood, as the store held it before the write.
    left: Vec<(usize, String)>,
    /// What came, by id.
    came: Vec<String>,
}

/// What a run's commits changed, held until the run ends: the views hear of a run's writes once
/// the store holds them, as one change of each document.
pub(super) struct Heard {
    /// How many runs were open when it was committed: what a run that fails takes back.
    pub depth: usize,
    /// How many trial runs were open: what a trial run dropped takes back.
    pub trial: usize,
    pub touched: BTreeMap<DocId, BTreeSet<FieldKey>>,
    pub collections: BTreeMap<DocId, BTreeSet<FieldKey>>,
    /// The resources written, in the order first touched.
    pub order: Vec<String>,
    pub by: Option<Actor>,
}

/// What the changes heard of one document told: the fields they wrote, the collections among
/// them, and who wrote the last.
type Told = (BTreeSet<FieldKey>, BTreeSet<FieldKey>, Option<Actor>);

impl ResourceStore {
    /// What writing what [`Inner::local`] holds of `order` does to each of `collections` whose
    /// items it changed, as far as the store can say before the write reaches it. The caller
    /// knows the store holds everything else as it was: what is written here and unsent is
    /// this write and nothing more.
    pub(super) fn steps_before(
        &self,
        collections: &BTreeMap<DocId, BTreeSet<FieldKey>>,
        order: &[String],
    ) -> Vec<Stepping> {
        let mut out = Vec::new();
        for (doc, keys) in collections {
            for key in keys {
                if let Some(stepping) = self.stepping(order, doc, key) {
                    out.push(stepping);
                }
            }
        }
        out
    }

    /// Tell the views what `heard` changed, now that the store holds it, with the steps
    /// `stepping` took: one change of each document, every field any of them named.
    pub(super) fn tell(&self, heard: Vec<Heard>, stepping: Vec<Stepping>) {
        let mut listings: BTreeMap<DocId, Vec<ListingChange>> = BTreeMap::new();
        for (doc, change) in self.steps_after(stepping) {
            listings.entry(doc).or_default().push(change);
        }
        let mut docs: BTreeMap<DocId, Told> = BTreeMap::new();
        for heard in heard {
            for (doc, fields) in heard.touched {
                let held = docs.entry(doc.clone()).or_default();
                held.0.extend(fields);
                if let Some(collections) = heard.collections.get(&doc) {
                    held.1.extend(collections.iter().cloned());
                }
                held.2.clone_from(&heard.by);
            }
        }
        for (doc, (fields, collections, by)) in docs {
            let listings = listings.remove(&doc).unwrap_or_default();
            self.notify(&ChangeSummary {
                doc,
                fields: fields.into_iter().collect(),
                collections: collections.into_iter().collect(),
                listings,
                by,
            });
        }
    }

    /// [`Self::steps_before`] of the collection `key`: `None` where its items are of more than
    /// one kind, as a union's are, so no one listing holds them.
    fn stepping(&self, order: &[String], doc: &DocId, key: &FieldKey) -> Option<Stepping> {
        let ty = self.type_of_id(&key.resource)?;
        let entry = self.field(&ty, &key.field)?;
        let items = entry.items;
        let [item] = items.as_slice() else {
            return None;
        };
        let kinds = self.kinds_of_type(item).ok()?;
        let [kind] = kinds.as_slice() else {
            return None;
        };
        let query = Query::of(kind.clone(), 1).filter(Filter::Is(
            PARENT.to_owned(),
            Value::Text(key.resource.clone()),
        ));
        let local = self.inner.local.borrow();
        let mut left = Vec::new();
        let mut came = Vec::new();
        let stood_at = self.positions(&query, order)?;
        for (id, stood) in order.iter().zip(stood_at) {
            let now = local
                .get(id)
                .and_then(Option::as_ref)
                .filter(|held| held.kind == *kind && held.parent.as_deref() == Some(&key.resource));
            let was = match stood {
                Some(at) => {
                    let Reply::Resource(Some(indexed)) = self
                        .command(Command::Get {
                            kind: kind.clone(),
                            id: id.clone(),
                        })
                        .ok()?
                    else {
                        return None;
                    };
                    Some((at, indexed.order))
                }
                None => None,
            };
            match (was, now) {
                (None, None) => {}
                (Some((_, order)), Some(held)) if order == held.order => {}
                (was, now) => {
                    if let Some((at, _)) = was {
                        left.push((at, id.clone()));
                    }
                    if now.is_some() {
                        came.push(id.clone());
                    }
                }
            }
        }
        Some(Stepping {
            doc: doc.clone(),
            key: key.clone(),
            query,
            left,
            came,
        })
    }

    /// The steps of `stepping`, now that the write is in the store: what left taken out from
    /// the last, then what came put in from the first, each where the listing has it. A
    /// collection a position of which the store cannot say takes none, and its readers read it
    /// whole.
    fn steps_after(&self, stepping: Vec<Stepping>) -> Vec<(DocId, ListingChange)> {
        let mut out = Vec::new();
        'collections: for Stepping {
            doc,
            key,
            query,
            mut left,
            came,
        } in stepping
        {
            left.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
            let mut splices: Vec<Splice> = left
                .into_iter()
                .map(|(at, id)| Splice::Remove {
                    at,
                    item: item_of(&id),
                })
                .collect();
            let Some(stand) = self.positions(&query, &came) else {
                continue 'collections;
            };
            let mut placed = Vec::with_capacity(came.len());
            for (id, at) in came.into_iter().zip(stand) {
                let Some(at) = at else {
                    continue 'collections;
                };
                placed.push((at, id));
            }
            placed.sort();
            splices.extend(placed.into_iter().map(|(at, id)| Splice::Insert {
                at,
                item: item_of(&id),
            }));
            out.push((doc, ListingChange { key, splices }));
        }
        out
    }

    /// Where each of `ids` stands in the listing of `query`, in their order, `None` for one it
    /// holds none of: one read for all of them. `None` where the store could not be asked.
    fn positions(&self, query: &Query, ids: &[String]) -> Option<Vec<Option<usize>>> {
        if ids.is_empty() {
            return Some(Vec::new());
        }
        match self.command(Command::Positions {
            query: query.clone(),
            ids: ids.to_vec(),
        }) {
            Ok(Reply::Positions(at)) if at.len() == ids.len() => Some(at),
            _ => None,
        }
    }
}
