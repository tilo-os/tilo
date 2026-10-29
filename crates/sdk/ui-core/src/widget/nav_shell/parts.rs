//! The shell's parts: the element each one is, and every one it owns.

use super::{Column, HistoryControl, INERT_PART, NavShell, Part, at};
use crate::widget::Widget;

impl NavShell {
    pub(super) fn part(&self, part: Part) -> Option<&dyn Widget> {
        match part {
            Part::Body(Column::Sidebar) => self.sidebar.as_deref(),
            Part::Body(Column::Overview) => self.overview.as_deref(),
            Part::Body(Column::Content) => Some(self.content.as_ref()),
            Part::Body(Column::Inspector) => self.inspector.as_deref(),
            Part::Rail => self.rail.as_deref(),
            Part::Title(column) => self
                .headers
                .get(column.index())
                .map(|header| &header.title as &dyn Widget),
            Part::Leading(column) => self
                .headers
                .get(column.index())
                .and_then(|header| header.leading.as_deref()),
            Part::Action(column, i) => self
                .headers
                .get(column.index())
                .and_then(|header| header.actions.get(i))
                .map(AsRef::as_ref),
            Part::More(column) => self
                .headers
                .get(column.index())
                .map(|header| &header.more as &dyn Widget),
            Part::Controls => self.controls.as_deref(),
            Part::Filter => self.filter.as_deref().map(|f| f as &dyn Widget),
            Part::Toggle => Some(&*self.toggle),
            Part::Back => Some(&*self.back),
            Part::InspectorToggle => Some(&*self.inspector_toggle),
            Part::History(false) => Some(&*self.history_open),
            Part::History(true) => Some(&*self.history_close),
            Part::StatusAction => self.status_action.as_ref().map(|b| b as &dyn Widget),
            Part::Band => self.band.as_deref(),
        }
    }

    pub(super) fn part_mut(&mut self, part: Part) -> Option<&mut (dyn Widget + 'static)> {
        match part {
            Part::Body(Column::Sidebar) => self.sidebar.as_deref_mut(),
            Part::Body(Column::Overview) => self.overview.as_deref_mut(),
            Part::Body(Column::Content) => Some(self.content.as_mut()),
            Part::Body(Column::Inspector) => self.inspector.as_deref_mut(),
            Part::Rail => self.rail.as_deref_mut(),
            Part::Title(column) => self
                .headers
                .get_mut(column.index())
                .map(|header| &mut header.title as &mut dyn Widget),
            Part::Leading(column) => self
                .headers
                .get_mut(column.index())
                .and_then(|header| header.leading.as_deref_mut()),
            Part::Action(column, i) => self
                .headers
                .get_mut(column.index())
                .and_then(|header| header.actions.get_mut(i))
                .map(AsMut::as_mut),
            Part::More(column) => self
                .headers
                .get_mut(column.index())
                .map(|header| &mut header.more as &mut dyn Widget),
            Part::Controls => self.controls.as_deref_mut(),
            Part::Filter => self.filter.as_deref_mut().map(|f| f as &mut dyn Widget),
            Part::Toggle => Some(&mut *self.toggle),
            Part::Back => Some(&mut *self.back),
            Part::InspectorToggle => Some(&mut *self.inspector_toggle),
            Part::History(false) => Some(&mut *self.history_open),
            Part::History(true) => Some(&mut *self.history_close),
            Part::StatusAction => self
                .status_action
                .as_mut()
                .map(|b| b as &mut (dyn Widget + 'static)),
            Part::Band => self.band.as_deref_mut(),
        }
    }

    /// Every element the shell owns, shown or not: sync, update and wake
    /// reach parts the shell is not showing.
    pub(super) fn all_parts(&self) -> Vec<Part> {
        let mut parts = vec![
            Part::Body(Column::Overview),
            Part::Body(Column::Content),
            Part::Body(Column::Inspector),
            Part::Body(Column::Sidebar),
            Part::Rail,
        ];
        for column in [
            Column::Overview,
            Column::Content,
            Column::Inspector,
            Column::Sidebar,
        ] {
            let header = at(&self.headers, column);
            if header.leading.is_some() {
                parts.push(Part::Leading(column));
            }
            for i in 0..header.actions.len() {
                parts.push(Part::Action(column, i));
            }
            if !header.actions.is_empty() {
                parts.push(Part::More(column));
            }
        }
        parts.extend([
            Part::Controls,
            Part::Filter,
            Part::Toggle,
            Part::Back,
            Part::InspectorToggle,
            Part::History(false),
            Part::History(true),
            Part::StatusAction,
            Part::Band,
        ]);
        parts
    }

    /// What the host meets for `part`: the part, or a stand-in while its
    /// push screen is not in front, so a walk that pairs children with the
    /// tree's nodes never reaches an inert column.
    pub(super) fn exposed(&self, part: Part) -> Option<&dyn Widget> {
        if self.part_inert(part) {
            return Some(&INERT_PART);
        }
        self.part(part)
    }

    /// The parts the host walks: those in the host tree once it is built,
    /// in its order (the host pairs them with the tree's nodes), and every
    /// owned part before the first build, when there is no tree for their
    /// order to disagree with. A part of a screen not in front answers as an
    /// [`InertPart`]; an action in the More menu stays, so its door is still
    /// published.
    pub(super) fn exposed_parts(&self) -> Vec<Part> {
        if self.built {
            self.host_parts.clone()
        } else {
            let history = self.inputs.borrow().history.map(HistoryControl::is_open);
            self.all_parts()
                .into_iter()
                .filter(|part| self.part(*part).is_some())
                .filter(|part| match part {
                    Part::Toggle => self.has_sidebar(),
                    Part::InspectorToggle => self.inspector.is_some(),
                    Part::Back | Part::More(_) => false,
                    Part::History(open) => history == Some(*open),
                    _ => true,
                })
                .collect()
        }
    }
}
