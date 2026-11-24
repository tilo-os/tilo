//! A catalogue page and its rows as the JSON an agent reads.

use serde_json::{Value, json};
use tilo_actions::remote::{CataloguePage, CatalogueRow};

pub(super) fn page_json(page: &CataloguePage) -> Value {
    json!({"revision":page.revision,"rows":page.rows.iter().map(browsed_row_json).collect::<Vec<_>>(),"next_cursor":page.next_cursor,"exhausted":page.exhausted,"stale_cursor":page.stale_cursor,"budget_exhausted":page.budget_exhausted})
}
/// A parameter schema larger than this is left to `tilo_resolve` in a
/// browsed page: one rich-text `apply` schema is tens of kilobytes, and an
/// agent pays for every byte of every page it reads.
const BROWSED_SCHEMA_BYTES: usize = 1024;
/// [`row_json`] with large parameter schemas replaced by a pointer to
/// `tilo_resolve`, which returns them in full.
pub(super) fn browsed_row_json(row: &CatalogueRow) -> Value {
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
pub(super) fn row_json(row: &CatalogueRow) -> Value {
    let descriptor = row.action.as_ref().map(|action| {
        let mut descriptor = json!({"resource":action.address.resource,"action":action.address.action,"resource_kind":action.resource_kind,"resource_label":action.resource_label,"action_label":action.action_label,"description":action.description,"operation_kind":action.kind,"params":action.params,"contract":action.contract});
        // What it writes besides its resource, where it declares any.
        if let Some(writes) = action.writes()
            && let Some(fields) = descriptor.as_object_mut()
        {
            fields.insert("writes".into(), json!(writes));
        }
        descriptor
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

/// How many resources an error names before it stops.
const LISTED_RESOURCES: usize = 16;

/// `rows`' resources as `label (kind)`, each once.
pub(super) fn resource_list(rows: &[CatalogueRow]) -> String {
    let mut seen = Vec::<&str>::new();
    let mut listed = Vec::<String>::new();
    for row in rows {
        if seen.contains(&row.resource.id.as_str()) {
            continue;
        }
        seen.push(&row.resource.id);
        if listed.len() == LISTED_RESOURCES {
            listed.push("…".into());
            break;
        }
        listed.push(format!("{} ({})", row.resource.label, row.resource.kind));
    }
    if listed.is_empty() {
        "it lists none".into()
    } else {
        listed.join(", ")
    }
}

/// Offer `path` on the workspace's application `open` actions, which this
/// bridge opens through the launched app's own action.
pub(super) fn offer_open_path(page: &mut CataloguePage) {
    for action in page.rows.iter_mut().filter_map(|row| row.action.as_mut()) {
        let app = action
            .address
            .resource
            .strip_prefix(tilo_actions::remote::APPLICATION_RESOURCE_PREFIX);
        if action.address.action == tilo_actions::remote::APPLICATION_OPEN_ACTION
            && app.is_some_and(|app| !app.starts_with("host:"))
            && !action.params.iter().any(|param| param.name == "path")
        {
            action.params.push(tilo_actions::ActionParam::optional(
                "path",
                tilo_actions::ParamType::Text,
                "A file to open in it; the app reads only files it was granted",
            ));
        }
    }
}
