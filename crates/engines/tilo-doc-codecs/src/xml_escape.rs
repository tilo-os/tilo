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

pub fn push_text(out: &mut String, c: char) {
    match c {
        '&' => out.push_str("&amp;"),
        '<' => out.push_str("&lt;"),
        '>' => out.push_str("&gt;"),
        _ => out.push(c),
    }
}

#[must_use]
pub fn escape_text(s: &str) -> String {
    escape(s, None, None)
}

#[must_use]
pub fn escape_attr(s: &str) -> String {
    escape(s, Some("&quot;"), None)
}

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
