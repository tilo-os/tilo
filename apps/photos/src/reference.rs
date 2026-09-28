//! What the open photograph is *about*.
//!
//! Photos' half of "select anything and say something about it". It arrives
//! through the same [`Referent`] trait the word processor and the file explorer
//! implement, so the compose surface never learns which app it is talking to —
//! which is what makes the gesture a system service rather than a feature three
//! apps happen to share.
//!
//! ## The address is the one the app already publishes
//!
//! [`crate::actions::PhotosResources`] publishes the open photograph as
//! `photos.image:<content>`, carrying the operations that can be performed on
//! it — rotate, crop, reset. A remark minted here names *that same id*, through
//! [`crate::actions::resource_id`], rather than a parallel spelling. That is
//! the whole point: somebody says "this one, but it needs to be square", and
//! whoever reads it resolves the address to the resource whose `crop` action
//! makes it square. Two vocabularies would leave the message pointing at
//! something the door cannot act on.
//! `the_address_matches_the_id_the_app_publishes` holds the two together.
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
//! Nothing in this crate calls [`image_reference`] yet: the compose surface
//! that would raise it on a right-click (`ui::widget::RemarkCompose`) has not
//! landed in the shared toolkit. The trait implementation stands on its own —
//! any caller that already holds a [`PhotosApp`] and a
//! document can resolve what it is showing to an address today — and is ready
//! for that surface the moment it exists.

use automerge::AutoCommit;

use tilo_reference::{Referent, ResourceRef};

use crate::actions::{IMAGE_KIND, resource_id};
use crate::app::PhotosApp;
use crate::doc::PhotosDoc;

/// The address of the photograph open in `doc`, or `None` when none is.
///
/// The label is the file's own name — what a person recognises in a chip —
/// while the content is the address, exactly the split [`ResourceRef`] exists
/// to hold. A remark therefore keeps naming the picture on the machine it is
/// read on, rather than a directory that only existed in the session that
/// wrote it.
pub fn image_reference(doc: &AutoCommit) -> Option<ResourceRef> {
    let image = PhotosDoc::image(doc)?;
    let id = resource_id(&image);
    Some(ResourceRef::whole(IMAGE_KIND, id, image.name))
}

impl Referent for PhotosApp {
    /// Which photograph is open lives in the shared document, not in this
    /// widget, so the address is the document's answer rather than the paint
    /// layer's — and a photograph a peer opened is referenceable here without
    /// this instance having been the one to open it.
    fn selection_reference(&self, doc: &AutoCommit) -> Option<ResourceRef> {
        image_reference(doc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::PhotosResources;
    use crate::model::OpenImage;

    const PATH: &str = "/var/workspace/photos/harbour.png";

    fn doc_with_image(path: &str) -> AutoCommit {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        PhotosDoc::set_image(&mut doc, Some(&OpenImage::from_path(path, 1200, 800)));
        doc
    }

    /// The headline property, in the third app and in the same shape as the
    /// first two: what is open produces an address, asserted on the id.
    #[test]
    fn the_open_photograph_has_one_whole_published_address_that_edits_keep() {
        let doc = doc_with_image(PATH);
        let r = image_reference(&doc).expect("an open photograph is referenceable");
        assert_eq!(
            r.resource_id().as_str(),
            "photos.image:/var/workspace/photos/harbour.png"
        );
        assert_eq!(r.kind, IMAGE_KIND);

        // The address a remark carries is the one the action door publishes, so a
        // reader can go from the reference to what can be *done* to it — which in
        // this app is the crop the whole gesture exists to ask for.
        let doc = doc_with_image(PATH);
        let r = image_reference(&doc).unwrap();

        let published = PhotosResources.publish_resources(&doc);
        let ids: Vec<String> = published
            .iter()
            .map(|res| res.id.as_str().to_string())
            .collect();

        assert!(
            ids.contains(&r.resource_id().as_str().to_string()),
            "a remark minted `{}`, which the app never publishes: {ids:?}",
            r.resource_id().as_str()
        );
        assert_eq!(
            published[0].kind, r.kind,
            "the kind tag must be the one the door uses too"
        );
        // And the address is followable to the operation the demo asks for.
        assert!(
            published[0].actions.iter().any(|a| a.id == "crop"),
            "the referenced resource must carry the crop action"
        );

        // A photograph is referenced whole. The absence of a span is meaningful:
        // it is how a reader knows the remark is about the picture and not about
        // some rectangle inside it.
        let doc = doc_with_image(PATH);
        let r = image_reference(&doc).unwrap();
        assert_eq!(r.span, None);
        assert!(!r.is_partial());

        // Editing the photograph does not move its address. The crop in force is
        // a property of the document at that address, so a remark written before
        // an edit still names the same picture after it.
        let mut doc = doc_with_image(PATH);
        let before = image_reference(&doc).unwrap();
        PhotosDoc::set_edits(&mut doc, &crate::model::Edits::default().rotated_right());
        let after = image_reference(&doc).unwrap();
        assert_eq!(before.resource_id(), after.resource_id());
    }

    /// The name is the label and the picture is the address: two photographs
    /// that share a filename must not share an identity.
    #[test]
    fn labels_name_the_file_addresses_name_the_picture_and_empty_apps_have_none() {
        let a = image_reference(&doc_with_image("/var/workspace/a/harbour.png")).unwrap();
        let b = image_reference(&doc_with_image("/var/workspace/b/harbour.png")).unwrap();
        assert_eq!(a.label, "harbour.png");
        assert_eq!(b.label, "harbour.png", "same name");
        assert_ne!(
            a.resource_id(),
            b.resource_id(),
            "two different photographs must not collide on one address"
        );
        assert!(
            !a.label.contains('/'),
            "the label is for a person, not the path: {:?}",
            a.label
        );

        // Nothing open, nothing to talk about — the gesture is not offered rather
        // than a reference to nothing being minted.
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        assert!(image_reference(&doc).is_none());
    }
}
