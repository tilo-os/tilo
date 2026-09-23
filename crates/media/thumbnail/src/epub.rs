//! Pulling the cover image out of an EPUB container.
//!
//! An EPUB is a ZIP: `META-INF/container.xml` points at the OPF package
//! document, whose `<manifest>` names the cover image. This lifts exactly that
//! resolution path (in descending confidence: `properties="cover-image"`, the
//! legacy `<meta name="cover">` pointer, then a cover-named image item) out of
//! the ebook reader so the shared generator needs no `epub` crate and stays
//! hermetic. It returns the *raw cover bytes*; [`crate::generate`] decodes them.

use std::collections::HashMap;
use std::io::Cursor;

use bounded_zip::{BoundedArchive, ZipLimits};
use roxmltree::Document;

type Archive<'a> = BoundedArchive<Cursor<&'a [u8]>>;

/// A cover image is never larger than a few MB; cap the read so a malformed
/// entry can't make a worker allocate without bound.
const MAX_COVER_BYTES: u64 = 32 * 1024 * 1024;
/// `META-INF/container.xml` names one rootfile; the OPF lists the manifest.
/// Both are small text documents, bounded like the ebook reader bounds them.
const MAX_CONTAINER_BYTES: u64 = 512 * 1024;
const MAX_PACKAGE_BYTES: u64 = 2 * 1024 * 1024;

/// The raw (undecoded) bytes of an EPUB's cover image, or `None` if the book
/// declares no cover or the container is unreadable.
pub fn cover_bytes(epub: &[u8]) -> Option<Vec<u8>> {
    let mut archive = BoundedArchive::from_bytes(epub, ZipLimits::default()).ok()?;
    let opf_path = find_opf_path(&mut archive)?;
    let opf_src = archive.read_text(&opf_path, MAX_PACKAGE_BYTES).ok()?;
    let base = parent_dir(&opf_path);
    let cover_href = resolve_cover_href(&opf_src, &base)?;
    archive.read(&cover_href, MAX_COVER_BYTES).ok()
}

fn find_opf_path(archive: &mut Archive<'_>) -> Option<String> {
    let xml = archive
        .read_text("META-INF/container.xml", MAX_CONTAINER_BYTES)
        .ok()?;
    let doc = Document::parse(&xml).ok()?;
    let rootfile = doc.descendants().find(|n| n.has_tag_name("rootfile"))?;
    Some(normalize(rootfile.attribute("full-path")?))
}

/// The cover href, resolved relative to the OPF base, in descending confidence.
#[allow(
    clippy::suspicious_operation_groupings,
    reason = "`id.to_ascii_lowercase().contains(\"cover\") || href...contains(\"cover\")` \
              deliberately checks the same substring against two different fields (id, href), \
              not a mismatched-pair typo"
)]
fn resolve_cover_href(opf: &str, base: &str) -> Option<String> {
    let doc = Document::parse(opf).ok()?;

    // Legacy pointer: `<meta name="cover" content="item-id"/>`.
    let cover_meta_id = doc
        .descendants()
        .find(|n| {
            n.has_tag_name("meta")
                && n.attribute("name") == Some("cover")
                && n.attribute("content").is_some()
        })
        .and_then(|n| n.attribute("content"))
        .map(std::string::ToString::to_string);

    let mut manifest: HashMap<String, String> = HashMap::new();
    let mut cover_by_props: Option<String> = None;
    let mut cover_by_name: Option<String> = None;
    if let Some(man) = doc.descendants().find(|n| n.has_tag_name("manifest")) {
        for item in man.children().filter(|n| n.has_tag_name("item")) {
            let id = item.attribute("id").unwrap_or_default().to_string();
            let href = item.attribute("href").unwrap_or_default();
            if id.is_empty() || href.is_empty() {
                continue;
            }
            let resolved = join(base, href);
            let props = item.attribute("properties").unwrap_or_default();
            let media = item.attribute("media-type").unwrap_or_default();
            if props.split_whitespace().any(|p| p == "cover-image") {
                cover_by_props = Some(resolved.clone());
            }
            if media.starts_with("image/")
                && cover_by_name.is_none()
                && (id.to_ascii_lowercase().contains("cover")
                    || href.to_ascii_lowercase().contains("cover"))
            {
                cover_by_name = Some(resolved.clone());
            }
            manifest.insert(id, resolved);
        }
    }

    cover_by_props
        .or_else(|| cover_meta_id.and_then(|id| manifest.get(&id).cloned()))
        .or(cover_by_name)
}

fn parent_dir(path: &str) -> String {
    match path.rfind('/') {
        // `/` is ASCII, so `i` is always a char boundary; `.get()` avoids
        // slicing on that trust anyway.
        Some(i) => path.get(..i).unwrap_or(path).to_string(),
        None => String::new(),
    }
}

fn join(base: &str, href: &str) -> String {
    if let Some(rest) = href.strip_prefix('/') {
        return normalize(rest);
    }
    if base.is_empty() {
        return normalize(href);
    }
    normalize(&format!("{base}/{href}"))
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    /// Build a minimal EPUB whose OPF names a cover image (the given
    /// PNG bytes at `OEBPS/cover.png`, tagged `properties="cover-image"`).
    pub(crate) fn epub_with_cover(cover_png: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(Cursor::new(&mut buf));
            let stored =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            let deflated =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

            // The mimetype must be first and stored.
            zip.start_file("mimetype", stored).unwrap();
            zip.write_all(b"application/epub+zip").unwrap();

            zip.start_file("META-INF/container.xml", deflated).unwrap();
            zip.write_all(
                br#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#,
            )
            .unwrap();

            zip.start_file("OEBPS/content.opf", deflated).unwrap();
            zip.write_all(
                br#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata><dc:title xmlns:dc="http://purl.org/dc/elements/1.1/">Sample</dc:title></metadata>
  <manifest>
    <item id="cover" href="cover.png" media-type="image/png" properties="cover-image"/>
    <item id="c1" href="ch1.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine><itemref idref="c1"/></spine>
</package>"#,
            )
            .unwrap();

            zip.start_file("OEBPS/ch1.xhtml", deflated).unwrap();
            zip.write_all(b"<html><body><p>hi</p></body></html>")
                .unwrap();

            zip.start_file("OEBPS/cover.png", deflated).unwrap();
            zip.write_all(cover_png).unwrap();

            zip.finish().unwrap();
        }
        buf
    }

    #[test]
    fn extracts_declared_cover_bytes() {
        let cover = crate::testutil::solid_png(20, 30, [200, 50, 10]);
        let epub = epub_with_cover(&cover);
        let got = cover_bytes(&epub).expect("cover resolved");
        assert_eq!(got, cover, "the exact cover entry bytes come back");
    }

    #[test]
    fn cover_bytes_is_none_for_non_epub() {
        assert!(cover_bytes(b"definitely not a zip").is_none());
    }
}
