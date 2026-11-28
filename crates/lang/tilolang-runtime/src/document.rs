//! Document files: a Word, `OpenDocument`, RTF, EPUB, HTML or Markdown file read as the Markdown
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
            // `File.of_rich_document(name, rich, format)`: the editor's text written as a document
            // of that format, its comments and tracked changes with it.
            Imp::FileOfRichDocument => {
                let name = self.text(a.h(0)?)?;
                let rich = self.rich_arg(a.h(1)?)?;
                let format = self.text(a.h(2)?)?;
                let format = format.trim().trim_start_matches('.').to_ascii_lowercase();
                let bytes = if format == "pdf" {
                    tilo_doc_codecs::rich::document_from_blocks(&rich.blocks)
                        .map_err(|why| why.to_string())
                        .and_then(|document| {
                            tilo_doc_pdf::render_pdf(
                                &document.as_ref().materialize(),
                                &tilo_content::NoContent,
                            )
                            .map_err(|why| format!("{why:?}"))
                        })
                } else {
                    tilo_doc_codecs::rich::document_of_blocks(&rich.blocks, &format)
                        .map_err(|why| why.to_string())
                };
                match bytes {
                    Ok(bytes) => match self.keep_bytes(&name, media_of(&format), &bytes) {
                        Ok(file) => self.ret_h(Value::file(file)),
                        Err(why) => trap(why),
                    },
                    Err(why) => trap(format!("a document cannot be written as .{format}: {why}")),
                }
            }
            // `File.overwrite(path, file)`: the bytes of `file` written over the file the person
            // chose at `path`, as saving a document back where it was opened. A file that was not
            // chosen cannot be written back to: it names no place on this device.
            Imp::FileOverwrite => {
                let path = self.text(a.h(0)?)?;
                let file = self.file_arg(a.h(1)?)?;
                let answer = if path.trim().is_empty() {
                    Err(
                        "this document is not a file on this device: save it as a new file"
                            .to_owned(),
                    )
                } else {
                    match self.file_bytes(&file) {
                        Ok(bytes) => match self.blobs.borrow().as_ref() {
                            Some(blobs) => blobs.write_back(&file, &path, &bytes),
                            None => Err("this host writes no files".to_owned()),
                        },
                        Err(why) => Err(why),
                    }
                };
                self.result(answer.map(|()| Value::Unit))
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
