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
#[cfg(all(feature = "fixture-servers", not(target_family = "wasm")))]
pub use fetch::fetch_looking_up;
pub use fetch::{FetchError, FetchLimits, FetchedBody, FetchedPage, fetch};
pub use guard::{Refusal, check_address, check_url};
#[cfg(not(target_family = "wasm"))]
pub use guard::{public_only, system_lookup};
pub use merge::SearchResult;
pub use readable::NeedsBrowser;
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

/// The client every search and fetch goes out through. It dials a name only
/// at the addresses its own lookup answered and found on the public internet
/// ([`public_only`]): that lookup is the connection's, so nothing resolves
/// the name a second time behind the check.
pub fn client() -> Result<tilo_net::Client, tilo_net::NetError> {
    #[cfg(not(target_family = "wasm"))]
    let builder = builder().lookup(public_only(system_lookup()));
    #[cfg(target_family = "wasm")]
    let builder = builder();
    builder.build()
}

/// [`client`] for a server that reads a page for somebody: it dials every
/// address itself and takes no proxy from its environment. A proxy resolves
/// the names sent through it by a lookup of its own, behind the one this
/// client judges ([`public_only`]), so that refusal does not hold for what a
/// proxy reaches. [`client`] takes the proxy its environment names, as before.
#[cfg(not(target_family = "wasm"))]
pub fn direct_client() -> Result<tilo_net::Client, tilo_net::NetError> {
    builder()
        .direct()
        .lookup(public_only(system_lookup()))
        .build()
}

/// [`direct_client`], resolving names by `lookup` in place of the system's
/// resolver and under the same refusal: for a test whose names no resolver
/// knows.
#[cfg(all(feature = "fixture-servers", not(target_family = "wasm")))]
pub fn direct_client_looking_up(
    lookup: tilo_net::Lookup,
) -> Result<tilo_net::Client, tilo_net::NetError> {
    builder().direct().lookup(public_only(lookup)).build()
}

/// [`client`], resolving names by `lookup` in place of the system's resolver
/// and under the same refusal: for a test whose names no resolver knows.
#[cfg(all(feature = "fixture-servers", not(target_family = "wasm")))]
pub fn client_looking_up(lookup: tilo_net::Lookup) -> Result<tilo_net::Client, tilo_net::NetError> {
    builder().lookup(public_only(lookup)).build()
}

fn builder() -> tilo_net::ClientBuilder {
    tilo_net::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(5))
        .redirects(tilo_net::Redirects::None)
        .default_header("accept-language", "en-US,en;q=0.7")
}
