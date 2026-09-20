use super::*;
use crate::application::{LaunchTarget, RestoreInput};
use crate::application_test_support::{ProbeHost, instantiate_probe};

#[test]
fn direct_affordance_policy_uses_live_authority_and_refuses_lost_owned_custody() {
    let host = ProbeHost::new();
    let component = instantiate_probe(&host, 77, LaunchTarget::New, RestoreInput::None).unwrap();
    let binding = component.callback_binding();
    *binding.permissions.borrow_mut() = crate::application_test_support::runner_permissions()
        .borrow()
        .clone();
    let resource = tilo_actions::Resource::new("fixture", "document", "Fixture").with_action(
        tilo_actions::ActionSpec::prompted(
            "rename",
            "Rename",
            "Title",
            "title",
            tilo_actions::ExecutionLocus::ClientLocal,
            |_| tilo_actions::EncodedAction {
                type_id: "fixture.rename".into(),
                payload: vec![],
            },
        ),
    );
    let mut outer = AutoCommit::new();
    assert!(
        authorize_current_resource_action(&outer, &resource, "rename").is_ok(),
        "genuinely unbound native compatibility"
    );
    binding
        .with_document(&mut outer, |doc, _| {
            assert!(authorize_current_resource_action(doc, &resource, "rename").is_ok());
            assert!(authorize_current_resource_action(doc, &resource, "missing").is_err());
            assert!(
                authorize_current_resource_action(&doc.clone(), &resource, "rename").is_err(),
                "a cloned foreign Doc cannot borrow active authority"
            );
            let permissions = binding.permissions.borrow().clone();
            *binding.permissions.borrow_mut() =
                crate::permissions::PermissionContext::from_capabilities(
                    crate::permissions::Actor::person(
                        Some(crate::permissions::Role::Viewer),
                        &binding.author.0,
                    ),
                    vec![],
                );
            assert!(
                authorize_current_resource_action(doc, &resource, "rename").is_err(),
                "revocation is observed at execution"
            );
            *binding.permissions.borrow_mut() = permissions;
            binding.closing.set(true);
            assert!(
                authorize_current_resource_action(doc, &resource, "rename").is_err(),
                "closed owner cannot use unbound fallback"
            );
            binding.closing.set(false);
            let frame = ACTIVE.with_borrow(|active| active.as_ref().unwrap().3.clone().unwrap());
            let registry = frame
                .borrow_mut()
                .as_mut()
                .unwrap()
                .mutation
                .as_mut()
                .unwrap()
                .binding
                .registry_owner
                .clone();
            frame
                .borrow_mut()
                .as_mut()
                .unwrap()
                .mutation
                .as_mut()
                .unwrap()
                .binding
                .registry_owner = std::rc::Weak::new();
            assert!(
                authorize_current_resource_action(doc, &resource, "rename").is_err(),
                "expired registry custody cannot use unbound fallback"
            );
            frame
                .borrow_mut()
                .as_mut()
                .unwrap()
                .mutation
                .as_mut()
                .unwrap()
                .binding
                .registry_owner = registry;
            let mutation = frame.borrow_mut().as_mut().unwrap().mutation.take();
            assert!(
                authorize_current_resource_action(doc, &resource, "rename").is_err(),
                "an active frame without an owner fails closed"
            );
            frame.borrow_mut().as_mut().unwrap().mutation = mutation;
            let owned = ACTIVE.with_borrow_mut(|active| active.as_mut().unwrap().3.take());
            assert!(
                authorize_current_resource_action(doc, &resource, "rename").is_err(),
                "missing frame cannot use unbound fallback"
            );
            ACTIVE.with_borrow_mut(|active| active.as_mut().unwrap().3 = owned);
            assert!(
                authorize_current_resource_action(doc, &resource, "rename").is_ok(),
                "current valid authority still works"
            );
        })
        .unwrap();
}
