//! What the window shows while it has nothing to list
//! ([`NavShell::nothing_listed`]): one screen where the overview, the
//! content and the inspector stand, so a first run or an empty library is
//! a whole window's empty state, never a narrow list's beside a blank pane.
//! The sidebar stays. The app's own columns are held while it shows and
//! come back, split as before, once it is cleared.

use super::{Column, NavShell, Staged, at, at_mut};
use crate::widget::Element;

/// The app's columns, held while the empty state stands in for them.
pub(super) struct Held {
    overview: Option<Element>,
    content: Element,
    inspector: Option<Element>,
}

impl NavShell {
    /// Show `empty` in place of the overview, content and inspector while
    /// there is nothing to list: a first run, an empty library. The sidebar
    /// stays; the columns given before or after come back once
    /// [`Self::set_nothing_listed`] clears it.
    #[must_use]
    pub fn nothing_listed(mut self, empty: Element) -> Self {
        self.set_nothing_listed(Some(empty));
        self
    }

    /// [`Self::nothing_listed`] on a kept shell: `None` brings the app's
    /// columns back.
    pub fn set_nothing_listed(&mut self, empty: Option<Element>) {
        self.pending.empty = Some(Staged(empty));
        self.pending_changed();
    }

    /// Whether the empty state shows at the next build.
    pub(super) fn nothing_listed_shown(&self) -> bool {
        match &self.pending.empty {
            Some(Staged(empty)) => empty.is_some(),
            None => self.held.is_some(),
        }
    }

    /// Whether `column` has a body at the next build, `staged` being a
    /// swap of it waiting.
    pub(super) fn has_body(&self, column: Column, staged: Option<&Staged>) -> bool {
        if column != Column::Sidebar && self.nothing_listed_shown() {
            return column == Column::Content;
        }
        let current = match (&self.held, column) {
            (Some(held), Column::Overview) => held.overview.is_some(),
            (Some(held), Column::Inspector) => held.inspector.is_some(),
            (_, Column::Overview) => self.overview.is_some(),
            (_, Column::Inspector) => self.inspector.is_some(),
            (_, Column::Sidebar) => self.sidebar.is_some(),
            (_, Column::Content) => true,
        };
        staged.map_or(current, Staged::is_some)
    }

    /// The slot a staged body for `column` lands in: the held one while the
    /// empty state stands in for it.
    pub(super) fn body_slot(&mut self, column: Column) -> BodySlot<'_> {
        match (self.held.as_mut(), column) {
            (_, Column::Sidebar) => BodySlot::Optional(&mut self.sidebar),
            (Some(held), Column::Overview) => BodySlot::Optional(&mut held.overview),
            (Some(held), Column::Content) => BodySlot::Required(&mut held.content),
            (Some(held), Column::Inspector) => BodySlot::Optional(&mut held.inspector),
            (None, Column::Overview) => BodySlot::Optional(&mut self.overview),
            (None, Column::Content) => BodySlot::Required(&mut self.content),
            (None, Column::Inspector) => BodySlot::Optional(&mut self.inspector),
        }
    }

    /// Put the staged empty state in place, holding the app's columns, or
    /// bring them back. Only from `apply_pending`.
    pub(super) fn apply_nothing_listed(&mut self, empty: Option<Staged>) {
        match empty {
            Some(Staged(Some(empty))) => match self.held {
                Some(_) => self.content = empty,
                None => {
                    self.held = Some(Held {
                        overview: self.overview.take(),
                        content: std::mem::replace(&mut self.content, empty),
                        inspector: self.inspector.take(),
                    });
                }
            },
            Some(Staged(None)) => {
                if let Some(held) = self.held.take() {
                    self.overview = held.overview;
                    self.content = held.content;
                    self.inspector = held.inspector;
                }
            }
            None => {}
        }
    }

    /// The app's own body of `column` for typed access: the one waiting to
    /// be shown if a swap is pending, else the one shown or held.
    pub(super) fn app_body_mut(
        &mut self,
        column: Column,
    ) -> Option<&mut (dyn crate::widget::Widget + 'static)> {
        if at(&self.pending.bodies, column).is_some() {
            return at_mut(&mut self.pending.bodies, column)
                .as_mut()
                .and_then(|Staged(pending)| pending.as_deref_mut());
        }
        match self.body_slot(column) {
            BodySlot::Optional(slot) => slot.as_deref_mut(),
            BodySlot::Required(slot) => Some(slot.as_mut()),
        }
    }
}

/// Where a body goes: a column that may be absent, or the content.
pub(super) enum BodySlot<'a> {
    Optional(&'a mut Option<Element>),
    Required(&'a mut Element),
}
