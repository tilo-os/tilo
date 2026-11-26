//! The device's typesetting, end to end: a source compiled, a diagnostic's span, a page drawn as
//! a PNG, its text, the outline, the PDF, a plugin that runs out of budget, and the clock.

use crate::{Clock, DiagKind, Engine, civil_from_days};

fn clock() -> Clock {
    Clock::at("2026-06-30", 1_782_400_000).expect("the test's day is a date")
}

#[test]
fn a_document_typesets_to_pages_a_pdf_and_its_sizes() {
    let engine = Engine::shared();
    let compiled = engine.compile(
        "#set page(width: 300pt, height: 400pt)\n= Note\n\nA body paragraph, typeset.\n",
        clock(),
    );
    assert!(compiled.ok, "{:?}", compiled.diagnostics);
    assert!(
        compiled.diagnostics.is_empty(),
        "{:?}",
        compiled.diagnostics
    );
    assert_eq!(compiled.pages.len(), 1, "one page");
    let page = compiled.pages.first().expect("a page");
    assert!((page.width - 300.0).abs() < 0.5, "the page's own width");
    assert!((page.height - 400.0).abs() < 0.5, "the page's own height");
    assert_eq!(page.look.len(), 32, "a page names what it draws");
    let pdf = compiled.pdf.expect("a clean compile writes a PDF");
    assert!(pdf.starts_with(b"%PDF"), "the PDF is a PDF");
    assert!(pdf.len() > 400, "a real page is more than a stub");
}

#[test]
fn an_error_and_a_warning_carry_spans_on_their_lines() {
    let engine = Engine::shared();
    let compiled = engine.compile("Text #missing_var more.\n", clock());
    assert!(!compiled.ok, "an unknown variable does not typeset");
    let error = compiled
        .diagnostics
        .iter()
        .find(|d| d.kind == DiagKind::Error)
        .expect("an error");
    assert_eq!(error.line, Some(1), "on the first line");
    assert!(error.column.is_some() && error.end_column.is_some());
    assert!(
        error.end_column > error.column,
        "the span is wider than nothing"
    );
    assert!(compiled.pdf.is_none(), "a broken source writes no PDF");

    let both = engine.compile(
        "#set text(font: \"No Such Face\")\n= Heading\n\n#undefined_thing\n",
        clock(),
    );
    let warning = both
        .diagnostics
        .iter()
        .find(|d| d.kind == DiagKind::Warning)
        .expect("the font warning");
    let error = both
        .diagnostics
        .iter()
        .find(|d| d.kind == DiagKind::Error)
        .expect("the blocking error");
    assert_ne!(warning.line, error.line, "the two stand on different lines");
    assert_eq!(
        both.diagnostics.first().map(|d| d.kind),
        Some(DiagKind::Error),
        "the blocking problem leads"
    );
}

#[test]
fn a_page_draws_as_a_png_at_the_width_it_is_asked_for() {
    let engine = Engine::shared();
    let source = "#set page(width: 300pt, height: 400pt)\n= Drawn\n\nSome words.\n";
    let png = engine
        .picture(source, 1, 600, clock())
        .expect("the page draws");
    assert_eq!(
        png.get(..8),
        Some(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a][..]),
        "a PNG's signature"
    );
    let picture = image::load_from_memory(&png).expect("the picture decodes");
    assert_eq!(picture.width(), 600, "as wide as it was asked for");
    assert_eq!(picture.height(), 800, "its height follows the page's shape");
    let error = engine
        .picture(source, 2, 600, clock())
        .expect_err("there is no second page");
    assert!(error.contains("no page 2"), "{error}");
}

#[test]
fn a_page_s_text_is_its_glyphs_in_reading_order() {
    let engine = Engine::shared();
    let source = "= Hello\n\nSome body text here.\n";
    let cells = engine.text(source, 1, clock()).expect("the page's words");
    assert!(!cells.is_empty(), "the page has glyphs");
    let said: String = cells.iter().map(|cell| cell.text.as_str()).collect();
    assert!(said.contains("Hello"), "the heading is read: {said}");
    for pair in cells.windows(2) {
        if pair.len() == 2 {
            let (left, right) = (&pair[0], &pair[1]);
            if left.line == right.line {
                assert!(right.x0 + 1e-6 >= left.x0, "cells advance rightward");
            }
        }
    }
    assert!(
        cells.iter().all(|cell| cell.bottom > cell.top),
        "a line box has a positive height"
    );
}

#[test]
fn the_outline_is_read_without_typesetting() {
    let engine = Engine::shared();
    let headings = engine.outline("= One\n\n=== Deep\n== Two\n");
    assert_eq!(headings.len(), 3, "{headings:?}");
    assert_eq!(headings.first().map(|h| h.line), Some(1));
    assert_eq!(headings.get(1).map(|h| h.level), Some(3));
}

#[test]
fn a_plugin_that_never_returns_stops_with_a_message_and_the_next_document_typesets() {
    let module = wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (func (export "spin") (param i32) (result i32)
                (loop $forever (br $forever))
                (i32.const 0)))"#,
    )
    .expect("the plugin is written as WAT");
    let bytes = module
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let engine = Engine::shared();
    let started = std::time::Instant::now();
    let compiled = engine.compile(
        &format!("#let p = plugin(bytes(({bytes})))\n#p.spin(bytes(()))\n"),
        clock(),
    );
    assert!(
        !compiled.ok,
        "a plugin that never returns fails the compile"
    );
    assert!(
        compiled
            .diagnostics
            .iter()
            .any(|d| d.message.contains("did not finish")),
        "the error says the plugin ran out of budget: {:?}",
        compiled.diagnostics
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(120),
        "the compile stopped in bounded time"
    );

    let after = engine.compile("= Hello\n\nA body paragraph.\n", clock());
    assert!(
        after.ok,
        "the engine is usable after a plugin failed: {:?}",
        after.diagnostics
    );
}

#[test]
fn today_is_the_person_s_own_and_an_offset_shifts_it() {
    let engine = Engine::shared();
    let compiled = engine.compile("Dated #datetime.today().display()\n", clock());
    assert!(compiled.ok, "{:?}", compiled.diagnostics);
    assert_eq!(compiled.pages.len(), 1);

    assert_eq!(civil_from_days(0), (1970, 1, 1));
    assert_eq!(civil_from_days(-1), (1969, 12, 31));
    assert_eq!(civil_from_days(11_016), (2000, 2, 29), "a leap day");
    assert_eq!(civil_from_days(20_634), (2026, 6, 30));
}

#[test]
fn a_source_that_does_not_typeset_says_why_when_a_page_is_drawn() {
    let engine = Engine::shared();
    let error = engine
        .picture("Body #undefined_var end.\n", 1, 400, clock())
        .expect_err("a broken source draws no page");
    assert!(
        error.contains("undefined") || error.contains("unknown"),
        "the refusal is the compiler's own words: {error}"
    );
    assert!(
        engine.picture("= Fine\n\nBody.\n", 1, 400, clock()).is_ok(),
        "the engine draws the next document"
    );
}
