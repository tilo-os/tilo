//! What the palette's rows and field read: the facets several providers merged into one, the words a
//! row is found by, and the typed text with a facet's token taken out of it.

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

/// Whether `action` only shows an item as picking its search hit does: the hit is the row for that, so
/// the palette lists no second one.
pub(crate) fn shows_a_hit(action: &tilo_actions::ActionSpec) -> bool {
    action.operation_type.as_deref() == Some(tilo_actions::remote::APPLICATION_OPEN_TYPE)
}

/// Whether the palette may offer this action of `fa`'s resource to a person at all: listed for people
/// ([`listed_for_people`](tilo_actions::listed_for_people)), available in the window's state now, and
/// no second row for a search hit open.
pub(crate) fn offers_a_person(fa: &tilo_actions::FlatAction<'_>) -> bool {
    fa.action.contract.availability.is_available()
        && fa.action.reaches_people()
        && tilo_actions::listed_for_people(&fa.resource_kind, &fa.resource_label)
        && !shows_a_hit(fa.action)
}

/// Whether the palette lists this action of `fa`'s resource as a row: what a row can take itself
/// ([`a_row_a_person_can_take`](crate::search::system::a_row_a_person_can_take)).
pub(crate) fn lists_a_row(fa: &tilo_actions::FlatAction<'_>) -> bool {
    offers_a_person(fa) && fa.action.takes_a_row_answer()
}
