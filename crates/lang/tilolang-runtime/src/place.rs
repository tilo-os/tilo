//! Where a document's items are.
//!
//! A document stores its items flat, each under its id. Which collection holds an item, and
//! where in it, is a field of the item, its **placement**: the item holding the collection (or
//! the document itself), the collection's field, and an order key. A collection is derived from
//! the placements: the items placed in it, by order key, ties by id. So an item keeps its id,
//! its history and every reference to it when it moves to another collection, and a field edited
//! while someone else moves the item stays edited: the move writes the placement, the edit the
//! field.
//!
//! How placements merge (convergence.md, "Storage and sync"):
//!
//! * The placement is one register: of two concurrent moves of an item, the last in the
//!   record's order wins, parent, field and place together.
//! * Removing an item writes its placement as removed (last writer wins against a concurrent
//!   move of the same item). Its items go with it: an item is there only while every item
//!   above it is. An item moved out before the removal, or moved out concurrently with it, is
//!   no longer below it and stays. An item moved or inserted into it concurrently goes with it,
//!   as an edit inside it does. Nothing is deleted from the record: undoing the removal brings
//!   the item back with everything in it.
//! * Concurrent moves can place two items inside each other. In such a cycle the move that is
//!   last in the record's order does not take effect: that item stays where it was before it
//!   (the placement records where it came from). Should that place be in a cycle too, the item
//!   is in no collection until someone moves it.

use std::collections::{BTreeMap, HashMap};

use serde_json::{Value as Json, json};

use crate::ids::{FieldId, ItemId};

/// The key, in a document's record, of the map holding its items by id.
pub const ITEMS_KEY: &str = "$items";

/// The key, in an item's map, of its placement.
pub const AT_KEY: &str = "$at";

/// Where an item is: the item holding its collection (`None`: the document itself), the
/// collection's field, and its order key there.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Place {
    pub parent: Option<ItemId>,
    pub field: FieldId,
    pub key: String,
}

impl Place {
    /// The collection it names: its holder and field.
    #[must_use]
    pub fn collection(&self) -> (Option<&str>, &str) {
        (self.parent.as_deref(), self.field.as_str())
    }

    fn to_json(&self) -> Json {
        let mut out = json!({ "f": self.field, "k": self.key });
        if let (Some(parent), Json::Object(map)) = (&self.parent, &mut out) {
            map.insert("in".into(), Json::String(parent.clone()));
        }
        out
    }

    fn of_json(json: &Json) -> Option<Self> {
        Some(Self {
            parent: match json.get("in") {
                Some(Json::String(parent)) => Some(parent.clone()),
                None | Some(Json::Null) => None,
                Some(_) => return None,
            },
            field: json.get("f")?.as_str()?.to_owned(),
            key: json.get("k")?.as_str()?.to_owned(),
        })
    }
}

/// An item's placement as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Placement {
    /// In a collection; `was` is where the move that put it there took it from.
    At { place: Place, was: Option<Place> },
    /// Removed, with everything in it.
    Removed,
}

impl Placement {
    /// The placement as the item's `$at` register holds it.
    #[must_use]
    pub fn encode(&self) -> String {
        match self {
            Self::At { place, was } => {
                let mut out = place.to_json();
                if let (Some(was), Json::Object(map)) = (was, &mut out) {
                    map.insert("was".into(), was.to_json());
                }
                out.to_string()
            }
            Self::Removed => json!({ "removed": true }).to_string(),
        }
    }

    /// The placement an `$at` register holds; `None` for text that is not one.
    #[must_use]
    pub fn decode(text: &str) -> Option<Self> {
        let json: Json = serde_json::from_str(text).ok()?;
        if json.get("removed").and_then(Json::as_bool) == Some(true) {
            return Some(Self::Removed);
        }
        Some(Self::At {
            place: Place::of_json(&json)?,
            was: json.get("was").and_then(Place::of_json),
        })
    }

    /// Where it is, unless removed.
    #[must_use]
    pub fn place(&self) -> Option<&Place> {
        match self {
            Self::At { place, .. } => Some(place),
            Self::Removed => None,
        }
    }
}

/// When a placement was written, in the record's order: the Lamport counter and actor of the
/// write. A placement nobody wrote (an item of a record written before items were flat, where
/// it is) comes before every write.
pub type Written = (u64, Vec<u8>);

/// One item as its document holds it: its placement and when that was written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Held {
    pub placement: Placement,
    pub written: Written,
}

/// A collection of a document: the item holding it (`None`: the document) and its field.
pub type Collection = (Option<ItemId>, FieldId);

/// The items of a document that are in a collection, resolved from their placements.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Resolved {
    /// Each item that is in a collection, and where.
    pub places: HashMap<ItemId, Place>,
    /// Each collection's items, by order key, ties by id.
    pub children: BTreeMap<Collection, Vec<(String, ItemId)>>,
    /// The items whose last move did not take effect, as it closed a cycle.
    pub held_back: Vec<ItemId>,
}

impl Resolved {
    /// The items of the collection `field` of `parent` (the document for `None`), in order.
    #[must_use]
    pub fn items(&self, parent: Option<&str>, field: &str) -> Vec<ItemId> {
        self.children
            .get(&(parent.map(str::to_owned), field.to_owned()))
            .map(|items| items.iter().map(|(_, id)| id.clone()).collect())
            .unwrap_or_default()
    }

    /// Whether `item` is in a collection, `ancestor` (an item) above it.
    #[must_use]
    pub fn within(&self, item: &str, ancestor: &str) -> bool {
        let mut at = item;
        for _ in 0..=self.places.len() {
            if at == ancestor {
                return true;
            }
            match self
                .places
                .get(at)
                .and_then(|place| place.parent.as_deref())
            {
                Some(parent) => at = parent,
                None => return false,
            }
        }
        false
    }
}

/// Resolve `items` into the collections they are in. `fits(parent, field, item)` says whether
/// the collection `field` of `parent` (an item, or the document for `None`) may hold `item`: an
/// item placed where it does not fit is in no collection.
pub fn resolve(
    items: &BTreeMap<ItemId, Held>,
    fits: &dyn Fn(Option<&str>, &str, &str) -> bool,
) -> Resolved {
    // 0: its own placement, 1: where it was before, 2: nowhere.
    let mut stage: HashMap<&str, u8> = HashMap::new();
    let current = |id: &str, stage: &HashMap<&str, u8>| -> Option<Place> {
        let held = items.get(id)?;
        let Placement::At { place, was } = &held.placement else {
            return None;
        };
        match stage.get(id).copied().unwrap_or(0) {
            0 => Some(place.clone()),
            1 => was.clone(),
            _ => None,
        }
    };
    let mut held_back = Vec::new();
    // Each round holds back the last move of every cycle; a held-back item is never held back
    // twice past nowhere, so this ends within twice as many rounds as items.
    for _ in 0..=items.len().saturating_mul(2) {
        let parent_of: HashMap<&str, Option<&str>> = items
            .keys()
            .map(|id| {
                let parent = current(id, &stage).and_then(|place| place.parent);
                let parent = parent.and_then(|parent| {
                    items
                        .get_key_value(parent.as_str())
                        .map(|(k, _)| k.as_str())
                });
                (id.as_str(), parent)
            })
            .collect();
        let cycles = cycles(&parent_of);
        if cycles.is_empty() {
            break;
        }
        for cycle in cycles {
            let Some(last) = cycle
                .iter()
                .max_by(|a, b| {
                    let written = |id: &&str| items.get(*id).map(|held| &held.written);
                    written(a).cmp(&written(b)).then_with(|| a.cmp(b))
                })
                .copied()
            else {
                continue;
            };
            let next = stage.get(last).copied().unwrap_or(0).saturating_add(1);
            stage.insert(last, next);
            if !held_back.iter().any(|held: &ItemId| held == last) {
                held_back.push(last.to_owned());
            }
        }
    }
    let placed: HashMap<&str, Place> = items
        .keys()
        .filter_map(|id| Some((id.as_str(), current(id, &stage)?)))
        .collect();
    let mut out = Resolved {
        held_back,
        ..Resolved::default()
    };
    // An item is in a collection while it fits there and every item above it is in one.
    let mut known: HashMap<&str, bool> = HashMap::new();
    for id in items.keys() {
        let mut chain: Vec<&str> = Vec::new();
        let mut at = id.as_str();
        let reached = loop {
            if let Some(known) = known.get(at) {
                break *known;
            }
            let Some(place) = placed.get(at) else {
                break false;
            };
            if !fits(place.parent.as_deref(), &place.field, at) || chain.contains(&at) {
                break false;
            }
            chain.push(at);
            match place.parent.as_deref() {
                None => break true,
                Some(parent) if items.contains_key(parent) => at = parent,
                Some(_) => break false,
            }
        };
        for link in chain {
            known.insert(link, reached);
        }
        known.entry(id.as_str()).or_insert(reached);
    }
    for (id, place) in &placed {
        if known.get(*id).copied() != Some(true) {
            continue;
        }
        out.children
            .entry((place.parent.clone(), place.field.clone()))
            .or_default()
            .push((place.key.clone(), (*id).to_owned()));
        out.places.insert((*id).to_owned(), place.clone());
    }
    for items in out.children.values_mut() {
        items.sort();
    }
    out
}

/// The cycles of the graph where each item points at its parent.
fn cycles<'a>(parent_of: &HashMap<&'a str, Option<&'a str>>) -> Vec<Vec<&'a str>> {
    let mut done: HashMap<&str, bool> = HashMap::new();
    let mut out = Vec::new();
    let mut ids: Vec<&&str> = parent_of.keys().collect();
    ids.sort();
    for start in ids {
        let mut path: Vec<&str> = Vec::new();
        let mut at = Some(*start);
        while let Some(id) = at {
            if done.contains_key(id) {
                break;
            }
            if let Some(from) = path.iter().position(|seen| *seen == id) {
                out.push(path.get(from..).unwrap_or_default().to_vec());
                break;
            }
            path.push(id);
            at = parent_of.get(id).copied().flatten();
        }
        for id in path {
            done.insert(id, true);
        }
    }
    out
}

/// When the op `id` was written, in the record's order.
#[must_use]
pub fn written_of(id: &automerge::ObjId) -> Written {
    match id {
        automerge::ObjId::Id(counter, actor, _) => (*counter, actor.to_bytes().to_vec()),
        automerge::ObjId::Root => (0, Vec::new()),
    }
}

/// The placement the item map `item` holds, and when it was written: `None` when it holds
/// none, or one that does not read.
pub fn held_at(doc: &impl automerge::ReadDoc, item: &automerge::ObjId) -> Option<Held> {
    let (automerge::Value::Scalar(scalar), id) = doc.get(item, AT_KEY).ok()?? else {
        return None;
    };
    let automerge::ScalarValue::Str(text) = scalar.as_ref() else {
        return None;
    };
    Some(Held {
        placement: Placement::decode(text)?,
        written: written_of(&id),
    })
}

/// An order key between `low` and `high` (the open ends for `None`).
#[must_use]
pub fn key_between(low: Option<&str>, high: Option<&str>) -> String {
    tilo_resource::order::between(low, high)
}

/// The order key for the place `index` among `others` (a collection's items in order, with
/// their keys, the one being placed left out): before the item at `index`, else at the end.
#[must_use]
pub fn key_at(others: &[(String, ItemId)], index: usize) -> String {
    let low = index
        .checked_sub(1)
        .and_then(|before| others.get(before))
        .map(|(key, _)| key.as_str());
    let high = others.get(index).map(|(key, _)| key.as_str());
    let low = if high.is_none() {
        others.last().map(|(key, _)| key.as_str())
    } else {
        low
    };
    key_between(low, high)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(parent: Option<&str>, field: &str, key: &str) -> Place {
        Place {
            parent: parent.map(str::to_owned),
            field: field.to_owned(),
            key: key.to_owned(),
        }
    }

    fn held(place: Place, was: Option<Place>, counter: u64) -> Held {
        Held {
            placement: Placement::At { place, was },
            written: (counter, Vec::new()),
        }
    }

    #[test]
    fn a_placement_reads_back() {
        let moved = Placement::At {
            place: at(Some("c1"), "cards", "m"),
            was: Some(at(None, "cards", "a")),
        };
        assert_eq!(Placement::decode(&moved.encode()), Some(moved));
        assert_eq!(
            Placement::decode(&Placement::Removed.encode()),
            Some(Placement::Removed)
        );
        assert_eq!(Placement::decode("not json"), None);
    }

    #[test]
    fn items_of_a_removed_item_go_with_it() {
        let items = BTreeMap::from([
            (
                "col".to_owned(),
                Held {
                    placement: Placement::Removed,
                    written: (5, Vec::new()),
                },
            ),
            (
                "card".to_owned(),
                held(at(Some("col"), "cards", "a"), None, 1),
            ),
            ("out".to_owned(), held(at(None, "cards", "a"), None, 1)),
        ]);
        let resolved = resolve(&items, &|_, _, _| true);
        assert_eq!(resolved.items(None, "cards"), ["out"]);
        assert!(!resolved.places.contains_key("card"));
    }

    #[test]
    fn the_last_move_of_a_cycle_stays_where_it_was() {
        // a moved into b, and b into a, at the same time; b's move is the later one.
        let items = BTreeMap::from([
            (
                "a".to_owned(),
                held(at(Some("b"), "kids", "m"), Some(at(None, "kids", "a")), 3),
            ),
            (
                "b".to_owned(),
                held(at(Some("a"), "kids", "m"), Some(at(None, "kids", "b")), 4),
            ),
        ]);
        let resolved = resolve(&items, &|_, _, _| true);
        assert_eq!(resolved.items(None, "kids"), ["b"]);
        assert_eq!(resolved.items(Some("b"), "kids"), ["a"]);
        assert_eq!(resolved.held_back, ["b"]);
    }

    #[test]
    fn keys_place_before_the_item_named_else_at_the_end() {
        let others = vec![
            ("b".to_owned(), "x".to_owned()),
            ("d".to_owned(), "y".to_owned()),
        ];
        let first = key_at(&others, 0);
        assert!(first.as_str() < "b");
        let middle = key_at(&others, 1);
        assert!("b" < middle.as_str() && middle.as_str() < "d");
        assert!(key_at(&others, 2).as_str() > "d");
        assert!(key_at(&others, 9).as_str() > "d");
        assert!(!key_at(&[], 0).is_empty());
    }
}
