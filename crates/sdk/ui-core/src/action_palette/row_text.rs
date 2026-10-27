//! What the palette's rows and field read: the facets several providers
//! merged into one, the words a row is found by, and the typed text with a
//! facet's token taken out of it.

use super::{Facet, FacetFilter, ScopeDecl, SearchProvider};

/// Every provider's facets, merged by key in first-declared order.
pub(crate) fn merged_facets(providers: &[Box<dyn SearchProvider>]) -> Vec<Facet> {
    let mut merged: Vec<Facet> = Vec::new();
    for facet in providers.iter().flat_map(|provider| provider.facets()) {
        if let Some(existing) = merged.iter_mut().find(|known| known.key == facet.key) {
            for value in facet.values {
                if !existing
                    .values
                    .iter()
                    .any(|known| known.value == value.value)
                {
                    existing.values.push(value);
                }
            }
        } else {
            merged.push(facet);
        }
    }
    merged
}

/// What the field's scope token reads.
pub(crate) fn scope_token_label(scope: &ScopeDecl) -> String {
    format!("Only {}", scope.label)
}

/// `text` without the typed token(s) for `filter`.
pub(crate) fn without_token(text: &str, filter: &FacetFilter) -> String {
    text.split_whitespace()
        .filter(|token| {
            token.split_once(':').is_none_or(|(key, value)| {
                !(key.eq_ignore_ascii_case(&filter.key)
                    && value.trim_matches('"').eq_ignore_ascii_case(&filter.value))
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The words besides its title a row is found by: what it acts on (named
/// under it or not: "Apply" of the open "Sealed" is found by "sealed"). Never
/// its description, which is written for agents and would pull unrelated
/// actions into a query.
pub(crate) fn found_by(row: &tilo_actions::ListedRow, resource: &str) -> Option<String> {
    let resource = Some(resource).filter(|name| Some(*name) != row.subtitle.as_deref());
    let words = [row.subtitle.as_deref(), resource]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    (!words.is_empty()).then_some(words)
}

/// Whether `action` only shows an item as picking its search hit does: the
/// hit is the row for that, so the palette lists no second one.
pub(crate) fn shows_a_hit(action: &tilo_actions::ActionSpec) -> bool {
    action.operation_type.as_deref() == Some(tilo_actions::remote::APPLICATION_OPEN_TYPE)
}
