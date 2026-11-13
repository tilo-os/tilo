//! The window's slots in its bar ([`ui::window_toolbar`]): the shell's own
//! sidebar toggle and Back, and what the window shows — a place, or the agent
//! whose card stands open — as its location ([`ui::framework::WindowBar`]).
//!
//! The window draws no header row of its own: the title on screen is the one in
//! its bar, and its bar names the window for the desktop too (a tab, the
//! overview and search read the same words).

use ui::framework::{Dispatcher, WindowBar};
use ui::prelude::*;
use ui::widget::WindowBarLink;

use crate::Console;

/// The window, with its slots in the window's bar.
pub(crate) struct ConsoleWindow {
    /// The window's page: the shell, with the sidebar and the place shown.
    page: Element,
    /// The shell inside that page: the bar's slots come from it.
    link: WindowBarLink,
    /// What the window shows, as its bar names it.
    location: String,
    /// The record's actions, for the bar's own slots.
    dispatcher: Dispatcher<Console>,
    /// The bar as it was declared last: the press heard now names one of its
    /// slots, then the window declares the bar for what it shows.
    bar: WindowBar<Console>,
}

impl ConsoleWindow {
    pub(crate) fn new(
        page: Element,
        bar: WindowBar<Console>,
        link: WindowBarLink,
        location: String,
    ) -> Self {
        let dispatcher = bar.dispatcher();
        Self {
            page,
            link,
            location,
            dispatcher,
            bar,
        }
    }

    fn declare(&mut self) {
        self.bar = WindowBar::new(self.dispatcher.clone())
            .under(&self.link)
            .label(self.location.clone());
        self.bar.declare();
    }
}

impl ui::widget::Decorator for ConsoleWindow {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.page.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.page.as_mut()
    }

    fn sync(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) {
        self.bar.take_events();
        self.declare();
        self.page.sync(state, path);
    }

    fn update(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) -> bool {
        let asked = self.bar.take_events();
        self.declare();
        self.page.update(state, path) || asked
    }

    fn name(&self) -> &'static str {
        "ConsoleWindow"
    }
}
