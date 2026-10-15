//! Keyless web search and page reading, run on the device.
//!
//! [`search`] asks several public search engines the same question at once,
//! reads each one's ordinary result page and merges their rankings; [`fetch`]
//! downloads one page and reduces it to readable text. Neither needs an
//! account, an API key or a hosted service: the device talks to the engines
//! and pages directly, as a browser would.
//!
//! The engine scrapers and the merge are ported from
//! [metasearch2](https://github.com/mat-1/metasearch2) by mat-1, which is
//! dedicated to the public domain under CC0-1.0; `PROVENANCE.md` beside this
//! crate records the upstream revision and what changed.
//!
//! Everything a remote server sends is untrusted: pages are parsed by a
//! bounded, forgiving reader ([`html`]) that never runs or fetches anything,
//! every download has a size cap and a deadline, and one engine failing or
//! asking for a CAPTCHA never fails a search the others answered.

pub mod engines;
mod fetch;
mod guard;
pub mod html;
pub mod merge;
pub mod readable;
mod search;
pub mod urls;

pub use engines::{Engine, EngineError, EngineResult};
pub use fetch::{FetchError, FetchLimits, FetchedPage, fetch};
pub use guard::{Refusal, check_address, check_url};
pub use merge::SearchResult;
pub use search::{
    EngineReport, EngineStatus, MAX_QUERY_CHARS, SearchError, SearchLimits, SearchResponse, search,
};

use std::time::Duration;

/// The `User-Agent` every request carries: says what is asking and where to
/// read about it, rather than impersonating a browser.
pub const USER_AGENT: &str = "Mozilla/5.0 (compatible; Tilo-Web-Search/0.1; +https://tilo.so)";

/// The HTTP client searches and fetches use: Tilo's TLS stack, this crate's
/// user agent, no redirects followed implicitly (each hop of a fetch is
/// checked by [`check_url`] first), and a connect deadline.
///
/// # Errors
///
/// [`tilo_net::NetError`] when the TLS stack cannot be set up.
pub fn client() -> Result<tilo_net::Client, tilo_net::NetError> {
    tilo_net::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(5))
        .redirects(tilo_net::Redirects::None)
        .default_header("accept-language", "en-US,en;q=0.7")
        .build()
}
