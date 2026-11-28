//! Search as a platform service: apps never draw a search field or a result list, they declare
//! what they can find and the platform presents it.
//!
//! - **Providers.** A [`SearchProvider`] per findable kind answers bounded, paged queries
//!   ([`SearchQuery`]: text, [`Facet`] filters, scope); a slow source returns
//!   [`SearchPage::pending`] and finishes through [`SearchProvider::poll_page`].
//! - **Rows.** A hit says what it is with a [`SearchRow`]; the generic row is the fallback.
//! - **Opening search.**
//!   [`search_button`](tilo_ui::widget::search_request::search_button) /
//!   [`open_search_binding`](tilo_ui::widget::search_request::open_search_binding) open the one
//!   platform palette narrowed to the app or one of its parts
//!   ([`SearchRequest`](tilo_ui::widget::search_request::SearchRequest),
//!   [`ScopeDecl`](tilo_ui::widget::search_request::ScopeDecl)); `/` and Ctrl+K do the same.
//! - **In-content narrowing** is a [`SearchField`](tilo_ui::widget::SearchField) bound to a
//!   [`Filterable`](tilo_ui::widget::Filterable) list, and **find in a document** is the
//!   [`FindBar`](tilo_ui::widget::FindBar) over a [`Findable`](tilo_ui::widget::Findable) view;
//!   neither searches the app.
//!
//! ```
//! use crate::search::{SearchRegistry, SearchResult};
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
mod button_component;
pub mod capability;
mod command;
pub mod index_publication;
mod ordinals;
mod preview;
mod provider;
mod query;
pub mod rank;
mod registry;
mod row;
pub mod scope;
#[cfg(any(test, feature = "test-support"))]
pub mod testing;

pub use body::{BodyPiece, SearchBody, SearchBodySource};
pub use button_component::{SearchButton, SearchButtonProps};
pub use command::{ArgumentKind, ArgumentSpec, CommandEntry, CommandOutcome};
pub use preview::{PREVIEW_BODY_LINES, PREVIEW_LINES, SearchPreview};
pub use provider::{
    CatalogSearchProvider, DEFAULT_SEARCH_CANDIDATE_LIMIT, DEFAULT_SEARCH_RESULT_LIMIT,
    MAX_SEARCH_QUERY_SCALARS, OpenResourceSearch, PagePoll, PendingToken, SearchCatalog,
    SearchCatalogError, SearchCatalogStats, SearchCursor, SearchItem, SearchPage, SearchProvider,
    SearchResult,
};
pub use query::{Facet, FacetFilter, FacetValue, SearchQuery};
pub use rank::{MatchTier, result_relevance};
pub use registry::{FnProvider, SearchGroup, SearchRegistry, group_results, rank_results};
pub use row::{ActivationModifier, Alternate, CommandRow, RowFace, SearchRow};
pub use scope::{AppScope, Scope, WindowGroupId};

pub mod system;
