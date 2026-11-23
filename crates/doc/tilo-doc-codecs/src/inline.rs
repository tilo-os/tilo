//! Inline Markdown, both ways: a block's text and marks written as Markdown, and an inline
//! source read back as its text and marks.
//!
//! Every Markdown projection uses this one reading: the document codec, a body's block lines
//! (which an agent reads and edits), and the block editor's reading of an answer. `**strong**`,
//! `*em*`, `~~struck~~`, `` `code` `` and `[words](target)` therefore mean the same everywhere,
//! and text that would read as one of them is escaped instead.

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use tilo_doc::document::{MarkSpan, MarkValue};
use tilo_doc::export::InlinePlan;

/// How a line break inside one block is written.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Breaks {
    /// A plain newline.
    Raw,
    /// A backslash before the newline, which a reader of block lines takes as the break.
    Backslashed,
    /// An arrow, which no Markdown reads as a break: a body read for an agent.
    Arrow,
}

/// What a caller's Markdown carries beside the marks every caller carries.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Style {
    pub breaks: Breaks,
    /// Styling spans (`<span style>`), superscript and subscript, and footnote references.
    pub extras: bool,
    /// Emphasis opens with `_` where it can (`_em_`), as a body's lines read best, and with
    /// `*` where it cannot (`word*em*`); `false` writes `*` everywhere.
    pub underscore_em: bool,
    /// Brackets are escaped, so words that look like a link stay words. A whole document keeps
    /// them raw: a picture and a link kept as their Markdown source stay the picture and link
    /// they are.
    pub escape_brackets: bool,
}

impl Style {
    /// A whole document, as a file or a copy of it.
    pub const DOCUMENT: Self = Self {
        breaks: Breaks::Raw,
        extras: true,
        underscore_em: false,
        escape_brackets: false,
    };
    /// One line of a body's block lines.
    pub const BLOCK: Self = Self {
        breaks: Breaks::Backslashed,
        extras: false,
        underscore_em: true,
        escape_brackets: true,
    };
    /// A body read for an agent, where a break is drawn rather than written.
    pub const READ: Self = Self {
        breaks: Breaks::Arrow,
        extras: false,
        underscore_em: false,
        escape_brackets: true,
    };
}

/// The prefix that keeps a source's first character from reading as a block marker (`# `,
/// `> `, `- `, `1. `) while leaving word-initial emphasis to read as emphasis.
const GUARD: char = ',';

/// A hyperlink target a document may carry: an `https` or `mailto` address, or a heading in the
/// same document (`#install`).
#[must_use]
pub fn valid_hyperlink_target(target: &str) -> bool {
    if target.is_empty() || target.trim() != target || target.chars().count() > 2_048 {
        return false;
    }
    if let Some(fragment) = target.strip_prefix('#') {
        return !fragment.is_empty()
            && fragment.len() <= 128
            && fragment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    }
    let Ok(url) = url::Url::parse(target) else {
        return false;
    };
    match url.scheme() {
        "https" => {
            url.host_str().is_some() && url.username().is_empty() && url.password().is_none()
        }
        "mailto" => {
            let address = url.path();
            address
                .split_once('@')
                .is_some_and(|(local, domain)| !local.is_empty() && !domain.is_empty())
        }
        _ => false,
    }
}

/// The target a link mark reads back as: what Markdown cannot carry in a destination,
/// percent-encoded on the way out and taken back here.
fn decode_target(target: &str) -> String {
    target
        .replace("%20", " ")
        .replace("%28", "(")
        .replace("%29", ")")
        .replace("%0A", "\n")
}

/// A link's destination as written. A target given in angle brackets (`<Notes/Task 1.md>`) keeps
/// them, and inside them its spaces and parentheses stand as they are, as Markdown reads them,
/// so the path a person or agent reads in the file is the path; only what brackets cannot hold
/// is escaped. A bare target percent-encodes what would end it.
fn encode_into(out: &mut String, target: &str) {
    if let Some(inner) = target
        .strip_prefix('<')
        .and_then(|rest| rest.strip_suffix('>'))
    {
        out.push('<');
        for c in inner.chars() {
            match c {
                '<' => out.push_str("\\<"),
                '>' => out.push_str("\\>"),
                '\n' => out.push_str("%0A"),
                _ => out.push(c),
            }
        }
        out.push('>');
        return;
    }
    for c in target.chars() {
        match c {
            ' ' => out.push_str("%20"),
            '(' => out.push_str("%28"),
            ')' => out.push_str("%29"),
            _ => out.push(c),
        }
    }
}

fn mono(mark: &MarkSpan) -> bool {
    matches!(mark.name.as_str(), "font" | "fontFamily")
        && matches!(&mark.value, MarkValue::Str(family) if family == "mono")
}

/// The delimiter a toggle mark writes, when it is one this Markdown carries. Emphasis takes
/// `_` unless it would sit inside a word, where only `*` opens.
fn delim(
    mark: &MarkSpan,
    before: Option<char>,
    start: usize,
    style: Style,
) -> Option<&'static str> {
    if mark.value == MarkValue::Bool(false) {
        return None;
    }
    match mark.name.as_str() {
        "strong" => Some("**"),
        "em" => {
            if style.underscore_em && !(start > 0 && before.is_some_and(char::is_alphanumeric)) {
                Some("_")
            } else {
                Some("*")
            }
        }
        "strike" => Some("~~"),
        _ if mono(mark) => Some("`"),
        _ => None,
    }
}

/// The `style="…"` value a run's styling marks write, sorted so the same run writes the same
/// span every time; `None` when the run carries no styling Markdown has syntax for.
fn span_style(marks: &[&MarkSpan]) -> Option<String> {
    let mut decls: Vec<String> = marks.iter().filter_map(|mark| span_decl(mark)).collect();
    if decls.is_empty() {
        return None;
    }
    decls.sort();
    decls.dedup();
    Some(decls.join("; "))
}

fn span_decl(mark: &MarkSpan) -> Option<String> {
    match mark.name.as_str() {
        "color" => mark.value.as_str().map(|c| format!("color: {c}")),
        "highlight" => mark.value.as_str().map(|c| format!("background: {c}")),
        "fontSize" => mark.value.as_int().map(|n| format!("font-size: {n}pt")),
        "fontFamily" => match mark.value.as_str() {
            Some("serif") => Some("font-family: serif".into()),
            Some("sans") => Some("font-family: sans-serif".into()),
            _ => None,
        },
        _ => None,
    }
}

fn vert_wrap(marks: &[&MarkSpan]) -> Option<&'static str> {
    let mut sub = false;
    for mark in marks {
        match mark.name.as_str() {
            "super" => return Some("sup"),
            "sub" => sub = true,
            _ => {}
        }
    }
    sub.then_some("sub")
}

/// `text` with `marks` as inline Markdown.
#[must_use]
pub fn render(text: &str, marks: &[MarkSpan], style: Style) -> String {
    render_with(text, marks, style, |_| None)
}

/// `text` with `marks` as inline Markdown, a link to a note written as `note` says (a footnote
/// reference); a note it says nothing of is left out.
pub fn render_with(
    text: &str,
    marks: &[MarkSpan],
    style: Style,
    mut note: impl FnMut(&str) -> Option<String>,
) -> String {
    let plan = InlinePlan::new(text, marks);
    let n = text.chars().count();
    let cap = n.saturating_add(1);
    let mut open_at: Vec<Vec<&'static str>> = vec![Vec::new(); cap];
    let mut close_at: Vec<Vec<&'static str>> = vec![Vec::new(); cap];
    let chars: Vec<char> = text.chars().collect();
    for mark in marks {
        let start = mark.start.min(n);
        let end = mark.end.min(n);
        if start >= end {
            continue;
        }
        let before = start.checked_sub(1).and_then(|at| chars.get(at).copied());
        let Some(d) = delim(mark, before, start, style) else {
            continue;
        };
        if let Some(slot) = open_at.get_mut(start) {
            slot.push(d);
        }
        if let Some(slot) = close_at.get_mut(end) {
            slot.push(d);
        }
    }
    let mut out = String::new();
    let mut cur_link: Option<&str> = None;
    let mut cur_span: Option<String> = None;
    let mut cur_wrap: Option<&'static str> = None;
    for point in plan.points() {
        for d in close_at.get(point.offset).into_iter().flatten() {
            out.push_str(d);
        }
        let span = point
            .run
            .as_ref()
            .filter(|_| style.extras)
            .and_then(|run| span_style(&run.marks));
        let wrap = point
            .run
            .as_ref()
            .filter(|_| style.extras)
            .and_then(|run| vert_wrap(&run.marks));
        let link = point.run.as_ref().and_then(|run| run.link);
        let link_changing = link != cur_link;
        if (link_changing || span != cur_span || wrap != cur_wrap)
            && let Some(w) = cur_wrap.take()
        {
            out.push_str("</");
            out.push_str(w);
            out.push('>');
        }
        if (link_changing || span != cur_span) && cur_span.is_some() {
            out.push_str("</span>");
            cur_span = None;
        }
        if link_changing {
            if let Some(href) = cur_link {
                out.push_str("](");
                encode_into(&mut out, href);
                out.push(')');
            }
            cur_link = link;
            if cur_link.is_some() {
                out.push('[');
            }
        }
        if cur_span.is_none()
            && let Some(style) = &span
        {
            out.push_str("<span style=\"");
            out.push_str(style);
            out.push_str("\">");
            cur_span = Some(style.clone());
        }
        if cur_wrap.is_none()
            && let Some(w) = wrap
        {
            out.push('<');
            out.push_str(w);
            out.push('>');
            cur_wrap = Some(w);
        }
        for d in open_at.get(point.offset).into_iter().flatten() {
            out.push_str(d);
        }
        if let Some(run) = &point.run {
            let noted = run.note_reference.and_then(&mut note);
            if let Some(written) = noted {
                out.push_str(&written);
            } else if run.marks.iter().any(|mark| mono(mark)) {
                out.push_str(run.text);
            } else {
                escape_into(&mut out, run.text, style);
            }
        }
    }
    out
}

/// The characters that mean something inline, escaped; a line break as `style` writes it.
fn escape_into(out: &mut String, text: &str, style: Style) {
    for c in text.chars() {
        match (c, style.breaks) {
            ('\\' | '`' | '*' | '_' | '~', _) => {
                out.push('\\');
                out.push(c);
            }
            ('[' | ']', _) if style.escape_brackets => {
                out.push('\\');
                out.push(c);
            }
            ('\n', Breaks::Raw) => out.push('\n'),
            ('\n', Breaks::Backslashed) => out.push_str("\\\n"),
            ('\n', Breaks::Arrow) => out.push('\u{21b5}'),
            _ => out.push(c),
        }
    }
}

/// One link being read: where it opens in the text, and the target it names.
struct LinkIn {
    start: usize,
    target: String,
    accepted: bool,
}

/// The text and inline marks `source` says. A link whose target `accept` refuses stays the
/// words and address it is written as.
#[must_use]
pub fn parse(source: &str, accept: &dyn Fn(&str) -> bool) -> (String, Vec<MarkSpan>) {
    let mut wrapped = String::with_capacity(source.len().saturating_add(1));
    wrapped.push(GUARD);
    wrapped.push_str(source);
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    let events: Vec<Event<'_>> = Parser::new_ext(&wrapped, options).collect();
    let mut text = String::new();
    let mut len = 0_usize;
    let mut marks: Vec<MarkSpan> = Vec::new();
    let mut toggles: Vec<(&'static str, usize)> = Vec::new();
    let mut links: Vec<LinkIn> = Vec::new();
    let mut images: Vec<(usize, String)> = Vec::new();
    for event in events {
        match event {
            Event::Start(Tag::Strong) => toggles.push(("strong", len)),
            Event::Start(Tag::Emphasis) => toggles.push(("em", len)),
            Event::Start(Tag::Strikethrough) => toggles.push(("strike", len)),
            Event::End(TagEnd::Strong | TagEnd::Emphasis | TagEnd::Strikethrough) => {
                if let Some((name, start)) = toggles.pop() {
                    marks.push(span(name, start, len, MarkValue::Bool(true)));
                }
            }
            Event::Code(code) => {
                let start = len;
                push(&mut text, &mut len, code.as_ref());
                marks.push(span(
                    "fontFamily",
                    start,
                    len,
                    MarkValue::Str("mono".into()),
                ));
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                let target = decode_target(dest_url.as_ref());
                let accepted = accept(&target);
                if !accepted {
                    push(&mut text, &mut len, "[");
                }
                links.push(LinkIn {
                    start: len,
                    target,
                    accepted,
                });
            }
            Event::End(TagEnd::Link) => {
                if let Some(link) = links.pop() {
                    if link.accepted {
                        marks.push(span("link", link.start, len, MarkValue::Str(link.target)));
                    } else {
                        push(&mut text, &mut len, "](");
                        push(&mut text, &mut len, &link.target);
                        push(&mut text, &mut len, ")");
                    }
                }
            }
            Event::Start(Tag::Image { dest_url, .. }) => {
                push(&mut text, &mut len, "![");
                images.push((len, decode_target(dest_url.as_ref())));
            }
            Event::End(TagEnd::Image) => {
                if let Some((_, target)) = images.pop() {
                    push(&mut text, &mut len, "](");
                    push(&mut text, &mut len, &target);
                    push(&mut text, &mut len, ")");
                }
            }
            Event::Text(words) => push(&mut text, &mut len, &words),
            Event::SoftBreak | Event::HardBreak => push(&mut text, &mut len, "\n"),
            Event::InlineHtml(html) | Event::Html(html) => push(&mut text, &mut len, &html),
            _ => {}
        }
    }
    // The guard is one character of text before the source; offsets count from the source.
    if text.starts_with(GUARD) {
        text.remove(0);
        let len = text.chars().count();
        marks = marks
            .into_iter()
            .filter_map(|mut mark| {
                mark.start = mark.start.checked_sub(1)?;
                mark.end = mark.end.saturating_sub(1).min(len);
                (mark.start < mark.end).then_some(mark)
            })
            .collect();
    }
    marks.sort_by_key(|mark: &MarkSpan| (mark.start, mark.end));
    (text, marks)
}

fn push(text: &mut String, len: &mut usize, words: &str) {
    text.push_str(words);
    *len = len.saturating_add(words.chars().count());
}

fn span(name: &str, start: usize, end: usize, value: MarkValue) -> MarkSpan {
    MarkSpan {
        start,
        end,
        name: name.to_owned(),
        value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mark(start: usize, end: usize, name: &str, value: MarkValue) -> MarkSpan {
        MarkSpan {
            start,
            end,
            name: name.into(),
            value,
        }
    }

    fn everything_is_read_back() {
        let source = "Deadline: *Friday*, see [docs](https://example.test), run `cargo`, ~~old~~";
        let (text, marks) = parse(source, &valid_hyperlink_target);
        assert_eq!(
            text, "Deadline: Friday, see docs, run cargo, old",
            "the delimiters are not text"
        );
        let names: Vec<&str> = marks.iter().map(|mark| mark.name.as_str()).collect();
        assert_eq!(names, ["em", "link", "fontFamily", "strike"], "{marks:?}");
        assert_eq!(text.get(10..16), Some("Friday"));
        assert!(
            marks
                .iter()
                .any(|mark| mark.name == "link"
                    && mark.value.as_str() == Some("https://example.test"))
        );
    }

    fn what_is_read_writes_back_as_the_same_text_and_marks() {
        let spans = vec![
            mark(
                7,
                9,
                "link",
                MarkValue::Str("https://example.test/a b".into()),
            ),
            mark(14, 18, "fontFamily", MarkValue::Str("mono".into())),
        ];
        let written = render("a link to and code", &spans, Style::BLOCK);
        assert_eq!(
            written, "a link [to](https://example.test/a%20b) and `code`",
            "{written}"
        );
        let (text, back) = parse(&written, &valid_hyperlink_target);
        assert_eq!(text, "a link to and code");
        assert_eq!(back, spans);
    }

    fn a_bracketed_path_is_written_as_it_reads_and_read_back() {
        let spans = vec![mark(
            4,
            12,
            "link",
            MarkValue::Str("<Under/Task (1) a<b>.md>".into()),
        )];
        let written = render("see the task", &spans, Style::BLOCK);
        assert_eq!(
            written, "see [the task](<Under/Task (1) a\\<b\\>.md>)",
            "{written}"
        );
        let (text, back) = parse(&written, &|_| true);
        assert_eq!(text, "see the task");
        assert_eq!(
            back,
            vec![mark(
                4,
                12,
                "link",
                MarkValue::Str("Under/Task (1) a<b>.md".into())
            )]
        );
    }

    fn text_that_would_read_as_syntax_is_escaped() {
        let spans = Vec::new();
        let text = "snake_case and 2*3 and [a] and a\\b";
        let written = render(text, &spans, Style::BLOCK);
        assert_eq!(written, "snake\\_case and 2\\*3 and \\[a\\] and a\\\\b");
        let (read, marks) = parse(&written, &valid_hyperlink_target);
        assert_eq!(read, text);
        assert!(marks.is_empty());
    }

    fn a_source_that_opens_like_a_block_stays_inline() {
        for source in [
            "# not a heading",
            "> not a quote",
            "- not a list",
            "1. not numbered",
        ] {
            let (text, _) = parse(source, &valid_hyperlink_target);
            assert_eq!(text, source, "{source}");
        }
    }

    #[test]
    fn inline_markdown_reads_and_writes_the_same_text_and_marks() {
        let cases: [(&str, fn()); 5] = [
            ("everything is read back", everything_is_read_back),
            (
                "what is read writes back as the same text and marks",
                what_is_read_writes_back_as_the_same_text_and_marks,
            ),
            (
                "a bracketed path is written as it reads and read back",
                a_bracketed_path_is_written_as_it_reads_and_read_back,
            ),
            (
                "text that would read as syntax is escaped",
                text_that_would_read_as_syntax_is_escaped,
            ),
            (
                "a source that opens like a block stays inline",
                a_source_that_opens_like_a_block_stays_inline,
            ),
        ];
        for (case, run) in cases {
            eprintln!("case: {case}");
            run();
        }
    }
}
