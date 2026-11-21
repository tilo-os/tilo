//! The menu a right click, the Menu key or Shift+F10 opens on an event: open it, then every
//! action the event's resource offers, the same ones the palette and an agent find on it.

use ui::widget::{ContextMenuArea, Element, MenuEntry};
use vello::kurbo::Point;

use super::{Action, CalendarApp, Id, Sel, store::Ask};

/// What a row of an event's menu asks of the window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EventAction {
    /// The event's resource and the action it publishes under its id.
    pub address: tilo_actions::ActionAddress,
    /// What the event is called, which a palette asking for the action's values names.
    pub label: String,
    /// Whether it asks for values first: then the palette opens on it to ask.
    pub asks: bool,
}

/// `calendar` with the menu of the event under the pointer, or of the event chosen when the
/// keyboard opens it.
pub(crate) fn with_event_menu(calendar: CalendarApp) -> Element {
    Box::new(ContextMenuArea::over_content(
        Box::new(calendar),
        |content, at| {
            content
                .as_any()
                .and_then(|any| any.downcast_ref::<CalendarApp>())
                .map(|calendar| calendar.event_menu(at))
                .unwrap_or_default()
        },
    ))
}

impl CalendarApp {
    /// The event a menu opened at `at` is about: the one drawn there, or the one chosen when the
    /// keyboard opened it.
    fn event_for_menu(&self, at: Option<Point>) -> Option<Id> {
        match at {
            Some(at) => match self.hit_action_at(at)? {
                Action::OpenEvent(id) => Some(id),
                _ => None,
            },
            None => match self.selection? {
                Sel::Event(id) => Some(id),
                Sel::Task(_) => None,
            },
        }
    }

    /// The menu of the event at `at`: open it, then each action its resource offers now.
    pub(crate) fn event_menu(&self, at: Option<Point>) -> Vec<MenuEntry> {
        let Some(id) = self.event_for_menu(at) else {
            return Vec::new();
        };
        let resource = crate::actions::event_resource_id(id);
        let Some(published) =
            <crate::model::Calendar as ui::tilo_resource::Resource>::child(&self.model, &resource)
        else {
            return Vec::new();
        };
        let store = self.shared();
        let opens = std::rc::Rc::clone(&store);
        let mut entries = vec![MenuEntry::new(
            crate::resource::t("calendar-event-open"),
            move || {
                opens.borrow_mut().ask(Ask::OpenItem(resource.clone()));
            },
        )];
        let actions: Vec<_> = published
            .actions
            .iter()
            .filter(|spec| {
                spec.contract.offered_to_people() && spec.contract.availability.is_available()
            })
            .collect();
        if !actions.is_empty() {
            entries.push(MenuEntry::separator());
        }
        for spec in actions {
            // The address the resource publishes: an action it stopped
            // publishing offers no row rather than an address of our own.
            let Some(address) = published.action_address(&spec.id) else {
                continue;
            };
            let act = EventAction {
                address,
                label: published.label.clone(),
                asks: spec.params().iter().any(|param| param.required),
            };
            let store = std::rc::Rc::clone(&store);
            let choose = move || store.borrow_mut().ask(Ask::Act(act.clone()));
            let label = if spec.params().iter().any(|param| param.required) {
                format!("{}…", spec.label)
            } else {
                spec.label.clone()
            };
            entries.push(
                if spec.contract.risk == tilo_actions::RiskLevel::Destructive {
                    MenuEntry::destructive(label, choose)
                } else {
                    MenuEntry::new(label, choose)
                },
            );
        }
        entries
    }

    /// Carry out a row of an event's menu: at once when it asks for nothing, else in the palette
    /// that asks for what it needs.
    pub(super) fn act_on_event(&mut self, act: EventAction) {
        if act.asks {
            self.search_opener.open(ui::search::SearchRequest::asking(
                ui::search::ScopeDecl::new(act.address.resource, act.label),
                act.address.action,
            ));
        } else {
            self.menu_invocations
                .push_back(tilo_actions::ControlInvocation {
                    address: act.address,
                    arguments: serde_json::Map::new(),
                });
        }
        self.dirty = true;
    }
}
