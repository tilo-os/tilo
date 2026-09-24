//! End-to-end tests of the sandboxed engine on real PDFs.
//!
//! The fixtures are Typst output (see `examples/gen_fixtures.rs`). Every
//! test runs against the real helper binary; a host without a pdfium
//! library skips them with a message rather than failing.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::float_cmp,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::Path;

use tilo_pdf::{
    AnnotationKind, Destination, Edits, Engine, Error, FieldKind, FieldValue, FormValue,
    NewAnnotation, NewAnnotationKind, PageEdit, PageSource, PixelRect, Point, Rect, RenderOptions,
    Rgba, Rotation, SearchOptions, TileGrid,
};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name),
    )
    .expect("fixture")
}

fn pdfium_present() -> bool {
    let mut candidates: Vec<std::path::PathBuf> = std::env::var_os("TILO_PDFIUM_LIB")
        .map(Into::into)
        .into_iter()
        .collect();
    candidates.extend(["/usr/lib64/libpdfium.so", "/usr/lib/libpdfium.so"].map(Into::into));
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(Path::new(&home).join(".local/lib/pdfium/libpdfium.so"));
    }
    candidates.iter().any(|p| p.exists())
}

fn engine() -> Option<Engine> {
    if !pdfium_present() {
        eprintln!("skipping: no pdfium library on this host (set TILO_PDFIUM_LIB)");
        return None;
    }
    match Engine::with_helper(Path::new(env!("CARGO_BIN_EXE_tilo-pdf-helper"))) {
        Ok(engine) => Some(engine),
        // A container that cannot enforce the sandbox cannot run the helper
        // at all; production refuses the same way. Say so rather than fail,
        // unless the environment promises a sandbox-capable kernel.
        Err(Error::Unavailable(message))
            if (message.contains("landlock") || message.contains("seccomp"))
                && std::env::var_os("TILO_REQUIRE_PDF_SANDBOX").is_none() =>
        {
            eprintln!("skipping: this host cannot confine the PDF helper: {message}");
            None
        }
        Err(e) => panic!("the PDF helper does not start: {e}"),
    }
}

fn page_text(engine: &Engine, name: &str) -> String {
    let doc = engine.open_bytes(&fixture(name)).expect("open");
    let layer = doc.text(0).expect("text");
    layer.text_for(&layer.select_range(0..layer.len()))
}

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn latin_text_copies_exactly() {
    let Some(engine) = engine() else { return };
    let text = page_text(&engine, "latin.pdf");
    assert_eq!(
        squash(&text),
        "The quick brown fox jumps over the lazy dog. Second paragraph with numbers 1234 and punctuation!"
    );
}

#[test]
fn ligatures_copy_as_letters_and_select_by_halves() {
    let Some(engine) = engine() else { return };
    let doc = engine.open_bytes(&fixture("ligatures.pdf")).unwrap();
    let layer = doc.text(0).unwrap();
    let all = layer.text_for(&layer.select_range(0..layer.len()));
    assert_eq!(
        squash(&all),
        "The office staff fluently shuffled fifty affine files."
    );
    assert!(
        !all.contains('\u{FB01}') && !all.contains('\u{FB03}'),
        "no presentation forms: {all:?}"
    );
    // "office": the "ffi" glyph's three letters each get their own box, so a
    // drag over the word selects exactly it.
    let hit = &layer.search("office", SearchOptions::default())[0];
    let sel = layer.select_range(hit.range.clone());
    assert_eq!(layer.text_for(&sel), "office");
    let o = layer.chars()[hit.range.start];
    let f1 = layer.chars()[hit.range.start + 1];
    let f2 = layer.chars()[hit.range.start + 2];
    assert!(
        f1.rect.x0 >= o.rect.x0 && f2.rect.x0 > f1.rect.x0,
        "ligature parts advance: {f1:?} {f2:?}"
    );
    // Dragging from the start of "o" to the middle of the ligature copies
    // only the letters actually covered.
    let from = Point::new(o.rect.x0 + 0.1, o.rect.center().y);
    let to = Point::new(f2.rect.x1 - 0.1, f2.rect.center().y);
    assert_eq!(layer.text_for(&layer.select_between(from, to)), "off");
}

#[test]
fn arabic_copies_in_logical_order() {
    let Some(engine) = engine() else { return };
    let doc = engine.open_bytes(&fixture("arabic.pdf")).unwrap();
    let layer = doc.text(0).unwrap();
    let all = layer.text_for(&layer.select_range(0..layer.len()));
    assert_eq!(squash(&all), "مرحبا بالعالم السلام عليكم");
    assert!(
        layer.lines().iter().all(|l| l.rtl),
        "lines are right-to-left"
    );
    // The first word in reading order is the rightmost on the first line.
    let hit = &layer.search("مرحبا", SearchOptions::default())[0];
    let word = hit.rects[0];
    let line = layer.lines()[0].rect;
    assert!(
        (word.x1 - line.x1).abs() < 2.0,
        "first word sits at the right edge"
    );
}

#[test]
fn cjk_copies_exactly_and_selects_per_character() {
    let Some(engine) = engine() else { return };
    let doc = engine.open_bytes(&fixture("cjk.pdf")).unwrap();
    let layer = doc.text(0).unwrap();
    let all = layer.text_for(&layer.select_range(0..layer.len()));
    assert_eq!(
        squash(&all),
        "你好，世界。这是一个测试。 日本語のテキストも含みます。"
    );
    let hit = &layer.search("世界", SearchOptions::default())[0];
    assert_eq!(hit.range.len(), 2);
    assert_eq!(hit.rects.len(), 1);
}

#[test]
fn two_columns_read_and_select_column_by_column() {
    let Some(engine) = engine() else { return };
    let doc = engine.open_bytes(&fixture("columns.pdf")).unwrap();
    let layer = doc.text(0).unwrap();
    let text = squash(layer.text());
    let pos = |needle: &str| {
        text.find(needle)
            .unwrap_or_else(|| panic!("{needle} in {text}"))
    };
    assert!(pos("Two Column Title") < pos("Alpha"));
    assert!(pos("Alpha") < pos("Beta"), "{text}");
    assert!(
        pos("Beta") < pos("Gamma"),
        "left column before right: {text}"
    );
    assert!(pos("Gamma") < pos("Delta"));
    // A drag from "Alpha" down to "Beta" (left column) never includes the
    // right column even though its lines sit between them vertically.
    let alpha = &layer.search("Alpha", SearchOptions::default())[0];
    let beta = &layer.search("column.", SearchOptions::default())[0];
    let a = alpha.rects[0];
    let b = beta.rects.last().copied().unwrap();
    let sel = layer.select_between(
        Point::new(a.x0 + 0.1, a.center().y),
        Point::new(b.x1 - 0.1, b.center().y),
    );
    let copied = layer.text_for(&sel);
    assert!(copied.starts_with("Alpha"), "{copied}");
    assert!(!copied.contains("Gamma"), "{copied}");
}

#[test]
fn search_finds_hits_across_pages_with_rects() {
    let Some(engine) = engine() else { return };
    let doc = engine.open_bytes(&fixture("multipage.pdf")).unwrap();
    assert_eq!(doc.page_count(), 3);
    let hits = doc.search("needle", SearchOptions::default(), 100).unwrap();
    assert_eq!(hits.iter().map(|h| h.page).collect::<Vec<_>>(), vec![1, 2]);
    let size = doc.page_size(1).unwrap();
    for h in &hits {
        assert_eq!(h.hit.rects.len(), 1);
        let r = h.hit.rects[0];
        assert!(
            r.x0 > 0.0 && r.x1 < size.width && r.y0 > 0.0 && r.y1 < size.height,
            "{r:?}"
        );
    }
}

#[test]
fn outline_and_links_resolve() {
    let Some(engine) = engine() else { return };
    let doc = engine.open_bytes(&fixture("multipage.pdf")).unwrap();
    let outline = doc.outline().unwrap();
    let titles: Vec<&str> = outline.iter().map(|e| e.title.as_str()).collect();
    assert_eq!(titles, vec!["First Chapter", "Second Chapter"]);
    assert!(matches!(
        outline[1].destination,
        Some(Destination::Page { page: 2, .. })
    ));
    let links = doc.links(1).unwrap();
    assert!(
        links
            .iter()
            .any(|l| l.destination == Destination::Uri("https://example.org".into())),
        "{links:?}"
    );
}

#[test]
fn internal_and_external_links_resolve_with_their_areas() {
    let Some(engine) = engine() else { return };
    let doc = engine.open_bytes(&fixture("links.pdf")).unwrap();
    let size = doc.page_size(0).unwrap();
    let links = doc.links(0).unwrap();
    assert_eq!(links.len(), 2, "{links:?}");
    let (internal, page, at) = links
        .iter()
        .find_map(|l| match l.destination {
            Destination::Page { page, at } => Some((l, page, at)),
            _ => None,
        })
        .expect("an internal link");
    assert_eq!(page, 2, "the appendix is on the third page");
    // The heading sits part way down its page, in display space (y down).
    let at = at.expect("the destination names a point");
    assert!(at.y > 40.0 && at.y < size.height, "{at:?}");
    let external = links
        .iter()
        .find(|l| l.destination == Destination::Uri("https://example.org/docs".into()))
        .expect("an external link");
    for link in [internal, external] {
        let r = link.rect;
        assert!(r.x0 >= 0.0 && r.x1 <= size.width && r.x0 < r.x1, "{r:?}");
        assert!(r.y0 >= 0.0 && r.y1 <= size.height && r.y0 < r.y1, "{r:?}");
        // First line of the page, in display space.
        assert!(r.y1 < 60.0, "{r:?}");
    }
    assert!(internal.rect.x1 <= external.rect.x0, "reading order");
    assert!(doc.links(1).unwrap().is_empty());
}

#[test]
fn render_region_matches_the_full_render() {
    let Some(engine) = engine() else { return };
    let doc = engine.open_bytes(&fixture("latin.pdf")).unwrap();
    let size = doc.page_size(0).unwrap();
    let options = RenderOptions::fit_width(size, 600);
    let full = doc.render(0, &options).unwrap();
    assert_eq!(full.width(), 600);
    assert_eq!(
        full.pixels().len(),
        (full.width() * full.height() * 4) as usize
    );
    assert!(
        full.pixels().chunks(4).any(|p| p[0] < 128),
        "some ink on the page"
    );
    let grid = TileGrid { tile: 128 };
    let tiles = grid.tiles(
        size,
        &options,
        PixelRect {
            x: 0,
            y: 0,
            width: 600,
            height: 600,
        },
    );
    let tile = tiles.iter().find(|t| t.col == 1 && t.row == 0).unwrap();
    let part = doc
        .render_tile(0, options.scale, Rotation::None, tile)
        .unwrap();
    assert_eq!(
        (part.width(), part.height()),
        (tile.region.width, tile.region.height)
    );
    let mut differing = 0usize;
    for y in 0..part.height() {
        for x in 0..part.width() {
            let a = ((y * part.width() + x) * 4) as usize;
            let b = (((y + tile.region.y) * full.width() + x + tile.region.x) * 4) as usize;
            if part.pixels()[a..a + 4] != full.pixels()[b..b + 4] {
                differing += 1;
            }
        }
    }
    // Anti-aliasing at tile seams may differ by a pixel column.
    assert!(
        differing <= part.height() as usize * 2,
        "{differing} pixels differ"
    );
    let rotated = doc
        .render(
            0,
            &RenderOptions {
                rotation: Rotation::Cw90,
                ..options
            },
        )
        .unwrap();
    assert_eq!(
        (rotated.width(), rotated.height()),
        (full.height(), full.width())
    );
}

#[test]
fn form_fields_are_read() {
    let Some(engine) = engine() else { return };
    let doc = engine.open_bytes(&fixture("form.pdf")).unwrap();
    assert!(doc.info().has_form);
    let fields = doc.form_fields().unwrap();
    let get = |name: &str| {
        fields
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("{name}"))
    };
    assert_eq!(get("name").kind, FieldKind::Text);
    assert_eq!(get("name").value, "Ada");
    assert_eq!(get("agree").kind, FieldKind::CheckBox);
    assert!(!get("agree").widgets[0].checked);
    assert_eq!(get("colour").kind, FieldKind::ComboBox);
    assert_eq!(get("colour").options, vec!["Red", "Green", "Blue"]);
}

#[test]
fn the_sandbox_refuses_files_sockets_and_exec() {
    let Some(engine) = engine() else { return };
    assert!(engine.is_sandboxed());
    let dir = tempfile::tempdir().unwrap();
    let secret = dir.path().join("secret.txt");
    std::fs::write(&secret, "private").unwrap();
    // The helper runs as the same user, so only the sandbox stands between
    // it and this file.
    assert!(std::fs::read(&secret).is_ok());
    let report = engine
        .probe_sandbox(secret.to_str().unwrap())
        .unwrap()
        .expect("sandboxed engine reports");
    assert!(!report.opened_file, "helper read a file outside its grant");
    assert!(!report.created_file, "helper created a file");
    assert!(!report.inet_socket, "helper opened a network socket");
    assert!(!report.unix_socket, "helper opened a local socket");
    assert!(!report.exec, "helper ran another program");
    // And it still renders afterwards: the probe did not break the helper.
    let doc = engine.open_bytes(&fixture("latin.pdf")).unwrap();
    assert!(doc.render(0, &RenderOptions::default()).is_ok());
}

#[test]
fn a_crashed_helper_is_an_error_and_restarts() {
    let Some(engine) = engine() else { return };
    let doc = engine.open_bytes(&fixture("multipage.pdf")).unwrap();
    assert!(doc.render(0, &RenderOptions::default()).is_ok());
    let first = engine.helper_pid().expect("helper running");
    // The helper dies while handling a request: that request fails...
    let err = engine.crash_helper_for_test().expect("sandboxed engine");
    assert!(matches!(err, Error::Crashed(_)), "{err:?}");
    assert!(err.is_transient());
    // ...and the next one starts a new helper and re-opens the document.
    let bitmap = doc
        .render(1, &RenderOptions::default())
        .expect("restarted helper renders");
    assert!(bitmap.width() > 0);
    assert_eq!(engine.helpers_started(), 2);
    assert_ne!(engine.helper_pid(), Some(first));
    assert_eq!(
        doc.text(2)
            .unwrap()
            .search("needle", SearchOptions::default())
            .len(),
        1
    );
}

#[test]
fn a_helper_killed_between_requests_is_replaced() {
    let Some(engine) = engine() else { return };
    let doc = engine.open_bytes(&fixture("latin.pdf")).unwrap();
    assert!(doc.render(0, &RenderOptions::default()).is_ok());
    let pid = engine.helper_pid().expect("helper running");
    // SAFETY: signalling the helper child this test's engine started.
    assert_eq!(unsafe { libc::kill(pid.cast_signed(), libc::SIGKILL) }, 0);
    // A request racing the kill may see the helper die under it; at most
    // that one fails, and the document keeps working.
    let outcomes: Vec<bool> = (0..3)
        .map(|_| doc.render(0, &RenderOptions::default()).is_ok())
        .collect();
    assert!(outcomes[1] && outcomes[2], "{outcomes:?}");
    assert_ne!(engine.helper_pid(), Some(pid));
}

#[test]
fn garbage_is_a_format_error_not_a_crash() {
    let Some(engine) = engine() else { return };
    let err = engine
        .open_bytes(b"%PDF-1.7\nthis is not a pdf")
        .expect_err("garbage");
    assert!(matches!(err, Error::Format(_)), "{err:?}");
    assert!(engine.open_bytes(&fixture("latin.pdf")).is_ok());
}

fn user_quads(doc: &tilo_pdf::Document, page: usize, needle: &str) -> Vec<Rect> {
    let layer = doc.text(page).unwrap();
    let geometry = doc.page(page).unwrap();
    layer.search(needle, SearchOptions::default())[0]
        .rects
        .iter()
        .map(|r| geometry.rect_to_user(*r))
        .collect()
}

#[test]
fn export_round_trips_annotations_forms_and_page_operations() {
    let Some(engine) = engine() else { return };
    let source = fixture("multipage.pdf");
    let doc = engine.open_bytes(&source).unwrap();
    let quads = user_quads(&doc, 2, "needle");
    let g2 = doc.page(2).unwrap();
    let edits = Edits {
        // Page three first (rotated), then page one; page two deleted.
        pages: vec![
            PageEdit {
                source: PageSource::Original(2),
                rotation: Rotation::Cw90,
            },
            PageEdit {
                source: PageSource::Original(0),
                rotation: Rotation::None,
            },
        ],
        annotations: vec![
            NewAnnotation {
                page: 0,
                kind: NewAnnotationKind::Highlight {
                    quads: quads.clone(),
                },
                color: Rgba::new(255, 220, 0, 255),
                author: "Ada".into(),
                contents: String::new(),
                name: "hl-1".into(),
                modified: "D:20260924120000Z".into(),
            },
            NewAnnotation {
                page: 0,
                kind: NewAnnotationKind::Note {
                    at: g2.to_user(Point::new(20.0, 20.0)),
                },
                color: Rgba::new(255, 200, 0, 255),
                author: "Ada".into(),
                contents: "Check this".into(),
                name: "note-1".into(),
                modified: String::new(),
            },
            NewAnnotation {
                page: 1,
                kind: NewAnnotationKind::Ink {
                    strokes: vec![vec![
                        Point::new(30.0, 30.0),
                        Point::new(60.0, 50.0),
                        Point::new(90.0, 30.0),
                    ]],
                    width: 2.0,
                },
                color: Rgba::new(0, 0, 255, 255),
                author: "Ada".into(),
                contents: String::new(),
                name: "ink-1".into(),
                modified: String::new(),
            },
        ],
        form_values: Vec::new(),
    };
    let out = doc.export(&edits, &[]).unwrap();
    assert!(
        out.starts_with(&source),
        "an incremental update keeps the original bytes"
    );
    let again = doc.export(&edits, &[]).unwrap();
    assert_eq!(out, again, "export is deterministic");
    // A second engine is another helper process, with its own heap and
    // address layout: nothing host- or run-dependent reaches the bytes.
    let other = self::engine().unwrap();
    let elsewhere = other
        .open_bytes(&source)
        .unwrap()
        .export(&edits, &[])
        .unwrap();
    assert_eq!(out, elsewhere, "export is deterministic across processes");

    let reloaded = engine.open_bytes(&out).unwrap();
    assert_eq!(reloaded.page_count(), 2);
    assert_eq!(reloaded.page(0).unwrap().rotation, Rotation::Cw90);
    assert_eq!(reloaded.page(1).unwrap().rotation, Rotation::None);
    assert_eq!(
        reloaded
            .text(0)
            .unwrap()
            .search("needle", SearchOptions::default())
            .len(),
        1
    );
    let first = reloaded.annotations(0).unwrap();
    let highlight = first
        .iter()
        .find(|a| a.kind == AnnotationKind::Highlight)
        .expect("highlight");
    assert_eq!(highlight.name, "hl-1");
    assert_eq!(highlight.author, "Ada");
    assert_eq!(highlight.quads.len(), quads.len());
    let note = first
        .iter()
        .find(|a| a.kind == AnnotationKind::Text)
        .expect("note");
    assert_eq!(note.contents, "Check this");
    let second = reloaded.annotations(1).unwrap();
    let ink = second
        .iter()
        .find(|a| a.kind == AnnotationKind::Ink)
        .expect("ink");
    assert_eq!(ink.ink.len(), 1);
    assert_eq!(ink.ink[0].len(), 3);
    // The highlight's appearance was written into the file: pdfium draws it
    // (yellow over the word) after reload.
    let size = reloaded.page_size(0).unwrap();
    let bitmap = reloaded
        .render(0, &RenderOptions::fit_width(size, 400))
        .unwrap();
    let yellow = bitmap
        .pixels()
        .chunks(4)
        .filter(|p| p[0] > 200 && p[1] > 180 && p[2] < 120)
        .count();
    assert!(
        yellow > 50,
        "highlight visible after reload ({yellow} yellow pixels)"
    );
}

#[test]
fn stamps_carry_their_own_appearance() {
    let Some(engine) = engine() else { return };
    let doc = engine.open_bytes(&fixture("latin.pdf")).unwrap();
    let edits = Edits {
        annotations: vec![NewAnnotation {
            page: 0,
            kind: NewAnnotationKind::Stamp {
                rect: Rect::from_xywh(40.0, 20.0, 160.0, 50.0),
                strokes: vec![vec![
                    Point::new(45.0, 40.0),
                    Point::new(120.0, 60.0),
                    Point::new(190.0, 35.0),
                ]],
                stroke_width: 2.0,
                text: vec![tilo_pdf::StampText {
                    at: Point::new(45.0, 24.0),
                    size: 9.0,
                    text: "Ada Lovelace".into(),
                }],
            },
            color: Rgba::new(20, 30, 200, 255),
            author: "Ada".into(),
            contents: "Signed".into(),
            name: "sig-1".into(),
            modified: String::new(),
        }],
        ..Edits::default()
    };
    let out = doc.export(&edits, &[]).unwrap();
    let reloaded = engine.open_bytes(&out).unwrap();
    let annots = reloaded.annotations(0).unwrap();
    assert!(
        annots
            .iter()
            .any(|a| a.kind == AnnotationKind::Stamp && a.name == "sig-1")
    );
    let size = reloaded.page_size(0).unwrap();
    let bitmap = reloaded
        .render(0, &RenderOptions::fit_width(size, 600))
        .unwrap();
    let blue = bitmap
        .pixels()
        .chunks(4)
        .filter(|p| p[2] > 150 && p[0] < 100 && p[1] < 120)
        .count();
    assert!(
        blue > 40,
        "stamp strokes drawn after reload ({blue} blue pixels)"
    );
}

#[test]
fn export_writes_real_form_values_deterministically_across_helpers() {
    let Some(engine) = engine() else { return };
    let source = fixture("form.pdf");
    let edits = Edits {
        form_values: vec![
            FormValue {
                name: "name".into(),
                value: FieldValue::Text("Grace Hopper".into()),
            },
            FormValue {
                name: "agree".into(),
                value: FieldValue::Checked {
                    on: true,
                    export: None,
                },
            },
            FormValue {
                name: "colour".into(),
                value: FieldValue::Choice("Blue".into()),
            },
        ],
        ..Edits::default()
    };
    let out = engine
        .open_bytes(&source)
        .unwrap()
        .export(&edits, &[])
        .unwrap();
    // A second engine is a different helper process with a different heap.
    let other = self::engine().unwrap();
    let out2 = other
        .open_bytes(&source)
        .unwrap()
        .export(&edits, &[])
        .unwrap();
    assert_eq!(out, out2, "byte-identical across processes");
    let reloaded = engine.open_bytes(&out).unwrap();
    let fields = reloaded.form_fields().unwrap();
    let get = |name: &str| fields.iter().find(|f| f.name == name).unwrap();
    assert_eq!(get("name").value, "Grace Hopper");
    assert!(get("agree").widgets[0].checked);
    assert_eq!(get("colour").value, "Blue");
}

#[test]
fn export_imports_pages_from_other_documents() {
    let Some(engine) = engine() else { return };
    let base = engine.open_bytes(&fixture("latin.pdf")).unwrap();
    let other = engine.open_bytes(&fixture("cjk.pdf")).unwrap();
    let edits = Edits {
        pages: vec![
            PageEdit {
                source: PageSource::Original(0),
                rotation: Rotation::None,
            },
            PageEdit {
                source: PageSource::Imported { import: 0, page: 0 },
                rotation: Rotation::None,
            },
            PageEdit {
                source: PageSource::Original(0),
                rotation: Rotation::Cw180,
            },
        ],
        ..Edits::default()
    };
    let out = base.export(&edits, &[&other]).unwrap();
    let reloaded = engine.open_bytes(&out).unwrap();
    assert_eq!(reloaded.page_count(), 3);
    assert!(squash(reloaded.text(1).unwrap().text()).starts_with("你好"));
    assert!(squash(reloaded.text(2).unwrap().text()).starts_with("The quick"));
    assert_eq!(reloaded.page(2).unwrap().rotation, Rotation::Cw180);
}

#[test]
fn images_open_as_one_page_documents() {
    let Some(engine) = engine() else { return };
    let mut png = Vec::new();
    let img = image::RgbaImage::from_pixel(40, 20, image::Rgba([200, 30, 30, 255]));
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    let doc = engine.open_image_bytes(&png).unwrap();
    assert_eq!(doc.page_count(), 1);
    let size = doc.page_size(0).unwrap();
    assert_eq!(size.width, 612.0);
    assert!((size.height - 306.0).abs() < 1.0);
    let bitmap = doc.render(0, &RenderOptions::fit_width(size, 100)).unwrap();
    let px = &bitmap.pixels()[(bitmap.width() * 4 * (bitmap.height() / 2) + 200) as usize..][..4];
    assert!(px[0] > 150 && px[1] < 80, "image colour rendered: {px:?}");
}

#[test]
fn in_process_and_sandboxed_engines_agree() {
    let Some(engine) = engine() else { return };
    let local = Engine::in_process().unwrap();
    for name in ["latin.pdf", "arabic.pdf", "columns.pdf"] {
        let a = engine.open_bytes(&fixture(name)).unwrap();
        let b = local.open_bytes(&fixture(name)).unwrap();
        assert_eq!(a.raw_text(0).unwrap(), b.raw_text(0).unwrap(), "{name}");
        let o = RenderOptions::default();
        assert_eq!(
            a.render(0, &o).unwrap().pixels(),
            b.render(0, &o).unwrap().pixels(),
            "{name}"
        );
    }
}
