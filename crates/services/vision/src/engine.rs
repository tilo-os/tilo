//! The bound between a decoded picture and the verified local model: what
//! may be sent, and how the answer is read.
//!
//! [`discover_local_capability`] accepts only a configured, loopback,
//! revision-pinned model that declares `vision`; [`prepare`] bounds pixels and
//! encoded bytes before data crosses any boundary; [`analyze`] asks the
//! fixed-schema gateway operation and [`parse_analysis`] bounds its answer.

use tilo_image_codec::{DecodedImage, EncodeError, Encoding};

use agent_provider_config::{ModelDescriptor, ProviderConfig};
#[cfg(unix)]
use base64::Engine as _;
use image::{DynamicImage, GenericImageView};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;

pub const MAX_INPUT_EDGE: u32 = 1_536;
pub const MAX_INPUT_PIXELS: u64 = 8_000_000;
pub const MAX_ENCODED_PREVIEW_BYTES: usize = 1_024 * 1_024;
pub const MAX_DESCRIPTION_CHARS: usize = 480;
pub const MAX_LABELS: usize = 24;
pub const MAX_LABEL_CHARS: usize = 96;
pub const MAX_OCR_CHARS: usize = 24 * 1024;
pub const MAX_FACE_BATCH: usize = 12;
pub const MAX_FACE_PREVIEW_BYTES: usize = 128 * 1024;
pub const MAX_FACE_PREVIEW_EDGE: u32 = 512;

/// A model the local-model manager published as vision capable. The revision
/// is kept with each result so an upgrade never silently mixes outputs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalVisionCapability {
    pub profile_id: String,
    pub model_id: String,
    pub model_label: String,
    pub revision: String,
}

/// A separately signed capability for face detection, embedding and local
/// clustering, never inferred from [`LocalVisionCapability`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalFaceCapability {
    pub profile_id: String,
    pub model_id: String,
    pub model_label: String,
    pub revision: String,
}

/// What the face runtime returns: membership only. Names, embeddings, crops,
/// geometry and confidences are absent.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FaceClusterResponse {
    #[serde(default)]
    pub clusters: Vec<FaceCluster>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FaceCluster {
    pub id: String,
    pub photo_ids: Vec<String>,
}

/// Discover only an explicitly declared, revision-pinned, loopback face
/// model.
#[must_use]
pub fn discover_local_face_capability(config: &ProviderConfig) -> Option<LocalFaceCapability> {
    let (profile_id, model_id, model_label, revision) =
        discover_local_model(config, |model| model.face_recognition)?;
    Some(LocalFaceCapability {
        profile_id,
        model_id,
        model_label,
        revision,
    })
}

/// A downscaled JPEG for the model gateway. `source_content_hash` identifies
/// the original source bytes, never the preview.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedImage {
    pub media_type: &'static str,
    pub jpeg: Vec<u8>,
    pub source_content_hash: String,
    pub width: u32,
    pub height: u32,
}

/// The fixed answer shape requested from the model: provisional analysis until
/// a user shares it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Analysis {
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub pets: Vec<String>,
    #[serde(default)]
    pub ocr: String,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("no signed, ready local vision model is available")]
    Unavailable,
    #[error("decoded image has no usable pixels")]
    EmptyImage,
    #[error("image encoding failed: {0}")]
    Image(#[from] EncodeError),
    #[error("local model gateway failed: {0}")]
    Gateway(#[from] tilo_model_client::ClientError),
    #[error("local model returned invalid analysis JSON")]
    InvalidResponse,
}

impl Error {
    /// A short explanation for a person: what did not happen, in words.
    #[must_use]
    pub const fn user_message(&self) -> &'static str {
        match self {
            Self::Unavailable => {
                "No verified local image-analysis model is ready. Install and start one in Settings."
            }
            Self::EmptyImage => "This image cannot be prepared for local analysis.",
            Self::Image(_) => "A safe preview of the image could not be made for local analysis.",
            Self::Gateway(_) => {
                "The local image-analysis service is unavailable. Try again shortly."
            }
            Self::InvalidResponse => {
                "The local model returned an unusable result; nothing was added."
            }
        }
    }
}

/// Discover the selected local capability without probing a socket or trusting
/// a model name. `None`, never another provider.
#[must_use]
pub fn discover_local_capability(config: &ProviderConfig) -> Option<LocalVisionCapability> {
    let (profile_id, model_id, model_label, revision) =
        discover_local_model(config, |model| model.vision)?;
    Some(LocalVisionCapability {
        profile_id,
        model_id,
        model_label,
        revision,
    })
}

/// The active profile's default model when it would be admitted for a
/// device-local operation carrying the needed capability.
fn discover_local_model(
    config: &ProviderConfig,
    declares: impl Fn(&ModelDescriptor) -> bool,
) -> Option<(String, String, String, String)> {
    let profile = config.active_profile()?;
    if !profile.enabled || !tilo_model_client::is_supervised_loopback(profile) {
        return None;
    }
    let model_id = profile.default_model.as_ref()?;
    let model = profile.models.iter().find(|model| &model.id == model_id)?;
    let revision = model.revision.as_ref()?;
    if !declares(model) || !tilo_model_client::is_pinned_revision(revision) {
        return None;
    }
    Some((
        profile.id.clone(),
        model.id.clone(),
        model.display_label().to_owned(),
        revision.clone(),
    ))
}

/// An adaptive JPEG preview from decoded pixels, bounded in both dimensions
/// and encoded bytes, with the original's SHA-256 for provenance. It never
/// reads a pathname.
pub fn prepare(image: &DecodedImage, original_bytes: &[u8]) -> Result<PreparedImage, Error> {
    let image = dynamic(image)?;
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 || original_bytes.is_empty() {
        return Err(Error::EmptyImage);
    }
    let image = &image;
    let mut preview = bounded_resize(image);
    for quality in [85, 74, 62, 50] {
        let encoded = encode_jpeg(&preview, quality)?;
        if encoded.len() <= MAX_ENCODED_PREVIEW_BYTES {
            let (width, height) = preview.dimensions();
            return Ok(PreparedImage {
                media_type: "image/jpeg",
                jpeg: encoded,
                source_content_hash: hex::encode(Sha256::digest(original_bytes)),
                width,
                height,
            });
        }
        let (width, height) = preview.dimensions();
        let next_width = (width.saturating_mul(3) / 4).max(1);
        let next_height = (height.saturating_mul(3) / 4).max(1);
        preview = preview.resize_exact(
            next_width,
            next_height,
            image::imageops::FilterType::Triangle,
        );
    }
    // The last attempt is smaller rather than over the gateway's limit.
    let (width, height) = preview.dimensions();
    let preview = preview.resize_exact(
        (width / 2).max(1),
        (height / 2).max(1),
        image::imageops::FilterType::Triangle,
    );
    let encoded = encode_jpeg(&preview, 42)?;
    if encoded.len() > MAX_ENCODED_PREVIEW_BYTES {
        return Err(Error::EmptyImage);
    }
    let (width, height) = preview.dimensions();
    Ok(PreparedImage {
        media_type: "image/jpeg",
        jpeg: encoded,
        source_content_hash: hex::encode(Sha256::digest(original_bytes)),
        width,
        height,
    })
}

/// Ask the gateway's fixed-schema analysis on `descriptor`, the connection
/// its caller holds.
///
/// # Errors
/// [`Error::Gateway`] when the connection fails or the model is refused, and
/// [`Error::InvalidResponse`] for an answer outside the fixed schema.
#[cfg(unix)]
pub fn analyze_on(
    descriptor: std::os::fd::BorrowedFd<'_>,
    capability: &LocalVisionCapability,
    image: PreparedImage,
) -> Result<Analysis, Error> {
    let request = request_of(image);
    let output = tilo_model_client::analyze_image_on(
        descriptor,
        &capability.profile_id,
        &capability.model_id,
        request,
    )?;
    parse_analysis(&output)
}

// The model gateway is reached over a Unix socket's descriptor; only Linux runs one.
#[cfg(unix)]
/// The gateway request one prepared image is sent as.
fn request_of(image: PreparedImage) -> tilo_model_client::VisionImage {
    tilo_model_client::VisionImage {
        media_type: image.media_type.to_string(),
        data_base64: base64::engine::general_purpose::STANDARD.encode(image.jpeg),
        source_content_hash: image.source_content_hash,
    }
}

/// Prepare and cluster one small, explicit face batch on `descriptor`, the
/// connection its caller holds.
///
/// # Errors
/// [`Error::EmptyImage`] for an empty or oversized batch or an unusable image,
/// [`Error::InvalidResponse`] for a cluster outside the supplied ids, and
/// [`Error::Gateway`] when the connection fails or the model is refused.
#[cfg(unix)]
pub fn cluster_faces_on(
    descriptor: std::os::fd::BorrowedFd<'_>,
    capability: &LocalFaceCapability,
    images: Vec<(String, DecodedImage, Vec<u8>)>,
) -> Result<FaceClusterResponse, Error> {
    let (ids, requests) = face_requests(images)?;
    let output = tilo_model_client::cluster_faces_on(
        descriptor,
        &capability.profile_id,
        &capability.model_id,
        requests,
    )?;
    parse_face_clusters(&output, &ids)
}

// The model gateway is reached over a Unix socket's descriptor; only Linux runs one.
#[cfg(unix)]
/// The bounded batch a face request is sent as: the ids it may answer about,
/// and each image's preview.
fn face_requests(
    images: Vec<(String, DecodedImage, Vec<u8>)>,
) -> Result<
    (
        std::collections::BTreeSet<String>,
        Vec<tilo_model_client::FaceImage>,
    ),
    Error,
> {
    if images.is_empty() || images.len() > MAX_FACE_BATCH {
        return Err(Error::EmptyImage);
    }
    let mut ids = std::collections::BTreeSet::new();
    let mut requests = Vec::with_capacity(images.len());
    for (photo_id, image, bytes) in images {
        if !valid_image_id(&photo_id) || !ids.insert(photo_id.clone()) {
            return Err(Error::InvalidResponse);
        }
        let preview = prepare_face_preview(&dynamic(&image)?, &bytes)?;
        requests.push(tilo_model_client::FaceImage {
            photo_id,
            media_type: preview.media_type.to_owned(),
            data_base64: base64::engine::general_purpose::STANDARD.encode(preview.jpeg),
            source_content_hash: preview.source_content_hash,
        });
    }
    Ok((ids, requests))
}

/// Parse and bound a model response, so callers test their persistence
/// boundary without an inference runtime.
pub fn parse_analysis(raw: &str) -> Result<Analysis, Error> {
    let mut result = serde_json::from_str::<Analysis>(raw)
        .or_else(|_| {
            let envelope = serde_json::from_str::<serde_json::Value>(raw)
                .map_err(|_| serde_json::Error::io(std::io::Error::other("invalid envelope")))?;
            let content = envelope
                .pointer("/choices/0/message/content")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    serde_json::Error::io(std::io::Error::other("missing model content"))
                })?;
            serde_json::from_str::<Analysis>(content)
        })
        .map_err(|_| Error::InvalidResponse)?;
    result.description = bounded_text(&result.description, MAX_DESCRIPTION_CHARS);
    result.ocr = bounded_text(&result.ocr, MAX_OCR_CHARS);
    result.labels = result
        .labels
        .into_iter()
        .map(|label| bounded_text(&label, MAX_LABEL_CHARS))
        .filter(|label| !label.is_empty())
        .take(MAX_LABELS)
        .collect();
    result.pets = result
        .pets
        .into_iter()
        .map(|label| bounded_text(&label, MAX_LABEL_CHARS))
        .filter(|label| !label.is_empty())
        .take(MAX_LABELS)
        .collect();
    Ok(result)
}

/// Validate a face-runtime response against the image ids in the batch, so a
/// model cannot create a phantom photo association.
///
/// # Errors
/// [`Error::InvalidResponse`] when the response (or the chat envelope around
/// it) is not JSON, or a cluster has an invalid id, fewer than two or more
/// than the batch limit of photos, a duplicate photo, or one not in the
/// batch.
pub fn parse_face_clusters(
    raw: &str,
    allowed_photo_ids: &std::collections::BTreeSet<String>,
) -> Result<FaceClusterResponse, Error> {
    let response = serde_json::from_str::<FaceClusterResponse>(raw)
        .or_else(|_| {
            let envelope = serde_json::from_str::<serde_json::Value>(raw)
                .map_err(|_| serde_json::Error::io(std::io::Error::other("invalid envelope")))?;
            let content = envelope
                .pointer("/choices/0/message/content")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    serde_json::Error::io(std::io::Error::other("missing model content"))
                })?;
            serde_json::from_str::<FaceClusterResponse>(content)
        })
        .map_err(|_| Error::InvalidResponse)?;
    if response.clusters.len() > MAX_FACE_BATCH
        || response.clusters.iter().any(|cluster| {
            !valid_cluster_id(&cluster.id)
                || cluster.photo_ids.len() < 2
                || cluster.photo_ids.len() > MAX_FACE_BATCH
                || cluster
                    .photo_ids
                    .iter()
                    .any(|id| !allowed_photo_ids.contains(id))
                || {
                    let unique: std::collections::BTreeSet<_> = cluster.photo_ids.iter().collect();
                    unique.len() != cluster.photo_ids.len()
                }
        })
    {
        return Err(Error::InvalidResponse);
    }
    Ok(response)
}

// The model gateway is reached over a Unix socket's descriptor; only Linux runs one.
#[cfg(unix)]
fn prepare_face_preview(
    image: &DynamicImage,
    original_bytes: &[u8],
) -> Result<PreparedImage, Error> {
    if image.width() == 0 || image.height() == 0 || original_bytes.is_empty() {
        return Err(Error::EmptyImage);
    }
    let mut preview = image.resize(
        MAX_FACE_PREVIEW_EDGE,
        MAX_FACE_PREVIEW_EDGE,
        image::imageops::FilterType::Triangle,
    );
    for quality in [78, 65, 52, 42] {
        let jpeg = encode_jpeg(&preview, quality)?;
        if jpeg.len() <= MAX_FACE_PREVIEW_BYTES {
            return Ok(PreparedImage {
                media_type: "image/jpeg",
                jpeg,
                source_content_hash: hex::encode(Sha256::digest(original_bytes)),
                width: preview.width(),
                height: preview.height(),
            });
        }
        preview = preview.resize(
            (preview.width().saturating_mul(3) / 4).max(1),
            (preview.height().saturating_mul(3) / 4).max(1),
            image::imageops::FilterType::Triangle,
        );
    }
    Err(Error::EmptyImage)
}

// The model gateway is reached over a Unix socket's descriptor; only Linux runs one.
#[cfg(unix)]
/// A caller's name for one image of a face batch: opaque to the model, so any
/// stable address the caller can map back (an app resource id, a content
/// address) up to a bound.
fn valid_image_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 4_096 && !value.chars().any(char::is_control)
}

fn valid_cluster_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 160
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

/// The decoded pixels in the form the encoder takes.
fn dynamic(image: &DecodedImage) -> Result<DynamicImage, Error> {
    image::RgbaImage::from_raw(image.width, image.height, image.rgba.clone())
        .map(DynamicImage::ImageRgba8)
        .ok_or(Error::EmptyImage)
}

fn bounded_resize(image: &DynamicImage) -> DynamicImage {
    let (width, height) = image.dimensions();
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if width <= MAX_INPUT_EDGE && height <= MAX_INPUT_EDGE && pixels <= MAX_INPUT_PIXELS {
        return image.clone();
    }
    // The edge ceiling is below sqrt(MAX_INPUT_PIXELS), so this integer resize meets both limits.
    image.resize(
        MAX_INPUT_EDGE,
        MAX_INPUT_EDGE,
        image::imageops::FilterType::Triangle,
    )
}

fn encode_jpeg(image: &DynamicImage, quality: u8) -> Result<Vec<u8>, EncodeError> {
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    tilo_image_codec::encode_now(
        &DecodedImage {
            rgba: rgba.into_raw(),
            width,
            height,
        },
        Encoding::Jpeg { quality },
    )
}

pub(crate) fn bounded_text(value: &str, limit: usize) -> String {
    value
        .chars()
        .take(limit)
        .collect::<String>()
        .trim()
        .to_string()
}
