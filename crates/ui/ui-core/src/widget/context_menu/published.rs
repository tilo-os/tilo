//! An open menu's rows as the host reaches them: a row that runs a published action
//! ([`MenuItem::for_action`]) is bound to that action under the target its accessibility
//! node names, and the widget that opened the menu runs the action behind the row when the row
//! is chosen, by a person or through its accessibility action.

use super::{ContextMenu, MenuItem};

impl ContextMenu {
    /// The owner's top-level rows a person can choose now that run a published action, with
    /// their index.
    fn action_rows(&self) -> Vec<(usize, tilo_actions::ActionAddress)> {
        if !self.open {
            return Vec::new();
        }
        let own = self.own_rows.unwrap_or(self.items.len());
        self.items
            .iter()
            .enumerate()
            .take(own)
            .filter(|(_, item)| item.is_enabled() && item.label().is_some())
            .filter(|(_, item)| item.submenu_items().is_none())
            .filter_map(|(index, item)| Some((index, item.action_address()?)))
            .collect()
    }

    /// Every press the menu's rows hold, nested rows under their parent.
    fn presses(&self) -> Vec<crate::widget::Press> {
        fn collect(items: &[MenuItem], into: &mut Vec<crate::widget::Press>) {
            for item in items {
                if let Some(press) = item.press() {
                    into.push(press.clone());
                }
                if let MenuItem::Action {
                    submenu: Some(nested),
                    ..
                } = item
                {
                    collect(nested, into);
                }
            }
        }
        let mut presses = Vec::new();
        collect(&self.items, &mut presses);
        presses
    }
}

impl crate::widget::controls::ActionControls for ContextMenu {
    fn attach_controls(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_domain::DomainAttachment,
    ) {
        for press in self.presses() {
            press.attach(domain, attachment);
        }
    }

    /// A row pressed before the menu closed hands its invocation over here.
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.presses().into_iter().find_map(|press| press.take())
    }

    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        let mut changed = false;
        for press in self.presses() {
            changed |= press.refresh_availability(allowed);
        }
        if changed {
            self.dirty = true;
        }
    }

    /// Each row that runs an action is bound to it, under the target its accessibility node
    /// names.
    fn control_action_bindings(&self) -> Vec<crate::widget::ControlBindingMetadata> {
        let mut bindings = crate::widget::controls::default_bindings(self);
        for (index, address) in self.action_rows() {
            bindings.push(crate::widget::ControlBindingMetadata {
                control_id: format!("context-menu:item:{index}"),
                kind: crate::widget::ControlBindingKind::Action(address),
            });
        }
        bindings
    }
}

#[cfg(test)]
mod tests {
    use super::super::{ContextMenu, MenuItem};

    /// A row that runs a published action is that action, named as the row's binding; a row
    /// of the menu's own binds nothing.
    #[test]
    fn a_row_that_runs_a_published_action_is_bound_to_it() {
        let share = tilo_actions::ActionAddress::new("doc:1", "share");
        let mut menu = ContextMenu::with_items(vec![
            MenuItem::selection("Share").for_action(share.clone()),
            MenuItem::selection("Rename"),
        ]);
        menu.open_at([0.0, 0.0]);
        let bindings = crate::widget::controls::control_action_bindings(&menu)
            .into_iter()
            .filter_map(|binding| match binding.kind {
                crate::widget::ControlBindingKind::Action(address) => {
                    Some((binding.control_id, address))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(bindings, [("context-menu:item:0".to_owned(), share)]);
    }
}
