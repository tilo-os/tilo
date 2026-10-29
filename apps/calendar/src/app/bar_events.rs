//! The calendar's side of its window bar: declaring the slots
//! ([`crate::window_bar`]) and running what the person pressed there.

use super::{Action, CalendarApp};
use crate::model::View;

impl CalendarApp {
    /// Declare the window bar's slots ([`crate::window_bar`]).
    pub(super) fn declare_window_toolbar(&self) {
        // The location names the task list only on the Tasks screen.
        ui::window_toolbar::declare(Some(crate::window_bar::window_toolbar(
            &self.bar_entries,
            self.view,
            self.anchor,
            &self.tasks_source().title(),
            self.sidebar_open(),
        )));
    }

    /// Run what the person did in the window's bar as the calendar's own
    /// actions, as a press on the calendar's own controls runs them: the
    /// view is this window's, chosen at once.
    pub(super) fn apply_window_toolbar(&mut self, doc: &mut ui::widget::Edits<'_>) {
        use crate::window_bar::BarAction;
        for event in ui::window_toolbar::take_events() {
            // The sidebar's toggle is the shell's.
            if self.bar.as_ref().is_some_and(|bar| bar.event(&event)) {
                self.dirty = true;
                continue;
            }
            let Some(action) = self.bar_entries.pressed(&event) else {
                continue;
            };
            let action = match action {
                BarAction::Previous => Action::Prev,
                BarAction::Next => Action::Next,
                BarAction::Today => Action::Today,
                BarAction::New if self.view == View::Tasks => Action::NewTask,
                BarAction::New => Action::NewEvent,
                BarAction::QuickAdd => Action::QuickAdd,
                BarAction::Shortcuts => Action::Shortcuts,
                BarAction::Export => Action::Export,
                BarAction::Import => Action::Import,
                BarAction::View(view) => Action::View(view),
            };
            self.apply_action(action, doc);
            self.dirty = true;
        }
        self.declare_window_toolbar();
    }
}
