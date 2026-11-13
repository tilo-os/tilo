//! The search engines: how each is asked, and how its answer page is read.
//!
//! Every engine works without an account or key, reading the same result page a
//! browser gets. The Bing, Brave and Marginalia scrapers come from metasearch2
//! (CC0-1.0), `DuckDuckGo` is Tilo's own, and Google is asked the way `SearXNG`
//! does (AGPL-3.0-or-later); `PROVENANCE.md` records both.
//!
//! A search may ask for filtered results ([`SafeMode::Strict`]): an engine without
//! a filter is reported as skipped and named, never asked unfiltered. An answer is
//! never trusted to have the expected shape: an engine showing a challenge is
//! [`EngineError::Blocked`], one asking for fewer requests is
//! [`EngineError::RateLimited`] and is left alone for a while, and an answer with no
//! result list where one belongs is [`EngineError::Layout`], so a change to an
//! engine's markup surfaces instead of as silently empty results.

mod bing;
mod brave;
mod duckduckgo;
pub(crate) mod google;
mod marginalia;

use std::fmt;

use serde::Serialize;
use url::Url;

use crate::html::{Document, NodeId, Selector, collapse_whitespace};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SafeMode {
    Off,
    Strict,
}

impl SafeMode {
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Strict => "strict",
        }
    }

    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        [Self::Off, Self::Strict]
            .into_iter()
            .find(|safe| safe.id() == id)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    Bing,
    Brave,
    DuckDuckGo,
    Google,
    Marginalia,
}

impl Engine {
    pub const ALL: [Self; 5] = [
        Self::Google,
        Self::Brave,
        Self::Bing,
        Self::DuckDuckGo,
        Self::Marginalia,
    ];

    /// The engines asked when the caller names none: the ones that answer a
    /// plain HTTP client today, Google first because its ranking is the best.
    /// The others are asked only when named. `DuckDuckGo` answers with a
    /// challenge whenever the client's TLS handshake is not a browser's, and
    /// Marginalia's public endpoint asks clients it does not recognise to
    /// wait.
    pub const DEFAULT: [Self; 3] = [Self::Google, Self::Brave, Self::Bing];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Bing => "bing",
            Self::Brave => "brave",
            Self::DuckDuckGo => "duckduckgo",
            Self::Google => "google",
            Self::Marginalia => "marginalia",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|engine| engine.id() == id)
    }

    /// How much a result's rank on this engine counts when results from
    /// several engines are merged. metasearch2's weights, except that Google
    /// leads, so its ranking and its titles win. `DuckDuckGo`, which largely
    /// relays Bing, counts as Bing does.
    pub const fn weight(self) -> f64 {
        match self {
            Self::Google => 1.5,
            Self::Brave => 1.25,
            Self::Bing | Self::DuckDuckGo => 1.0,
            Self::Marginalia => 0.15,
        }
    }

    /// Whether this engine can be asked for [`SafeMode::Strict`] results.
    /// Marginalia's public endpoint offers no filter, so a filtered search
    /// names it as skipped instead of asking it unfiltered.
    #[must_use]
    pub const fn filters(self) -> bool {
        !matches!(self, Self::Marginalia)
    }

    /// The request that asks this engine `query` under `safe`, or `None` when
    /// the engine does not take such a query. An engine that cannot filter is
    /// never asked with [`SafeMode::Strict`]; the caller skips it.
    pub fn request(self, query: &str, safe: SafeMode) -> Option<EngineRequest> {
        match self {
            Self::Bing => bing::request(query, safe),
            Self::Brave => brave::request(query, safe),
            Self::DuckDuckGo => duckduckgo::request(query, safe),
            Self::Google => google::request(query, safe),
            Self::Marginalia => marginalia::request(query),
        }
    }

    pub fn parse(self, body: &str) -> Result<Vec<EngineResult>, EngineError> {
        if self == Self::Google {
            return google::parse(body);
        }
        let doc = Document::parse(body);
        if let Some(reason) = blocked_reason(self, &doc, body) {
            return Err(EngineError::Blocked(reason));
        }
        match self {
            Self::Bing => bing::parse(&doc),
            Self::Brave => brave::parse(&doc),
            Self::DuckDuckGo => duckduckgo::parse(&doc),
            Self::Google => Err(EngineError::Layout("Google answers with JSON".into())),
            Self::Marginalia => marginalia::parse(&doc),
        }
    }
}

impl fmt::Display for Engine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineRequest {
    pub url: Url,
    pub headers: Vec<(&'static str, String)>,
    pub form: Option<Vec<(&'static str, String)>>,
}

impl EngineRequest {
    /// Ask `base` instead of the engine, with the same query pairs and form
    /// fields: what a fixture search does to stand a server on this machine
    /// in for an engine. Only a build with `fixture-servers` has it: the
    /// routing that passes a base is [`crate::search`]'s own, and only that
    /// build hands it one.
    #[cfg(feature = "fixture-servers")]
    pub(crate) fn point_at(&mut self, base: &str) {
        let Ok(mut url) = Url::parse(base) else {
            return;
        };
        let pairs: Vec<(String, String)> = self
            .url
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        url.query_pairs_mut().extend_pairs(pairs);
        self.url = url;
    }

    /// `GET base?params`. Every base is a constant `https` URL, so `None`
    /// is a typo the request tests catch, never a runtime condition.
    fn get(base: &str, params: &[(&str, &str)]) -> Option<Self> {
        Some(Self {
            url: Url::parse_with_params(base, params).ok()?,
            headers: Vec::new(),
            form: None,
        })
    }

    fn post(url: &str, fields: &[(&'static str, &str)]) -> Option<Self> {
        Some(Self {
            url: Url::parse(url).ok()?,
            headers: Vec::new(),
            form: Some(fields.iter().map(|&(k, v)| (k, v.to_owned())).collect()),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineResult {
    pub url: String,
    pub title: String,
    pub snippet: String,
}

/// Why an engine's page gave no results.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EngineError {
    /// The engine answered with a challenge (a CAPTCHA, a JavaScript check)
    /// instead of results.
    Blocked(String),
    /// The engine says it is getting too many requests. Asking again soon
    /// only makes that worse.
    RateLimited(String),
    Refused(String),
    Layout(String),
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Blocked(reason) => write!(f, "blocked: {reason}"),
            Self::RateLimited(reason) => write!(f, "rate-limited: {reason}"),
            Self::Refused(reason) => write!(f, "refused: {reason}"),
            Self::Layout(reason) => write!(f, "unexpected page: {reason}"),
        }
    }
}

fn blocked_reason(engine: Engine, doc: &Document, body: &str) -> Option<String> {
    let title = page_title(doc).to_ascii_lowercase();
    if title.contains("captcha") || title.contains("are you a robot") {
        return Some(format!("{engine} asked for a CAPTCHA"));
    }
    let markers: &[(&str, &str)] = match engine {
        Engine::Google => &[],
        Engine::Bing => &[
            ("id=\"b_captcha", "Bing asked for a CAPTCHA"),
            ("/turing/captcha", "Bing asked for a CAPTCHA"),
        ],
        Engine::Brave => &[("/search/captcha", "Brave asked for a CAPTCHA")],
        Engine::DuckDuckGo => &[
            ("anomaly-modal", "DuckDuckGo suspects automated traffic"),
            ("challenge-form", "DuckDuckGo asked for a challenge"),
        ],
        Engine::Marginalia => &[(
            "barraged by queries from bots",
            "Marginalia is rate-limiting unrecognised clients",
        )],
    };
    markers
        .iter()
        .find(|(marker, _)| body.contains(marker))
        .map(|(_, reason)| (*reason).to_owned())
}

fn page_title(doc: &Document) -> String {
    Selector::parse("title")
        .ok()
        .and_then(|title| doc.select_first(Document::ROOT, &title))
        .map(|id| collapse_whitespace(&doc.text(id)))
        .unwrap_or_default()
}

enum Query {
    Text(&'static str),
    Href(&'static str),
    Manual(fn(&Document, NodeId) -> Option<String>),
}

impl Query {
    fn read(&self, doc: &Document, result: NodeId) -> Result<String, EngineError> {
        Ok(match self {
            Self::Text(css) => doc
                .select_first(result, &selector(css)?)
                .map(|id| collapse_whitespace(&doc.text(id)))
                .unwrap_or_default(),
            Self::Href(css) => doc
                .select_first(result, &selector(css)?)
                .map(|id| {
                    doc.attr(id, "href")
                        .map_or_else(|| collapse_whitespace(&doc.text(id)), str::to_owned)
                })
                .unwrap_or_default(),
            Self::Manual(read) => read(doc, result).unwrap_or_default(),
        })
    }
}

/// Selectors are constants; one that does not parse is a bug the parser
/// tests catch, reported as a layout error rather than a panic.
fn selector(css: &str) -> Result<Selector, EngineError> {
    Selector::parse(css).map_err(|e| EngineError::Layout(e.to_string()))
}

/// A result-list page described by selectors: metasearch2's `ParseOpts`.
struct Layout {
    /// The element that must exist for the page to be a results page.
    container: &'static str,
    result: &'static str,
    title: Query,
    href: Query,
    snippet: Query,
    clean_href: fn(&str) -> Option<String>,
}

impl Layout {
    fn parse(&self, doc: &Document) -> Result<Vec<EngineResult>, EngineError> {
        if doc
            .select_first(Document::ROOT, &selector(self.container)?)
            .is_none()
        {
            return Err(EngineError::Layout(format!(
                "no `{}` on the page",
                self.container
            )));
        }
        let mut results = Vec::new();
        for result in doc.select(Document::ROOT, &selector(self.result)?) {
            let title = self.title.read(doc, result)?;
            let snippet = shorten(&self.snippet.read(doc, result)?, MAX_SNIPPET_CHARS);
            // As in metasearch2: an entry with no snippet is a featured box,
            // a related-searches strip or an ad shell, not an organic result.
            if title.is_empty() || snippet.is_empty() {
                continue;
            }
            let href = self.href.read(doc, result)?;
            let Some(url) = (self.clean_href)(&href) else {
                continue;
            };
            results.push(EngineResult {
                url,
                title,
                snippet,
            });
        }
        Ok(results)
    }
}

const MAX_SNIPPET_CHARS: usize = 200;

fn shorten(text: &str, max: usize) -> String {
    let Some((cut, _)) = text.char_indices().nth(max) else {
        return text.to_owned();
    };
    let head = text.get(..cut).unwrap_or(text);
    let head = head
        .rfind(' ')
        .and_then(|space| head.get(..space))
        .unwrap_or(head);
    format!("{}…", head.trim_end())
}

fn plain_href(href: &str) -> Option<String> {
    crate::urls::clean(href)
}

fn query_param(url: &Url, key: &str) -> Option<String> {
    url.query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_snippets_are_cut_at_a_word() {
        assert_eq!(shorten("short", 10), "short");
        assert_eq!(shorten("one two three four", 10), "one two…");
        assert_eq!(shorten("ééééééééééé", 4), "éééé…");
    }

    #[test]
    fn every_engine_builds_a_request_to_its_own_host() {
        for engine in Engine::ALL {
            let request = engine.request("rust", SafeMode::Off).unwrap();
            assert_eq!(request.url.scheme(), "https", "{engine}");
            let in_form = request.form.iter().flatten().any(|(_, v)| v == "rust");
            assert!(
                in_form || request.url.query_pairs().any(|(_, v)| v == "rust"),
                "{engine}: {}",
                request.url
            );
            assert_eq!(Engine::from_id(engine.id()), Some(engine));
        }
        assert_eq!(Engine::from_id("altavista"), None);
        assert_eq!(SafeMode::from_id("strict"), Some(SafeMode::Strict));
        assert_eq!(SafeMode::from_id("moderate"), None);
    }

    #[test]
    fn an_engine_that_can_filter_is_asked_to() {
        // Google, Bing, Brave and DuckDuckGo each carry their own filter
        // parameter; Marginalia has none and says so rather than being asked
        // unfiltered.
        for engine in [
            Engine::Google,
            Engine::Bing,
            Engine::Brave,
            Engine::DuckDuckGo,
        ] {
            assert!(engine.filters(), "{engine}");
            let request = engine.request("rust", SafeMode::Strict).unwrap();
            let asked: Vec<(String, String)> = match request.form.as_ref() {
                Some(fields) => fields
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), v.clone()))
                    .collect(),
                None => request
                    .url
                    .query_pairs()
                    .map(|(k, v)| (k.into_owned(), v.into_owned()))
                    .collect(),
            };
            let filter = asked
                .iter()
                .find(|(key, _)| matches!(key.as_str(), "safe" | "adlt" | "safesearch" | "kp"));
            let (_, value) = filter.unwrap_or_else(|| panic!("{engine} carries no filter"));
            assert!(
                matches!(value.as_str(), "active" | "strict" | "1"),
                "{engine}: {value}"
            );
        }
        assert!(!Engine::Marginalia.filters());
    }

    #[test]
    fn a_challenge_page_is_blocked_not_empty() {
        let captcha = "<html><head><title>Captcha</title></head><body>prove it</body></html>";
        for engine in Engine::ALL {
            assert!(
                matches!(
                    engine.parse(captcha),
                    Err(EngineError::Blocked(_) | EngineError::RateLimited(_))
                ),
                "{engine}"
            );
        }
    }

    #[test]
    fn a_page_without_a_result_list_is_a_layout_error() {
        for engine in Engine::ALL {
            assert!(
                matches!(
                    engine.parse("<html><body><p>Hello</p></body></html>"),
                    Err(EngineError::Layout(_))
                ),
                "{engine}"
            );
        }
    }
}
