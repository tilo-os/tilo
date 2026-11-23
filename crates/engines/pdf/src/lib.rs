//! A PDF as a reader works with it, and nothing about how it is shown.
//!
//! The app that reads PDFs is `apps/tl/pdf`; what it cannot do itself is here: opening the
//! file's bytes, laying its pages out, reading their text, links and form fields, drawing one
//! page as a picture, taking over the markup the file already carries, and writing a copy with
//! the reader's markup in it. Every answer is data an app holds as resources; the window's
//! views and actions are the app's own.
//!
//! ## Space
//!
//! Every rectangle and point here is in a page's **display space**: PDF points from the top
//! left of the page's visible box, after the page's own `/Rotate`, x to the right and y down.
//! A page's [`Page::width`] and [`Page::height`] are that page's display size, so a mark a
//! reader placed at a fraction of one time lands at the same fraction of the page at every
//! zoom.
//!
//! A reader may also turn a page (its page plan's rotation, independent of the file's). This
//! crate neither applies nor knows that turn: the app turns its own drawing, and [`save`]
//! takes each output page's extra turn so markup reaches the file in the source page's own
//! user space.
//!
//! ## The markup a file carries
//!
//! [`Document::markup`] answers the highlights, notes, free text, ink, form values and replies
//! already in the bytes, in one flat list per page. The app decides which of them become
//! resources of its own; [`save`] then keeps an unchanged one as the file's own object and
//! replaces a changed one, so no mark is ever shown twice.

use serde::{Deserialize, Serialize};
use tilo_pdf::{Destination, OutlineEntry};

mod adopt;
mod dates;
mod export;

pub use adopt::{MAX_IMPORTED_ANNOTATIONS, annotation_key, imported_id, kind_of};
pub use dates::{parse_pdf_date, pdf_date};
pub use export::{ExportAdopted, ExportMark, ExportPage, ExportReply, ExportValue, SaveRequest, save};

/// The largest file a reader opens, and the largest one it takes pages from.
pub const MAX_DOCUMENT_BYTES: usize = 128 * 1024 * 1024;
pub const MAX_IMPORT_BYTES: usize = 64 * 1024 * 1024;

/// The most pages a document opens with, and the most one import brings.
pub const MAX_PAGES: usize = 2_048;
pub const MAX_IMPORT_PAGES: usize = 512;

/// The most characters one page's text layer holds, and the most links it lists.
pub const MAX_PAGE_CHARS: usize = 200_000;
pub const MAX_PAGE_LINKS: usize = 4_096;

/// The most entries a document's contents lists.
const MAX_OUTLINE: usize = 2_000;

/// A picture this crate draws: the most pixels a page may be rendered at on either side.
pub const MAX_PICTURE_SIDE: u32 = 4_096;

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    fn of(rect: tilo_pdf::Rect) -> Self {
        Self {
            x: rect.x0,
            y: rect.y0,
            w: rect.width(),
            h: rect.height(),
        }
    }

    fn to_pdf(self) -> tilo_pdf::Rect {
        tilo_pdf::Rect::from_xywh(self.x, self.y, self.w, self.h)
    }

    /// The rectangle as the points a request names it by: `[x, y, w, h]`.
    #[must_use]
    pub fn coordinates(self) -> [f32; 4] {
        [self.x, self.y, self.w, self.h]
    }

    #[must_use]
    pub fn from_coordinates(c: [f32; 4]) -> Self {
        Self {
            x: c[0],
            y: c[1],
            w: c[2],
            h: c[3],
        }
    }
}

/// One page of the document, as the reader shows it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Page {
    /// Its number, counted from 1.
    pub page: usize,
    /// Its display width and height, in points.
    pub width: f32,
    pub height: f32,
}

/// One character of a page's text layer, in display space: what a reader selects, copies and
/// searches by. Characters the engine placed itself — an inter-word space, a line's end — are
/// marked so the app knows the text is not the file's own glyphs there.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Glyph {
    pub text: String,
    /// The line it stands on, from 0 down the page: a line's end is where the next glyph's
    /// line changes.
    pub line: u32,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Whether the engine placed it (a space between words, a line's end) rather than the file.
    pub placed: bool,
}

/// Where a link goes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "to", rename_all = "lowercase")]
pub enum LinkTarget {
    /// A page of the same document, counted from 1.
    Page(usize),
    /// An address outside the file.
    Uri(String),
    /// Another file, by its path.
    File(String),
}

/// One link area of a page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Link {
    pub rect: Rect,
    #[serde(flatten)]
    pub target: LinkTarget,
}

/// One entry of the document's contents.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Outline {
    pub title: String,
    /// The page it leads to, counted from 1; 0 for an entry that leads nowhere.
    pub page: usize,
    /// How deep it stands: 0 at the top.
    pub level: usize,
}

/// The kind of a form field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldKind {
    Text,
    Checkbox,
    Radio,
    Choice,
}

/// One form field of the document, with the widget a person fills in.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Field {
    pub name: String,
    pub kind: FieldKind,
    /// Its value as text: what a text field holds, a choice's option, or a checkbox's export.
    pub value: String,
    /// Whether a checkbox or a radio button stands on.
    pub checked: bool,
    /// For a checkbox or radio: the export value its widget sets.
    pub export: String,
    /// The options a choice offers.
    pub options: Vec<String>,
    /// The page its widget stands on (1-based), when it has one.
    pub page: Option<usize>,
    /// Its widget's area on that page, in display space.
    pub rect: Option<Rect>,
    pub read_only: bool,
    pub required: bool,
}

/// A mark the document's own bytes already carry, in display space.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExistingMark {
    /// Its place on its page, 0-based.
    pub page: usize,
    /// Its index in the page's own annotation list: with the page, its address in the file.
    pub index: usize,
    /// Its `/NM`, empty when it names none.
    pub name: String,
    /// `highlight`, `underline`, `strikeout`, `squiggly`, `ink`, `note`, `freetext`, `popup`
    /// or `other`.
    pub kind: String,
    /// Its colour, as `#rrggbb`, when it says one.
    pub color: Option<String>,
    /// What it says: a note's or free text's words.
    pub contents: String,
    pub author: String,
    /// `/CreationDate` and `/M` as the file writes them (`D:20260317120000Z`).
    pub created: String,
    pub modified: String,
    /// Where it is: its area, the lines a text mark covers, the strokes ink was drawn with.
    pub rect: Rect,
    pub quads: Vec<Rect>,
    pub strokes: Vec<Vec<f32>>,
    /// A free text's font size.
    pub size: Option<f32>,
    /// The index of the annotation on the same page this one answers.
    pub in_reply_to: Option<usize>,
    /// Whether it answers another annotation.
    pub reply: bool,
    /// Whether the file hides it or marks it not to be shown.
    pub hidden: bool,
    /// Whether it is one of the marks a reader keeps as its own: text markup, a note, free
    /// text or ink, with the replies and pop-ups that belong to them.
    pub readable: bool,
}

/// The markup a document's bytes carry.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Existing {
    pub marks: Vec<ExistingMark>,
    pub fields: Vec<Field>,
}

/// Why a document could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The bytes are more than a reader opens.
    TooLarge,
    /// The file's password never came.
    Password,
    /// The host has no engine that can read PDFs now.
    Unavailable(String),
    /// The file is not a PDF, or not one this engine reads.
    Unreadable(String),
    /// The page is not one the document has.
    NoPage(usize),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge => write!(
                f,
                "the file is larger than the {} MB a reader opens",
                MAX_DOCUMENT_BYTES / (1024 * 1024)
            ),
            Self::Password => write!(f, "the PDF is password-protected, so it cannot be opened"),
            Self::Unavailable(why) => write!(f, "PDFs cannot be read on this device now: {why}"),
            Self::Unreadable(why) => write!(f, "the file is not a readable PDF ({why})"),
            Self::NoPage(page) => write!(f, "the document has no page {page}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<tilo_pdf::Error> for Error {
    fn from(error: tilo_pdf::Error) -> Self {
        match error {
            tilo_pdf::Error::Password => Self::Password,
            error @ (tilo_pdf::Error::Unavailable(_) | tilo_pdf::Error::Unconfined(_)) => {
                Self::Unavailable(error.to_string())
            }
            error => Self::Unreadable(error.to_string()),
        }
    }
}

/// A PDF the reader reads.
pub struct Document {
    inner: tilo_pdf::Document,
}

impl std::fmt::Debug for Document {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Document")
            .field("pages", &self.inner.page_count())
            .finish_non_exhaustive()
    }
}

impl Document {
    /// The file's bytes, read as a PDF.
    ///
    /// # Errors
    ///
    /// Why the bytes are no PDF a reader opens, or that this device can read none now.
    pub fn open(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() > MAX_DOCUMENT_BYTES {
            return Err(Error::TooLarge);
        }
        let engine = tilo_pdf::Engine::shared()?;
        let inner = engine.open_bytes(bytes)?;
        if inner.page_count() == 0 || inner.page_count() > MAX_PAGES {
            return Err(Error::Unreadable(format!(
                "it has {} pages, and a reader opens 1 to {MAX_PAGES}",
                inner.page_count()
            )));
        }
        Ok(Self { inner })
    }

    #[must_use]
    pub fn page_count(&self) -> usize {
        self.inner.page_count()
    }

    /// Every page, in order, with its display size.
    ///
    /// # Errors
    ///
    /// Why a page's geometry could not be read.
    pub fn pages(&self) -> Result<Vec<Page>, Error> {
        (0..self.inner.page_count())
            .map(|page| {
                let size = self
                    .inner
                    .page_size(page)
                    .ok_or_else(|| Error::Unreadable(format!("page {} cannot be read", page + 1)))?;
                Ok(Page {
                    page: page + 1,
                    width: size.width,
                    height: size.height,
                })
            })
            .collect()
    }

    /// One page's text, in reading order.
    ///
    /// # Errors
    ///
    /// Why the page is not one the document has, or its text cannot be read.
    pub fn text(&self, page: usize) -> Result<Vec<Glyph>, Error> {
        if page == 0 || page > self.inner.page_count() {
            return Err(Error::NoPage(page));
        }
        let layer = self.inner.text(page - 1)?;
        let mut glyphs = Vec::new();
        for ch in layer.chars() {
            if ch.flags & tilo_pdf::text::char_flags::LINE_BREAK != 0 || ch.rect.is_empty() {
                continue;
            }
            if glyphs.len() >= MAX_PAGE_CHARS {
                break;
            }
            glyphs.push(Glyph {
                text: ch.ch.to_string(),
                line: ch.line,
                x: ch.rect.x0,
                y: ch.rect.y0,
                w: ch.rect.width(),
                h: ch.rect.height(),
                placed: ch.flags & tilo_pdf::text::char_flags::GENERATED != 0,
            });
        }
        Ok(glyphs)
    }

    /// One page's links.
    ///
    /// # Errors
    ///
    /// Why the page is not one the document has, or its links cannot be read.
    pub fn links(&self, page: usize) -> Result<Vec<Link>, Error> {
        if page == 0 || page > self.inner.page_count() {
            return Err(Error::NoPage(page));
        }
        Ok(self
            .inner
            .links(page - 1)?
            .into_iter()
            .take(MAX_PAGE_LINKS)
            .map(|link| Link {
                rect: Rect::of(link.rect),
                target: match link.destination {
                    Destination::Page { page, .. } => LinkTarget::Page(page + 1),
                    Destination::Uri(uri) => LinkTarget::Uri(uri),
                    Destination::File(path) => LinkTarget::File(path),
                },
            })
            .collect())
    }

    /// The document's contents, flattened, each entry with its depth.
    pub fn outline(&self) -> Vec<Outline> {
        let Ok(entries) = self.inner.outline() else {
            return Vec::new();
        };
        OutlineEntry::flatten(&entries)
            .into_iter()
            .filter(|(_, entry)| !entry.title.trim().is_empty())
            .take(MAX_OUTLINE)
            .map(|(level, entry)| Outline {
                title: entry.title.clone(),
                page: match entry.destination {
                    Some(Destination::Page { page, .. }) => page + 1,
                    _ => 0,
                },
                level,
            })
            .collect()
    }

    /// The document's form fields, each with its widget's page and area.
    ///
    /// # Errors
    ///
    /// Why the form cannot be read.
    pub fn fields(&self) -> Result<Vec<Field>, Error> {
        fields_of(&self.inner)
    }

    /// The markup the bytes already carry, and the form's filled values.
    ///
    /// # Errors
    ///
    /// Why a page's annotations cannot be read.
    pub fn markup(&self) -> Result<Existing, Error> {
        adopt::existing(&self.inner)
    }

    /// One page drawn as a PNG picture `width` pixels wide (its height follows the page's
    /// shape), the page's own turn applied, annotations of the file not drawn: a reader shows
    /// its own markup from its resources.
    ///
    /// # Errors
    ///
    /// Why the page cannot be drawn or encoded, or that it is not one the document has.
    pub fn picture(&self, page: usize, width: u32) -> Result<Vec<u8>, Error> {
        if page == 0 || page > self.inner.page_count() {
            return Err(Error::NoPage(page));
        }
        let width = width.clamp(64, MAX_PICTURE_SIDE);
        let size = self
            .inner
            .page_size(page - 1)
            .ok_or(Error::NoPage(page))?;
        let options = tilo_pdf::RenderOptions {
            scale: if size.width > 0.0 {
                width as f32 / size.width
            } else {
                1.0
            },
            annotations: true,
            reader_markup: false,
            ..tilo_pdf::RenderOptions::default()
        };
        let bitmap = self.inner.render(page - 1, &options)?;
        let image = tilo_image_codec::DecodedImage {
            rgba: bitmap.pixels().to_vec(),
            width: bitmap.width(),
            height: bitmap.height(),
        };
        tilo_image_codec::encode_now(&image, tilo_image_codec::Encoding::Png)
            .map_err(|error| Error::Unreadable(format!("the picture cannot be encoded ({error})")))
    }

    /// The page's own display size, in points.
    ///
    /// # Errors
    ///
    /// Why the page is not one the document has.
    pub fn page_size(&self, page: usize) -> Result<(f32, f32), Error> {
        let size = self
            .inner
            .page_size(page - 1)
            .ok_or(Error::NoPage(page))?;
        Ok((size.width, size.height))
    }
}

/// A document's form fields, each with its widget's page and area: the one mapping from the
/// engine's fields to a reader's, for the reader's own `fields` and for the fields a save takes
/// over with the rest of a file's markup.
///
/// # Errors
///
/// Why the form cannot be read.
pub(crate) fn fields_of(document: &tilo_pdf::Document) -> Result<Vec<Field>, Error> {
    let fields = document.form_fields()?;
    Ok(fields
        .into_iter()
        .map(|field| {
            let widget = field.widgets.first();
            let geometry = widget.and_then(|w| document.page(w.page));
            let rect = widget.and_then(|w| geometry.map(|g| Rect::of(g.rect_to_display(w.rect))));
            let kind = match field.kind {
                tilo_pdf::FieldKind::Text => FieldKind::Text,
                tilo_pdf::FieldKind::CheckBox => FieldKind::Checkbox,
                tilo_pdf::FieldKind::RadioButton => FieldKind::Radio,
                _ => FieldKind::Choice,
            };
            Field {
                name: field.name,
                kind,
                value: field.value,
                checked: field.widgets.iter().any(|w| w.checked),
                export: widget.map(|w| w.export_value.clone()).unwrap_or_default(),
                options: field.options,
                page: widget.map(|w| w.page + 1),
                rect,
                read_only: field.read_only,
                required: field.required,
            }
        })
        .collect())
}

/// What a file is, before it is imported: `pdf` or `image`, and the display size of each page
/// of it.
///
/// # Errors
///
/// Why the file is not one a document takes pages from.
pub fn import(bytes: &[u8]) -> Result<(&'static str, Vec<(f32, f32)>), Error> {
    if bytes.len() > MAX_IMPORT_BYTES {
        return Err(Error::TooLarge);
    }
    let engine = tilo_pdf::Engine::shared()?;
    let (kind, document) = if bytes.starts_with(b"%PDF-") {
        ("pdf", engine.open_bytes(bytes)?)
    } else {
        (
            "image",
            engine
                .open_image_bytes(bytes)
                .map_err(|error| Error::Unreadable(error.to_string()))?,
        )
    };
    let count = document.page_count();
    if count == 0 || count > MAX_IMPORT_PAGES {
        return Err(Error::Unreadable(format!(
            "it has {count} pages, and an import brings 1 to {MAX_IMPORT_PAGES}"
        )));
    }
    let sizes = (0..count)
        .map(|page| {
            document
                .page_size(page)
                .map(|size| (size.width, size.height))
                .ok_or_else(|| Error::Unreadable(format!("page {} cannot be read", page + 1)))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((kind, sizes))
}

/// The colour a mark says, as `#rrggbb`.
#[must_use]
pub fn colour_of(rgba: tilo_pdf::Rgba) -> String {
    format!("#{:02x}{:02x}{:02x}", rgba.r, rgba.g, rgba.b)
}

/// The red, green and blue a `#rrggbb` or `#rrggbbaa` colour says; black for one nothing reads.
#[must_use]
pub fn parse_colour(text: &str) -> tilo_pdf::Rgba {
    let hex = text.trim_start_matches('#');
    let byte = |at: usize| u8::from_str_radix(hex.get(at..at + 2).unwrap_or("00"), 16).unwrap_or(0);
    if hex.len() < 6 {
        return tilo_pdf::Rgba::new(0, 0, 0, 255);
    }
    let alpha = if hex.len() >= 8 { byte(6) } else { 255 };
    tilo_pdf::Rgba::new(byte(0), byte(2), byte(4), alpha)
}

#[cfg(test)]
mod tests;
