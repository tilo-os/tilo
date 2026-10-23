//! The search engines: how each is asked, and how its answer page is read.
//!
//! Every engine here works without an account or key. Bing, Brave,
//! `DuckDuckGo` and Marginalia are read from the same result page a browser
//! gets. The Bing, Brave and Marginalia scrapers are ported from metasearch2's
//! `src/engines/search/` and its `src/parse.rs` (CC0-1.0); `DuckDuckGo` is
//! Tilo's own. Google is asked through the search element sites embed, a
//! technique taken from `SearXNG` (AGPL-3.0-or-later). `PROVENANCE.md` records
//! both.
//!
//! An answer is never trusted to have the expected shape:
//!
//! - An engine that shows a challenge it shows every client like this one is
//!   [`EngineError::Blocked`].
//! - One that says it is getting too many requests is
//!   [`EngineError::RateLimited`], and the search leaves it alone for a while.
//! - An answer with no result list where one belongs is
//!   [`EngineError::Layout`]. That is how a change to an engine's markup
//!   surfaces, instead of as silently empty results.

mod bing;
mod brave;
mod duckduckgo;
pub(crate) mod google;
mod marginalia;

use std::fmt;

use serde::Serialize;
use url::Url;

use crate::html::{Document, NodeId, Selector, collapse_whitespace};

/// One search engine.
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
    /// Every engine, in the order results are reported.
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

    /// The engine's stable name, as callers name it.
    pub const fn id(self) -> &'static str {
        match self {
            Self::Bing => "bing",
            Self::Brave => "brave",
            Self::DuckDuckGo => "duckduckgo",
            Self::Google => "google",
            Self::Marginalia => "marginalia",
        }
    }

    /// The engine named `id`.
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

    /// The request that asks this engine `query`, or `None` when the engine
    /// does not take such a query.
    pub fn request(self, query: &str) -> Option<EngineRequest> {
        match self {
            Self::Bing => bing::request(query),
            Self::Brave => brave::request(query),
            Self::DuckDuckGo => duckduckgo::request(query),
            Self::Google => google::request(query),
            Self::Marginalia => marginalia::request(query),
        }
    }

    /// Read this engine's answer.
    ///
    /// # Errors
    ///
    /// [`EngineError::Blocked`] or [`EngineError::RateLimited`] when the
    /// answer is a challenge rather than results, [`EngineError::Refused`]
    /// when the engine refused the request, and [`EngineError::Layout`] when
    /// it has no result list.
    pub fn parse(self, body: &str) -> Result<Vec<EngineResult>, EngineError> {
        // Google answers with JSON, not a page.
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

/// One request to an engine: a `GET` of `url`, or a `POST` of `form` to it,
/// with these extra headers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineRequest {
    pub url: Url,
    pub headers: Vec<(&'static str, String)>,
    /// Form fields to `POST`, for an engine whose result page answers a
    /// submitted form rather than a link.
    pub form: Option<Vec<(&'static str, String)>>,
}

impl EngineRequest {
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
    /// The engine refused the request itself, for example an expired token.
    Refused(String),
    /// The page had no result list where one belongs.
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

/// Text a challenge page carries and a results page does not, per engine.
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

/// How one part of a result is read.
enum Query {
    /// The collapsed text of the first match of a selector.
    Text(&'static str),
    /// The `href` of the first match of a selector, or its text.
    Href(&'static str),
    /// Code of the engine's own.
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
    /// Turns the address an engine links to into the page's own.
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

/// The longest snippet kept, in characters: enough to judge a result by,
/// short enough that a page of results stays readable.
const MAX_SNIPPET_CHARS: usize = 200;

/// `text` cut to at most `max` characters, at a word boundary where there is
/// one, marked with an ellipsis when cut.
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

/// The address as linked, made absolute and cleaned.
fn plain_href(href: &str) -> Option<String> {
    crate::urls::clean(href)
}

/// The value of query parameter `key` in `url`.
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
            let request = engine.request("rust").unwrap();
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
