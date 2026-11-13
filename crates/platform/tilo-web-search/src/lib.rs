//! Keyless web search and page reading, run on the device.
//!
//! [`search`] asks several public search engines the same question at once, reads each
//! one's ordinary result page and merges their rankings; [`fetch`] downloads one page and
//! reduces it to readable text, or hands a body that is not text back whole. Neither
//! needs an account, an API key or a hosted service.
//!
//! Most engine scrapers and the merge are ported from metasearch2 by mat-1 (CC0-1.0);
//! Google is asked the way `SearXNG`'s `google_cse` engine asks it (AGPL-3.0-or-later).
//! `PROVENANCE.md` beside this crate records both.

pub mod engines;
mod fetch;
#[cfg(all(feature = "fixture-servers", not(target_family = "wasm")))]
pub mod fixtures;
mod guard;
pub mod html;
pub mod merge;
pub mod readable;
mod search;
pub mod urls;

pub use engines::{Engine, EngineError, EngineResult, SafeMode};
#[cfg(feature = "fixture-servers")]
pub use fetch::fetch_fixture;
pub use fetch::{FetchError, FetchLimits, FetchedBody, FetchedPage, fetch};
pub use guard::{Refusal, check_address, check_url};
pub use merge::SearchResult;
#[cfg(feature = "fixture-servers")]
pub use search::search_fixture;
pub use search::{
    EngineReport, EngineStatus, FIRST_BACKOFF, MAX_BACKOFF, MAX_QUERY_CHARS, SearchError,
    SearchLimits, SearchResponse, SearchState, search,
};

use std::time::Duration;

/// The `User-Agent` every request carries: says what is asking and where to
/// read about it, rather than impersonating a browser.
pub const USER_AGENT: &str = "Mozilla/5.0 (compatible; Tilo-Web-Search/0.1; +https://tilo.so)";

pub fn client() -> Result<tilo_net::Client, tilo_net::NetError> {
    tilo_net::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(5))
        .redirects(tilo_net::Redirects::None)
        .default_header("accept-language", "en-US,en;q=0.7")
        .build()
}
