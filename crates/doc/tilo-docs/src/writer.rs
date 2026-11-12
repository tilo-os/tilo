//! Markdown events as HTML. `Event::Html` is markup this crate made (a
//! page's own raw HTML arrives here already turned into text).

use pulldown_cmark::{Alignment, Event, Tag, TagEnd};
use std::fmt::Write as _;

use crate::html::escape;

pub fn push_html<'a>(out: &mut String, events: impl IntoIterator<Item = Event<'a>>) {
    let mut writer = Writer::default();
    for event in events {
        writer.event(out, event);
    }
}

#[derive(Default)]
struct Writer {
    alignments: Vec<Alignment>,
    in_head: bool,
    cell: usize,
    /// Inside an image, whose text is its `alt`, not markup.
    image_depth: usize,
}

impl Writer {
    fn event(&mut self, out: &mut String, event: Event<'_>) {
        if self.image_depth > 0 {
            match event {
                Event::Start(Tag::Image { .. }) => {
                    self.image_depth = self.image_depth.saturating_add(1);
                }
                Event::End(TagEnd::Image) => {
                    self.image_depth = self.image_depth.saturating_sub(1);
                    if self.image_depth == 0 {
                        out.push_str("\">");
                    }
                }
                Event::Text(text) | Event::Code(text) => out.push_str(&escape(&text)),
                _ => {}
            }
            return;
        }
        match event {
            Event::Start(tag) => self.start(out, tag),
            Event::End(tag) => self.end(out, tag),
            Event::Text(text) => out.push_str(&escape(&text)),
            Event::Code(code) => {
                out.push_str("<code>");
                out.push_str(&escape(&code));
                out.push_str("</code>");
            }
            Event::Html(html) | Event::InlineHtml(html) => out.push_str(&html),
            Event::SoftBreak => out.push('\n'),
            Event::HardBreak => out.push_str("<br>\n"),
            Event::Rule => out.push_str("<hr>\n"),
            Event::TaskListMarker(done) => {
                out.push_str(if done {
                    "<input type=\"checkbox\" disabled checked> "
                } else {
                    "<input type=\"checkbox\" disabled> "
                });
            }
            Event::FootnoteReference(text) | Event::InlineMath(text) | Event::DisplayMath(text) => {
                out.push_str(&escape(&text));
            }
        }
    }

    fn start(&mut self, out: &mut String, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => out.push_str("<p>"),
            Tag::BlockQuote(_) => out.push_str("<blockquote>\n"),
            Tag::List(Some(1)) => out.push_str("<ol>\n"),
            Tag::List(Some(first)) => {
                let _ = writeln!(out, "<ol start=\"{first}\">");
            }
            Tag::List(None) => out.push_str("<ul>\n"),
            Tag::Item => out.push_str("<li>"),
            Tag::Table(alignments) => {
                self.alignments = alignments;
                out.push_str("<table>");
            }
            Tag::TableHead => {
                self.in_head = true;
                self.cell = 0;
                out.push_str("<thead><tr>");
            }
            Tag::TableRow => {
                self.cell = 0;
                out.push_str("<tr>");
            }
            Tag::TableCell => {
                out.push_str(if self.in_head { "<th" } else { "<td" });
                match self.alignments.get(self.cell) {
                    Some(Alignment::Center) => out.push_str(" style=\"text-align:center\""),
                    Some(Alignment::Right) => out.push_str(" style=\"text-align:right\""),
                    _ => {}
                }
                out.push('>');
            }
            Tag::Emphasis => out.push_str("<em>"),
            Tag::Strong => out.push_str("<strong>"),
            Tag::Strikethrough => out.push_str("<del>"),
            Tag::Link {
                dest_url, title, ..
            } => {
                let _ = write!(out, "<a href=\"{}\"", escape(safe_url(&dest_url)));
                if !title.is_empty() {
                    let _ = write!(out, " title=\"{}\"", escape(&title));
                }
                out.push('>');
            }
            Tag::Image {
                dest_url, title, ..
            } => {
                let _ = write!(
                    out,
                    "<img src=\"{}\" loading=\"lazy\"",
                    escape(safe_url(&dest_url))
                );
                if !title.is_empty() {
                    let _ = write!(out, " title=\"{}\"", escape(&title));
                }
                out.push_str(" alt=\"");
                self.image_depth = 1;
            }
            Tag::Heading { .. }
            | Tag::CodeBlock(_)
            | Tag::HtmlBlock
            | Tag::FootnoteDefinition(_)
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
            | Tag::MetadataBlock(_) => {}
        }
    }

    fn end(&mut self, out: &mut String, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => out.push_str("</p>\n"),
            TagEnd::BlockQuote(_) => out.push_str("</blockquote>\n"),
            TagEnd::List(true) => out.push_str("</ol>\n"),
            TagEnd::List(false) => out.push_str("</ul>\n"),
            TagEnd::Item => out.push_str("</li>\n"),
            TagEnd::Table => out.push_str("</tbody></table>"),
            TagEnd::TableHead => {
                self.in_head = false;
                out.push_str("</tr></thead><tbody>\n");
            }
            TagEnd::TableRow => out.push_str("</tr>\n"),
            TagEnd::TableCell => {
                out.push_str(if self.in_head { "</th>" } else { "</td>" });
                self.cell = self.cell.saturating_add(1);
            }
            TagEnd::Emphasis => out.push_str("</em>"),
            TagEnd::Strong => out.push_str("</strong>"),
            TagEnd::Strikethrough => out.push_str("</del>"),
            TagEnd::Link => out.push_str("</a>"),
            TagEnd::Image
            | TagEnd::Heading(_)
            | TagEnd::CodeBlock
            | TagEnd::HtmlBlock
            | TagEnd::FootnoteDefinition
            | TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition
            | TagEnd::MetadataBlock(_) => {}
        }
    }
}

/// A link may lead anywhere but into script.
fn safe_url(url: &str) -> &str {
    if url
        .trim_start()
        .to_ascii_lowercase()
        .starts_with("javascript:")
    {
        "#"
    } else {
        url
    }
}

#[cfg(test)]
mod tests {
    use pulldown_cmark::{Options, Parser};

    fn html(markdown: &str) -> String {
        let mut out = String::new();
        super::push_html(
            &mut out,
            Parser::new_ext(
                markdown,
                Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH,
            ),
        );
        out
    }

    #[test]
    fn prose_lists_links_and_tables() {
        assert_eq!(
            html("A *b* **c** `d` ~~e~~ [f](/g \"h\")\n"),
            "<p>A <em>b</em> <strong>c</strong> <code>d</code> <del>e</del> <a href=\"/g\" title=\"h\">f</a></p>\n"
        );
        assert_eq!(
            html("3. x\n4. y\n"),
            "<ol start=\"3\">\n<li>x</li>\n<li>y</li>\n</ol>\n"
        );
        assert_eq!(
            html("| a | b |\n|---|--:|\n| 1 | 2 |\n"),
            "<table><thead><tr><th>a</th><th style=\"text-align:right\">b</th></tr></thead><tbody>\n<tr><td>1</td><td style=\"text-align:right\">2</td></tr>\n</tbody></table>"
        );
        assert_eq!(
            html("![a *b*](/i.png)\n"),
            "<p><img src=\"/i.png\" loading=\"lazy\" alt=\"a b\"></p>\n"
        );
        assert_eq!(
            html("[x](javascript:alert(1))\n"),
            "<p><a href=\"#\">x</a></p>\n"
        );
    }
}
