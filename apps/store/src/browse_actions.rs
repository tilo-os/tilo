//! Agent-visible, device-local Store browsing operations.
//!
//! Search and facets change only this window's presentation. They are still
//! published as stable `View` actions so the palette, keyboard users, and
//! agents can drive exactly the same catalogue state without creating a
//! collaborative document mutation or history entry.

use serde::{Deserialize, Serialize};
use tilo_actions::{
    ActionArgs, ActionBinding, ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType,
    Resource,
};

pub const STORE_BROWSE_OPERATION_TYPE: &str = "store.browse";
pub const BROWSE_RESOURCE_ID: &str = "store:filters";
const SOURCE_FILTERS: [&str; 4] = ["All", "Tilo", "Flatpak", "On device"];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BrowseOperation {
    SetSearch { query: String },
    SetSource { source: String },
    SetCategory { category: String },
    Clear,
}

/// # Errors
///
/// Returns an error when a payload claiming the Store browse operation type
/// cannot be decoded as a [`BrowseOperation`].
pub fn decode(type_id: &str, payload: &[u8]) -> Result<Option<BrowseOperation>, String> {
    if type_id != STORE_BROWSE_OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|error| error.to_string())
}

#[allow(
    clippy::expect_used,
    reason = "browse actions have serde-only payloads"
)]
fn typed(
    id: &str,
    label: &str,
    description: &str,
    params: Vec<ActionParam>,
    make: impl Fn(&ActionArgs) -> BrowseOperation + Send + Sync + 'static,
) -> ActionSpec {
    ActionSpec::typed_operation(
        id,
        label,
        description,
        OperationKind::View,
        STORE_BROWSE_OPERATION_TYPE,
        params,
        move |args| {
            EncodedAction::encode_operation(STORE_BROWSE_OPERATION_TYPE, &make(args))
                .expect("browse operation serializes")
        },
    )
}

/// The stable local browsing surface. `categories` is a bounded, current
/// catalogue projection; an explicit text parameter keeps agents able to
/// select any valid value without mirroring the catalogue into the document.
#[allow(
    clippy::expect_used,
    clippy::missing_panics_doc,
    reason = "the fixed browse operation is serde-only and cannot fail to encode"
)]
pub fn resource(categories: &[String]) -> Resource {
    let mut category_choices = vec!["All".to_owned()];
    category_choices.extend(
        categories
            .iter()
            .filter(|category| category.as_str() != "All")
            .cloned(),
    );
    Resource::new(
        BROWSE_RESOURCE_ID,
        "store.browse",
        "This window's catalogue view",
    )
    .with_action(typed(
        "set-search",
        "Search apps",
        "app name, publisher, category, or description",
        vec![ActionParam::required(
            "query",
            ParamType::Text,
            "Search query",
        )],
        |args| BrowseOperation::SetSearch {
            query: args.text("query").unwrap_or_default().to_owned(),
        },
    ))
    .with_action(typed(
        "set-source",
        "Filter apps by source",
        "All, Tilo, Flatpak, or Installed",
        vec![
            ActionParam::required("source", ParamType::Text, "App source")
                .with_choices(SOURCE_FILTERS),
        ],
        |args| BrowseOperation::SetSource {
            source: args.text("source").unwrap_or("All").to_owned(),
        },
    ))
    .with_action(typed(
        "set-category",
        "Filter apps by category",
        "App category, or All",
        vec![
            ActionParam::required("category", ParamType::Text, "App category")
                .with_choices(category_choices),
        ],
        |args| BrowseOperation::SetCategory {
            category: args.text("category").unwrap_or("All").to_owned(),
        },
    ))
    .with_action(
        ActionSpec::operation(
            "clear",
            "Clear catalogue filters",
            OperationKind::View,
            STORE_BROWSE_OPERATION_TYPE,
            &BrowseOperation::Clear,
        )
        .expect("browse operation serializes"),
    )
}

#[allow(
    clippy::expect_used,
    clippy::missing_panics_doc,
    reason = "these bindings reference fixed actions generated immediately above"
)]
fn binding<T>(
    categories: &[String],
    action_id: &str,
    args: impl Fn(T) -> serde_json::Map<String, serde_json::Value> + Send + Sync + 'static,
) -> ActionBinding<T> {
    let resource = resource(categories);
    let spec = resource
        .actions
        .iter()
        .find(|action| action.id == action_id)
        .expect("browse resource contains every requested action");
    ActionBinding::new(&resource, spec, args).expect("browse binding matches its action")
}

pub fn search_binding(categories: &[String]) -> ActionBinding<String> {
    binding(categories, "set-search", |query| {
        serde_json::Map::from_iter([("query".into(), serde_json::Value::String(query))])
    })
}

pub fn source_binding(categories: &[String]) -> ActionBinding<String> {
    binding(categories, "set-source", |source| {
        serde_json::Map::from_iter([("source".into(), serde_json::Value::String(source))])
    })
}

pub fn category_binding(categories: &[String]) -> ActionBinding<String> {
    binding(categories, "set-category", |category| {
        serde_json::Map::from_iter([("category".into(), serde_json::Value::String(category))])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_contains_only_local_view_actions() {
        let resource = resource(&["Games".into()]);
        assert!(
            resource
                .actions
                .iter()
                .all(|action| action.kind == OperationKind::View)
        );
        assert!(
            resource
                .actions
                .iter()
                .any(|action| action.id == "set-search")
        );
    }
}
