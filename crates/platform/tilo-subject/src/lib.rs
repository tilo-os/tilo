//! The subject of a picture, found on this device by the segmentation model
//! the person installed (Settings › Models), through the model gateway; and
//! the maths an app refines and cleans its edge with (`tilo-segment`).
//!
//! The model and its runtime live in the gateway: an app holds neither, and
//! nothing leaves the device.

use std::io::Cursor;

use image::ImageEncoder as _;
pub use tilo_segment::{Matte, Picture, defringe, object_at, refine_edge};

use base64::Engine as _;
use image::{DynamicImage, RgbaImage};
use sha2::{Digest as _, Sha256};
use thiserror::Error;

/// The longest side of the picture sent to the model: it sees far less
/// (320 pixels for U²-Net), and the edge is refined at full size after.
pub const MAX_SUBJECT_EDGE: u32 = 1024;

/// A subject found, at the size of the picture it was asked for.
#[derive(Clone, Debug, PartialEq)]
pub struct Subject {
    pub matte: Matte,
    pub model_id: String,
    pub revision: String,
    pub load_ms: u64,
    pub infer_ms: u64,
}

#[derive(Debug, Error)]
pub enum SubjectError {
    /// No segmentation model is installed, or no gateway was handed over.
    #[error("{0}")]
    Unavailable(String),
    #[error("the picture is empty")]
    Empty,
    #[error("the picture cannot be prepared: {0}")]
    Image(#[from] image::ImageError),
    #[error("the subject model failed: {0}")]
    Failed(String),
}

impl SubjectError {
    /// Whether setting up the model in Settings is what is missing.
    pub const fn needs_model(&self) -> bool {
        matches!(self, Self::Unavailable(_))
    }
}

/// How much of each pixel of `picture` (straight RGBA, `width` by
/// `height`) is its subject.
///
/// # Errors
/// Returns that no model is installed or reachable, that the picture is
/// empty, or the model's failure.
pub fn find_subject(rgba: &[u8], width: u32, height: u32) -> Result<Subject, SubjectError> {
    let image = RgbaImage::from_raw(width, height, rgba.to_vec())
        .filter(|_| width > 0 && height > 0)
        .ok_or(SubjectError::Empty)?;
    let image = DynamicImage::ImageRgba8(image);
    let sent = if width.max(height) > MAX_SUBJECT_EDGE {
        image.resize(
            MAX_SUBJECT_EDGE,
            MAX_SUBJECT_EDGE,
            image::imageops::FilterType::Triangle,
        )
    } else {
        image
    };
    let jpeg = encode_jpeg(&sent, 90)?;
    let hash = hex::encode(Sha256::digest(rgba));
    let request = tilo_model_gateway::VisionImage {
        media_type: "image/jpeg".into(),
        data_base64: base64::engine::general_purpose::STANDARD.encode(&jpeg),
        source_content_hash: hash,
    };
    let answer = tilo_model_gateway::segment_subject(request).map_err(|error| match error {
        tilo_model_gateway::ClientError::Unavailable => {
            SubjectError::Unavailable("the model service is not available to this app".into())
        }
        tilo_model_gateway::ClientError::Rejected(message)
            if message.contains(tilo_model_gateway::NO_SEGMENTATION_MODEL)
                || message.contains("not authorized")
                || message.contains("unknown variant") =>
        {
            SubjectError::Unavailable(message)
        }
        other => SubjectError::Failed(other.to_string()),
    })?;
    let png = base64::engine::general_purpose::STANDARD
        .decode(&answer.matte_png_base64)
        .map_err(|_| SubjectError::Failed("its matte is not base64".into()))?;
    let grey = image::load_from_memory_with_format(&png, image::ImageFormat::Png)?.to_luma8();
    let (matte_width, matte_height) = grey.dimensions();
    let values = grey
        .as_raw()
        .iter()
        .map(|&value| f32::from(value) / 255.0)
        .collect();
    let matte = Matte::new(matte_width as usize, matte_height as usize, values)
        .ok_or_else(|| SubjectError::Failed("its matte is empty".into()))?
        .resized(width as usize, height as usize);
    Ok(Subject {
        matte,
        model_id: answer.model_id,
        revision: answer.revision,
        load_ms: answer.load_ms,
        infer_ms: answer.infer_ms,
    })
}

/// A matte as the greyscale PNG [`tilo_model_gateway::SubjectMatte`] carries.
///
/// # Errors
/// Returns the encoder's failure.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a coverage clamped to 0..=1 scaled to a byte"
)]
pub fn matte_png(matte: &Matte) -> Result<Vec<u8>, image::ImageError> {
    let bytes: Vec<u8> = matte
        .values
        .iter()
        .map(|value| (value.clamp(0.0, 1.0) * 255.0).round() as u8)
        .collect();
    let width = u32::try_from(matte.width).unwrap_or(u32::MAX);
    let height = u32::try_from(matte.height).unwrap_or(u32::MAX);
    let image =
        image::GrayImage::from_raw(width, height, bytes).ok_or(image::ImageError::Limits(
            image::error::LimitError::from_kind(image::error::LimitErrorKind::DimensionError),
        ))?;
    let mut out = std::io::Cursor::new(Vec::new());
    DynamicImage::ImageLuma8(image).write_to(&mut out, image::ImageFormat::Png)?;
    Ok(out.into_inner())
}

fn encode_jpeg(image: &DynamicImage, quality: u8) -> Result<Vec<u8>, image::ImageError> {
    let mut output = Cursor::new(Vec::new());
    let rgb = image.to_rgb8();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut output, quality).write_image(
        rgb.as_raw(),
        rgb.width(),
        rgb.height(),
        image::ExtendedColorType::Rgb8,
    )?;
    Ok(output.into_inner())
}
