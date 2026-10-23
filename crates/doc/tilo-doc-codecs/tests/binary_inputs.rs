//! Binary content an office file carries is a binary input of the document, not part of it.
//!
//! The document keeps a descriptor and the content address; import hands the bytes to the
//! caller, and export reads them back through a content source. Nothing a package carries is
//! lost on the way — as long as the caller keeps what import handed it.

use std::io::{Cursor, Read, Write};

use tilo_content::{ContentInputs, ContentStore, NoContent};
use tilo_doc::Document;
use tilo_doc_codecs::{DocxCodec, FormatProvider, OdtCodec};

const SAMPLE_DOCX: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/sample.docx"));
const SAMPLE_ODT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/sample.odt"));

/// A font-like part: not text, so the preservation layer must not keep it in the document.
fn binary_part() -> Vec<u8> {
    let mut bytes = vec![0x00, 0x01, 0x00, 0x00, 0xff, 0xfe];
    bytes.extend((0u8..=255).cycle().take(8 * 1024));
    bytes
}

fn with_parts(package: &[u8], extra: &[(&str, &[u8])]) -> Vec<u8> {
    let mut source = zip::ZipArchive::new(Cursor::new(package)).unwrap();
    let mut out = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for i in 0..source.len() {
        let mut entry = source.by_index(i).unwrap();
        let name = entry.name().unwrap().into_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        let stored = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        out.start_file(
            name.clone(),
            if name == "mimetype" { stored } else { options },
        )
        .unwrap();
        out.write_all(&bytes).unwrap();
    }
    for (name, bytes) in extra {
        out.start_file(*name, options).unwrap();
        out.write_all(bytes).unwrap();
    }
    out.finish().unwrap().into_inner()
}

fn part(package: &[u8], name: &str) -> Option<Vec<u8>> {
    let mut archive = zip::ZipArchive::new(Cursor::new(package)).ok()?;
    let mut entry = archive.by_name(name).ok()?;
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn round_trip(codec: &dyn FormatProvider, package: &[u8], part_name: &str) {
    let font = binary_part();
    let source = with_parts(package, &[(part_name, &font)]);

    let mut doc = Document::new();
    let mut inputs = ContentInputs::new();
    codec
        .import(&source, &mut doc.as_mut(), &mut inputs)
        .unwrap();

    // The bytes went to the caller, and the saved document only names them.
    let hash = tilo_content::hash_bytes(&font);
    assert_eq!(inputs.get(&hash), Some(font.as_slice()));
    assert!(doc.referenced_content().contains(&hash));
    let saved = doc.save();
    assert!(
        !contains(&saved, &font[..64]),
        "the saved document carries the binary part ({} bytes)",
        saved.len()
    );

    // Kept in a store, the part comes back out byte for byte.
    let store = ContentStore::memory();
    store.put_inputs(&inputs).unwrap();
    let reopened = Document::load(&saved).unwrap();
    let exported = codec.export(reopened.as_ref(), &store).unwrap();
    assert_eq!(part(&exported, part_name).as_deref(), Some(font.as_slice()));

    // Without its content the part cannot be written; everything else still is.
    let exported = codec.export(reopened.as_ref(), &NoContent).unwrap();
    assert_eq!(part(&exported, part_name), None);
}

#[test]
fn a_docx_font_part_is_a_binary_input_that_round_trips() {
    round_trip(&DocxCodec, SAMPLE_DOCX, "word/fonts/font1.odttf");
}

#[test]
fn an_odt_binary_part_is_a_binary_input_that_round_trips() {
    round_trip(&OdtCodec, SAMPLE_ODT, "Thumbnails/thumbnail.png");
}
