//! Browser settings: the preferences the browser document holds.

use tilo_resource::{self, Model, Refusal};

/// The default search-engine query template; the `?q=` suffix lets the chrome build
/// a search URL by appending typed terms.
pub const DEFAULT_SEARCH_ENGINE: &str = "https://duckduckgo.com/?q=";
pub const DEFAULT_HOMEPAGE: &str = "https://duckduckgo.com";

/// The search engines offered by name, each with its query template. A template set
/// some other way stays selected and is shown by its host.
pub const SEARCH_ENGINES: &[(&str, &str)] = &[
    ("DuckDuckGo", DEFAULT_SEARCH_ENGINE),
    ("Google", "https://www.google.com/search?q="),
    ("Bing", "https://www.bing.com/search?q="),
    ("Brave Search", "https://search.brave.com/search?q="),
    ("Ecosia", "https://www.ecosia.org/search?q="),
    ("Startpage", "https://www.startpage.com/do/search?q="),
    ("Kagi", "https://kagi.com/search?q="),
];

/// Durable browser preferences, as the browser document holds them.
///
/// A new resource starts with ad-blocking on, clear-on-exit off and `DuckDuckGo` for
/// both search and homepage. The homepage and search engine are always absolute
/// http(s) addresses.
#[derive(Clone, Debug, PartialEq, Eq, Model)]
#[model(check = Settings::valid)]
pub struct Settings {
    pub adblock_enabled: bool,
    pub clear_on_exit: bool,
    pub search_engine: String,
    pub homepage: String,
}

impl Settings {
    pub fn valid(&self) -> Result<(), Refusal> {
        if crate::urls::normalized_bookmark_url(&self.homepage).is_none() {
            return Err(Refusal::new("the homepage is an absolute http(s) URL"));
        }
        if crate::urls::normalized_bookmark_url(&self.search_engine).is_none() {
            return Err(Refusal::new("the search engine is an absolute http(s) URL"));
        }
        Ok(())
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            adblock_enabled: true,
            clear_on_exit: false,
            search_engine: DEFAULT_SEARCH_ENGINE.to_string(),
            homepage: DEFAULT_HOMEPAGE.to_string(),
        }
    }
}
