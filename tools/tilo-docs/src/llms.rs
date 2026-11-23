//! The book for agents (<https://llmstxt.org>): `llms.txt` lists every page's
//! Markdown with what it covers, and `llms-full.txt` is every page in one file.

use crate::book::Book;
use std::fmt::Write as _;

#[must_use]
pub fn index(book: &Book, summary: &str) -> String {
    let mut out = format!("# {}\n\n> {summary}\n", book.title);
    let mut section = None;
    for (number, page) in &book.pages {
        if section != Some(*number) {
            section = Some(*number);
            let title = book
                .sections
                .get(*number)
                .map_or("", |section| section.title.as_str());
            let _ = write!(out, "\n## {title}\n\n");
        }
        let _ = writeln!(
            out,
            "- [{}]({}): {}",
            page.title,
            book.absolute(&page.markdown_url(&book.base)),
            page.description
        );
    }
    out
}

#[must_use]
pub fn full(book: &Book) -> String {
    let mut out = String::new();
    for (_, page) in &book.pages {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        let _ = write!(out, "<!-- {} -->\n\n", book.absolute(&page.url));
        out.push_str(page.markdown.trim_end());
        out.push('\n');
    }
    out
}
