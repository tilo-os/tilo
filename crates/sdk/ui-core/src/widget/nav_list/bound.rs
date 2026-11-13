//! A navigation list's gestures as published actions: a row's buttons, a section header's
//! button, renaming, deleting, dragging and checking a row each run an action the app binds,
//! with the row or section the gesture was on.

use std::collections::{HashMap, HashSet};

use tilo_actions::{ActionAddress, ActionBinding, ControlInvocation};

use super::{NavAction, NavDrop, NavList, NavModel, NavShared};
use crate::widget::{ActionEmitter, ControlBindingMetadata};

/// The emitters a list's bound gestures queue into. They live as long as the
/// list, so new sections keep them and the host's custody of them.
#[derive(Default)]
pub(super) struct NavBindings {
    pub(super) items: HashMap<String, ActionEmitter<String>>,
    pub(super) sections: HashMap<String, ActionEmitter<String>>,
    pub(super) rename: Option<ActionEmitter<(String, String)>>,
    pub(super) delete: Option<ActionEmitter<String>>,
    pub(super) moves: Option<ActionEmitter<(String, NavDrop)>>,
    pub(super) check: Option<ActionEmitter<(String, bool)>>,
}

/// Queue `input` on `emitter`; an input the action's parameters refuse is
/// logged, never sent.
pub(super) fn emit<I>(emitter: &mut ActionEmitter<I>, input: I) {
    if let Err(error) = emitter.emit(input) {
        tracing::warn!(%error, "navigation list action refused its input");
    }
}

impl NavBindings {
    fn singles(&self) -> [Option<&dyn Producer>; 4] {
        [
            self.rename.as_ref().map(|e| e as &dyn Producer),
            self.delete.as_ref().map(|e| e as &dyn Producer),
            self.moves.as_ref().map(|e| e as &dyn Producer),
            self.check.as_ref().map(|e| e as &dyn Producer),
        ]
    }

    fn producers_mut(&mut self) -> Vec<&mut dyn Producer> {
        let mut all: Vec<&mut dyn Producer> = Vec::new();
        let Self {
            items,
            sections,
            rename,
            delete,
            moves,
            check,
        } = self;
        all.extend(items.values_mut().map(|e| e as &mut dyn Producer));
        all.extend(sections.values_mut().map(|e| e as &mut dyn Producer));
        all.extend(rename.as_mut().map(|e| e as &mut dyn Producer));
        all.extend(delete.as_mut().map(|e| e as &mut dyn Producer));
        all.extend(moves.as_mut().map(|e| e as &mut dyn Producer));
        all.extend(check.as_mut().map(|e| e as &mut dyn Producer));
        all
    }

    pub(super) fn is_bound(&self) -> bool {
        !self.items.is_empty()
            || !self.sections.is_empty()
            || self.singles().iter().any(Option::is_some)
    }

    pub(super) fn install_custody(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) {
        for producer in self.producers_mut() {
            producer.install(domain, attachment);
        }
    }

    pub(super) fn take(&mut self) -> Option<ControlInvocation> {
        self.producers_mut()
            .into_iter()
            .find_map(|producer| producer.take())
    }

    pub(super) fn refresh_availability(&mut self, allowed: &HashSet<ActionAddress>) {
        for producer in self.producers_mut() {
            producer.refresh(allowed);
        }
    }

    pub(super) fn any_unavailable(&self) -> bool {
        self.items
            .values()
            .chain(self.sections.values())
            .any(|emitter| !emitter.is_available())
            || self
                .singles()
                .into_iter()
                .flatten()
                .any(|producer| !producer.available())
    }

    pub(super) fn unavailable_check(&self, checkable: bool) -> bool {
        checkable
            && self
                .check
                .as_ref()
                .is_some_and(|emitter| !emitter.is_available())
    }

    /// Whether `target` is a row's or a section header's button bound to an
    /// unavailable action.
    pub(super) fn unavailable_button(&self, target: &str, shared: &NavShared) -> bool {
        if let Some(key) = target.strip_prefix("section-action:")
            && shared
                .section_add_actions
                .get(key)
                .is_some_and(|emitter| !emitter.is_available())
        {
            return true;
        }
        self.button_emitter(target, shared)
            .is_some_and(|emitter| !emitter.is_available())
    }

    fn button_emitter(&self, target: &str, shared: &NavShared) -> Option<&ActionEmitter<String>> {
        if let Some(key) = target.strip_prefix("section-action:") {
            let action = shared.section_actions.get(key)?;
            return self.sections.get(action);
        }
        let (index, _) = target.strip_prefix("row-action:")?.split_once(':')?;
        let action = shared.item_actions.get(index.parse::<usize>().ok()?)?;
        self.items.get(action)
    }

    /// The bound buttons the row of `node` shows: its section header's
    /// action, or the item's row actions.
    pub(super) fn row_controls(
        &self,
        node: &str,
        shared: &NavShared,
        model: &NavModel,
    ) -> Vec<ControlBindingMetadata> {
        if let Some(action) = shared
            .section_actions
            .get(node)
            .and_then(|action| self.sections.get(action))
        {
            return vec![action.metadata(format!("section-action:{node}"))];
        }
        if let Some(action) = shared.section_add_actions.get(node) {
            return vec![action.metadata(format!("section-action:{node}"))];
        }
        if self.items.is_empty() || !model.shows_row_actions(node) {
            return Vec::new();
        }
        shared
            .item_actions
            .iter()
            .enumerate()
            .filter_map(|(index, action)| {
                self.items
                    .get(action)
                    .map(|emitter| emitter.metadata(format!("row-action:{index}:{node}")))
            })
            .collect()
    }

    pub(super) fn check_control(&self, node: &str) -> Option<ControlBindingMetadata> {
        self.check
            .as_ref()
            .map(|emitter| emitter.metadata(format!("key:{node}")))
    }
}

trait Producer {
    fn install(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    );
    fn take(&mut self) -> Option<ControlInvocation>;
    fn refresh(&mut self, allowed: &HashSet<ActionAddress>);
    fn available(&self) -> bool;
}

impl<I> Producer for ActionEmitter<I> {
    fn install(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) {
        let _ = self.install_custody(domain, attachment);
    }
    fn take(&mut self) -> Option<ControlInvocation> {
        ActionEmitter::take(self)
    }
    fn refresh(&mut self, allowed: &HashSet<ActionAddress>) {
        self.refresh_availability(allowed);
    }
    fn available(&self) -> bool {
        self.is_available()
    }
}

impl NavList {
    /// A button at the trailing edge of every item that has not opted out
    /// ([`NavItem::without_actions`](super::NavItem::without_actions)), running `binding` with
    /// the item's id.
    #[must_use]
    pub fn item_action(mut self, action: NavAction, binding: ActionBinding<String>) -> Self {
        self.shared
            .borrow_mut()
            .bound
            .items
            .insert(action.id.clone(), ActionEmitter::new(binding));
        self.row_actions.push(NavAction {
            opens_menu: false,
            ..action
        });
        self.with_installed_row_actions()
    }

    /// A button at the trailing edge of every item that opens the row's
    /// menu ([`Self::on_context_menu`]), the menu a right click opens; what
    /// the menu offers is the menu's own.
    #[must_use]
    pub fn item_menu(mut self, action: NavAction) -> Self {
        self.row_actions.push(action.opens_context_menu());
        self.with_installed_row_actions()
    }

    /// Run `binding` with the section's id when the trailing action
    /// `action` ([`NavSection::trailing_action`](super::NavSection::trailing_action))
    /// of a section header is pressed.
    #[must_use]
    pub fn section_action(self, action: impl Into<String>, binding: ActionBinding<String>) -> Self {
        self.shared
            .borrow_mut()
            .bound
            .sections
            .insert(action.into(), ActionEmitter::new(binding));
        self
    }

    /// Run `binding` with the item's id and its new label when the person
    /// renames an editable item (F2 or a double click).
    #[must_use]
    pub fn on_rename_action(mut self, binding: ActionBinding<(String, String)>) -> Self {
        self.shared.borrow_mut().bound.rename = Some(ActionEmitter::new(binding));
        self.list = Box::new(self.list.rename_on_double_click());
        self
    }

    /// Run `binding` with the focused item's id when the person presses
    /// Delete (never Backspace, which belongs to whatever editor sits beside
    /// the list).
    #[must_use]
    pub fn on_delete_action(self, binding: ActionBinding<String>) -> Self {
        self.shared.borrow_mut().bound.delete = Some(ActionEmitter::new(binding));
        self
    }

    /// Let the person drag items, as [`Self::on_move`] does, running
    /// `binding` with the item's id and where it landed. The app hands the
    /// list its new sections once the action has moved it.
    #[must_use]
    pub fn on_move_action(self, binding: ActionBinding<(String, NavDrop)>) -> Self {
        self.shared.borrow_mut().bound.moves = Some(ActionEmitter::new(binding));
        self.with_reordering()
    }

    /// Run `binding` with the item's id and its new state when the person
    /// toggles a checkable row ([`NavItem::checked`](super::NavItem::checked)):
    /// a click anywhere on it, or Space or Enter.
    #[must_use]
    pub fn on_check_action(self, binding: ActionBinding<(String, bool)>) -> Self {
        self.shared.borrow_mut().bound.check = Some(ActionEmitter::new(binding));
        self
    }
}
