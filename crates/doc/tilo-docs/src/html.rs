#[must_use]
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn markup_characters_are_escaped() {
        assert_eq!(
            super::escape(r#"<a href="x">Tom's & co</a>"#),
            "&lt;a href=&quot;x&quot;&gt;Tom&#39;s &amp; co&lt;/a&gt;"
        );
    }
}
