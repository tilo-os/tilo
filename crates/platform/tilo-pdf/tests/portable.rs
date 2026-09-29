//! The portable engine (hayro, in process) on the same fixtures as the
//! helper suite. It is the only engine on macOS and the web, so these run on
//! every target.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::Path;

use tilo_pdf::{Destination, Engine, Error, PixelRect, RenderOptions, Rotation, SearchOptions};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name),
    )
    .expect("fixture")
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
    let doc = Engine::portable()
        .open_bytes(&fixture("multipage.pdf"))
        .unwrap();
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
    let doc = Engine::portable()
        .open_bytes(&fixture("multipage.pdf"))
        .unwrap();
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
fn outline_and_links_resolve() {
    let doc = Engine::portable()
        .open_bytes(&fixture("multipage.pdf"))
        .unwrap();
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
    let doc = Engine::portable()
        .open_bytes(&fixture("links.pdf"))
        .unwrap();
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
    let doc = Engine::portable()
        .open_bytes(&fixture("latin.pdf"))
        .unwrap();
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
    let doc = Engine::portable()
        .open_bytes(&fixture("latin.pdf"))
        .unwrap();
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
    let doc = Engine::portable()
        .open_bytes(&fixture("latin.pdf"))
        .unwrap();
    assert!(matches!(
        doc.export(&tilo_pdf::Edits::default(), &[]),
        Err(Error::Unavailable(_))
    ));
}
