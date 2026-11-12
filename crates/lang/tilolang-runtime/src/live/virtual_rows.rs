//! Virtual lists: the rows of a `list` built when the host shows them, and absent until then.
//!
//! A `list` whose rows come from one loop holds, as its children in the tree, only the rows
//! built so far (shown, in order); its length and the key of each row are read from what the
//! rows are made of (a collection's listing, or a list of values) when the host asks. Opening a
//! list of 10,000 items builds no row, no reference and no node per item: it costs the rows on
//! screen. The host holds every row's key (`list_len`, `row_key`); the patch stream speaks of
//! the rows as before (`Insert` at a row's index, `Remove` and `Move` by key), and a row not
//! shown is, for the patches, an empty `fragment`.
//!
//! The rows follow their collection's steps as the loops of other containers do. When the
//! boundary holding the list runs again, the rows built that still stand are kept by key, and
//! if what the rows are made of changed (steps that did not fit, another list of values) the
//! steps from the rows the host holds to the new ones are found by key. Rows of a filtered or
//! sorted query follow it the same way: a change names the items it touched, the store says
//! where each was in the listing the rows stand for and where it is now
//! ([`crate::store::moved_steps`]), and those are the steps, so a post that brings one item to
//! the top of 100,000 reads the pages of that item, not the list's.
//!
//! A row the person's own change takes out of the rows stands where it is until the view is
//! left, and so does the row standing open into its item, whoever's change takes it out: what
//! a person reads does not vanish from under them ([`Virtual::open`]). Rows made of another
//! query (a filter or an order chosen) are new rows: the host reads them anew, as it reads a
//! list new to the tree, and choosing a filter costs the rows it shows, not the collection's.

use std::collections::HashMap;
use std::rc::Rc;

use super::{Bound, BoundId, Builder, Live, Pending, Steps};
use crate::core::Core;
use crate::error::RuntimeError;
use crate::host_ui::SET_ACTION;
use crate::render::{RowSource, RowsOf, VirtualRows, item_key};
use crate::store::{Listing, Splice};
use crate::value::{ActionRef, CollRef, Value};
use crate::view::{Patch, RowStep, RowsChange, UiNode, row_steps};
use crate::view_state::StateKey;

/// The rows a virtual list holds built: each row's place and boundary, and the fragments that
/// stand for the rows in the list.
type BuiltRows = (Vec<(usize, BoundId)>, Vec<(usize, UiNode)>);

/// The rows of a virtual list as the host holds them: what they are made of, as of the last
/// steps the host was given.
#[derive(Clone)]
pub(crate) enum Rows {
    Coll {
        coll: Rc<CollRef>,
        listing: Rc<Listing>,
        /// The rows the person's own changes, or any change to the row standing open, took out
        /// of the listing, kept where they stand (`place`, item) until the view is left: see
        /// [`Live::keep_rows`].
        kept: Vec<(usize, Rc<str>)>,
    },
    List {
        values: Rc<[Value]>,
        /// The rows the person's own changes, or any change to the row standing open, took out
        /// of the list, kept where they stand (`place`, item) until the view is left: see
        /// [`still_kept_list`].
        kept: Vec<(usize, Value)>,
    },
}

impl Rows {
    fn len(&self) -> usize {
        match self {
            Self::Coll { listing, kept, .. } => listing.len().saturating_add(kept.len()),
            Self::List { values, kept } => values.len().saturating_add(kept.len()),
        }
    }

    /// The collection whose items the rows are, if they are a collection's.
    fn coll(&self) -> Option<Rc<CollRef>> {
        match self {
            Self::Coll { coll, .. } => Some(Rc::clone(coll)),
            Self::List { .. } => None,
        }
    }

    /// Whether [`Self::len`] is the rows' length and not the most they can hold: a listing that
    /// checks its items one by one as it reads them says how many it holds only once it has read
    /// them all, and before then the most its run can name, a number that moves with the run's
    /// bounds.
    fn len_is_exact(&self) -> bool {
        match self {
            Self::Coll { listing, .. } => listing.len_is_exact(),
            Self::List { .. } => true,
        }
    }

    /// The collection's item of row `index`, kept rows counted.
    fn coll_row(&self, index: usize) -> Option<Rc<str>> {
        let Self::Coll { listing, kept, .. } = self else {
            return None;
        };
        let before = kept.partition_point(|(place, _)| *place < index);
        if let Some((place, item)) = kept.get(before)
            && *place == index
        {
            return Some(Rc::clone(item));
        }
        listing.get(index.saturating_sub(before))
    }

    /// The list item of row `index`, kept rows counted.
    fn list_row(&self, index: usize) -> Option<&Value> {
        let Self::List { values, kept } = self else {
            return None;
        };
        let before = kept.partition_point(|(place, _)| *place < index);
        if let Some((place, item)) = kept.get(before)
            && *place == index
        {
            return Some(item);
        }
        values.get(index.saturating_sub(before))
    }

    /// The rows of a collection kept for the viewer, and where they stand.
    fn kept(&self) -> &[(usize, Rc<str>)] {
        match self {
            Self::Coll { kept, .. } => kept,
            Self::List { .. } => &[],
        }
    }

    /// The key of row `index`, built from `template` without a reference to its item.
    fn key(&self, template: &RowsOf, index: usize) -> Option<String> {
        match self {
            Self::Coll { coll, .. } => self
                .coll_row(index)
                .map(|id| template.coll_row_key(coll, &id)),
            Self::List { .. } => self
                .list_row(index)
                .map(|item| template.key_of(&item_key(item, index))),
        }
    }

    /// The item of row `index`.
    fn item(&self, core: &Core, index: usize) -> Option<Value> {
        match self {
            Self::Coll { coll, .. } => self.coll_row(index).map(|id| core.item_ref(coll, &id)),
            Self::List { .. } => self.list_row(index).cloned(),
        }
    }

    /// Where the row of the item `id` stands among the rows, when one of them shows it.
    fn place(&self, id: &str) -> Option<usize> {
        match self {
            Self::Coll { .. } => self.listing()?.position(id).flatten(),
            Self::List { .. } => (0..self.len())
                .find(|index| self.list_row(*index).and_then(item_ref_id) == Some(id)),
        }
    }

    /// The rows kept for the viewer settle: the view they stood in is left.
    fn settle(&mut self) {
        match self {
            Self::Coll { kept, .. } => kept.clear(),
            Self::List { kept, .. } => kept.clear(),
        }
    }

    /// These rows as a listing a store reads (`crate::store::moved_steps`): the listing, with
    /// the rows kept for the viewer at their places. `None` for rows of values.
    fn listing(&self) -> Option<Rc<Listing>> {
        match self {
            Self::Coll { listing, kept, .. } if kept.is_empty() => Some(Rc::clone(listing)),
            Self::Coll { listing, kept, .. } => Some(Rc::new(Listing::paged(Rc::new(KeptRows {
                base: Rc::clone(listing),
                kept: kept.clone(),
            })))),
            Self::List { .. } => None,
        }
    }

    /// Whether these are the same rows as `other` for the rows `built` (the ones the host
    /// holds, by index): the same collection, the same rows kept for the viewer, and at each
    /// of those indexes the same item.
    ///
    /// A row nobody holds need not be compared: a virtual list is read one screen at a time,
    /// so saying every item of the room stood where it stood would read every page of it each
    /// time the room changed at all.
    fn same(&self, other: &Self, built: &[(usize, BoundId)]) -> bool {
        match (self, other) {
            (
                Self::Coll {
                    coll: mine_coll,
                    kept: kept_coll_mine,
                    ..
                },
                Self::Coll {
                    coll: theirs_coll,
                    kept: kept_coll_theirs,
                    ..
                },
            ) => {
                mine_coll == theirs_coll
                    && kept_coll_mine == kept_coll_theirs
                    && built.iter().all(|(index, _)| {
                        match (self.coll_row(*index), other.coll_row(*index)) {
                            (Some(mine), Some(theirs)) => mine == theirs,
                            (None, None) => true,
                            _ => false,
                        }
                    })
            }
            (
                Self::List {
                    values: mine_values,
                    kept: kept_mine,
                },
                Self::List {
                    values: theirs_values,
                    kept: kept_theirs,
                },
            ) => {
                kept_mine == kept_theirs
                    && (Rc::ptr_eq(mine_values, theirs_values) || mine_values == theirs_values)
            }
            _ => false,
        }
    }

    /// Every row's key: what the steps between two versions of a list are found by.
    fn keys(&self, template: &RowsOf) -> Vec<String> {
        (0..self.len())
            .filter_map(|index| self.key(template, index))
            .collect()
    }
}

/// A listing with the rows the viewer's own changes took out of it kept at their places: what
/// the rows of a list stand for until the view is left.
#[derive(Debug)]
struct KeptRows {
    base: Rc<Listing>,
    /// Sorted by place.
    kept: Vec<(usize, Rc<str>)>,
}

impl KeptRows {
    /// How many kept rows stand before the row `index`.
    fn before(&self, index: usize) -> usize {
        self.kept.partition_point(|(place, _)| *place < index)
    }

    /// Where the base's item `index` stands among the rows: after every kept row at or before
    /// the place it lands on. Each kept row before it moves it on by one, and a kept row at the
    /// place it lands on stands there instead ([`Self::get`]).
    fn place_of(&self, index: usize) -> usize {
        let mut kept = 0;
        while let Some((place, _)) = self.kept.get(kept) {
            if *place > index.saturating_add(kept) {
                break;
            }
            kept = kept.saturating_add(1);
        }
        index.saturating_add(kept)
    }
}

impl crate::store::PagedItems for KeptRows {
    fn len(&self) -> usize {
        self.base.len().saturating_add(self.kept.len())
    }

    fn len_is_exact(&self) -> bool {
        self.base.len_is_exact()
    }

    fn get(&self, index: usize) -> Option<Rc<str>> {
        let before = self.before(index);
        if let Some((place, item)) = self.kept.get(before)
            && *place == index
        {
            return Some(Rc::clone(item));
        }
        self.base.get(index.saturating_sub(before))
    }

    fn position(&self, id: &str) -> Option<Option<usize>> {
        if let Some((place, _)) = self.kept.iter().find(|(_, item)| &**item == id) {
            return Some(Some(*place));
        }
        let index = self.base.position(id)?;
        Some(index.map(|index| self.place_of(index)))
    }

    /// Whether the rows hold the item: a kept row does, and the base answers for the others
    /// from the item's own entries.
    fn holds(&self, id: &str) -> Option<bool> {
        if self.kept.iter().any(|(_, item)| &**item == id) {
            return Some(true);
        }
        self.base.holds(id)
    }
}

/// A virtual list: the boundary holding it, how its rows are made and from what, the rows built
/// (each with its index, in order: the list node's children) and how many rows the host holds.
pub(crate) struct Virtual {
    pub holder: BoundId,
    template: RowsOf,
    rows: Rows,
    built: Vec<(usize, BoundId)>,
    pub len: usize,
    /// Rows the steps inserted or removed since the rows were read: the listing the rows stand
    /// for does not hold them, and the count the host holds is the listing's with them.
    stepped: bool,
    /// The key of the row standing open into its item where it stands: the host says which one
    /// it shows open, and a row's document is built for it alone. A key, not a place, so the row
    /// the person reads stays the row it is as the rows around it come, go and move.
    open: Option<String>,
    /// The host's rows are no longer known: steps were given that did not fit. The host reads
    /// the list anew when its holder next runs.
    lost: bool,
}

impl Virtual {
    /// The list as its holder just read it: `rows` its rows as the host holds them, and `built`
    /// the ones built, in order. The length is the rows' own: the steps since add theirs.
    fn new(
        holder: BoundId,
        template: RowsOf,
        rows: Rows,
        built: Vec<(usize, BoundId)>,
        open: Option<String>,
    ) -> Self {
        let len = rows.len();
        Self {
            holder,
            template,
            rows,
            built,
            len,
            stepped: false,
            open,
            lost: false,
        }
    }

    /// The list's length as reading a row of it left it: a listing that checks its items as it
    /// reads them holds what it holds once it has read them all, and the count the host holds
    /// until then is the most they can be. The rows the steps added or removed since the listing
    /// was read stand whatever it says ([`Self::stepped`]).
    pub(super) fn settle_len(&mut self) {
        let held = self.rows.len();
        if !self.stepped && self.rows.len_is_exact() && held < self.len {
            self.len = held;
        }
    }

    /// The key of row `index`.
    pub fn key(&self, index: usize) -> Option<String> {
        if index >= self.len {
            return None;
        }
        self.rows.key(&self.template, index)
    }

    /// Where the row of the item `id` stands among the rows, when one of them shows it: where
    /// the divider of the person's unseen part goes.
    pub(super) fn place(&self, id: &str) -> Option<usize> {
        self.rows.place(id).filter(|index| *index < self.len)
    }

    /// Where the row at `place` among the rows built stands among the list's rows.
    pub(super) fn row(&self, place: usize) -> Option<usize> {
        self.built.get(place).map(|(index, _)| *index)
    }

    /// The item of the collection whose row stands open, when the rows are a collection's.
    fn open_id(&self) -> Option<&str> {
        let Rows::Coll { coll, .. } = &self.rows else {
            return None;
        };
        self.template.coll_row_id(coll, self.open.as_deref()?)
    }

    /// The items of the rows built now (the visible rows and the overscan): what the window
    /// shows of this list.
    pub(super) fn shown_items(&self, core: &Core) -> Vec<Value> {
        self.built
            .iter()
            .filter_map(|(index, _)| self.rows.item(core, *index))
            .collect()
    }
}

impl Live {
    /// The row of `list`, a list of values, standing open into its item: where it stands among
    /// the rows, and its item. A row open is one the host shows, so it is built.
    fn open_value(&self, list: &Virtual) -> Option<(usize, Value)> {
        let open = list.open.as_deref()?;
        let (index, _) = list
            .built
            .iter()
            .find(|(_, id)| self.bound(*id).is_some_and(|b| b.key == open))?;
        Some((*index, list.rows.list_row(*index)?.clone()))
    }

    /// The host shows the row at `open` of the list `list` standing open into its item: the row
    /// that opens builds its item's document and the row that closes drops it, both rendered
    /// again where they stand — the list sends them on as the rows that changed.
    pub fn open_row(&mut self, list: &str, open: Option<&str>) {
        let Some(rows) = self.lists.get(list) else {
            return;
        };
        if rows.open.as_deref() == open {
            return;
        }
        let before = rows.open.clone();
        let bounds = &self.bounds;
        let reopened: Vec<(BoundId, bool)> = rows
            .built
            .iter()
            .filter_map(|(_, id)| {
                let key = bounds.get(*id)?.as_ref()?.key.as_str();
                let opens = key == open.unwrap_or_default();
                (opens || Some(key) == before.as_deref()).then_some((*id, opens))
            })
            .collect();
        if let Some(rows) = self.lists.get_mut(list) {
            rows.open = open.map(str::to_owned);
        }
        for (id, opens) in reopened {
            if let Some(b) = self.bound_mut(id) {
                b.open = opens;
            }
            self.dirty.insert(id);
        }
    }

    /// The host's rows of the virtual list `list`, if it is one, are no longer known.
    pub(super) fn lose_rows(&mut self, list: &str) {
        if let Some(rows) = self.lists.get_mut(list) {
            rows.lost = true;
        }
    }

    /// The lists the moved view state `left` (each boundary that read it, and the state) leaves:
    /// those a boundary that read it encloses. A list's own row opening or closing — the state its
    /// `opened` and `closed` events set — leaves none: the person reads on in the same rows.
    pub(super) fn left_lists(&self, core: &Core, left: &[(BoundId, &StateKey)]) -> Vec<String> {
        self.lists
            .iter()
            .filter(|(key, list)| {
                left.iter().any(|(moved, state)| {
                    self.encloses(*moved, list.holder) && !self.opens_with(core, key, state)
                })
            })
            .map(|(key, _)| key.clone())
            .collect()
    }

    /// Whether the list `list` opens and closes its rows with the view state `state`: its
    /// `opened` or `closed` event sets it.
    fn opens_with(&self, core: &Core, list: &str, state: &StateKey) -> bool {
        let Some(node) = self.find(list) else {
            return false;
        };
        let frames = core.frames.borrow();
        [OPENED, CLOSED].iter().any(|event| {
            node.events
                .get(*event)
                .and_then(|retained| frames.retained(*retained))
                .is_some_and(|action| matches!(action, Value::Action(set) if sets(set, state)))
        })
    }

    /// The rows kept for the viewer settle in the lists `settling`: the view they stood in is
    /// left.
    pub(super) fn settle_kept(&mut self, settling: &[String]) {
        for key in settling {
            if let Some(list) = self.lists.get_mut(key) {
                // The rows of a query were never steps the host took: the list alone knows it
                // held them, so where one stood the host reads the list anew when its holder
                // runs, as it does when another filter is chosen.
                if !list.rows.kept().is_empty() {
                    list.lost = true;
                }
                list.rows.settle();
            }
        }
    }

    /// Whether the boundary `outer` holds the boundary `inner`: itself, or a call it makes.
    fn encloses(&self, outer: BoundId, inner: BoundId) -> bool {
        if outer == inner {
            return true;
        }
        let (Some(outer), Some(inner)) = (self.bound(outer), self.bound(inner)) else {
            return false;
        };
        inner.path.starts_with(&outer.path)
    }

    /// Treat every list as holding all its rows, built: for a host that draws the whole tree,
    /// once it has shown them all.
    pub fn devirtualize(&mut self) {
        self.lists.clear();
    }

    /// Forget the virtual lists the boundary `id` held, and the row `id` was in one.
    pub(super) fn forget_rows_of(&mut self, id: BoundId) {
        self.lists.retain(|_, list| list.holder != id);
        for list in self.lists.values_mut() {
            list.built.retain(|(_, row)| *row != id);
        }
    }

    /// The path of the virtual list `list` in the tree.
    fn virtual_path(&self, list: &str) -> Option<Vec<usize>> {
        let holder = self.lists.get(list)?.holder;
        self.node_path_in(holder, list)
    }

    /// Point the paths of the rows built in `list` (at `at`) at their places among its
    /// children.
    fn place_built(&mut self, list: &str, at: &[usize]) {
        let built: Vec<BoundId> = self
            .lists
            .get(list)
            .map(|rows| rows.built.iter().map(|(_, id)| *id).collect())
            .unwrap_or_default();
        for (place, id) in built.into_iter().enumerate() {
            self.set_index(id, at.len(), place);
        }
    }

    /// Build row `index` of the virtual list `list` from `item`: a boundary of its holder, its
    /// `fragment` put among the rows built, rendered. Returns its key.
    fn build_row(
        &mut self,
        core: &Rc<Core>,
        list: &str,
        (index, item): (usize, Value),
    ) -> Result<Option<String>, RuntimeError> {
        let Some(at) = self.virtual_path(list) else {
            return Ok(None);
        };
        let Some(rows) = self.lists.get(list) else {
            return Ok(None);
        };
        let holder = rows.holder;
        // Where the row stands among the rows of the collection it is a row of, which its body
        // reads the rows around it by.
        let among = rows.rows.coll().map(|coll| (coll, index));
        let (key, args) = rows.template.row(&item, index);
        let fragment = rows.template.fragment(key.clone(), Vec::new());
        let place = rows.built.partition_point(|(other, _)| *other < index);
        // A row the host holds open carries its item's document, built with it.
        let mut stub = rows.template.stub(&key, args);
        stub.open = rows.open.as_deref() == Some(key.as_str());
        let mut bound = Bound::new(stub, true);
        bound.path = [at.as_slice(), &[place]].concat();
        let id = self.add_bound(bound);
        if let Some(b) = self.bound_mut(holder) {
            b.children.push(id);
        }
        if let Some(rows) = self.lists.get_mut(list) {
            rows.built.insert(place, (index, id));
        }
        match self.node_at_mut(&at) {
            Some(node) if place <= node.children.len() => node.children.insert(place, fragment),
            _ => return Ok(None),
        }
        self.place_built(list, &at);
        self.render_row(core, id, among)?;
        Ok(Some(key))
    }

    /// Row `index` of the virtual list `list`, built and rendered now if it was not.
    pub(super) fn show_virtual(
        &mut self,
        core: &Rc<Core>,
        list: &str,
        index: usize,
    ) -> Result<Option<String>, RuntimeError> {
        let Some(rows) = self.lists.get(list) else {
            return Ok(None);
        };
        if let Some((_, id)) = rows.built.iter().find(|(other, _)| *other == index) {
            return Ok(self.bound(*id).map(|b| b.key.clone()));
        }
        if index >= rows.len {
            return Ok(None);
        }
        let Some(item) = rows.rows.item(core, index) else {
            return Ok(None);
        };
        let key = self.build_row(core, list, (index, item));
        self.release(core);
        key
    }

    /// Forget the rows of the virtual list `list` built outside `keep`. Returns the patches
    /// that empty them: for the host, a row not shown is an empty `fragment`.
    pub(super) fn hide_virtual(
        &mut self,
        core: &Core,
        list: &str,
        keep: &std::ops::Range<usize>,
    ) -> Vec<Patch> {
        let Some(at) = self.virtual_path(list) else {
            return Vec::new();
        };
        let Some(rows) = self.lists.get(list) else {
            return Vec::new();
        };
        let holder = rows.holder;
        let gone: Vec<(usize, BoundId)> = rows
            .built
            .iter()
            .enumerate()
            .filter(|(_, (index, _))| !keep.contains(index))
            .map(|(place, (_, id))| (place, *id))
            .collect();
        let mut patches = Vec::new();
        // From the last, so each place is the one the removals before it left.
        for (place, id) in gone.into_iter().rev() {
            let path = [at.as_slice(), &[place]].concat();
            let keys = self.keys_at(&path);
            if let Some(node) = self.node_at_mut(&at)
                && place < node.children.len()
            {
                let fragment = node.children.remove(place);
                for child in fragment.children {
                    patches.push(Patch::Remove {
                        parent: keys.clone(),
                        key: child.key,
                        at: None,
                    });
                }
            }
            self.drop_bound(id);
            if let Some(b) = self.bound_mut(holder) {
                b.children.retain(|child| *child != id);
            }
        }
        self.place_built(list, &at);
        self.release(core);
        patches
    }

    /// Apply one change's steps to the rows of a virtual list: every step is a patch for the
    /// host's rows, and only a row that is built moves in the tree. False when they do not fit.
    pub(super) fn splice_virtual(
        &mut self,
        core: &Rc<Core>,
        step: &Pending,
        patches: &mut Vec<Patch>,
    ) -> Result<bool, RuntimeError> {
        let list = step.rows.parent.as_str();
        let Some(at) = self.virtual_path(list) else {
            return Ok(false);
        };
        if let Some(lingered) = self.linger_in_list(list, step) {
            return Ok(lingered);
        }
        // Only a step the person's rows did keep is one the host takes no step for: one that
        // cannot keep them reads no store and falls through to the host's own steps.
        if self.linger_in_query(core, list, step) == Some(true) {
            return Ok(true);
        }
        let parent = self.keys_at(&at);
        let found;
        let step_splices = match &step.steps {
            Steps::Splices(splices) => splices,
            Steps::Moved(ids) => {
                let Some(steps) = self.moved_in_query(core, list, &step.rows, ids, step.mine)
                else {
                    return Ok(false);
                };
                found = steps;
                &found
            }
        };
        let mut splices = step_splices.iter().peekable();
        while let Some(splice) = splices.next() {
            let patch = match splice {
                // A removal and an insertion of the same item: the row moves, kept as it is.
                Splice::Remove { at: from, item }
                    if splices.peek().is_some_and(
                        |next| matches!(next, Splice::Insert { item: to, .. } if to == item),
                    ) =>
                {
                    let Some(Splice::Insert { at: to, .. }) = splices.next() else {
                        return Ok(false);
                    };
                    let key = step.rows.row_key(item);
                    if !self.move_built(list, &at, (*from, *to), &key) {
                        return Ok(false);
                    }
                    Patch::Move {
                        parent: parent.clone(),
                        key,
                        from: *from,
                        to: *to,
                    }
                }
                Splice::Remove { at: index, item } => {
                    let key = step.rows.row_key(item);
                    if !self.remove_built(list, &at, *index, &key) {
                        return Ok(false);
                    }
                    Patch::Remove {
                        parent: parent.clone(),
                        key,
                        at: Some(*index),
                    }
                }
                Splice::Insert { at: index, item } => {
                    let key = step.rows.row_key(item);
                    let Some(rows) = self.lists.get_mut(list) else {
                        return Ok(false);
                    };
                    let taken = rows.built.iter().any(|(_, id)| {
                        self.bounds
                            .get(*id)
                            .and_then(Option::as_ref)
                            .is_some_and(|b| b.key == key)
                    });
                    if taken || *index > rows.len {
                        return Ok(false);
                    }
                    for (other, _) in &mut rows.built {
                        if *other >= *index {
                            *other = other.saturating_add(1);
                        }
                    }
                    rows.len = rows.len.saturating_add(1);
                    rows.stepped = true;
                    let fragment = rows.template.fragment(key.clone(), Vec::new());
                    let node = if self.lazy_rows {
                        fragment
                    } else {
                        let item = core.item_ref(&step.rows.coll, item);
                        self.build_row(core, list, (*index, item))?;
                        self.find(&key).cloned().unwrap_or(fragment)
                    };
                    Patch::Insert {
                        parent: parent.clone(),
                        index: *index,
                        node,
                    }
                }
            };
            patches.push(patch);
        }
        self.place_built(list, &at);
        Ok(true)
    }

    /// What the steps of `step` do to `list` when its rows are the values its holder made: what
    /// the guest made of them says which items they are, so a row the person's own change took
    /// out of it, or the row standing open, stands where it is — the host's rows do not move and
    /// the holder runs again to keep it — and anyone else's change to other rows makes the rows
    /// new, as a query's steps do. `None` for a list whose rows are a query's.
    fn linger_in_list(&mut self, list: &str, step: &Pending) -> Option<bool> {
        let held = self.lists.get(list)?;
        if !matches!(held.rows, Rows::List { .. }) {
            return None;
        }
        let Steps::Moved(ids) = &step.steps else {
            return Some(false);
        };
        if step.mine && !ids.is_empty() {
            self.mine_items.extend(ids.iter().cloned());
        } else if !self
            .open_value(held)
            .is_some_and(|(_, item)| item_ref_id(&item).is_some_and(|id| ids.contains(id)))
        {
            return Some(false);
        }
        self.dirty.insert(step.holder);
        Some(true)
    }

    /// What a step that only takes rows out of `list`, a query of a collection, does when the
    /// change is the person's, or takes out only the row standing open: every row it takes out
    /// is still in the collection, so each stands where it is, the host's rows untouched, until
    /// the view is left ([`Rows::settle`] settles them). `None` when the rows are a list of
    /// values' or the step is not one that only takes rows out, or is someone else's.
    fn linger_in_query(&mut self, core: &Core, list: &str, step: &Pending) -> Option<bool> {
        let Steps::Splices(splices) = &step.steps else {
            return Some(false);
        };
        if splices.is_empty() {
            return Some(false);
        }
        let held = self.lists.get(list)?;
        let open = held.open_id();
        if !step.mine && splices.iter().any(|splice| Some(splice.item()) != open) {
            return Some(false);
        }
        let Rows::Coll {
            coll,
            listing,
            kept,
        } = &held.rows
        else {
            return Some(false);
        };
        let (coll, listing, kept) = (Rc::clone(coll), Rc::clone(listing), kept.clone());
        if *coll != *step.rows.coll {
            return Some(false);
        }
        let before = Rows::Coll {
            coll: Rc::clone(&coll),
            listing,
            kept: kept.clone(),
        }
        .listing()?;
        let mut keeping = kept;
        for (removed, splice) in splices.iter().enumerate() {
            let Splice::Remove { at, item } = splice else {
                return Some(false);
            };
            if keeps(core, &coll, item) != Some(true)
                || keeping.iter().any(|(_, held)| &**held == item.as_str())
            {
                return Some(false);
            }
            keeping.push((at.saturating_add(removed), Rc::from(item.as_str())));
        }
        keeping.sort_unstable_by_key(|(place, _)| *place);
        let now = core.coll_listing(&step.rows.coll).ok()?;
        let after = Rows::Coll {
            coll: Rc::clone(&coll),
            listing: now,
            kept: keeping,
        };
        let after_listing = after.listing()?;
        if after_listing.len() != before.len() {
            return Some(false);
        }
        for (place, item) in after.kept() {
            if after_listing.position(item) != Some(Some(*place)) {
                return Some(false);
            }
        }
        if let Some(held) = self.lists.get_mut(list) {
            held.rows = after;
        }
        Some(true)
    }

    /// The steps that take the rows of `list`, a query of a collection (filtered or sorted), from
    /// the listing they stand for to the query's listing now, when only the items `ids` came,
    /// went or moved: each found where it was and where it is, which reads a page or two per
    /// item whatever the listing's length. And the rows kept after it: a row the person's own
    /// change took out of the query stands where it is ([`Self::keep_rows`]). `None` when the
    /// rows are not of that query, or a listing cannot say where an item is.
    fn moved_in_query(
        &mut self,
        core: &Core,
        list: &str,
        rows: &crate::render::LoopRows,
        ids: &std::collections::BTreeSet<String>,
        mine: bool,
    ) -> Option<Vec<Splice>> {
        let (coll, before) = match &self.lists.get(list)?.rows {
            Rows::Coll {
                coll,
                listing,
                kept,
            } => (
                Rc::clone(coll),
                Rows::Coll {
                    coll: Rc::clone(coll),
                    listing: Rc::clone(listing),
                    kept: kept.clone(),
                }
                .listing()?,
            ),
            Rows::List { .. } => return None,
        };
        if !Rc::ptr_eq(&coll, &rows.coll) && *coll != *rows.coll {
            return None;
        }
        let now = core.coll_listing(&rows.coll).ok()?;
        let kept = self.keep_rows(core, list, &coll, &before, &now, ids, mine)?;
        let after = Rows::Coll {
            coll,
            kept,
            listing: now,
        };
        let steps = crate::store::moved_steps(
            before.as_ref(),
            after.listing()?.as_ref(),
            ids.iter().map(String::as_str),
        )?;
        // The rows the person's own change took out of the query are kept, and the steps just
        // left them where they are: the list stands for `after` from now on.
        if let Some(held) = self.lists.get_mut(list) {
            held.rows = after;
        }
        Some(steps)
    }

    /// The rows kept after the change of the query `now`: those kept before that the query still
    /// does not hold, and each row whose item `ids` names and that the change took out of the
    /// query, at its place — every one for the person's own change, the row standing open for
    /// anyone's. Any other row someone else's change takes out moves as it ever did; one the
    /// query holds again (an undo) is the query's own row now ([`still_kept`]).
    fn keep_rows(
        &self,
        core: &Core,
        list: &str,
        coll: &CollRef,
        before: &Listing,
        now: &Listing,
        ids: &std::collections::BTreeSet<String>,
        mine: bool,
    ) -> Option<Vec<(usize, Rc<str>)>> {
        let held = self.lists.get(list)?;
        let mut kept = still_kept(core, coll, now, held.rows.kept());
        let open = held.open_id();
        if !mine && open.is_none_or(|open| !ids.contains(open)) {
            return Some(kept);
        }
        // A row the change takes out of the query stands where it stood, whether the host has
        // built it yet or not: the host takes no step for it.
        for id in ids {
            if (!mine && Some(id.as_str()) != open)
                || kept.iter().any(|(_, held)| &**held == id.as_str())
                || now.holds(id) != Some(false)
            {
                continue;
            }
            let Some(Some(place)) = before.position(id) else {
                continue;
            };
            kept.push((place, Rc::from(id.as_str())));
        }
        kept.sort_unstable_by_key(|(place, _)| *place);
        // A row the change took out of the collection itself goes; only what the query left
        // stands where it is.
        Some(still_kept(core, coll, now, &kept))
    }

    /// Move the row keyed `key` from index `from` to `to` among the rows of `list`, its node
    /// too if it is built. False when a row built at `from` is another.
    fn move_built(
        &mut self,
        list: &str,
        at: &[usize],
        (from, to): (usize, usize),
        key: &str,
    ) -> bool {
        let built = self.built_at(list, from);
        if let Some((_, id)) = built
            && self.bound(id).is_none_or(|b| b.key != key)
        {
            return false;
        }
        let Some(rows) = self.lists.get_mut(list) else {
            return false;
        };
        if from >= rows.len || to >= rows.len {
            return false;
        }
        let taken = built.map(|(place, id)| (rows.built.remove(place), place, id));
        for (other, _) in &mut rows.built {
            if *other > from {
                *other = other.saturating_sub(1);
            }
            if *other >= to {
                *other = other.saturating_add(1);
            }
        }
        let Some(((_, id), place, _)) = taken else {
            return true;
        };
        let new_place = rows.built.partition_point(|(other, _)| *other < to);
        rows.built.insert(new_place, (to, id));
        if let Some(node) = self.node_at_mut(at)
            && place < node.children.len()
        {
            let fragment = node.children.remove(place);
            node.children
                .insert(new_place.min(node.children.len()), fragment);
        }
        true
    }

    /// Remove the row keyed `key` at `index` from the rows of `list`, and its boundary if it is
    /// built. False when a row built there is another.
    fn remove_built(&mut self, list: &str, at: &[usize], index: usize, key: &str) -> bool {
        let built = self.built_at(list, index);
        if let Some((_, id)) = built
            && self.bound(id).is_none_or(|b| b.key != key)
        {
            return false;
        }
        let Some(rows) = self.lists.get_mut(list) else {
            return false;
        };
        if index >= rows.len {
            return false;
        }
        rows.len = rows.len.saturating_sub(1);
        rows.stepped = true;
        let holder = rows.holder;
        if let Some((place, id)) = built {
            rows.built.remove(place);
            if let Some(node) = self.node_at_mut(at)
                && place < node.children.len()
            {
                node.children.remove(place);
            }
            self.drop_bound(id);
            if let Some(b) = self.bound_mut(holder) {
                b.children.retain(|child| *child != id);
            }
        }
        if let Some(rows) = self.lists.get_mut(list) {
            for (other, _) in &mut rows.built {
                if *other > index {
                    *other = other.saturating_sub(1);
                }
            }
        }
        true
    }

    /// The place among the rows built of `list` and the boundary of the row at `index`, if it
    /// is built.
    fn built_at(&self, list: &str, index: usize) -> Option<(usize, BoundId)> {
        self.lists.get(list).and_then(|rows| {
            rows.built
                .iter()
                .position(|(other, _)| *other == index)
                .and_then(|place| rows.built.get(place).map(|(_, id)| (place, *id)))
        })
    }

    /// Whether the rows of a virtual list over a collection are its items after their steps:
    /// as many, and each row built where its item is. They then stand for the listing as it is.
    pub(super) fn virtual_fits(&mut self, core: &Core, rows: &crate::render::LoopRows) -> bool {
        let Ok(listing) = core.coll_listing(&rows.coll) else {
            return false;
        };
        let Some(list) = self.lists.get(&rows.parent) else {
            return false;
        };
        // The rows of a list of values are what its holder made of them: they fit it as it
        // stands, and its holder keeps them when it runs again.
        if matches!(list.rows, Rows::List { .. }) {
            return true;
        }
        let now = Rows::Coll {
            coll: Rc::clone(&rows.coll),
            kept: still_kept(core, &rows.coll, &listing, list.rows.kept()),
            listing,
        };
        // A count that is only the most the listing can hold moves with its run's bounds
        // without a row coming or going: the rows fit as their items stand.
        let fits = (list.len == now.len() || !now.len_is_exact())
            && list.built.iter().all(|(index, id)| {
                let key = now.key(&list.template, *index);
                self.bound(*id)
                    .is_some_and(|b| Some(&b.key) == key.as_ref())
            });
        if fits && let Some(list) = self.lists.get_mut(&rows.parent) {
            list.rows = now;
        }
        fits
    }
}

/// The rows of `kept` that stand: those `listing` still does not hold — one the query holds
/// again (an undo) is the query's own row now — and whose items its collection still has, a
/// removed item's row going as it goes.
fn still_kept(
    core: &Core,
    coll: &CollRef,
    listing: &Listing,
    kept: &[(usize, Rc<str>)],
) -> Vec<(usize, Rc<str>)> {
    if kept.is_empty() {
        return Vec::new();
    }
    // The collection's own listing, whatever the query holds of it.
    let plain = Rc::new(CollRef {
        filter: None,
        order: None,
        ..coll.clone()
    });
    let Ok(all) = core.coll_listing(&plain) else {
        return Vec::new();
    };
    kept.iter()
        .filter(|(_, item)| listing.holds(item) == Some(false))
        .filter(|(_, item)| all.holds(item) == Some(true))
        .map(|(place, item)| (*place, Rc::clone(item)))
        .collect()
}

/// Whether the collection of the query `coll` still holds the item `id`: `Some(false)` when it
/// no longer does, `None` when its listing cannot say.
fn keeps(core: &Core, coll: &CollRef, id: &str) -> Option<bool> {
    // The collection's own listing, whatever the query holds of it.
    let plain = Rc::new(CollRef {
        filter: None,
        order: None,
        ..coll.clone()
    });
    let all = core.coll_listing(&plain).ok()?;
    all.holds(id)
}

/// The rows of a list of values the person looking took out of it that still stand: those kept
/// before, those this change named, and the row standing `open`, where they stood in it. They
/// stand there while the list is the one that kept them — its other rows there still, in order —
/// and the items are still there: a row the list holds again is its own row, and one whose item
/// is gone has no row.
fn still_kept_list(
    core: &Core,
    before: &Rows,
    values: &[Value],
    named: &std::collections::BTreeSet<String>,
    open: Option<(usize, Value)>,
) -> Vec<(usize, Value)> {
    let Rows::List { kept: old, .. } = before else {
        return Vec::new();
    };
    if old.is_empty() && named.is_empty() && open.is_none() {
        return Vec::new();
    }
    let mut keep = old.clone();
    if let Some((index, item)) = open
        && keep.iter().all(|(place, _)| *place != index)
    {
        keep.push((index, item));
    }
    if !named.is_empty() {
        for index in 0..before.len() {
            let Some(item) = before.list_row(index) else {
                continue;
            };
            let named = item_ref_id(item).is_some_and(|id| named.contains(id));
            if named && keep.iter().all(|(place, _)| *place != index) {
                keep.push((index, item.clone()));
            }
        }
    }
    let rest = (0..before.len())
        .filter(|index| keep.iter().all(|(place, _)| place != index))
        .filter_map(|index| before.list_row(index).cloned());
    if !stands_in(rest, values) {
        return Vec::new();
    }
    keep.sort_unstable_by_key(|(place, _)| *place);
    keep.into_iter()
        .filter(|(_, item)| !values.contains(item) && still_there(core, item))
        .collect()
}

/// Whether `rest` stands in `values` in that order: the list is the one that kept rows, with
/// some items gone, another's changes to it and all.
fn stands_in(rest: impl Iterator<Item = Value>, values: &[Value]) -> bool {
    let mut values = values.iter();
    rest.into_iter()
        .all(|item| values.any(|there| *there == item))
}

/// Whether the item of a kept row is still there to draw.
fn still_there(core: &Core, item: &Value) -> bool {
    match item {
        Value::Ref(item) => core.store.borrow().type_of(&item.target).is_ok(),
        _ => false,
    }
}

/// Whether `now` is made of another query than `before`: another collection, filter or order,
/// or a collection where there were values or values where there was a collection.
fn another_query(before: &Rows, now: &Rows) -> bool {
    match (before, now) {
        (Rows::Coll { coll: a, .. }, Rows::Coll { coll: b, .. }) => !Rc::ptr_eq(a, b) && a != b,
        (Rows::List { .. }, Rows::List { .. }) => false,
        _ => true,
    }
}

impl Builder<'_> {
    /// The rows the person's own changes took out of the list, and the row standing open, stand
    /// where they are for as long as the view is there, so a list built again keeps them; one
    /// the list holds again is its own row now.
    fn carry_kept(&self, before: Option<&Virtual>, rows: &mut Rows) {
        let Some(before) = before else {
            return;
        };
        match rows {
            Rows::Coll {
                coll,
                listing,
                kept,
            } => {
                let mut held = before.rows.kept().to_vec();
                if let Some(open) = before.open_id()
                    && listing.holds(open) == Some(false)
                    && held.iter().all(|(_, item)| &**item != open)
                    && let Some(place) = before.rows.place(open)
                {
                    held.push((place, Rc::from(open)));
                    held.sort_unstable_by_key(|(place, _)| *place);
                }
                *kept = still_kept(self.core, coll, listing, &held);
            }
            Rows::List { values, kept } => {
                *kept = still_kept_list(
                    self.core,
                    &before.rows,
                    values,
                    &self.live.mine_items,
                    self.live.open_value(before),
                );
            }
        }
    }

    /// The rows of a virtual list as the boundary holding it runs: those built before that still
    /// stand, rendered again if they changed, as the list's children. What became of the rows
    /// is kept for the diff.
    pub(super) fn build_virtual(&mut self, list: VirtualRows) -> Result<Vec<UiNode>, RuntimeError> {
        let VirtualRows {
            list: key,
            rows: template,
            source,
        } = list;
        let mut rows = match source {
            RowSource::Coll(coll) => {
                let listing = self
                    .core
                    .coll_listing(&coll)
                    .map_err(RuntimeError::Package)?;
                Rows::Coll {
                    coll,
                    listing,
                    kept: Vec::new(),
                }
            }
            RowSource::List(values) => Rows::List {
                values,
                kept: Vec::new(),
            },
        };
        let before = self
            .live
            .lists
            .remove(&key)
            .filter(|old| old.holder == self.id && !old.lost)
            .filter(|old| !another_query(&old.rows, &rows));
        self.carry_kept(before.as_ref(), &mut rows);
        let Some(before) = before else {
            // A list new to the tree: the host reads its rows.
            self.live
                .row_changes
                .insert(key.clone(), RowsChange::Replace);
            self.live
                .lists
                .insert(key, Virtual::new(self.id, template, rows, Vec::new(), None));
            return Ok(Vec::new());
        };
        // The row the host holds open stays open across the run: its document is built with the
        // row it belongs to, as the rows built before it are kept.
        let open = before.open;
        // The rows stand as they are when the rows built before hold the same items, and the
        // count is the rows' own: a listing that checks its items as it reads them answers the
        // most it can hold rather than how many, so its count moves with the bounds of its run
        // without a row coming or going. A count that moved is the host's to hear, not a reason
        // to find every row's key again, which reads the listing to its end.
        let same = before.rows.same(&rows, &before.built)
            && (before.len == rows.len() || !rows.len_is_exact())
            && before.template.makes_as(&template);
        let (steps, (built, children)): (Vec<RowStep>, BuiltRows) = if same {
            // Nothing the list shows changed and none of its rows moved.
            (Vec::new(), self.keep_built(&before.built, &template)?)
        } else {
            // Where each row built before is now, by key.
            let old = before.rows.keys(&before.template);
            let new = rows.keys(&template);
            let at: HashMap<&str, usize> = new
                .iter()
                .enumerate()
                .map(|(index, key)| (key.as_str(), index))
                .collect();
            let fragment = |key: &str| template.fragment(key.to_owned(), Vec::new());
            let placed: Vec<(usize, usize)> = before
                .built
                .iter()
                .filter_map(|(index, _)| {
                    let key = old.get(*index)?;
                    at.get(key.as_str()).map(|now| (*index, *now))
                })
                .collect();
            let mut built: Vec<(usize, BoundId)> = Vec::with_capacity(placed.len());
            let mut children: Vec<(usize, UiNode)> = Vec::with_capacity(placed.len());
            for (_, now) in placed {
                let Some(item) = rows.item(self.core, now) else {
                    continue;
                };
                let (row_key, args) = template.row(&item, now);
                let (id, content) = self.keep_row(template.stub(&row_key, args))?;
                built.push((now, id));
                children.push((now, template.fragment(row_key, content)));
            }
            built.sort_unstable_by_key(|(index, _)| *index);
            children.sort_unstable_by_key(|(index, _)| *index);
            (row_steps(&old, &new, &fragment), (built, children))
        };
        self.live
            .row_changes
            .insert(key.clone(), RowsChange::Steps(steps));
        self.live
            .lists
            .insert(key, Virtual::new(self.id, template, rows, built, open));
        Ok(children.into_iter().map(|(_, node)| node).collect())
    }

    /// The rows `before` built, kept: the list's rows are the same and none of them moved, so
    /// each is still a boundary of its holder and its fragment stands under the same key.
    /// Building each one's item reference, key and arguments again would read every shown item
    /// of the list to say nothing about it changed. A row that read what changed runs again.
    fn keep_built(
        &mut self,
        before: &[(usize, BoundId)],
        template: &RowsOf,
    ) -> Result<BuiltRows, RuntimeError> {
        let mut built: Vec<(usize, BoundId)> = Vec::with_capacity(before.len());
        let mut children: Vec<(usize, UiNode)> = Vec::with_capacity(before.len());
        for (index, id) in before {
            // Dropped rows leave the list's `built` (see `forget_rows_of`): a row here is one
            // still in the tree.
            let Some(b) = self.live.bound(*id) else {
                continue;
            };
            let row_key = b.key.clone();
            let fresh = b.rendered && !self.live.dirty.contains(id);
            self.new.push(*id);
            if fresh {
                self.live.reused.insert(row_key.clone());
                children.push((*index, template.fragment(row_key, Vec::new())));
            } else {
                let content = vec![self.live.render(self.core, *id)?];
                children.push((*index, template.fragment(row_key, content)));
            }
            built.push((*index, *id));
        }
        built.sort_unstable_by_key(|(index, _)| *index);
        children.sort_unstable_by_key(|(index, _)| *index);
        Ok((built, children))
    }

    /// A row built before, kept: its subtree as it is when its view, arguments and reads are
    /// unchanged, else rendered again. Returns its boundary and the fragment's children.
    fn keep_row(
        &mut self,
        stub: crate::render::Stub<'_>,
    ) -> Result<(BoundId, Vec<UiNode>), RuntimeError> {
        if let Some(&id) = self.old.get(stub.key) {
            let same = self.live.bound(id).is_some_and(|b| {
                b.target.module == stub.target.module
                    && b.target.export == stub.target.export
                    && b.args == stub.args
            });
            if same {
                self.new.push(id);
                let fresh = self
                    .live
                    .bound(id)
                    .is_some_and(|b| b.rendered && !self.live.dirty.contains(&id));
                if fresh {
                    self.live.reused.insert(stub.key.to_owned());
                    return Ok((id, Vec::new()));
                }
                return Ok((id, vec![self.live.render(self.core, id)?]));
            }
            self.old.remove(stub.key);
            self.live.drop_bound(id);
        }
        let id = self.live.add_bound(Bound::new(stub, true));
        self.new.push(id);
        Ok((id, vec![self.live.render(self.core, id)?]))
    }
}

/// The event a list fires as the person opens a row where it stands, and as it closes.
const OPENED: &str = "opened";
const CLOSED: &str = "closed";

/// Whether `action` puts a value in the view state `state` (`set(opened, x)`).
fn sets(action: &ActionRef, state: &StateKey) -> bool {
    let named =
        |at: usize, text: &str| matches!(action.args.get(at), Some(Value::Text(t)) if t == text);
    action.action == SET_ACTION
        && named(0, &state.window)
        && named(1, state.principal.as_deref().unwrap_or_default())
        && named(2, &state.state)
}

/// The id of the item a row was built for, when it is one resource (`Value::Ref`).
fn item_ref_id(item: &Value) -> Option<&str> {
    match item {
        Value::Ref(item) => item.target.item.as_deref(),
        _ => None,
    }
}
