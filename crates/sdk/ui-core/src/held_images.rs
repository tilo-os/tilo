//! Pictures an app already holds as data: a contact's photo, artwork a media file embeds. They load
//! like any other image (decoded off the UI thread by an [`ImageStore`], cached, a placeholder painted
//! until then), but nothing is fetched: the store's fetcher reads the bytes the app registered with
//! [`HeldImages::hold`], and an address no one holds fails.

use std::collections::HashMap;
use std::hash::{Hash as _, Hasher as _};
use std::sync::{Arc, RwLock};

use tilo_lock::RwLockRecover as _;

use crate::image_store::{Fetcher, ImageStore};

/// The side, in pixels, of a [`portrait`]: sharp at the largest avatar and cover sizes on a 2× display,
/// small enough to carry in a record.
pub const PORTRAIT_SIDE: u32 = 192;

/// Encoded pictures by address, shared with the fetcher of every store [`Self::store`] made.
#[derive(Clone, Default)]
pub struct HeldImages {
    bytes: Arc<RwLock<HashMap<String, Held>>>,
}

/// A held picture: its bytes, or [`picture_text`] decoded only when loaded.
#[derive(Clone)]
enum Held {
    Bytes(Arc<[u8]>),
    Text(Arc<str>),
}

impl std::fmt::Debug for HeldImages {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeldImages").finish_non_exhaustive()
    }
}

impl HeldImages {
    /// Hold `encoded` (PNG, JPEG or WebP) and return the address an [`ImageStore`] from [`Self::store`]
    /// loads it by. The same bytes always get the same address, so a picture is decoded once however
    /// often it is held.
    pub fn hold(&self, encoded: &[u8]) -> String {
        let address = address("held", encoded);
        self.bytes
            .write_recover()
            .entry(address.clone())
            .or_insert_with(|| Held::Bytes(Arc::from(encoded)));
        address
    }

    /// Hold a picture kept as [`picture_text`] (a record field), decoding it only when a store loads
    /// it, off the UI thread.
    pub fn hold_text(&self, text: &str) -> String {
        if text.is_empty() {
            return String::new();
        }
        let address = address("held-text", text.as_bytes());
        self.bytes
            .write_recover()
            .entry(address.clone())
            .or_insert_with(|| Held::Text(Arc::from(text)));
        address
    }

    #[must_use]
    pub fn holds(&self, address: &str) -> bool {
        self.bytes.read_recover().contains_key(address)
    }

    /// Keep only the pictures at `addresses`, so pictures no one shows any more are not kept forever.
    pub fn retain(&self, addresses: &std::collections::HashSet<String>) {
        self.bytes
            .write_recover()
            .retain(|address, _| addresses.contains(address));
    }

    /// A fetcher that answers held addresses and nothing else.
    #[must_use]
    pub fn fetcher(&self) -> Fetcher {
        let bytes = Arc::clone(&self.bytes);
        Arc::new(move |address: &str| {
            let held = bytes.read_recover().get(address).cloned()?;
            match held {
                Held::Bytes(encoded) => Some(encoded.to_vec()),
                Held::Text(text) => picture_bytes(&text),
            }
        })
    }

    /// An image store that loads held pictures.
    #[must_use]
    pub fn store(&self) -> ImageStore {
        ImageStore::new(&self.fetcher())
    }
}

/// `encoded` (any format the toolkit decodes) as a [`PORTRAIT_SIDE`] square JPEG, cropped to its
/// centre, or `None` when it does not decode.
#[must_use]
pub fn portrait(encoded: &[u8]) -> Option<Vec<u8>> {
    let image = image::load_from_memory(encoded).ok()?;
    let side = image.width().min(image.height());
    if side == 0 {
        return None;
    }
    let x = image.width().saturating_sub(side) / 2;
    let y = image.height().saturating_sub(side) / 2;
    let square = image.crop_imm(x, y, side, side).resize_exact(
        PORTRAIT_SIDE.min(side),
        PORTRAIT_SIDE.min(side),
        image::imageops::FilterType::Triangle,
    );
    let mut out = std::io::Cursor::new(Vec::new());
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 82)
        .encode_image(&square.to_rgb8())
        .ok()?;
    Some(out.into_inner())
}

/// A 2×2 mosaic of up to four pictures, as a [`PORTRAIT_SIDE`] square JPEG: a playlist's cover from its
/// tracks' artwork.
#[must_use]
pub fn mosaic(pictures: &[&[u8]]) -> Option<Vec<u8>> {
    let half = PORTRAIT_SIDE / 2;
    let tiles: Vec<image::RgbImage> = pictures
        .iter()
        .filter_map(|encoded| image::load_from_memory(encoded).ok())
        .take(4)
        .map(|image| {
            let side = image.width().min(image.height()).max(1);
            let x = image.width().saturating_sub(side) / 2;
            let y = image.height().saturating_sub(side) / 2;
            image
                .crop_imm(x, y, side, side)
                .resize_exact(half, half, image::imageops::FilterType::Triangle)
                .to_rgb8()
        })
        .collect();
    let first = tiles.first()?;
    if tiles.len() == 1 {
        let whole = image::imageops::resize(
            first,
            PORTRAIT_SIDE,
            PORTRAIT_SIDE,
            image::imageops::FilterType::Triangle,
        );
        return encode_jpeg(&whole);
    }
    let mut canvas = image::RgbImage::new(PORTRAIT_SIDE, PORTRAIT_SIDE);
    for (index, (x, y)) in [(0, 0), (half, 0), (0, half), (half, half)]
        .into_iter()
        .enumerate()
    {
        // Fewer than four pictures repeat in order to fill the quarters.
        let tile = tiles
            .get(index)
            .or_else(|| tiles.get(index.checked_rem(tiles.len()).unwrap_or(0)))
            .unwrap_or(first);
        image::imageops::replace(&mut canvas, tile, i64::from(x), i64::from(y));
    }
    encode_jpeg(&canvas)
}

fn address(kind: &str, data: &[u8]) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    data.hash(&mut hasher);
    format!("{kind}:{:016x}:{}", hasher.finish(), data.len())
}

/// A picture as text, for a record field or a card: standard base64.
#[must_use]
pub fn picture_text(encoded: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(encoded)
}

/// The picture [`picture_text`] wrote (or any standard or MIME-wrapped base64, as vCards carry it), or
/// `None` when it is not base64.
#[must_use]
pub fn picture_bytes(text: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::STANDARD
        .decode(compact.as_bytes())
        .ok()
        .filter(|bytes| !bytes.is_empty())
}

/// A cover already decoded (`crate::peniko::ImageData` from an image store or a media reading) as the
/// small `data:` picture a card draws, or another process is handed: a `edge` square cut from its
/// centre, JPEG-encoded so it costs a few kilobytes rather than a cover's own tens.
#[must_use]
pub fn cover(picture: &crate::peniko::ImageData, edge: u32) -> String {
    /// Enough quality for a thumbnail.
    const QUALITY: u8 = 82;
    /// What the encoded picture may cost before it is cut smaller.
    const SMALLER: u8 = 62;
    if picture.format != crate::peniko::ImageFormat::Rgba8 || edge == 0 {
        return String::new();
    }
    let Some(rgba) =
        image::RgbaImage::from_raw(picture.width, picture.height, picture.data.data().to_vec())
    else {
        return String::new();
    };
    let side = rgba.width().min(rgba.height());
    if side == 0 {
        return String::new();
    }
    let square = image::imageops::crop_imm(
        &rgba,
        (rgba.width() - side) / 2,
        (rgba.height() - side) / 2,
        side,
        side,
    )
    .to_image();
    for (edge, quality) in [
        (edge, QUALITY),
        (edge, SMALLER),
        (edge / 2, QUALITY),
        (edge / 4, QUALITY),
    ] {
        let Some(small) = (edge > 0).then(|| {
            image::imageops::resize(&square, edge, edge, image::imageops::FilterType::Triangle)
        }) else {
            continue;
        };
        let mut encoded = Vec::new();
        if image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, quality)
            .encode_image(&image::DynamicImage::ImageRgba8(small))
            .is_err()
        {
            return String::new();
        }
        if encoded.len() <= PICTURE_BUDGET {
            return format!("data:image/jpeg;base64,{}", picture_text(&encoded));
        }
    }
    String::new()
}

/// The most a cover's encoded bytes may cost: a live activity carries the picture beside the song under
/// a wire bound, and a base64 `data:` picture takes four thirds of that room.
pub const PICTURE_BUDGET: usize = 6 * 1024;

/// The most a hero picture's encoded bytes may cost: the card shows it at nine sixteenths of its width,
/// so it is worth more bytes than a cover, and it goes to a session's own floating layer rather than a
/// shared board.
pub const HERO_BUDGET: usize = 96 * 1024;

/// A whole picture as the `data:` picture a card heads itself with, `width` wide and as tall as the
/// picture's own shape: no ground around it, so what it caught fills the card.
#[must_use]
pub fn hero(picture: &image::RgbaImage, width: u32) -> String {
    if width == 0 || picture.width() == 0 || picture.height() == 0 {
        return String::new();
    }
    let fit = f64::from(width) / f64::from(picture.width());
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a card's picture is a few hundred pixels wide"
    )]
    let (wide, tall) = (
        width,
        ((f64::from(picture.height()) * fit).round() as u32).max(1),
    );
    let scaled =
        image::imageops::resize(picture, wide, tall, image::imageops::FilterType::Triangle);
    for (edge, quality) in [(wide, 82_u8), (wide, 62), (wide / 2, 82), (wide / 4, 82)] {
        let Some(small) = (edge > 0).then(|| {
            let tall = (f64::from(tall) * f64::from(edge) / f64::from(wide)).round();
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a card's picture is a few hundred pixels wide"
            )]
            let tall = (tall as u32).max(1);
            image::imageops::resize(&scaled, edge, tall, image::imageops::FilterType::Triangle)
        }) else {
            continue;
        };
        let mut encoded = Vec::new();
        if image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, quality)
            .encode_image(&image::DynamicImage::ImageRgba8(small))
            .is_err()
        {
            return String::new();
        }
        if encoded.len() <= HERO_BUDGET {
            return format!("data:image/jpeg;base64,{}", picture_text(&encoded));
        }
    }
    String::new()
}

/// [`mosaic`] for pictures already decoded (covers an image store loaded): a `side` square of up to
/// four, sampled straight into place, so composing it costs one pass over its pixels.
#[must_use]
pub fn mosaic_of(
    tiles: &[crate::peniko::ImageData],
    side: u32,
) -> Option<crate::peniko::ImageData> {
    use crate::peniko::{Blob, ImageAlphaType, ImageData, ImageFormat};
    let tiles: Vec<&ImageData> = tiles
        .iter()
        .filter(|tile| {
            matches!(tile.format, ImageFormat::Rgba8 | ImageFormat::Bgra8)
                && tile.width > 0
                && tile.height > 0
        })
        .take(4)
        .collect();
    let first = *tiles.first()?;
    let side = side.max(2);
    let half = side / 2;
    let stride = usize::try_from(side).ok()?.checked_mul(4)?;
    let mut out = vec![0_u8; stride.checked_mul(usize::try_from(side).ok()?)?];
    for (index, (x0, y0)) in [(0, 0), (half, 0), (0, half), (half, half)]
        .into_iter()
        .enumerate()
    {
        let tile = tiles
            .get(index)
            .or_else(|| tiles.get(index.checked_rem(tiles.len()).unwrap_or(0)))
            .copied()
            .unwrap_or(first);
        let pixels = tile.data.data();
        let crop = tile.width.min(tile.height);
        let left = tile.width.saturating_sub(crop) / 2;
        let top = tile.height.saturating_sub(crop) / 2;
        let bgra = matches!(tile.format, ImageFormat::Bgra8);
        for y in 0..half {
            let source_y =
                top.saturating_add(y.saturating_mul(crop).checked_div(half).unwrap_or(0));
            for x in 0..half {
                let source_x =
                    left.saturating_add(x.saturating_mul(crop).checked_div(half).unwrap_or(0));
                let from = usize::try_from(
                    u64::from(source_y)
                        .saturating_mul(u64::from(tile.width))
                        .saturating_add(u64::from(source_x))
                        .saturating_mul(4),
                )
                .ok()?;
                let to = usize::try_from(
                    u64::from(y0.saturating_add(y))
                        .saturating_mul(u64::from(side))
                        .saturating_add(u64::from(x0.saturating_add(x)))
                        .saturating_mul(4),
                )
                .ok()?;
                let (Some(source), Some(target)) = (
                    pixels.get(from..from.saturating_add(4)),
                    out.get_mut(to..to.saturating_add(4)),
                ) else {
                    continue;
                };
                target.copy_from_slice(source);
                if bgra {
                    target.swap(0, 2);
                }
            }
        }
    }
    Some(ImageData {
        data: Blob::new(Arc::new(out)),
        format: ImageFormat::Rgba8,
        alpha_type: if tiles
            .iter()
            .all(|tile| tile.alpha_type == ImageAlphaType::Alpha)
        {
            ImageAlphaType::Alpha
        } else {
            ImageAlphaType::AlphaPremultiplied
        },
        width: side,
        height: side,
    })
}

fn encode_jpeg(image: &image::RgbImage) -> Option<Vec<u8>> {
    let mut out = std::io::Cursor::new(Vec::new());
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 82)
        .encode_image(image)
        .ok()?;
    Some(out.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32, colour: [u8; 3]) -> Vec<u8> {
        let image = image::RgbImage::from_pixel(width, height, image::Rgb(colour));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(image)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    #[test]
    fn held_pictures_load_by_address_and_nothing_else_is_fetched() {
        let held = HeldImages::default();
        let picture = png(4, 4, [200, 10, 10]);
        let address = held.hold(&picture);
        assert_eq!(held.hold(&picture), address, "the same bytes, one address");
        let fetch = held.fetcher();
        assert_eq!(fetch(&address), Some(picture.clone()));
        assert_eq!(fetch("https://example.com/a.png"), None);
        let text = held.hold_text(&picture_text(&picture));
        assert_eq!(fetch(&text), Some(picture.clone()));
        assert_eq!(held.hold_text(""), "");
        held.retain(&std::collections::HashSet::new());
        assert!(!held.holds(&address));
    }

    #[test]
    fn portraits_are_small_centred_squares_and_mosaics_tile_four_covers() {
        let wide = png(800, 400, [10, 200, 10]);
        let face = image::load_from_memory(&portrait(&wide).unwrap()).unwrap();
        assert_eq!(
            (face.width(), face.height()),
            (PORTRAIT_SIDE, PORTRAIT_SIDE)
        );
        assert_eq!(portrait(b"not a picture"), None);

        let colours = [[250, 0, 0], [0, 250, 0], [0, 0, 250], [250, 250, 0]];
        let covers: Vec<Vec<u8>> = colours.iter().map(|&c| png(64, 64, c)).collect();
        let refs: Vec<&[u8]> = covers.iter().map(Vec::as_slice).collect();
        let tiled = image::load_from_memory(&mosaic(&refs).unwrap())
            .unwrap()
            .to_rgb8();
        let quarter = PORTRAIT_SIDE / 4;
        let three_quarters = quarter * 3;
        for ((x, y), colour) in [
            (quarter, quarter),
            (three_quarters, quarter),
            (quarter, three_quarters),
            (three_quarters, three_quarters),
        ]
        .into_iter()
        .zip(colours)
        {
            let pixel = tiled.get_pixel(x, y).0;
            for (got, want) in pixel.iter().zip(colour) {
                assert!(
                    got.abs_diff(want) < 40,
                    "{pixel:?} at {x},{y} vs {colour:?}"
                );
            }
        }
        assert_eq!(mosaic(&[b"nothing".as_slice()]), None);

        let decoded: Vec<crate::peniko::ImageData> = colours
            .iter()
            .map(|&[r, g, b]| crate::peniko::ImageData {
                data: crate::peniko::Blob::new(Arc::new([r, g, b, 255].repeat(16 * 8))),
                format: crate::peniko::ImageFormat::Rgba8,
                alpha_type: crate::peniko::ImageAlphaType::Alpha,
                width: 16,
                height: 8,
            })
            .collect();
        let tiled = mosaic_of(&decoded, 64).unwrap();
        assert_eq!((tiled.width, tiled.height), (64, 64));
        let pixels = tiled.data.data();
        for ((x, y), [r, g, b]) in [(8, 8), (40, 8), (8, 40), (40, 40)]
            .into_iter()
            .zip(colours)
        {
            let at = (y * 64 + x) * 4;
            assert_eq!(&pixels[at..at + 4], &[r, g, b, 255], "{x},{y}");
        }
        assert!(mosaic_of(&[], 64).is_none());
    }

    #[test]
    fn a_cover_becomes_a_small_square_picture_a_card_can_draw() {
        let wide = crate::peniko::ImageData {
            data: crate::peniko::Blob::new(Arc::new([30, 120, 200, 255].repeat(80 * 40))),
            format: crate::peniko::ImageFormat::Rgba8,
            alpha_type: crate::peniko::ImageAlphaType::Alpha,
            width: 80,
            height: 40,
        };
        let picture = cover(&wide, 24);
        assert!(picture.starts_with("data:image/jpeg;base64,"), "{picture}");
        let bytes = picture_bytes(picture.strip_prefix("data:image/jpeg;base64,").unwrap())
            .expect("the encoded picture");
        let decoded = image::load_from_memory(&bytes).unwrap().to_rgb8();
        assert_eq!((decoded.width(), decoded.height()), (24, 24));
        let pixel = decoded.get_pixel(12, 12).0;
        for (got, want) in pixel.iter().zip([30, 120, 200]) {
            assert!(got.abs_diff(want) < 40, "{pixel:?}");
        }
        let grey = crate::peniko::ImageData {
            format: crate::peniko::ImageFormat::Bgra8,
            ..wide.clone()
        };
        assert_eq!(cover(&grey, 24), "", "a picture it does not read");
        assert_eq!(cover(&wide, 0), "", "a picture with no pixels");
    }

    /// Noise is what costs a JPEG bytes: a cover that fills its square with detail is cut down until it
    /// fits the budget the song's message has.
    #[test]
    fn a_busy_cover_keeps_inside_the_picture_budget() {
        let mut seed = 0x2545_f491_u32;
        let mut pixels = Vec::with_capacity(512 * 512 * 4);
        for _ in 0..512 * 512 {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            pixels.extend_from_slice(&[
                (seed >> 24) as u8,
                (seed >> 16) as u8,
                (seed >> 8) as u8,
                255,
            ]);
        }
        let busy = crate::peniko::ImageData {
            data: crate::peniko::Blob::new(Arc::new(pixels.into_boxed_slice())),
            format: crate::peniko::ImageFormat::Rgba8,
            alpha_type: crate::peniko::ImageAlphaType::Alpha,
            width: 512,
            height: 512,
        };
        let picture = cover(&busy, 112);
        assert!(picture.starts_with("data:image/jpeg;base64,"), "{picture}");
        let bytes = picture_bytes(picture.strip_prefix("data:image/jpeg;base64,").unwrap())
            .expect("the encoded picture");
        assert!(
            bytes.len() <= PICTURE_BUDGET,
            "{} bytes is over the budget",
            bytes.len()
        );
    }
}
