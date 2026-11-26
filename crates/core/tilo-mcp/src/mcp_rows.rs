//! A catalogue page and its rows as the JSON an agent reads.

use tilo_actions::remote::CatalogueRow;

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

