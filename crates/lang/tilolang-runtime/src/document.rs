//! Document files: a Word, OpenDocument, RTF, EPUB, HTML or Markdown file read as the Markdown
//! an editor shows, and Markdown written as a file of one of those formats or as a PDF.
//!
//! The work is the engine's ([`tilo_doc_codecs::convert`], and `tilo-doc-pdf` for a PDF), not
//! the language's and not an app's: every package that opens or saves a document reaches the
//! same code, so a file reads the same whichever app asked. A call reads the content bytes it
//! was given and nothing outside them, so an action that only reads a document declares no
//! `uses`, as one reading a file as text does.

use crate::abi::Imp;
use crate::core::{Args, Core, HResult, trap};
use crate::engine::Wasm;
use crate::value::Value;

impl Core {
    /// `file.markdown()` and `File.of_document(name, markdown, format)`.
    pub(crate) fn document_import(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        match imp {
            Imp::FileMarkdown => {
                let file = self.file_arg(a.h(0)?)?;
                let answer = self.file_bytes(&file).and_then(|bytes| {
                    tilo_doc_codecs::convert::markdown_of(&extension(&file.name), &bytes)
                        .map_err(|why| why.to_string())
                });
                self.result(answer.map(Value::text))
            }
            Imp::FileOfDocument => {
                let name = self.text(a.h(0)?)?;
                let markdown = self.text(a.h(1)?)?;
                let format = self.text(a.h(2)?)?;
                match written(&markdown, &format) {
                    Ok(bytes) => match self.keep_bytes(&name, media_of(&format), &bytes) {
                        Ok(file) => self.ret_h(Value::file(file)),
                        Err(why) => trap(why),
                    },
                    Err(why) => trap(why),
                }
            }
            _ => trap("not a document import"),
        }
    }
}

/// Markdown written as a file of `format`, its extension without the dot.
fn written(markdown: &str, format: &str) -> Result<Vec<u8>, String> {
    let format = format.trim().trim_start_matches('.').to_ascii_lowercase();
    let said = |why: tilo_doc_codecs::CodecError| {
        format!("a document cannot be written as .{format}: {why}")
    };
    if format == "pdf" {
        let (document, inputs) =
            tilo_doc_codecs::convert::import("md", markdown.as_bytes()).map_err(said)?;
        return tilo_doc_pdf::render_pdf(&document.as_ref().materialize(), &inputs)
            .map_err(|why| format!("a document cannot be written as a PDF: {why:?}"));
    }
    tilo_doc_codecs::convert::from_markdown(markdown, &format).map_err(said)
}

/// The extension a file's name ends in, lowercased, without the dot: `Letter.docx` reads `docx`.
fn extension(name: &str) -> String {
    name.rsplit_once('.')
        .map_or_else(|| name.to_owned(), |(_, ext)| ext.to_owned())
        .to_ascii_lowercase()
}

/// The media type a document written as `format` is: what a host keeps the file under.
fn media_of(format: &str) -> &'static str {
    match format
        .trim()
        .trim_start_matches('.')
        .to_ascii_lowercase()
        .as_str()
    {
        "md" | "markdown" => "text/markdown",
        "txt" | "text" => "text/plain",
        "html" | "htm" => "text/html",
        "rtf" => "application/rtf",
        "odt" => "application/vnd.oasis.opendocument.text",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "epub" => "application/epub+zip",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}
