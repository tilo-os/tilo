//! Photos — view a photograph, adjust it, and get the result out.
//!
//! A viewer with a small set of edits:
//!
//!   * **Open** an image through the host XDG portal chooser (never a bespoke
//!     dialog), or from a path on the command line.
//!   * **Display** it fit to the window — contain-scaled, so a photograph
//!     larger than the viewport is scaled down rather than cropped by the
//!     frame. Scroll to zoom, drag to pan.
//!   * **Adjust** it: quarter turns either way, and a crop of whatever the
//!     viewport is currently framing. Both change the pixels.
//!   * **Get it out**: copy the edited pixels to the system clipboard, or save
//!     them as a PNG beside the original.
//!
//! # Where state lives
//!
//! A photograph is its original plus what was done to it. Importing puts the
//! original's bytes in the app's content store (`tilo-content`, a handle the
//! app is given at startup) and records an `ImportPhoto` action naming them by
//! hash; every edit after that is a `SetEdits` action replayed onto that
//! original. The CRDT holds the hash, small metadata and the edits in force
//! ([`doc`]); it never holds the pixels, and a rendered edit (Copy, Save, the
//! export operation) is an output that never replaces the original. The decode
//! and the render live in the app instance, because an automerge change big
//! enough to carry a photograph is dropped by the sync transport and freezes
//! every peer's replica permanently.
//!
//! # The shape of the surface
//!
//! [`actions`] publishes the open photograph as a `Resource` whose edits are
//! `ActionSpec`s, addressed by stable id. A caller with no display can list
//! what is open and act on it without a coordinate ever entering the picture.
//!
//! [`PhotosShell`] is the framework app every host launches. The live app
//! starts **empty**; the `--shots` scenarios ([`sample`]) show generated test
//! patterns that exist only on that path.

// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop, and proving every layout sum cannot
// overflow is a tax a third-party app author would pay too. The lints that
// produce a *crash* stay denied — indexing_slicing, string_slice,
// unwrap_used, expect_used, panic — because a panicking app is a bad app
// whoever wrote it. `shell`, `ui` and everything under `crates/` keep the
// full set: a fault there takes the session, or every app with it.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
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
pub mod doc;
mod files;
/// Bounded, cancellation-aware real-file discovery and metadata ingestion.
pub mod import;
pub mod library_analysis;
pub mod model;
pub mod people;
pub mod pixels;
/// Offline-capable place grouping and map projection shared by the timeline
/// surface and resource/action routing. This module deliberately has no tile
/// client, so opening a Photos location never leaks it to a network provider.
pub mod places;
pub mod reference;
pub mod resource;
pub mod sample;
pub mod search;
pub mod strings;
mod timeline_projection;

pub use app::PhotosApp;
pub use model::{Edits, LocationProvenance, OpenImage, PhotoLocation, Rect01};

/// Import a Camera portable-media reference into Photos without ever trusting
/// a machine-local path.  The content is read, verified against its hash, from
/// `store` first; missing bytes are an actionable refusal rather than a
/// phantom library entry.
///
/// # Errors
///
/// Returns an error when the Camera identity is malformed, the verified bytes
/// have not arrived on this device, exceed the import budget, fail integrity
/// validation, or are not a safely decodable still image.
pub fn camera_import_photo(
    media_id: &str,
    name: &str,
    store: &tilo_content::ContentStore,
) -> Result<model::LibraryPhoto, String> {
    let hash = media_id
        .strip_prefix("media:blake3:")
        .filter(|hash| {
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .ok_or_else(|| crate::strings::tr("photos-camera-error-identity"))?;
    // The store verifies what it reads against the hash; content that fails
    // that check reads as absent.
    let bytes = store
        .read(hash)
        .ok_or_else(|| crate::strings::tr("photos-camera-error-not-here"))?;
    if bytes.is_empty() || bytes.len() > 64 * 1024 * 1024 {
        return Err(crate::strings::tr("photos-camera-error-integrity"));
    }
    let image = pixels::decode(&bytes)
        .ok_or_else(|| crate::strings::tr("photos-camera-error-unsupported"))?;
    let open = OpenImage {
        path: name.to_owned(),
        name: name.to_owned(),
        width: image.w,
        height: image.h,
        content: Some(hash.to_owned()),
    }
    .portable();
    Ok(model::LibraryPhoto::from_image(open))
}

/// The app as every host mounts it.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::framework::definition::<PhotosShell>()
}

pub use resource::{Photos, PhotosShell, PhotosState};
pub use sample::{shots_pattern, shots_pattern_variant};

#[cfg(test)]
mod camera_import_tests {
    use automerge::AutoCommit;
    use tilo_actions::Action as _;
    use tilo_content::ContentStore;

    use super::{camera_import_photo, pixels};
    use crate::actions::PhotosAction;
    use crate::doc::PhotosDoc;

    #[test]
    fn camera_import_verifies_real_content_before_naming_one_photograph() {
        eprintln!("phase camera_import_verifies_bytes_and_imports_one_content_addressed_photo");
        {
            let rgba = pixels::Rgba::new(1, 1, vec![12, 34, 56, 255]).expect("fixture rgba");
            let bytes = pixels::encode_png(&rgba).expect("fixture png");
            let store = ContentStore::memory();
            let hash = store.put(&bytes).expect("fixture content");
            let media_id = format!("media:blake3:{hash}");
            let mut doc = AutoCommit::new();

            let photo =
                camera_import_photo(&media_id, "Camera photo.png", &store).expect("verified");
            let again = camera_import_photo(&media_id, "renamed.png", &store).expect("verified");
            assert_eq!(photo.id, again.id, "one photograph per content");
            for photo in [photo, again] {
                PhotosAction::ImportPhoto { photo }
                    .apply(&mut doc)
                    .expect("the import applies");
            }

            let library = PhotosDoc::library(&doc);
            assert_eq!(library.photos.len(), 1);
            assert_eq!(
                library.photos[0].image.content.as_deref(),
                Some(hash.as_str())
            );
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
