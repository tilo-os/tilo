//! What the photograph a window shows is *about*.
//!
//! Photos' half of "select anything and say something about it". It arrives
//! through the same [`Referent`] trait the word processor and the file explorer
//! implement, so the compose surface never learns which app it is talking to —
//! which is what makes the gesture a system service rather than a feature three
//! apps happen to share.
//!
//! ## The address is the one the app already publishes
//!
//! The library publishes every photograph as an item of its own, named by
//! its content (`photos.library/photos/photos.photo:<content>`), carrying
//! the operations that can be performed on it — rotate, crop, remove. A
//! remark minted here names *that same id*, rather than a parallel spelling. That is
//! the whole point: somebody says "this one, but it needs to be square", and
//! whoever reads it resolves the address to the resource whose `crop` action
//! makes it square. Two vocabularies would leave the message pointing at
//! something the door cannot act on.
//! `a_photograph_has_one_whole_published_address_that_edits_keep` holds the
//! two together.
//!
//! ## Referenced whole, and why there is no span
//!
//! A [`Span`](tilo_reference::Span) counts characters or bytes into text; a
//! photograph has neither. The framing a person happens to be looking at is
//! local view state that never leaves the machine (see [`crate::actions`]), so
//! a reference derived from it would name something no other peer can see. The
//! remark is about the photograph, and the crop in force at any moment is a
//! property of the document at that address — a reader resolves it and finds
//! the current state rather than a stale rectangle copied out of somebody's
//! viewport.
//!
//! ## Where this is invoked from
//!
//! Nothing in this crate calls [`photo_reference`] yet: the compose surface
//! that would raise it on a right-click (`ui::widget::RemarkCompose`) has not
//! landed in the shared toolkit. The trait implementation stands on its own —
//! any caller that already holds a [`PhotosApp`] and a
//! document can resolve what it is showing to an address today — and is ready
//! for that surface the moment it exists.

use automerge::AutoCommit;

use tilo_reference::{Referent, ResourceRef};

use crate::actions::PHOTO_KIND;
use crate::app::PhotosApp;
use crate::model::LibraryPhoto;

/// The address of `photo`.
///
/// The label is the file's own name — what a person recognises in a chip —
/// while the content is the address, exactly the split [`ResourceRef`] exists
/// to hold. A remark therefore keeps naming the picture on the machine it is
/// read on, rather than a directory that only existed in the session that
/// wrote it.
pub fn photo_reference(photo: &LibraryPhoto) -> ResourceRef {
    ResourceRef::whole(
        PHOTO_KIND,
        crate::resource::photo_resource_id(&photo.id),
        photo.image.name.clone(),
    )
}

impl Referent for PhotosApp {
    /// The photograph this window shows, addressed as the library publishes
    /// it, so a remark about it reaches the actions it offers.
    fn selection_reference(&self, _doc: &AutoCommit) -> Option<ResourceRef> {
        self.shown_photo().as_ref().map(photo_reference)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::model::OpenImage;

    const PATH: &str = "/var/workspace/photos/harbour.png";

    fn photo(path: &str) -> LibraryPhoto {
        LibraryPhoto::from_image(OpenImage::new(
            path,
            1200,
            800,
            tilo_content::hash_bytes(path.as_bytes()),
        ))
    }

    fn doc_with(photo: &LibraryPhoto) -> AutoCommit {
        let mut doc = crate::test_doc::new_doc();
        let mut library = crate::test_doc::library(&doc);
        library.add_photo(photo.clone());
        crate::test_doc::set_library(&mut doc, &library);
        doc
    }

    /// What is open produces an address, and it is the one the action door
    /// publishes, so a reader can go from the reference to what can be
    /// *done* to it.
    #[test]
    fn a_photograph_has_one_whole_published_address_that_edits_keep() {
        let harbour = photo(PATH);
        let mut doc = doc_with(&harbour);
        let r = photo_reference(&harbour);
        assert_eq!(
            r.resource_id().as_str(),
            crate::resource::photo_resource_id(&harbour.id)
        );
        assert_eq!(r.kind, PHOTO_KIND);

        let library = crate::test_doc::library(&doc);
        let published = <crate::model::PhotoLibrary as ui::tilo_resource::Resource>::child(
            &library,
            r.resource_id().as_str(),
        )
        .unwrap_or_else(|| {
            panic!(
                "a remark minted `{}`, which the app never publishes",
                harbour.id
            )
        });
        assert_eq!(
            published.kind, r.kind,
            "the kind tag is the one the door uses"
        );
        assert!(
            published.actions.iter().any(|a| a.id == "crop"),
            "the referenced photograph carries the crop action"
        );

        // A photograph is referenced whole.
        assert_eq!(r.span, None);
        assert!(!r.is_partial());

        // Editing the photograph does not move its address.
        let mut library = crate::test_doc::library(&doc);
        library.photos.iter_mut().next().unwrap().1.edits =
            crate::model::Edits::default().rotated_right();
        crate::test_doc::set_library(&mut doc, &library);
        let after = photo_reference(&crate::test_doc::photo(&doc, &harbour.id).unwrap());
        assert_eq!(r.resource_id(), after.resource_id());
    }

    /// The name is the label and the picture is the address: two photographs
    /// that share a filename must not share an identity.
    #[test]
    fn labels_name_the_file_addresses_name_the_picture() {
        let a = photo_reference(&photo("/var/workspace/a/harbour.png"));
        let b = photo_reference(&photo("/var/workspace/b/harbour.png"));
        assert_eq!(a.label, "harbour.png");
        assert_eq!(b.label, "harbour.png", "same name");
        assert_ne!(
            a.resource_id(),
            b.resource_id(),
            "two different photographs must not collide on one address"
        );
        assert!(
            !a.label.contains('/'),
            "the label is for a person: {:?}",
            a.label
        );
    }
}
