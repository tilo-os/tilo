//! Percent-decoding for URIs, with the `+` question answered once. Both answers are right
//! — for different inputs, which is exactly why a single blanket rule would have been
//! wrong.

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC};

/// Everything except the RFC 3986 §2.3 unreserved characters
/// (`ALPHA / DIGIT / "-" / "." / "_" / "~"`).
const COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// Percent-encode a URI component: every byte outside the RFC 3986 unreserved set becomes
/// an upper-case `%XX` escape of its UTF-8 encoding.
pub fn encode_component(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, COMPONENT).to_string()
}

/// Percent-decode a URI component per **RFC 3986**: `%XX` escapes are decoded and `+` is
/// left as a literal plus.
pub fn decode(s: &str) -> String {
    percent_encoding::percent_decode_str(s)
        .decode_utf8_lossy()
        .into_owned()
}

/// Parse an `application/x-www-form-urlencoded` query string into its key/value pairs,
/// decoding `%XX` escapes **and** `+` as a space.
pub fn parse_form_query(query: &str) -> Vec<(String, String)> {
    form_urlencoded::parse(query.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_component_keeps_only_the_unreserved_set_and_round_trips() {
        for (raw, want) in [
            ("AZaz09-._~", "AZaz09-._~"),
            (
                "a b+c/d?e=f&g%h:i@j#k",
                "a%20b%2Bc%2Fd%3Fe%3Df%26g%25h%3Ai%40j%23k",
            ),
            ("*!'(),;$", "%2A%21%27%28%29%2C%3B%24"),
            ("café ☕", "caf%C3%A9%20%E2%98%95"),
            ("\u{0}\u{7f}", "%00%7F"),
            ("", ""),
        ] {
            assert_eq!(encode_component(raw), want, "encode {raw:?}");
        }

        let raw = "C++ & friends: 100% café/π?";
        let encoded = encode_component(raw);
        assert_eq!(decode(&encoded), raw);
        assert_eq!(parse_form_query(&format!("k={encoded}"))[0].1, raw);
    }

    #[test]
    fn rfc3986_decode_keeps_plus_literal_and_malformed_escapes() {
        for (input, want) in [
            ("my+song.mp3", "my+song.mp3"),
            ("Google+", "Google+"),
            ("C++", "C++"),
            ("%2B1%20555", "+1 555"),
            ("audio%2Fmp4", "audio/mp4"),
            ("Acme%20Corp", "Acme Corp"),
            ("AT%26T", "AT&T"),
            ("caf%C3%A9", "café"),
            ("plain", "plain"),
            // A truncated or non-hex escape must not eat the rest of the string.
            ("a%2", "a%2"),
            ("a%zz", "a%zz"),
            ("100%", "100%"),
        ] {
            assert_eq!(decode(input), want, "decode {input:?}");
        }
    }

    #[test]
    fn form_query_decodes_plus_as_space_unlike_rfc3986() {
        let pairs = |list: &[(&str, &str)]| -> Vec<(String, String)> {
            list.iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect()
        };
        for (query, want) in [
            (
                "code=abc+def&state=x+y",
                pairs(&[("code", "abc def"), ("state", "x y")]),
            ),
            (
                "error=access_denied&desc=User%20said%20no&empty=",
                pairs(&[
                    ("error", "access_denied"),
                    ("desc", "User said no"),
                    ("empty", ""),
                ]),
            ),
            // `%2B` is a real plus even in form data; only a bare `+` is a space.
            ("a=1%2B1&b=1+1", pairs(&[("a", "1+1"), ("b", "1 1")])),
            ("", Vec::new()),
        ] {
            assert_eq!(parse_form_query(query), want, "form query {query:?}");
        }

        // The whole point: same bytes, two answers, chosen by provenance.
        assert_eq!(decode("my+song.mp3"), "my+song.mp3");
        assert_eq!(
            parse_form_query("f=my+song.mp3"),
            vec![("f".into(), "my song.mp3".into())]
        );
        assert_eq!(decode("caf%C3%A9"), "café");
        assert_eq!(parse_form_query("f=caf%C3%A9")[0].1, "café");
    }
}
