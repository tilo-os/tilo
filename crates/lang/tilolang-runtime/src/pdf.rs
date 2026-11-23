//! PDFs: what a reader asks of the device's PDF engine, and nothing about how a page is shown.
//!
//! A package reads a PDF through `pdf.*` (`pdf.pages`, `pdf.text`, `pdf.links`, `pdf.fields`,
//! `pdf.outline`, `pdf.markup`, `pdf.picture`, `pdf.import`, `pdf.save`). Each answer is JSON
//! text the package reads into its own records (`json.decode<T>(text)`), so the shapes are the
//! engine's ([`tilo-pdf-reader`](https://docs.rs/tilo-pdf-reader)) and the app's, and nothing
//! of the language's is added for one app.
//!
//! The work is the device's, never the sandboxed code's: the runtime hands the file's bytes to
//! the host's [`PdfHost`], which answers with the engine's JSON or a page's picture. Until a
//! host sets one, every call answers that this host cannot read PDFs.
//!
//! Every call is free of effects as far as the language is concerned — it reads the bytes
//! it was given and nothing outside them — so an action that only reads a PDF declares no
//! `uses`, exactly as one reading a file as text does.
//!
//! ## Geometry
//!
//! Every rectangle and point an answer holds is in the page's **display space**: points from
//! the top left of the page's visible box after the page's own rotation. A page's plan turn
//! (a reader's own quarter turn) is the app's to apply when it draws and to pass back when it
//! saves.

use crate::abi::Imp;
use crate::core::{Args, Core, HResult, trap};
use crate::engine::Wasm;
use crate::value::Value;

/// The device's PDF engine, as the runtime reaches it.
///
/// A host answers from the app's own file bytes: the runtime reads the content store and hands
/// them over, so the host never needs a chooser or a path.
pub trait PdfHost {
    /// Every page of the document and its display size, as
    /// `[{"page": 1, "width": 612.0, "height": 792.0}]`.
    ///
    /// # Errors
    ///
    /// Why the bytes are no PDF this device reads.
    fn pages(&self, bytes: &[u8]) -> Result<String, String>;

    /// The document's contents, flattened, each entry with its depth, as
    /// `[{"title": "Chapter 1", "page": 3, "level": 0}]`; an entry that leads nowhere names page
    /// 0.
    ///
    /// # Errors
    ///
    /// Why the bytes are no PDF this device reads.
    fn outline(&self, bytes: &[u8]) -> Result<String, String>;

    /// One page's text in reading order, as
    /// `[{"text": "H", "line": 0, "x": 0.0, "y": 0.0, "w": 0.0, "h": 0.0, "placed": false}]`,
    /// the page counted from 1.
    ///
    /// # Errors
    ///
    /// Why the page's text cannot be read.
    fn text(&self, bytes: &[u8], page: i64) -> Result<String, String>;

    /// One page's links, as `[{"x": 0.0, "y": 0.0, "w": 0.0, "h": 0.0, "kind": "page",
    /// "to": 3}]`: `kind` is `page` (a page of this document, counted from 1), `uri` or `file`.
    ///
    /// # Errors
    ///
    /// Why the page's links cannot be read.
    fn links(&self, bytes: &[u8], page: i64) -> Result<String, String>;

    /// The document's form fields, as `[{"name": "", "kind": "text", "value": "", "checked":
    /// false, "export": "", "options": [], "page": 3, "rect": [0, 0, 0, 0], "read_only": false,
    /// "required": false}]`, a field's page counted from 1.
    ///
    /// # Errors
    ///
    /// Why the form cannot be read.
    fn fields(&self, bytes: &[u8]) -> Result<String, String>;

    /// The markup the file already carries and the values its form holds, as
    /// `{"marks": [...], "fields": [...]}`: each mark `{"page": 0, "index": 0, "name": "",
    /// "kind": "highlight", "color": "#ffff00", "contents": "", "author": "", "created": "",
    /// "modified": "", "rect": [0.0, 0.0, 0.0, 0.0], "quads": [], "strokes": [], "size": null,
    /// "in_reply_to": null, "reply": false, "hidden": false, "readable": true}`.
    ///
    /// # Errors
    ///
    /// Why a page's markup cannot be read.
    fn markup(&self, bytes: &[u8]) -> Result<String, String>;

    /// One page drawn as a PNG picture `width` pixels wide, its height following the page's
    /// shape, the page's own turn applied, the file's own reader markup not drawn.
    ///
    /// # Errors
    ///
    /// Why the page cannot be drawn or encoded.
    fn picture(&self, bytes: &[u8], page: i64, width: i64) -> Result<Vec<u8>, String>;

    /// What the file holds as pages, as `{"kind": "pdf", "pages": [{"page": 1, "width": 612.0,
    /// "height": 792.0}]}`; `kind` is `pdf` or `image`.
    ///
    /// # Errors
    ///
    /// Why the file is not one a document takes pages from.
    fn import(&self, bytes: &[u8]) -> Result<String, String>;

    /// The pages and markup a copy holds, as a PDF's bytes: `sources[0]` is the document, the
    /// rest are the files its imported pages come from, and `request` is the save request
    /// (`crates/engines/pdf` `SaveRequest`).
    ///
    /// # Errors
    ///
    /// Why a file is no PDF, why a page cannot be read or written, or that this device can
    /// write none now.
    fn save(&self, sources: &[&[u8]], request: &str) -> Result<Vec<u8>, String>;
}

/// A host that can read no PDFs: every call says so, in words a person reads.
pub struct NoPdf;

impl PdfHost for NoPdf {
    fn pages(&self, _bytes: &[u8]) -> Result<String, String> {
        Err(unavailable())
    }

    fn outline(&self, _bytes: &[u8]) -> Result<String, String> {
        Err(unavailable())
    }

    fn text(&self, _bytes: &[u8], _page: i64) -> Result<String, String> {
        Err(unavailable())
    }

    fn links(&self, _bytes: &[u8], _page: i64) -> Result<String, String> {
        Err(unavailable())
    }

    fn fields(&self, _bytes: &[u8]) -> Result<String, String> {
        Err(unavailable())
    }

    fn markup(&self, _bytes: &[u8]) -> Result<String, String> {
        Err(unavailable())
    }

    fn picture(&self, _bytes: &[u8], _page: i64, _width: i64) -> Result<Vec<u8>, String> {
        Err(unavailable())
    }

    fn import(&self, _bytes: &[u8]) -> Result<String, String> {
        Err(unavailable())
    }

    fn save(&self, _sources: &[&[u8]], _request: &str) -> Result<Vec<u8>, String> {
        Err(unavailable())
    }
}

fn unavailable() -> String {
    "PDFs cannot be read on this device".to_owned()
}

impl Core {
    /// `pdf.*`: the device's engine reads the bytes of the files the call names.
    pub(crate) fn pdf_import(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        match imp {
            Imp::PdfSave => {
                let files = self.file_list(a.h(0)?)?;
                let request = self.text(a.h(1)?)?;
                let sources: Vec<&[u8]> = files.iter().map(Vec::as_slice).collect();
                match self.pdf.borrow().save(&sources, &request) {
                    Ok(bytes) => {
                        let file = self
                            .keep_bytes("annotated.pdf", "application/pdf", &bytes)
                            .or_else(trap)?;
                        self.ret_h(Value::file(file))
                    }
                    Err(why) => self.result(Err(why)),
                }
            }
            Imp::PdfPicture => {
                let bytes = self.pdf_bytes(a.h(0)?)?;
                let page = a.long(1)?;
                let width = a.long(2)?;
                match self.pdf.borrow().picture(&bytes, page, width) {
                    Ok(png) => {
                        let file = self.keep_bytes("page.png", "image/png", &png).or_else(trap)?;
                        self.ret_h(Value::file(file))
                    }
                    Err(why) => self.result(Err(why)),
                }
            }
            _ => {
                let bytes = self.pdf_bytes(a.h(0)?)?;
                let page = if matches!(imp, Imp::PdfText | Imp::PdfLinks) {
                    a.long(1)?
                } else {
                    0
                };
                let host = self.pdf.borrow();
                let answer = match imp {
                    Imp::PdfPages => host.pages(&bytes),
                    Imp::PdfOutline => host.outline(&bytes),
                    Imp::PdfText => host.text(&bytes, page),
                    Imp::PdfLinks => host.links(&bytes, page),
                    Imp::PdfFields => host.fields(&bytes),
                    Imp::PdfMarkup => host.markup(&bytes),
                    Imp::PdfImport => host.import(&bytes),
                    _ => return trap("not a pdf import"),
                };
                drop(host);
                self.result(answer.map(Value::text))
            }
        }
    }

    /// The bytes of the file behind `h`, when this device holds them.
    fn pdf_bytes(&self, h: i32) -> HResult<Vec<u8>> {
        let file = self.file_arg(h)?;
        self.file_bytes(&file).or_else(trap)
    }

    /// The bytes of each file in the list behind `h`.
    fn file_list(&self, h: i32) -> HResult<Vec<Vec<u8>>> {
        let files = self.val_whole(h)?;
        let Value::List(items) = files else {
            return trap("a list of files was expected");
        };
        items
            .iter()
            .map(|item| match item {
                Value::File(file) => self.file_bytes(file).or_else(trap),
                other => trap(format!("expected a file, got {}", other.kind_name())),
            })
            .collect()
    }
}
