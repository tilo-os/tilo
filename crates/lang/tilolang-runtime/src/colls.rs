//! Collections read lazily in views, and what a view depends on of one.
//!
//! A view that reads a collection field gets a [`Value::Coll`]: the field, not a list of its
//! items. Its length, an item at an index, its first and its last item are read through the
//! store's listing ([`crate::store::Store::listing`]), shared, one at a time, and each read is
//! recorded as what it saw ([`CollSeen`]). A change to the collection re-runs a view only if
//! something it saw is no longer so: `messages.last()` depends on the last message, not on the
//! 10,000 before it, and builds one reference, not 10,000.
//!
//! Wherever the items are needed as values (sorted, compared, stored, put into a list or a
//! tuple) the collection is read whole into a [`Value::List`], and the view then depends on
//! the whole collection, as before.

use std::rc::Rc;

use crate::core::{Core, HResult, trap};
use crate::handles::FrameKind;
use crate::meta::{FieldKind, Ty};
use crate::store::{FieldRead, FieldSpec, Listing, Lit, Store, StoreError};
use crate::value::{CollRef, Value};

/// What a view saw of one collection: its length, its first and last item, and the item at
/// each index it read (an item by its id). It still holds after a change when each is still
/// what it was.
#[derive(Clone, Debug)]
pub(crate) struct CollSeen {
    pub coll: Rc<CollRef>,
    /// The collection's resource, as [`crate::value::ResourceRef::id_string`] writes it.
    pub resource: String,
    pub len: Option<usize>,
    pub first: End,
    pub last: End,
    pub at: Vec<(usize, Rc<str>)>,
    /// How many items were read one after the other from the first: a loop over the
    /// collection. Once it read them all, the view depends on the whole collection (a read of
    /// its field) and what it reads of it item by item is no longer kept.
    sweep: usize,
    whole: bool,
}

impl CollSeen {
    fn new(coll: &Rc<CollRef>) -> Self {
        Self {
            coll: Rc::clone(coll),
            resource: coll.target.id_string(),
            len: None,
            first: End::Unread,
            last: End::Unread,
            at: Vec::new(),
            sweep: 0,
            whole: false,
        }
    }

    /// What a preview of `coll` sees: its length and its first `shown` items.
    pub fn shown(coll: &Rc<CollRef>, listing: &Listing, shown: usize) -> Self {
        let mut seen = Self::new(coll);
        seen.len = Some(listing.len());
        seen.at = listing.iter().take(shown).enumerate().collect();
        seen
    }

    /// Whether this is what was seen of `coll`.
    fn is_of(&self, coll: &CollRef) -> bool {
        self.coll.field == coll.field
            && self.coll.target == coll.target
            && self.coll.filter == coll.filter
    }

    /// Whether everything seen is still so in `listing`.
    pub fn holds(&self, listing: &Listing) -> bool {
        // Read whole, it depends on the collection's field instead.
        self.whole
            || self.len.is_none_or(|len| len == listing.len())
                && self.first.holds(listing.first().as_ref())
                && self.last.holds(listing.last().as_ref())
                && self
                    .at
                    .iter()
                    .all(|(i, id)| listing.get(*i).as_ref() == Some(id))
    }

    /// Whether everything seen is still so in the store.
    pub fn holds_now(&self, core: &Core) -> bool {
        listing_of(&**core.store.borrow(), &self.coll).is_ok_and(|listing| self.holds(&listing))
    }

    /// Take on what `other`, of the same collection, saw.
    fn absorb(&mut self, other: Self) {
        if self.whole || other.whole {
            self.whole = true;
            self.at = Vec::new();
            return;
        }
        self.len = self.len.or(other.len);
        if matches!(self.first, End::Unread) {
            self.first = other.first;
        }
        if matches!(self.last, End::Unread) {
            self.last = other.last;
        }
        self.at.extend(other.at);
    }
}

/// What a view saw at one end of a collection: nothing, or the item there (none when empty).
#[derive(Clone, Debug)]
pub(crate) enum End {
    Unread,
    Read(Option<Rc<str>>),
}

impl End {
    /// Whether the end still has the item seen, `now` being the item there now.
    fn holds(&self, now: Option<&Rc<str>>) -> bool {
        match self {
            Self::Unread => true,
            Self::Read(seen) => seen.as_ref() == now,
        }
    }
}

/// Merge what nested views saw into `into`, one entry per collection.
pub(crate) fn merge_seen(into: &mut Vec<CollSeen>, seen: Vec<CollSeen>) {
    for one in seen {
        match into.iter_mut().find(|held| held.is_of(&one.coll)) {
            Some(held) => held.absorb(one),
            None => into.push(one),
        }
    }
}

/// The listing of `coll` in `store`: its items, or those its filter holds of.
///
/// # Errors
///
/// As [`Store::listing`] and [`Store::listing_where`].
pub(crate) fn listing_of(store: &dyn Store, coll: &CollRef) -> Result<Rc<Listing>, StoreError> {
    let spec = FieldSpec {
        id: coll.field.clone(),
        kind: FieldKind::Collection,
    };
    match coll.filter.as_deref() {
        None => store.listing(&coll.target, &spec),
        Some(pred) => store.listing_where(&coll.target, &spec, &coll.item, pred),
    }
}

/// One read of a collection, recorded as what it saw.
enum Read {
    Len(usize),
    First(Option<Rc<str>>),
    Last(Option<Rc<str>>),
    At(usize, Rc<str>),
}

impl Core {
    /// A reference to the item `id` of `coll`, counted: what reading collections materialized.
    pub(crate) fn item_ref(&self, coll: &CollRef, id: &str) -> Value {
        self.items_built
            .set(self.items_built.get().saturating_add(1));
        coll.item_ref(id)
    }

    /// The listing of `coll`: the one this view run read already, else the store's.
    pub(crate) fn coll_listing(&self, coll: &Rc<CollRef>) -> Result<Rc<Listing>, String> {
        if let Ok(frame) = self.frames.borrow().top()
            && let FrameKind::View(vs) = &frame.kind
            && let Some((_, listing)) = vs
                .listings
                .iter()
                .find(|(held, _)| Rc::ptr_eq(held, coll) || **held == **coll)
        {
            return Ok(Rc::clone(listing));
        }
        let listing = listing_of(&**self.store.borrow(), coll).map_err(|e| e.to_string())?;
        if let Ok(frame) = self.frames.borrow_mut().top_mut()
            && let FrameKind::View(vs) = &mut frame.kind
        {
            vs.listings.push((Rc::clone(coll), Rc::clone(&listing)));
        }
        Ok(listing)
    }

    /// Record, in a view's frame, what one read of `coll`, of `len` items, saw.
    fn saw(&self, coll: &Rc<CollRef>, read: Read, len: usize) {
        let mut frames = self.frames.borrow_mut();
        let Ok(frame) = frames.top_mut() else {
            return;
        };
        let FrameKind::View(vs) = &mut frame.kind else {
            return;
        };
        let at = vs
            .colls
            .iter()
            .position(|seen| seen.is_of(coll))
            .unwrap_or_else(|| {
                vs.colls.push(CollSeen::new(coll));
                vs.colls.len().saturating_sub(1)
            });
        let Some(seen) = vs.colls.get_mut(at) else {
            return;
        };
        if seen.whole {
            return;
        }
        match read {
            Read::Len(len) => seen.len = Some(len),
            Read::First(id) => seen.first = End::Read(id),
            Read::Last(id) => seen.last = End::Read(id),
            Read::At(i, id) => {
                if i == seen.sweep {
                    seen.sweep = seen.sweep.saturating_add(1);
                }
                seen.at.push((i, id));
            }
        }
        // A filtered collection read whole still depends on what it saw item by item: an item
        // changing whether it is in is no change to the collection's field.
        if seen.sweep == len && len > 0 && coll.filter.is_none() {
            seen.whole = true;
            seen.at = Vec::new();
            vs.reads.push((seen.resource.clone(), coll.field.clone()));
        }
    }

    /// `coll_where(list, pred)`: the items of the collection `list` that the predicate written
    /// `pred` holds of, as a collection the store lists (some), when `list` is a collection read
    /// in a view and the predicate reads fields the store answers for; none otherwise, and the
    /// caller's loop runs instead.
    pub(crate) fn coll_where(&self, list: i32, pred: i32) -> HResult<Value> {
        let Value::Coll(coll) = self.val(list)? else {
            return Ok(Value::none());
        };
        let text = self.text(pred)?;
        let Ok(written) = serde_json::from_str::<tilolang::pred::Pred<String>>(&text) else {
            return Ok(Value::none());
        };
        let Some((_, item)) = self.registry.borrow().type_info(&coll.item) else {
            return Ok(Value::none());
        };
        let resolved = written.resolve(&mut |id: &String| {
            let field = item.fields.iter().find(|f| f.id == *id)?;
            let absent = match (field.kind, &field.ty) {
                (FieldKind::Register, Ty::Bool | Ty::Int | Ty::Text) => {
                    crate::host_res::declared_default(&field.ty, field.default.as_ref()).ok()?
                }
                (FieldKind::Counter, Ty::Int) => Value::Int(0),
                (FieldKind::Text, Ty::Text) => Value::text(""),
                _ => return None,
            };
            let absent = match absent {
                Value::Bool(b) => Lit::Bool(b),
                Value::Int(i) => Lit::Int(i),
                Value::Text(t) => Lit::Text(t),
                _ => return None,
            };
            Some(FieldRead {
                spec: FieldSpec {
                    id: field.id.clone(),
                    kind: field.kind,
                },
                absent,
            })
        });
        Ok(resolved.map_or_else(Value::none, |pred| {
            Value::some(Value::Coll(Rc::new(coll.filtered(pred))))
        }))
    }

    /// How many items `coll` has.
    pub(crate) fn coll_len(&self, coll: &Rc<CollRef>) -> HResult<usize> {
        let len = self.coll_listing(coll)?.len();
        self.saw(coll, Read::Len(len), len);
        Ok(len)
    }

    /// A reference to the item at `index` of `coll`.
    pub(crate) fn coll_get(&self, coll: &Rc<CollRef>, index: usize) -> HResult<Value> {
        let listing = self.coll_listing(coll)?;
        let Some(id) = listing.get(index) else {
            return trap(format!(
                "list index {index} out of range for length {}",
                listing.len()
            ));
        };
        self.saw(coll, Read::At(index, Rc::clone(&id)), listing.len());
        Ok(self.item_ref(coll, &id))
    }

    /// The first or last item of `coll`, as an option.
    pub(crate) fn coll_end(&self, coll: &Rc<CollRef>, last: bool) -> HResult<Value> {
        let listing = self.coll_listing(coll)?;
        let end = if last {
            listing.last()
        } else {
            listing.first()
        };
        let value = end
            .as_deref()
            .map_or_else(Value::none, |id| Value::some(self.item_ref(coll, id)));
        self.saw(
            coll,
            if last {
                Read::Last(end)
            } else {
                Read::First(end)
            },
            listing.len(),
        );
        Ok(value)
    }

    /// Every item of `coll`, as references: the whole collection, which a view running now then
    /// depends on.
    ///
    /// # Errors
    ///
    /// A message when the store cannot list the collection.
    pub(crate) fn coll_items(&self, coll: &Rc<CollRef>) -> Result<Vec<Value>, String> {
        let listing = self.coll_listing(coll)?;
        if coll.filter.is_some() {
            self.saw(coll, Read::Len(listing.len()), listing.len());
            for (i, id) in listing.iter().enumerate() {
                self.saw(coll, Read::At(i, id), listing.len());
            }
        } else if let Ok(frame) = self.frames.borrow_mut().top_mut()
            && let FrameKind::View(vs) = &mut frame.kind
        {
            vs.reads.push((coll.target.id_string(), coll.field.clone()));
        }
        Ok(listing.iter().map(|id| self.item_ref(coll, &id)).collect())
    }

    /// `v`, with a collection read whole into a list: what goes into another value.
    ///
    /// # Errors
    ///
    /// A message when the store cannot list the collection.
    pub(crate) fn whole(&self, v: Value) -> Result<Value, String> {
        match v {
            Value::Coll(coll) => self.coll_items(&coll).map(Value::List),
            other => Ok(other),
        }
    }

    /// A copy of the value behind a handle, a collection read whole into a list: for a value
    /// that goes into another one, or out of the view.
    pub(crate) fn val_whole(&self, h: i32) -> HResult<Value> {
        let v = self.val(h)?;
        Ok(self.whole(v)?)
    }
}
