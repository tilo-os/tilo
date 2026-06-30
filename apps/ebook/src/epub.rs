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
//! We open the ZIP ([`zip`]), read `container.xml` to find the OPF, parse the
//! OPF ([`roxmltree`]) for metadata + the ordered spine, then read each spine
//! document and lower its XHTML into [`Block`]s via [`crate::xhtml`]. The TOC
//! (nav doc preferred, NCX fallback) supplies chapter titles.

use std::collections::HashMap;
use std::io::{Cursor, Read};

use anyhow::{anyhow, Context, Result};
use roxmltree::Document;
use zip::ZipArchive;

use crate::model::{Book, Chapter};
use crate::xhtml::parse_xhtml;

type Archive = ZipArchive<Cursor<Vec<u8>>>;

/// Parse an in-memory EPUB into a [`Book`]. `cover_tint`/`subject` are cosmetic
/// shelf hints supplied by the caller.
pub fn parse_epub(bytes: &[u8], cover_tint: usize, subject: &str) -> Result<Book> {
    let mut archive = ZipArchive::new(Cursor::new(bytes.to_vec()))
        .context("open epub zip")?;

    let opf_path = find_opf_path(&mut archive)?;
    let opf_src = read_entry(&mut archive, &opf_path)?;
    let base = parent_dir(&opf_path);

    let pkg = OpfPackage::parse(&opf_src, &base)?;

    // Build href -> title map from the TOC (nav doc first, then NCX).
    let toc = read_toc(&mut archive, &pkg, &base);

    let mut chapters = Vec::with_capacity(pkg.spine.len());
    for href in &pkg.spine {
        let bare = strip_fragment(href);
        let src = match read_entry(&mut archive, bare) {
            Ok(s) => s,
            Err(_) => continue, // skip an unreadable spine item rather than fail
        };
        let blocks = parse_xhtml(&src);
        let title = toc
            .get(bare)
            .cloned()
            .or_else(|| first_heading(&blocks))
            .unwrap_or_else(|| format!("Chapter {}", chapters.len() + 1));
        chapters.push(Chapter { title, href: bare.to_string(), blocks });
    }

    if chapters.is_empty() {
        return Err(anyhow!("epub has no readable chapters"));
    }

    let id = if pkg.identifier.is_empty() {
        format!("book:{}", slug(&pkg.title))
    } else {
        pkg.identifier.clone()
    };

    Ok(Book {
        id,
        title: if pkg.title.is_empty() { "Untitled".into() } else { pkg.title },
        author: pkg.creator,
        publisher: pkg.publisher,
        description: pkg.description,
        chapters,
        cover_tint,
        subject: subject.to_string(),
    })
}

// ---------------------------------------------------------------------------
// container.xml -> OPF path
// ---------------------------------------------------------------------------

fn find_opf_path(archive: &mut Archive) -> Result<String> {
    let xml = read_entry(archive, "META-INF/container.xml")
        .context("read META-INF/container.xml")?;
    let doc = Document::parse(&xml).context("parse container.xml")?;
    let rootfile = doc
        .descendants()
        .find(|n| n.has_tag_name("rootfile"))
        .ok_or_else(|| anyhow!("no <rootfile> in container.xml"))?;
    let path = rootfile
        .attribute("full-path")
        .ok_or_else(|| anyhow!("<rootfile> missing full-path"))?;
    Ok(normalize(path))
}

// ---------------------------------------------------------------------------
// OPF package document
// ---------------------------------------------------------------------------

struct OpfPackage {
    title: String,
    creator: String,
    publisher: String,
    description: String,
    identifier: String,
    /// Spine hrefs in reading order (resolved relative to the OPF base).
    spine: Vec<String>,
    /// manifest id -> resolved href.
    manifest: HashMap<String, String>,
    /// href of the EPUB3 nav document, if declared (properties="nav").
    nav_href: Option<String>,
    /// href of the NCX, if any (spine toc= idref or manifest media-type).
    ncx_href: Option<String>,
}

impl OpfPackage {
    fn parse(src: &str, base: &str) -> Result<Self> {
        let doc = Document::parse(src).context("parse OPF")?;

        let meta_text = |tag: &str| -> String {
            doc.descendants()
                .find(|n| n.is_element() && n.tag_name().name().eq_ignore_ascii_case(tag))
                .and_then(|n| n.text())
                .map(|s| s.trim().to_string())
                .unwrap_or_default()
        };

        let title = meta_text("title");
        let creator = meta_text("creator");
        let publisher = meta_text("publisher");
        let description = meta_text("description");
        let identifier = meta_text("identifier");

        // Manifest: id -> (href, properties, media-type)
        let mut manifest = HashMap::new();
        let mut nav_href = None;
        let mut ncx_by_id: HashMap<String, String> = HashMap::new();
        if let Some(man) = doc
            .descendants()
            .find(|n| n.has_tag_name("manifest"))
        {
            for item in man.children().filter(|n| n.has_tag_name("item")) {
                let id = item.attribute("id").unwrap_or_default().to_string();
                let href = item.attribute("href").unwrap_or_default();
                if id.is_empty() || href.is_empty() {
                    continue;
                }
                let resolved = join(base, href);
                let props = item.attribute("properties").unwrap_or_default();
                let media = item.attribute("media-type").unwrap_or_default();
                if props.split_whitespace().any(|p| p == "nav") {
                    nav_href = Some(resolved.clone());
                }
                if media == "application/x-dtbncx+xml" {
                    ncx_by_id.insert(id.clone(), resolved.clone());
                }
                manifest.insert(id, resolved);
            }
        }

        // Spine: ordered idrefs -> hrefs; `toc` attr names the NCX item.
        let mut spine = Vec::new();
        let mut ncx_href = None;
        if let Some(sp) = doc.descendants().find(|n| n.has_tag_name("spine")) {
            if let Some(toc_id) = sp.attribute("toc") {
                ncx_href = ncx_by_id
                    .get(toc_id)
                    .cloned()
                    .or_else(|| manifest.get(toc_id).cloned());
            }
            for itemref in sp.children().filter(|n| n.has_tag_name("itemref")) {
                if let Some(idref) = itemref.attribute("idref") {
                    if let Some(href) = manifest.get(idref) {
                        spine.push(href.clone());
                    }
                }
            }
        }
        if ncx_href.is_none() {
            ncx_href = ncx_by_id.into_values().next();
        }

        if spine.is_empty() {
            return Err(anyhow!("OPF spine is empty"));
        }

        Ok(Self {
            title,
            creator,
            publisher,
            description,
            identifier,
            spine,
            manifest,
            nav_href,
            ncx_href,
        })
    }
}

// ---------------------------------------------------------------------------
// Table of contents -> href(no fragment) -> label
// ---------------------------------------------------------------------------

fn read_toc(archive: &mut Archive, pkg: &OpfPackage, base: &str) -> HashMap<String, String> {
    if let Some(nav) = &pkg.nav_href {
        if let Ok(src) = read_entry(archive, nav) {
            let nav_base = parent_dir(nav);
            let map = parse_nav_doc(&src, &nav_base);
            if !map.is_empty() {
                return map;
            }
        }
    }
    if let Some(ncx) = &pkg.ncx_href {
        if let Ok(src) = read_entry(archive, ncx) {
            let ncx_base = parent_dir(ncx);
            let map = parse_ncx(&src, &ncx_base);
            if !map.is_empty() {
                return map;
            }
        }
    }
    let _ = base;
    HashMap::new()
}

/// EPUB3 nav: `<nav epub:type="toc"><ol><li><a href="..">Label</a>`.
fn parse_nav_doc(src: &str, base: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let sanitized = src.replace("&nbsp;", "&#160;");
    let Ok(doc) = Document::parse(&sanitized) else { return out };
    // Find the toc <nav>, or any <nav> as a fallback.
    let nav = doc
        .descendants()
        .filter(|n| n.has_tag_name("nav"))
        .min_by_key(|n| {
            // prefer epub:type="toc"
            let is_toc = n
                .attributes()
                .any(|a| a.name() == "type" && a.value().contains("toc"));
            if is_toc { 0 } else { 1 }
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
    let Ok(doc) = Document::parse(src) else { return out };
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
        if d.is_text() {
            if let Some(t) = d.text() {
                s.push_str(t);
            }
        }
    }
    s.trim().to_string()
}

fn first_heading(blocks: &[crate::model::Block]) -> Option<String> {
    blocks.iter().find_map(|b| match b {
        crate::model::Block::Heading { spans, .. } => {
            let t: String = spans.iter().map(|s| s.text.as_str()).collect();
            if t.trim().is_empty() { None } else { Some(t.trim().to_string()) }
        }
        _ => None,
    })
}

// ---------------------------------------------------------------------------
// ZIP + path helpers
// ---------------------------------------------------------------------------

fn read_entry(archive: &mut Archive, name: &str) -> Result<String> {
    let mut file = archive
        .by_name(name)
        .with_context(|| format!("zip entry not found: {name}"))?;
    let mut buf = String::new();
    file.read_to_string(&mut buf)
        .with_context(|| format!("read zip entry: {name}"))?;
    Ok(buf)
}

fn strip_fragment(href: &str) -> &str {
    href.split('#').next().unwrap_or(href)
}

fn parent_dir(path: &str) -> String {
    match path.rfind('/') {
        Some(i) => path[..i].to_string(),
        None => String::new(),
    }
}

/// Join a relative href onto a base directory and normalise.
fn join(base: &str, href: &str) -> String {
    if href.starts_with('/') {
        return normalize(&href[1..]);
    }
    if base.is_empty() {
        return normalize(href);
    }
    normalize(&format!("{base}/{href}"))
}

/// Collapse `.` / `..` segments and `./` prefixes; ZIP names use `/`.
fn normalize(path: &str) -> String {
    let mut stack: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            s => stack.push(s),
        }
    }
    stack.join("/")
}

fn slug(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_resolves_dot_segments() {
        assert_eq!(normalize("OEBPS/./ch1.xhtml"), "OEBPS/ch1.xhtml");
        assert_eq!(normalize("OEBPS/text/../ch1.xhtml"), "OEBPS/ch1.xhtml");
    }

    #[test]
    fn join_resolves_relative() {
        assert_eq!(join("OEBPS", "ch1.xhtml"), "OEBPS/ch1.xhtml");
        assert_eq!(join("OEBPS/text", "../images/c.png"), "OEBPS/images/c.png");
        assert_eq!(join("", "ch1.xhtml"), "ch1.xhtml");
    }

    #[test]
    fn strip_fragment_works() {
        assert_eq!(strip_fragment("ch1.xhtml#sec2"), "ch1.xhtml");
        assert_eq!(strip_fragment("ch1.xhtml"), "ch1.xhtml");
    }

    #[test]
    fn parses_the_bundled_sample() {
        let bytes = include_bytes!("../assets/sample.epub");
        let book = parse_epub(bytes, 0, "Fable").expect("sample.epub parses");
        assert_eq!(book.title, "The Lantern-Keeper's Almanac");
        assert_eq!(book.author, "Wren Halloway");
        assert_eq!(book.chapters.len(), 4);
        // TOC supplied real chapter titles.
        assert!(book.chapters[0].title.contains("Tide-House"));
        // First chapter has a heading + several paragraphs + a blockquote.
        assert!(book.chapters[0]
            .blocks
            .iter()
            .any(|b| matches!(b, crate::model::Block::Quote { .. })));
        assert!(book.chapters[0].blocks.len() >= 5);
    }
}
