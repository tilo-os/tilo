//! A component whose "Send" button is bound to its typed `send` action, an effect that reaches
//! outside the person's device: a press on it is the action's call, which the workspace's grants
//! decide (asking the person, or refusing), where a toolkit closure is never gated.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::disallowed_methods,
    reason = "test fixture: a failure is the assertion, the probe paints through the widget contract, and a test-support build hands another crate's tests only some of it"
)]

use std::cell::Cell;
use std::rc::Rc;

use super::Button;
use tilo_ui_base::widget::Widget;

/// What [`sends`] shows: its button, the resource it publishes and what a send runs.
struct Sends {
    child: tilo_ui_base::widget::Element,
    resource: tilo_actions::ResourceSpec,
    sent: Box<dyn FnMut()>,
}

/// The component boundary around [`Sends`], as a window's root is: the record owner its controls
/// belong to, whose permissions the window's device answers before a press reaches them.
struct BoundSends {
    binding: Rc<crate::bound_dispatch::CallbackBinding>,
    content: Box<Sends>,
    registry: tilo_actions::ActionRegistry,
    /// The controls' ownership: the presses in this component are its own, partitioned to it
    /// rather than to the window's outer document.
    control_domain: tilo_ui_base::ControlDomain,
    _scope: crate::WakeScope,
    _registry_owner: Rc<()>,
    _owners: crate::resource::ResourceOwners,
}

const SEND_OPERATION: &str = "fixture.send";

/// The fixture, its button named `go`: `sent` runs only when the action is admitted.
pub fn sends(sent: impl FnMut() + 'static) -> tilo_ui_base::widget::Element {
    sends_through(sent, |binding| {
        Box::new(Button::new("go", "Send").on_action(binding))
    })
}

/// The fixture with the control `control` makes over the send action in its button's place: any
/// control a person presses to send, whatever input it takes (the action has no parameters).
pub fn sends_through<I: 'static>(
    sent: impl FnMut() + 'static,
    control: impl FnOnce(tilo_actions::ActionBinding<I>) -> tilo_ui_base::widget::Element,
) -> tilo_ui_base::widget::Element {
    use tilo_actions::{ActionBinding, ActionSpec, EncodedAction, OperationKind, ResourceSpec};
    let spec = ActionSpec::typed_operation(
        "send",
        "Send",
        "Send it",
        OperationKind::Effect,
        SEND_OPERATION,
        vec![],
        |_| EncodedAction::encode_operation(SEND_OPERATION, &()).unwrap(),
    );
    let resource = ResourceSpec::new("fixture:outbox", "fixture", "Outbox").with_action(spec);
    let binding = ActionBinding::new(&resource, &resource.actions[0], |_: I| {
        serde_json::Map::new()
    })
    .unwrap();
    let (bound, control_domain, scope, registry_owner, owners) = mount();
    Box::new(BoundSends {
        binding: Rc::new(bound),
        content: Box::new(Sends {
            child: Box::new(
                crate::widget::Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .push(control(binding)),
            ),
            resource,
            sent: Box::new(sent),
        }),
        registry: tilo_actions::ActionRegistry::new(),
        control_domain,
        _scope: scope,
        _registry_owner: registry_owner,
        _owners: owners,
    })
}

/// A component binding over a record of its own, as [`crate::application::ProcessContext`] mounts
/// one: the questions its controls ask are answered by the window's device.
fn mount() -> (
    crate::bound_dispatch::CallbackBinding,
    tilo_ui_base::ControlDomain,
    crate::WakeScope,
    Rc<()>,
    crate::resource::ResourceOwners,
) {
    use crate::resource::{DocumentCheckout, ResourceOwners, ResourceType};
    // A window's records live in a workspace of their own, whose route the window names when it
    // joins its workspace: a route-less component could only show controls, never run one.
    let workspace = uuid::Uuid::new_v4().to_string();
    let mut owners = ResourceOwners::reading(Rc::new(crate::resource::MemoryWorkspace::default()));
    let records = owners.open("fixture", &workspace).unwrap();
    let checkout =
        DocumentCheckout::new(crate::DocumentId::for_tab(61), ResourceType::new("outbox"));
    records
        .reserve_resource(&checkout)
        .unwrap()
        .initialize(|_| Ok(()))
        .unwrap();
    let scope = crate::WakeScope::new();
    let registry_owner = Rc::new(());
    let control_domain = tilo_ui_base::ControlDomain::new();
    let binding = crate::bound_dispatch::CallbackBinding {
        lifetime: scope.handle(),
        closing: Rc::new(Cell::new(false)),
        registry_owner: Rc::downgrade(&registry_owner),
        lifetime_id: uuid::Uuid::new_v4(),
        control_domain: control_domain.downgrade(),
        queued_controls: Rc::new(std::cell::RefCell::new(std::collections::VecDeque::new())),
        search_requests: Rc::default(),
        instance_id: 61,
        app_id: "fixture".into(),
        device_id: "fixture-device".into(),
        local_person: tilo_ui_base::permissions::MemberId::person("fixture-author"),
        records,
        checkout,
        permissions: tilo_ui_base::permissions::SharedPermissions::default(),
        author: tilo_ui_base::permissions::MemberId::person("fixture-author"),
        operation_context: Rc::default(),
        writes_as_author: false,
    };
    control_domain.set_owner(tilo_ui_base::control_domain::ControlOwner::Component(
        Rc::new(binding.clone()),
    ));
    (binding, control_domain, scope, registry_owner, owners)
}

impl Widget for BoundSends {
    tilo_ui_base::provides!(
        dyn crate::bound_dispatch::BoundWidget,
        dyn tilo_ui_base::widget::component_walk::ComponentBoundary,
        dyn crate::bound_dispatch::BoundTraversal,
        dyn tilo_ui_base::widget::controls::ActionControls
    );
    crate::testing::delegate_widget_methods!(
        content: sync,
        build_layout,
        apply_layout,
        draw,
        handle_event,
        get_rect
    );
    fn name(&self) -> &str {
        "Outbox fixture"
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.content.as_ref()]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![self.content.as_mut()]
    }
}

impl tilo_ui_base::widget::component_walk::ComponentBoundary for BoundSends {
    fn component_scope(&self) -> Option<tilo_ui_base::widget::component_walk::Binding> {
        Some(Rc::clone(&self.binding) as tilo_ui_base::widget::component_walk::Binding)
    }
}

impl crate::bound_dispatch::BoundWidget for BoundSends {}

impl crate::bound_dispatch::BoundTraversal for BoundSends {
    fn visit_components(
        &mut self,
        request: &mut crate::bound_dispatch::ComponentRequest<'_>,
    ) -> Result<(), crate::bound_dispatch::BoundDispatchError> {
        if request
            .target
            .is_some_and(|target| !target.same_component(&self.binding))
        {
            return Ok(());
        }
        let _entry = self.binding.enter()?;
        let _ = (request.visit)(&self.binding, &mut *self.content, &self.registry);
        request.found = true;
        Ok(())
    }
}

impl tilo_ui_base::widget::controls::ActionControls for BoundSends {
    /// The component's controls belong to the component's domain, under the window's: what they
    /// press is delivered as the component's own call, never the window's.
    fn attach_controls(
        &mut self,
        parent: &tilo_ui_base::ControlDomain,
        attachment: tilo_ui_base::control_domain::DomainAttachment,
    ) {
        let _ = self.control_domain.attach_to(parent);
        tilo_ui_base::widget::controls::attach_controls(
            &mut *self.content,
            &self.control_domain,
            attachment,
        );
    }

    /// The component's own presses decide its controls' availability from its device's verdicts;
    /// the window's unbound projection, which knows only what lies outside a component, must not
    /// push into it (as a hosted app's boundary does not).
    fn refresh_control_action_availability(
        &mut self,
        _allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
    }
}

impl Widget for Sends {
    tilo_ui_base::provides!(
        dyn tilo_ui_base::widget::controls::ActionControls,
        dyn crate::widget::resources::PublishesResources,
        dyn tilo_ui_base::widget::ResourceOperations
    );

    crate::testing::delegate_widget_methods!(
        child: sync,
        build_layout,
        apply_layout,
        draw,
        handle_event,
        get_rect
    );
    fn name(&self) -> &str {
        "Outbox fixture"
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.child.as_ref()]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![self.child.as_mut()]
    }
}

impl tilo_ui_base::widget::ResourceOperations for Sends {
    fn resource_operation_handlers(&self) -> std::collections::HashSet<String> {
        std::iter::once(SEND_OPERATION.to_owned()).collect()
    }

    fn start_operation(
        &mut self,
        op: &tilo_ui_base::widget::ResourceOperation<'_>,
        _: &mut tilo_ui_base::widget::Edits<'_>,
        _: &automerge::ObjId,
    ) -> Result<Option<tilo_ui_base::widget::ResourceOperationStart>, String> {
        if op.request().type_id != SEND_OPERATION {
            return Ok(None);
        }
        (self.sent)();
        Ok(Some(
            tilo_ui_base::widget::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            ),
        ))
    }
}

impl crate::widget::resources::PublishesResources for Sends {
    fn resources(&self, _: &automerge::AutoCommit) -> Vec<tilo_actions::ResourceSpec> {
        vec![self.resource.clone()]
    }
}

impl tilo_ui_base::widget::controls::ActionControls for Sends {
    fn attach_controls(
        &mut self,
        domain: &tilo_ui_base::ControlDomain,
        attachment: tilo_ui_base::control_domain::DomainAttachment,
    ) {
        tilo_ui_base::widget::controls::attach_controls(self.child.as_mut(), domain, attachment);
    }
}

crate::testing::test_app_widget!(Sends, "Outbox fixture");
