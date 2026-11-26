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

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    use super::*;

    fn archive(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut zw = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data) in entries {
            zw.start_file(*name, SimpleFileOptions::default()).unwrap();
            zw.write_all(data.as_bytes()).unwrap();
        }
        zw.finish().unwrap().into_inner()
    }

    const CONTAINER: &str = r#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#;

    fn package(spine: &str) -> String {
        format!(
            r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>The Tide Glass</dc:title>
    <dc:creator>Ada Ferrer</dc:creator>
    <dc:language>en</dc:language>
    <dc:subject>Fable</dc:subject>
  </metadata>
  <manifest>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
    <item id="c1" href="c1.xhtml" media-type="application/xhtml+xml"/>
    <item id="c2" href="c2.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine>{spine}</spine>
</package>"#
        )
    }

    const NAV: &str = r#"<?xml version="1.0"?>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops">
<body><nav epub:type="toc"><ol>
  <li><a href="c1.xhtml">The Glass</a></li>
  <li><a href="c2.xhtml">The Tide</a></li>
</ol></nav></body></html>"#;

    #[test]
    fn a_book_reads_its_facts_and_chapters_in_spine_order() {
        let bytes = archive(&[
            ("META-INF/container.xml", CONTAINER),
            (
                "OEBPS/content.opf",
                &package(r#"<itemref idref="c1"/><itemref idref="c2"/>"#),
            ),
            ("OEBPS/nav.xhtml", NAV),
            (
                "OEBPS/c1.xhtml",
                "<html><body><h1>The Glass</h1><p>One.</p></body></html>",
            ),
            (
                "OEBPS/c2.xhtml",
                "<html><body><h1>The Tide</h1><p>Two.</p></body></html>",
            ),
        ]);
        let book = read(&bytes).unwrap();
        assert_eq!(book.title, "The Tide Glass");
        assert_eq!(book.author, "Ada Ferrer");
        assert_eq!(book.subject, "Fable");
        assert_eq!(book.language, "en");
        assert_eq!(book.chapters.len(), 2);
        assert_eq!(book.chapters[0].title, "The Glass");
        assert!(book.chapters[0].xhtml.contains("One."));
        assert_eq!(book.chapters[1].title, "The Tide");
    }

    #[test]
    fn a_spine_in_the_package_order_is_the_reading_order_it_names() {
        let bytes = archive(&[
            ("META-INF/container.xml", CONTAINER),
            (
                "OEBPS/content.opf",
                &package(r#"<itemref idref="c2"/><itemref idref="c1"/>"#),
            ),
            ("OEBPS/nav.xhtml", NAV),
            ("OEBPS/c1.xhtml", "<html><body><p>One.</p></body></html>"),
            ("OEBPS/c2.xhtml", "<html><body><p>Two.</p></body></html>"),
        ]);
        let book = read(&bytes).unwrap();
        assert_eq!(book.chapters[0].title, "The Tide");
        assert!(book.chapters[0].xhtml.contains("Two."));
    }

    #[test]
    fn a_missing_chapter_fails_rather_than_opening_a_short_book() {
        let bytes = archive(&[
            ("META-INF/container.xml", CONTAINER),
            (
                "OEBPS/content.opf",
                &package(r#"<itemref idref="c1"/><itemref idref="c2"/>"#),
            ),
            ("OEBPS/c1.xhtml", "<html><body><p>One.</p></body></html>"),
        ]);
        assert_eq!(
            read(&bytes).unwrap_err().to_string(),
            CodecError::Unsupported("part of the book is missing or damaged".into()).to_string()
        );
    }

    #[test]
    fn a_package_without_a_spine_is_not_a_book() {
        let bytes = archive(&[
            ("META-INF/container.xml", CONTAINER),
            ("OEBPS/content.opf", &package("")),
        ]);
        assert!(read(&bytes).is_err());
    }

    #[test]
    fn bytes_that_are_not_a_zip_are_damaged() {
        assert_eq!(
            read(b"not a zip").unwrap_err().to_string(),
            CodecError::Unsupported(BookProblem::Damaged.to_string()).to_string()
        );
    }

    #[test]
    fn a_cover_is_read_when_the_package_declares_one() {
        let png = [0x89u8, b'P', b'N', b'G'];
        let mut zw = ZipWriter::new(Cursor::new(Vec::new()));
        zw.start_file("META-INF/container.xml", SimpleFileOptions::default())
            .unwrap();
        zw.write_all(CONTAINER.as_bytes()).unwrap();
        let package = package(r#"<itemref idref="c1"/>"#).replace(
            "</manifest>",
            r#"<item id="x" href="cover.png" media-type="image/png" properties="cover-image"/></manifest>"#,
        );
        zw.start_file("OEBPS/content.opf", SimpleFileOptions::default())
            .unwrap();
        zw.write_all(package.as_bytes()).unwrap();
        zw.start_file("OEBPS/c1.xhtml", SimpleFileOptions::default())
            .unwrap();
        zw.write_all(b"<html><body><p>One.</p></body></html>")
            .unwrap();
        zw.start_file("OEBPS/cover.png", SimpleFileOptions::default())
            .unwrap();
        zw.write_all(&png).unwrap();
        let bytes = zw.finish().unwrap().into_inner();
        let book = read(&bytes).unwrap();
        assert_eq!(book.cover, Some(("image/png".to_owned(), png.to_vec())));
        assert_eq!(cover_media("a/b/Cover.JPEG"), Some("image/jpeg"));
    }
}
