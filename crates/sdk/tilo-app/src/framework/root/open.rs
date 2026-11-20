//! The framework's `open`: showing the item an anchor names.

use super::ResourceSpec;

/// The operation type of the framework's `open` view: show the item at an
/// anchor, as picking a search hit does.
pub(crate) const OPEN_TYPE: &str = tilo_actions::remote::APPLICATION_OPEN_TYPE;

/// The parameter of `open` naming what to show.
pub(crate) const OPEN_ANCHOR: &str = "anchor";

/// `open`: show the item at an anchor (a search entry's anchor). Published
/// when a view shows items ([`ViewCx::on_open`](crate::framework::ViewCx::on_open)), so
/// an agent opens the app where a person's search hit would.
pub(crate) fn open_spec() -> tilo_actions::ActionSpec {
    tilo_actions::ActionSpec::typed_operation(
        "open",
        "Open",
        "Item to open",
        tilo_actions::OperationKind::View,
        OPEN_TYPE,
        vec![tilo_actions::ActionParam::required(
            OPEN_ANCHOR,
            tilo_actions::ParamType::Text,
            "Item to open: the anchor of a search hit",
        )],
        |args| {
            tilo_actions::EncodedAction::encode_operation(
                OPEN_TYPE,
                &serde_json::json!({ OPEN_ANCHOR: args.text(OPEN_ANCHOR) }),
            )
            .unwrap_or_else(|_| tilo_actions::EncodedAction {
                type_id: OPEN_TYPE.into(),
                payload: Vec::new(),
            })
        },
    )
    .of_kind(tilo_actions::ActionKind::Read)
    .with_description("Open the app at one item: the anchor of a search hit.")
}

/// The label of the item view the framework gives an item resource that
/// presents no view of its own.
const ITEM_VIEW_LABEL: &str = "Item";

/// `open` on one item resource: show that item, as its search hit would.
pub(crate) fn item_open_spec(anchor: &str) -> tilo_actions::ActionSpec {
    let encoded = tilo_actions::EncodedAction::encode_operation(
        OPEN_TYPE,
        &serde_json::json!({ OPEN_ANCHOR: anchor }),
    )
    .unwrap_or_else(|_| tilo_actions::EncodedAction {
        type_id: OPEN_TYPE.into(),
        payload: Vec::new(),
    });
    tilo_actions::ActionSpec::typed_operation(
        "open",
        "Open",
        "Open",
        tilo_actions::OperationKind::View,
        OPEN_TYPE,
        Vec::new(),
        move |_| encoded.clone(),
    )
    .of_kind(tilo_actions::ActionKind::Read)
    .with_description("Show this item.")
}

/// Offer `open` on the item resource `resource`, with the view and the own
/// `share-link` that go with it, unless it has an `open` already: what shows
/// the item where the app's window shows it and lets a link name it. An item
/// of the app's record and one kept in a part of a chain are offered alike,
/// so a link to either opens the item and not the record.
pub(crate) fn offer_item(resource: &mut ResourceSpec) {
    if resource
        .actions
        .iter()
        .any(|existing| existing.id == "open")
    {
        return;
    }
    resource.actions.push(item_open_spec(resource.id.as_str()));
    // The opener is also a presentation binding, not just a verb in the
    // catalogue. Keep an app's explicit views; otherwise advertise the
    // item view that this exact bound opener actually shows. The view
    // names what it shows, the item, and not which item: the item's own
    // label is the resource's, published once with it. Every item's
    // `open` is then one catalogue entry however many items there are,
    // where a copy of the item's label made each item's `open`, and its
    // whole action template, an entry of its own.
    if resource.views.is_empty() {
        resource.views.push(
            tilo_actions::ResourceViewSpec::new("item", ITEM_VIEW_LABEL, "open").with_primary(true),
        );
        // A view is also what a link opens: with one, the item offers
        // `share-link` of its own, which names the item rather than the
        // record it is in.
        tilo_resource::share::offer(resource);
    }
}

/// Offer `open` on every item resource under `resource` (an item of the
/// collections of the resource `item_of` knows the items of), bound to that
/// item ([`offer_item`]).
pub(crate) fn offer_item_open(item_of: fn(&str) -> bool, resource: &mut ResourceSpec) {
    if item_of(resource.id.as_str()) {
        offer_item(resource);
    }
    for child in &mut resource.children {
        offer_item_open(item_of, child);
    }
}
