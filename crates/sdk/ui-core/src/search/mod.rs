//! Shared global-search framework for Tilo.
//!
//! Rather than each app rolling its own search bar, result list, and
//! ranking, an app declares what it can find by implementing
//! [`SearchProvider`], and the framework queries, merges, and ranks.
//! Paired with [`SearchField`](crate::widget::SearchField) and
//! [`SearchResultsView`](crate::widget::SearchResultsView), that's a
//! consistent OS-styled search surface for free — and lets a future
//! shell-level Ctrl+K drive the same [`SearchRegistry`] across every app.
//!
//! ```
//! use tilo_ui::search::{SearchRegistry, SearchResult};
//!
//! let mut registry = SearchRegistry::new();
//! registry.register_fn("Contacts", |q: &str| {
//!     ["Alice Ng", "Bob Reyes"]
//!         .into_iter()
//!         .filter(|name| name.to_lowercase().contains(&q.to_lowercase()))
//!         .map(|name| SearchResult::new("Contacts", name, name))
//!         .collect()
//! });
//!
//! let groups = registry.query_grouped("al");
//! assert_eq!(groups[0].kind, "Contacts");
//! assert_eq!(groups[0].results[0].title, "Alice Ng");
//! ```

mod body;
pub mod capability;
mod provider;
pub mod rank;
mod registry;
#[cfg(any(test, feature = "test-support"))]
pub mod testing;

pub use body::{BodyPiece, SearchBody, SearchBodySource};
pub use provider::{
    CatalogSearchProvider, DEFAULT_SEARCH_CANDIDATE_LIMIT, DEFAULT_SEARCH_INDEX_LIMIT,
    DEFAULT_SEARCH_RESULT_LIMIT, MAX_SEARCH_QUERY_SCALARS, OpenResourceSearch, SearchCatalog,
    SearchCatalogError, SearchCatalogStats, SearchCursor, SearchPage, SearchProvider, SearchRecord,
    SearchResult, normalize_search_text,
};
pub use rank::{FieldWeight, MatchTier, result_relevance};
pub use registry::{FnProvider, SearchGroup, SearchRegistry};
/// Fold text the way every index does: case, compatibility forms and
/// alphabetic diacritics.
pub use tilo_search::fold as fold_search_text;
pub use tilo_search::{ListIndex, MAX_FIELD_RANK, Matching, fold_char, locate};

pub mod system;
