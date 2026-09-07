//! Replayable Store-document actions (catalogue preferences and reports).

use automerge::transaction::Transactable;
use automerge::{ObjType, ROOT, ReadDoc, Value};
use qos_actions::{
    Action, ActionArgs, ActionBinding, ActionError, ActionParam, ActionRegistry, ActionSpec, Doc,
    EncodedAction, ExecutionLocus, ParamType, Resource,
};
use serde::{Deserialize, Serialize};

pub const STORE_DOCUMENT_ACTION_TYPE: &str = "store.document";
pub const REPORT_RESOURCE_KEY_PREFIX: &str = "store.report_resource.";
pub const SEARCH_QUERY_KEY: &str = "store.search_query";
pub const SOURCE_FILTER_KEY: &str = "store.source_filter";
pub const CATEGORY_FILTER_KEY: &str = "store.category_filter";
const MAX_SEARCH_CHARS: usize = 512;
const MAX_CATEGORY_CHARS: usize = 128;
const SOURCE_FILTERS: [&str; 4] = ["All", "qOS", "Flatpak", "Installed"];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StoreDocumentAction {
    CreateReportDraft { app_id: String },
    SetSearch { query: String },
    SetSourceFilter { value: String },
    SetCategoryFilter { value: String },
}

impl Action for StoreDocumentAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            Self::CreateReportDraft { app_id } => {
                let app_id = app_id.trim();
                if app_id.is_empty() || app_id.len() > 255 {
                    return Err(ActionError::Refused(
                        "invalid app id for report draft".into(),
                    ));
                }
                doc.put(
                    &ROOT,
                    format!("{REPORT_RESOURCE_KEY_PREFIX}{app_id}"),
                    "Draft · not submitted",
                )?;
            }
            Self::SetSearch { query } => {
                if query.chars().count() > MAX_SEARCH_CHARS {
                    return Err(ActionError::Refused("search query is too long".into()));
                }
                replace_text(doc, SEARCH_QUERY_KEY, query)?;
            }
            Self::SetSourceFilter { value } => {
                if !SOURCE_FILTERS.contains(&value.as_str()) {
                    return Err(ActionError::Refused("unknown Store source filter".into()));
                }
                doc.put(&ROOT, SOURCE_FILTER_KEY, value)?;
            }
            Self::SetCategoryFilter { value } => {
                if value.chars().count() > MAX_CATEGORY_CHARS || value.chars().any(char::is_control)
                {
                    return Err(ActionError::Refused("invalid Store category filter".into()));
                }
                doc.put(&ROOT, CATEGORY_FILTER_KEY, value)?;
            }
        }
        Ok(())
    }
}

fn replace_text(doc: &mut Doc, key: &str, text: &str) -> Result<(), ActionError> {
    if let Some((Value::Object(ObjType::Text), object)) = doc.get(&ROOT, key)? {
        doc.splice_text(&object, 0, doc.length(&object) as isize, text)?;
    } else {
        let object = doc.put_object(&ROOT, key, ObjType::Text)?;
        if !text.is_empty() {
            doc.splice_text(&object, 0, 0, text)?;
        }
    }
    Ok(())
}

#[allow(
    clippy::expect_used,
    reason = "StoreDocumentAction primitives always serialize"
)]
fn encode(action: &StoreDocumentAction) -> EncodedAction {
    EncodedAction::encode(STORE_DOCUMENT_ACTION_TYPE, action)
        .expect("StoreDocumentAction serializes")
}

fn search_spec() -> ActionSpec {
    ActionSpec::typed(
        "set-search",
        "Search apps",
        "app name, publisher, category, or description",
        ExecutionLocus::ClientLocal,
        vec![ActionParam::required(
            "query",
            ParamType::Text,
            "Search query",
        )],
        |args: &ActionArgs| {
            encode(&StoreDocumentAction::SetSearch {
                query: args.text("query").unwrap_or_default().to_owned(),
            })
        },
    )
    .reversible()
}

fn source_filter_spec() -> ActionSpec {
    ActionSpec::typed(
        "set-source-filter",
        "Filter apps by source",
        "All, qOS, Flatpak, or Installed",
        ExecutionLocus::ClientLocal,
        vec![
            ActionParam::required("source", ParamType::Text, "App source")
                .with_choices(SOURCE_FILTERS),
        ],
        |args: &ActionArgs| {
            encode(&StoreDocumentAction::SetSourceFilter {
                value: args.text("source").unwrap_or("All").to_owned(),
            })
        },
    )
    .reversible()
}

fn category_filter_spec(categories: &[String]) -> ActionSpec {
    let mut choices = vec!["All".to_owned()];
    choices.extend(
        categories
            .iter()
            .filter(|value| value.as_str() != "All")
            .cloned(),
    );
    ActionSpec::typed(
        "set-category-filter",
        "Filter apps by category",
        "app category",
        ExecutionLocus::ClientLocal,
        vec![
            ActionParam::required("category", ParamType::Text, "App category")
                .with_choices(choices.clone()),
        ],
        |args: &ActionArgs| {
            let selected = args.text("category").unwrap_or("All");
            encode(&StoreDocumentAction::SetCategoryFilter {
                value: if selected == "All" { "" } else { selected }.to_owned(),
            })
        },
    )
    .reversible()
}

/// Canonical collaborative catalogue preferences. The same resource backs
/// Store controls, global search, agents, history, undo, and peer replay.
pub fn preferences_resource(categories: &[String]) -> Resource {
    Resource::new("store:filters", "store.filters", "Catalogue filters")
        .with_action(search_spec())
        .with_action(source_filter_spec())
        .with_action(category_filter_spec(categories))
}

#[allow(
    clippy::expect_used,
    clippy::missing_panics_doc,
    reason = "the canonical filters resource always exports search"
)]
pub fn search_binding(categories: &[String]) -> ActionBinding<String> {
    let resource = preferences_resource(categories);
    let spec = resource
        .actions
        .iter()
        .find(|action| action.id == "set-search")
        .expect("search action exists");
    ActionBinding::new(&resource, spec, |query| {
        serde_json::Map::from_iter([("query".into(), serde_json::Value::String(query))])
    })
    .expect("search binding matches its action")
}

#[allow(
    clippy::expect_used,
    clippy::missing_panics_doc,
    reason = "the canonical filters resource always exports source"
)]
pub fn source_filter_binding(categories: &[String]) -> ActionBinding<usize> {
    let resource = preferences_resource(categories);
    let spec = resource
        .actions
        .iter()
        .find(|action| action.id == "set-source-filter")
        .expect("source filter action exists");
    ActionBinding::new(&resource, spec, |index| {
        let value = SOURCE_FILTERS.get(index).copied().unwrap_or("All");
        serde_json::Map::from_iter([("source".into(), serde_json::Value::String(value.into()))])
    })
    .expect("source filter binding matches its action")
}

#[allow(
    clippy::expect_used,
    clippy::missing_panics_doc,
    reason = "the canonical filters resource always exports source"
)]
pub fn source_filter_value_binding(categories: &[String]) -> ActionBinding<String> {
    let resource = preferences_resource(categories);
    let spec = resource
        .actions
        .iter()
        .find(|action| action.id == "set-source-filter")
        .expect("source filter action exists");
    ActionBinding::new(&resource, spec, |source| {
        serde_json::Map::from_iter([("source".into(), serde_json::Value::String(source))])
    })
    .expect("source filter binding matches its action")
}

#[allow(
    clippy::expect_used,
    clippy::missing_panics_doc,
    reason = "the canonical filters resource always exports category"
)]
pub fn category_filter_binding(categories: &[String]) -> ActionBinding<usize> {
    let resource = preferences_resource(categories);
    let spec = resource
        .actions
        .iter()
        .find(|action| action.id == "set-category-filter")
        .expect("category filter action exists");
    let choices = std::iter::once("All".to_owned())
        .chain(
            categories
                .iter()
                .filter(|value| value.as_str() != "All")
                .cloned(),
        )
        .collect::<Vec<_>>();
    ActionBinding::new(&resource, spec, move |index| {
        let value = choices.get(index).cloned().unwrap_or_else(|| "All".into());
        serde_json::Map::from_iter([("category".into(), serde_json::Value::String(value))])
    })
    .expect("category filter binding matches its action")
}

#[allow(
    clippy::expect_used,
    clippy::missing_panics_doc,
    reason = "the canonical filters resource always exports category"
)]
pub fn category_filter_value_binding(categories: &[String]) -> ActionBinding<String> {
    let resource = preferences_resource(categories);
    let spec = resource
        .actions
        .iter()
        .find(|action| action.id == "set-category-filter")
        .expect("category filter action exists");
    ActionBinding::new(&resource, spec, |category| {
        serde_json::Map::from_iter([("category".into(), serde_json::Value::String(category))])
    })
    .expect("category filter binding matches its action")
}

pub fn register(registry: &mut ActionRegistry) {
    registry.register::<StoreDocumentAction>(STORE_DOCUMENT_ACTION_TYPE);
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::ReadDoc;

    #[test]
    fn report_draft_is_a_replayable_document_resource() {
        let mut doc = Doc::new();
        StoreDocumentAction::CreateReportDraft {
            app_id: "dev.qos.notes".into(),
        }
        .apply(&mut doc)
        .unwrap();
        assert!(
            doc.get(&ROOT, "store.report_resource.dev.qos.notes")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn catalogue_preferences_are_replayable_and_bounded() {
        let mut doc = Doc::new();
        StoreDocumentAction::SetSearch {
            query: "drawing".into(),
        }
        .apply(&mut doc)
        .unwrap();
        StoreDocumentAction::SetSourceFilter {
            value: "Flatpak".into(),
        }
        .apply(&mut doc)
        .unwrap();
        StoreDocumentAction::SetCategoryFilter {
            value: "Graphics".into(),
        }
        .apply(&mut doc)
        .unwrap();
        let (_, search) = doc.get(&ROOT, SEARCH_QUERY_KEY).unwrap().unwrap();
        assert_eq!(doc.text(&search).unwrap(), "drawing");
        assert!(matches!(
            doc.get(&ROOT, SOURCE_FILTER_KEY).unwrap(),
            Some((Value::Scalar(value), _)) if value.to_str() == Some("Flatpak")
        ));
        assert!(
            StoreDocumentAction::SetSourceFilter {
                value: "Anything".into()
            }
            .apply(&mut doc)
            .is_err()
        );
    }
}
