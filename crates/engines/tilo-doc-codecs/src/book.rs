//! A reflowable book read from an EPUB container: its metadata and its chapters' XHTML, in
//! reading order, with the table of contents naming each chapter.
//!
//! The container is resolved through [`bounded_zip::epub`] (the same `container.xml` → OPF
//! resolution the thumbnailer uses, under the shared ZIP-bomb bounds), and every read and count
//! is bounded here: a mistaken disk image or a hostile file fails quickly with a sentence a person
//! can act on, never by exhausting the device.
//!
//! The chapters are returned as the XHTML each spine document holds, not as a model: what a
//! reader makes of a chapter's markup is [`crate::rich::blocks_of_html`], so a book and a page on
//! the web are read by one importer.

use bounded_zip::epub::{self, strip_fragment};
use bounded_zip::{BoundedArchive, ZipLimits};

use crate::format::CodecError;

/// The largest EPUB read: a reader never holds an arbitrarily large file twice (the ZIP reader
/// owns a copy of the bytes), and this is generous for an illustrated book while making a wrong
/// file fail at once.
pub const MAX_EPUB_BYTES: usize = 128 * 1024 * 1024;
/// A chapter's XHTML.
const MAX_CHAPTER_BYTES: u64 = 4 * 1024 * 1024;
/// Everything the spine holds, so many individually valid chapters cannot add up to a device's
/// memory.
const MAX_TOTAL_SPINE_BYTES: usize = 64 * 1024 * 1024;
/// A cover image.
const MAX_COVER_BYTES: u64 = 12 * 1024 * 1024;
/// A book declares its reading order; this is more than any book a person reads.
const MAX_SPINE_ITEMS: usize = 1_024;

/// One chapter in reading order: what the contents call it, and the XHTML the spine document
/// holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chapter {
    pub title: String,
    pub xhtml: String,
}

/// A book as its package declares it, with each chapter's markup.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Book {
    pub title: String,
    pub author: String,
    pub language: String,
    pub subject: String,
    pub chapters: Vec<Chapter>,
    /// The cover image's bytes and media type, when the package declares one and it reads.
    pub cover: Option<(String, Vec<u8>)>,
}

/// Why a book could not be read, in words a reader can act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookProblem {
    /// Larger than [`MAX_EPUB_BYTES`].
    TooLarge,
    /// Not a ZIP archive, or no readable package inside it.
    Damaged,
    /// The package lists no chapters, or none of them hold text.
    NoChapters,
    /// A chapter the package lists is missing or unreadable.
    MissingChapter,
    /// More chapters or text than a reader holds in memory.
    TooLong,
}

impl std::fmt::Display for BookProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::TooLarge => "the book is larger than 128 MiB",
            Self::Damaged => "the file is damaged or is not an EPUB book",
            Self::NoChapters => "the book has no chapters to read",
            Self::MissingChapter => "part of the book is missing or damaged",
            Self::TooLong => "the book is too long to open",
        })
    }
}

impl From<BookProblem> for CodecError {
    fn from(problem: BookProblem) -> Self {
        Self::Unsupported(problem.to_string())
    }
}

/// Read the EPUB `bytes` as a book: its metadata, its cover and its chapters in reading order.
///
/// # Errors
///
/// [`BookProblem`] as the [`CodecError`]'s message: too large, damaged, without chapters, a
/// chapter missing, or too long.
pub fn read(bytes: &[u8]) -> Result<Book, CodecError> {
    if bytes.len() > MAX_EPUB_BYTES {
        return Err(BookProblem::TooLarge.into());
    }
    let mut archive = BoundedArchive::from_bytes(bytes, ZipLimits::default())
        .map_err(|_| BookProblem::Damaged)?;
    let opf_path = epub::find_opf_path(&mut archive).map_err(|_| BookProblem::Damaged)?;
    let pkg = epub::read_package(&mut archive, &opf_path).map_err(|_| BookProblem::Damaged)?;
    if pkg.spine.is_empty() {
        return Err(BookProblem::NoChapters.into());
    }
    if pkg.spine.len() > MAX_SPINE_ITEMS {
        return Err(BookProblem::TooLong.into());
    }
    let toc = epub::read_toc(&mut archive, &pkg);

    let mut chapters = Vec::with_capacity(pkg.spine.len());
    let mut total = 0usize;
    for href in &pkg.spine {
        let name = strip_fragment(href);
        // A spine is the book's declared order: leaving out a member (above all one a limit
        // refused) would make an opened book lie about what it holds.
        let xhtml = archive
            .read_text(name, MAX_CHAPTER_BYTES)
            .map_err(|_| BookProblem::MissingChapter)?;
        total = total.saturating_add(xhtml.len());
        if total > MAX_TOTAL_SPINE_BYTES {
            return Err(BookProblem::TooLong.into());
        }
        let title = toc
            .get(name)
            .cloned()
            .unwrap_or_else(|| format!("Chapter {}", chapters.len().saturating_add(1)));
        chapters.push(Chapter { title, xhtml });
    }
    if chapters.is_empty() {
        return Err(BookProblem::NoChapters.into());
    }

    let cover = pkg.cover_href.as_deref().and_then(|href| {
        let bytes = archive.read(href, MAX_COVER_BYTES).ok()?;
        let media = cover_media(href)?;
        Some((media.to_string(), bytes))
    });

    Ok(Book {
        title: if pkg.title.is_empty() {
            "Untitled".to_owned()
        } else {
            pkg.title
        },
        author: pkg.creator,
        language: if pkg.language.is_empty() {
            "en".to_owned()
        } else {
            pkg.language
        },
        subject: pkg.subject,
        chapters,
        cover,
    })
}

/// The media type of a cover image by its name, for the file a reader shows it as.
fn cover_media(href: &str) -> Option<&'static str> {
    let ext = href
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())?;
    Some(match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        _ => return None,
    })
}
