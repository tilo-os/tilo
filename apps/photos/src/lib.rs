//! Photos — view a photograph, adjust it, and get the result out. A viewer with a small set of
//! edits: * **Open** an image through the host XDG portal chooser (never a bespoke dialog), or from
//! a path on the command line.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "app-local arithmetic; see the note above"
)]
#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod actions;
pub mod analysis;
pub mod app;
pub mod derived;
mod files;
pub mod import;
pub mod library_analysis;
pub mod model;
pub mod people;
pub mod places;
pub mod reference;
pub mod resource;
#[cfg(feature = "samples")]
pub mod sample;
pub mod search;
pub mod strings;
mod timeline_projection;

pub use tilo_image_codec::DecodedImage;

pub use app::PhotosApp;
pub use model::{Edits, LocationProvenance, OpenImage, PhotoLocation, Rect01};

/// The most a photograph's encoded bytes may be: the bound an import, an open and a save read
/// under, so a mistaken file cannot exhaust the session.
pub const MAX_ENCODED_BYTES: usize = 64 * 1024 * 1024;

/// The most pixels a photograph may decode to. RGBA8 needs four bytes a pixel, so this caps one
/// picture at about 128 MiB, and a second buffer while an edit applies stays bounded too.
pub const MAX_DECODED_PIXELS: u64 = 32 * 1024 * 1024;

/// `bytes` decoded as the upright picture a viewer shows: the codec turns the pixels the way the
/// container's EXIF orientation says they are shown.
pub fn decode_image(bytes: &[u8]) -> Option<DecodedImage> {
    tilo_image_codec::decode_upright_now(bytes, None).ok()
}

/// [`decode_image`] away from the calling thread: a photograph decoded by the session's runtime.
pub async fn decode_image_async(bytes: &[u8]) -> Option<DecodedImage> {
    tilo_image_codec::decode_upright(bytes, None).await.ok()
}

/// The upright size `bytes` state, when they are an image this app opens within its bounds.
pub fn image_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() > MAX_ENCODED_BYTES {
        return None;
    }
    tilo_image_codec::sniff(bytes)?;
    let info = tilo_image_codec::info::read(bytes)?;
    let (width, height) = (info.width, info.height);
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    (width != 0 && height != 0 && pixels <= MAX_DECODED_PIXELS).then_some((width, height))
}

/// `image` encoded as a PNG: what an edited photograph is saved, copied and exported as.
pub fn encode_png(image: &DecodedImage) -> Option<Vec<u8>> {
    tilo_image_codec::encode_now(image, tilo_image_codec::Encoding::Png).ok()
}

pub fn camera_import_photo(
    media_id: &str,
    name: &str,
    store: &tilo_content::ContentStore,
) -> Result<model::LibraryPhoto, String> {
    let reference = media_id
        .strip_prefix("media:blake3:")
        .filter(|reference| tilo_content::valid_reference(reference))
        .ok_or_else(|| crate::strings::tr("photos-camera-error-identity"))?;
    let bytes = store
        .read(reference)
        .ok_or_else(|| crate::strings::tr("photos-camera-error-not-here"))?;
    if bytes.is_empty() || bytes.len() > MAX_ENCODED_BYTES {
        return Err(crate::strings::tr("photos-camera-error-integrity"));
    }
    let image =
        image_size(&bytes).ok_or_else(|| crate::strings::tr("photos-camera-error-unsupported"))?;
    Ok(model::LibraryPhoto::from_image(OpenImage::new(
        name,
        image.0,
        image.1,
        ui::tilo_resource::Bytes::kept_as(&bytes, reference),
    )))
}

/// The app as every host mounts it.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(PhotosShell)
}

pub use model::PhotoLibrary;
pub use resource::PhotosShell;
#[cfg(feature = "samples")]
pub use sample::{shots_pattern, shots_pattern_variant};

#[cfg(test)]
mod camera_import_tests {
    use tilo_content::ContentStore;
    use ui::tilo_resource::Replica;

    use super::{DecodedImage, camera_import_photo, encode_png};
    use crate::model::PhotoLibrary;

    tilo_test_env::scenario! {
        fn camera_import_verifies_real_content_before_naming_one_photograph;

        phase camera_import_verifies_bytes_and_imports_one_content_addressed_photo;
        {
            let rgba = DecodedImage::from_rgba(1, 1, vec![12, 34, 56, 255]).expect("fixture rgba");
            let bytes = encode_png(&rgba).expect("fixture png");
            let store = ContentStore::memory();
            let hash = store.put(&bytes).expect("fixture content");
            let media_id = format!("media:blake3:{hash}");
            let mut library = Replica::<PhotoLibrary>::new();

            let photo =
                camera_import_photo(&media_id, "Camera photo.png", &store).expect("verified");
            let again = camera_import_photo(&media_id, "renamed.png", &store).expect("verified");
            assert_eq!(photo.id, again.id, "one photograph per content");
            for photo in [photo, again] {
                library
                    .apply(crate::resource::import(&photo))
                    .expect("the import applies");
            }

            let library = library.state().expect("the library reads");
            assert_eq!(library.photos.len(), 1);
            assert_eq!(library.all_photos().next().unwrap().image.hash(), hash);
        }

        phase camera_import_refuses_missing_or_invalid_portable_bytes;
        {
            let store = ContentStore::memory();
            assert!(camera_import_photo("media:blake3:not-a-hash", "x.png", &store).is_err());
            let missing = format!("media:blake3:{}", "0".repeat(64));
            assert!(camera_import_photo(&missing, "x.png", &store).is_err());
        }
    }
}

#[cfg(test)]
pub(crate) mod test_doc {
    use automerge::{AutoCommit, ROOT};
    use ui::tilo_resource::{Fields as _, Resource as _};

    use crate::model::{Edits, LibraryPhoto, OpenImage, PhotoLibrary};

    pub(crate) fn new_doc() -> AutoCommit {
        let mut doc = AutoCommit::new();
        PhotoLibrary::initialize(&mut doc).expect("a new library initializes");
        doc
    }

    pub(crate) fn library(doc: &AutoCommit) -> PhotoLibrary {
        PhotoLibrary::read(doc).expect("the library reads")
    }

    pub(crate) fn set_library(doc: &mut AutoCommit, library: &PhotoLibrary) {
        library
            .write_fields(doc, &ROOT)
            .expect("the library writes");
    }

    pub(crate) fn put_image(doc: &mut AutoCommit, image: &OpenImage) -> String {
        let photo = LibraryPhoto::from_image(image.clone());
        let id = photo.id.clone();
        let mut library = library(doc);
        library.add_photo(photo);
        set_library(doc, &library);
        id
    }

    pub(crate) fn photo(doc: &AutoCommit, id: &str) -> Option<LibraryPhoto> {
        library(doc).photo(id).cloned()
    }

    pub(crate) fn photo_edits(doc: &AutoCommit, id: &str) -> Edits {
        photo(doc, id).map(|photo| photo.edits).unwrap_or_default()
    }

    pub(crate) fn set_photo_edits(doc: &mut AutoCommit, id: &str, edits: Edits) {
        let mut library = library(doc);
        if let Some(photo) = library.photo_mut(id) {
            photo.edits = edits;
            set_library(doc, &library);
        }
    }
}
