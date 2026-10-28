//! Messages' controls, as slots in the window's bar ([`ui::window_toolbar`]):
//! the shell's sidebar toggle and Back; where the window is as the location
//! (the people, the person a conversation is with, the open groups or the
//! account setup); the conversation's route as the location's choices; and
//! the actions: Accounts, Refresh messages, and in the groups Refresh
//! membership and Close groups. The window draws no header row.

use std::cell::RefCell;
use std::rc::Rc;

use ui::LocalState;
use ui::framework::Dispatcher;
use ui::prelude::Id;
use ui::widget::{Element, NavShell, Widget, WindowBarLink};
use ui::window_toolbar::{
    LocationChoice, ToolbarAction, ToolbarEvent, ToolbarLocation, WindowToolbar,
};

use crate::view::{Selection, Sources, choices};
use crate::{CATALOG, Messages, MessagesEffect};

pub(crate) const ACCOUNTS: &str = "messages.accounts";
pub(crate) const REFRESH_HISTORY: &str = "messages.refresh-history";
pub(crate) const REFRESH_GROUP: &str = "messages.group.refresh";
pub(crate) const CLOSE_GROUPS: &str = "messages.groups.close";

/// What the window bar is made of: the window's state, the door its
/// effects go through, and the shell's own slots as the shell last built
/// laid them out.
#[derive(Clone)]
pub(crate) struct Bar {
    pub(crate) sources: Sources,
    pub(crate) selection: LocalState<Selection>,
    pub(crate) history: crate::history::HistoryView,
    pub(crate) setup: crate::setup::SetupView,
    pub(crate) groups: crate::groups::GroupsView,
    pub(crate) dispatcher: Dispatcher<Messages>,
    /// The shell's link; the shell is built again with what it shows.
    pub(crate) link: Rc<RefCell<Option<WindowBarLink>>>,
}

impl Bar {
    /// The window bar's slots for what the window shows now.
    pub(crate) fn window_toolbar(&self) -> WindowToolbar {
        let mut toolbar = self
            .link
            .borrow()
            .as_ref()
            .map(WindowBarLink::slots)
            .unwrap_or_default();
        let selected = self.selection.get();
        let mut actions = Vec::new();
        let location = if let Some((title, refresh)) = self.groups.shown() {
            toolbar.leading.push(
                ToolbarAction::new(
                    CLOSE_GROUPS,
                    "arrow-left",
                    CATALOG.t("messages-close-groups"),
                )
                .navigating(),
            );
            if refresh.is_some() {
                actions.push(ToolbarAction::new(
                    REFRESH_GROUP,
                    "refresh",
                    CATALOG.t("messages-refresh-group-membership"),
                ));
            }
            label(title)
        } else if self.setup.visible() {
            label(CATALOG.t("messages-setup-accounts"))
        } else if let Some(name) = self.person_name(&selected.person) {
            if self.history.refresh(&self.sources, &selected).is_some() {
                actions.push(ToolbarAction::new(
                    REFRESH_HISTORY,
                    "refresh",
                    CATALOG.t("messages-refresh-history"),
                ));
            }
            self.route(name, &selected)
        } else {
            label(CATALOG.t("messages-people"))
        };
        if !self.setup.visible() {
            actions.push(ToolbarAction::new(
                ACCOUNTS,
                "users",
                CATALOG.t("messages-setup-accounts"),
            ));
        }
        toolbar.location = Some(location);
        NavShell::push_window_actions(&mut toolbar, actions);
        toolbar
    }

    fn person_name(&self, person: &str) -> Option<String> {
        if person.is_empty() {
            return None;
        }
        self.sources
            .contacts()
            .people
            .get(&Id::from_raw(person))
            .filter(|person| !person.removed)
            .map(|person| person.label.clone())
    }

    /// The person a conversation is with, with the routes to them as the
    /// location's choices, the one in use marked.
    fn route(&self, name: String, selected: &Selection) -> ToolbarLocation {
        let options = choices(&self.sources, &selected.person);
        let current = options
            .iter()
            .find(|choice| choice.value() == selected.choice);
        let accessible = current.map_or_else(
            || CATALOG.t("messages-route"),
            |choice| {
                CATALOG.t_args(
                    "messages-route-current",
                    &[(
                        "route",
                        ui::i18n::ArgValue::Text(choice.label(&self.sources)),
                    )],
                )
            },
        );
        ToolbarLocation {
            label: name,
            name: accessible,
            choices: options
                .iter()
                .map(|choice| LocationChoice {
                    label: choice.label(&self.sources),
                    value: choice.value(),
                    current: choice.value() == selected.choice,
                    separated: false,
                })
                .collect(),
            ..ToolbarLocation::default()
        }
    }

    /// Run what the person did in the window's bar: the shell's own slots,
    /// then the window's, each through the effect its control in the
    /// window ran. Returns whether anything was asked.
    fn take_events(&self) -> bool {
        let mut asked = false;
        for event in ui::window_toolbar::take_events() {
            asked = true;
            if self
                .link
                .borrow()
                .as_ref()
                .is_some_and(|link| link.event(&event))
            {
                continue;
            }
            let selected = self.selection.get();
            let effect = match event {
                ToolbarEvent::Commit { text } => Some(MessagesEffect::SelectRoute { choice: text }),
                ToolbarEvent::Action { id } => match id.as_str() {
                    ACCOUNTS => Some(MessagesEffect::OpenAccountSetup),
                    REFRESH_HISTORY => self.history.refresh(&self.sources, &selected),
                    REFRESH_GROUP => self.groups.shown().and_then(|(_, refresh)| refresh),
                    CLOSE_GROUPS => Some(MessagesEffect::CloseGroups),
                    _ => None,
                },
                _ => None,
            };
            if let Some(effect) = effect {
                self.dispatcher.effect(&effect);
            }
        }
        asked
    }
}

fn label(label: String) -> ToolbarLocation {
    ToolbarLocation {
        label,
        ..ToolbarLocation::default()
    }
}

/// The window, with its slots in the window's bar.
pub(crate) struct MessagesWindow {
    pub(crate) content: Element,
    pub(crate) bar: Bar,
}

impl ui::widget::Decorator for MessagesWindow {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    fn sync(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) {
        self.bar.take_events();
        ui::window_toolbar::declare(Some(self.bar.window_toolbar()));
        self.content.sync(state, path);
    }

    fn update(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) -> bool {
        let asked = self.bar.take_events();
        ui::window_toolbar::declare(Some(self.bar.window_toolbar()));
        self.content.update(state, path) || asked
    }

    fn name(&self) -> &'static str {
        "MessagesWindow"
    }
}
