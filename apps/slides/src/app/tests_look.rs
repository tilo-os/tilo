//! A deck's look is document content: its light or dark surfaces and inks
//! are stored with the deck, and the canvas, Present and a written file all
//! draw them, whatever the reader's UI theme.

use super::bench::{Bench, WINDOW};
use super::*;
use crate::model::DeckLook;
use ui::offscreen::OffscreenRenderer;

/// The pixels of `area` (logical px) out of a frame `frame_w` device pixels
/// wide, at `scale` device pixels per logical one.
fn crop(rgba: &[u8], frame_w: usize, scale: f64, area: Rect) -> Vec<u8> {
    let px = |v: f64| (v * scale).round() as usize;
    let (x0, x1, y0, y1) = (px(area.x0), px(area.x1), px(area.y0), px(area.y1));
    let mut out = Vec::with_capacity((x1 - x0) * (y1 - y0) * 4);
    for y in y0..y1 {
        let row = (y * frame_w + x0) * 4;
        out.extend_from_slice(&rgba[row..row + (x1 - x0) * 4]);
    }
    out
}

/// The window's frame under UI theme `theme`, with its width and device
/// scale.
fn frame_in(bench: &mut Bench, theme: Theme) -> (Vec<u8>, usize, f64) {
    bench.app.harness().set_theme(theme);
    bench.frame();
    let rgba = bench.app.harness().render_rgba();
    let (w, h) = (f64::from(WINDOW.0), f64::from(WINDOW.1));
    let scale = ((rgba.len() / 4) as f64 / (w * h)).sqrt();
    (rgba, (w * scale).round() as usize, scale)
}

/// The slide on the canvas, inside its rounded card edge.
fn slide_area(bench: &mut Bench) -> Rect {
    bench.read(|app| app.compute_chrome().slide_dest.inset(-16.0))
}

/// How many of the slide's pixels differ between the dark and the light UI
/// theme, past antialiasing.
fn slide_differs_across_ui_themes(bench: &mut Bench, area: impl Fn(&mut Bench) -> Rect) -> usize {
    let (dark, w, scale) = frame_in(bench, Theme::dark());
    let dark = crop(&dark, w, scale, area(bench));
    let (light, w, scale) = frame_in(bench, Theme::light());
    let light = crop(&light, w, scale, area(bench));
    assert_eq!(dark.len(), light.len());
    OffscreenRenderer::pixels_differing_beyond_aa(&dark, &light)
}

// A deck's look is document content: the same on every screen, in Present
// and in the exported file, whatever the UI theme.
#[test]
fn a_deck_looks_the_same_in_the_dark_and_the_light_ui_theme() {
    let deck = crate::model::seed_deck();
    for look in DeckLook::ALL {
        let deck = crate::model::Deck {
            look,
            ..deck.clone()
        };
        let mut bench = Bench::open(&deck, WINDOW);
        for index in 0..deck.slides.len() {
            bench.with(|app| {
                app.cur_slide = index;
                app.sel = None;
            });
            let differing = slide_differs_across_ui_themes(&mut bench, slide_area);
            assert_eq!(
                differing,
                0,
                "slide {index} of a {} deck draws the same on the canvas in either UI theme",
                look.as_str()
            );
        }
        // Present draws the deck's slide the same way.
        bench.with(SlidesApp::enter_present);
        let differing = slide_differs_across_ui_themes(&mut bench, |_| {
            let (w, h) = (f64::from(WINDOW.0), f64::from(WINDOW.1));
            Rect::new(w * 0.15, h * 0.15, w * 0.85, h * 0.85)
        });
        assert_eq!(
            differing,
            0,
            "a {} deck presents the same in either UI theme",
            look.as_str()
        );
    }
}

/// The look is the deck's own: switched in the inspector, it is stored in
/// the document, and the slide is drawn in it.
#[test]
fn the_deck_look_is_stored_in_the_document_and_drawn() {
    let mut bench = Bench::sample();
    assert_eq!(bench.deck().look, DeckLook::Light, "a deck starts light");
    let surface = bench
        .deck()
        .slides
        .iter()
        .position(|slide| slide.background == Background::Surface)
        .expect("the sample deck has a slide on the deck's surface");
    bench.with(|app| {
        app.cur_slide = surface;
        app.sel = None;
    });
    let (light, w, scale) = frame_in(&mut bench, Theme::dark());
    let area = slide_area(&mut bench);
    let light = crop(&light, w, scale, area);
    bench.with(|app| app.apply_hit(Hit::SetLook(DeckLook::Dark)));
    bench.frame();
    assert_eq!(
        bench.deck().look,
        DeckLook::Dark,
        "the look is document content"
    );
    assert_eq!(
        bench.read(|app| app.deck.look),
        DeckLook::Dark,
        "the editor follows the document"
    );
    let (dark, w, scale) = frame_in(&mut bench, Theme::dark());
    let dark = crop(&dark, w, scale, area);
    assert!(
        OffscreenRenderer::pixels_differing_beyond_aa(&light, &dark) > light.len() / 4 / 8,
        "the slide is drawn in the deck's look"
    );
}

/// A written file carries the deck's colours as Slides draws them, and
/// opens back in the same look.
#[test]
fn a_written_deck_carries_its_look_and_colours() {
    use std::io::Read;
    let content = tilo_content::ContentStore::memory();
    for look in DeckLook::ALL {
        let deck = crate::model::Deck {
            look,
            palette: PaletteKind::Emerald,
            ..crate::model::seed_deck()
        };
        let palette = Palette::resolve(deck.palette, look);
        let hex = |color| ui::color::to_hex(color);

        let pptx = crate::pptx::write_pptx(&deck, &content).unwrap();
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&pptx)).unwrap();
        let mut theme = String::new();
        archive
            .by_name("ppt/theme/theme1.xml")
            .unwrap()
            .read_to_string(&mut theme)
            .unwrap();
        for (slot, color) in [
            ("lt1", palette.surface),
            ("dk1", palette.text),
            ("accent1", palette.accent),
            ("accent6", palette.deep),
        ] {
            let rgb = hex(color).trim_start_matches('#').to_owned();
            assert!(
                theme.contains(&format!(r#"<a:{slot}><a:srgbClr val="{rgb}"/>"#)),
                "the {} theme's {slot} is the deck's colour {rgb}: {theme}",
                look.as_str()
            );
        }
        let back = crate::pptx::read_pptx(&pptx, &content).unwrap();
        assert_eq!(
            back.look, look,
            "a .pptx opens in the look it was written in"
        );
        assert_eq!(back.palette, PaletteKind::Emerald);

        let odp = crate::odp::write_odp(&deck, &content).unwrap();
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&odp)).unwrap();
        let mut xml = String::new();
        archive
            .by_name("content.xml")
            .unwrap()
            .read_to_string(&mut xml)
            .unwrap();
        for slide in &deck.slides {
            let ground = palette.on(slide.background).slide_bg(slide.background);
            assert!(
                xml.contains(&format!(r#"draw:fill-color="{}""#, hex(ground))),
                "the page of a {} slide is filled with its ground",
                slide.background.as_str()
            );
            for element in slide.elements.iter().filter(|e| !e.text.is_empty()) {
                let ink = crate::render::text_ink(&palette.on(slide.background), slide, element);
                assert!(
                    xml.contains(&format!(r#"fo:color="{}""#, hex(ink))),
                    "{:?} is written in the ink Slides draws it in",
                    element.text
                );
            }
        }
        let back = crate::odp::read_odp(&odp, &content).unwrap();
        assert_eq!(
            back.look, look,
            "a .odp opens in the look it was written in"
        );
    }
}
