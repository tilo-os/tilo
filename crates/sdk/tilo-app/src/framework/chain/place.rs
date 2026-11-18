//! Where a chain's items are kept: one policy for every app's items.
//!
//! An app declares the items it keeps (the collections of its parts'
//! resource) and the verbs over them; the chain alone decides which part
//! holds each item:
//!
//! - **A new item** goes to the newest part, unless that part holds
//!   [`PART_ITEMS`] items or [`PART_BYTES`] of them: then to the next part,
//!   which the write starts. Two writers starting the same part start the
//!   same resource, so their items meet there.
//! - **A change to an item** goes to the part holding it, found by the item's
//!   id in an index of every item held ([`ItemIndex`]), not by walking the
//!   parts. A verb names its item with its first parameter, a field edit with
//!   its path ([`Resource::action_item`](tilo_resource::Resource::action_item)).
//!   An item named that no part held here keeps is a new item.
//! - **A verb over several items** (`#[action(items = param)]`) is carried
//!   out in each part holding some of them, over those; the new items it
//!   lists fill the newest part and start the ones after it.
//! - **An item kept beside another** ([`super::Chained::beside`]: a reply,
//!   beside the message it answers) goes to that one's part.
//!
//! The index follows the parts as they change: a changed part's items are
//! looked at again, and only an item whose value changed is measured again,
//! so what a change costs here is bounded by one part, whatever the chain
//! holds.

use std::any::Any;
use std::cell::Cell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use tilo_resource::Id;
use tilo_resource::stored_items::stored_size;
use tilo_resource::table::ModelTable;

/// Items a part holds before the next one starts.
pub const PART_ITEMS: usize = 1000;

/// What a part's items take stored, in bytes, before the next part starts:
/// well under what one resource's snapshot may hold, so one change, and one
/// newcomer's first read, stays cheap whatever the items say.
pub const PART_BYTES: usize = 512 * 1024;

thread_local! {
    static LIMITS: Cell<(usize, usize)> = const { Cell::new((PART_ITEMS, PART_BYTES)) };
}

/// The bounds of a part on this thread, other than [`PART_ITEMS`] and
/// [`PART_BYTES`] while it is held: what a test starts parts sooner with,
/// so a few items show what thousands would.
#[must_use = "the bounds hold only while this is held"]
pub struct PartLimits {
    was: (usize, usize),
}

impl PartLimits {
    /// Parts of at most `items` items and `bytes` bytes until this is
    /// dropped.
    pub fn scoped(items: usize, bytes: usize) -> Self {
        let was = LIMITS.with(|limits| limits.replace((items.max(1), bytes.max(1))));
        Self { was }
    }
}

impl Drop for PartLimits {
    fn drop(&mut self) {
        LIMITS.with(|limits| limits.set(self.was));
    }
}

fn limits() -> (usize, usize) {
    LIMITS.with(Cell::get)
}

/// Each item a part's state holds: its id, its value and its type's table.
pub(super) type EachItem = fn(&dyn Any, &mut dyn FnMut(Id, &dyn Any, &'static ModelTable));

/// What the index knows of one part.
struct Indexed {
    /// The state it was read from: a part whose state is still this one is
    /// not looked at again.
    state: Rc<dyn Any>,
    /// Each item's value (by its address, which stays the same while the
    /// item is unchanged: a collection's clones share their items) and its
    /// stored size.
    items: HashMap<Id, (usize, usize)>,
    bytes: usize,
}

/// Every item the parts held here keep, by its id: what finds the part an
/// action changes, and how full the newest part is.
#[derive(Default)]
pub(crate) struct ItemIndex {
    parts: BTreeMap<u64, Indexed>,
    at: HashMap<Id, u64>,
}

impl ItemIndex {
    /// Follow `held`, the parts held, oldest first: look again at the parts
    /// that changed since, forget the ones no longer held.
    pub(super) fn follow(&mut self, held: &[(u64, Rc<dyn Any>)], each: EachItem) {
        let gone: Vec<u64> = self
            .parts
            .keys()
            .filter(|index| {
                held.binary_search_by_key(*index, |(held, _)| *held)
                    .is_err()
            })
            .copied()
            .collect();
        for index in gone {
            if let Some(part) = self.parts.remove(&index) {
                self.forget(index, part.items.keys());
            }
        }
        for (index, state) in held {
            if self
                .parts
                .get(index)
                .is_some_and(|part| Rc::ptr_eq(&part.state, state))
            {
                continue;
            }
            self.read(*index, state, each);
        }
    }

    /// Read part `index` again from `state`.
    fn read(&mut self, index: u64, state: &Rc<dyn Any>, each: EachItem) {
        let before = self.parts.remove(&index);
        let mut items = HashMap::new();
        let mut bytes = 0usize;
        each(&**state, &mut |id, value, table| {
            let address = std::ptr::from_ref(value).cast::<()>() as usize;
            let size = before
                .as_ref()
                .and_then(|part| part.items.get(&id))
                .filter(|(was, _)| *was == address)
                .map_or_else(|| stored_size(table, value), |(_, size)| *size);
            bytes = bytes.saturating_add(size);
            items.insert(id, (address, size));
        });
        if let Some(before) = &before {
            let removed: Vec<Id> = before
                .items
                .keys()
                .filter(|id| !items.contains_key(*id))
                .cloned()
                .collect();
            self.forget(index, removed.iter());
        }
        for id in items.keys() {
            match self.at.get_mut(id) {
                Some(at) if *at > index => {}
                Some(at) => *at = index,
                None => {
                    self.at.insert(id.clone(), index);
                }
            }
        }
        self.parts.insert(
            index,
            Indexed {
                state: Rc::clone(state),
                items,
                bytes,
            },
        );
    }

    /// Part `index` no longer keeps `ids`: each is found in the newest other
    /// part keeping it, if any.
    fn forget<'a>(&mut self, index: u64, ids: impl Iterator<Item = &'a Id>) {
        for id in ids {
            if self.at.get(id) != Some(&index) {
                continue;
            }
            let other = self
                .parts
                .iter()
                .rev()
                .find(|(other, part)| **other != index && part.items.contains_key(id))
                .map(|(other, _)| *other);
            match other {
                Some(other) => {
                    self.at.insert(id.clone(), other);
                }
                None => {
                    self.at.remove(id);
                }
            }
        }
    }

    /// The part holding `id`, among those held here.
    #[must_use]
    pub(crate) fn holding(&self, id: &Id) -> Option<u64> {
        self.at.get(id).copied()
    }

    /// How many items the parts held here keep.
    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.at.len()
    }

    /// The newest part held, and how many items it holds and their bytes.
    fn head(&self) -> Option<(u64, usize, usize)> {
        self.parts
            .iter()
            .next_back()
            .map(|(index, part)| (*index, part.items.len(), part.bytes))
    }

    /// The part a new item goes to: the newest, or the next once it is full.
    #[must_use]
    pub(crate) fn for_new(&self) -> u64 {
        let (items, bytes) = limits();
        match self.head() {
            None => 0,
            Some((index, count, size)) if count >= items || size >= bytes => {
                index.saturating_add(1)
            }
            Some((index, _, _)) => index,
        }
    }

    /// The part `item` (named by an action, or the one a new item is kept
    /// beside) is kept in; a new item's when no part held here keeps it.
    #[must_use]
    pub(crate) fn for_item(&self, item: Option<&Id>) -> u64 {
        item.and_then(|item| self.holding(item))
            .unwrap_or_else(|| self.for_new())
    }

    /// Which part carries out which entries of a verb listing `entries`:
    /// each named item in the part holding it, the new ones filling the
    /// newest part and the ones after it. Parts in order, each with the
    /// positions of its entries.
    #[must_use]
    pub(crate) fn shares(&self, entries: &[Option<Id>]) -> Vec<(u64, Vec<usize>)> {
        let (limit, _) = limits();
        let mut shares: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
        let mut new = Vec::new();
        for (at, entry) in entries.iter().enumerate() {
            match entry.as_ref().and_then(|id| self.holding(id)) {
                Some(index) => shares.entry(index).or_default().push(at),
                None => new.push(at),
            }
        }
        if !new.is_empty() {
            let mut index = self.for_new();
            let mut free = match self.head() {
                Some((head, count, _)) if head == index => limit.saturating_sub(count),
                _ => limit,
            };
            for at in new {
                if free == 0 {
                    index = index.saturating_add(1);
                    free = limit;
                }
                shares.entry(index).or_default().push(at);
                free = free.saturating_sub(1);
            }
        }
        shares.into_iter().collect()
    }
}

impl super::ChainState {
    /// The index of the items `parts` keep, followed to them.
    pub(super) fn indexed(&self, parts: &[(u64, Rc<dyn Any>)]) -> std::cell::Ref<'_, ItemIndex> {
        self.index.borrow_mut().follow(parts, self.part().each_item);
        self.index.borrow()
    }

    /// The part `action` changes: where the app places it
    /// ([`Chained::part_for`]), else the part holding the item it names, or
    /// a new item's.
    pub(super) fn place(
        &self,
        action: &dyn Any,
        placed: Option<Result<u64, String>>,
        parts: &[(u64, Rc<dyn Any>)],
    ) -> Result<u64, String> {
        placed.unwrap_or_else(|| {
            let item = (self.part().item)(action);
            Ok(self.indexed(parts).for_item(item.as_ref()))
        })
    }

    /// The shares of `action` each part carries out, when it is a verb over
    /// several items: the app's ([`Chained::split`]), else one per part
    /// holding some of the items it lists ([`place::ItemIndex::shares`]).
    /// `None` for an action one part carries out.
    #[allow(clippy::type_complexity, reason = "a part's number beside its share")]
    pub(super) fn shares(
        &self,
        action: &dyn Any,
        typed: &dyn Any,
        parts: &[(u64, Rc<dyn Any>)],
    ) -> Result<Option<Vec<(u64, Box<dyn Any>)>>, String> {
        let part = self.part();
        if let Some(shares) = (part.split)(action, typed)? {
            return Ok(Some(shares));
        }
        let Some(entries) = (part.listed_items)(action) else {
            return Ok(None);
        };
        let shares = self.indexed(parts).shares(&entries);
        if shares.len() <= 1 {
            return Ok(None);
        }
        Ok(Some(
            shares
                .into_iter()
                .filter_map(|(index, keep)| Some((index, (part.keep_listed)(action, &keep)?)))
                .collect(),
        ))
    }

    /// The part a verb over several items carries out alone, when its items
    /// are all in one: that part, or a new item's.
    pub(super) fn listed_part(
        &self,
        action: &dyn Any,
        parts: &[(u64, Rc<dyn Any>)],
    ) -> Option<u64> {
        let entries = (self.part().listed_items)(action)?;
        let index = self.indexed(parts);
        Some(
            index
                .shares(&entries)
                .first()
                .map_or_else(|| index.for_new(), |(at, _)| *at),
        )
    }
}

impl<P: super::Chained> super::Chain<P> {
    /// The part keeping the item `id`, among the parts held here, found
    /// in the chain's index of its items rather than by reading the parts:
    /// what reads one item out of thousands reads it from.
    #[must_use]
    pub fn find(&self, id: &tilo_resource::Id) -> Option<Rc<P::State>> {
        let held = self.state.parts();
        let index = self.state.indexed(&held).holding(id)?;
        let at = held.binary_search_by_key(&index, |(at, _)| *at).ok()?;
        let (_, state) = held.into_iter().nth(at)?;
        state.downcast().ok()
    }

    /// What the read the chain publishes answers `args` with (`cursor`,
    /// `pageSize`, `fields`, `where`, `item`), as a program or an agent
    /// calling it is answered: a page of the items held, newest first, and
    /// where the next page starts.
    ///
    /// # Errors
    /// Refuses arguments the read does not take.
    pub fn read(&self, args: &serde_json::Value) -> Result<serde_json::Value, String> {
        let payload = serde_json::to_vec(args).map_err(|error| error.to_string())?;
        self.state.read(&payload)
    }

    /// How many items the parts held here keep.
    #[must_use]
    pub fn len(&self) -> usize {
        let held = self.state.parts();
        self.state.indexed(&held).len()
    }

    /// Whether the parts held here keep no item.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The part the engine writes `action` into, given `parts` (oldest first),
/// as a window holding them would: what a test applying actions to its own
/// copies of the parts, or a program replaying a chain without a window, asks
/// where each one goes.
#[must_use]
pub fn placed<P: super::Chained>(action: &P::Action, parts: &[(u64, Rc<P::State>)]) -> u64 {
    let held: Vec<(u64, Rc<dyn Any>)> = parts
        .iter()
        .map(|(index, state)| (*index, Rc::clone(state) as Rc<dyn Any>))
        .collect();
    let mut index = ItemIndex::default();
    index.follow(&held, super::part::each_item::<P>);
    let item = P::beside(action).or_else(|| P::action_item(action));
    if let Some(entries) = P::listed_items(action) {
        return index
            .shares(&entries)
            .first()
            .map_or_else(|| index.for_new(), |(at, _)| *at);
    }
    index.for_item(item.as_ref())
}
