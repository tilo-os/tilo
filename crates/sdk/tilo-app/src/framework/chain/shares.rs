//! A verb over items of several parts, written as one step.

use tilo_actions::{ActionError, ActionInvoker};

use std::any::Any;

use super::{Chain, ChainState, Chained, Opened, PartEdit, Write};

/// The parts one step wrote into, in the order written: one, or each part a
/// verb over items of several parts changed ([`Chained::split`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Written(pub(super) Vec<u64>);

impl Written {
    pub(super) fn one(index: u64) -> Self {
        Self(vec![index])
    }

    /// The part written last: what a caller asking for one part is told.
    pub(super) fn last(&self) -> u64 {
        self.0.last().copied().unwrap_or(0)
    }
}

impl ChainState {
    /// Write the shares of a verb over items of several parts
    /// ([`Chained::split`]), each into its part, as one step. Nothing is
    /// written while a part a share changes is on its way.
    pub(super) fn write_shares(
        &self,
        mut shares: Vec<(u64, Box<dyn Any>)>,
        invoker: &ActionInvoker,
        place: Option<&str>,
    ) -> Result<Write, ActionError> {
        shares.sort_by_key(|(index, _)| *index);
        for (_, share) in &shares {
            self.may(&**share)?;
        }
        let head = self.core.parts.borrow().keys().next_back().copied();
        for (index, _) in &shares {
            match self
                .core
                .open_or_start(*index)
                .map_err(|error| self.core.refused(error))?
            {
                Opened::Held | Opened::Started => {}
                Opened::Asked => return Ok(Write::Arriving),
            }
        }
        let mut written = Vec::with_capacity(shares.len());
        let mut created = Vec::new();
        for (index, share) in shares {
            // A part this write starts carries what the one before it, held
            // or written just now, says of the parts before.
            if head.is_some_and(|head| index > head)
                && let Some(previous) = index.checked_sub(1)
            {
                self.carry(previous, index, invoker)?;
            }
            let (share, encoded) = (self.part().written)(&*share)?;
            created.extend(self.write(index, share, encoded, invoker, place.map(str::to_owned))?);
            written.push(index);
        }
        if written.is_empty() {
            return Err(ActionError::Refused(
                "the action changes no part of the chain".into(),
            ));
        }
        Ok(Write::Done(Written(written), created))
    }

    /// Remember `written` as a step the window's person may take back with
    /// [`Chain::undo_last`], when it is theirs and a step at all: an agent's
    /// step is its own to take back, and housekeeping or content from outside
    /// is nobody's.
    pub(super) fn wrote(&self, invoker: &ActionInvoker, written: Written, step: bool) {
        if step && super::super::resources::acts_for(invoker, &self.core.workspace.me) {
            self.written.borrow_mut().push(written);
            self.undone.borrow_mut().clear();
        }
    }
}

impl<P: Chained> Chain<P> {
    /// Take back the person's newest step in the part they last wrote into
    /// through this window, as the window's Undo does when that write is
    /// newer than any in the app's own resource. Whether there was one.
    pub fn undo_last(&self) -> bool {
        let Some(written) = self.state.written.borrow_mut().pop() else {
            return false;
        };
        // Newest first, so a step over several parts is taken back whole.
        let mut undone = false;
        for index in written.0.iter().rev() {
            undone |= self.undo_in(*index).is_ok();
        }
        if undone {
            self.state.undone.borrow_mut().push(written);
        }
        undone
    }

    /// Bring back the step [`Self::undo_last`] took back last. Whether there
    /// was one.
    pub fn redo_last(&self) -> bool {
        let Some(undone) = self.state.undone.borrow_mut().pop() else {
            return false;
        };
        let me = self.dispatch.me().unwrap_or(&self.state.core.workspace.me);
        let mut redone = false;
        for index in &undone.0 {
            let edit = PartEdit {
                part: *index,
                edit: super::super::history::HistoryEdit::Redo {
                    steps: None,
                    undos: None,
                },
            };
            redone |= self
                .state
                .apply_history(&edit, &ActionInvoker::member(me.clone()))
                .is_ok();
        }
        if redone {
            self.state.written.borrow_mut().push(undone);
        }
        redone
    }
}
