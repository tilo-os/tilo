//! A page's Markdown as HTML: headings with ids and anchors, code blocks
//! coloured and grouped into one example's language tabs, GitHub alert
//! blockquotes as callouts, and wide tables that scroll on their own.
//!
//! Content carries no layout: raw HTML in a page is shown as text.

use pulldown_cmark::{
    BlockQuoteKind, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd,
};
use std::fmt::Write as _;

use crate::highlight::highlight;
use crate::html::escape;
use crate::slug::Slugger;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heading {
    pub level: u8,
    pub id: String,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct Rendered {
    /// The body without its `# title`, which the page's layout shows.
    pub html: String,
    /// Every heading below the title, in order.
    pub headings: Vec<Heading>,
    /// The prose as plain text, for search; code is left out.
    pub text: String,
    /// Every link's target, in order, to check them against the site.
    pub links: Vec<String>,
}

/// One fenced block's info string: `python example=create-workspace`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Info {
    pub language: String,
    pub example: Option<String>,
}

impl Info {
    #[must_use]
    pub fn parse(info: &str) -> Self {
        let mut words = info.split_whitespace();
        let language = words.next().unwrap_or_default().to_ascii_lowercase();
        let example = words
            .find_map(|word| word.strip_prefix("example="))
            .map(|id| id.trim_matches('"').to_string());
        Self { language, example }
    }
}

/// The tab or bar label for a block's language; empty for plain output.
#[must_use]
pub fn language_label(language: &str, in_example: bool) -> &'static str {
    match language {
        "python" | "py" => "Python",
        "typescript" | "ts" => "TypeScript",
        "javascript" | "js" => "JavaScript",
        "curl" => "curl",
        "sh" | "bash" | "shell" | "console" if in_example => "CLI",
        "sh" | "bash" | "shell" | "console" => "Shell",
        "json" => "JSON",
        "toml" => "TOML",
        "http" => "HTTP",
        _ => "",
    }
}

/// The order an example's tabs are shown in, whatever order the page has them.
fn tab_rank(language: &str) -> usize {
    ["curl", "python", "typescript", "ts", "sh", "bash", "shell"]
        .iter()
        .position(|candidate| *candidate == language)
        .unwrap_or(usize::MAX)
}

struct Block {
    info: Info,
    code: String,
}

#[must_use]
pub fn render(markdown: &str) -> Rendered {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_GFM
        | Options::ENABLE_TASKLISTS;
    let events: Vec<Event<'_>> = Parser::new_ext(markdown, options).collect();
    let mut out: Vec<Event<'_>> = Vec::with_capacity(events.len());
    let mut headings = Vec::new();
    let mut slugger = Slugger::default();
    let mut links = Vec::new();
    let mut rest = events.as_slice();
    while let Some((event, tail)) = rest.split_first() {
        rest = tail;
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                let (inner, after) = split_before(rest, |event| {
                    matches!(event, Event::End(TagEnd::Heading(_)))
                });
                rest = after;
                let title = plain_text(inner);
                let id = slugger.id(&title);
                if *level == HeadingLevel::H1 {
                    continue;
                }
                let number = heading_number(*level);
                headings.push(Heading {
                    level: number,
                    id: id.clone(),
                    text: title,
                });
                out.push(Event::Html(
                    format!("<h{number} id=\"{}\">", escape(&id)).into(),
                ));
                out.extend(inner.iter().map(|event| inert(event.clone())));
                out.push(Event::Html(
                    format!("<a class=\"anchor\" href=\"#{}\" aria-label=\"Link to this section\"></a></h{number}>\n", escape(&id))
                        .into(),
                ));
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                let (first, after) = read_block(kind, rest);
                rest = after;
                let mut group = vec![first];
                // The blocks right after it with the same example id are its tabs.
                while let Some((Event::Start(Tag::CodeBlock(kind)), tail)) = rest.split_first() {
                    let (block, after) = read_block(kind, tail);
                    if block.info.example.is_none()
                        || block.info.example
                            != group.first().and_then(|first| first.info.example.clone())
                    {
                        break;
                    }
                    group.push(block);
                    rest = after;
                }
                out.push(Event::Html(code_group(group).into()));
            }
            Event::Start(Tag::BlockQuote(Some(kind))) => {
                let (class, title) = callout(*kind);
                out.push(Event::Html(
                    format!(
                        "<aside class=\"callout {class}\"><p class=\"callout-title\">{title}</p>\n"
                    )
                    .into(),
                ));
            }
            Event::End(TagEnd::BlockQuote(Some(_))) => out.push(Event::Html("</aside>\n".into())),
            Event::Start(Tag::Table(_)) => {
                out.push(Event::Html("<div class=\"table-wrap\">".into()));
                out.push(event.clone());
            }
            Event::End(TagEnd::Table) => {
                out.push(event.clone());
                out.push(Event::Html("</div>\n".into()));
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                links.push(dest_url.to_string());
                out.push(event.clone());
            }
            other => out.push(inert(other.clone())),
        }
    }
    let text = prose(&events);
    let mut html = String::with_capacity(markdown.len().saturating_mul(2));
    crate::writer::push_html(&mut html, out);
    Rendered {
        html,
        headings,
        text,
        links,
    }
}

/// Raw HTML in a page becomes text: pages carry content, never layout.
fn inert(event: Event<'_>) -> Event<'_> {
    match event {
        Event::Html(html) | Event::InlineHtml(html) => Event::Text(html),
        other => other,
    }
}

/// The events before the first that is `end`, and those after it.
fn split_before<'e, 'a>(
    events: &'e [Event<'a>],
    end: impl Fn(&Event<'a>) -> bool,
) -> (&'e [Event<'a>], &'e [Event<'a>]) {
    match events.iter().position(end) {
        Some(at) => {
            let (inner, after) = events.split_at_checked(at).unwrap_or((events, &[]));
            (inner, after.get(1..).unwrap_or_default())
        }
        None => (events, &[]),
    }
}

fn plain_text(events: &[Event<'_>]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Text(text) | Event::Code(text) => Some(text.as_ref()),
            _ => None,
        })
        .collect::<String>()
}

/// The prose as plain text: the title and code left out, and the words of
/// two blocks or two lines kept apart.
fn prose(events: &[Event<'_>]) -> String {
    let mut text = String::new();
    let mut skipping = false;
    for event in events {
        match event {
            Event::Start(
                Tag::Heading {
                    level: HeadingLevel::H1,
                    ..
                }
                | Tag::CodeBlock(_),
            ) => skipping = true,
            Event::End(TagEnd::Heading(HeadingLevel::H1) | TagEnd::CodeBlock) => skipping = false,
            Event::Text(words) | Event::Code(words) if !skipping => text.push_str(words),
            Event::End(
                TagEnd::Paragraph
                | TagEnd::Heading(_)
                | TagEnd::Item
                | TagEnd::TableCell
                | TagEnd::BlockQuote(_)
                | TagEnd::List(_),
            )
            | Event::SoftBreak
            | Event::HardBreak => text.push(' '),
            _ => {}
        }
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn heading_number(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// The code block whose start was `kind` and whose text and end begin
/// `events`, and the events after it.
fn read_block<'e, 'a>(
    kind: &CodeBlockKind<'a>,
    events: &'e [Event<'a>],
) -> (Block, &'e [Event<'a>]) {
    let info = match kind {
        CodeBlockKind::Fenced(info) => Info::parse(info),
        CodeBlockKind::Indented => Info::default(),
    };
    let (inner, after) = split_before(events, |event| {
        matches!(event, Event::End(TagEnd::CodeBlock))
    });
    let code = inner.iter().fold(String::new(), |mut code, event| {
        if let Event::Text(text) = event {
            code.push_str(text);
        }
        code
    });
    (Block { info, code }, after)
}

/// One code block, or an example's blocks as tabs, with a copy button.
fn code_group(mut blocks: Vec<Block>) -> String {
    blocks.sort_by_key(|block| tab_rank(&block.info.language));
    let example = blocks.first().and_then(|block| block.info.example.clone());
    let mut html = String::from("<div class=\"code\"");
    if let Some(example) = &example {
        let _ = write!(html, " data-example=\"{}\"", escape(example));
    }
    html.push_str("><div class=\"code-bar\">");
    if example.is_some() {
        html.push_str("<div class=\"tabs\" role=\"tablist\" aria-label=\"Language\">");
        for (number, block) in blocks.iter().enumerate() {
            let label = language_label(&block.info.language, true);
            let label = if label.is_empty() {
                block.info.language.as_str()
            } else {
                label
            };
            let _ = write!(
                html,
                "<button type=\"button\" role=\"tab\" data-lang=\"{}\" aria-selected=\"{}\">{}</button>",
                escape(&block.info.language),
                number == 0,
                escape(label)
            );
        }
        html.push_str("</div>");
    } else if let Some(block) = blocks.first() {
        let _ = write!(
            html,
            "<span class=\"code-lang\">{}</span>",
            language_label(&block.info.language, false)
        );
    }
    html.push_str(
        "<button type=\"button\" class=\"copy\" aria-label=\"Copy code\">Copy</button></div>",
    );
    for (number, block) in blocks.iter().enumerate() {
        let hidden = if number == 0 { "" } else { " hidden" };
        let _ = write!(
            html,
            "<pre data-lang=\"{}\"{hidden}><code>{}</code></pre>",
            escape(&block.info.language),
            highlight(&block.info.language, block.code.trim_end_matches('\n'))
        );
    }
    html.push_str("</div>\n");
    html
}

fn callout(kind: BlockQuoteKind) -> (&'static str, &'static str) {
    match kind {
        BlockQuoteKind::Note => ("note", "Note"),
        BlockQuoteKind::Tip => ("tip", "Tip"),
        BlockQuoteKind::Important => ("important", "Important"),
        BlockQuoteKind::Warning => ("warning", "Warning"),
        BlockQuoteKind::Caution => ("caution", "Caution"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_title_is_left_to_the_layout_and_headings_get_ids() {
        let page = render(
            "# Files\n\nLead.\n\n## Put a file in\n\n### `tilo_do` and you\n\n## Put a file in\n",
        );
        assert!(!page.html.contains("<h1"), "{}", page.html);
        assert!(page.html.contains(
            "<h2 id=\"put-a-file-in\">Put a file in<a class=\"anchor\" href=\"#put-a-file-in\""
        ));
        assert!(
            page.html
                .contains("<h3 id=\"tilo_do-and-you\"><code>tilo_do</code> and you")
        );
        let ids: Vec<_> = page
            .headings
            .iter()
            .map(|heading| heading.id.as_str())
            .collect();
        assert_eq!(ids, ["put-a-file-in", "tilo_do-and-you", "put-a-file-in-1"]);
        assert!(
            page.text.starts_with("Lead. Put a file in tilo_do and you"),
            "{}",
            page.text
        );
        let inline = render("Run `tilo keys revoke <grant>`), then *stop*.\n");
        assert_eq!(inline.text, "Run tilo keys revoke <grant>), then stop.");
    }

    #[test]
    fn one_examples_blocks_become_tabs_in_a_fixed_order() {
        let page = render(
            "```typescript example=create\nconst a = 1;\n```\n\n```python example=create\na = 1\n```\n\n```python example=other\nb = 2\n```\n",
        );
        let first = page.html.find("data-example=\"create\"").unwrap();
        let other = page.html.find("data-example=\"other\"").unwrap();
        let tabs = page.html.get(first..other).unwrap();
        let python = tabs
            .find("data-lang=\"python\" aria-selected=\"true\">Python")
            .unwrap();
        let typescript = tabs
            .find("data-lang=\"typescript\" aria-selected=\"false\">TypeScript")
            .unwrap();
        assert!(python < typescript);
        assert!(tabs.contains("<pre data-lang=\"typescript\" hidden>"));
        assert_eq!(page.html.matches("class=\"code\"").count(), 2);
    }

    #[test]
    fn plain_blocks_stand_alone() {
        let page = render("```sh\nuv add tilo\n```\n\n```sh\ntilo login\n```\n");
        assert_eq!(
            page.html
                .matches("<span class=\"code-lang\">Shell</span>")
                .count(),
            2
        );
    }

    #[test]
    fn alerts_become_callouts_and_tables_scroll() {
        let page =
            render("> [!WARNING]\n> Deleting is final.\n\n| a | b |\n|---|---|\n| 1 | 2 |\n");
        assert!(
            page.html.contains(
                "<aside class=\"callout warning\"><p class=\"callout-title\">Warning</p>"
            )
        );
        assert!(page.html.contains("<div class=\"table-wrap\"><table>"));
    }

    #[test]
    fn raw_html_is_shown_not_run() {
        let page = render("Hi <script>alert(1)</script>\n\n<div class=\"x\">y</div>\n");
        assert!(!page.html.contains("<script>"), "{}", page.html);
        assert!(page.html.contains("&lt;script&gt;"));
    }
}
