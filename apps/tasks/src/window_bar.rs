//! The window's slots in its bar ([`ui::window_toolbar`]): the shell's own
//! sidebar toggle and Back, the four places as the bar's views, and the open
//! issue — or the place — as its location ([`ui::framework::WindowBar`]).
//!
//! The window draws no header row and no Back of its own: opening an issue
//! is a place the bar's Back returns from, and the views switch between the
//! list, the board and the runs. Search is the bar's own.

use ui::framework::{Choice, Dispatcher, WindowBar};
use ui::prelude::*;
use ui::widget::{AppModel, ModelPath, NavShell, Stack};

use crate::{Filter, Nav, Place, TasksEffect, Tracker, reference, tr};

/// What shows `place`: the effect the place's own view runs, in the bar or
/// from the palette or an agent alike.
fn show(place: Place) -> TasksEffect {
    match place {
        Place::List => TasksEffect::ShowList,
        Place::Board => TasksEffect::ShowBoard,
        Place::Tasks => TasksEffect::ShowTasks,
        Place::Thread => TasksEffect::ShowThread,
    }
}

/// The window, with its slots in the window's bar.
pub(crate) struct TasksWindow {
    /// The window's page, with the tracker's own dialogs over it.
    page: Stack,
    /// The shell inside that page: the bar's slots come from it, so a
    /// dialog standing over the page leaves them alone.
    link: ui::widget::WindowBarLink,
    nav: Nav,
    /// The tracker as the window reads it, for the location.
    tracker: LocalState<Tracker>,
    /// The record's actions, for the bar's own slots.
    dispatcher: Dispatcher<Tracker>,
    /// The bar as it was declared last: the press heard now names one of its
    /// slots, then the window declares the bar for what it shows now.
    bar: WindowBar<Tracker>,
}

impl TasksWindow {
    pub(crate) fn new(
        shell: NavShell,
        bar: WindowBar<Tracker>,
        nav: Nav,
        tracker: LocalState<Tracker>,
        dialog: Element,
    ) -> Self {
        let link = shell.window_bar_link();
        let dispatcher = bar.dispatcher();
        let page = Stack::new().push(Box::new(shell)).push(dialog);
        Self {
            page,
            link,
            nav,
            tracker,
            dispatcher,
            bar,
        }
    }

    /// The window bar's slots for what the window shows now.
    fn window_bar(&self) -> WindowBar<Tracker> {
        let open = self.nav.open.with(Option::clone);
        let place = self.nav.place.with(|place| *place);
        let chosen = Place::ALL.iter().position(|each| *each == place);
        let choices = Place::ALL
            .iter()
            .map(|each| Choice::new(each.title(), &show(*each)));
        WindowBar::new(self.dispatcher.clone())
            .under(&self.link)
            .label(self.location(open.as_ref(), place, &self.nav.filter()))
            .views(tr("tasks-views"), chosen.unwrap_or_default(), choices)
    }

    /// Where the window is: the open issue as it reads in a reference, the
    /// open project by its name, else the place, narrowed by the filter when
    /// one is chosen.
    fn location(&self, open: Option<&Id>, place: Place, filter: &Filter) -> String {
        if let Some(area) = self.nav.area() {
            let name = self
                .tracker
                .with(|tracker| tracker.area(&area).map(|area| area.name.clone()));
            if let Some(name) = name {
                return name;
            }
        }
        if let Some(id) = open {
            let shown = self.tracker.with(|tracker| {
                tracker
                    .issues
                    .get(id)
                    .map(|issue| format!("{} {}", reference(issue.number), issue.title))
            });
            if let Some(shown) = shown {
                return shown;
            }
        }
        match filter {
            Filter::All => place.title(),
            filter => ui::i18n::I18nManager::format(
                "tasks-location-filtered",
                &[
                    ("place", place.title().into()),
                    ("filter", filter.title().into()),
                ],
            ),
        }
    }

    fn declare(&mut self) {
        self.bar = self.window_bar();
        self.bar.declare();
    }
}

impl ui::widget::Decorator for TasksWindow {
    type Inner = Stack;

    fn inner(&self) -> &Stack {
        &self.page
    }

    fn inner_mut(&mut self) -> &mut Stack {
        &mut self.page
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.bar.take_events();
        self.declare();
        ui::widget::Widget::sync(&mut self.page, state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let asked = self.bar.take_events();
        self.declare();
        ui::widget::Widget::update(&mut self.page, state, path) || asked
    }

    fn name(&self) -> &'static str {
        "TasksWindow"
    }
}
