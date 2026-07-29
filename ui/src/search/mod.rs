//! Shared global-search framework for qOS.
//!
//! A dozen apps (`music`, `mail`, `tube`, `contacts`, `file-explorer`,
//! `software_store`, …) each roll their own search bar, their own result list,
//! and their own ranking. This module is the seam that kills that redundancy:
//! an app **declares what it can find** by implementing [`SearchProvider`], and
//! the framework does the querying, merging, and ranking. Paired with the
//! shared [`SearchField`](crate::widget::SearchField) input and the
//! [`SearchResultsView`](crate::widget::SearchResultsView) overlay, an app gets
//! a consistent, OS-styled search surface for free — and a future shell-level
//! Ctrl+K can drive the very same [`SearchRegistry`] across every app's
//! providers.
//!
//! ```
//! use ui::search::{SearchRegistry, SearchResult};
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

mod provider;
mod registry;

pub use provider::{fuzzy_score, SearchProvider, SearchResult};
pub use registry::{FnProvider, SearchGroup, SearchRegistry};
