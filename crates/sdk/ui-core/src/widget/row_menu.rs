//! `RowMenu`: the More button at a row's trailing edge, holding the row's
//! secondary actions (rename, forget, remove) in a menu, so a row carries
//! one control of its own instead of a line of buttons.
//!
//! Each entry runs an exported resource action through an
//! [`ActionEmitter`]: the host resolves its policy (and asks for
//! confirmation where it must) before it runs, exactly as for a bound
//! button, and an entry whose action the surface does not offer is shown
//! disabled. Agents and assistive technology reach the same actions on the
//! row's resource; the menu is how a pointer reaches them.

use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;

use winit::event::WindowEvent;

use crate::icons::Icon;
use crate::widget::controls::ActionControls;
use crate::widget::{
    ActionEmitter, Button, ControlBindingMetadata, ControlSize, EventCtx, MenuButton, MenuItem,
    ModelPath, Widget,
};

struct Entry {
    label: String,
    destructive: bool,
    /// `None`: the action is not offered here, so the entry is disabled.
    emitter: Option<ActionEmitter<()>>,
}

/// A row's More button and the menu of its secondary actions.
pub struct RowMenu {
    menu: MenuButton,
    id: String,
    entries: Vec<Entry>,
    /// The entry the menu last ran, until its action is emitted.
    chosen: Rc<Cell<Option<usize>>>,
}

impl RowMenu {
    /// A More button `id`, named `name` ("More for Headphones"): the name
    /// is its tooltip and what a screen reader says.
    #[must_use]
    pub fn new(id: impl Into<String>, name: &str) -> Self {
        let id = id.into();
        Self {
            menu: Button::icon_only(&id, Icon::MoreHorizontal, name)
                .control_size(ControlSize::Small)
                .with_menu(Vec::new())
                .aligned(crate::widget::floating_surface::PanelAlign::Trailing),
            id,
            entries: Vec::new(),
            chosen: Rc::new(Cell::new(None)),
        }
    }

    /// An entry reading `label` that runs `binding`; shown disabled when
    /// there is none (the action is not offered here).
    #[must_use]
    pub fn action(
        self,
        label: impl Into<String>,
        binding: impl Into<Option<tilo_actions::ActionBinding<()>>>,
    ) -> Self {
        self.entry(label.into(), binding.into(), false)
    }

    /// An entry that removes or forgets something: set apart in the menu's
    /// danger colour and never the menu's first keyboard target.
    #[must_use]
    pub fn destructive(
        self,
        label: impl Into<String>,
        binding: impl Into<Option<tilo_actions::ActionBinding<()>>>,
    ) -> Self {
        self.entry(label.into(), binding.into(), true)
    }

    /// Whether the menu has any entry.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The entries' labels, in order.
    #[must_use]
    pub fn labels(&self) -> Vec<&str> {
        self.entries
            .iter()
            .map(|entry| entry.label.as_str())
            .collect()
    }

    fn entry(
        mut self,
        label: String,
        binding: Option<tilo_actions::ActionBinding<()>>,
        destructive: bool,
    ) -> Self {
        self.entries.push(Entry {
            label,
            destructive,
            emitter: binding.map(ActionEmitter::new),
        });
        self.rebuild_items();
        self
    }

    fn rebuild_items(&mut self) {
        let items = self
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let chosen = Rc::clone(&self.chosen);
                let run = move || chosen.set(Some(index));
                let item = if entry.destructive {
                    MenuItem::destructive_action(entry.label.clone(), run)
                } else {
                    MenuItem::action(entry.label.clone(), run)
                };
                item.with_enabled(
                    entry
                        .emitter
                        .as_ref()
                        .is_some_and(ActionEmitter::is_available),
                )
            })
            .collect();
        self.menu.set_items(items);
    }

    /// Emit the entry the menu ran, if one.
    fn run_chosen(&mut self) {
        if let Some(index) = self.chosen.take()
            && let Some(emitter) = self
                .entries
                .get_mut(index)
                .and_then(|entry| entry.emitter.as_mut())
            && let Err(error) = emitter.emit(())
        {
            tracing::warn!(%error, control = %self.id, "row menu entry refused its arguments");
        }
    }
}

impl crate::widget::Decorator for RowMenu {
    type Inner = MenuButton;

    fn inner(&self) -> &MenuButton {
        &self.menu
    }

    fn inner_mut(&mut self) -> &mut MenuButton {
        &mut self.menu
    }

    fn provide<'a>(&'a self, request: &mut crate::widget::capability::Request<'a, '_>) {
        if request.wants::<dyn ActionControls>() {
            request.provide::<dyn ActionControls>(self);
        } else {
            Widget::provide(&self.menu, request);
        }
    }

    fn provide_mut<'a>(&'a mut self, request: &mut crate::widget::capability::RequestMut<'a, '_>) {
        if request.wants::<dyn ActionControls>() {
            request.provide::<dyn ActionControls>(self);
        } else {
            Widget::provide_mut(&mut self.menu, request);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        Widget::handle_event(&mut self.menu, event, ctx);
        self.run_chosen();
    }

    fn sync(&mut self, state: &crate::widget::AppModel, path: &ModelPath) {
        Widget::sync(&mut self.menu, state, path);
        self.run_chosen();
    }
}

impl ActionControls for RowMenu {
    fn install_control_custody(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) {
        for emitter in self
            .entries
            .iter_mut()
            .filter_map(|entry| entry.emitter.as_mut())
        {
            let _ = emitter.install_custody(domain, attachment);
        }
    }

    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.run_chosen();
        self.entries
            .iter_mut()
            .filter_map(|entry| entry.emitter.as_mut())
            .find_map(ActionEmitter::take)
    }

    fn control_action_bindings(&self) -> Vec<ControlBindingMetadata> {
        self.entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                entry
                    .emitter
                    .as_ref()
                    .map(|emitter| emitter.metadata(format!("{}/entry/{index}", self.id)))
            })
            .collect()
    }

    fn refresh_control_action_availability(
        &mut self,
        allowed: &HashSet<tilo_actions::ActionAddress>,
    ) {
        let mut changed = false;
        for emitter in self
            .entries
            .iter_mut()
            .filter_map(|entry| entry.emitter.as_mut())
        {
            let before = emitter.is_available();
            emitter.refresh_availability(allowed);
            changed |= before != emitter.is_available();
        }
        if changed {
            self.rebuild_items();
        }
    }

    fn has_pending_control_invocation(&self) -> bool {
        self.chosen.get().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding() -> tilo_actions::ActionBinding<()> {
        use tilo_actions::{ActionSpec, EncodedAction, OperationKind, Resource};
        let spec = ActionSpec::typed_operation(
            "forget",
            "Forget",
            "device",
            OperationKind::Effect,
            "test.forget",
            vec![],
            |_| EncodedAction::encode_operation("test.forget", &()).unwrap(),
        );
        let resource = Resource::new("test:device", "test", "Test").with_action(spec.clone());
        tilo_actions::ActionBinding::new(&resource, &spec, |()| serde_json::Map::new())
            .expect("the spec is exported")
    }

    /// Choosing an entry queues its action for the host, once.
    #[test]
    fn a_chosen_entry_queues_its_bound_action() {
        let mut menu = RowMenu::new("device.more", "More for Headphones")
            .action("Rename", binding())
            .destructive("Forget", binding());
        assert_eq!(menu.labels(), ["Rename", "Forget"]);
        menu.chosen.set(Some(1));
        assert!(menu.has_pending_control_invocation());
        assert!(menu.take_control_invocation().is_some());
        assert!(menu.take_control_invocation().is_none());
    }

    /// An entry whose action the surface does not offer stays in the menu,
    /// disabled, and queues nothing.
    #[test]
    fn an_entry_the_surface_does_not_offer_is_disabled() {
        let mut menu =
            RowMenu::new("device.more", "More for Headphones").action("Rename", binding());
        menu.refresh_control_action_availability(&HashSet::new());
        assert!(!menu.menu.menu_mut().items()[0].is_enabled());
        menu.chosen.set(Some(0));
        assert!(menu.take_control_invocation().is_none());
    }
}
