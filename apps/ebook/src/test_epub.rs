//! Small, valid EPUBs built in memory for tests: a stored ZIP holding the
//! container, an EPUB 3 package document, its nav document and one XHTML
//! document per chapter (a heading, then its paragraphs).
use std::fmt::Write as _;

/// An EPUB titled `title` with one spine document per `(heading, paragraphs)`.
pub(crate) fn epub(title: &str, chapters: &[(&str, Vec<String>)]) -> Vec<u8> {
    let mut files = vec![
        ("mimetype".to_string(), b"application/epub+zip".to_vec()),
        (
            "META-INF/container.xml".to_string(),
            br#"<?xml version="1.0" encoding="utf-8"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#.to_vec(),
        ),
    ];
    let (mut manifest, mut spine, mut nav) = (String::new(), String::new(), String::new());
    for (index, (heading, paragraphs)) in chapters.iter().enumerate() {
        let href = format!("text/chapter-{index}.xhtml");
        let _ = write!(
            manifest,
            r#"<item id="c{index}" href="{href}" media-type="application/xhtml+xml"/>"#
        );
        let _ = write!(spine, r#"<itemref idref="c{index}"/>"#);
        let _ = write!(nav, r#"<li><a href="{href}">{heading}</a></li>"#);
        let mut body = String::new();
        for paragraph in paragraphs {
            let _ = write!(body, "<p>{paragraph}</p>");
        }
        files.push((
            format!("OEBPS/{href}"),
            format!(
                r#"<?xml version="1.0" encoding="utf-8"?><html xmlns="http://www.w3.org/1999/xhtml"><head><title>{heading}</title></head><body><h2>{heading}</h2>{body}</body></html>"#
            )
            .into_bytes(),
        ));
    }
    files.push((
        "OEBPS/content.opf".to_string(),
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="uid"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="uid">urn:tilo:test:{title}</dc:identifier><dc:title>{title}</dc:title><dc:creator>Homer</dc:creator><dc:language>en</dc:language></metadata><manifest><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>{manifest}</manifest><spine>{spine}</spine></package>"#
        )
        .into_bytes(),
    ));
    files.push((
        "OEBPS/nav.xhtml".to_string(),
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?><html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><head><title>Contents</title></head><body><nav epub:type="toc"><ol>{nav}</ol></nav></body></html>"#
        )
        .into_bytes(),
    ));
    stored_zip(&files)
}

/// A book of `count` chapters named "Book I", "Book II", … each with `lines`
/// numbered paragraphs, laid out like a long classic.
pub(crate) fn long_classic(count: usize, lines: usize) -> Vec<u8> {
    let chapters: Vec<(String, Vec<String>)> = (1..=count)
        .map(|book| {
            (
                format!("Book {book}"),
                (1..=lines)
                    .map(|line| format!("Book {book}, paragraph {line}: sing to me of the man."))
                    .collect(),
            )
        })
        .collect();
    let borrowed: Vec<(&str, Vec<String>)> = chapters
        .iter()
        .map(|(heading, paragraphs)| (heading.as_str(), paragraphs.clone()))
        .collect();
    epub("The Test Odyssey", &borrowed)
}

fn stored_zip(files: &[(String, Vec<u8>)]) -> Vec<u8> {
    const DOS_DATE_1980_01_01: u16 = 0x21;
    let (mut out, mut central) = (Vec::new(), Vec::new());
    for (name, data) in files {
        let offset = out.len() as u32;
        let (crc, size, name_len) = (crc32(data), data.len() as u32, name.len() as u16);
        for (header, signature) in [(&mut out, 0x0403_4b50_u32), (&mut central, 0x0201_4b50)] {
            header.extend(signature.to_le_bytes());
            if signature == 0x0201_4b50 {
                header.extend(20u16.to_le_bytes());
            }
            header.extend(20u16.to_le_bytes());
            header.extend([0; 4]); // flags, method: stored
            header.extend(0u16.to_le_bytes());
            header.extend(DOS_DATE_1980_01_01.to_le_bytes());
            header.extend(crc.to_le_bytes());
            header.extend(size.to_le_bytes());
            header.extend(size.to_le_bytes());
            header.extend(name_len.to_le_bytes());
            header.extend(0u16.to_le_bytes()); // extra field
        }
        central.extend([0; 6]); // comment length, disk, internal attributes
        central.extend([0; 4]); // external attributes
        central.extend(offset.to_le_bytes());
        central.extend(name.as_bytes());
        out.extend(name.as_bytes());
        out.extend(data);
    }
    let (directory_offset, directory_size) = (out.len() as u32, central.len() as u32);
    let entries = files.len() as u16;
    out.extend(central);
    out.extend(0x0605_4b50_u32.to_le_bytes());
    out.extend([0; 4]);
    out.extend(entries.to_le_bytes());
    out.extend(entries.to_le_bytes());
    out.extend(directory_size.to_le_bytes());
    out.extend(directory_offset.to_le_bytes());
    out.extend([0; 2]);
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[test]
fn a_generated_classic_parses_with_every_chapter() {
    let bytes = long_classic(24, 6);
    let book = crate::epub::parse_epub(&bytes, 0, "").expect("the generated EPUB parses");
    assert_eq!(book.title, "The Test Odyssey");
    assert_eq!(book.author, "Homer");
    assert_eq!(book.chapters.len(), 24);
    assert_eq!(book.chapters[23].title, "Book 24");
    assert_eq!(
        book.chapters[3].blocks.len(),
        7,
        "the heading and six paragraphs"
    );
    assert_eq!(book.id, crate::epub::content_id(&bytes));
}
