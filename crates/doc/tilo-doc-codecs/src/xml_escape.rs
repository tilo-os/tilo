//! XML/HTML escaping shared by every exporter.
//!
//! Text content and attribute values need different sets, and the exporters
//! pin their output byte for byte, so each set has one spelling here:
//!
//! | function               | `&` `<` `>` | `"`      | `'`      |
//! |------------------------|-------------|----------|----------|
//! | [`escape_text`]        | escaped     | —        | —        |
//! | [`escape_attr`]        | escaped     | `&quot;` | —        |
//! | [`escape_attr_quotes`] | escaped     | `&quot;` | `&apos;` |
//! | [`escape_html_attr`]   | escaped     | `&quot;` | `&#39;`  |
//!
//! [`escape_attr`] is for the double-quoted attributes every exporter writes;
//! [`escape_attr_quotes`] is safe in either quoting and in text. HTML 4 has no
//! `&apos;`, hence the numeric reference in [`escape_html_attr`].

/// Appends `c` escaped for XML/HTML text content.
pub fn push_text(out: &mut String, c: char) {
    match c {
        '&' => out.push_str("&amp;"),
        '<' => out.push_str("&lt;"),
        '>' => out.push_str("&gt;"),
        _ => out.push(c),
    }
}

/// `s` escaped for XML/HTML text content (`&`, `<`, `>`).
#[must_use]
pub fn escape_text(s: &str) -> String {
    escape(s, None, None)
}

/// `s` escaped for a double-quoted attribute value (`&`, `<`, `>`, `"`).
#[must_use]
pub fn escape_attr(s: &str) -> String {
    escape(s, Some("&quot;"), None)
}

/// `s` escaped for an XML attribute in either quoting, or text content: also
/// `'` as `&apos;`.
#[must_use]
pub fn escape_attr_quotes(s: &str) -> String {
    escape(s, Some("&quot;"), Some("&apos;"))
}

/// `s` escaped for an HTML attribute in either quoting: `'` as `&#39;`, which
/// HTML 4 understands where it doesn't know `&apos;`.
#[must_use]
pub fn escape_html_attr(s: &str) -> String {
    escape(s, Some("&quot;"), Some("&#39;"))
}

fn escape(s: &str, quot: Option<&str>, apos: Option<&str>) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match (c, quot, apos) {
            ('"', Some(entity), _) | ('\'', _, Some(entity)) => out.push_str(entity),
            _ => push_text(&mut out, c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: &str = r#"a&b<c>d"e'f"#;

    #[test]
    fn xml_escapers_escape_exactly_their_contexts_specials() {
        // Phase `text_leaves_quotes_alone`.
        {
            assert_eq!(escape_text(ALL), r#"a&amp;b&lt;c&gt;d"e'f"#);
            let mut out = String::new();
            for c in ALL.chars() {
                push_text(&mut out, c);
            }
            assert_eq!(out, escape_text(ALL));
        }
        // Phase `attr_escapes_the_double_quote_only`.
        {
            assert_eq!(escape_attr(ALL), r"a&amp;b&lt;c&gt;d&quot;e'f");
        }
        // Phase `attr_quotes_escape_both_quotes_as_xml_entities`.
        {
            assert_eq!(escape_attr_quotes(ALL), "a&amp;b&lt;c&gt;d&quot;e&apos;f");
        }
        // Phase `html_attr_uses_a_numeric_apostrophe`.
        {
            assert_eq!(escape_html_attr(ALL), "a&amp;b&lt;c&gt;d&quot;e&#39;f");
        }
        // Phase `non_ascii_and_existing_entities_pass_through_escaped_once`.
        {
            assert_eq!(escape_text("é &amp; 😀"), "é &amp;amp; 😀");
            assert_eq!(escape_attr(""), "");
        }
    }
}
