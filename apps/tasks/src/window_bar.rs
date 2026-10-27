//! The window's slots in its bar ([`ui::window_toolbar`]): the shell's own
//! sidebar toggle and Back, the two places as the bar's views, and the open
//! issue — or the place — as its location.
//!
//! The window draws no header row and no Back of its own: opening an issue
//! is a place the bar's Back returns from, and the views switch between the
//! list and the board. Search is the bar's own.

use ui::prelude::*;
use ui::widget::{AppModel, ModelPath, NavShell};
use ui::window_toolbar::{ToolbarEvent, ToolbarLocation, ToolbarView, ToolbarViews, WindowToolbar};

use crate::{Filter, Nav, Place, Tracker, reference, tr};

/// The bar action id of a view: `place:` and the place's key.
const PLACE_PREFIX: &str = "place:";

/// The window, with its slots in the window's bar.
pub(crate) struct TasksWindow {
    shell: NavShell,
    nav: Nav,
    /// The tracker as the window reads it, for the location and the views.
    tracker: LocalState<Tracker>,
}

impl TasksWindow {
    pub(crate) fn new(shell: NavShell, nav: Nav, tracker: LocalState<Tracker>) -> Self {
        Self {
            shell,
            nav,
            tracker,
        }
    }

    /// The window bar's slots for what the window shows now.
    fn window_toolbar(&self) -> WindowToolbar {
        let mut toolbar = self.shell.window_toolbar();
        let open = self.nav.open.with(Option::clone);
        let place = self.nav.place.with(|place| *place);
        let filter = self.nav.filter();
        toolbar.location = Some(ToolbarLocation {
            label: self.location(open.as_ref(), place, &filter),
            ..ToolbarLocation::default()
        });
        toolbar.views = Some(ToolbarViews {
            name: tr("tasks-views"),
            choices: Place::ALL
                .into_iter()
                .map(|place| ToolbarView {
                    id: format!("{PLACE_PREFIX}{}", place.key()),
                    label: place.title(),
                })
                .collect(),
            chosen: format!("{PLACE_PREFIX}{}", place.key()),
        });
        toolbar
    }

    /// Where the window is: the open issue as it reads in a reference, else
    /// the place, narrowed by the filter when one is chosen.
    fn location(&self, open: Option<&Id>, place: Place, filter: &Filter) -> String {
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

    /// What the person asked for in the bar: the shell's own slots first,
    /// then a press on one of the views.
    fn take_events(&mut self) -> bool {
        let mut asked = false;
        for event in ui::window_toolbar::take_events() {
            asked = true;
            if self.shell.window_toolbar_event(&event) {
                continue;
            }
            if let ToolbarEvent::Action { id } = event
                && let Some(place) = id.strip_prefix(PLACE_PREFIX).and_then(Place::of)
            {
                self.nav.place.set(place);
                self.nav.open.set(None);
            }
        }
        asked
    }

    fn declare(&self) {
        ui::window_toolbar::declare(Some(self.window_toolbar()));
    }
}

impl ui::widget::Decorator for TasksWindow {
    type Inner = NavShell;

    fn inner(&self) -> &NavShell {
        &self.shell
    }

    fn inner_mut(&mut self) -> &mut NavShell {
        &mut self.shell
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.take_events();
        self.declare();
        self.shell.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let asked = self.take_events();
        self.declare();
        self.shell.update(state, path) || asked
    }

    fn name(&self) -> &'static str {
        "TasksWindow"
    }
}
