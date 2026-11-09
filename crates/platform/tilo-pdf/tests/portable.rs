//! The portable engine (hayro, in process) on the same fixtures as the
//! helper suite. It is the only engine on macOS and the web, so these run on
//! every target.

#![allow(
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::Path;

use tilo_pdf::{
    Destination, Engine, Error, FieldKind, PixelRect, RenderOptions, Rotation, SearchOptions,
};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(Path::new(env!("OUT_DIR")).join(name)).expect("fixture")
}

/// The portable engine on one of the fixtures.
fn open(name: &str) -> tilo_pdf::Document {
    Engine::portable().open_bytes(&fixture(name)).expect("open")
}

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn page_text(name: &str) -> String {
    let doc = Engine::portable().open_bytes(&fixture(name)).expect("open");
    let layer = doc.text(0).expect("text");
    layer.text_for(&layer.select_range(0..layer.len()))
}

#[test]
fn opens_with_geometry_and_metadata() {
    let doc = open("multipage.pdf");
    assert_eq!(doc.page_count(), 3);
    let size = doc.page_size(0).unwrap();
    assert!(size.width > 100.0 && size.height > 100.0, "{size:?}");
    assert!(!doc.engine().is_sandboxed());
}

#[test]
fn garbage_is_a_format_error() {
    assert!(matches!(
        Engine::portable().open_bytes(b"not a pdf at all"),
        Err(Error::Format(_))
    ));
}

#[test]
fn latin_text_copies_exactly() {
    assert_eq!(
        squash(&page_text("latin.pdf")),
        "The quick brown fox jumps over the lazy dog. Second paragraph with numbers 1234 and punctuation!"
    );
}

#[test]
fn search_finds_hits_across_pages_with_rects() {
    let doc = open("multipage.pdf");
    let hits = doc.search("needle", SearchOptions::default(), 100).unwrap();
    assert_eq!(hits.iter().map(|h| h.page).collect::<Vec<_>>(), vec![1, 2]);
    let size = doc.page_size(1).unwrap();
    for h in &hits {
        let r = h.hit.rects[0];
        assert!(
            r.x0 > 0.0 && r.x1 < size.width && r.y0 > 0.0 && r.y1 < size.height,
            "{r:?}"
        );
    }
}

#[test]
fn form_fields_are_read() {
    let doc = open("form.pdf");
    assert!(doc.info().has_form);
    let fields = doc.form_fields().unwrap();
    let get = |name: &str| {
        fields
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("{name}"))
    };
    let text = get("name");
    assert_eq!(text.kind, FieldKind::Text);
    assert_eq!(text.value, "Ada");
    assert!(!text.read_only && !text.required && !text.multiline);
    assert_eq!(text.widgets.len(), 1);
    assert_eq!(text.widgets[0].page, 0);
    // `/Rect [100 140 280 164]` on a 300×200 page, y down from the top.
    let r = text.widgets[0].rect;
    assert!(
        (r.x0 - 100.0).abs() < 0.01 && (r.x1 - 280.0).abs() < 0.01,
        "{r:?}"
    );
    assert!(
        (r.y0 - 36.0).abs() < 0.01 && (r.y1 - 60.0).abs() < 0.01,
        "{r:?}"
    );
    let agree = get("agree");
    assert_eq!(agree.kind, FieldKind::CheckBox);
    assert_eq!(agree.value, "Off");
    assert!(!agree.widgets[0].checked);
    assert_eq!(agree.widgets[0].export_value, "Yes");
    let colour = get("colour");
    assert_eq!(colour.kind, FieldKind::ComboBox);
    assert_eq!(colour.value, "Red");
    assert_eq!(colour.options, vec!["Red", "Green", "Blue"]);
}

#[test]
fn a_pdf_without_a_form_has_no_fields() {
    let doc = open("latin.pdf");
    assert!(!doc.info().has_form);
    assert!(doc.form_fields().unwrap().is_empty());
}

#[test]
fn nested_fields_inherit_their_name_kind_and_flags() {
    let doc = Engine::portable().open_bytes(&nested_form_pdf()).unwrap();
    let fields = doc.form_fields().unwrap();
    let names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, vec!["person.first", "person.last", "size"]);
    let first = &fields[0];
    assert_eq!(first.kind, FieldKind::Text);
    assert_eq!(first.value, "Ada");
    assert!(first.read_only, "the parent's /Ff applies to its kids");
    assert!(!first.required && !first.multiline);
    assert_eq!(first.widgets.len(), 1);
    assert_eq!(first.widgets[0].page, 0);
    let r = first.widgets[0].rect;
    assert!(
        (r.y0 - 60.0).abs() < 0.01 && (r.y1 - 80.0).abs() < 0.01,
        "{r:?}"
    );
    assert_eq!(fields[1].name, "person.last");
    assert_eq!(fields[1].value, "Lovelace");
    let size = &fields[2];
    assert_eq!(size.kind, FieldKind::RadioButton);
    assert_eq!(size.value, "medium");
    let states: Vec<(&str, bool)> = size
        .widgets
        .iter()
        .map(|widget| (widget.export_value.as_str(), widget.checked))
        .collect();
    assert_eq!(states, vec![("small", false), ("medium", true)]);
}

#[test]
fn outline_and_links_resolve() {
    let doc = open("multipage.pdf");
    let outline = doc.outline().unwrap();
    let titles: Vec<&str> = outline.iter().map(|e| e.title.as_str()).collect();
    assert_eq!(titles, vec!["First Chapter", "Second Chapter"]);
    assert!(matches!(
        outline[1].destination,
        Some(Destination::Page { page: 2, .. })
    ));
    assert!(
        doc.links(1)
            .unwrap()
            .iter()
            .any(|l| l.destination == Destination::Uri("https://example.org".into()))
    );
}

#[test]
fn internal_and_external_links_have_display_space_areas() {
    let doc = open("links.pdf");
    let size = doc.page_size(0).unwrap();
    let links = doc.links(0).unwrap();
    assert_eq!(links.len(), 2, "{links:?}");
    let internal = links
        .iter()
        .find(|l| matches!(l.destination, Destination::Page { page: 2, .. }))
        .expect("an internal link to the third page");
    for link in &links {
        let r = link.rect;
        assert!(r.x0 >= 0.0 && r.x1 <= size.width && r.x0 < r.x1, "{r:?}");
        assert!(r.y1 < 60.0, "first line, y down: {r:?}");
    }
    if let Destination::Page { at: Some(at), .. } = internal.destination {
        assert!(at.y > 40.0 && at.y < size.height, "{at:?}");
    }
}

#[test]
fn renders_white_pages_with_ink_and_crops_regions_from_the_full_render() {
    let doc = open("latin.pdf");
    let size = doc.page_size(0).unwrap();
    let options = RenderOptions::fit_width(size, 300);
    let full = doc.render(0, &options).unwrap();
    assert_eq!((full.width(), full.height()), options.page_pixels(size));
    let px = full.pixels();
    assert_eq!(&px[..4], &[255, 255, 255, 255], "white paper");
    assert!(
        px.chunks_exact(4).any(|p| p[0] < 128),
        "text leaves dark pixels"
    );
    let region = PixelRect {
        x: 20,
        y: 30,
        width: 64,
        height: 48,
    };
    let tile = doc
        .render(
            0,
            &RenderOptions {
                region: Some(region),
                ..options
            },
        )
        .unwrap();
    assert_eq!((tile.width(), tile.height()), (64, 48));
    let w = full.width() as usize;
    for y in 0..48usize {
        let src = ((30 + y) * w + 20) * 4;
        assert_eq!(
            &tile.pixels()[y * 64 * 4..(y + 1) * 64 * 4],
            &px[src..src + 64 * 4]
        );
    }
}

#[test]
fn an_extra_quarter_turn_swaps_the_render() {
    let doc = open("latin.pdf");
    let size = doc.page_size(0).unwrap();
    let options = RenderOptions {
        rotation: Rotation::Cw90,
        ..RenderOptions::fit_width(size, 200)
    };
    let bitmap = doc.render(0, &options).unwrap();
    assert_eq!((bitmap.width(), bitmap.height()), options.page_pixels(size));
    assert!(bitmap.height() < bitmap.width() || size.width > size.height);
}

#[test]
fn saving_edits_is_unavailable() {
    let doc = open("latin.pdf");
    assert!(matches!(
        doc.export(&tilo_pdf::Edits::default(), &[]),
        Err(Error::Unavailable(_))
    ));
}

/// A one-page PDF drawing `lines` in non-embedded Helvetica, each its own
/// `Tj` at 12 pt: the standard font whose widths come from the engine, as an
/// invoice generator writes it.
fn helvetica_pdf(lines: &[&str]) -> Vec<u8> {
    let mut stream = String::from("BT /F1 12 Tf 72 720 Td 14 TL\n");
    for line in lines {
        stream.push('(');
        stream.push_str(line);
        stream.push_str(") Tj T*\n");
    }
    stream.push_str("ET");
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>"
            .to_owned(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
            .to_owned(),
        format!(
            "<< /Length {} >>\nstream\n{stream}\nendstream",
            stream.len()
        ),
    ];
    assemble(&objects)
}

/// A PDF whose `objects` become objects 1 and up, behind a plain xref table.
fn assemble(objects: &[String]) -> Vec<u8> {
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (number, body) in (1..).zip(objects) {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let xref = pdf.len();
    let size = offsets.len().saturating_add(1);
    pdf.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    pdf
}

/// A form whose text fields are named by a parent that carries their `/FT`
/// and `/Ff`, and whose radio group is one field of two widget kids: the
/// shape a form builder writes for grouped fields.
fn nested_form_pdf() -> Vec<u8> {
    let appearance =
        "<< /Type /XObject /Subtype /Form /BBox [0 0 16 16] /Length 0 >>\nstream\n\nendstream";
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [4 0 R 7 0 R] \
         /DA (/Helv 0 Tf 0 g) >> >>"
            .to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] \
         /Annots [5 0 R 6 0 R 8 0 R 9 0 R] >>"
            .to_owned(),
        // Read only, and the kind both kids take: neither sets `/FT` itself.
        "<< /T (person) /FT /Tx /Ff 1 /Kids [5 0 R 6 0 R] >>".to_owned(),
        "<< /Type /Annot /Subtype /Widget /Parent 4 0 R /T (first) /V (Ada) \
         /Rect [40 120 160 140] /P 3 0 R >>"
            .to_owned(),
        "<< /Type /Annot /Subtype /Widget /Parent 4 0 R /T (last) /V (Lovelace) \
         /Rect [40 80 160 100] /P 3 0 R >>"
            .to_owned(),
        "<< /T (size) /FT /Btn /Ff 32768 /V /medium /Kids [8 0 R 9 0 R] >>".to_owned(),
        "<< /Type /Annot /Subtype /Widget /Parent 7 0 R /Rect [40 40 56 56] /AS /Off \
         /P 3 0 R /AP << /N << /Off 10 0 R /small 10 0 R >> >> >>"
            .to_owned(),
        "<< /Type /Annot /Subtype /Widget /Parent 7 0 R /Rect [70 40 86 56] /AS /medium \
         /P 3 0 R /AP << /N << /Off 10 0 R /medium 10 0 R >> >> >>"
            .to_owned(),
        appearance.to_owned(),
    ];
    assemble(&objects)
}

/// A narrow digit like `1` inks under half its advance; word gaps are
/// measured between advances, so numbers come out whole.
#[test]
fn narrow_digits_in_helvetica_read_without_inserted_spaces() {
    let doc = Engine::portable()
        .open_bytes(&helvetica_pdf(&[
            "Total GBP 3,412.50",
            "Ref R-2026-1187 and 1111/11",
        ]))
        .unwrap();
    let layer = doc.text(0).unwrap();
    assert_eq!(
        squash(&layer.text_for(&layer.select_range(0..layer.len()))),
        "Total GBP 3,412.50 Ref R-2026-1187 and 1111/11"
    );
}
