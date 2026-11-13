//! What the photograph a window shows is *about*. Photos' half of "select anything and say
//! something about it".

use automerge::AutoCommit;

use tilo_reference::{Referent, ResourceRef};

use crate::actions::PHOTO_KIND;
use crate::app::PhotosApp;
use crate::model::LibraryPhoto;

pub fn photo_reference(photo: &LibraryPhoto) -> ResourceRef {
    ResourceRef::whole(
        PHOTO_KIND,
        crate::resource::photo_resource_id(&photo.id),
        photo.image.name.clone(),
    )
}

impl Referent for PhotosApp {
    fn selection_reference(&self, _doc: &AutoCommit) -> Option<ResourceRef> {
        self.state().shown_photo().as_ref().map(photo_reference)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::model::OpenImage;

    const PATH: &str = "/var/workspace/photos/harbour.png";

    fn photo(path: &str) -> LibraryPhoto {
        LibraryPhoto::from_image(OpenImage::named(
            path,
            1200,
            800,
            &tilo_content::hash_bytes(path.as_bytes()),
        ))
    }

    fn doc_with(photo: &LibraryPhoto) -> AutoCommit {
        let mut doc = crate::test_doc::new_doc();
        let mut library = crate::test_doc::library(&doc);
        library.add_photo(photo.clone());
        crate::test_doc::set_library(&mut doc, &library);
        doc
    }

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

        assert_eq!(r.span, None);
        assert!(!r.is_partial());

        let mut library = crate::test_doc::library(&doc);
        library.photos.iter_mut().next().unwrap().1.edits =
            crate::model::Edits::default().rotated_right();
        crate::test_doc::set_library(&mut doc, &library);
        let after = photo_reference(&crate::test_doc::photo(&doc, &harbour.id).unwrap());
        assert_eq!(r.resource_id(), after.resource_id());
    }

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
