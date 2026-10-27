//! What an app's search ([`SearchScope::ActiveApp`]) arranges beyond the
//! fan-out every scope shares.
//!
//! - The focused window answers for itself when the shell can read it
//!   ([`super::providers::focused_window`]); the broker's copy of the same
//!   app's catalogue ([`super::SearchProvider::gives_way_to_window`]) then steps
//!   aside, so no action is listed twice. A window the shell cannot read
//!   (another process's) is still searched through the broker.
//! - Actions acting on the person's selection lead what can be done.

use super::{KindGroup, ProviderQuery, QueryArgs, SearchContext, SearchScope};

/// Whether a provider answered `args` from the focused window itself.
pub(super) fn window_answers(
    sources: &[ProviderQuery<'_>],
    args: &QueryArgs<'_>,
    include_programmatic: bool,
) -> bool {
    args.scope == SearchScope::ActiveApp
        && sources.iter().any(|source| {
            !source.results.is_empty()
                && source.provider.speaks_for_active_window(&SearchContext {
                    query_scope: Some(args.scope),
                    include_programmatic,
                    needle: args.needle,
                    wanted: &source.wanted,
                    chrome: args.chrome,
                    doc: args.doc,
                    doc_root: args.doc_root,
                })
        })
}

/// Order each group of `actions` so those whose contract declares the
/// selection as their precondition lead the others, each part keeping its
/// order.
pub(super) fn selection_first(actions: &mut [KindGroup]) {
    for group in actions {
        group.results.sort_by_key(|row| {
            !row.contract.as_ref().is_some_and(|contract| {
                contract
                    .requirements()
                    .contains(&tilo_actions::ContextRequirement::Selection)
            })
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::search::{ResourceKind, SearchResult};

    fn action(id: &str, selection: bool) -> SearchResult {
        let mut row =
            SearchResult::nullary(ResourceKind::Action, "p", id, id, None, 50, |_, _, _| {});
        let mut contract = tilo_actions::ActionContract::default();
        if selection {
            contract
                .tags
                .push(tilo_actions::ContextRequirement::Selection.tag().into());
        }
        row.contract = Some(contract);
        row
    }

    #[test]
    fn actions_on_the_selection_lead_and_the_rest_keep_their_order() {
        let mut groups = vec![KindGroup {
            kind: ResourceKind::Action,
            total: 4,
            results: vec![
                action("new", false),
                action("copy", true),
                action("rename", false),
                action("cut", true),
            ],
        }];
        selection_first(&mut groups);
        let ids: Vec<&str> = groups[0]
            .results
            .iter()
            .map(|row| row.id.as_str())
            .collect();
        assert_eq!(ids, ["copy", "cut", "new", "rename"]);
    }
}
