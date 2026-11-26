//! What a message says as a recipient's mail program shows it: the Markdown the composer edits,
//! rendered to HTML and carried beside the words as the message's formatted alternative. The
//! rendering is the document system's, so a message reads as a document of the workspace does.

use tilo_doc_codecs::convert::import;
use tilo_doc_codecs::export_html::{HtmlOptions, render_html};

/// The HTML a message written as `markdown` carries beside its words, or empty text when the
/// message says nothing (an empty alternative is no alternative: the words alone are the
/// message then).
///
/// A message goes out as `multipart/alternative`, so every recipient reads what its program
/// shows best: the HTML where it renders formatted mail, and the words as they were written
/// where it does not.
#[must_use]
pub fn of_markdown(markdown: &str) -> String {
    if markdown.trim().is_empty() {
        return String::new();
    }
    let Ok((document, inputs)) = import("md", markdown.as_bytes()) else {
        // What the document system will not read goes as the words alone: a message still
        // leaves, and it leaves whole.
        return String::new();
    };
    let options = HtmlOptions {
        fragment: true,
        ..HtmlOptions::default()
    };
    render_html(&document.as_ref().materialize(), options, &inputs)
}
