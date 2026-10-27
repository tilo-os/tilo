use super::*;

/// The public parts constructor keeps the component instance and formats
/// the domain address. The host still resolves that instance against its
/// fresh resource tree; constructing an address grants nothing.
#[test]
fn a_bound_action_address_names_one_instance_and_round_trips_its_address() {
    let instance: crate::application::InstanceId = 42;
    let bound =
        BoundActionAddress::new(instance, "notes.note:1", "edit-title").expect("a stable address");
    assert_eq!(bound.instance_id, instance);
    assert_eq!(
        bound.address,
        tilo_actions::ActionAddress::new("notes.note:1", "edit-title")
    );
    assert_eq!(
        tilo_actions::ActionAddress::parse(&bound.address.encode()),
        Some(bound.address)
    );
    assert!(BoundActionAddress::new(instance, "notes\u{1f}note", "edit-title").is_none());
    assert!(BoundActionAddress::new(instance, "notes.note:1", "").is_none());
}
