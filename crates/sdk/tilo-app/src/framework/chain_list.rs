//! Reading back through a chain's parts ([`Chain`]): the list view of
//! anything an app keeps as a chain, a chat's messages or a log.
//!
//! A device holds a chain's head, and whatever earlier parts someone read
//! back to. A view of the chain opens the part before the oldest it holds
//! when its reader comes to the top of what it shows, one part at a time:
//! the next only once the last one opened is here. A part this device has
//! none of is started from its genesis and arrives from the room
//! ([`Chain::hold`]); until it has, the view reserves the place its items
//! will take and shows them arriving ([`ChainPager::asked`]). The view keeps
//! the item the reader looks at where it is while earlier ones come in
//! above it.
//!
//! Two more reasons to read back, both the app's to give:
//!
//! - **An item looked for** that no part held here has (a link to an old
//!   message): [`ChainPager::seek`] opens earlier parts in turn until one
//!   has it, or the chain's first part did not.
//! - **Parts something else reads** (the unread counts of a reader who was
//!   away while parts filled): [`ChainPager::reach_back_to`] holds the
//!   parts back to one.
//!
//! [`ChainPager`] is the mechanism, for a view that paints its own rows;
//! [`ChainList`] is the same as a [`ListModel`] for a [`VirtualList`].
//! Nothing here polls: the chain's [`Chain::revision`] moves, and the
//! store wakes the window ([`ui_core::record::RecordStore::wake_on_arrival`]),
//! when a part's history arrives, and opening a part waits for a pass where
//! the app's records take writes ([`ChainPager::settle`]).
//!
//! [`VirtualList`]: ui_core::widget::virtual_list::VirtualList

use std::collections::HashMap;
use std::hash::Hash;
use std::ops::Range;
use std::rc::Rc;

use tilo_resource::Resource;
use ui_core::arriving::{Arrival, ArrivalClock, Source};
use ui_core::widget::virtual_list::{ListModel, ListModelChange};

use super::chain::{Chain, Chained};

/// What a settle pass of a [`ChainPager`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Paged {
    /// Nothing to open, or not now.
    Nothing,
    /// Opened part `n`: held from this device, or on its way from the room.
    Opened(u64),
    /// A held part has the item [`ChainPager::seek`] looked for.
    Found,
    /// Every part is held and none has the item looked for.
    NotFound,
}

/// A predicate over one part: whether it holds what is looked for.
type Seek<P> = Box<dyn Fn(&<P as Resource>::State) -> bool>;

/// Where a view stands reading back through a chain. See the
/// [module docs](self).
pub struct ChainPager<P: Chained> {
    chain: Chain<P>,
    /// The reader came to the top of what is held.
    wanted: bool,
    /// Hold every part from this one on.
    reach: Option<u64>,
    seeking: Option<Seek<P>>,
    /// When the part on its way was asked for, from the room.
    asked: Option<ArrivalClock>,
}

impl<P: Chained> ChainPager<P> {
    #[must_use]
    pub fn new(chain: Chain<P>) -> Self {
        Self {
            chain,
            wanted: false,
            reach: None,
            seeking: None,
            asked: None,
        }
    }

    #[must_use]
    pub fn chain(&self) -> &Chain<P> {
        &self.chain
    }

    /// The oldest part held; `None` while nothing was written to the chain.
    #[must_use]
    pub fn oldest(&self) -> Option<u64> {
        self.chain.parts().first().map(|(index, _)| *index)
    }

    /// Whether the chain has parts before the oldest held.
    #[must_use]
    pub fn has_earlier(&self) -> bool {
        self.oldest().is_some_and(|oldest| oldest > 0)
    }

    /// Whether a held part is still on its way from the room.
    #[must_use]
    pub fn arriving(&self) -> bool {
        self.chain
            .parts()
            .iter()
            .any(|(index, _)| self.chain.arriving(*index))
    }

    /// Whether the view shows earlier items as still to come above what it
    /// holds: there are earlier parts, or one is on its way.
    #[must_use]
    pub fn loading(&self) -> bool {
        self.has_earlier() || self.arriving()
    }

    /// When the part on its way was asked for, and from where: what a
    /// view's placeholder for it steps through
    /// ([`ui_core::widget::ArrivalFace`]). `None` while nothing is on its
    /// way.
    #[must_use]
    pub fn asked(&self) -> Option<&ArrivalClock> {
        self.asked.as_ref().filter(|_| self.arriving())
    }

    /// The reader came to the top of what is held: the next settle pass
    /// opens the part before.
    pub fn reached_top(&mut self) {
        if self.has_earlier() && !self.arriving() {
            self.wanted = true;
        }
    }

    /// Hold every part from `part` on (and keep holding them), one per
    /// settle pass.
    pub fn reach_back_to(&mut self, part: u64) {
        self.reach = Some(self.reach.map_or(part, |reach| reach.min(part)));
    }

    /// Look for the item `found` names in the parts held, and open earlier
    /// ones in turn until one has it: a settle pass answers [`Paged::Found`]
    /// or [`Paged::NotFound`].
    pub fn seek(&mut self, found: impl Fn(&P::State) -> bool + 'static) {
        self.seeking = Some(Box::new(found));
    }

    /// Whether an item is looked for.
    #[must_use]
    pub fn seeking(&self) -> bool {
        self.seeking.is_some()
    }

    /// Whether a settle pass has something to do now: a view asks for one
    /// (a frame) when this turns true.
    #[must_use]
    pub fn wants_settle(&self) -> bool {
        if self.seeking.is_some() {
            return !self.arriving();
        }
        let Some(oldest) = self.oldest().filter(|oldest| *oldest > 0) else {
            return false;
        };
        !self.arriving() && (self.wanted || self.reach.is_some_and(|reach| reach < oldest))
    }

    /// Open the part before the oldest held when the reader reached the top,
    /// an item is looked for or the parts reached back to are not all held;
    /// one at a time, once the last one opened arrived. Run where the app's
    /// records take writes (a settle pass); anywhere else it waits.
    pub fn settle(&mut self) -> Paged {
        if !self.chain.takes_changes() {
            return Paged::Nothing;
        }
        let parts = self.chain.parts();
        if let Some(found) = &self.seeking
            && parts.iter().any(|(_, part)| found(part))
        {
            self.seeking = None;
            return Paged::Found;
        }
        if parts.iter().any(|(index, _)| self.chain.arriving(*index)) {
            return Paged::Nothing;
        }
        let Some(oldest) = parts.first().map(|(index, _)| *index) else {
            return Paged::Nothing;
        };
        if oldest == 0 {
            self.wanted = false;
            if self.seeking.take().is_some() {
                return Paged::NotFound;
            }
            return Paged::Nothing;
        }
        let reach = self.reach.is_some_and(|reach| reach < oldest);
        if !(self.wanted || self.seeking.is_some() || reach) {
            return Paged::Nothing;
        }
        self.wanted = false;
        let earlier = oldest.saturating_sub(1);
        match self.chain.hold(earlier) {
            Ok(()) => {
                if self.chain.arriving(earlier) {
                    self.asked = Some(ArrivalClock::new(ui_core::time::now(), Source::host("")));
                }
                Paged::Opened(earlier)
            }
            Err(error) => {
                tracing::warn!(kind = P::KIND, part = earlier, %error, "earlier part not opened");
                self.seeking = None;
                Paged::Nothing
            }
        }
    }
}

/// A row of a [`ChainList`]: the place of the items still to come from
/// earlier parts, or an item.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ChainRow<K> {
    Earlier,
    Item(K),
}

/// The items of the parts held, oldest first, each with its key.
type Rows<P, K, T> = Box<dyn Fn(&[(u64, Rc<<P as Resource>::State>)]) -> Vec<(K, T)>>;

/// A chain's items as a [`ListModel`]: every held part's items, oldest
/// first, after a row standing for the earlier parts while there are any
/// ([`ChainRow::Earlier`], pending while one is on its way). Showing that
/// row opens the part before ([`ChainPager::reached_top`]); a
/// [`VirtualList`](ui_core::widget::virtual_list::VirtualList) keeps the
/// row the reader looks at in place as items come in above it.
pub struct ChainList<P: Chained, K, T> {
    pager: ChainPager<P>,
    rows: Rows<P, K, T>,
    items: Vec<(K, T)>,
    index: HashMap<K, usize>,
    earlier: bool,
    seen: Option<u64>,
}

impl<P: Chained, K: Clone + Eq + Hash + 'static, T> ChainList<P, K, T> {
    /// The items `rows` reads from the parts `chain` holds.
    #[must_use]
    pub fn new(
        chain: Chain<P>,
        rows: impl Fn(&[(u64, Rc<P::State>)]) -> Vec<(K, T)> + 'static,
    ) -> Self {
        let mut list = Self {
            pager: ChainPager::new(chain),
            rows: Box::new(rows),
            items: Vec::new(),
            index: HashMap::new(),
            earlier: false,
            seen: None,
        };
        list.read();
        list
    }

    #[must_use]
    pub fn pager(&mut self) -> &mut ChainPager<P> {
        &mut self.pager
    }

    /// The item of row `index`; `None` for the earlier parts' row.
    #[must_use]
    pub fn item(&self, index: usize) -> Option<&T> {
        let index = index.checked_sub(usize::from(self.earlier))?;
        self.items.get(index).map(|(_, item)| item)
    }

    /// Read the items again when the chain moved. Whether it did.
    fn read(&mut self) -> bool {
        let revision = self.pager.chain().revision();
        if self.seen == Some(revision) {
            return false;
        }
        self.seen = Some(revision);
        self.items = (self.rows)(&self.pager.chain().parts());
        self.index = self
            .items
            .iter()
            .enumerate()
            .map(|(at, (key, _))| (key.clone(), at))
            .collect();
        self.earlier = self.pager.loading();
        true
    }
}

impl<P: Chained, K: Clone + Eq + Hash + 'static, T> ListModel for ChainList<P, K, T> {
    type Key = ChainRow<K>;

    fn len(&self) -> usize {
        self.items.len().saturating_add(usize::from(self.earlier))
    }

    fn key_at(&self, index: usize) -> Option<Self::Key> {
        if self.earlier && index == 0 {
            return Some(ChainRow::Earlier);
        }
        let index = index.checked_sub(usize::from(self.earlier))?;
        self.items
            .get(index)
            .map(|(key, _)| ChainRow::Item(key.clone()))
    }

    fn index_of(&self, key: &Self::Key) -> Option<usize> {
        match key {
            ChainRow::Earlier => self.earlier.then_some(0),
            ChainRow::Item(key) => self
                .index
                .get(key)
                .map(|at| at.saturating_add(usize::from(self.earlier))),
        }
    }

    fn row_state(&self, index: usize) -> Arrival<(), &str> {
        if self.earlier && index == 0 {
            let from = self
                .pager
                .asked()
                .map_or_else(|| Source::host(""), |asked| asked.source().clone());
            return Arrival::Pending { from };
        }
        Arrival::Ready(())
    }

    fn request_range(&mut self, range: Range<usize>) {
        if self.earlier && range.start == 0 {
            self.pager.reached_top();
            // Here when the app's records take writes; otherwise at the
            // next pass that reads the list.
            self.pager.settle();
        }
    }

    fn poll_changed(&mut self) -> bool {
        if self.pager.wants_settle() {
            self.pager.settle();
        }
        self.read()
    }

    fn poll_changes(&mut self) -> Vec<ListModelChange<Self::Key>> {
        Vec::new()
    }
}
