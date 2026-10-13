//! Local, bounded image intelligence.
//!
//! Recognition happens from pixels available on this device.  The helper does
//! not write a document, start a network request, or retain an image; callers
//! choose whether a result stays local or becomes an explicit shared action.

use crate::model::{AnalysisProvenance, ImageAnalysis, OpenImage};
use crate::people::{VerifiedFaceCluster, VerifiedFaceClusterBatch};
use crate::pixels;
use crate::strings::tr;
use tilo_content::ContentStore;

/// Maximum text retained from an OCR engine.  This is deliberately below the
/// CRDT's practical change budget and prevents a pathological scan becoming a
/// collaborative denial-of-service payload.
pub const MAX_OCR_CHARS: usize = 32 * 1024;

/// Run the shared Tilo OCR pipeline on a locally available photograph.
///
/// # Errors
/// Returns a stable, non-sensitive error when the bytes are unavailable, not
/// decodable, or OCR cannot run. Engine diagnostics and source paths never
/// cross this boundary.
pub fn recognize_text(
    open: &OpenImage,
    language: &str,
    store: &ContentStore,
) -> Result<ImageAnalysis, String> {
    if !valid_language(language) {
        return Err(tr("photos-analysis-error-language"));
    }
    let bytes =
        crate::model::bytes_of(open, store).ok_or_else(|| tr("photos-analysis-error-not-here"))?;
    let rgba = pixels::decode(&bytes).ok_or_else(|| tr("photos-analysis-error-unreadable"))?;
    let image = image::RgbaImage::from_raw(rgba.w, rgba.h, rgba.bytes)
        .ok_or_else(|| tr("photos-analysis-error-unreadable"))?;
    let document = tilo_ocr::recognize(&image::DynamicImage::ImageRgba8(image), language)
        .map_err(|error| error.user_message().to_owned())?;
    let mut text = document.plain_text();
    truncate_chars(&mut text, MAX_OCR_CHARS);
    let provenance = AnalysisProvenance {
        source: "local-ocr".to_owned(),
        engine: "tilo-ocr".to_owned(),
        model_id: String::new(),
        model_revision: String::new(),
        language: language.to_owned(),
        content: open.content.clone(),
        // This return value is intentionally local. `SetImageAnalysis`
        // changes it to shared only at the collaborative consent boundary.
        scope: "local".to_owned(),
    };
    Ok(ImageAnalysis {
        ocr_text: text,
        provenance: provenance.clone(),
        ocr_provenance: Some(provenance),
        ..ImageAnalysis::default()
    })
}

/// Run the fixed-schema local vision capability on locally available bytes.
///
/// This deliberately has no remote fallback. The caller must have discovered
/// a signed, loopback capability and chosen this operation explicitly; only
/// the compact result is returned for the caller to decide whether to share.
///
/// # Errors
///
/// Returns a stable, non-sensitive message if local bytes are unavailable or
/// unsafe to decode/prepare, or if the signed local service rejects the fixed
/// schema request.
pub fn analyze_image(
    open: &OpenImage,
    capability: &tilo_vision::LocalVisionCapability,
    store: &ContentStore,
) -> Result<ImageAnalysis, String> {
    let bytes =
        crate::model::bytes_of(open, store).ok_or_else(|| tr("photos-analysis-error-not-here"))?;
    let rgba = pixels::decode(&bytes).ok_or_else(|| tr("photos-analysis-error-unreadable"))?;
    let image = image::RgbaImage::from_raw(rgba.w, rgba.h, rgba.bytes)
        .ok_or_else(|| tr("photos-analysis-error-unreadable"))?;
    let prepared = tilo_vision::prepare(&image::DynamicImage::ImageRgba8(image), &bytes)
        .map_err(|error| error.user_message().to_owned())?;
    let result = tilo_vision::analyze(capability, prepared)
        .map_err(|error| error.user_message().to_owned())?;
    Ok(image_analysis_from_vision_result(open, capability, result))
}

/// Detect and cluster faces for one bounded, explicitly selected library
/// batch.  Decoding and preview creation happen on the Photos worker; the
/// model receives only JPEG previews and stable photo ids.  The returned
/// memberships remain local and must be reviewed before any person label is
/// shared.
///
/// # Errors
///
/// Returns a user-facing message when the batch is empty or larger than
/// [`tilo_vision::MAX_FACE_BATCH`], when a photograph's bytes are not yet
/// available locally or cannot be decoded, or when face clustering fails.
pub fn detect_people(
    images: Vec<(String, OpenImage)>,
    capability: &tilo_vision::LocalFaceCapability,
    store: &ContentStore,
) -> Result<VerifiedFaceClusterBatch, String> {
    if images.is_empty() || images.len() > tilo_vision::MAX_FACE_BATCH {
        return Err(tr("photos-people-error-batch"));
    }
    let mut prepared = Vec::with_capacity(images.len());
    for (photo_id, open) in images {
        let bytes = crate::model::bytes_of(&open, store).ok_or_else(|| {
            "A selected photograph is not available on this device yet; retry after it syncs"
                .to_owned()
        })?;
        let rgba = pixels::decode(&bytes).ok_or_else(|| {
            "A selected photograph cannot be safely prepared for People recognition".to_owned()
        })?;
        let image = image::RgbaImage::from_raw(rgba.w, rgba.h, rgba.bytes)
            .ok_or_else(|| "A selected photograph has invalid decoded pixels".to_owned())?;
        prepared.push((photo_id, image::DynamicImage::ImageRgba8(image), bytes));
    }
    let result = tilo_vision::cluster_faces(capability, prepared)
        .map_err(|error| error.user_message().to_owned())?;
    Ok(VerifiedFaceClusterBatch {
        model_id: capability.model_id.clone(),
        revision: capability.revision.clone(),
        clusters: result
            .clusters
            .into_iter()
            .map(|cluster| VerifiedFaceCluster {
                // The model's opaque id is only scoped to one request. Make
                // it stable across redraws yet collision-free across bounded
                // library batches before it enters the local review store.
                id: format!(
                    "face:{}",
                    tilo_content::hash_bytes(
                        format!(
                            "{}\0{}\0{}\0{}",
                            capability.model_id,
                            capability.revision,
                            cluster.id,
                            cluster.photo_ids.join("\0")
                        )
                        .as_bytes()
                    )
                ),
                photo_ids: cluster.photo_ids,
            })
            .collect(),
    })
}

fn image_analysis_from_vision_result(
    open: &OpenImage,
    capability: &tilo_vision::LocalVisionCapability,
    result: tilo_vision::Analysis,
) -> ImageAnalysis {
    let provenance = AnalysisProvenance {
        source: "local-vision".to_owned(),
        engine: "tilo-vision".to_owned(),
        model_id: capability.model_id.clone(),
        model_revision: capability.revision.clone(),
        language: String::new(),
        content: open.content.clone(),
        scope: "local".to_owned(),
    };
    let has_ocr = !result.ocr.is_empty();
    ImageAnalysis {
        ocr_text: result.ocr,
        description: result.description,
        // The fixed schema keeps generic objects and explicitly visible pets
        // separate. Neither result is ever upgraded into a person identity.
        objects: result.labels,
        pets: result.pets,
        provenance: provenance.clone(),
        ocr_provenance: has_ocr.then(|| provenance.clone()),
        vision_provenance: Some(provenance),
    }
}

fn valid_language(language: &str) -> bool {
    !language.is_empty()
        && language.len() <= 16
        && language
            .bytes()
            .all(|byte| byte.is_ascii_alphabetic() || byte == b'_')
}

fn truncate_chars(value: &mut String, max: usize) {
    if value.chars().count() > max {
        let end = value
            .char_indices()
            .nth(max)
            .map_or(value.len(), |(index, _)| index);
        value.truncate(end);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_analysis_refuses_bad_language_truncates_safely_and_pins_vision_models() {
        eprintln!("phase invalid_language_is_refused_before_reading_pixels");
        {
            let open = OpenImage::fixture("/not/read.png", 1, 1);
            assert!(recognize_text(&open, "eng; rm -rf", &ContentStore::memory()).is_err());
        }

        eprintln!("phase truncation_keeps_unicode_boundaries");
        {
            let mut value = "ééé".to_owned();
            truncate_chars(&mut value, 2);
            assert_eq!(value, "éé");
        }

        eprintln!("phase vision_output_stays_generic_and_records_the_pinned_model");
        {
            let open = OpenImage::new("vision.png", 3, 2, "b".repeat(64));
            let capability = tilo_vision::LocalVisionCapability {
                profile_id: "local".to_owned(),
                model_id: "vision-model".to_owned(),
                model_label: "Vision model".to_owned(),
                revision: "a".repeat(64),
            };
            let analysis = image_analysis_from_vision_result(
                &open,
                &capability,
                tilo_vision::Analysis {
                    description: "A dog beside a bicycle".to_owned(),
                    labels: vec!["dog".to_owned(), "bicycle".to_owned()],
                    pets: vec!["dog".to_owned()],
                    ocr: "42 North Street".to_owned(),
                },
            );
            assert_eq!(analysis.objects, ["dog", "bicycle"]);
            assert_eq!(analysis.pets, ["dog"]);
            assert_eq!(analysis.ocr_text, "42 North Street");
            assert_eq!(analysis.provenance.source, "local-vision");
            assert_eq!(analysis.provenance.model_id, "vision-model");
            assert_eq!(analysis.provenance.model_revision, "a".repeat(64));
        }
    }
}
