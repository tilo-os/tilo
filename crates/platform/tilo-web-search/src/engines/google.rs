//! Google, through the search element that sites embed (Programmable Search).
//!
//! Google's own result page has needed JavaScript since 2025. The no-script
//! layouts scrapers used to read (`gbv=1`, text browsers, the WML page for old
//! phones) now get a "browser not supported" page or a 403, so asking
//! `www.google.com/search` gets no results. The embeddable search element still
//! answers a plain HTTP client. Its loader script, `cse.js`, hands out a
//! short-lived token, and `cse/element/v1` answers a query with Google's
//! ranking as JSON. The technique, endpoints and engine id follow `SearXNG`'s
//! `google_cse` engine (AGPL-3.0-or-later, see `PROVENANCE.md`).
//!
//! Google ends whole-web search for such engines on 2027-01-01. After that
//! this engine fails or answers with nothing, and the other engines carry the
//! search, as they do whenever one engine fails.

use serde::Deserialize;
use url::Url;

use super::{EngineError, EngineRequest, EngineResult, MAX_SNIPPET_CHARS, shorten};
use crate::html::collapse_whitespace;

/// The engine id asked: blackle.com's, which `SearXNG` uses. The id is public,
/// since every site that embeds the element publishes its own. It has to be
/// one set to search the whole web, and engines created today can no longer
/// be, so an older one is used.
const ENGINE_ID: &str = "partner-pub-8993703457585266:4862972284";

/// The element serves pages that embed it; it expects their referrer.
const REFERER: &str = "https://cse.google.com/";

/// Results asked for; the element answers at most 20.
const RESULTS: &str = "10";

/// What `cse.js` hands out, needed by every query until it expires.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    value: String,
    library_version: String,
    experiments: String,
}

pub(crate) fn token_request() -> Option<EngineRequest> {
    let mut request =
        EngineRequest::get("https://cse.google.com/cse/cse.js", &[("cx", ENGINE_ID)])?;
    request.headers.push(("referer", REFERER.to_owned()));
    Some(request)
}

/// The token in `cse.js`, which ends by passing its options to a loader:
/// `…({"cse_token": …, "cselibVersion": …, "exp": […], …});`.
pub(crate) fn parse_token(script: &str) -> Option<Token> {
    #[derive(Deserialize)]
    struct Options {
        cse_token: Option<String>,
        #[serde(rename = "cselibVersion", default)]
        library_version: String,
        #[serde(default)]
        exp: Vec<String>,
    }
    let start = script.rfind("({")?.checked_add(1)?;
    let end = script.rfind("});")?.checked_add(1)?;
    let options: Options = serde_json::from_str(script.get(start..end)?).ok()?;
    let token = options.cse_token.filter(|token| !token.is_empty())?;
    Some(Token {
        value: token,
        library_version: options.library_version,
        experiments: options.exp.join(","),
    })
}

pub(super) fn request(query: &str) -> Option<EngineRequest> {
    let mut request = EngineRequest::get(
        "https://cse.google.com/cse/element/v1",
        &[
            ("rsz", "filtered_cse"),
            ("num", RESULTS),
            ("cx", ENGINE_ID),
            ("q", query),
            ("safe", "off"),
            // The element answers only as JSONP; without a callback it
            // serves an HTML page.
            ("callback", "_"),
            ("rurl", ""),
            ("searchtype", ""),
        ],
    )?;
    request.headers.push(("referer", REFERER.to_owned()));
    Some(request)
}

/// Add `token` to a query `url` from [`request`].
pub(crate) fn authorize(url: &mut Url, token: &Token) {
    let mut pairs = url.query_pairs_mut();
    pairs
        .append_pair("cse_tok", &token.value)
        .append_pair("cselibv", &token.library_version);
    if !token.experiments.is_empty() {
        pairs.append_pair("exp", &token.experiments);
    }
}

#[derive(Deserialize)]
struct Answer {
    error: Option<Refusal>,
    cursor: Option<serde_json::Value>,
    results: Option<Vec<Item>>,
}

#[derive(Deserialize)]
struct Refusal {
    code: Option<u16>,
    #[serde(default)]
    message: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Item {
    unescaped_url: Option<String>,
    #[serde(default)]
    title_no_formatting: String,
    #[serde(default)]
    content_no_formatting: String,
}

/// Read the element's JSONP answer, `/*O_o*/ _({…});`.
pub(super) fn parse(body: &str) -> Result<Vec<EngineResult>, EngineError> {
    let json = body
        .find('{')
        .zip(body.rfind('}'))
        .and_then(|(start, end)| body.get(start..=end));
    let Some(answer) = json.and_then(|json| serde_json::from_str::<Answer>(json).ok()) else {
        // A challenge comes as a page, not as JSON. Results can mention a
        // CAPTCHA, so the page is checked for one only when it is not JSON.
        let lower = body.to_ascii_lowercase();
        if lower.contains("/sorry/") || lower.contains("captcha") {
            return Err(EngineError::RateLimited(
                "Google reports unusual traffic".into(),
            ));
        }
        return Err(EngineError::Layout("Google's answer is not JSON".into()));
    };
    if let Some(refusal) = answer.error {
        let message = collapse_whitespace(&refusal.message);
        return Err(if refusal.code == Some(429) {
            EngineError::RateLimited(format!("Google: {message}"))
        } else {
            EngineError::Refused(format!(
                "Google refused the search ({}): {message}",
                refusal.code.unwrap_or_default()
            ))
        });
    }
    if answer.cursor.is_none() && answer.results.is_none() {
        return Err(EngineError::Layout(
            "Google's answer has neither results nor a cursor".into(),
        ));
    }
    Ok(answer
        .results
        .unwrap_or_default()
        .into_iter()
        .filter_map(|item| {
            let url = crate::urls::clean(item.unescaped_url.as_deref()?)?;
            let title = collapse_whitespace(&item.title_no_formatting);
            (!title.is_empty()).then(|| EngineResult {
                url,
                title,
                snippet: shorten(
                    &collapse_whitespace(&item.content_no_formatting),
                    MAX_SNIPPET_CHARS,
                ),
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_token_is_read_from_the_loader_options() {
        let script = r#"(function(){ var x = {"a": 1}; })();
            window.__gcse.loader({"cx":"id","cse_token":"AHb:tok","cselibVersion":"3735a6ee","exp":["cc","sps"]});"#;
        assert_eq!(
            parse_token(script),
            Some(Token {
                value: "AHb:tok".into(),
                library_version: "3735a6ee".into(),
                experiments: "cc,sps".into(),
            })
        );
        assert_eq!(parse_token(r#"loader({"cselibVersion":"1"});"#), None);
        assert_eq!(parse_token("<html>moved</html>"), None);
    }

    #[test]
    fn a_query_carries_the_token() {
        let mut url = request("rust").unwrap().url;
        authorize(
            &mut url,
            &Token {
                value: "t".into(),
                library_version: "v".into(),
                experiments: String::new(),
            },
        );
        let pairs: Vec<_> = url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert!(pairs.contains(&("cse_tok".into(), "t".into())));
        assert!(pairs.contains(&("q".into(), "rust".into())));
        assert!(!pairs.iter().any(|(k, _)| k == "exp"));
    }

    #[test]
    fn refusals_say_whether_to_back_off() {
        let limited = r#"/*O_o*/ _({"error": {"code": 429, "message": "Rate  limit"}});"#;
        assert_eq!(
            parse(limited),
            Err(EngineError::RateLimited("Google: Rate limit".into()))
        );
        let forbidden = r#"_({"error": {"code": 403, "message": "Unauthorized access"}});"#;
        assert!(matches!(parse(forbidden), Err(EngineError::Refused(_))));
        let sorry =
            r#"<html><a href="https://www.google.com/sorry/index?continue=x">here</a></html>"#;
        assert!(matches!(parse(sorry), Err(EngineError::RateLimited(_))));
    }

    #[test]
    fn results_that_mention_a_captcha_are_still_results() {
        let body = r#"_({"cursor": {}, "results": [{"unescapedUrl": "https://example.org/captcha",
            "titleNoFormatting": "CAPTCHA solving", "contentNoFormatting": "About captchas"}]});"#;
        assert_eq!(parse(body).unwrap().len(), 1);
        assert_eq!(parse(r#"_({"cursor": {}});"#), Ok(Vec::new()));
    }
}
