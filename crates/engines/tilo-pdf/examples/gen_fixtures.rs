//! Generate PDF review candidates under `target/pdf-fixture-candidates/`.
//! `TILO_PDF_FIXTURE_OUT` selects another output directory; frozen pins are explicit.
//!
//! ```text
//! cargo run --locked -p tilo-pdf --features fixture-gen --example gen_fixtures [NAME.pdf…]
//! ```
//!
//! With names, only those fixtures are written.
//!
//! Text fixtures are typeset by Typst from the sources below with fonts the
//! Tilo image ships (Libertinus from typst-assets; Noto Sans CJK and Noto
//! Naskh Arabic from the system font directories), so every fixture is
//! real producer output rather than a hand-drawn PDF. Typst subsets the
//! fonts, which keeps each file a few kilobytes. The form fixture is written
//! directly, because Typst does not produce interactive forms.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::print_stdout,
    reason = "a developer tool: failing loudly is the right behaviour"
)]

use std::path::{Path, PathBuf};

use typst::diag::{FileError, FileResult};
use typst::foundations::{Bytes, Datetime, Duration};
use typst::syntax::{FileId, Source};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt, World};
use typst_layout::PagedDocument;

struct FixtureWorld {
    library: LazyHash<Library>,
    book: LazyHash<FontBook>,
    fonts: Vec<Font>,
    main: Source,
}

impl World for FixtureWorld {
    fn library(&self) -> &LazyHash<Library> {
        &self.library
    }
    fn book(&self) -> &LazyHash<FontBook> {
        &self.book
    }
    fn main(&self) -> FileId {
        self.main.id()
    }
    fn source(&self, id: FileId) -> FileResult<Source> {
        if id == self.main.id() {
            Ok(self.main.clone())
        } else {
            Err(FileError::NotFound(PathBuf::from(
                id.vpath().get_without_slash(),
            )))
        }
    }
    fn file(&self, id: FileId) -> FileResult<Bytes> {
        Err(FileError::NotFound(PathBuf::from(
            id.vpath().get_without_slash(),
        )))
    }
    fn font(&self, index: usize) -> Option<Font> {
        self.fonts.get(index).cloned()
    }
    fn today(&self, _offset: Option<Duration>) -> Option<Datetime> {
        None
    }
}

fn system_font(candidates: &[&str]) -> Vec<Font> {
    for c in candidates {
        if let Ok(bytes) = std::fs::read(c) {
            let fonts: Vec<Font> = Font::iter(Bytes::new(bytes)).collect();
            if !fonts.is_empty() {
                return fonts;
            }
        }
    }
    panic!("none of these fonts exist: {candidates:?}");
}

/// The fonts the named fixtures need: typst-assets always, the system CJK
/// and Arabic faces only for the fixtures set in them.
fn fonts(wanted: &dyn Fn(&str) -> bool) -> Vec<Font> {
    let mut fonts: Vec<Font> = typst_assets::fonts()
        .flat_map(|data| Font::iter(Bytes::new(data)))
        .collect();
    if wanted("cjk.pdf") {
        fonts.extend(system_font(&[
            "/usr/share/fonts/google-noto-sans-cjk-fonts/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        ]));
    }
    if wanted("arabic.pdf") {
        fonts.extend(system_font(&[
            "/usr/share/fonts/google-noto/NotoNaskhArabic-Regular.ttf",
            "/usr/share/fonts/google-noto-vf/NotoNaskhArabic[wght].ttf",
            "/usr/share/fonts/truetype/noto/NotoNaskhArabic-Regular.ttf",
        ]));
    }
    fonts
}

fn compile(fonts: &[Font], src: &str) -> Vec<u8> {
    let world = FixtureWorld {
        library: LazyHash::new(Library::builder().build()),
        book: LazyHash::new(FontBook::from_fonts(fonts.iter())),
        fonts: fonts.to_vec(),
        main: Source::detached(src),
    };
    let document = typst::compile::<PagedDocument>(&world)
        .output
        .unwrap_or_else(|e| panic!("typst: {e:?}"));
    typst_pdf::pdf(&document, &typst_pdf::PdfOptions::default())
        .unwrap_or_else(|e| panic!("typst-pdf: {e:?}"))
}

const PAGE: &str = "#set page(width: 300pt, height: 200pt, margin: 20pt)\n";

fn fixtures() -> Vec<(&'static str, String)> {
    vec![
        (
            "latin.pdf",
            format!(
                "{PAGE}#set text(font: \"Libertinus Serif\", size: 12pt)\n\
                 The quick brown fox jumps over the lazy dog.\n\n\
                 Second paragraph with numbers 1234 and punctuation!"
            ),
        ),
        (
            "ligatures.pdf",
            format!(
                "{PAGE}#set text(font: \"Libertinus Serif\", size: 14pt)\n\
                 The office staff fluently shuffled fifty affine files."
            ),
        ),
        (
            "arabic.pdf",
            format!(
                "{PAGE}#set text(font: \"Noto Naskh Arabic\", size: 16pt, lang: \"ar\", dir: rtl)\n\
                 مرحبا بالعالم\n\n\
                 السلام عليكم"
            ),
        ),
        (
            "cjk.pdf",
            format!(
                "{PAGE}#set text(font: \"Noto Sans CJK SC\", size: 14pt, lang: \"zh\")\n\
                 你好，世界。这是一个测试。\n\n\
                 日本語のテキストも含みます。"
            ),
        ),
        (
            "columns.pdf",
            "#set page(width: 400pt, height: 260pt, margin: 20pt)\n\
             #set text(font: \"Libertinus Serif\", size: 10pt)\n\
             #set par(justify: false)\n\
             = Two Column Title\n\
             #columns(2, gutter: 24pt)[\n\
             Alpha begins the left column and keeps going for a while so it wraps onto several lines of text.\n\n\
             Beta closes the left column.\n\
             #colbreak()\n\
             Gamma begins the right column and also wraps across more than one line of text here.\n\n\
             Delta closes the right column.\n\
             ]"
                .to_string(),
        ),
        (
            "multipage.pdf",
            "#set page(width: 300pt, height: 200pt, margin: 20pt)\n\
             #set text(font: \"Libertinus Serif\", size: 12pt)\n\
             #outline()\n\
             #pagebreak()\n\
             = First Chapter\n\
             Findable needle on page two. #link(\"https://example.org\")[A web link].\n\
             #pagebreak()\n\
             = Second Chapter\n\
             Another needle on page three."
                .to_string(),
        ),
        (
            "links.pdf",
            "#set page(width: 300pt, height: 200pt, margin: 20pt)\n\
             #set text(font: \"Libertinus Serif\", size: 12pt)\n\
             Read #link(<appendix>)[the appendix] or visit #link(\"https://example.org/docs\")[the website].\n\
             #pagebreak()\n\
             A middle page without links.\n\
             #pagebreak()\n\
             #v(60pt)\n\
             = Appendix <appendix>\n\
             The appendix starts part way down page three.\n\
             #pagebreak()\n\
             A closing page."
                .to_string(),
        ),
    ]
}

/// A one-page PDF with an `AcroForm`: a text field `name`, a check box
/// `agree` and a combo box `colour`, each with appearance streams.
fn form_pdf() -> Vec<u8> {
    let objects: Vec<String> = vec![
        // 1: catalog
        "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [4 0 R 5 0 R 8 0 R] /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 9 0 R >> >> >> >>".into(),
        // 2: pages
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        // 3: page
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Annots [4 0 R 5 0 R 8 0 R] /Contents 10 0 R /Resources << /Font << /Helv 9 0 R >> >> >>".into(),
        // 4: text field
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /V (Ada) /DA (/Helv 12 Tf 0 g) /Rect [100 140 280 164] /F 4 /P 3 0 R /MK << /BG [0.95 0.95 1] >> >>".into(),
        // 5: check box
        "<< /Type /Annot /Subtype /Widget /FT /Btn /T (agree) /V /Off /AS /Off /Rect [100 100 116 116] /F 4 /P 3 0 R /MK << /CA (4) >> /AP << /N << /Yes 6 0 R /Off 7 0 R >> >> >>".into(),
        // 6: check box on-appearance
        stream("<< /Type /XObject /Subtype /Form /BBox [0 0 16 16] >>", "0 g 3 3 10 10 re f"),
        // 7: check box off-appearance
        stream("<< /Type /XObject /Subtype /Form /BBox [0 0 16 16] >>", "0 G 0.5 0.5 15 15 re S"),
        // 8: combo box
        "<< /Type /Annot /Subtype /Widget /FT /Ch /Ff 131072 /T (colour) /Opt [(Red) (Green) (Blue)] /V (Red) /DA (/Helv 12 Tf 0 g) /Rect [100 60 200 80] /F 4 /P 3 0 R >>".into(),
        // 9: font
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".into(),
        // 10: page content
        stream(
            "<< >>",
            "BT /Helv 12 Tf 20 146 Td (Name) Tj 0 -40 Td (I agree) Tj 0 -40 Td (Colour) Tj ET",
        ),
    ];
    let mut out = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R /ID [<00112233445566778899AABBCCDDEEFF><00112233445566778899AABBCCDDEEFF>] >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

fn stream(dict: &str, content: &str) -> String {
    let dict = dict.trim_end_matches(">>").trim_end();
    format!(
        "{dict} /Length {} >>\nstream\n{content}\nendstream",
        content.len()
    )
}

fn main() {
    let dir = std::env::var_os("TILO_PDF_FIXTURE_OUT").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../target/pdf-fixture-candidates"),
        std::path::PathBuf::from,
    );
    std::fs::create_dir_all(&dir).expect("fixture dir");
    // Naming fixtures on the command line regenerates only those, so adding
    // a fixture does not rewrite the others.
    let only: Vec<String> = std::env::args().skip(1).collect();
    let wanted = |name: &str| only.is_empty() || only.iter().any(|o| o == name);
    let fonts = fonts(&wanted);
    for (name, src) in fixtures().into_iter().filter(|(name, _)| wanted(name)) {
        let bytes = compile(&fonts, &src);
        std::fs::write(dir.join(name), &bytes).expect("write fixture");
        println!("{name}: {} bytes", bytes.len());
    }
    if !wanted("form.pdf") {
        return;
    }
    let form = form_pdf();
    std::fs::write(dir.join("form.pdf"), &form).expect("write form fixture");
    println!("form.pdf: {} bytes", form.len());
}
