//! The fields a window reads of its parts only when a view asks: a part
//! type with `#[model(demanded)]` fields (an issue's description and thread)
//! is held without them until a view demands the items that show them, and
//! search reads such parts with every field.

use std::any::Any;

use super::part::live_of;
use super::{Chain, ChainState, Chained};

impl ChainState {
    /// What search finds in part `index`, held as `state`, searched again
    /// only when the part changed ([`super::ChainCore::part_searched`]).
    pub(super) fn searched_part(
        &self,
        index: u64,
        state: &dyn Any,
    ) -> std::rc::Rc<Vec<tilo_resource::SearchEntry>> {
        self.core
            .part_searched(index, &|| self.search_part(index, state))
    }

    /// What search finds in part `index`, held as `state`: read with every
    /// field when the window holds fewer ([`super::PartType::search_all`]).
    fn search_part(&self, index: u64, state: &dyn Any) -> Vec<tilo_resource::SearchEntry> {
        let part = self.part();
        if !part
            .table
            .is_some_and(tilo_resource::table::reads_on_demand)
        {
            return (part.search)(state);
        }
        let mut readers = self.core.search_readers.borrow_mut();
        let reader = readers.entry(index).or_default();
        self.core
            .workspace
            .store
            .read(&self.core.chain.part(index), |doc| {
                (part.search_all)(reader, doc)
            })
            .unwrap_or_default()
    }

    /// Drop the search readers of the parts no longer held.
    pub(super) fn forget_search_readers(&self, held: &[u64]) {
        self.core
            .search_readers
            .borrow_mut()
            .retain(|index, _| held.contains(index));
    }
}

impl<P: Chained> Chain<P> {
    /// Ask part `index` for the parts of it `demand` names (replacing what
    /// was asked of it before), reading those its window's reader left
    /// unread: the items whose `#[model(demanded)]` fields a view shows (an
    /// issue's page, its thread). The part's state holds them at once, for
    /// the view that asked to read next; the chain's other views do not
    /// read the part again, as none of them shows those fields. Whether it
    /// read more.
    pub fn demand(&self, index: u64, demand: tilo_resource::Demand) -> bool
    where
        P::State: Clone,
    {
        let live = self
            .state
            .core
            .parts
            .borrow()
            .get(&index)
            .and_then(|part| live_of::<P>(&**part).cloned());
        let Some(live) = live else {
            return false;
        };
        let read = self
            .state
            .core
            .workspace
            .store
            .read(&self.state.core.chain.part(index), |doc| {
                live.demand(doc, demand)
            });
        match read {
            Some(Ok(read)) => read,
            None => false,
            Some(Err(error)) => {
                tracing::warn!(part = index, %error, "the part did not read what a view asked of it");
                false
            }
        }
    }
}
