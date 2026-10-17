//! Finding the subject of a picture with the segmentation model the person
//! installed (Settings › Models), run here on the CPU.
//!
//! The model is a verified, content-addressed file in the local model store
//! (`local_model_service`); the gateway loads it once and keeps it while it
//! runs, so only the first request pays for loading. The caller sends one
//! bounded picture and gets back a greyscale matte; it never learns where
//! the model lives, and nothing leaves the device.

use std::io::Cursor;
use std::sync::Mutex;
use std::time::Instant;

use crate::{NO_SEGMENTATION_MODEL, SubjectMatte, VisionImage};
use base64::Engine as _;
use tilo_lock::MutexRecover as _;
use tilo_segment::{Family, Picture, SubjectModel};

/// The longest side of a picture the gateway decodes; callers send at most
/// `tilo_vision::MAX_SUBJECT_EDGE`.
const MAX_EDGE: u32 = 2048;

/// The model loaded, and which file it came from.
struct Loaded {
    sha256: String,
    model_id: String,
    revision: String,
    model: SubjectModel,
}

/// One model at a time, and one picture through it at a time: the CPU it
/// runs on is the desktop's.
static LOADED: Mutex<Option<Loaded>> = Mutex::new(None);

/// How a catalogued model wants its picture.
fn family(model_id: &str) -> Option<Family> {
    if model_id.starts_with("u2net") {
        Some(Family::U2Net)
    } else if model_id.starts_with("isnet") {
        Some(Family::IsNet)
    } else {
        None
    }
}

/// Threads for one inference: half the machine, so the desktop stays
/// responsive while it runs.
fn threads() -> usize {
    std::thread::available_parallelism()
        .map_or(2, std::num::NonZeroUsize::get)
        .div_ceil(2)
        .clamp(1, 8)
}

/// Find the subject of `image` (see the module docs).
pub fn segment(image: &VisionImage) -> Result<String, String> {
    super::validate_vision_image(image)?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&image.data_base64)
        .map_err(|_| "the picture is not valid base64".to_string())?;
    let decoded =
        image::load_from_memory(&bytes).map_err(|_| "the picture cannot be decoded".to_string())?;
    if decoded.width() == 0
        || decoded.height() == 0
        || decoded.width().max(decoded.height()) > MAX_EDGE
    {
        return Err(format!("the picture must be 1 to {MAX_EDGE} pixels a side"));
    }
    let rgba = decoded.to_rgba8();
    let root = local_model_service::user_model_root()
        .ok_or_else(|| "the model store has no data home".to_string())?;
    let installed = local_model_service::installed_in_process_model(
        &root,
        local_model_service::InProcessCapability::Segmentation,
    )
    .map_err(|error| format!("the model store cannot be read: {error}"))?
    .ok_or_else(|| NO_SEGMENTATION_MODEL.to_string())?;
    let family = family(&installed.id).ok_or_else(|| {
        format!(
            "{} is not a subject model this gateway can run",
            installed.id
        )
    })?;
    let mut loaded = LOADED.lock_recover();
    let began = Instant::now();
    if loaded
        .as_ref()
        .is_none_or(|loaded| loaded.sha256 != installed.sha256)
    {
        let file = std::fs::read(&installed.path)
            .map_err(|error| format!("the subject model cannot be read: {error}"))?;
        // The store checks only the file's size when it is looked up; the
        // bytes run here are checked against the digest the signed
        // catalogue pinned, once per load.
        let hashing = Instant::now();
        if hex_digest(&file) != installed.sha256 {
            return Err(TAMPERED.to_string());
        }
        if std::env::var_os("TILO_MODEL_GATEWAY_TIMING").is_none_or(|value| value != "0") {
            eprintln!(
                "tilo-model-gateway: timing subject model={} bytes={} hash_us={}",
                installed.id,
                file.len(),
                hashing.elapsed().as_micros(),
            );
        }
        let model = SubjectModel::load(file, family)
            .map_err(|error| error.to_string())?
            .with_threads(threads());
        *loaded = Some(Loaded {
            sha256: installed.sha256.clone(),
            model_id: installed.id.clone(),
            revision: installed.revision,
            model,
        });
    }
    let load_ms = millis(began);
    let Some(current) = loaded.as_ref() else {
        return Err(NO_SEGMENTATION_MODEL.to_string());
    };
    let began = Instant::now();
    let matte = current
        .model
        .matte(&Picture {
            rgba: rgba.as_raw(),
            width: rgba.width() as usize,
            height: rgba.height() as usize,
        })
        .map_err(|error| error.to_string())?;
    let infer_ms = millis(began);
    let (model_id, revision) = (current.model_id.clone(), current.revision.clone());
    drop(loaded);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a coverage clamped to 0..=1 scaled to a byte"
    )]
    let grey: Vec<u8> = matte
        .values
        .iter()
        .map(|value| (value.clamp(0.0, 1.0) * 255.0).round() as u8)
        .collect();
    let grey = image::GrayImage::from_raw(rgba.width(), rgba.height(), grey)
        .ok_or_else(|| "the matte has the wrong size".to_string())?;
    let mut png = Cursor::new(Vec::new());
    grey.write_to(&mut png, image::ImageFormat::Png)
        .map_err(|error| format!("the matte cannot be encoded: {error}"))?;
    serde_json::to_string(&SubjectMatte {
        width: rgba.width(),
        height: rgba.height(),
        matte_png_base64: base64::engine::general_purpose::STANDARD.encode(png.into_inner()),
        model_id,
        revision,
        load_ms,
        infer_ms,
    })
    .map_err(|error| error.to_string())
}

/// Why a model whose bytes no longer match their pinned digest is refused.
const TAMPERED: &str = "the subject model on this device does not match its verified download; remove it and set it up again in Settings › Models";

/// The SHA-256 of `bytes` in lowercase hexadecimal, as the store names files.
fn hex_digest(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    hex::encode(Sha256::digest(bytes))
}

fn millis(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_digest_is_spelt_as_the_store_names_files() {
        assert_eq!(
            hex_digest(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn only_known_model_families_run() {
        assert_eq!(family("u2net-subject"), Some(Family::U2Net));
        assert_eq!(family("isnet-general"), Some(Family::IsNet));
        assert_eq!(family("smolvlm2-500m"), None);
    }

    #[test]
    fn a_picture_that_is_not_one_is_refused_before_any_model() {
        let image = VisionImage {
            media_type: "image/png".into(),
            data_base64: base64::engine::general_purpose::STANDARD.encode(b"not a picture"),
            source_content_hash: "0".repeat(64),
        };
        assert_eq!(segment(&image), Err("the picture cannot be decoded".into()));
    }
}
