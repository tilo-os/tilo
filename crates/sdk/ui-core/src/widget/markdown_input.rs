//! Markdown typed as formatting.

use std::ops::Range;

/// The formatting a completed span asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineKind {
    Bold,
    Italic,
    Code,
    Strikethrough,
    Link,
}

/// A span completed by the last character of the text before the caret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineMatch {
    pub kind: InlineKind,
    pub open: Range<usize>,
    pub content: Range<usize>,
    pub close: Range<usize>,
    pub target: Option<String>,
}

fn linkable(target: &str) -> bool {
    let rest = ["https://", "http://", "mailto:", "tilo://open#"]
        .iter()
        .find_map(|scheme| target.strip_prefix(scheme));
    rest.is_some_and(|rest| !rest.is_empty())
        && !target.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn detect_link(before_caret: &str) -> Option<InlineMatch> {
    let caret = before_caret.len();
    let head = before_caret.strip_suffix(')')?;
    let target_start = head.rfind("](")?.checked_add(2)?;
    let target = head.get(target_start..)?;
    if !linkable(target) {
        return None;
    }
    let text_end = target_start.checked_sub(2)?;
    let open = before_caret.get(..text_end)?.rfind('[')?;
    let text = before_caret.get(open.checked_add(1)?..text_end)?;
    if text.is_empty() || text.contains(['[', ']']) || text.trim() != text {
        return None;
    }
    Some(InlineMatch {
        kind: InlineKind::Link,
        open: open..open.saturating_add(1),
        content: open.saturating_add(1)..text_end,
        close: text_end..caret,
        target: Some(target.to_owned()),
    })
}

fn detect_autolink(before_caret: &str) -> Option<InlineMatch> {
    let caret = before_caret.len();
    let head = before_caret.strip_suffix(' ')?;
    let start = head
        .rfind(char::is_whitespace)
        .map_or(0, |at| at.saturating_add(1));
    let word = head.get(start..)?;
    let address = word.trim_end_matches(['.', ',', ';', ':', '!', '?', ')']);
    if !(address.starts_with("https://") || address.starts_with("http://")) || !linkable(address) {
        return None;
    }
    let end = start.saturating_add(address.len());
    Some(InlineMatch {
        kind: InlineKind::Link,
        open: start..start,
        content: start..end,
        close: caret..caret,
        target: Some(address.to_owned()),
    })
}

/// The span the last character of `before_caret` completes, if any.
///
/// `before_caret` is the block's text from its start to the caret,
/// including the closing delimiter just typed.
///
/// The rules keep a stray delimiter from ever eating text:
/// - The closing delimiter is the whole run of the typed character ending at
///   the caret, exactly as wide as its kind: one `` ` ``, two `~`, one `*`
///   or `_` for italic, two for bold. `***` or ```` ``` ```` is left alone.
/// - The opening delimiter is the nearest run of the same character to the
///   left, exactly as wide as the closing one.
/// - The content is not empty, holds no delimiter character of its kind, and
///   neither starts nor ends with whitespace, so `2 * 3 * 4` stays as typed.
/// - An `_` opener never follows a letter or digit, so `snake_case_name`
///   stays as typed.
#[must_use]
pub fn detect_inline(before_caret: &str) -> Option<InlineMatch> {
    match before_caret.as_bytes().last()? {
        b')' => return detect_link(before_caret),
        b' ' => return detect_autolink(before_caret),
        _ => {}
    }
    let bytes = before_caret.as_bytes();
    let caret = bytes.len();
    let &delimiter = bytes.last()?;
    let run_ending_at = |end: usize| {
        bytes.get(..end).map_or(0, |head| {
            head.iter().rev().take_while(|&&b| b == delimiter).count()
        })
    };
    let width = run_ending_at(caret);
    let kind = match (delimiter, width) {
        (b'`', 1) => InlineKind::Code,
        (b'~', 2) => InlineKind::Strikethrough,
        (b'*' | b'_', 1) => InlineKind::Italic,
        (b'*' | b'_', 2) => InlineKind::Bold,
        _ => return None,
    };
    let content_end = caret.checked_sub(width)?;
    let open_end = bytes
        .get(..content_end)?
        .iter()
        .rposition(|&b| b == delimiter)?
        .checked_add(1)?;
    if run_ending_at(open_end) != width || open_end >= content_end {
        return None;
    }
    let open_start = open_end.checked_sub(width)?;
    let first = *bytes.get(open_end)?;
    let last = *bytes.get(content_end.checked_sub(1)?)?;
    if first.is_ascii_whitespace() || last.is_ascii_whitespace() {
        return None;
    }
    if delimiter == b'_'
        && open_start
            .checked_sub(1)
            .and_then(|before| bytes.get(before))
            .is_some_and(u8::is_ascii_alphanumeric)
    {
        return None;
    }
    Some(InlineMatch {
        kind,
        open: open_start..open_end,
        content: open_end..content_end,
        close: content_end..caret,
        target: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn applied(text: &str) -> Option<(InlineKind, String, String)> {
        let found = detect_inline(text)?;
        let stripped = format!(
            "{}{}{}{}",
            &text[..found.open.start],
            &text[found.content.clone()],
            &text[found.content.end..found.close.start],
            &text[found.close.end..]
        );
        Some((found.kind, text[found.content].to_string(), stripped))
    }

    #[test]
    fn completed_spans_are_recognised() {
        for (typed, kind, content, stripped) in [
            ("**bold**", InlineKind::Bold, "bold", "bold"),
            ("__bold__", InlineKind::Bold, "bold", "bold"),
            ("*i*", InlineKind::Italic, "i", "i"),
            ("_i_", InlineKind::Italic, "i", "i"),
            (" _x_", InlineKind::Italic, "x", " x"),
            ("`code`", InlineKind::Code, "code", "code"),
            ("`a*b*`", InlineKind::Code, "a*b*", "a*b*"),
            ("~~gone~~", InlineKind::Strikethrough, "gone", "gone"),
            ("a *b*", InlineKind::Italic, "b", "a b"),
            ("**two words**", InlineKind::Bold, "two words", "two words"),
            ("*café*", InlineKind::Italic, "café", "café"),
        ] {
            assert_eq!(
                applied(typed),
                Some((kind, content.to_string(), stripped.to_string())),
                "{typed:?}"
            );
        }
    }

    #[test]
    fn links_and_typed_addresses_become_links() {
        let link = detect_inline("see [the plan](https://tilo.so/plan)").expect("a link");
        assert_eq!(link.kind, InlineKind::Link);
        assert_eq!(link.target.as_deref(), Some("https://tilo.so/plan"));
        assert_eq!(
            applied("see [the plan](https://tilo.so/plan)")
                .map(|(_, content, text)| (content, text)),
            Some(("the plan".to_owned(), "see the plan".to_owned()))
        );
        let resource = detect_inline("[Standup](tilo://open#/me/so.tilo.calendar/-/e:1)");
        assert_eq!(
            resource.and_then(|found| found.target),
            Some("tilo://open#/me/so.tilo.calendar/-/e:1".to_owned())
        );
        let typed = detect_inline("read https://tilo.so/docs. ").expect("an autolink");
        assert_eq!(typed.target.as_deref(), Some("https://tilo.so/docs"));
        assert_eq!(
            applied("read https://tilo.so/docs. ").map(|(_, content, text)| (content, text)),
            Some((
                "https://tilo.so/docs".to_owned(),
                "read https://tilo.so/docs. ".to_owned()
            ))
        );
        for typed in [
            "[x](not a url)",
            "[](https://tilo.so)",
            "[x](javascript:alert(1))",
            "(https://tilo.so)",
            "https:// ",
            "ftp://host ",
            "word ",
            " ",
        ] {
            assert_eq!(detect_inline(typed), None, "{typed:?}");
        }
    }

    #[test]
    fn incomplete_or_ambiguous_input_is_left_alone() {
        for typed in [
            "**bold*",
            "~s~",
            "```",
            "``",
            "****",
            "**",
            "*",
            "* not bold *",
            "*trailing *",
            "* leading*",
            "2 * 3 *",
            "foo_bar_",
            "snake_case_",
            "***x***",
            "plain text",
            "# heading",
            "",
        ] {
            assert_eq!(detect_inline(typed), None, "{typed:?}");
        }
    }
}
