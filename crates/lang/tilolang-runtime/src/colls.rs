//! Collections read lazily in views, and what a view depends on of one.
//!
//! A view that reads a collection field gets a [`Value::Coll`]: the field, not a list of its
//! items. Its length, an item at an index, its first and its last item are read through the
//! store's listing ([`crate::store::Store::listing`]), shared, one at a time, and each read is
//! recorded as what it saw ([`CollSeen`]). A change to the collection re-runs a view only if
//! something it saw is no longer so: `messages.last()` depends on the last message, not on the
//! 10,000 before it, and builds one reference, not 10,000.
//!
//! Wherever the items are needed as values (compared, stored, put into a list or a tuple) the
//! collection is read whole into a [`Value::List`], and the view then depends on the whole
//! collection, as before. A sort keeps the collection: the store lists its items in the order
//! asked for ([`Core::coll_ordered`]), so a list sorted by a field stays one list of the
//! collection, read a page at a time.

use std::rc::Rc;

use crate::core::{Core, HResult, trap};
use crate::meta::FieldKind;
use crate::store::{FieldRead, FieldSpec, Listing, Store, StoreError};
use crate::value::{CollRef, RefValue, Value};

/// What a view saw of one collection: its length, its first and last item, the item at each
/// index it read (an item by its id), and the item right before each item it asked for. It
/// still holds after a change when each is still what it was.
#[derive(Clone, Debug)]
pub(crate) struct CollSeen {
    pub coll: Rc<CollRef>,
    /// The collection's resource, as [`crate::value::ResourceRef::id_string`] writes it.
    pub resource: String,
    /// How long it is: the most it can hold while its filter checks items one by one as it
    /// reads them.
    pub len: Option<usize>,
    /// How many items a view counted it as holding, where it read the count to show it.
    pub count: Option<usize>,
    pub first: End,
    pub last: End,
    pub at: Vec<(usize, Rc<str>)>,
    /// The item read as the one right before each item a view asked for
    /// ([`crate::host_values::Imp::ListPrevious`]): a row's neighbour, which stays the one it
    /// saw while the rows ahead of them come and go.
    before: Vec<(Rc<str>, Option<Rc<str>>)>,
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
            count: None,
            first: End::Unread,
            last: End::Unread,
            at: Vec::new(),
            before: Vec::new(),
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
            && self.coll.order == coll.order
    }

    /// Whether everything seen is still so in `listing`.
    pub fn holds(&self, listing: &Listing) -> bool {
        // Read whole, it depends on the collection's field instead.
        if self.whole {
            return true;
        }
        if self.len.is_some_and(|len| len != listing.len()) {
            return false;
        }
        // A count is exact where the listing knows its length without reading its run: one that
        // checks its entries as it reads them was read to its end to say the count, and a
        // listing that changed is read again rather than trusted.
        if self
            .count
            .is_some_and(|count| !listing.len_is_exact() || count != listing.len())
        {
            return false;
        }
        // This listing is another state of the store, and reads its run again. Where it holds
        // as many items as were seen but is not sure it has them all (a filter that checks
        // them one by one as it reads, [`Listing::len_is_exact`]), where its end is would cost
        // reading the run: the view runs again and keeps the rows it has, as a list of any
        // length does.
        if !listing.len_is_exact() && (!self.at.is_empty() || matches!(self.last, End::Read(_))) {
            return false;
        }
        self.first.holds_in(|| listing.first())
            && self.last.holds_in(|| listing.last())
            && self
                .at
                .iter()
                .all(|(i, id)| listing.get(*i).as_ref() == Some(id))
            && self.before.iter().all(|(item, seen)| {
                matches!(before_in(listing, item), Neighbour::At(now) if now == *seen)
            })
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
            self.before = Vec::new();
            return;
        }
        self.len = self.len.or(other.len);
        self.count = self.count.or(other.count);
        if matches!(self.first, End::Unread) {
            self.first = other.first;
        }
        if matches!(self.last, End::Unread) {
            self.last = other.last;
        }
        self.at.extend(other.at);
        self.before.extend(other.before);
    }
}

/// What a view saw at one end of a collection: nothing, or the item there (none when empty).
#[derive(Clone, Debug)]
pub(crate) enum End {
    Unread,
    Read(Option<Rc<str>>),
}

impl End {
    /// Whether the end still has the item seen, `now` reading the item there now: read only
    /// when an item was seen there, so a collection whose length alone was seen reads none of
    /// its items.
    fn holds_in(&self, now: impl FnOnce() -> Option<Rc<str>>) -> bool {
        match self {
            Self::Unread => true,
            Self::Read(seen) => *seen == now(),
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
    let listed = match (coll.filter.as_deref(), &coll.order) {
        (None, None) => store.listing(&coll.target, &spec),
        (Some(pred), None) => store.listing_where(&coll.target, &spec, &coll.item, pred),
        (pred, Some(order)) => {
            store.listing_query(&coll.target, &spec, &coll.item, pred, Some(order))
        }
    };
    match listed {
        Err(StoreError::NotFound(_)) if coll.target.is_unmade_root() => {
            Ok(Rc::new(Listing::from_ids(Vec::new())))
        }
        other => other,
    }
}

/// `pred`, its fields (by id) resolved to what the store reads of items of the type `item`:
/// `None` when it reads a field the type lacks or one no filter reads.
pub(crate) fn field_reads(
    pred: &tilolang::pred::Pred<String>,
    item: &crate::registry::TypeInfo,
) -> Option<crate::store::Pred> {
    pred.resolve(&mut |id: &String| {
        let field = item.fields.iter().find(|f| f.id == **id)?;
        FieldRead::of(id, field.kind, &field.ty, field.default.as_ref())
    })
}

/// A value the code holds as a predicate compares it: `None` for one it does not.
fn lit_of_value(v: &Value) -> Option<tilolang::pred::Lit> {
    use tilolang::pred::{Float, Lit};
    Some(match v {
        Value::Int(i) => Lit::Int(*i),
        Value::Float(f) => Lit::Float(Float(*f)),
        Value::Bool(b) => Lit::Bool(*b),
        Value::Text(t) => Lit::Text(t.clone()),
        Value::Ref(r) => Lit::Ref(r.target.id_string()),
        Value::Opt(None) => Lit::Absent,
        Value::Opt(Some(inner)) => return lit_of_value(inner),
        _ => return None,
    })
}

/// The item right before `item` in `listing`: a row's neighbour holds while the item before
/// it is still the one it saw, wherever the rows ahead of them come and go.
fn before_in(listing: &Listing, item: &str) -> Neighbour {
    match listing.position(item) {
        Some(at) => Neighbour::At(
            at.and_then(|at| at.checked_sub(1))
                .and_then(|at| listing.get(at)),
        ),
        None => Neighbour::Unknown,
    }
}

/// What a listing says stands right before an item: the item there, or none where it is the
/// first or not in the listing. A listing that cannot say where the item stands says nothing.
enum Neighbour {
    At(Option<Rc<str>>),
    Unknown,
}

/// One read of a collection, recorded as what it saw.
enum Read {
    Len(usize),
    Count(usize),
    First(Option<Rc<str>>),
    Last(Option<Rc<str>>),
    At(usize, Rc<str>),
    Before(Rc<str>, Option<Rc<str>>),
}

impl Core {
    /// A reference to the item `id` of `coll`, counted: what reading collections materialized.
    pub(crate) fn item_ref(&self, coll: &CollRef, id: &str) -> Value {
        self.items_built
            .set(self.items_built.get().saturating_add(1));
        if coll.item.is_empty() {
            // A collection of a union's types: the item's own type.
            let target = coll.target.item(id, String::new());
            let ty = self.stored_type(&target);
            return Value::Ref(crate::value::RefValue {
                target: Rc::new(coll.target.item(id, ty)),
                author: coll.author.clone(),
            });
        }
        coll.item_ref(id)
    }

    /// The listing of `coll`: the one this view run read already, else the store's.
    pub(crate) fn coll_listing(&self, coll: &Rc<CollRef>) -> Result<Rc<Listing>, String> {
        if let Some(vs) = self.frames.borrow().view()
            && let Some((_, listing)) = vs
                .listings
                .iter()
                .find(|(held, _)| Rc::ptr_eq(held, coll) || **held == **coll)
        {
            return Ok(Rc::clone(listing));
        }
        let listing = listing_of(&**self.store.borrow(), coll).map_err(|e| e.to_string())?;
        if let Some(vs) = self.frames.borrow_mut().view_mut() {
            vs.listings.push((Rc::clone(coll), Rc::clone(&listing)));
        }
        Ok(listing)
    }

    /// Record, in a view's frame, what one read of `coll`, of `len` items, saw.
    fn saw(&self, coll: &Rc<CollRef>, read: Read, len: usize) {
        let mut frames = self.frames.borrow_mut();
        let Some(vs) = frames.view_mut() else {
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
            Read::Count(count) => seen.count = Some(count),
            Read::First(id) => seen.first = End::Read(id),
            Read::Last(id) => seen.last = End::Read(id),
            Read::At(i, id) => {
                if i == seen.sweep {
                    seen.sweep = seen.sweep.saturating_add(1);
                }
                seen.at.push((i, id));
            }
            Read::Before(item, before) => seen.before.push((item, before)),
        }
        // A filtered or ordered collection read whole still depends on what it saw item by
        // item: an item changing whether it is in, or where, is no change to the collection's
        // field.
        if seen.sweep == len && len > 0 && !coll.derived() {
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
        let resolved = field_reads(&written, &item);
        Ok(resolved.map_or_else(Value::none, |pred| {
            Value::some(Value::Coll(Rc::new(coll.filtered(pred))))
        }))
    }

    /// `coll_relatives(holder, field, parent, node, down)`: the items of the collection `field`
    /// of `holder` under `node` (above it, for `down` 0) by the reference field `parent`, those
    /// under it in the order the collection keeps them and those above it from the nearest, as
    /// a list (some); none when the store cannot follow that field, and the caller's walk runs
    /// instead.
    ///
    /// A view is not answered: what it depends on is the `parent` field of every item that is
    /// not under the one it asks after, which the walk reads and records one item at a time.
    pub(crate) fn coll_relatives(
        &self,
        holder: i32,
        field: i32,
        parent: i32,
        node: i32,
        down: i32,
    ) -> HResult<Value> {
        if self.frames.borrow().view().is_some() {
            return Ok(Value::none());
        }
        let Ok(holder) = self.ref_arg(holder) else {
            return Ok(Value::none());
        };
        let Ok(node) = self.ref_arg(node) else {
            return Ok(Value::none());
        };
        let (Ok(field), Ok(parent)) = (self.text(field), self.text(parent)) else {
            return Ok(Value::none());
        };
        let Ok((_, collection)) = self.lookup_field(&holder.target, &field) else {
            return Ok(Value::none());
        };
        let Ok(item) = self.item_type(&collection.ty) else {
            return Ok(Value::none());
        };
        let Some(parent) = item.field_by_id_or_name(&parent) else {
            return Ok(Value::none());
        };
        let Some(found) = self
            .store
            .borrow()
            .relatives(
                &holder.target,
                &collection.id,
                &parent.id,
                &node.target,
                down != 0,
            )
            .map_err(|e| e.to_string())?
        else {
            return Ok(Value::none());
        };
        let author = Some(self.module_of_frame()?);
        let holder = holder.target;
        Ok(Value::some(Value::List(
            found
                .into_iter()
                .map(|(id, ty)| {
                    Value::Ref(RefValue {
                        target: Rc::new(holder.item(&id, ty)),
                        author: author.clone(),
                    })
                })
                .collect(),
        )))
    }

    /// `coll_ordered(list, order)`: the items of the collection `list` in the order written
    /// `order` (`{"field": id, "descending": bool, "absent_last": bool, "then": {...}}`, each
    /// field a declared one or a version, `$created` or `$updated`), as a collection the store
    /// lists (some), when `list` is a collection read in a view and the store can order by the
    /// fields; none otherwise, and the caller sorts the items itself. A collection already in an
    /// order keeps it where this one holds two items equal: the order the caller gave stands
    /// among the items this one cannot tell apart.
    pub(crate) fn coll_ordered(&self, list: i32, order: i32) -> HResult<Value> {
        #[derive(serde::Deserialize)]
        struct Written {
            field: String,
            #[serde(default)]
            descending: bool,
            #[serde(default)]
            absent_last: bool,
            #[serde(default)]
            then: Option<Box<Written>>,
        }
        let Value::Coll(coll) = self.val(list)? else {
            return Ok(Value::none());
        };
        let text = self.text(order)?;
        let Ok(written) = serde_json::from_str::<Written>(&text) else {
            return Ok(Value::none());
        };
        let Some((_, item)) = self.registry.borrow().type_info(&coll.item) else {
            return Ok(Value::none());
        };
        let mut levels = Vec::new();
        let mut at = Some(&written);
        while let Some(level) = at {
            let kind = if crate::store::Reflected::of(&level.field).is_some() {
                FieldKind::Register
            } else {
                match item.field(&level.field).map(|f| f.kind) {
                    Some(kind @ (FieldKind::Register | FieldKind::Counter | FieldKind::Text)) => {
                        kind
                    }
                    _ => return Ok(Value::none()),
                }
            };
            levels.push(crate::query::Order {
                field: level.field.clone(),
                kind,
                descending: level.descending,
                absent_last: level.absent_last,
                then: None,
            });
            at = level.then.as_deref();
        }
        // The order the collection is already in settles the items this one cannot separate.
        levels.extend(coll.order.clone());
        let Some(order) = crate::query::Order::chained(levels) else {
            return Ok(Value::none());
        };
        let ordered = CollRef {
            order: Some(order),
            ..(*coll).clone()
        };
        Ok(Value::some(Value::Coll(Rc::new(ordered))))
    }

    /// `coll_where_with(list, pred, args)`: as [`Self::coll_where`], the predicate comparing
    /// fields with the values the tuple `args` holds (`c -> c.column == col`): each bound as
    /// the literal it is now. None when one is a value no predicate compares.
    pub(crate) fn coll_where_with(&self, list: i32, pred: i32, args: i32) -> HResult<Value> {
        let Value::Coll(coll) = self.val(list)? else {
            return Ok(Value::none());
        };
        let text = self.text(pred)?;
        let Ok(written) = serde_json::from_str::<tilolang::pred::Pred<String>>(&text) else {
            return Ok(Value::none());
        };
        let Value::Tuple(values) = self.val(args)? else {
            return Ok(Value::none());
        };
        let Some(lits) = values.iter().map(lit_of_value).collect::<Option<Vec<_>>>() else {
            return Ok(Value::none());
        };
        let Some(bound) = written.bind(&lits) else {
            return Ok(Value::none());
        };
        let Some((_, item)) = self.registry.borrow().type_info(&coll.item) else {
            return Ok(Value::none());
        };
        Ok(field_reads(&bound, &item).map_or_else(Value::none, |pred| {
            Value::some(Value::Coll(Rc::new(coll.filtered(pred))))
        }))
    }

    /// How long `coll` is: what its listing knows of its length without reading it. A filter
    /// that checks items one by one as it reads them knows only the most it can hold until it
    /// has read them all, and says that rather than claim it exact
    /// ([`crate::store::Listing::len_is_exact`]); how many it holds is [`Self::coll_count`],
    /// which a view asks for where it shows a count rather than a list's extent.
    pub(crate) fn coll_len(&self, coll: &Rc<CollRef>) -> HResult<usize> {
        let listing = self.coll_listing(coll)?;
        let len = listing.len();
        self.saw(coll, Read::Len(len), len);
        Ok(len)
    }

    /// How many items `coll` holds: its listing's count, read to its end where a filter checks
    /// its items as it reads them ([`crate::store::Listing::count`]). A view pays what the
    /// collection shows for it, so it counts a collection it draws as a whole, never the
    /// workspace a filter picks from.
    ///
    /// What it depends on is the count itself: it is another one when the listing's length is,
    /// which the store answers without reading the run, and a listing that has to check its
    /// entries again is read again rather than trusted.
    pub(crate) fn coll_count(&self, coll: &Rc<CollRef>) -> HResult<usize> {
        let listing = self.coll_listing(coll)?;
        let count = listing.count();
        self.saw(coll, Read::Count(count), listing.len());
        Ok(count)
    }

    /// [`Core::coll_count`], when the store has read the whole workspace, and none before it
    /// has: a view shows a number only when it is the workspace's, never the part read so far.
    ///
    /// A run the store does not count answers the number only where it is no longer than the
    /// page a window reads anyway: a filter checked item by item over a run the workspace holds
    /// is never read to its end for one, so a place filtered on the day or the person reading
    /// it says nothing there rather than read the workspace into the frame that opens it.
    pub(crate) fn coll_counted(&self, coll: &Rc<CollRef>) -> HResult<Option<usize>> {
        let listing = self.coll_listing(coll)?;
        let len = listing.len();
        if !listing.len_is_exact() && len > crate::reads::READ_PAGE {
            self.saw(coll, Read::Len(len), len);
            return Ok(None);
        }
        let count = listing.count();
        self.saw(coll, Read::Count(count), len);
        Ok(self.settled().then_some(count))
    }

    /// Whether the store has read the whole workspace ([`crate::store::Store::settled`]).
    pub(crate) fn settled(&self) -> bool {
        self.store.borrow().settled()
    }

    /// Whether `coll` holds nothing: whether it shows no first item. A listing read page by
    /// page answers that by reading the page its first item is on, never by reading its run to
    /// the end, which is what counting it costs ([`Self::coll_count`]).
    ///
    /// What it depends on is where its first item is: the listing shows another one, or none,
    /// when that changes.
    pub(crate) fn coll_empty(&self, coll: &Rc<CollRef>) -> HResult<bool> {
        let listing = self.coll_listing(coll)?;
        let first = listing.first();
        let empty = first.is_none();
        self.saw(coll, Read::First(first), listing.len());
        Ok(empty)
    }

    /// [`Core::coll_len`] or [`Core::coll_count`], as an import asks for one or the other.
    pub(crate) fn coll_len_or_count(&self, coll: &Rc<CollRef>, count: bool) -> HResult<usize> {
        if count {
            self.coll_count(coll)
        } else {
            self.coll_len(coll)
        }
    }

    /// Record that a view read the whole of `coll`: it depends on the collection's field, or,
    /// for a filtered or ordered one, on the items it saw, each of which decides whether it is
    /// in or where.
    fn read_whole(&self, coll: &Rc<CollRef>, listing: &Listing) {
        if coll.derived() {
            self.saw(coll, Read::Len(listing.len()), listing.len());
            for (i, id) in listing.iter().enumerate() {
                self.saw(coll, Read::At(i, id), listing.len());
            }
        } else if let Some(vs) = self.frames.borrow_mut().view_mut() {
            vs.reads.push((coll.target.id_string(), coll.field.clone()));
        }
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

    /// The item right before the item at `at` in `coll`, as a row asks for its neighbour: the
    /// read is what the item before it is, not the place either stood at, so rows after one
    /// that left keep theirs.
    pub(crate) fn coll_before(&self, coll: &Rc<CollRef>, at: usize, item: &str) -> HResult<Value> {
        let listing = self.coll_listing(coll)?;
        let before = at.checked_sub(1).and_then(|before| listing.get(before));
        self.saw(
            coll,
            Read::Before(Rc::from(item), before.clone()),
            listing.len(),
        );
        Ok(before
            .as_deref()
            .map_or_else(Value::none, |id| Value::some(self.item_ref(coll, id))))
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

    /// The first `n` items of `coll`, as references: the items a `take` shows, and no more, so
    /// a view that walks a collection up to a count reads that many of its items, not all.
    ///
    /// # Errors
    ///
    /// A message when the store cannot list the collection.
    pub(crate) fn coll_take(&self, coll: &Rc<CollRef>, n: usize) -> Result<Vec<Value>, String> {
        let listing = self.coll_listing(coll)?;
        let len = listing.len();
        let mut out = Vec::new();
        for i in 0..n {
            let Some(id) = listing.get(i) else {
                break;
            };
            self.saw(coll, Read::At(i, Rc::clone(&id)), len);
            out.push(self.item_ref(coll, &id));
        }
        Ok(out)
    }

    /// Every item of `coll`, as references: the whole collection, which a view running now then
    /// depends on.
    ///
    /// # Errors
    ///
    /// A message when the store cannot list the collection.
    pub(crate) fn coll_items(&self, coll: &Rc<CollRef>) -> Result<Vec<Value>, String> {
        let listing = self.coll_listing(coll)?;
        self.read_whole(coll, &listing);
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
