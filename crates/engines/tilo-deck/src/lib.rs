//! A deck's engine: the slides, their elements and words, and the `.odp` and `.pptx`
//! interchange. It draws nothing and knows no window.
//!
//! A deck crosses this boundary as its own JSON shape ([`Deck`]). A picture crosses as bytes it
//! keeps in the workspace's content store ([`Pictures`]): an import keeps each picture it finds
//! under the package and turns its reference into the store's address; an export reads each
//! address back and bundles the bytes under the file.

use std::collections::HashMap;

pub mod colour;
pub mod model;
pub mod odp;
pub mod pptx;

pub use model::{
    Background, Block, BlockMark, Deck, DeckLook, Element, ElementKind, Layout, MarkValue,
    PaletteKind, SLIDE_H, SLIDE_W, Slide, TextAlign,
};

/// Where a deck's pictures live while a file is read or written.
pub trait Pictures {
    /// The bytes the address `src` names, when this device holds them.
    fn read(&self, src: &str) -> Option<Vec<u8>>;

    /// Keep `bytes` as a picture, answering the address a deck stores for it.
    fn keep(&self, bytes: &[u8]) -> Option<String>;
}

/// Which interchange format a file is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// An `OpenDocument` presentation.
    Odp,
    /// A presentation in the `Office Open XML` format.
    Pptx,
}

impl Format {
    /// The format a file name's ending names.
    #[must_use]
    pub fn of_name(name: &str) -> Option<Self> {
        let ext = name.rsplit('.').next()?.to_ascii_lowercase();
        match ext.as_str() {
            "odp" => Some(Self::Odp),
            "pptx" => Some(Self::Pptx),
            _ => None,
        }
    }
}

/// The deck a presentation file holds, its pictures kept in `pictures`.
///
/// # Errors
/// The file is not a readable presentation package, or holds more than a deck can.
pub fn import(bytes: &[u8], name: &str, pictures: &dyn Pictures) -> Result<Deck, String> {
    let format = Format::of_name(name)
        .ok_or_else(|| format!("`{name}` is not a presentation file"))?;
    let read = match format {
        Format::Odp => odp::read_odp(bytes, pictures),
        Format::Pptx => pptx::read_pptx(bytes, pictures),
    };
    read.map_err(|error| error.to_string())
}

/// The bytes of `deck` written as a presentation file of `format`, its pictures read from
/// `pictures`.
///
/// # Errors
/// The file could not be written.
pub fn export(deck: &Deck, pictures: &dyn Pictures, format: Format) -> Result<Vec<u8>, String> {
    let written = match format {
        Format::Odp => odp::write_odp(deck, pictures),
        Format::Pptx => pptx::write_pptx(deck, pictures),
    };
    written.map_err(|error| error.to_string())
}

/// How tall each text box of `deck` shows, in slide units: what an exported file's frame takes,
/// so a box its words outgrew is not cut short. Laid out from the font size and the box width,
/// as a plain estimate: the exported file carries every position and size the deck holds, so
/// this only sizes a box's word area for a reader that lays the words out itself.
#[must_use]
pub fn text_heights(deck: &Deck) -> HashMap<String, f64> {
    deck.slides
        .iter()
        .flat_map(|slide| &slide.elements)
        .filter(|element| element.kind == ElementKind::Text)
        .map(|element| (element.id.clone(), estimated_height(element)))
        .collect()
}

fn estimated_height(element: &Element) -> f64 {
    let size = if element.font_size > 0.0 { element.font_size } else { 24.0 };
    let per_line = (element.w / (size * 0.55)).max(1.0);
    let lines: f64 = element
        .rich_blocks()
        .iter()
        .map(|block| {
            let chars = block.text.chars().count() as f64;
            (chars / per_line).ceil().max(1.0)
        })
        .sum();
    element.h.max(lines * size * 1.25)
}

/// The ink a text element is drawn in, resolved against what it stands on: the fills of the
/// shapes under it, else the slide's background.
#[must_use]
pub fn text_ink(palette: &colour::Palette, slide: &Slide, element: &Element) -> colour::Rgb {
    let centre = |other: &Element| {
        (
            element.x + element.w / 2.0 >= other.x
                && element.x + element.w / 2.0 <= other.x + other.w,
            element.y + element.h / 2.0 >= other.y
                && element.y + element.h / 2.0 <= other.y + other.h,
        )
    };
    let mut ground = palette.on(slide.background).slide_bg(slide.background);
    for other in &slide.elements {
        if other.id == element.id {
            break;
        }
        let covers = match other.kind {
            ElementKind::Rect => {
                let (inside_x, inside_y) = centre(other);
                inside_x && inside_y
            }
            ElementKind::Ellipse => {
                let (rx, ry) = (other.w / 2.0, other.h / 2.0);
                if rx <= 0.0 || ry <= 0.0 {
                    false
                } else {
                    let dx = (element.x + element.w / 2.0 - (other.x + rx)) / rx;
                    let dy = (element.y + element.h / 2.0 - (other.y + ry)) / ry;
                    dx * dx + dy * dy <= 1.0
                }
            }
            ElementKind::Text | ElementKind::Line | ElementKind::Image => false,
        };
        if covers && let Some(fill) = palette.on(slide.background).resolve_role(&other.fill) {
            ground = fill;
        }
    }
    palette.on(slide.background).text_ink(&element.fill, ground)
}

/// Whether `deck` holds no more than one deck can: slides, elements, words and pictures counted
/// across it. The most a file may bring.
#[must_use]
pub fn within_limits(deck: &Deck) -> bool {
    if deck.slides.len() > model::MAX_SLIDES {
        return false;
    }
    let mut elements = 0_usize;
    let mut text = deck.title.chars().count();
    let mut pictures = 0_usize;
    for slide in &deck.slides {
        elements = elements.saturating_add(slide.elements.len());
        text = text.saturating_add(slide.notes.chars().count());
        for element in &slide.elements {
            text = text.saturating_add(element.plain_text().chars().count());
            pictures = pictures.saturating_add(usize::from(element.kind == ElementKind::Image));
        }
    }
    elements <= 2_000_000 && text <= 2_000_000 && pictures <= 2_048
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_deck_round_trips_through_both_formats_with_its_pictures() {
        let pictures = Memory::default();
        let picture = pictures.keep(&tiny_png()).expect("kept");
        let deck = Deck {
            title: "Quarterly review".to_owned(),
            palette: PaletteKind::Emerald,
            look: DeckLook::Dark,
            slides: vec![Slide {
                id: "s1".to_owned(),
                layout: Layout::Title,
                background: Background::Accent,
                notes: "Say hello".to_owned(),
                elements: vec![
                    Element::text("Hello\nworld", 96.0, 90.0, 600.0, 120.0, 54.0).with(|e| {
                        e.bold = true;
                    }),
                    Element::image(&picture, 100.0, 300.0, 400.0, 200.0),
                ],
            }],
        };
        for format in [Format::Odp, Format::Pptx] {
            let bytes = export(&deck, &pictures, format).expect("exported");
            let read = import(&bytes, match format {
                Format::Odp => "review.odp",
                Format::Pptx => "review.pptx",
            }, &pictures)
            .expect("imported");
            assert_eq!(read.title, deck.title);
            assert_eq!(read.look, deck.look);
            assert_eq!(read.slides.len(), 1);
            assert_eq!(read.slides[0].notes, "Say hello");
            let texts: Vec<String> = read.slides[0]
                .elements
                .iter()
                .filter(|element| element.kind == ElementKind::Text)
                .map(Element::plain_text)
                .collect();
            assert_eq!(texts, vec!["Hello\nworld".to_owned()]);
            let image = read.slides[0]
                .elements
                .iter()
                .find(|element| element.kind == ElementKind::Image)
                .expect("the picture came back");
            assert!(pictures.read(&image.src).is_some(), "kept: {}", image.src);
        }
    }

    #[derive(Default)]
    struct Memory(std::cell::RefCell<HashMap<String, Vec<u8>>>);

    impl Pictures for Memory {
        fn read(&self, src: &str) -> Option<Vec<u8>> {
            self.0.borrow().get(src).cloned()
        }

        fn keep(&self, bytes: &[u8]) -> Option<String> {
            let hash = content_hash(bytes);
            let src = format!("tilo-blob://{hash}");
            self.0.borrow_mut().insert(src.clone(), bytes.to_vec());
            Some(src)
        }
    }

    fn content_hash(bytes: &[u8]) -> String {
        // A stand-in address for the test: the store's own hash is not this crate's.
        format!("{:064x}", bytes.len())
    }

    fn tiny_png() -> Vec<u8> {
        const PNG: &[u8] = &[
            0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, b'I', b'H',
            b'D', b'R', 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x00, b'I', b'E', b'N', b'D', 0xAE,
            0x42, 0x60, 0x82,
        ];
        PNG.to_vec()
    }
}
