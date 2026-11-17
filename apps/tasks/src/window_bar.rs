//! The window's slots in its bar ([`ui::window_toolbar`]): the shell's own
//! sidebar toggle and Back, the three places as the bar's views, and the open
//! issue — or the place — as its location ([`ui::framework::WindowBar`]).
//!
//! The window draws no header row and no Back of its own: opening an issue
//! is a place the bar's Back returns from, and the views switch between the
//! list, the board and the threads. Search is the bar's own.

use ui::framework::{Choice, Dispatcher, WindowBar};
use ui::prelude::*;
use ui::widget::{AppModel, ModelPath, NavShell, Stack};

use crate::model::{Kind, tracker};
use crate::{Filter, Nav, Place, Tasks, TasksEffect, Tracker, reference, tr};

/// What shows `place`: the effect the place's own view runs, in the bar or
/// from the palette or an agent alike.
fn show(place: Place) -> TasksEffect {
    match place {
        Place::List => TasksEffect::ShowList,
        Place::Board => TasksEffect::ShowBoard,
        Place::Thread => TasksEffect::ShowThread,
    }
}

/// The window, with its slots in the window's bar.
pub(crate) struct TasksWindow {
    /// The window's page, with the tracker's own dialogs over it.
    page: Stack,
    /// The shell inside that page: the bar's slots come from it, so a
    /// dialog over the page leaves them alone.
    link: ui::widget::WindowBarLink,
    nav: Nav,
    /// The tracker as the window reads it, for the location.
    tasks: LocalState<Tasks>,
    /// The resource's actions, for the bar's own slots.
    dispatcher: Dispatcher<Tracker>,
    /// The issues' chain, whose writes the window's Undo takes back.
    chain: Option<ui::framework::Chain<crate::IssuePart>>,
    /// The bar as it was declared last: the press heard now names one of its
    /// slots, then the window declares the bar for what it shows now.
    bar: WindowBar<Tracker>,
}

impl TasksWindow {
    pub(crate) fn new(
        shell: NavShell,
        bar: WindowBar<Tracker>,
        nav: Nav,
        tasks: LocalState<Tasks>,
        chain: Option<ui::framework::Chain<crate::IssuePart>>,
        dialog: Element,
    ) -> Self {
        let link = shell.window_bar_link();
        let dispatcher = bar.dispatcher();
        let page = Stack::new().push(Box::new(shell)).push(dialog);
        Self {
            page,
            link,
            nav,
            tasks,
            dispatcher,
            chain,
            bar,
        }
    }

    /// The window bar's slots for what the window shows now.
    fn window_bar(&self) -> WindowBar<Tracker> {
        let open = self.nav.open.with(Option::clone);
        let place = self.nav.place.with(|place| *place);
        let chosen = Place::ALL.iter().position(|each| *each == place);
        let (label, section) = self.location(open.as_ref(), place, &self.nav.filter());
        let choices = Place::ALL
            .iter()
            .map(|each| Choice::new(each.title(), &show(*each)));
        let bar = WindowBar::new(self.dispatcher.clone())
            .under(&self.link)
            .label(label)
            .section(section)
            .views(tr("tasks-views"), chosen.unwrap_or_default(), choices);
        // A project's or an area's own page offers, in the bar's menu, what is
        // done to it as a whole. Deleting it is the tracker's own verb: Undo
        // takes it back, so nobody is asked first, as with a deleted note,
        // and a member who may not change the tracker finds the entry
        // disabled.
        let shown = self.nav.area().filter(|_| open.is_none()).and_then(|area| {
            let kind = self
                .tasks
                .with(|tasks| tasks.tracker.area(&area).map(|area| area.kind))?;
            Some((area, kind))
        });
        match shown {
            Some((area, kind)) => bar
                .action(Tracker::DELETE_AREA.with(tracker::DeleteArea { area }))
                .in_menu()
                .named(
                    "trash-2",
                    tr(match kind {
                        Kind::Project => "tasks-project-delete",
                        Kind::Area => "tasks-area-delete",
                    }),
                ),
            None => bar,
        }
    }

    /// Where the window is: the open issue as it reads in a reference, the
    /// open project by its name, else the place, narrowed by the filter when
    /// one is chosen; and whether that is a place of the tracker rather than something in it, which
    /// leaves the window named "Tasks".
    fn location(&self, open: Option<&Id>, place: Place, filter: &Filter) -> (String, bool) {
        if let Some(area) = self.nav.area() {
            let name = self
                .tasks
                .with(|tasks| tasks.tracker.area(&area).map(|area| area.name.clone()));
            if let Some(name) = name {
                return (name, false);
            }
        }
        if let Some(id) = open {
            let shown = self.tasks.with(|tasks| {
                tasks
                    .issues()
                    .get(id)
                    .map(|issue| format!("{} {}", reference(issue.number), issue.title))
            });
            if let Some(shown) = shown {
                return (shown, false);
            }
        }
        let label = match filter {
            Filter::All => place.title(),
            filter => crate::locales::CATALOG.t_args(
                "tasks-location-filtered",
                &[
                    ("place", place.title().into()),
                    ("filter", filter.title().into()),
                ],
            ),
        };
        (label, true)
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

    fn provide<'a>(&'a self, request: &mut ui::widget::capability::Request<'a, '_>) {
        if request.wants::<dyn ui::undo::LocalUndo>() {
            request.provide::<dyn ui::undo::LocalUndo>(self);
            return;
        }
        ui::widget::Widget::provide(&self.page, request);
    }

    fn provide_mut<'a>(&'a mut self, request: &mut ui::widget::capability::RequestMut<'a, '_>) {
        if request.wants::<dyn ui::undo::LocalUndo>() {
            request.provide::<dyn ui::undo::LocalUndo>(self);
            return;
        }
        ui::widget::Widget::provide_mut(&mut self.page, request);
    }
}

/// The window's Undo: what has a history of its own inside it (a field being
/// typed in) first, then the person's last change to an issue, which lives in
/// a part beside the tracker; the tracker's own changes after that.
impl ui::undo::LocalUndo for TasksWindow {
    fn handle_local_undo_request(&mut self, request: ui::undo::UndoRequest) -> bool {
        if ui::undo::handle_local_undo_request(&mut self.page, request) {
            return true;
        }
        let Some(chain) = &self.chain else {
            return false;
        };
        match request {
            ui::undo::UndoRequest::Undo => chain.undo_last(),
            ui::undo::UndoRequest::Redo => chain.redo_last(),
        }
    }
}
