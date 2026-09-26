//! A deliberately narrow bridge from a decoded photograph to a verified local
//! multimodal model.
//!
//! This is not a generic prompt runner. [`discover_local_capability`] asks the
//! gateway which model serves the device-only `photos.vision` task and accepts
//! only a loopback, revision-pinned model that explicitly declares `vision`;
//! [`prepare`] bounds pixels and encoded bytes before any data crosses a
//! sandbox service boundary; and [`analyze`] calls the Photos-only fixed schema
//! gateway operation. The returned bytes are model output, not labels
//! inferred from filenames, EXIF, or fixtures.

use std::io::Cursor;

use agent_provider_config::{EndpointLocation, ModelCapability, is_pinned_revision, tasks};
use base64::Engine as _;
use image::{DynamicImage, GenericImageView, ImageEncoder};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use tilo_model_gateway::Route;

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

/// One model the signed local-model manager has published as vision capable.
/// The revision is kept alongside each result so an index can be refreshed
/// deliberately after a model upgrade rather than silently mixing outputs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalVisionCapability {
    pub model_id: String,
    pub model_label: String,
    pub revision: String,
}

/// A separately signed capability for face detection, embedding and local
/// clustering. It is never inferred from [`LocalVisionCapability`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalFaceCapability {
    pub model_id: String,
    pub model_label: String,
    pub revision: String,
}

/// Fixed Photos-only gateway request. The preview is bounded by
/// [`PreparedImage`]; embeddings/geometry stay in the loopback runtime and
/// are returned only as opaque local cluster membership there.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FaceClusterRequest {
    pub operation: String,
    pub source_content_hash: String,
}

/// Opaque membership returned by the verified face runtime.  Names,
/// embeddings, crops, geometry, and confidences are intentionally absent.
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

/// Discover the model that serves `photos.faces`: only an explicitly
/// declared, revision-pinned, device-local face model. A generic vision model
/// cannot enter this path.
#[must_use]
pub fn discover_local_face_capability() -> Option<LocalFaceCapability> {
    let route = tilo_model_gateway::resolve(tasks::PHOTOS_FACES.id, None).ok()?;
    face_capability(&route)
}

/// The face capability `route` offers, if it is an explicit, pinned model on
/// this device.
#[must_use]
pub fn face_capability(route: &Route) -> Option<LocalFaceCapability> {
    let revision = local_revision(route)?;
    route
        .model
        .declares(ModelCapability::FaceRecognition)
        .then(|| LocalFaceCapability {
            model_id: route.model.id.clone(),
            model_label: route.model.display_label().to_owned(),
            revision,
        })
}

fn local_revision(route: &Route) -> Option<String> {
    let revision = route.model.revision.as_deref()?.trim();
    (route.location == EndpointLocation::ThisDevice && is_pinned_revision(revision))
        .then(|| revision.to_owned())
}

/// A downscaled JPEG ready for the authenticated model-gateway boundary.
/// `source_content_hash` always identifies the original source bytes, never
/// the temporary preview.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedImage {
    pub media_type: &'static str,
    pub jpeg: Vec<u8>,
    pub source_content_hash: String,
    pub width: u32,
    pub height: u32,
}

/// The fixed answer shape requested from the model. Apps should keep this as
/// local, provisional analysis until a user explicitly shares it.
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
    Image(#[from] image::ImageError),
    #[error("local model gateway failed: {0}")]
    Gateway(#[from] tilo_model_gateway::ClientError),
    #[error("local model returned invalid analysis JSON")]
    InvalidResponse,
}

impl Error {
    /// A stable explanation safe to show in a Photos notice.
    #[must_use]
    pub const fn user_message(&self) -> &'static str {
        match self {
            Self::Unavailable => {
                "No verified local image-analysis model is ready. Install and start one in Settings."
            }
            Self::EmptyImage => "This image cannot be prepared for local analysis.",
            Self::Image(_) => "Photos could not prepare a safe preview for local analysis.",
            Self::Gateway(_) => {
                "The local image-analysis service is unavailable. Try again shortly."
            }
            Self::InvalidResponse => {
                "The local model returned an unusable result; nothing was added."
            }
        }
    }
}

/// Discover the model that serves `photos.vision` without trusting a model
/// name. `None` means unavailable—not that another provider may be
/// substituted.
#[must_use]
pub fn discover_local_capability() -> Option<LocalVisionCapability> {
    let route = tilo_model_gateway::resolve(tasks::PHOTOS_VISION.id, None).ok()?;
    vision_capability(&route)
}

/// The vision capability `route` offers, if it is an explicit, pinned model
/// on this device.
#[must_use]
pub fn vision_capability(route: &Route) -> Option<LocalVisionCapability> {
    let revision = local_revision(route)?;
    route
        .model
        .declares(ModelCapability::Vision)
        .then(|| LocalVisionCapability {
            model_id: route.model.id.clone(),
            model_label: route.model.display_label().to_string(),
            revision,
        })
}

/// Produce an adaptive JPEG preview from already decoded pixels. This function
/// caps both dimensions and encoded bytes, and preserves an original-content
/// SHA-256 for provenance. It never reads an arbitrary pathname.
///
/// # Errors
///
/// Returns [`Error::EmptyImage`] for an empty image and [`Error::Image`] if
/// JPEG encoding fails.
pub fn prepare(image: &DynamicImage, original_bytes: &[u8]) -> Result<PreparedImage, Error> {
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 || original_bytes.is_empty() {
        return Err(Error::EmptyImage);
    }
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
    // The final attempt is deliberately smaller rather than sending a frame
    // that would breach the gateway's limit.
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

/// Ask the Photos-only local gateway and validate its bounded fixed schema.
/// No remote provider fallback exists.
///
/// # Errors
///
/// Returns [`Error::Gateway`] when the launcher did not provide the gateway or
/// the verified model refuses the request, and [`Error::InvalidResponse`] for
/// malformed or oversized model output.
pub fn analyze(
    capability: &LocalVisionCapability,
    image: PreparedImage,
) -> Result<Analysis, Error> {
    let _ = capability;
    let output = tilo_model_gateway::analyze_image(tilo_model_gateway::VisionImage {
        media_type: image.media_type.to_string(),
        data_base64: base64::engine::general_purpose::STANDARD.encode(image.jpeg),
        source_content_hash: image.source_content_hash,
    })?;
    parse_analysis(&output)
}

/// Prepare and cluster one small, explicit face batch.  The caller supplies
/// stable Photos ids and decoded local images; no pathname, identity label,
/// face crop, or geometry is sent to the runtime.
///
/// # Errors
///
/// Returns [`Error::EmptyImage`] for an empty/oversized batch or unusable
/// image, and [`Error::InvalidResponse`] when the local runtime emits a
/// cluster outside the supplied photo ids.
pub fn cluster_faces(
    capability: &LocalFaceCapability,
    images: Vec<(String, DynamicImage, Vec<u8>)>,
) -> Result<FaceClusterResponse, Error> {
    if images.is_empty() || images.len() > MAX_FACE_BATCH {
        return Err(Error::EmptyImage);
    }
    let mut ids = std::collections::BTreeSet::new();
    let mut requests = Vec::with_capacity(images.len());
    for (photo_id, image, bytes) in images {
        if !valid_photo_id(&photo_id) || !ids.insert(photo_id.clone()) {
            return Err(Error::InvalidResponse);
        }
        let preview = prepare_face_preview(&image, &bytes)?;
        requests.push(tilo_model_gateway::FaceImage {
            photo_id,
            media_type: preview.media_type.to_owned(),
            data_base64: base64::engine::general_purpose::STANDARD.encode(preview.jpeg),
            source_content_hash: preview.source_content_hash,
        });
    }
    let _ = capability;
    let output = tilo_model_gateway::cluster_faces(requests)?;
    parse_face_clusters(&output, &ids)
}

/// Parse and bound a model response. Keeping this separate lets callers test
/// their persistence boundary without an inference runtime.
///
/// # Errors
///
/// Returns [`Error::InvalidResponse`] if neither the direct JSON result nor a
/// standard OpenAI-compatible response envelope contains the fixed schema.
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

/// Validate a face-runtime response against the exact image ids in the batch.
/// This prevents a model from creating a phantom photo association or leaking
/// an arbitrary string into the local People manager.
///
/// # Errors
/// Returns [`Error::InvalidResponse`] when the response (or the chat envelope
/// around it) is not valid JSON, or when any cluster has an invalid id, fewer
/// than two or more than the batch limit of photos, a duplicate photo, or a
/// photo that was not in the batch.
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

fn valid_photo_id(value: &str) -> bool {
    value.starts_with("photos.photo:")
        && value.len() <= 4_096
        && !value.chars().any(char::is_control)
}

fn valid_cluster_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 160
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

fn bounded_resize(image: &DynamicImage) -> DynamicImage {
    let (width, height) = image.dimensions();
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if width <= MAX_INPUT_EDGE && height <= MAX_INPUT_EDGE && pixels <= MAX_INPUT_PIXELS {
        return image.clone();
    }
    // The edge ceiling is deliberately below sqrt(MAX_INPUT_PIXELS), so this
    // integer-only resize establishes both limits without lossy arithmetic.
    image.resize(
        MAX_INPUT_EDGE,
        MAX_INPUT_EDGE,
        image::imageops::FilterType::Triangle,
    )
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

fn bounded_text(value: &str, limit: usize) -> String {
    value
        .chars()
        .take(limit)
        .collect::<String>()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use agent_provider_config::ModelDescriptor;
    use image::{DynamicImage, Rgba, RgbaImage};

    use super::*;

    fn route(vision: bool, location: EndpointLocation, revision: &str) -> Route {
        Route {
            task: tasks::PHOTOS_VISION.id.into(),
            profile_id: agent_provider_config::LOCAL_PROFILE_ID.into(),
            profile_label: "This device".into(),
            protocol: agent_provider_config::ProviderProtocol::OpenAiCompatible,
            location,
            model: ModelDescriptor {
                id: "vision-1".into(),
                label: "Vision 1".into(),
                context_window: Some(8_192),
                revision: Some(revision.into()),
                capabilities: if vision {
                    [ModelCapability::Vision].into()
                } else {
                    std::collections::BTreeSet::new()
                },
                tool_calling: false,
                ..ModelDescriptor::default()
            },
            options: agent_provider_config::ModelOptions::default(),
            layer: agent_provider_config::assign::Layer::Recommended,
        }
    }

    #[test]
    fn accepts_only_an_explicit_pinned_vision_model_on_this_device() {
        let pin = "a".repeat(40);
        assert!(vision_capability(&route(true, EndpointLocation::ThisDevice, &pin)).is_some());
        assert!(vision_capability(&route(false, EndpointLocation::ThisDevice, &pin)).is_none());
        assert!(vision_capability(&route(true, EndpointLocation::PrivateNetwork, &pin)).is_none());
        assert!(vision_capability(&route(true, EndpointLocation::ThisDevice, "main")).is_none());
        assert!(face_capability(&route(true, EndpointLocation::ThisDevice, &pin)).is_none());
    }

    #[test]
    fn preparation_hashes_original_and_limits_pixels() {
        let image =
            DynamicImage::ImageRgba8(RgbaImage::from_pixel(3_200, 2_000, Rgba([5, 7, 9, 255])));
        let prepared = prepare(&image, b"original photo bytes").unwrap();
        assert!(prepared.width <= MAX_INPUT_EDGE);
        assert!(prepared.height <= MAX_INPUT_EDGE);
        assert!(prepared.jpeg.len() <= MAX_ENCODED_PREVIEW_BYTES);
        assert_eq!(
            prepared.source_content_hash,
            hex::encode(Sha256::digest(b"original photo bytes"))
        );
    }

    #[test]
    fn parsing_rejects_unknown_fields_and_bounds_model_text() {
        assert!(parse_analysis(r#"{"description":"x","unexpected":true}"#).is_err());
        let parsed = parse_analysis(&format!(
            r#"{{"description":"{}","labels":["cat",""],"ocr":"note"}}"#,
            "x".repeat(MAX_DESCRIPTION_CHARS + 10)
        ))
        .unwrap();
        assert_eq!(parsed.description.chars().count(), MAX_DESCRIPTION_CHARS);
        assert_eq!(parsed.labels, vec!["cat"]);
    }

    #[test]
    fn parsing_accepts_only_the_openai_content_field_from_an_envelope() {
        let parsed = parse_analysis(
            r#"{"choices":[{"message":{"content":"{\"description\":\"A dog\",\"labels\":[\"dog\"],\"ocr\":\"\"}"}}]}"#,
        )
        .unwrap();
        assert_eq!(parsed.description, "A dog");
        assert_eq!(parsed.labels, vec!["dog"]);
    }

    #[test]
    fn pet_labels_are_model_output_and_face_clusters_cannot_escape_the_batch() {
        let parsed =
            parse_analysis(r#"{"description":"","labels":["dog"],"pets":["dog"],"ocr":""}"#)
                .unwrap();
        assert_eq!(parsed.pets, ["dog"]);
        let ids = std::collections::BTreeSet::from([
            "photos.photo:one".to_owned(),
            "photos.photo:two".to_owned(),
        ]);
        let accepted = parse_face_clusters(
            r#"{"clusters":[{"id":"local-1","photo_ids":["photos.photo:one","photos.photo:two"]}]}"#,
            &ids,
        )
        .unwrap();
        assert_eq!(accepted.clusters.len(), 1);
        assert!(parse_face_clusters(
            r#"{"clusters":[{"id":"local-1","photo_ids":["photos.photo:one","photos.photo:other"]}]}"#,
            &ids,
        )
        .is_err());
    }
}
