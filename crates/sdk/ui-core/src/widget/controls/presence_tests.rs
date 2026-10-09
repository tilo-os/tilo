use super::*;
use crate::a11y::{AccessNode, Role};
use crate::widget::{Button, Column, Text};
use std::cell::Cell;
use std::rc::Rc;

struct SemanticOnly {
    inner: Text,
    calls: Rc<Cell<usize>>,
}
impl crate::widget::Decorator for SemanticOnly {
    type Inner = Text;
    fn inner(&self) -> &Text {
        &self.inner
    }
    fn inner_mut(&mut self) -> &mut Text {
        &mut self.inner
    }
    fn accessibility(&self) -> Option<AccessNode> {
        self.calls.set(self.calls.get() + 1);
        Some(AccessNode::new(Role::MultilineTextInput).with_action_target("semantic-editor"))
    }
}

fn semantic(calls: Rc<Cell<usize>>) -> SemanticOnly {
    SemanticOnly {
        inner: Text::new("Measured prose"),
        calls,
    }
}

fn action_button() -> Button {
    let resource = tilo_actions::Resource::new("doc:1", "doc", "Document").with_action(
        tilo_actions::ActionSpec::operation(
            "save",
            "Save",
            tilo_actions::OperationKind::Effect,
            "test.save",
            &(),
        )
        .unwrap(),
    );
    Button::new("save", "Save")
        .on_action(tilo_actions::ActionBinding::unit_for(&resource, "save").unwrap())
}

#[test]
fn presence_does_not_materialize_semantic_only_accessibility() {
    let calls = Rc::new(Cell::new(0));
    let root = semantic(Rc::clone(&calls));
    assert!(!has_action_control_bindings(&root));
    assert_eq!(
        calls.get(),
        0,
        "availability presence must not build semantic-only prose"
    );
    let bindings = control_action_bindings(&root);
    assert_eq!(
        calls.get(),
        1,
        "the full semantic inventory remains available"
    );
    assert_eq!(
        bindings,
        vec![ControlBindingMetadata::semantic("semantic-editor")]
    );
    calls.set(0);
    let root = Column::new()
        .push(Box::new(root))
        .push(Box::new(action_button()));
    assert!(has_action_control_bindings(&root));
    assert_eq!(
        calls.get(),
        0,
        "finding an emitter must not allocate unrelated semantic nodes"
    );
    assert!(
        control_action_bindings(&root)
            .iter()
            .any(|binding| matches!(binding.kind, ControlBindingKind::Action(_)))
    );
}

struct Exposure {
    inner: Button,
    exposed: bool,
}
impl crate::widget::Decorator for Exposure {
    fn provide<'a>(&'a self, _: &mut crate::widget::capability::Request<'a, '_>) {}
    fn provide_mut<'a>(&'a mut self, _: &mut crate::widget::capability::RequestMut<'a, '_>) {}
    type Inner = Button;
    fn inner(&self) -> &Button {
        &self.inner
    }
    fn inner_mut(&mut self) -> &mut Button {
        &mut self.inner
    }
    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        if self.exposed {
            visitor(&self.inner);
        }
    }
    fn children(&self) -> Vec<&dyn Widget> {
        if self.exposed {
            vec![&self.inner]
        } else {
            Vec::new()
        }
    }
}

#[test]
fn presence_follows_current_child_exposure_without_a_cached_boolean() {
    let mut root = Exposure {
        inner: action_button(),
        exposed: false,
    };
    for exposed in [false, true, false, true] {
        root.exposed = exposed;
        assert_eq!(has_action_control_bindings(&root), exposed);
        assert_eq!(
            has_action_control_bindings(&root),
            control_action_bindings(&root)
                .iter()
                .any(|binding| matches!(binding.kind, ControlBindingKind::Action(_)))
        );
    }
}

struct PrivateProducer {
    inner: Text,
    active: bool,
}
impl crate::widget::Decorator for PrivateProducer {
    crate::provides!(dyn ActionControls);
    type Inner = Text;
    fn inner(&self) -> &Text {
        &self.inner
    }
    fn inner_mut(&mut self) -> &mut Text {
        &mut self.inner
    }
}
impl ActionControls for PrivateProducer {
    fn control_action_bindings(&self) -> Vec<ControlBindingMetadata> {
        if self.active {
            vec![ControlBindingMetadata {
                control_id: "private-save".into(),
                kind: ControlBindingKind::Action(tilo_actions::ActionAddress::new("doc:1", "save")),
            }]
        } else {
            Vec::new()
        }
    }
}

#[test]
fn presence_preserves_custom_capability_inventory_fallback() {
    let mut root = PrivateProducer {
        inner: Text::new("Private control owner"),
        active: false,
    };
    for active in [false, true, false] {
        root.active = active;
        assert_eq!(
            has_action_control_bindings(&root),
            active,
            "custom producers are authoritative even without exposed child widgets"
        );
    }
}
