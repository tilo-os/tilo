//! Thumbnailing a batch of images as a content transform: the same bytes in give the
//! same PNGs out, whichever of a person's devices runs it, which is what makes the batch
//! offloadable.

use std::fmt;

pub const BATCH_EDGE: u32 = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Undecodable {
    pub digest: [u8; 32],
}

impl fmt::Display for Undecodable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "the content {} is not an image",
            hex::encode(self.digest)
        )
    }
}

impl std::error::Error for Undecodable {}

/// A PNG thumbnail, at most [`BATCH_EDGE`] on its longest edge, of each of
/// `inputs`, in order, or why that input made none. One undecodable input
/// does not sink the rest.
///
/// The work is spread over the device's cores; the output order, and every
/// byte of it, is independent of how.
pub fn thumbnail_batch(inputs: &[Vec<u8>]) -> Vec<Result<Vec<u8>, Undecodable>> {
    let workers = std::thread::available_parallelism()
        .map_or(1, std::num::NonZeroUsize::get)
        .min(inputs.len().max(1));
    let per_worker = inputs.len().div_ceil(workers).max(1);
    let mut thumbnails: Vec<Option<Result<Vec<u8>, Undecodable>>> = vec![None; inputs.len()];
    std::thread::scope(|scope| {
        for (inputs, slots) in inputs
            .chunks(per_worker)
            .zip(thumbnails.chunks_mut(per_worker))
        {
            scope.spawn(move || {
                for (input, slot) in inputs.iter().zip(slots) {
                    *slot = Some(
                        crate::generate::image_thumb(input, BATCH_EDGE)
                            .and_then(|thumb| crate::generate::encode_png(&thumb))
                            .map_or_else(
                                || {
                                    Err(Undecodable {
                                        digest: *blake3::hash(input).as_bytes(),
                                    })
                                },
                                Ok,
                            ),
                    );
                }
            });
        }
    });
    // Every slot is filled by its worker; one left empty reads as its input
    // having made no thumbnail rather than taking the batch down.
    thumbnails
        .into_iter()
        .zip(inputs)
        .map(|(thumbnail, input)| {
            thumbnail.unwrap_or_else(|| {
                Err(Undecodable {
                    digest: *blake3::hash(input).as_bytes(),
                })
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jpeg(width: u32, height: u32, seed: u8) -> Vec<u8> {
        let image = image::RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([
                u8::try_from(x % 256).unwrap_or(0) ^ seed,
                u8::try_from(y % 256).unwrap_or(0),
                seed,
            ])
        });
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(image)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        bytes
    }

    #[test]
    fn a_batch_is_ordered_deterministic_and_fails_per_input() {
        let inputs = vec![jpeg(1200, 800, 1), jpeg(640, 960, 2), jpeg(300, 300, 3)];
        let first = thumbnail_batch(&inputs);
        assert_eq!(first, thumbnail_batch(&inputs));
        assert_eq!(first.len(), 3);
        for (thumbnail, (width, height)) in first.iter().zip([(256, 171), (171, 256), (256, 256)]) {
            let decoded = crate::decode_png(thumbnail.as_ref().unwrap()).unwrap();
            assert_eq!((decoded.width, decoded.height), (width, height));
        }
        let mut broken = inputs;
        let bad = b"not an image".to_vec();
        broken.insert(1, bad.clone());
        let made = thumbnail_batch(&broken);
        assert_eq!(made.len(), 4);
        assert!(made[0].is_ok() && made[2].is_ok() && made[3].is_ok());
        assert_eq!(
            made[1],
            Err(Undecodable {
                digest: *blake3::hash(&bad).as_bytes()
            })
        );
        assert!(
            made[1]
                .as_ref()
                .unwrap_err()
                .to_string()
                .contains(&hex::encode(blake3::hash(&bad).as_bytes()))
        );
    }
}
