//! `qdoc-epub` — export a [`qdoc::DocSnapshot`] as a valid **EPUB 3** e-book.
//!
//! An EPUB is an [OCF] ZIP container holding XHTML content documents plus
//! packaging metadata. [`render_epub`] assembles: `mimetype` (the literal
//! `application/epub+zip`, **stored uncompressed and first** — the one
//! hard structural rule OCF imposes on entry order/compression, so a
//! reader can sniff the type without inflating anything);
//! `META-INF/container.xml` (points at the OPF); `OEBPS/content.opf`
//! (Dublin-Core metadata, a manifest of every resource, a reading-order
//! spine); `OEBPS/nav.xhtml` (the EPUB 3 nav/TOC document, derived from
//! heading blocks nested by level); `OEBPS/chapter-N.xhtml` (the body,
//! split into one chapter per top-level `H1`; a document with no `H1` is
//! a single chapter); `OEBPS/images/img-N.<ext>` (image blobs
//! referenced by `image` blocks).
//!
//! ## XHTML well-formedness
//!
//! EPUB content documents must be **XML-parseable XHTML**, not HTML5.
//! We reuse [`qdoc-html`] for inline/block markup, wrap each fragment in
//! an XHTML scaffold, rewrite HTML5 void elements (`<br>`, `<img …>`,
//! `<hr>`) into self-closing XML form, and re-point images from
//! `qdoc-html`'s inline `data:` URIs to packaged file resources.
//!
//! [OCF]: https://www.w3.org/publishing/epub3/epub-ocf.html

use qdoc::snapshot::AttrVal;
use qdoc::{BlockView, DocSnapshot};
use std::fmt;
use std::io::{Cursor, Write};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

/// Error produced while assembling an EPUB.
#[derive(Debug)]
pub enum DocEpubError {
    /// A ZIP-level write failure (entry start or byte write).
    Zip(String),
    /// An I/O failure writing archive bytes.
    Io(String),
}

impl fmt::Display for DocEpubError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DocEpubError::Zip(e) => write!(f, "epub zip error: {e}"),
            DocEpubError::Io(e) => write!(f, "epub io error: {e}"),
        }
    }
}

impl std::error::Error for DocEpubError {}

/// The OCF `mimetype` payload — fixed by the EPUB spec.
const MIMETYPE: &str = "application/epub+zip";

/// Render a materialized qdoc snapshot to the bytes of a `.epub` file:
/// `mimetype` first/stored, a container pointing at the OPF, an OPF
/// carrying metadata plus manifest and spine, a nav document with a TOC
/// entry per heading, the body chapters, and any embedded images.
/// Always succeeds — degenerate inputs still produce a valid book.
pub fn render_epub(snap: &DocSnapshot) -> Result<Vec<u8>, DocEpubError> {
    // 1. Collect image blobs into packaged resources, mapping blob-hash → item.
    let images = collect_images(snap);

    // 2. Split the body into chapters (one per top-level H1).
    let chapters = build_chapters(snap, &images);

    // 3. Build the nav (TOC) document from headings.
    let nav = build_nav(snap, &chapters);

    // 4. Build the OPF package document.
    let opf = build_opf(snap, &chapters, &images);

    // 5. Assemble the OCF ZIP.
    let mut zw = ZipWriter::new(Cursor::new(Vec::new()));
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    let zerr = |e: zip::result::ZipError| DocEpubError::Zip(e.to_string());
    let ierr = |e: std::io::Error| DocEpubError::Io(e.to_string());

    // The mimetype MUST be the first entry AND stored uncompressed.
    zw.start_file("mimetype", stored).map_err(zerr)?;
    zw.write_all(MIMETYPE.as_bytes()).map_err(ierr)?;

    zw.start_file("META-INF/container.xml", deflated)
        .map_err(zerr)?;
    zw.write_all(CONTAINER_XML.as_bytes()).map_err(ierr)?;

    zw.start_file("OEBPS/content.opf", deflated).map_err(zerr)?;
    zw.write_all(opf.as_bytes()).map_err(ierr)?;

    zw.start_file("OEBPS/nav.xhtml", deflated).map_err(zerr)?;
    zw.write_all(nav.as_bytes()).map_err(ierr)?;

    for ch in &chapters {
        zw.start_file(format!("OEBPS/{}", ch.href), deflated)
            .map_err(zerr)?;
        zw.write_all(ch.xhtml.as_bytes()).map_err(ierr)?;
    }

    for img in &images {
        // Image bytes are already compressed formats (PNG/JPEG/…); store
        // them to avoid deflating incompressible data.
        zw.start_file(format!("OEBPS/{}", img.href), stored)
            .map_err(zerr)?;
        zw.write_all(&img.bytes).map_err(ierr)?;
    }

    let cur = zw.finish().map_err(zerr)?;
    Ok(cur.into_inner())
}

// ---- container.xml ----------------------------------------------------------

const CONTAINER_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>
"#;

// ---- images -----------------------------------------------------------------

/// A packaged image resource derived from a blob in `snap.blobs`.
struct ImageItem {
    /// Manifest item id (e.g. `img-1`).
    id: String,
    /// Href relative to the OPF (e.g. `images/img-1.png`).
    href: String,
    /// MIME / media-type for the manifest (`image/png`, …).
    media_type: &'static str,
    /// The raw bytes.
    bytes: Vec<u8>,
    /// The blob hash key this resource was built from.
    blob_hash: String,
}

/// Walk every block, collecting the (in-order, de-duplicated) set of
/// image blobs actually referenced. Blocks whose blob is missing from
/// `snap.blobs` are skipped (they render as empty `<img/>`).
fn collect_images(snap: &DocSnapshot) -> Vec<ImageItem> {
    let mut items: Vec<ImageItem> = Vec::new();
    collect_images_in(&snap.blocks, snap, &mut items);
    items
}

fn collect_images_in(blocks: &[BlockView], snap: &DocSnapshot, items: &mut Vec<ImageItem>) {
    for b in blocks {
        if let Some(item) = image_item_for(b, snap, items.len()) {
            // Skip blobs already packaged (reused images are embedded once);
            // package the rest with a sniffed media-type.
            if !items.iter().any(|it| it.blob_hash == item.blob_hash) {
                items.push(item);
            }
        }
        if !b.children.is_empty() {
            collect_images_in(&b.children, snap, items);
        }
    }
}

/// Build a packaged [`ImageItem`] for an `image` block whose blob is
/// present, using `existing_count` to number the resource. `None` for
/// non-image blocks or missing blobs.
fn image_item_for(b: &BlockView, snap: &DocSnapshot, existing_count: usize) -> Option<ImageItem> {
    if b.kind != "image" {
        return None;
    }
    let AttrVal::Str(hash) = b.attrs.get("blob")? else {
        return None;
    };
    let bytes = snap.blobs.get(hash)?;
    let (media_type, ext) = image_media_type(bytes);
    let n = existing_count + 1;
    Some(ImageItem {
        id: format!("img-{n}"),
        href: format!("images/img-{n}.{ext}"),
        media_type,
        bytes: bytes.clone(),
        blob_hash: hash.clone(),
    })
}

/// Sniff image magic numbers → `(media-type, extension)`. Mirrors
/// `qdoc-html`'s sniff but also yields the packaged extension; unknown
/// bytes default to PNG.
fn image_media_type(bytes: &[u8]) -> (&'static str, &'static str) {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        ("image/png", "png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        ("image/jpeg", "jpg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        ("image/gif", "gif")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        ("image/webp", "webp")
    } else if bytes.starts_with(b"<svg") || bytes.starts_with(b"<?xml") {
        ("image/svg+xml", "svg")
    } else {
        ("image/png", "png")
    }
}

// ---- chapters ---------------------------------------------------------------

/// One spine chapter: manifest id, href, the headings it owns (for nav
/// TOC), and the rendered XHTML. The display title is folded into the
/// chapter's `<title>` at render time.
struct Chapter {
    id: String,
    href: String,
    /// Headings in this chapter with their anchor fragments, for a nested
    /// TOC. The chapter's lead H1 (if any) is the first entry.
    headings: Vec<Heading>,
    xhtml: String,
}

/// A heading recovered from the body, with an anchor id assigned for the nav.
struct Heading {
    level: u8,
    text: String,
    /// Fragment id to anchor to within the chapter document.
    frag: String,
}

/// Split the document's top-level blocks into chapters at each
/// top-level `H1`; everything before the first forms a lead chapter.
/// Never empty (no blocks → one empty chapter).
fn build_chapters(snap: &DocSnapshot, images: &[ImageItem]) -> Vec<Chapter> {
    // Group blocks into runs each beginning at a top-level H1; blocks
    // before the first H1 form the lead group.
    let mut groups: Vec<Vec<&BlockView>> = Vec::new();
    let mut current: Vec<&BlockView> = Vec::new();

    for b in &snap.blocks {
        if is_h1(b) && !current.is_empty() {
            groups.push(std::mem::take(&mut current));
        }
        current.push(b);
    }
    if !current.is_empty() {
        groups.push(current);
    }
    if groups.is_empty() {
        groups.push(Vec::new());
    }

    let mut chapters = Vec::with_capacity(groups.len());
    for (gi, blocks) in groups.into_iter().enumerate() {
        let n = gi + 1;
        let id = format!("chapter-{n}");
        let href = format!("{id}.xhtml");

        // Assign per-chapter heading anchors and collect them for the nav.
        let mut headings = Vec::new();
        let mut anchored: Vec<(String, &BlockView)> = Vec::new();
        let mut hcount = 0usize;
        for b in &blocks {
            if b.kind == "heading" {
                hcount += 1;
                let frag = format!("h{hcount}");
                headings.push(Heading {
                    level: heading_level(b),
                    text: b.text.clone(),
                    frag: frag.clone(),
                });
                anchored.push((frag, b));
            } else {
                anchored.push((String::new(), b));
            }
        }

        // Chapter <title>: the first heading's text if any, else a
        // fallback — used only inside <head>; the nav drives navigation.
        let title = headings
            .first()
            .map(|h| h.text.clone())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| {
                if gi == 0 {
                    "Start".to_string()
                } else {
                    format!("Chapter {n}")
                }
            });

        let xhtml = render_chapter_xhtml(snap, &anchored, &title, images);
        chapters.push(Chapter {
            id,
            href,
            headings,
            xhtml,
        });
    }
    chapters
}

fn is_h1(b: &BlockView) -> bool {
    b.kind == "heading" && heading_level(b) == 1
}

fn heading_level(b: &BlockView) -> u8 {
    match b.attrs.get("level") {
        Some(AttrVal::Int(n)) => (*n).clamp(1, 6) as u8,
        _ => 1,
    }
}

/// Render one chapter as a complete XHTML document. Block markup comes
/// from `qdoc-html` (fragment mode); we then anchor headings, fix void
/// elements, and re-point image `data:` URIs to packaged hrefs.
fn render_chapter_xhtml(
    snap: &DocSnapshot,
    anchored: &[(String, &BlockView)],
    title: &str,
    images: &[ImageItem],
) -> String {
    let mut body = String::new();
    for (frag, block) in anchored {
        // Render this block via qdoc-html by handing it a one-block
        // snapshot sharing blobs/styles, since its fragment renderer takes
        // whole snapshots.
        let mut sub = empty_like(snap);
        sub.blocks = vec![(*block).clone()];
        let opts = qdoc_html::HtmlOptions {
            fragment: true,
            title: None,
            indent: "  ".to_string(),
        };
        let mut frag_html = qdoc_html::render_html(&sub, opts);

        // Re-point image data: URIs to packaged file resources.
        frag_html = repoint_images(&frag_html, block, images);

        // Make the HTML5 fragment XML-well-formed (void elements self-close).
        frag_html = xhtmlify(&frag_html);

        if !frag.is_empty() {
            // Anchor the heading so the nav can link to it — inject an id on
            // the fragment's first start-tag.
            frag_html = inject_id(&frag_html, frag);
        }
        body.push_str(&frag_html);
    }

    wrap_xhtml(title, &body)
}

/// A blank snapshot sharing the source's blobs + styles, so a
/// sub-render of one block resolves styles/images identically.
fn empty_like(snap: &DocSnapshot) -> DocSnapshot {
    DocSnapshot {
        schema: snap.schema.clone(),
        schema_version: snap.schema_version,
        blobs: snap.blobs.clone(),
        styles: snap.styles.clone(),
        ..Default::default()
    }
}

/// Replace the inline `data:` URI `qdoc-html` emits for an image block
/// with the packaged file href (relative to the chapter doc, beside
/// `images/` under OEBPS). Non-image fragments pass through unchanged.
fn repoint_images(frag: &str, block: &BlockView, images: &[ImageItem]) -> String {
    if block.kind != "image" {
        return frag.to_string();
    }
    let Some(AttrVal::Str(hash)) = block.attrs.get("blob") else {
        return frag.to_string();
    };
    let Some(item) = images.iter().find(|it| &it.blob_hash == hash) else {
        return frag.to_string();
    };
    // qdoc-html emits `src="data:<mime>;base64,<...>"`; locate `src="`
    // and replace up to the closing quote.
    if let Some(start) = frag.find("src=\"") {
        let val_start = start + 5;
        if let Some(rel_end) = frag[val_start..].find('"') {
            let end = val_start + rel_end;
            let mut out = String::with_capacity(frag.len());
            out.push_str(&frag[..val_start]);
            out.push_str(&item.href);
            out.push_str(&frag[end..]);
            return out;
        }
    }
    frag.to_string()
}

/// Inject an `id="<frag>"` attribute into a fragment's first start-tag,
/// to anchor headings for nav links.
fn inject_id(frag: &str, id: &str) -> String {
    if let Some(lt) = frag.find('<') {
        // Find the end of the tag name (first space or `>` or `/`).
        let after = &frag[lt + 1..];
        let name_end = after
            .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
            .map_or(frag.len(), |p| lt + 1 + p);
        let mut out = String::with_capacity(frag.len() + id.len() + 8);
        out.push_str(&frag[..name_end]);
        out.push_str(&format!(" id=\"{}\"", escape_xml_attr(id)));
        out.push_str(&frag[name_end..]);
        return out;
    }
    frag.to_string()
}

/// Rewrite HTML5 void elements into XML self-closing form for
/// well-formed XHTML — `<br>`, `<img …>`, `<hr>`. Already-self-closed
/// tags are left alone.
fn xhtmlify(html: &str) -> String {
    let mut out = String::with_capacity(html.len() + 16);
    let bytes = html.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            // Find the matching '>'.
            if let Some(rel) = html[i..].find('>') {
                let end = i + rel; // index of '>'
                let tag = &html[i..=end]; // includes '<' and '>'
                let rewritten = self_close_void(tag);
                out.push_str(&rewritten);
                i = end + 1;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// If `tag` is a non-self-closed HTML5 void element, return its
/// self-closing form; otherwise unchanged.
fn self_close_void(tag: &str) -> String {
    // tag looks like "<name ...>" or "</name>" or "<name/>".
    if tag.starts_with("</") || tag.ends_with("/>") {
        return tag.to_string();
    }
    let inner = &tag[1..tag.len() - 1]; // strip '<' and '>'
    let name: String = inner
        .chars()
        .take_while(|c| !c.is_whitespace() && *c != '/')
        .collect::<String>()
        .to_ascii_lowercase();
    const VOID: &[&str] = &[
        "br", "img", "hr", "area", "base", "col", "embed", "input", "link", "meta", "param",
        "source", "track", "wbr",
    ];
    if VOID.contains(&name.as_str()) {
        let trimmed = inner.trim_end();
        format!("<{trimmed}/>")
    } else {
        tag.to_string()
    }
}

/// Wrap body markup in a complete XHTML 1.1 / EPUB-3 content document.
fn wrap_xhtml(title: &str, body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" xml:lang="en" lang="en">
<head>
<meta charset="utf-8"/>
<title>{}</title>
</head>
<body>
{}</body>
</html>
"#,
        escape_xml_text(title),
        body
    )
}

// ---- nav (TOC) --------------------------------------------------------------

/// Build the EPUB 3 nav document: an `<nav epub:type="toc">` whose
/// nested `<ol>` mirrors heading levels across chapters, each linking to
/// its anchor in the owning chapter.
fn build_nav(snap: &DocSnapshot, chapters: &[Chapter]) -> String {
    let title = doc_title(snap);

    // Flatten all headings (with their chapter href) into a level/label/link list.
    let mut entries: Vec<(u8, String, String)> = Vec::new(); // (level, label, href#frag)
    for ch in chapters {
        for h in &ch.headings {
            let link = format!("{}#{}", ch.href, h.frag);
            entries.push((h.level, h.text.clone(), link));
        }
    }

    let mut list = String::new();
    if entries.is_empty() {
        // A nav must contain at least one toc target; link to the first chapter.
        let href = chapters
            .first()
            .map_or("chapter-1.xhtml", |c| c.href.as_str());
        list.push_str("  <ol>\n");
        list.push_str(&format!(
            "    <li><a href=\"{}\">{}</a></li>\n",
            escape_xml_attr(href),
            escape_xml_text(&title)
        ));
        list.push_str("  </ol>\n");
    } else {
        list = render_nav_list(&entries);
    }

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops" xml:lang="en" lang="en">
<head>
<meta charset="utf-8"/>
<title>{}</title>
</head>
<body>
<nav epub:type="toc" id="toc">
<h1>{}</h1>
{}</nav>
</body>
</html>
"#,
        escape_xml_text(&title),
        escape_xml_text(&title),
        list
    )
}

/// A node in the TOC tree: a label/link plus sub-entries nested under it.
struct NavNode {
    label: String,
    link: String,
    children: Vec<NavNode>,
}

/// Render a flat (level, label, link) list as a well-formed, nested
/// `<ol>`. Builds a forest from heading levels first (deeper nests under
/// the most recent shallower one), then renders recursively — a flat
/// open/close-as-you-go approach is error-prone for skipped levels (an
/// H1 directly followed by an H3).
fn render_nav_list(entries: &[(u8, String, String)]) -> String {
    let forest = build_nav_forest(entries);
    let mut out = String::new();
    render_nav_forest(&forest, 1, &mut out);
    out
}

/// Build a forest of [`NavNode`]s from the flat level/label/link list.
fn build_nav_forest(entries: &[(u8, String, String)]) -> Vec<NavNode> {
    let mut roots: Vec<NavNode> = Vec::new();
    // Stack of (level, index-path) — tracks the path to the currently-open
    // ancestor chain by index rather than raw references, to dodge borrow issues.
    let mut path: Vec<(u8, usize)> = Vec::new(); // (level, index-within-its-parent)

    for (level, label, link) in entries {
        // Pop ancestors that are not strictly shallower than this heading.
        while path.last().is_some_and(|(l, _)| *l >= *level) {
            path.pop();
        }
        let node = NavNode {
            label: label.clone(),
            link: link.clone(),
            children: Vec::new(),
        };
        // Descend to the current parent's children vector by index path.
        let parent_children = walk_children(&mut roots, &path);
        let idx = parent_children.len();
        parent_children.push(node);
        path.push((*level, idx));
    }
    roots
}

/// Follow the index `path` from `roots` down to the children vec new
/// nodes at the current depth should be appended to.
fn walk_children<'a>(roots: &'a mut Vec<NavNode>, path: &[(u8, usize)]) -> &'a mut Vec<NavNode> {
    let mut cur = roots;
    for (_, idx) in path {
        cur = &mut cur[*idx].children;
    }
    cur
}

/// Render a forest of nav nodes as an `<ol>` at the given indent depth.
fn render_nav_forest(nodes: &[NavNode], depth: usize, out: &mut String) {
    if nodes.is_empty() {
        return;
    }
    let pad = "  ".repeat(depth);
    out.push_str(&pad);
    out.push_str("<ol>\n");
    for node in nodes {
        let ipad = "  ".repeat(depth + 1);
        out.push_str(&ipad);
        out.push_str(&format!(
            "<li><a href=\"{}\">{}</a>",
            escape_xml_attr(&node.link),
            escape_xml_text(&node.label)
        ));
        if node.children.is_empty() {
            out.push_str("</li>\n");
        } else {
            out.push('\n');
            render_nav_forest(&node.children, depth + 2, out);
            out.push_str(&ipad);
            out.push_str("</li>\n");
        }
    }
    out.push_str(&pad);
    out.push_str("</ol>\n");
}

// ---- OPF package ------------------------------------------------------------

/// Build the OPF package document: metadata (Dublin Core) + manifest + spine.
fn build_opf(snap: &DocSnapshot, chapters: &[Chapter], images: &[ImageItem]) -> String {
    let title = doc_title(snap);
    let author = snap.properties.get("author").cloned();
    let language = snap
        .properties
        .get("language")
        .cloned()
        .unwrap_or_else(|| "en".to_string());
    let identifier = snap
        .properties
        .get("identifier")
        .cloned()
        .unwrap_or_else(|| format!("urn:uuid:{}", stable_id(snap, &title)));
    let modified = snap
        .properties
        .get("modified")
        .cloned()
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string());

    let mut metadata = String::new();
    metadata.push_str(&format!(
        "    <dc:identifier id=\"book-id\">{}</dc:identifier>\n",
        escape_xml_text(&identifier)
    ));
    metadata.push_str(&format!(
        "    <dc:title>{}</dc:title>\n",
        escape_xml_text(&title)
    ));
    metadata.push_str(&format!(
        "    <dc:language>{}</dc:language>\n",
        escape_xml_text(&language)
    ));
    if let Some(a) = &author {
        metadata.push_str(&format!(
            "    <dc:creator>{}</dc:creator>\n",
            escape_xml_text(a)
        ));
    }
    if let Some(subject) = snap.properties.get("subject") {
        metadata.push_str(&format!(
            "    <dc:subject>{}</dc:subject>\n",
            escape_xml_text(subject)
        ));
    }
    // EPUB 3 requires a dcterms:modified meta.
    metadata.push_str(&format!(
        "    <meta property=\"dcterms:modified\">{}</meta>\n",
        escape_xml_text(&normalize_modified(&modified))
    ));

    // Manifest: nav + chapters + images.
    let mut manifest = String::new();
    manifest.push_str(
        "    <item id=\"nav\" href=\"nav.xhtml\" media-type=\"application/xhtml+xml\" properties=\"nav\"/>\n",
    );
    for ch in chapters {
        manifest.push_str(&format!(
            "    <item id=\"{}\" href=\"{}\" media-type=\"application/xhtml+xml\"/>\n",
            escape_xml_attr(&ch.id),
            escape_xml_attr(&ch.href)
        ));
    }
    for img in images {
        manifest.push_str(&format!(
            "    <item id=\"{}\" href=\"{}\" media-type=\"{}\"/>\n",
            escape_xml_attr(&img.id),
            escape_xml_attr(&img.href),
            img.media_type
        ));
    }

    // Spine: chapters in order.
    let mut spine = String::new();
    for ch in chapters {
        spine.push_str(&format!(
            "    <itemref idref=\"{}\"/>\n",
            escape_xml_attr(&ch.id)
        ));
    }

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="book-id" xml:lang="{lang}">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
{metadata}  </metadata>
  <manifest>
{manifest}  </manifest>
  <spine>
{spine}  </spine>
</package>
"#,
        lang = escape_xml_attr(&language),
        metadata = metadata,
        manifest = manifest,
        spine = spine,
    )
}

/// Best-effort document title from `properties`, falling back to "Untitled".
fn doc_title(snap: &DocSnapshot) -> String {
    snap.properties
        .get("title")
        .cloned()
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "Untitled".to_string())
}

/// Coerce a `modified` string into the EPUB-required
/// `CCYY-MM-DDThh:mm:ssZ` shape when obviously close; otherwise pass
/// through (readers tolerate extra precision).
fn normalize_modified(s: &str) -> String {
    let t = s.trim();
    if t.is_empty() {
        return "1970-01-01T00:00:00Z".to_string();
    }
    t.to_string()
}

/// A deterministic pseudo-id derived from title + block count, so two
/// renders share a `dc:identifier` without a uuid/random dep.
fn stable_id(snap: &DocSnapshot, title: &str) -> String {
    // FNV-1a 64-bit over title + block count — cheap, dependency-free, stable.
    let mut hash: u64 = 0xcbf29ce484222325;
    let feed = |hash: &mut u64, byte: u8| {
        *hash ^= u64::from(byte);
        *hash = hash.wrapping_mul(0x100000001b3);
    };
    for b in title.as_bytes() {
        feed(&mut hash, *b);
    }
    for b in (snap.blocks.len() as u64).to_le_bytes() {
        feed(&mut hash, b);
    }
    format!("{hash:016x}")
}

// ---- XML escaping -----------------------------------------------------------

/// Escape text destined for XML element content.
fn escape_xml_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// Escape a value destined for a double-quoted XML attribute.
fn escape_xml_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests;
