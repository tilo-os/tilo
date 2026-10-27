use super::*;

fn spec() -> ActionSpec {
    ActionSpec::immediate_encoded(
        "set-block-text",
        "Set block text",
        ExecutionLocus::default(),
        EncodedAction {
            type_id: "notes.notebook".into(),
            payload: Vec::new(),
        },
    )
}

/// A nested resource's action is the enclosing record's field edit, so
/// its history names the record's kind, which the change's claim is
/// decided by, and keeps the body it was addressed through as a facet.
#[test]
fn a_nested_action_is_named_as_the_resource_whose_action_it_is() {
    let id = "notes.notebook/pages/p1/body";
    let own = spec().history_context(id, "tilo.rich-text", "Body");
    let own_resource = own.resource.expect("a resource");
    assert_eq!(own_resource.kind, "tilo.rich-text");
    assert!(own.facets.is_empty());

    let nested = spec()
        .applied_as("notes.notebook", "Notebook")
        .history_context(id, "tilo.rich-text", "Body");
    let resource = nested.resource.expect("a resource");
    assert_eq!(
        resource.id, id,
        "still the resource it was addressed through"
    );
    assert_eq!(resource.kind, "notes.notebook");
    assert_eq!(resource.label, "Notebook");
    assert_eq!(
        nested.facets,
        [crate::ActionFacet::new(NESTED_RESOURCE_FACET, "Body")]
    );
}
