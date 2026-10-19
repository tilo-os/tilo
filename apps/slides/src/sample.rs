//! The decks screenshot scenarios and tests open: the sample deck grown long,
//! and with a picture. Never opened on a live launch.

use tilo_content::ContentStore;

use crate::model::{Deck, Element, ElementKind, Layout, Slide, new_id, seed_deck};

/// How many slides the long sample deck has: enough that the rail and the
/// overview overflow their viewports.
pub const LONG_DECK_SLIDES: usize = 12;

/// The sample deck repeated, with fresh ids, to [`LONG_DECK_SLIDES`] slides.
pub fn long_deck() -> Deck {
    let mut deck = seed_deck();
    let base = deck.slides.clone();
    for slide in base.iter().cycle() {
        if deck.slides.len() >= LONG_DECK_SLIDES {
            break;
        }
        let mut copy = slide.clone();
        copy.id = new_id();
        for element in &mut copy.elements {
            element.id = new_id();
        }
        deck.slides.push(copy);
    }
    deck
}

/// The sample deck ending on a slide that shows the picture `src` under a
/// title.
pub fn picture_deck(title: &str, src: &str) -> Deck {
    let mut deck = seed_deck();
    let mut slide = Slide::new(Layout::TitleContent);
    slide.add(Element::text(title, 96.0, 70.0, 1088.0, 90.0, 56.0).with(|e| e.bold = true));
    slide.add(
        Element::shape(ElementKind::Rect, 96.0, 168.0, 150.0, 8.0).with(|e| {
            e.fill = "accent".into();
            e.radius = 4.0;
        }),
    );
    slide.add(Element::image(src, 96.0, 200.0, 1088.0, 420.0));
    deck.slides.push(slide);
    deck
}

/// A small, unmistakably photographic raster: warm rings over a cool field,
/// clearly a picture rather than a flat UI gradient.
pub fn photo_png() -> Vec<u8> {
    use image::{Rgb, RgbImage};
    let (w, h) = (640u32, 400u32);
    let mut img = RgbImage::new(w, h);
    let cx = w as f32 / 2.0;
    let cy = h as f32 / 2.0;
    for (x, y, px) in img.enumerate_pixels_mut() {
        let dx = x as f32 - cx;
        let dy = y as f32 - cy;
        let r = (dx * dx + dy * dy).sqrt();
        let ring = (r * 0.06).sin() * 0.5 + 0.5;
        let red = (40.0 + 200.0 * ring) as u8;
        let green = (70.0 + 90.0 * ring) as u8;
        let blue = (120.0 + 60.0 * (1.0 - ring)) as u8;
        *px = Rgb([red, green, blue]);
    }
    // Encoding an in-memory RGB image as PNG cannot fail.
    ui::png::encode_image(&image::DynamicImage::ImageRgb8(img)).unwrap_or_default()
}

/// The content id [`photo_png`] is stored under, in any store.
pub fn photo_src() -> Option<String> {
    crate::assets::store_asset(&ContentStore::memory(), &photo_png())
}

/// A content id no store holds: a picture whose bytes have not arrived.
pub const MISSING_SRC: &str =
    "asset:blake3:0000000000000000000000000000000000000000000000000000000000000000";
