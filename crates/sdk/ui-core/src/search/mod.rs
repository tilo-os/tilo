//! Search as a platform service.
//!
//! Apps never draw a search field or a result list. An app declares what it
//! can find and the platform presents it:
//!
//! - **Providers.** A [`SearchProvider`] per findable kind answers bounded,
//!   paged queries ([`SearchQuery`]: text, [`Facet`] filters, scope). A slow
//!   source returns [`SearchPage::pending`] and finishes through
//!   [`SearchProvider::poll_page`].
//! - **Rows.** A hit says what it is with a [`SearchRow`] (a message, a video,
//!   a file hit); the generic row is the fallback.
//! - **Opening search.** [`search_button`] / [`open_search_binding`] open the
//!   one platform palette narrowed to the app or one of its parts
//!   ([`SearchRequest`], [`ScopeDecl`]); `/` and Ctrl+K do the same.
//! - **In-content narrowing** is a [`SearchField`](crate::widget::SearchField)
//!   bound to a [`Filterable`](crate::widget::Filterable) list, and **find in
//!   a document** is the [`FindBar`](crate::widget::FindBar) over a
//!   [`Findable`](crate::widget::Findable) view. Neither searches the app.
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
pub mod index_publication;
mod ordinals;
mod provider;
mod query;
pub mod rank;
mod registry;
pub(crate) mod request;
mod row;
#[cfg(any(test, feature = "test-support"))]
pub mod testing;

pub use body::{BodyPiece, SearchBody, SearchBodySource};
pub use provider::{
    CatalogSearchProvider, DEFAULT_SEARCH_CANDIDATE_LIMIT, DEFAULT_SEARCH_INDEX_LIMIT,
    DEFAULT_SEARCH_RESULT_LIMIT, MAX_SEARCH_QUERY_SCALARS, OpenResourceSearch, PagePoll,
    PendingToken, SearchCatalog, SearchCatalogError, SearchCatalogStats, SearchCursor, SearchPage,
    SearchProvider, SearchRecord, SearchResult, normalize_search_text,
};
pub use query::{Facet, FacetFilter, FacetValue, SearchQuery};
pub use rank::{FieldWeight, MatchTier, result_relevance};
pub use registry::{FnProvider, SearchGroup, SearchRegistry};
pub use request::{
    ScopeDecl, SearchOffer, SearchOpener, SearchRequest, SearchRequestScope, open_search_binding,
    open_search_invocation, scope_contains, search_button,
};
pub(crate) use request::{search_address, search_resource};
pub use row::SearchRow;
/// Fold text the way every index does: case, compatibility forms and
/// alphabetic diacritics.
pub use tilo_search::fold as fold_search_text;
pub use tilo_search::{Frecency, ListIndex, MAX_FIELD_RANK, Matching, fold_char, initials, locate};
/// Whether no search waits for the engine, and when to search again: where
/// the engine's code is fetched on first use (the browser desktop), a
/// search of a large index finds nothing until it is in.
pub use tilo_search::{engine_generation, engine_ready};

pub mod system;
