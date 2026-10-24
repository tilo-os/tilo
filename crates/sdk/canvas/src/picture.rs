use tilo_ui_paint::peniko::{ImageData, ImageQuality};

/// A decoded picture a canvas draws: a page raster, a photo, a map tile, a
/// video frame copied to the CPU. Cloning shares the pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Picture(pub(crate) ImageData);

impl Picture {
    /// `width`×`height` RGBA8 pixels with straight alpha.
    #[must_use]
    pub fn rgba8(pixels: Vec<u8>, width: u32, height: u32) -> Self {
        Self(tilo_ui_paint::raster::rgba8(pixels, width, height))
    }

    /// `width`×`height` RGBA8 pixels whose colour is already multiplied by
    /// alpha, as page rasterisers and decoders usually produce them.
    #[must_use]
    pub fn rgba8_premultiplied(pixels: Vec<u8>, width: u32, height: u32) -> Self {
        Self(tilo_ui_paint::raster::rgba8_premultiplied(
            pixels, width, height,
        ))
    }

    /// A picture from pixels already decoded for the renderer.
    #[must_use]
    pub fn from_image(image: ImageData) -> Self {
        Self(image)
    }

    #[must_use]
    pub fn width(&self) -> u32 {
        self.0.width
    }

    #[must_use]
    pub fn height(&self) -> u32 {
        self.0.height
    }
}

/// How a picture is sampled when drawn at another size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PictureQuality {
    /// Nearest pixel: pixel art, a zoomed-in raster, a terminal image.
    Pixelated,
    /// Smooth: photos, pages and frames.
    #[default]
    Smooth,
}

impl PictureQuality {
    pub(crate) fn sampling(self) -> ImageQuality {
        match self {
            Self::Pixelated => ImageQuality::Low,
            Self::Smooth => ImageQuality::High,
        }
    }
}
