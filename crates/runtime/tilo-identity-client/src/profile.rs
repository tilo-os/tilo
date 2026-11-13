//! The person's public presentation, independent of signing custody and image
//! codecs: a photo holds normalized straight-alpha RGBA pixels, which the
//! caller decoded and resized first.

use std::fmt;

use serde::de::{SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

pub const MAX_PHOTO_DIMENSION: u16 = 128;
/// Maximum normalized pixel payload (128 × 128 × four channels).
pub const MAX_PHOTO_BYTES: usize = 65_536;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "PhotoInput")]
pub struct ProfilePhoto {
    width: u16,
    height: u16,
    rgba: Vec<u8>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PhotoInput {
    width: u16,
    height: u16,
    #[serde(deserialize_with = "bounded_pixels")]
    rgba: Vec<u8>,
}

fn bounded_pixels<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
    struct Pixels;
    impl<'de> Visitor<'de> for Pixels {
        type Value = Vec<u8>;
        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("at most 65536 RGBA bytes")
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
            let mut bytes =
                Vec::with_capacity(sequence.size_hint().unwrap_or(0).min(MAX_PHOTO_BYTES));
            while let Some(byte) = sequence.next_element()? {
                if bytes.len() == MAX_PHOTO_BYTES {
                    return Err(serde::de::Error::custom(
                        "identity photo exceeds pixel bound",
                    ));
                }
                bytes.push(byte);
            }
            Ok(bytes)
        }
    }
    deserializer.deserialize_seq(Pixels)
}

impl TryFrom<PhotoInput> for ProfilePhoto {
    type Error = &'static str;
    fn try_from(input: PhotoInput) -> Result<Self, Self::Error> {
        Self::new(input.width, input.height, input.rgba)
    }
}

impl ProfilePhoto {
    pub fn new(width: u16, height: u16, rgba: Vec<u8>) -> Result<Self, &'static str> {
        if width == 0 || height == 0 || width > MAX_PHOTO_DIMENSION || height > MAX_PHOTO_DIMENSION
        {
            return Err("identity photo dimensions are outside the bound");
        }
        let expected = usize::from(width)
            .checked_mul(usize::from(height))
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or("identity photo dimensions overflow")?;
        if rgba.len() != expected {
            return Err("identity photo pixel length does not match dimensions");
        }
        Ok(Self {
            width,
            height,
            rgba,
        })
    }

    pub fn width(&self) -> u16 {
        self.width
    }
    pub fn height(&self) -> u16 {
        self.height
    }
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Serialize)]
    struct UncheckedPhoto {
        width: u16,
        height: u16,
        rgba: Vec<u8>,
    }

    #[test]
    fn binary_presentation_decoder_refuses_invalid_and_oversized_pixels() {
        let valid = ProfilePhoto::new(1, 1, vec![25, 90, 210, 255]).unwrap();
        assert_eq!(
            postcard::from_bytes::<ProfilePhoto>(&postcard::to_allocvec(&valid).unwrap()).unwrap(),
            valid
        );
        for (width, height, rgba) in [
            (0, 1, vec![0; 4]),
            (129, 1, vec![0; 516]),
            (1, 1, vec![0; 3]),
            (128, 128, vec![0; MAX_PHOTO_BYTES + 1]),
        ] {
            let bytes = postcard::to_allocvec(&UncheckedPhoto {
                width,
                height,
                rgba,
            })
            .unwrap();
            assert!(postcard::from_bytes::<ProfilePhoto>(&bytes).is_err());
        }
    }
}
