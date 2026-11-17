//! A catalogue row as the agent reads it: in full, browsed in a page, or
//! resolved alone.
use serde_json::{Value, json};
use tilo_actions::remote::{CataloguePage, CatalogueRow};

pub(crate) fn page_json(page: &CataloguePage) -> Value {
    json!({"revision":page.revision,"rows":page.rows.iter().map(browsed_row_json).collect::<Vec<_>>(),"next_cursor":page.next_cursor,"exhausted":page.exhausted,"stale_cursor":page.stale_cursor,"budget_exhausted":page.budget_exhausted})
}
/// A parameter schema larger than this is left to `tilo_resolve` in a
/// browsed page: one rich-text `apply` schema is tens of kilobytes, and an
/// agent pays for every byte of every page it reads.
const BROWSED_SCHEMA_BYTES: usize = 1024;
/// [`row_json`] with large parameter schemas replaced by a pointer to
/// `tilo_resolve`, which returns them in full.
pub(crate) fn browsed_row_json(row: &CatalogueRow) -> Value {
    let mut value = row_json(row);
    if let Some(params) = value
        .pointer_mut("/descriptor/params")
        .and_then(Value::as_array_mut)
    {
        for param in params {
            let large = param
                .get("schema")
                .is_some_and(|schema| schema.to_string().len() > BROWSED_SCHEMA_BYTES);
            if large && let Some(param) = param.as_object_mut() {
                param.remove("schema");
                param.insert(
                    "schema_omitted".into(),
                    json!("large schema: tilo_resolve this action to read it"),
                );
            }
        }
    }
    value
}
/// [`row_json`] without what it says twice: the descriptor's resource,
/// kind and label are the row's, a parent that is the resource itself, and
/// a visibility that is the action's own contract. Every resolve pays for
/// them, and the schema is what the agent came for.
pub(crate) fn resolved_row_json(row: &CatalogueRow) -> Value {
    let mut value = row_json(row);
    let own_visibility = value.get("visibility_action") == value.get("action")
        && value.get("visibility_contract") == value.pointer("/descriptor/contract");
    let Some(fields) = value.as_object_mut() else {
        return value;
    };
    if fields.get("parent_resource") == fields.get("resource") {
        fields.remove("parent_resource");
    }
    if own_visibility {
        fields.remove("visibility_action");
        fields.remove("visibility_contract");
    }
    let row_fields = [
        ("resource", fields.get("resource").cloned()),
        ("resource_kind", fields.get("resource_kind").cloned()),
        ("resource_label", fields.get("resource_label").cloned()),
    ];
    if let Some(Value::Object(descriptor)) = fields.get_mut("descriptor") {
        for (key, row_value) in row_fields {
            if descriptor.get(key) == row_value.as_ref() {
                descriptor.remove(key);
            }
        }
        if let Some(Value::Array(params)) = descriptor.get_mut("params") {
            params.iter_mut().for_each(param_said_once);
        }
        descriptor.retain(|_, item| !item.is_null());
    }
    fields.retain(|_, item| !item.is_null());
    value
}
/// A parameter whose schema says it all: its type, nullability and
/// description stand in the schema, so the copies beside it go, and a
/// description that only repeats the name says nothing.
pub(crate) fn param_said_once(param: &mut Value) {
    let Some(fields) = param.as_object_mut() else {
        return;
    };
    let Some(schema) = fields.get("schema").filter(|schema| schema.is_object()) else {
        return;
    };
    let repeated = fields.get("description").is_some_and(|description| {
        Some(description) == schema.get("description") || Some(description) == fields.get("name")
    });
    if repeated {
        fields.remove("description");
    }
    fields.remove("ty");
    fields.remove("nullable");
}
pub(crate) fn row_json(row: &CatalogueRow) -> Value {
    let descriptor = row.action.as_ref().map(|action| {
        json!({"resource":action.address.resource,"action":action.address.action,"resource_kind":action.resource_kind,"resource_label":action.resource_label,"action_label":action.action_label,"description":action.description,"operation_kind":action.kind,"params":action.params,"contract":action.contract})
    });
    json!({
        "parent_resource":row.parent_resource,
        "resource":row.resource.id,
        "resource_kind":row.resource.kind,
        "resource_label":row.resource.label,
        "action":row.action.as_ref().map(|action| &action.address.action),
        "descriptor":descriptor,
        "has_children":row.has_children,
        "visibility_action":row.visibility.as_ref().map(|visibility| &visibility.action),
        "visibility_contract":row.visibility.as_ref().map(|visibility| &visibility.contract),
    })
}
