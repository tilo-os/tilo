//! Percent-decoding for URIs, with the `+` question answered once.
//!
//! Both answers are right — for different inputs, which is exactly why a single
//! blanket rule would have been wrong:
//!
//! * In `application/x-www-form-urlencoded` data — HTML form submissions, and
//!   the query strings browsers and OAuth providers build from them — `+`
//!   **is** an encoded space. That encoding is defined by the URL living
//!   standard, not by RFC 3986. Use [`parse_form_query`].
//!
//! * Everywhere else in a URI — path segments, and query strings that are not
//!   form data — RFC 3986 gives `+` no special meaning: it is a literal plus.
//!   Decoding it as a space corrupts `Google+`, `C++`, and a leading `+` on a
//!   phone number. Use [`decode`].
//!
//! Pick per call site by asking what produced the string, not by what it looks
//! like.
//!
//! Encoding has one answer: [`encode_component`] escapes everything but the
//! RFC 3986 unreserved set, so its output is a valid path segment, query key
//! or query value, and a space always becomes `%20` (never `+`), which both
//! decoders read back as a space.

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC};

/// Everything except the RFC 3986 §2.3 unreserved characters
/// (`ALPHA / DIGIT / "-" / "." / "_" / "~"`).
const COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// Percent-encode a URI component: every byte outside the RFC 3986
/// unreserved set becomes an upper-case `%XX` escape of its UTF-8 encoding.
///
/// Safe for a query key or value (in a plain or form-encoded query), and for
/// a single path segment.
pub fn encode_component(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, COMPONENT).to_string()
}

/// Percent-decode a URI component per **RFC 3986**: `%XX` escapes are decoded
/// and `+` is left as a literal plus.
///
/// This is the right rule for path segments and for query strings that are not
/// form data (an `otpauth://` key URI, a `file://` path, a media type).
///
/// Invalid UTF-8 in the decoded bytes is replaced rather than erroring, and a
/// malformed escape (`%2`, `%zz`) is passed through verbatim — robustness over
/// strictness, since these strings come from other people's encoders.
pub fn decode(s: &str) -> String {
    percent_encoding::percent_decode_str(s)
        .decode_utf8_lossy()
        .into_owned()
}

/// Parse an `application/x-www-form-urlencoded` query string into its
/// key/value pairs, decoding `%XX` escapes **and** `+` as a space.
///
/// This is the right rule for anything a browser or an HTML form produced —
/// including an OAuth provider's redirect back to a loopback listener.
pub fn parse_form_query(query: &str) -> Vec<(String, String)> {
    form_urlencoded::parse(query.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_component_keeps_only_the_unreserved_set() {
        assert_eq!(
            encode_component("AZaz09-._~"),
            "AZaz09-._~",
            "unreserved characters pass through"
        );
        assert_eq!(
            encode_component("a b+c/d?e=f&g%h:i@j#k"),
            "a%20b%2Bc%2Fd%3Fe%3Df%26g%25h%3Ai%40j%23k"
        );
        assert_eq!(encode_component("*!'(),;$"), "%2A%21%27%28%29%2C%3B%24");
        assert_eq!(encode_component("café ☕"), "caf%C3%A9%20%E2%98%95");
        assert_eq!(encode_component("\u{0}\u{7f}"), "%00%7F");
        assert_eq!(encode_component(""), "");
    }

    #[test]
    fn encoded_components_round_trip_through_both_decoders() {
        let raw = "C++ & friends: 100% café/π?";
        let encoded = encode_component(raw);
        assert_eq!(decode(&encoded), raw);
        assert_eq!(parse_form_query(&format!("k={encoded}"))[0].1, raw);
    }

    #[test]
    fn rfc3986_decode_keeps_plus_literal() {
        // A `+` in a path or a non-form query is a literal plus; treating it
        // as a space corrupts real names.
        assert_eq!(decode("my+song.mp3"), "my+song.mp3");
        assert_eq!(decode("Google+"), "Google+");
        assert_eq!(decode("C++"), "C++");
        assert_eq!(decode("%2B1%20555"), "+1 555");
    }

    #[test]
    fn rfc3986_decode_handles_escapes() {
        assert_eq!(decode("audio%2Fmp4"), "audio/mp4");
        assert_eq!(decode("Acme%20Corp"), "Acme Corp");
        assert_eq!(decode("AT%26T"), "AT&T");
        assert_eq!(decode("caf%C3%A9"), "café");
        assert_eq!(decode("plain"), "plain");
    }

    #[test]
    fn rfc3986_decode_passes_malformed_escapes_through() {
        // A truncated or non-hex escape must not eat the rest of the string.
        assert_eq!(decode("a%2"), "a%2");
        assert_eq!(decode("a%zz"), "a%zz");
        assert_eq!(decode("100%"), "100%");
    }

    #[test]
    fn form_query_decodes_plus_as_space() {
        // The other half of the answer: a browser encodes a submitted space as
        // `+`, so an OAuth `state` really does mean a space here.
        assert_eq!(
            parse_form_query("code=abc+def&state=x+y"),
            vec![
                ("code".into(), "abc def".into()),
                ("state".into(), "x y".into())
            ]
        );
    }

    #[test]
    fn form_query_decodes_escapes_and_keeps_empty_values() {
        assert_eq!(
            parse_form_query("error=access_denied&desc=User%20said%20no&empty="),
            vec![
                ("error".into(), "access_denied".into()),
                ("desc".into(), "User said no".into()),
                ("empty".into(), String::new()),
            ]
        );
    }

    #[test]
    fn form_query_handles_an_encoded_plus_distinctly_from_a_bare_one() {
        // `%2B` is a real plus even in form data; only a bare `+` is a space.
        assert_eq!(
            parse_form_query("a=1%2B1&b=1+1"),
            vec![("a".into(), "1+1".into()), ("b".into(), "1 1".into())]
        );
    }

    #[test]
    fn form_query_on_empty_input_is_empty() {
        assert!(parse_form_query("").is_empty());
    }

    #[test]
    fn the_two_rules_disagree_exactly_where_they_should() {
        // The whole point: same bytes, two answers, chosen by provenance.
        assert_eq!(decode("my+song.mp3"), "my+song.mp3");
        assert_eq!(
            parse_form_query("f=my+song.mp3"),
            vec![("f".into(), "my song.mp3".into())]
        );
        // And they agree on everything that is not a bare plus.
        assert_eq!(decode("caf%C3%A9"), "café");
        assert_eq!(parse_form_query("f=caf%C3%A9")[0].1, "café");
    }
}
