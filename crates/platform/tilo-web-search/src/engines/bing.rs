//! Bing. Ported from metasearch2's `src/engines/search/bing.rs` (CC0-1.0).

use std::hash::{BuildHasher, Hasher};

use base64::Engine as _;
use url::Url;

use super::{EngineError, EngineRequest, EngineResult, Layout, Query, query_param};
use crate::html::{Document, NodeId, Selector, collapse_whitespace};

pub(super) fn request(query: &str) -> Option<EngineRequest> {
    let cvid = conversation_id();
    let mut request = EngineRequest::get(
        "https://www.bing.com/search",
        &[
            ("q", query),
            ("pq", query),
            ("cvid", &cvid),
            // `rcrse:"1"` stops Bing from answering a corrected query instead.
            ("filters", "rcrse:\"1\""),
            ("FORM", "PERE"),
            ("ghc", "1"),
            ("lq", "0"),
            ("qs", "n"),
            ("sk", ""),
            ("sp", "-1"),
        ],
    )?;
    request
        .headers
        .push(("cookie", format!("SRCHHPGUSR=IG={cvid}")));
    Some(request)
}

/// The per-search id Bing's own pages send: 32 random hex digits. It names
/// one search, not the device, so the process's hash seeds are random
/// enough.
fn conversation_id() -> String {
    let half = || std::hash::RandomState::new().build_hasher().finish();
    format!("{:016X}{:016X}", half(), half())
}

pub(super) fn parse(doc: &Document) -> Result<Vec<EngineResult>, EngineError> {
    Layout {
        container: "#b_results",
        result: "#b_results > li.b_algo",
        title: Query::Text(".b_algo h2 > a"),
        href: Query::Href("a[href]"),
        snippet: Query::Manual(snippet),
        clean_href,
    }
    .parse(doc)
}

/// The caption, without the icon Bing puts inside some of them.
fn snippet(doc: &Document, result: NodeId) -> Option<String> {
    let caption = Selector::parse(".b_caption > p, p.b_algoSlug, .b_caption .ipText").ok()?;
    let icon = Selector::parse(".algoSlug_icon").ok()?;
    let caption = doc.select_first(result, &caption)?;
    let mut text = String::new();
    for &child in doc.children(caption) {
        if let Some(t) = doc.text_node(child) {
            text.push_str(t);
        } else if !icon.matches(doc, child) {
            text.push_str(&doc.text(child));
        }
    }
    Some(collapse_whitespace(&text))
}

/// Bing links through `bing.com/ck/a?…&u=a1<base64url of the address>`.
fn clean_href(href: &str) -> Option<String> {
    let parsed = Url::parse(href).ok()?;
    if parsed.host_str() == Some("www.bing.com") && parsed.path() == "/ck/a" {
        let encoded = query_param(&parsed, "u")?;
        let encoded = encoded.strip_prefix("a1").unwrap_or(&encoded);
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(encoded.trim_end_matches('='))
            .ok()?;
        return crate::urls::clean(&String::from_utf8_lossy(&decoded));
    }
    crate::urls::clean(href)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracking_links_resolve_to_the_page() {
        assert_eq!(
            clean_href(
                "https://www.bing.com/ck/a?!&&p=abc&ptn=3&u=a1aHR0cHM6Ly9ydXN0LWxhbmcub3JnLw&ntb=1"
            )
            .as_deref(),
            Some("https://rust-lang.org/")
        );
        assert_eq!(
            clean_href("https://example.org/x").as_deref(),
            Some("https://example.org/x")
        );
    }

    #[test]
    fn each_search_has_its_own_conversation_id() {
        let a = conversation_id();
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, conversation_id());
    }
}
