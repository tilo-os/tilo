//! The shell's parts: the element each one is, and every one it owns.

use super::{Column, NavShell, Part, at};
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
            Part::Filter => self.filter.as_ref().map(|f| f as &dyn Widget),
            Part::Toggle => Some(&self.toggle),
            Part::Back => Some(&self.back),
            Part::InspectorToggle => Some(&self.inspector_toggle),
            Part::History(false) => Some(&self.history_open),
            Part::History(true) => Some(&self.history_close),
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
            Part::Filter => self.filter.as_mut().map(|f| f as &mut dyn Widget),
            Part::Toggle => Some(&mut self.toggle),
            Part::Back => Some(&mut self.back),
            Part::InspectorToggle => Some(&mut self.inspector_toggle),
            Part::History(false) => Some(&mut self.history_open),
            Part::History(true) => Some(&mut self.history_close),
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
}
