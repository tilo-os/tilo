//! The note's window: the bar's slots, declared where the window is.
//!
//! A toolkit decorator around the view: it says what the window's bar shows —
//! where the window is (the note's opening line, [`crate::opening`]) — and
//! forwards everything else. The bar draws it, so the window draws no header
//! row and carries no title of its own; outside a session the runner draws
//! the same slot, and the search and History controls beside it.

use ui::LocalState;
use ui::prelude::*;
use ui::widget::{AppModel, ModelPath};

pub(crate) struct NoteWindow {
    content: Element,
    /// The note's opening line, as the bar's location names it.
    location: LocalState<String>,
}

impl NoteWindow {
    pub(crate) fn new(content: Element, location: LocalState<String>) -> Self {
        Self { content, location }
    }

    fn declare(&self) {
        ui::window_toolbar::declare(Some(ui::window_toolbar::WindowToolbar {
            location: Some(ui::window_toolbar::ToolbarLocation {
                label: self.location.with(Clone::clone),
                ..ui::window_toolbar::ToolbarLocation::default()
            }),
            ..ui::window_toolbar::WindowToolbar::default()
        }));
    }
}

impl ui::widget::Decorator for NoteWindow {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.declare();
        self.content.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.declare();
        self.content.update(state, path)
    }

    fn name(&self) -> &'static str {
        "NoteWindow"
    }
}
