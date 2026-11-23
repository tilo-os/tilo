//! The device's PDF engine, as a package's `pdf.*` reaches it: the one reader crate
//! (`tilo-pdf-reader`) over the one engine (`tilo-pdf`), answering with the JSON a package
//! reads into its own records.
//!
//! Every call is made on the instance's thread, from the bytes the runtime read out of the
//! person's content store, so nothing here opens a file or holds a path.

use std::rc::Rc;

use tilo_pdf_reader as reader;
use tilolang_tilo::runtime::pdf::PdfHost;

/// The device's PDF engine: one per instance, sharing the process's engine.
#[derive(Clone, Copy, Debug, Default)]
pub struct Engine;

impl Engine {
    /// The engine an instance sets, when the device can read PDFs at all.
    #[must_use]
    pub fn host() -> Rc<dyn PdfHost> {
        Rc::new(Self)
    }
}

fn said(error: reader::Error) -> String {
    error.to_string()
}

fn json<T: serde::Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_string(value)
        .map_err(|error| format!("the PDF engine's answer cannot be written: {error}"))
}

impl PdfHost for Engine {
    fn pages(&self, bytes: &[u8]) -> Result<String, String> {
        let document = reader::Document::open(bytes).map_err(said)?;
        json(&document.pages().map_err(said)?)
    }

    fn outline(&self, bytes: &[u8]) -> Result<String, String> {
        let document = reader::Document::open(bytes).map_err(said)?;
        json(&document.outline())
    }

    fn text(&self, bytes: &[u8], page: i64) -> Result<String, String> {
        let document = reader::Document::open(bytes).map_err(said)?;
        let page = page_of(page)?;
        json(&document.text(page).map_err(said)?)
    }

    fn links(&self, bytes: &[u8], page: i64) -> Result<String, String> {
        let document = reader::Document::open(bytes).map_err(said)?;
        let page = page_of(page)?;
        json(&document.links(page).map_err(said)?)
    }

    fn fields(&self, bytes: &[u8]) -> Result<String, String> {
        let document = reader::Document::open(bytes).map_err(said)?;
        json(&document.fields().map_err(said)?)
    }

    fn markup(&self, bytes: &[u8]) -> Result<String, String> {
        let document = reader::Document::open(bytes).map_err(said)?;
        json(&document.markup().map_err(said)?)
    }

    fn picture(&self, bytes: &[u8], page: i64, width: i64) -> Result<Vec<u8>, String> {
        let document = reader::Document::open(bytes).map_err(said)?;
        let page = page_of(page)?;
        let width = u32::try_from(width.max(0)).unwrap_or(u32::MAX);
        document.picture(page, width).map_err(said)
    }

    fn import(&self, bytes: &[u8]) -> Result<String, String> {
        let (kind, pages) = reader::import(bytes).map_err(said)?;
        #[derive(serde::Serialize)]
        struct Held<'a> {
            kind: &'a str,
            pages: Vec<Page>,
        }
        #[derive(serde::Serialize)]
        struct Page {
            page: usize,
            width: f32,
            height: f32,
        }
        json(&Held {
            kind,
            pages: pages
                .iter()
                .enumerate()
                .map(|(at, (width, height))| Page {
                    page: at + 1,
                    width: *width,
                    height: *height,
                })
                .collect(),
        })
    }

    fn save(&self, sources: &[&[u8]], request: &str) -> Result<Vec<u8>, String> {
        let request = reader::SaveRequest::from_json(request)?;
        reader::save(sources, &request).map_err(said)
    }
}

fn page_of(page: i64) -> Result<usize, String> {
    usize::try_from(page).map_err(|_| format!("the document has no page {page}"))
}
