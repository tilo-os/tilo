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
pub mod pixels;
pub mod places;
pub mod reference;
pub mod resource;
#[cfg(feature = "samples")]
pub mod sample;
pub mod search;
pub mod strings;
mod timeline_projection;

pub use app::PhotosApp;
pub use model::{Edits, LocationProvenance, OpenImage, PhotoLocation, Rect01};

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
    if bytes.is_empty() || bytes.len() > 64 * 1024 * 1024 {
        return Err(crate::strings::tr("photos-camera-error-integrity"));
    }
    let image = pixels::size(&bytes)
        .ok_or_else(|| crate::strings::tr("photos-camera-error-unsupported"))?;
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

    use super::{camera_import_photo, pixels};
    use crate::model::PhotoLibrary;

    #[test]
    fn camera_import_verifies_real_content_before_naming_one_photograph() {
        eprintln!("phase camera_import_verifies_bytes_and_imports_one_content_addressed_photo");
        {
            let rgba = pixels::Rgba::new(1, 1, vec![12, 34, 56, 255]).expect("fixture rgba");
            let bytes = pixels::encode_png(&rgba).expect("fixture png");
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

        eprintln!("phase camera_import_refuses_missing_or_invalid_portable_bytes");
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
