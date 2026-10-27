//! EPUB package resolution: `META-INF/container.xml` → rootfile → OPF
//! `<metadata>`, `<manifest>`, `<spine>` and the cover image.
//!
//! An EPUB is a ZIP archive with a fixed shape:
//!
//! ```text
//! mimetype                     (stored, "application/epub+zip")
//! META-INF/container.xml       → points at the OPF package document
//! <base>/content.opf           → <metadata> + <manifest> + <spine>
//! <base>/*.xhtml               → the chapters, in spine order
//! ```
//!
//! Both reads here are bounded ([`MAX_CONTAINER_BYTES`], [`MAX_PACKAGE_BYTES`]);
//! the OPF parse is pure and never touches the archive. All hrefs are resolved
//! against the OPF's directory and normalised to ZIP entry names.

use std::collections::HashMap;
use std::io::{Read, Seek};

use roxmltree::Document;

use crate::{BoundedArchive, ZipError};

/// Where every EPUB names its package document.
pub const CONTAINER_PATH: &str = "META-INF/container.xml";
/// `container.xml` lists a handful of rootfiles; anything larger is not one.
pub const MAX_CONTAINER_BYTES: u64 = 512 * 1024;
/// Generous for the largest real-world OPF manifests.
pub const MAX_PACKAGE_BYTES: u64 = 2 * 1024 * 1024;

/// Why an EPUB's package document could not be located or parsed.
#[derive(Debug, thiserror::Error)]
pub enum EpubError {
    /// Reading `container.xml` or the OPF from the archive failed.
    #[error("read {name}: {source}")]
    Read {
        name: String,
        #[source]
        source: ZipError,
    },
    /// `container.xml` or the OPF is not well-formed XML.
    #[error("parse {name}: {source}")]
    Parse {
        name: &'static str,
        #[source]
        source: roxmltree::Error,
    },
    /// `container.xml` names no package document.
    #[error("no <rootfile> in container.xml")]
    NoRootfile,
    /// The `<rootfile>` element has no `full-path`.
    #[error("<rootfile> missing full-path")]
    RootfileWithoutPath,
}

/// The normalised ZIP path of the OPF package document named by
/// `META-INF/container.xml`.
///
/// # Errors
/// [`EpubError::Read`] when `container.xml` is missing or breaches its bound,
/// [`EpubError::Parse`] when it isn't XML, and [`EpubError::NoRootfile`] /
/// [`EpubError::RootfileWithoutPath`] when it names no package.
pub fn find_opf_path<R: Read + Seek>(archive: &mut BoundedArchive<R>) -> Result<String, EpubError> {
    let xml = archive
        .read_text(CONTAINER_PATH, MAX_CONTAINER_BYTES)
        .map_err(|source| EpubError::Read {
            name: CONTAINER_PATH.to_string(),
            source,
        })?;
    let doc = Document::parse(&xml).map_err(|source| EpubError::Parse {
        name: "container.xml",
        source,
    })?;
    let rootfile = doc
        .descendants()
        .find(|n| n.has_tag_name("rootfile"))
        .ok_or(EpubError::NoRootfile)?;
    let path = rootfile
        .attribute("full-path")
        .ok_or(EpubError::RootfileWithoutPath)?;
    Ok(normalize(path))
}

/// Reads the OPF at `opf_path` (bounded to [`MAX_PACKAGE_BYTES`]) and parses it.
///
/// # Errors
/// [`EpubError::Read`] or [`EpubError::Parse`].
pub fn read_package<R: Read + Seek>(
    archive: &mut BoundedArchive<R>,
    opf_path: &str,
) -> Result<OpfPackage, EpubError> {
    let src = archive
        .read_text(opf_path, MAX_PACKAGE_BYTES)
        .map_err(|source| EpubError::Read {
            name: opf_path.to_string(),
            source,
        })?;
    OpfPackage::parse(&src, &parent_dir(opf_path))
}

/// The parts of an OPF package document readers act on. Every href is
/// resolved relative to the OPF's directory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpfPackage {
    pub title: String,
    pub creator: String,
    pub publisher: String,
    pub language: String,
    pub description: String,
    pub subject: String,
    /// Spine hrefs in reading order. May be empty; whether that is an error is
    /// the reader's call.
    pub spine: Vec<String>,
    /// href of the EPUB3 nav document, if declared (`properties="nav"`).
    pub nav_href: Option<String>,
    /// href of the NCX, if any (spine `toc=` idref or manifest media-type).
    pub ncx_href: Option<String>,
    /// href of the cover image, from `properties="cover-image"`, the legacy
    /// `<meta name="cover">` pointer, or a cover-named image item — in that
    /// order of confidence.
    pub cover_href: Option<String>,
}

impl OpfPackage {
    /// Parses an OPF document whose hrefs are relative to `base`.
    ///
    /// # Errors
    /// [`EpubError::Parse`] when `src` is not well-formed XML.
    pub fn parse(src: &str, base: &str) -> Result<Self, EpubError> {
        let doc = Document::parse(src).map_err(|source| EpubError::Parse {
            name: "OPF",
            source,
        })?;

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
        let language = meta_text("language");
        let description = meta_text("description");
        let subject = meta_text("subject");

        // The legacy cover pointer: `<meta name="cover" content="item-id"/>`.
        let cover_meta_id = doc
            .descendants()
            .find(|n| {
                n.has_tag_name("meta")
                    && n.attribute("name") == Some("cover")
                    && n.attribute("content").is_some()
            })
            .and_then(|n| n.attribute("content"))
            .map(std::string::ToString::to_string);

        // Manifest: id -> resolved href.
        let mut manifest = HashMap::new();
        let mut nav_href = None;
        let mut ncx_by_id: HashMap<String, String> = HashMap::new();
        // Cover candidates, in descending confidence.
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
                if props.split_whitespace().any(|p| p == "nav") {
                    nav_href = Some(resolved.clone());
                }
                if props.split_whitespace().any(|p| p == "cover-image") {
                    cover_by_props = Some(resolved.clone());
                }
                // The id/href checks are deliberately symmetric (either one
                // naming "cover" is a signal), not a copy-paste of one field.
                #[allow(
                    clippy::suspicious_operation_groupings,
                    reason = "id and href are intentionally checked the same way, not swapped"
                )]
                let looks_like_cover = media.starts_with("image/")
                    && cover_by_name.is_none()
                    && (id.to_ascii_lowercase().contains("cover")
                        || href.to_ascii_lowercase().contains("cover"));
                if looks_like_cover {
                    cover_by_name = Some(resolved.clone());
                }
                if media == "application/x-dtbncx+xml" {
                    ncx_by_id.insert(id.clone(), resolved.clone());
                }
                manifest.insert(id, resolved);
            }
        }
        let cover_href = cover_by_props
            .or_else(|| cover_meta_id.and_then(|id| manifest.get(&id).cloned()))
            .or(cover_by_name);

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
                if let Some(idref) = itemref.attribute("idref")
                    && let Some(href) = manifest.get(idref)
                {
                    spine.push(href.clone());
                }
            }
        }
        if ncx_href.is_none() {
            ncx_href = ncx_by_id.into_values().next();
        }

        Ok(Self {
            title,
            creator,
            publisher,
            language,
            description,
            subject,
            spine,
            nav_href,
            ncx_href,
            cover_href,
        })
    }
}

/// `href` without its `#fragment`.
#[must_use]
pub fn strip_fragment(href: &str) -> &str {
    href.split('#').next().unwrap_or(href)
}

/// The directory part of a ZIP path (`""` at the root).
#[must_use]
pub fn parent_dir(path: &str) -> String {
    match path.rfind('/') {
        // `/` is ASCII, so `i` is always a char boundary; `.get()` avoids
        // slicing on that trust anyway.
        Some(i) => path.get(..i).unwrap_or_default().to_string(),
        None => String::new(),
    }
}

/// Join a relative href onto a base directory and normalise. A leading `/`
/// means the archive root.
#[must_use]
pub fn join(base: &str, href: &str) -> String {
    if let Some(stripped) = href.strip_prefix('/') {
        return normalize(stripped);
    }
    if base.is_empty() {
        return normalize(href);
    }
    normalize(&format!("{base}/{href}"))
}

/// Collapse `.` / `..` segments and `./` prefixes; ZIP names use `/`. A `..`
/// never climbs above the archive root.
#[must_use]
pub fn normalize(path: &str) -> String {
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
    use crate::ZipLimits;
    use std::io::{Cursor, Write};
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    fn epub(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut zw = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data) in entries {
            zw.start_file(*name, SimpleFileOptions::default()).unwrap();
            zw.write_all(data.as_bytes()).unwrap();
        }
        zw.finish().unwrap().into_inner()
    }

    const CONTAINER: &str = r#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/./content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#;

    #[test]
    fn package_paths_normalize_join_and_split() {
        for (path, want) in [
            ("OEBPS/./ch1.xhtml", "OEBPS/ch1.xhtml"),
            ("OEBPS/text/../ch1.xhtml", "OEBPS/ch1.xhtml"),
            ("../../etc/passwd", "etc/passwd"),
        ] {
            assert_eq!(normalize(path), want, "normalize {path}");
        }
        for (base, rel, want) in [
            ("OEBPS", "ch1.xhtml", "OEBPS/ch1.xhtml"),
            ("OEBPS/text", "../images/c.png", "OEBPS/images/c.png"),
            ("", "ch1.xhtml", "ch1.xhtml"),
            ("OEBPS", "/root.png", "root.png"),
        ] {
            assert_eq!(join(base, rel), want, "join {base} + {rel}");
        }
        assert_eq!(strip_fragment("ch1.xhtml#sec2"), "ch1.xhtml");
        assert_eq!(strip_fragment("ch1.xhtml"), "ch1.xhtml");
        assert_eq!(parent_dir("OEBPS/content.opf"), "OEBPS");
        assert_eq!(parent_dir("content.opf"), "");
    }

    #[test]
    fn packages_resolve_metadata_spine_nav_and_cover() {
        let opf = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title> A Title </dc:title><dc:creator>Someone</dc:creator><dc:language>nl</dc:language>
  </metadata>
  <manifest>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
    <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
    <item id="c2" href="text/ch2.xhtml" media-type="application/xhtml+xml"/>
    <item id="c1" href="text/ch1.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine toc="ncx"><itemref idref="c1"/><itemref idref="missing"/><itemref idref="c2"/></spine>
</package>"#;
        let bytes = epub(&[(CONTAINER_PATH, CONTAINER), ("OEBPS/content.opf", opf)]);
        let mut archive = BoundedArchive::from_bytes(&bytes, ZipLimits::default()).unwrap();
        let opf_path = find_opf_path(&mut archive).unwrap();
        assert_eq!(opf_path, "OEBPS/content.opf");
        let pkg = read_package(&mut archive, &opf_path).unwrap();
        assert_eq!(pkg.title, "A Title");
        assert_eq!(pkg.creator, "Someone");
        assert_eq!(pkg.language, "nl");
        assert_eq!(pkg.spine, ["OEBPS/text/ch1.xhtml", "OEBPS/text/ch2.xhtml"]);
        assert_eq!(pkg.nav_href.as_deref(), Some("OEBPS/nav.xhtml"));
        assert_eq!(pkg.ncx_href.as_deref(), Some("OEBPS/toc.ncx"));
        assert_eq!(pkg.cover_href, None);

        let with = |meta: &str, items: &str| {
            let opf = format!(
                "<package><metadata>{meta}</metadata><manifest>{items}</manifest><spine/></package>"
            );
            OpfPackage::parse(&opf, "OEBPS").unwrap().cover_href
        };
        let named = r#"<item id="img-cover" href="a.jpg" media-type="image/jpeg"/>"#;
        let pointed = r#"<item id="pic" href="b.jpg" media-type="image/jpeg"/>"#;
        let declared =
            r#"<item id="x" href="c.jpg" media-type="image/jpeg" properties="cover-image"/>"#;
        let meta = r#"<meta name="cover" content="pic"/>"#;
        assert_eq!(
            with(meta, &format!("{named}{pointed}{declared}")).as_deref(),
            Some("OEBPS/c.jpg")
        );
        assert_eq!(
            with(meta, &format!("{named}{pointed}")).as_deref(),
            Some("OEBPS/b.jpg")
        );
        assert_eq!(with("", named).as_deref(), Some("OEBPS/a.jpg"));
        // A cover-named item that is not an image is no cover.
        assert_eq!(
            with(
                "",
                r#"<item id="cover" href="cover.xhtml" media-type="application/xhtml+xml"/>"#
            ),
            None
        );

        let pkg = OpfPackage::parse("<package><manifest/></package>", "").unwrap();
        assert!(pkg.spine.is_empty());
    }

    #[test]
    fn container_errors_are_specific_and_bounded() {
        let bytes = epub(&[("mimetype", "application/epub+zip")]);
        let mut archive = BoundedArchive::from_bytes(&bytes, ZipLimits::default()).unwrap();
        assert!(matches!(
            find_opf_path(&mut archive),
            Err(EpubError::Read {
                source: ZipError::Missing { .. },
                ..
            })
        ));

        let bytes = epub(&[(CONTAINER_PATH, "<container><rootfiles/></container>")]);
        let mut archive = BoundedArchive::from_bytes(&bytes, ZipLimits::default()).unwrap();
        assert!(matches!(
            find_opf_path(&mut archive),
            Err(EpubError::NoRootfile)
        ));

        let bytes = epub(&[(CONTAINER_PATH, "<container><rootfile/></container>")]);
        let mut archive = BoundedArchive::from_bytes(&bytes, ZipLimits::default()).unwrap();
        assert!(matches!(
            find_opf_path(&mut archive),
            Err(EpubError::RootfileWithoutPath)
        ));

        // An oversized container is refused before parsing.
        let padding = " ".repeat(usize::try_from(MAX_CONTAINER_BYTES).unwrap());
        let container = format!("{CONTAINER}{padding}");
        let bytes = epub(&[(CONTAINER_PATH, &container)]);
        let mut archive = BoundedArchive::from_bytes(&bytes, ZipLimits::default()).unwrap();
        let err = find_opf_path(&mut archive).unwrap_err();
        assert!(
            matches!(&err, EpubError::Read { source, .. } if source.is_limit_breach()),
            "got {err}"
        );
    }
}
