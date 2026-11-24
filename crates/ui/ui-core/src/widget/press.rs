//! [`Press`] — what pressing a control that stands inside another widget does — a menu row, a
//! swipe, a nav place, a tree node, a card's face.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use crate::component::Event;
use crate::widget::{ActionEmitter, ControlBindingMetadata};

#[derive(Clone, Default)]
pub struct Press {
    /// The action, shared with the command the part runs (a menu row's command runs whenever
    /// it is chosen, and the menu is built anew each time it opens).
    action: Option<Rc<RefCell<ActionEmitter<()>>>>,
    view: Option<crate::component::Intent>,
    /// The address a part that runs nothing leads to, opened as a link opens it
    /// ([`crate::widget::Link`]): a menu row that is a way to a page.
    link: Option<Rc<str>>,
}

impl std::fmt::Debug for Press {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.action.is_some(), self.view.is_some(), &self.link) {
            (true, _, _) => f.write_str("Press::Action(..)"),
            (false, true, _) => f.write_str("Press::View(..)"),
            (false, false, Some(link)) => write!(f, "Press::Link({link})"),
            (false, false, None) => f.write_str("Press::None"),
        }
    }
}

impl Press {
    /// What pressing a part bound to `event` does.
    #[must_use]
    pub fn of(event: Event) -> Self {
        match event {
            Event::None => Self::default(),
            Event::Action(binding) => Self {
                action: Some(Rc::new(RefCell::new(ActionEmitter::new(binding)))),
                view: None,
                link: None,
            },
            Event::View(intent) => Self {
                action: None,
                view: Some(intent),
                link: None,
            },
        }
    }

    /// A part bound to nothing that leads to `link` instead: pressing it opens the address as a
    /// link does. A part that runs an action or reports to its view keeps doing that.
    #[must_use]
    pub fn leading_to(mut self, link: Option<String>) -> Self {
        if self.action.is_none() && self.view.is_none() {
            self.link = link.filter(|link| !link.is_empty()).map(Rc::from);
        }
        self
    }

    #[must_use]
    pub fn is_none(&self) -> bool {
        self.action.is_none() && self.view.is_none() && self.link.is_none()
    }

    #[must_use]
    pub fn is_available(&self) -> bool {
        match (&self.action, &self.view) {
            (Some(action), _) => action.borrow().is_available(),
            (None, Some(_)) => true,
            (None, None) => self.link.is_some(),
        }
    }

    /// The action the part runs, when it runs one: what a menu tells its placed commands from
    /// those the system adds by.
    #[must_use]
    pub fn address(&self) -> Option<tilo_actions::ActionAddress> {
        self.action
            .as_ref()
            .map(|action| action.borrow().binding().address().clone())
    }

    /// What the part does, as a window publishes it ([`tilo_actions::ActionAddress::naming`]): the
    /// action it runs, naming the one record it acts on. A menu row built for one record's action
    /// says so, whichever record's menu it stands in.
    #[must_use]
    pub fn identity(&self) -> Option<tilo_actions::ActionAddress> {
        self.action
            .as_ref()
            .map(|action| action.borrow().binding().identity())
    }

    /// Press it: run the action, or report the event to the view.
    pub fn run(&self) {
        if let Some(action) = &self.action {
            if let Err(error) = action.borrow_mut().emit(()) {
                tracing::warn!(%error, "a part's action refused its arguments");
            }
            return;
        }
        if let Some(view) = &self.view {
            view.fire(None);
            return;
        }
        if let Some(link) = &self.link {
            crate::widget::open_external_url(link);
        }
    }

    #[must_use]
    pub fn runner(&self) -> Rc<dyn Fn()> {
        let press = self.clone();
        Rc::new(move || press.run())
    }

    /// The press as a handler, `None` for a part that does nothing.
    #[must_use]
    pub fn handler(&self) -> Option<Box<dyn FnMut()>> {
        if self.is_none() {
            return None;
        }
        let press = self.clone();
        Some(Box::new(move || press.run()))
    }

    pub(crate) fn take(&self) -> Option<tilo_actions::ControlInvocation> {
        self.action
            .as_ref()
            .and_then(|action| action.borrow_mut().take())
    }

    /// The part as a control of the inventory, `None` for a part that runs no action.
    #[must_use]
    pub fn metadata(&self, control_id: impl Into<String>) -> Option<ControlBindingMetadata> {
        self.action
            .as_ref()
            .map(|action| action.borrow().metadata(control_id))
    }

    pub(crate) fn attach(
        &self,
        domain: &crate::ControlDomain,
        attachment: crate::control_domain::DomainAttachment,
    ) {
        if let Some(action) = &self.action {
            let _ = action.borrow_mut().attach(domain, attachment);
        }
    }

    pub(crate) fn refresh_availability(
        &self,
        allowed: &HashSet<tilo_actions::ActionAddress>,
    ) -> bool {
        let Some(action) = &self.action else {
            return false;
        };
        let mut action = action.borrow_mut();
        let before = action.is_available();
        action.refresh_availability(allowed);
        before != action.is_available()
    }
}
