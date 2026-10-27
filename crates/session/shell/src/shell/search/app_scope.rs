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
