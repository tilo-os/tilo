//! `Identified`: a control that runs an app action says which one, however the window
//! composes around it.

use super::{Element, Widget};
use crate::a11y::AccessNode;

/// A control and what it does.
#[crate::component::not_a_component(framework, "the host names what a view's control does")]
pub struct Identified {
    content: Element,
    identity: String,
}

impl Identified {
    /// `content`, a control that does `identity`: the action it runs, or the field it
    /// edits, written as the app's ids name them.
    #[must_use]
    pub fn new(content: Element, identity: impl Into<String>) -> Self {
        Self {
            content,
            identity: identity.into(),
        }
    }

    /// What the control does.
    #[must_use]
    pub fn identity(&self) -> &str {
        &self.identity
    }

    /// The control again, doing something else.
    pub fn identify(&mut self, identity: impl Into<String>) {
        self.identity = identity.into();
    }
}

impl super::Decorator for Identified {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    fn accessibility(&self) -> Option<AccessNode> {
        // What the control does is said where a press reaches it: a control that draws a button
        // of its own (an empty state's action) says it on that button, whose box a person, a
        // screen reader and an agent press. Said on the box around it, it would name a control
        // nothing answers at.
        self.content
            .accessibility()
            .map(|node| match self.content.pressable_child() {
                Some(part) => node.with_action_on_child(part, self.identity.clone()),
                None => node.with_action(self.identity.clone()),
            })
    }

    fn identify(&mut self, identity: &str) -> bool {
        Identified::identify(self, identity);
        true
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        self.content.as_any()
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        self.content.as_any_mut()
    }

    fn concrete_type_name(&self) -> &'static str {
        self.content.concrete_type_name()
    }

    /// The content's change: an identity adds no structure of its own.
    fn take_structure_change(&mut self) -> crate::widget::StructureChange {
        self.content.take_structure_change()
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &crate::theme::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let node = self.content.build_layout(taffy, theme, node_map);
        super::decorator::register_node(node_map, node, self);
        node
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a11y::Role;
    use crate::widget::Button;

    fn button() -> Element {
        Box::new(Button::new("new-note", "New note"))
    }

    tilo_test_env::scenario! {
        fn an_identified_control_says_what_it_does_and_stays_itself;

        phase a_control_says_what_it_does_in_its_own_node;
        {
            let identified = Identified::new(button(), "so.tilo.notes.Notebook\u{1f}new_note");
            let node = super::Widget::accessibility(&identified).expect("the control describes itself");
            assert_eq!(node.role, Role::Button);
            assert_eq!(
                node.action.as_deref(),
                Some("so.tilo.notes.Notebook\u{1f}new_note")
            );
            assert_eq!(node.name.as_deref(), Some("New note"));
        }

        phase the_widget_it_stands_for_is_reached_as_it_was;
        {
            let identified = Identified::new(button(), "notes.Notebook\u{1f}new_note");
            assert!(
                identified
                    .as_any()
                    .and_then(|any| any.downcast_ref::<Button>())
                    .is_some(),
                "a host that updates the control in place finds it"
            );
        }

        phase identifying_another_action_changes_only_what_it_does;
        {
            let mut identified = Identified::new(button(), "notes.Notebook\u{1f}new_note");
            identified.identify("notes.Notebook\u{1f}new_task");
            let node = super::Widget::accessibility(&identified).expect("the control describes itself");
            assert_eq!(node.action.as_deref(), Some("notes.Notebook\u{1f}new_task"));
        }

        phase a_control_that_holds_its_own_button_says_what_it_does_on_that_button;
        {
            let state = crate::widget::EmptyState::new("Start your first note")
                .with_action("New note")
                .with_action_id("first-note");
            let identified = Identified::new(Box::new(state), "notes.Notebook\u{1f}new_note");
            let node = super::Widget::accessibility(&identified).expect("the state describes itself");
            assert_eq!(
                node.action, None,
                "the box around the button is not what a press reaches"
            );
            let action = node
                .children
                .iter()
                .find(|child| child.role == Role::Button)
                .expect("the state describes its action as a button");
            assert_eq!(action.name.as_deref(), Some("New note"));
            assert_eq!(
                action.action.as_deref(),
                Some("notes.Notebook\u{1f}new_note"),
                "the button says what the state does"
            );
        }
    }
}
