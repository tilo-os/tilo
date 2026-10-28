//! Pulling the cover image out of an EPUB container.
//!
//! An EPUB is a ZIP: `META-INF/container.xml` points at the OPF package
//! document, whose `<manifest>` names the cover image. The resolution (in
//! descending confidence: `properties="cover-image"`, the legacy
//! `<meta name="cover">` pointer, then a cover-named image item) is the same
//! [`bounded_zip::epub`] path the ebook reader uses, so the shared generator
//! needs no app crate and stays hermetic. It returns the *raw cover bytes*;
//! [`crate::generate()`] decodes them.

use bounded_zip::{BoundedArchive, ZipLimits, epub};

/// A cover image is never larger than a few MB; cap the read so a malformed
/// entry can't make a worker allocate without bound.
const MAX_COVER_BYTES: u64 = 32 * 1024 * 1024;

/// The raw (undecoded) bytes of an EPUB's cover image, or `None` if the book
/// declares no cover or the container is unreadable.
pub fn cover_bytes(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut archive = BoundedArchive::from_bytes(bytes, ZipLimits::default()).ok()?;
    let opf_path = epub::find_opf_path(&mut archive).ok()?;
    let cover_href = epub::read_package(&mut archive, &opf_path)
        .ok()?
        .cover_href?;
    archive.read(&cover_href, MAX_COVER_BYTES).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
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
    fn epub_cover_bytes_come_only_from_a_declared_cover() {
        {
            let cover = crate::testutil::solid_png(20, 30, [200, 50, 10]);
            let epub = epub_with_cover(&cover);
            let got = cover_bytes(&epub).expect("cover resolved");
            assert_eq!(got, cover, "the exact cover entry bytes come back");
        }
        {
            assert!(cover_bytes(b"definitely not a zip").is_none());
        }
    }
}
