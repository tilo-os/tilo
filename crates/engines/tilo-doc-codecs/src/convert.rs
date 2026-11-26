//! A document file read as Markdown, and Markdown written as a document file: the two
//! projections chained through a scratch [`Document`], by file extension.
//!
//! Plain text (`.txt` and any extension this build has no codec for) is read line by line, one
//! paragraph a line, never parsed as Markdown.

use tilo_content::ContentInputs;
use tilo_doc::Document;

use crate::format::{CodecError, FormatProvider};
use crate::markdown::MarkdownCodec;

pub const MAX_DOCUMENT_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reading {
    Markdown,
    /// A document format this build has a codec for.
    Document,
    Text,
}

/// How a file with extension `ext` (case-insensitive, without the dot) is
/// read: Markdown, a document format this build imports, or plain text.
#[must_use]
pub fn reading(ext: &str) -> Reading {
    match ext.to_ascii_lowercase().as_str() {
        "md" | "markdown" => Reading::Markdown,
        other if crate::provider_for_ext(other).is_some() => Reading::Document,
        _ => Reading::Text,
    }
}

pub fn import(ext: &str, bytes: &[u8]) -> Result<(Document, ContentInputs), CodecError> {
    if bytes.len() > MAX_DOCUMENT_BYTES {
        return Err(CodecError::Unsupported(format!(
            "the file is larger than {} MiB",
            MAX_DOCUMENT_BYTES / (1024 * 1024)
        )));
    }
    let mut document = Document::new();
    let mut inputs = ContentInputs::new();
    match (reading(ext), crate::provider_for_ext(ext)) {
        (Reading::Document, Some(provider)) => {
            provider.import(bytes, &mut document.as_mut(), &mut inputs)?;
        }
        (Reading::Markdown, _) => {
            MarkdownCodec.import(bytes, &mut document.as_mut(), &mut inputs)?;
        }
        _ => import_text(&mut document, bytes)?,
    }
    Ok((document, inputs))
}

fn import_text(document: &mut Document, bytes: &[u8]) -> Result<(), CodecError> {
    let text = std::str::from_utf8(bytes).map_err(|_| {
        CodecError::Unsupported("the file is neither text nor a document format".into())
    })?;
    let mut dm = document.as_mut();
    dm.bulk(|dm| {
        let mut previous = None;
        for line in text.lines() {
            let block = dm.insert_block(previous.as_deref(), "paragraph");
            dm.splice_text(&block, 0, 0, line);
            previous = Some(block);
        }
    });
    Ok(())
}

pub fn markdown_of(ext: &str, bytes: &[u8]) -> Result<String, CodecError> {
    match reading(ext) {
        Reading::Document => {
            let (document, inputs) = import(ext, bytes)?;
            let markdown = MarkdownCodec.export(document.as_ref(), &inputs)?;
            String::from_utf8(markdown).map_err(|error| CodecError::Export(error.to_string()))
        }
        Reading::Markdown | Reading::Text => {
            if bytes.len() > MAX_DOCUMENT_BYTES {
                return Err(CodecError::Unsupported(format!(
                    "the file is larger than {} MiB",
                    MAX_DOCUMENT_BYTES / (1024 * 1024)
                )));
            }
            String::from_utf8(bytes.to_vec()).map_err(|_| {
                CodecError::Unsupported("the file is neither text nor a document format".into())
            })
        }
    }
}

/// `document` written as a file with extension `ext`: Markdown, plain text
/// (one line a paragraph), or a document format this build exports.
pub fn export(
    document: &Document,
    content: &dyn tilo_content::ContentSource,
    ext: &str,
) -> Result<Vec<u8>, CodecError> {
    match ext.to_ascii_lowercase().as_str() {
        "md" | "markdown" => MarkdownCodec.export(document.as_ref(), content),
        "txt" | "text" => {
            let lines: Vec<String> = document
                .as_ref()
                .materialize()
                .blocks
                .into_iter()
                .map(|block| block.text)
                .collect();
            Ok(format!("{}\n", lines.join("\n")).into_bytes())
        }
        #[cfg(feature = "html")]
        "html" | "htm" => Ok(crate::export_html::render_html(
            &document.as_ref().materialize(),
            crate::export_html::HtmlOptions::default(),
            content,
        )
        .into_bytes()),
        #[cfg(feature = "rtf")]
        "rtf" => Ok(crate::export_rtf::render_rtf(&document.as_ref().materialize()).into_bytes()),
        other => match crate::provider_for_ext(other) {
            Some(provider) => provider.export(document.as_ref(), content),
            None => Err(CodecError::Unsupported(format!(
                "no document format writes .{other} files"
            ))),
        },
    }
}

pub fn from_markdown(markdown: &str, ext: &str) -> Result<Vec<u8>, CodecError> {
    match ext.to_ascii_lowercase().as_str() {
        "md" | "markdown" | "txt" | "text" => Ok(markdown.as_bytes().to_vec()),
        _ => {
            let (document, inputs) = import("md", markdown.as_bytes())?;
            export(&document, &inputs, ext)
        }
    }
}
