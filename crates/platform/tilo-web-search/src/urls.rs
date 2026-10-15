//! Result addresses: what is returned, and what two engines' results are
//! compared by.
//!
//! Ported from metasearch2's `urls.rs` (CC0-1.0, see `PROVENANCE.md`) with
//! one change: metasearch2 rewrites every address to its normal form, which
//! upgrades `http` to `https` and can break a link to a site that serves only
//! one. Here the normal form is only the comparison key ([`dedup_key`]); the
//! returned address keeps its scheme and path and loses only its fragment and
//! known tracking parameters ([`clean`]).

use url::Url;

/// Query parameters that only identify the click, never the page.
const TRACKING_PARAMS: &[&str] = &[
    "ref_src",
    "_sm_au_",
    "utm_source",
    "utm_medium",
    "utm_campaign",
    "utm_term",
    "utm_content",
    "fbclid",
    "gclid",
    "msclkid",
    // Bing appends it to some result links.
    "msockid",
];

/// `url` as a web address worth returning: absolute `http`/`https`, without
/// its fragment or tracking parameters. `None` for anything else (relative
/// links, `javascript:`, an engine's own internal links).
pub fn clean(url: &str) -> Option<String> {
    let mut url = Url::parse(url.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return None;
    }
    url.set_fragment(None);
    strip_tracking(&mut url);
    Some(url.into())
}

fn strip_tracking(url: &mut Url) {
    if url.query().is_none() {
        return;
    }
    let kept: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| !TRACKING_PARAMS.contains(&key.as_ref()))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    if kept.len() == url.query_pairs().count() {
        // Nothing to strip: leave the query exactly as the page wrote it.
        return;
    }
    if kept.is_empty() {
        url.set_query(None);
    } else {
        url.query_pairs_mut().clear().extend_pairs(kept);
    }
}

/// The key two results are the same page by: scheme-less, `www.`-less,
/// without a trailing slash, fragment or tracking parameters, path
/// percent-decoded.
pub fn dedup_key(url: &str) -> String {
    let Ok(mut parsed) = Url::parse(url.trim()) else {
        return url.trim().to_owned();
    };
    parsed.set_fragment(None);
    strip_tracking(&mut parsed);
    let host = parsed
        .host_str()
        .unwrap_or_default()
        .trim_start_matches("www.")
        .to_ascii_lowercase();
    let path = percent_decode(parsed.path());
    let path = path.trim_end_matches('/');
    let mut key = host;
    if let Some(port) = parsed.port() {
        key.push(':');
        key.push_str(&port.to_string());
    }
    key.push_str(path);
    if let Some(query) = parsed.query() {
        key.push('?');
        key.push_str(query);
    }
    key
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut iter = bytes.iter().copied();
    while let Some(b) = iter.next() {
        if b == b'%' {
            let mut ahead = iter.clone();
            if let (Some(h), Some(l)) = (ahead.next(), ahead.next())
                && let (Some(h), Some(l)) = (hex_value(h), hex_value(l))
            {
                out.push(h.wrapping_shl(4) | l);
                iter = ahead;
                continue;
            }
        }
        out.push(b);
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(b: u8) -> Option<u8> {
    char::from(b)
        .to_digit(16)
        .and_then(|d| u8::try_from(d).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_keeps_the_scheme_and_drops_fragments_and_tracking() {
        assert_eq!(
            clean("http://example.com/a/?utm_source=x&id=3#top").as_deref(),
            Some("http://example.com/a/?id=3")
        );
        assert_eq!(
            clean("https://example.com/?fbclid=1").as_deref(),
            Some("https://example.com/")
        );
        assert_eq!(
            clean("https://www.whitepages.com/?msockid=0aca8d35").as_deref(),
            Some("https://www.whitepages.com/")
        );
        for refused in ["/relative", "javascript:alert(1)", "mailto:a@b.c", ""] {
            assert_eq!(clean(refused), None, "{refused}");
        }
    }

    #[test]
    fn the_same_page_has_one_key_however_an_engine_spells_it() {
        let a = dedup_key("https://www.rust-lang.org/");
        assert_eq!(a, dedup_key("http://rust-lang.org"));
        assert_eq!(a, dedup_key("https://rust-lang.org/#learn"));
        assert_eq!(
            dedup_key("https://en.wikipedia.org/wiki/Rust_%28programming_language%29"),
            dedup_key("https://en.wikipedia.org/wiki/Rust_(programming_language)")
        );
        assert_ne!(a, dedup_key("https://rust-lang.org/learn"));
    }
}
