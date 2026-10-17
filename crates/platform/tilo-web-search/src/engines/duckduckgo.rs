//! `DuckDuckGo`, through its no-JavaScript HTML endpoint.

use url::Url;

use super::{EngineError, EngineRequest, EngineResult, Layout, Query, query_param};
use crate::html::Document;

/// The form its search box submits. Asked by link instead, the endpoint
/// answers clients without a browser history with a challenge page.
pub(super) fn request(query: &str) -> Option<EngineRequest> {
    EngineRequest::post(
        "https://html.duckduckgo.com/html/",
        &[("q", query), ("b", "")],
    )
}

pub(super) fn parse(doc: &Document) -> Result<Vec<EngineResult>, EngineError> {
    Layout {
        container: "#links, .results",
        result: "div.result:not(.result--ad)",
        title: Query::Text("a.result__a"),
        href: Query::Href("a.result__a"),
        snippet: Query::Text(".result__snippet"),
        clean_href,
    }
    .parse(doc)
}

/// `DuckDuckGo` links through `//duckduckgo.com/l/?uddg=<address>`.
fn clean_href(href: &str) -> Option<String> {
    let absolute = if href.starts_with("//") {
        format!("https:{href}")
    } else {
        href.to_owned()
    };
    let parsed = Url::parse(&absolute).ok()?;
    if parsed.host_str() == Some("duckduckgo.com") && parsed.path() == "/l/" {
        return crate::urls::clean(&query_param(&parsed, "uddg")?);
    }
    crate::urls::clean(&absolute)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_links_resolve_to_the_page() {
        assert_eq!(
            clean_href("//duckduckgo.com/l/?uddg=https%3A%2F%2Frust%2Dlang.org%2F&rut=e2")
                .as_deref(),
            Some("https://rust-lang.org/")
        );
    }
}
