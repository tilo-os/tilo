//! Cutting a subject out of a picture.
//!
//! A [`Matte`] says how much of each pixel belongs to the subject, `0..=1`;
//! `SubjectModel` (with the `infer` feature) finds one, [`refine_edge`] pulls
//! its edge onto the picture's own, and [`defringe`] takes the background's
//! colour out of the pixels along it.
//!
//! Pictures are straight (not premultiplied) RGBA, row by row, four bytes a
//! pixel, as decoders and renderers hand them over.

mod matte;
#[cfg(feature = "infer")]
mod model;
mod refine;

pub use matte::{Matte, object_at};
#[cfg(feature = "infer")]
pub use model::{Family, SubjectModel};
pub use refine::{defringe, refine_edge};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("the picture is {width}x{height} but has {len} bytes; four a pixel are needed")]
    Pixels {
        width: usize,
        height: usize,
        len: usize,
    },
    #[error("the picture and the matte differ in size")]
    Size,
    #[error("the picture is empty")]
    Empty,
    #[error("the subject model cannot be loaded: {0}")]
    Load(String),
    #[error("the subject model failed: {0}")]
    Run(String),
}

#[derive(Clone, Copy, Debug)]
pub struct Picture<'a> {
    pub rgba: &'a [u8],
    pub width: usize,
    pub height: usize,
}

impl Picture<'_> {
    fn check(&self) -> Result<usize, Error> {
        let count = self
            .width
            .checked_mul(self.height)
            .filter(|count| *count > 0)
            .ok_or(Error::Empty)?;
        if count.checked_mul(4) != Some(self.rgba.len()) {
            return Err(Error::Pixels {
                width: self.width,
                height: self.height,
                len: self.rgba.len(),
            });
        }
        Ok(count)
    }

    fn colours(&self) -> Vec<[f32; 3]> {
        self.rgba
            .chunks_exact(4)
            .map(|pixel| {
                let channel = |at: usize| f32::from(pixel.get(at).copied().unwrap_or(0)) / 255.0;
                [channel(0), channel(1), channel(2)]
            })
            .collect()
    }
}
