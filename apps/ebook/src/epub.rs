//! A real, self-contained EPUB container reader.
//!
//! An EPUB is a ZIP archive with a fixed shape:
//!
//! ```text
//! mimetype                     (stored, "application/epub+zip")
//! META-INF/container.xml       → points at the OPF package document
//! <base>/content.opf           → <metadata> + <manifest> + <spine>
//! <base>/*.xhtml               → the chapters, in spine order
//! <base>/toc.ncx | nav.xhtml   → the table of contents
//! ```
//!
//! We open the ZIP under the shared zip-bomb bounds, resolve `container.xml` →
//! OPF → metadata, spine and cover through [`bounded_zip::epub`] (the same
//! resolution the thumbnailer uses), then read each spine document and lower its
//! XHTML into [`Block`](crate::model::Block)s via [`crate::xhtml`]. The TOC (nav doc preferred, NCX
//! fallback, parsed with [`roxmltree`]) supplies chapter titles.

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use bounded_zip::epub::{self, OpfPackage, join, parent_dir, strip_fragment};
use bounded_zip::{BoundedArchive, ZipLimits};
use roxmltree::Document;

use crate::model::{Book, Chapter, Cover};
use crate::xhtml::parse_xhtml;

/// Longest edge (px) we downscale a decoded cover to. The shelf shows covers
/// no larger than ~232px tall at 2x, so 480 keeps them crisp while capping the
/// decoded footprint of a large source JPEG.
const COVER_MAX_EDGE: u32 = 480;

/// A reader should never try to hold an arbitrarily large file twice in
/// memory (the ZIP reader owns a copy of the supplied bytes).  This is a
/// deliberately generous ceiling for image-heavy, illustrated EPUBs while
/// still making the failure mode of a mistaken disk image or a hostile file
/// quick and predictable.
pub const MAX_EPUB_BYTES: usize = 128 * 1024 * 1024;

/// Bounds for decompressed ZIP members. ZIP metadata advertises an
/// uncompressed length, so check it before allocating a buffer rather than
/// trusting a compressed member to be small. The aggregate spine budget also
/// prevents many individually-valid XHTML files from exhausting memory.
/// (`container.xml` and the OPF are bounded by [`bounded_zip::epub`].)
const MAX_NAV_BYTES: usize = 2 * 1024 * 1024;
const MAX_CHAPTER_BYTES: usize = 4 * 1024 * 1024;
const MAX_TOTAL_SPINE_BYTES: usize = 64 * 1024 * 1024;
const MAX_COVER_BYTES: usize = 12 * 1024 * 1024;
const MAX_SPINE_ITEMS: usize = 1_024;
/// Cover art is eventually downscaled to 480px. These source limits leave
/// room for a crisp, unusually shaped cover while avoiding a decompression
/// bomb that allocates tens or hundreds of megabytes before downscaling.
const MAX_COVER_DIMENSION: u32 = 4_096;
const MAX_COVER_PIXELS: u64 = 4_000_000;

/// Why a book could not be opened, in words a reader can act on. It is the
/// outermost context of every [`parse_epub`] error, so the error's `Display`
/// is this sentence; the parser's own detail stays in the chain (`{:#}`) for
/// logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenProblem {
    /// Larger than [`MAX_EPUB_BYTES`].
    TooLarge,
    /// Not a ZIP archive, or no readable EPUB package inside it.
    Damaged,
    /// The package lists no chapters, or none of them hold text.
    NoChapters,
    /// A chapter the package lists is missing or unreadable.
    MissingChapter,
    /// More chapters or text than the reader holds in memory.
    TooLong,
}

impl std::fmt::Display for OpenProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::TooLarge => "The book is larger than 128 MiB.",
            Self::Damaged => "The file is damaged or isn’t an EPUB book.",
            Self::NoChapters => "The book has no chapters to read.",
            Self::MissingChapter => "Part of the book is missing or damaged.",
            Self::TooLong => "The book is too long to open.",
        })
    }
}

type Archive<'a> = BoundedArchive<Cursor<&'a [u8]>>;

/// A book's stable identity: the BLAKE3 content hash of its EPUB bytes,
/// prefixed `book:`. Deriving identity from the bytes (not the file path or
/// the publisher's optional `<identifier>`) is what lets reading progress,
/// highlights and comments follow the book across folders, devices, and
/// peers — the same content-addressing the Tilo filesystem uses.
pub fn content_id(bytes: &[u8]) -> String {
    // 128 bits of the digest is ample to avoid collision across a personal
    // library while keeping the id short enough to read in a CRDT key.
    let full = blake3::hash(bytes).to_hex();
    let hex = full.get(..32).unwrap_or(&full);
    format!("book:{hex}")
}

/// Parse an in-memory EPUB into a [`Book`]. `cover_tint`/`subject` are cosmetic
/// shelf hints supplied by the caller. The book's id is its content hash
/// ([`content_id`]) so annotations and progress travel with the file.
///
/// # Errors
///
/// Returns an error if the bytes are not a readable ZIP, the container/OPF
/// metadata cannot be located or parsed, or the spine resolves to no
/// readable chapters.
pub fn parse_epub(bytes: &[u8], cover_tint: usize, subject: &str) -> Result<Book> {
    if bytes.len() > MAX_EPUB_BYTES {
        return Err(anyhow!(
            "epub is too large ({}; maximum is {})",
            ui::format::size(bytes.len() as u64),
            ui::format::memory(MAX_EPUB_BYTES as u64)
        )
        .context(OpenProblem::TooLarge));
    }
    // `Cursor<&[u8]>` lets the archive seek through the caller's buffer
    // directly. A `Cursor<Vec<u8>>` would duplicate even an accepted 128 MiB
    // book before we had read a single chapter.
    let mut archive = BoundedArchive::from_bytes(bytes, ZipLimits::default())
        .context("open epub zip")
        .context(OpenProblem::Damaged)?;

    let opf_path = epub::find_opf_path(&mut archive).context(OpenProblem::Damaged)?;
    let base = parent_dir(&opf_path);

    let pkg = epub::read_package(&mut archive, &opf_path).context(OpenProblem::Damaged)?;
    if pkg.spine.is_empty() {
        return Err(anyhow!("OPF spine is empty").context(OpenProblem::NoChapters));
    }

    // Build href -> title map from the TOC (nav doc first, then NCX).
    let toc = read_toc(&mut archive, &pkg, &base);

    if pkg.spine.len() > MAX_SPINE_ITEMS {
        return Err(
            anyhow!("epub has too many spine documents (maximum {MAX_SPINE_ITEMS})")
                .context(OpenProblem::TooLong),
        );
    }
    let mut chapters = Vec::with_capacity(pkg.spine.len());
    let mut total_spine_bytes = 0usize;
    for href in &pkg.spine {
        let spine_href = strip_fragment(href);
        // A spine is the book's declared reading order. Silently omitting a
        // member (especially one rejected by a safety limit) would make a
        // successfully-opened book lie about its contents, so report the
        // precise chapter instead.
        let src = read_entry(&mut archive, spine_href, MAX_CHAPTER_BYTES)
            .with_context(|| format!("read spine document: {spine_href}"))
            .context(OpenProblem::MissingChapter)?;
        total_spine_bytes = total_spine_bytes.saturating_add(src.len());
        if total_spine_bytes > MAX_TOTAL_SPINE_BYTES {
            return Err(anyhow!(
                "epub text is too large (maximum {})",
                ui::format::memory(MAX_TOTAL_SPINE_BYTES as u64)
            )
            .context(OpenProblem::TooLong));
        }
        let blocks = parse_xhtml(&src);
        let title = toc
            .get(spine_href)
            .cloned()
            .or_else(|| first_heading(&blocks))
            .unwrap_or_else(|| format!("Chapter {}", chapters.len() + 1));
        chapters.push(Chapter { title, blocks });
    }

    if chapters.is_empty() {
        return Err(anyhow!("epub has no readable chapters").context(OpenProblem::NoChapters));
    }

    // Identity is the content hash of the file, never the path or the
    // publisher's optional `<identifier>`: the same bytes always resolve to the
    // same book, so its resources are shared across paths, devices and peers.
    let id = content_id(bytes);

    // Decode the embedded cover, if the manifest declared one. A missing or
    // undecodable cover is not an error — the shelf falls back to a spine.
    let cover = pkg
        .cover_href
        .as_deref()
        .and_then(|href| read_entry_bytes(&mut archive, href, MAX_COVER_BYTES).ok())
        .and_then(|bytes| decode_cover(&bytes));

    // Prefer the book's own subject metadata; the caller's hint is a fallback
    // for EPUBs that omit it.
    let subject = if pkg.subject.is_empty() {
        subject.to_string()
    } else {
        pkg.subject.clone()
    };

    Ok(Book {
        id,
        title: if pkg.title.is_empty() {
            "Untitled".into()
        } else {
            pkg.title
        },
        author: pkg.creator,
        language: if pkg.language.is_empty() {
            "en".into()
        } else {
            pkg.language
        },
        chapters,
        cover_tint,
        subject,
        cover,
    })
}

/// Decode raster cover bytes (JPEG/PNG) into downscaled RGBA. Returns `None`
/// on any decode failure so a broken cover never breaks the shelf.
fn decode_cover(bytes: &[u8]) -> Option<Cover> {
    // The shared preview decoder checks the declared dimensions against these
    // bounds before an image buffer is allocated.
    let limits = thumbnail::DecodeLimits {
        max_pixels: MAX_COVER_PIXELS,
        max_dimension: MAX_COVER_DIMENSION,
    };
    let cover = thumbnail::image_thumb_with(bytes, COVER_MAX_EDGE, limits)?;
    Some(Cover {
        rgba: Arc::new(cover.rgba),
        width: cover.width,
        height: cover.height,
    })
}

// ---------------------------------------------------------------------------
// Table of contents -> href(no fragment) -> label
// ---------------------------------------------------------------------------

fn read_toc(archive: &mut Archive<'_>, pkg: &OpfPackage, base: &str) -> HashMap<String, String> {
    if let Some(nav) = &pkg.nav_href
        && let Ok(src) = read_entry(archive, nav, MAX_NAV_BYTES)
    {
        let nav_base = parent_dir(nav);
        let map = parse_nav_doc(&src, &nav_base);
        if !map.is_empty() {
            return map;
        }
    }
    if let Some(ncx) = &pkg.ncx_href
        && let Ok(src) = read_entry(archive, ncx, MAX_NAV_BYTES)
    {
        let ncx_base = parent_dir(ncx);
        let map = parse_ncx(&src, &ncx_base);
        if !map.is_empty() {
            return map;
        }
    }
    let _ = base;
    HashMap::new()
}

/// EPUB3 nav: `<nav epub:type="toc"><ol><li><a href="..">Label</a>`.
fn parse_nav_doc(src: &str, base: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let sanitized = src.replace("&nbsp;", "&#160;");
    let Ok(doc) = Document::parse(&sanitized) else {
        return out;
    };
    // Find the toc <nav>, or any <nav> as a fallback.
    let nav = doc
        .descendants()
        .filter(|n| n.has_tag_name("nav"))
        .min_by_key(|n| {
            // prefer epub:type="toc"
            let is_toc = n
                .attributes()
                .any(|a| a.name() == "type" && a.value().contains("toc"));
            i32::from(!is_toc)
        });
    let scope = nav.unwrap_or_else(|| doc.root_element());
    for a in scope.descendants().filter(|n| n.has_tag_name("a")) {
        if let Some(href) = a.attribute("href") {
            let label = node_text(a);
            if !label.is_empty() {
                out.insert(strip_fragment(&join(base, href)).to_string(), label);
            }
        }
    }
    out
}

/// NCX: `<navPoint><navLabel><text>Label</text></navLabel><content src=".."/>`.
fn parse_ncx(src: &str, base: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Ok(doc) = Document::parse(src) else {
        return out;
    };
    for np in doc.descendants().filter(|n| n.has_tag_name("navPoint")) {
        let label = np
            .descendants()
            .find(|n| n.has_tag_name("text"))
            .and_then(|n| n.text())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let src_attr = np
            .descendants()
            .find(|n| n.has_tag_name("content"))
            .and_then(|n| n.attribute("src"));
        if let (false, Some(src_attr)) = (label.is_empty(), src_attr) {
            out.insert(strip_fragment(&join(base, src_attr)).to_string(), label);
        }
    }
    out
}

fn node_text(node: roxmltree::Node) -> String {
    let mut s = String::new();
    for d in node.descendants() {
        if d.is_text()
            && let Some(t) = d.text()
        {
            s.push_str(t);
        }
    }
    s.trim().to_string()
}

fn first_heading(blocks: &[crate::model::Block]) -> Option<String> {
    blocks.iter().find_map(|b| match b {
        crate::model::Block::Heading { spans, .. } => {
            let t: String = spans.iter().map(|s| s.text.as_str()).collect();
            if t.trim().is_empty() {
                None
            } else {
                Some(t.trim().to_string())
            }
        }
        _ => None,
    })
}

// ---------------------------------------------------------------------------
// ZIP + path helpers
// ---------------------------------------------------------------------------

/// Read a UTF-8 entry, bounded to `limit` bytes. XHTML chapters may carry a
/// doctype, so this does not apply the office-format DTD rejection.
fn read_entry(archive: &mut Archive<'_>, name: &str, limit: usize) -> Result<String> {
    Ok(archive.read_text(name, limit as u64)?)
}

/// Read a binary entry (cover images are not UTF-8).
fn read_entry_bytes(archive: &mut Archive<'_>, name: &str, limit: usize) -> Result<Vec<u8>> {
    Ok(archive.read(name, limit as u64)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// EPUB identity is the content hash; the sample parses; zip member limits and oversized covers are handled before decoding.
    #[test]
    fn epub_parsing_is_content_addressed_and_bounded() {
        // --- phase: identity_is_the_content_hash_not_the_path ---
        {
            // The same bytes always produce the same id — that is what lets a book's
            // highlights and reading progress follow it across folders, devices and
            // peers. Two different files produce two different ids.
            let bytes = include_bytes!(concat!(env!("OUT_DIR"), "/sample.epub"));
            let a = parse_epub(bytes, 0, "Fable").unwrap();
            let b = parse_epub(bytes, 3, "Something else").unwrap();
            assert_eq!(
                a.id, b.id,
                "identical bytes → identical id, regardless of caller hints"
            );
            assert!(a.id.starts_with("book:"), "ids are namespaced");
            assert_eq!(
                a.id,
                content_id(bytes),
                "the book id is exactly the content hash"
            );

            let mut tweaked = bytes.to_vec();
            *tweaked.last_mut().unwrap() ^= 0xFF; // one flipped byte = a different file
            assert_ne!(
                content_id(&tweaked),
                a.id,
                "different bytes → a different book"
            );
        }

        // --- phase: parses_the_bundled_sample ---
        {
            let bytes = include_bytes!(concat!(env!("OUT_DIR"), "/sample.epub"));
            let book = parse_epub(bytes, 0, "Fable").expect("sample.epub parses");
            assert_eq!(book.title, "The Lantern-Keeper's Almanac");
            assert_eq!(book.author, "Wren Halloway");
            assert_eq!(book.chapters.len(), 4);
            // TOC supplied real chapter titles.
            assert!(book.chapters[0].title.contains("Tide-House"));
            // First chapter has a heading + several paragraphs + a blockquote.
            assert!(
                book.chapters[0]
                    .blocks
                    .iter()
                    .any(|b| matches!(b, crate::model::Block::Quote { .. }))
            );
            assert!(book.chapters[0].blocks.len() >= 5);
        }

        // --- phase: zip_member_limit_is_enforced_before_reading ---
        {
            let mut archive =
                BoundedArchive::from_bytes(crate::content::SAMPLE_EPUB, ZipLimits::default())
                    .unwrap();
            let error = read_entry(&mut archive, "META-INF/container.xml", 1)
                .expect_err("the container is larger than one byte");
            assert!(
                error.to_string().contains("per-part limit"),
                "the limit is reported instead of allocating the member: {error:#}"
            );
        }

        // --- phase: oversized_cover_dimensions_fall_back_without_decoding_the_image ---
        {
            // This PNG is tiny on disk but declares a source image wider than a
            // shelf cover can ever need. The preflight dimensions pass rejects it
            // before `DynamicImage` can allocate its full RGBA buffer.
            let image = image::RgbImage::new(MAX_COVER_DIMENSION + 1, 1);
            let mut png = Cursor::new(Vec::new());
            image
                .write_to(&mut png, image::ImageFormat::Png)
                .expect("a small generated PNG encodes");
            assert!(decode_cover(&png.into_inner()).is_none());
        }
    }
}
