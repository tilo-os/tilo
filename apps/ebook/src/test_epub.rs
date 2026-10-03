//! Small, valid EPUBs built in memory for tests: a stored ZIP holding the
//! container, an EPUB 3 package document, its nav document and one XHTML
//! document per chapter (a heading, then its paragraphs).
use std::fmt::Write as _;

/// An EPUB titled `title` with one spine document per `(heading, paragraphs)`.
pub(crate) fn epub(title: &str, chapters: &[(&str, Vec<String>)]) -> Vec<u8> {
    let documents: Vec<(&str, String)> = chapters
        .iter()
        .map(|(heading, paragraphs)| {
            let mut body = format!("<h2>{heading}</h2>");
            for paragraph in paragraphs {
                let _ = write!(body, "<p>{paragraph}</p>");
            }
            (*heading, body)
        })
        .collect();
    epub_documents(title, &documents)
}

/// A verse classic laid out like the Standard Ebooks Odyssey: front matter,
/// then `books` spine documents, each a z3998 poem section whose header
/// (heading, title, bridgehead) is blocks 0–2 and whose stanzas start at
/// block 3, their lines `<span>`s between `<br/>`s, some indented `i1`.
pub(crate) fn verse_classic(books: usize) -> Vec<u8> {
    let mut documents = vec![
        (
            "Titlepage",
            "<section epub:type=\"titlepage\"><h1>The Test Odyssey</h1><p>Homer</p></section>"
                .to_string(),
        ),
        (
            "Preface",
            "<section epub:type=\"preface\"><h2>Preface</h2><p>An address<br/>on two lines.</p></section>"
                .to_string(),
        ),
    ];
    let headings: Vec<String> = (1..=books).map(|book| format!("Book {book}")).collect();
    for heading in &headings {
        let mut body = format!(
            "<section epub:type=\"chapter z3998:poem\"><header><h2>{heading}</h2>\
             <p epub:type=\"title\">The voyage</p>\
             <p epub:type=\"se:bridgehead\">A council of the gods—Mercury despatched to Calypso.</p></header>"
        );
        for stanza in 1..=3 {
            let _ = write!(
                body,
                "<p><span class=\"first-child\">{heading}, stanza {stanza}: tell me, O Muse,</span><br/>\
                 <span>of that sagacious man who wandered far</span><br/>\
                 <span class=\"i1\">and visited the capitals of nations.</span></p>"
            );
        }
        body.push_str("</section>");
        documents.push((heading.as_str(), body));
    }
    epub_documents("The Test Odyssey", &documents)
}

/// An EPUB titled `title` with one spine document per `(title, body markup)`.
fn epub_documents(title: &str, chapters: &[(&str, String)]) -> Vec<u8> {
    let mut files = vec![
        ("mimetype".to_string(), b"application/epub+zip".to_vec()),
        (
            "META-INF/container.xml".to_string(),
            br#"<?xml version="1.0" encoding="utf-8"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#.to_vec(),
        ),
    ];
    let (mut manifest, mut spine, mut nav) = (String::new(), String::new(), String::new());
    for (index, (heading, body)) in chapters.iter().enumerate() {
        let href = format!("text/chapter-{index}.xhtml");
        let _ = write!(
            manifest,
            r#"<item id="c{index}" href="{href}" media-type="application/xhtml+xml"/>"#
        );
        let _ = write!(spine, r#"<itemref idref="c{index}"/>"#);
        let _ = write!(nav, r#"<li><a href="{href}">{heading}</a></li>"#);
        files.push((
            format!("OEBPS/{href}"),
            format!(
                r#"<?xml version="1.0" encoding="utf-8"?><html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><head><title>{heading}</title></head><body>{body}</body></html>"#
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

#[test]
fn a_generated_verse_classic_parses_stanzas_as_verse_at_prose_block_indices() {
    use crate::model::Block;

    let book = crate::epub::parse_epub(&verse_classic(3), 0, "").expect("parses");
    assert_eq!(book.chapters.len(), 5);
    let preface = &book.chapters[1].blocks[1];
    assert!(matches!(preface, Block::Paragraph { .. }), "{preface:?}");
    assert!(
        preface.spans().iter().any(|span| span.line_break),
        "a prose <br/> breaks its line"
    );
    let book_one = &book.chapters[2];
    assert_eq!(book_one.blocks.len(), 6, "{:#?}", book_one.blocks);
    assert!(matches!(book_one.blocks[0], Block::Heading { .. }));
    assert!(matches!(book_one.blocks[2], Block::Paragraph { .. }));
    let Block::Verse { spans, quoted } = &book_one.blocks[3] else {
        panic!("the first stanza is verse: {:?}", book_one.blocks[3]);
    };
    assert!(!quoted);
    let breaks: Vec<u8> = spans
        .iter()
        .filter(|span| span.line_break)
        .map(|span| span.line_indent)
        .collect();
    assert_eq!(breaks, [0, 1], "three lines, the last indented");
    assert_eq!(
        book_one.block_text(3),
        "Book 1, stanza 1: tell me, O Muse, of that sagacious man who wandered far \
         and visited the capitals of nations."
    );
}
