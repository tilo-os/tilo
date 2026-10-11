//! The library's model and verbs through the door every caller uses: field
//! edits, verbs, invariants, and two replicas converging.

use ui::tilo_resource::{Edit, Id, Invoke, Replica, Resource as _};

use super::*;
use crate::model::{
    Album, AnalysisProvenance, Edits, LocationProvenance, MAX_ALBUM_MEMBERS, MAX_NAME_CHARS,
    MetadataBatch, MetadataLabelsPatch, MetadataPatch, Rect01, album_paths, labels, photo_paths,
};

fn portable_photo(path: &str, width: u32, height: u32) -> LibraryPhoto {
    let hash = tilo_content::hash_bytes(path.as_bytes());
    LibraryPhoto::from_image(OpenImage::from_path(path, width, height).with_content(hash))
}

/// A library holding `photos`, imported through the door.
fn library_of(photos: &[LibraryPhoto]) -> Replica<PhotoLibrary> {
    let mut library = Replica::<PhotoLibrary>::new();
    for photo in photos {
        library
            .apply(Invoke::edit(&insert(photo)).unwrap())
            .expect("a valid photograph goes in");
    }
    library
}

fn edit(library: &mut Replica<PhotoLibrary>, edit: &Edit<PhotoLibrary>) -> Result<(), String> {
    library
        .apply(Invoke::edit(edit).unwrap())
        .map(drop)
        .map_err(|error| error.to_string())
}

fn group(library: &mut Replica<PhotoLibrary>, edits: &[Edit<PhotoLibrary>]) -> Result<(), String> {
    let members = edits.iter().map(|edit| Invoke::edit(edit).unwrap());
    library
        .apply(Invoke::group(members))
        .map(drop)
        .map_err(|error| error.to_string())
}

fn photo(library: &Replica<PhotoLibrary>, id: &str) -> LibraryPhoto {
    library
        .state()
        .unwrap()
        .photo(id)
        .cloned()
        .expect("the photograph is in the library")
}

fn ocr(content: &str, text: &str) -> ImageAnalysis {
    let provenance = AnalysisProvenance {
        source: "local-ocr".into(),
        engine: "tilo-ocr".into(),
        language: "eng".into(),
        content: content.into(),
        scope: "shared".into(),
        ..Default::default()
    };
    ImageAnalysis {
        ocr_text: text.into(),
        provenance: provenance.clone(),
        ocr_provenance: Some(provenance),
        ..Default::default()
    }
}

fn vision(content: &str) -> ImageAnalysis {
    let provenance = AnalysisProvenance {
        source: "local-vision".into(),
        engine: "tilo-vision".into(),
        model_id: "signed-vision-model".into(),
        model_revision: "a".repeat(64),
        content: content.into(),
        scope: "shared".into(),
        ..Default::default()
    };
    ImageAnalysis {
        ocr_text: "incidental sign text".into(),
        description: "A dog beside a red bicycle".into(),
        objects: vec!["bicycle".into()],
        pets: vec!["dog".into()],
        provenance: provenance.clone(),
        ocr_provenance: Some(provenance.clone()),
        vision_provenance: Some(provenance),
    }
}

fn merge(
    library: &mut Replica<PhotoLibrary>,
    id: &str,
    analysis: ImageAnalysis,
) -> Result<(), String> {
    library
        .apply(PhotoLibrary::MERGE_IMAGE_ANALYSIS.with((Id::from_raw(id), analysis)))
        .map(drop)
        .map_err(|error| error.to_string())
}

fn place(source: &str, label: &str) -> PhotoLocation {
    PhotoLocation {
        latitude: 37.7749,
        longitude: -122.4194,
        label: Some(label.to_owned()),
        provenance: LocationProvenance {
            source: source.to_owned(),
            scope: "shared".to_owned(),
        },
    }
}

#[test]
fn an_import_is_content_addressed_portable_idempotent_and_bounded() {
    let mut library = Replica::<PhotoLibrary>::new();
    let origin = "/var/home/alice/Private/harbour.png";
    let hash = "a".repeat(64);
    let image = OpenImage::from_path(origin, 800, 600).with_content(hash.clone());
    library
        .apply(PhotoLibrary::IMPORT_PHOTO.with((image.clone(), None, None)))
        .expect("a content-addressed photograph imports");
    let id = format!("photos.photo:{hash}");
    let imported = photo(&library, &id);
    assert_eq!(
        imported.image.path, "harbour.png",
        "the origin path stays home"
    );
    assert!(!String::from_utf8_lossy(&library.save()).contains(origin));

    // Importing the same content again keeps what the library made of it.
    edit(&mut library, &photo_paths(&id).favorite.set(true)).unwrap();
    library
        .apply(PhotoLibrary::IMPORT_PHOTO.with((image, None, None)))
        .unwrap();
    assert!(photo(&library, &id).favorite);
    assert_eq!(library.state().unwrap().photos.len(), 1);

    // A path with no content would ask other devices to probe it.
    let path_only = OpenImage::from_path("/private/should-not-be-probed.png", 20, 10);
    assert!(
        library
            .apply(PhotoLibrary::IMPORT_PHOTO.with((path_only, None, None)))
            .is_err()
    );
    // A photograph that claims another's identity is refused wherever it
    // comes from: it is named by its content.
    let mut impostor = portable_photo("/private/harbour.png", 20, 10);
    impostor.id = "photos.album:collision".into();
    assert!(edit(&mut library, &insert(&impostor)).is_err());

    // A photograph is a few dozen bytes of decisions, never its pixels.
    let big = portable_photo("/var/tmp/big.png", 4000, 3000);
    edit(&mut library, &insert(&big)).unwrap();
    library
        .apply(PhotoLibrary::ROTATE_RIGHT.with(big.item()))
        .unwrap();
    assert!(
        library.save().len() < 8192,
        "an image never travels in a change"
    );
}

#[test]
fn edits_rotate_compose_crops_and_refuse_what_is_not_a_crop() {
    let harbour = portable_photo("/x/harbour.png", 800, 600);
    let other = portable_photo("/x/other.png", 10, 10);
    let mut library = library_of(&[harbour.clone(), other.clone()]);

    library
        .apply(PhotoLibrary::ROTATE_RIGHT.with(harbour.item()))
        .unwrap();
    assert_eq!(photo(&library, &harbour.id).edits.rotate_quarters, 1);
    library
        .apply(PhotoLibrary::ROTATE_LEFT.with(harbour.item()))
        .unwrap();
    library
        .apply(PhotoLibrary::ROTATE_LEFT.with(harbour.item()))
        .unwrap();
    assert_eq!(photo(&library, &harbour.id).edits.rotate_quarters, 3);

    // A crop is of what is shown, and composes onto the crop in force.
    let half = Rect01 {
        x: 0.0,
        y: 0.0,
        w: 0.5,
        h: 0.5,
    };
    library
        .apply(PhotoLibrary::CROP.with((other.item(), half)))
        .unwrap();
    library
        .apply(PhotoLibrary::CROP.with((other.item(), half)))
        .unwrap();
    let cropped = photo(&library, &other.id).edits;
    assert!((cropped.crop.w - 0.25).abs() < 1e-9, "{cropped:?}");
    assert_eq!(
        photo(&library, &harbour.id).edits.rotate_quarters,
        3,
        "editing one photograph leaves every other alone"
    );

    // A region outside the photograph, or a malformed edit from a peer, is
    // refused and changes nothing.
    let outside = Rect01 {
        x: 1.0,
        y: 1.0,
        w: 1.0,
        h: 1.0,
    };
    assert!(
        library
            .apply(PhotoLibrary::CROP.with((other.item(), outside)))
            .is_err()
    );
    let paths = photo_paths(&other.id).edits;
    assert!(
        group(
            &mut library,
            &[
                paths.crop.set(Rect01 {
                    x: -2.0,
                    y: 0.5,
                    w: 9.0,
                    h: 9.0,
                }),
                paths.rotate_quarters.set(250),
            ],
        )
        .is_err()
    );
    assert_eq!(photo(&library, &other.id).edits, cropped);
    assert!(
        library
            .apply(PhotoLibrary::ROTATE_RIGHT.with(Id::from_raw("photos.photo:gone")))
            .is_err(),
        "an edit of a photograph the library lacks refuses"
    );

    // Reset is both fields, as one change.
    group(
        &mut library,
        &[paths.crop.set(Rect01::FULL), paths.rotate_quarters.set(0)],
    )
    .unwrap();
    assert!(photo(&library, &other.id).edits.is_identity());

    // The crop schema says what a region is.
    let crop = PhotoLibrary::surface()
        .verbs
        .into_iter()
        .find(|verb| verb.id == "crop")
        .expect("crop is published");
    assert!(crop.params.iter().any(|param| param.name == "region"));
}

#[test]
fn a_crop_and_a_turn_merge_independently_and_a_reset_supersedes_what_it_saw() {
    let harbour = portable_photo("/x/harbour.png", 800, 600);
    let mut library = library_of(std::slice::from_ref(&harbour));
    let mut cropper = library.fork();
    let mut rotator = library.fork();
    let crop = Rect01 {
        x: 0.2,
        y: 0.1,
        w: 0.6,
        h: 0.75,
    };
    edit(&mut cropper, &photo_paths(&harbour.id).edits.crop.set(crop)).unwrap();
    rotator
        .apply(PhotoLibrary::ROTATE_RIGHT.with(harbour.item()))
        .unwrap();
    cropper.sync_with(&mut rotator).unwrap();
    let both = photo(&cropper, &harbour.id).edits;
    assert_eq!(both.crop, crop);
    assert_eq!(both.rotate_quarters, 1);

    let paths = photo_paths(&harbour.id).edits;
    group(
        &mut cropper,
        &[paths.crop.set(Rect01::FULL), paths.rotate_quarters.set(0)],
    )
    .unwrap();
    library.sync_with(&mut cropper).unwrap();
    assert!(photo(&library, &harbour.id).edits.is_identity());
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn metadata_edits_are_bounded_canonical_and_merge_per_label() {
    eprintln!("phase details_save_keeps_clears_sets_adds_and_removes_atomically");
    {
        let mut first = portable_photo("/photos/first.png", 20, 10);
        first.folder = Some("Old folder".into());
        first.people = labels(["Sam", "Noor"]);
        first.tags = labels(["old"]);
        first.captured_at = Some("2024-01-02".into());
        let mut second = portable_photo("/photos/second.png", 20, 10);
        second.folder = Some("Other folder".into());
        second.people = labels(["Noor"]);
        second.tags = labels(["other"]);
        let mut library = library_of(&[first.clone(), second.clone()]);
        let batch = MetadataBatch {
            folder: MetadataPatch::Clear,
            people: MetadataLabelsPatch::Remove(vec!["Sam".into()]),
            tags: MetadataLabelsPatch::Set(vec!["trip".into(), "rain".into()]),
            captured_at: MetadataPatch::Set("2026-09-18 14:30".into()),
            location: MetadataPatch::Set(place("user", "Amsterdam")),
        };
        let ids = [first.id.clone(), second.id];
        let edits = library
            .state()
            .unwrap()
            .metadata_edits(&ids, &batch)
            .unwrap();
        group(&mut library, &edits).expect("a valid selection saves as one change");
        assert_eq!(library.log().len(), 3, "two imports and one save");
        for id in &ids {
            let saved = photo(&library, id);
            assert!(saved.folder.is_none());
            assert_eq!(saved.people(), ["Noor"]);
            assert_eq!(saved.tags(), ["rain", "trip"]);
            assert_eq!(saved.captured_at.as_deref(), Some("2026-09-18T14:30"));
            assert_eq!(
                saved
                    .location
                    .and_then(|location| location.label)
                    .as_deref(),
                Some("Amsterdam")
            );
        }

        // A stale selection, or an impossible date, refuses before any edit.
        let before = library.state().unwrap();
        let stale = before.metadata_edits(
            &[first.id.clone(), "photos.photo:gone".into()],
            &MetadataBatch {
                folder: MetadataPatch::Set("Must not apply".into()),
                ..batch.clone()
            },
        );
        assert!(stale.is_err());
        assert!(
            before
                .metadata_edits(
                    &ids,
                    &MetadataBatch {
                        captured_at: MetadataPatch::Set("2026-02-30T08:00Z".into()),
                        ..batch
                    },
                )
                .is_err()
        );
        // Malformed labels are refused by the photograph's own rules, not
        // normalized.
        assert!(
            edit(
                &mut library,
                &photo_paths(&first.id).folder.set(Some(" ".into()))
            )
            .is_err()
        );
        assert!(
            edit(
                &mut library,
                &photo_paths(&first.id)
                    .captured_at
                    .set(Some("2026-09-18 14:30".into()))
            )
            .is_err(),
            "a stored capture time is canonical"
        );
        assert_eq!(library.state().unwrap(), before);
        assert!(crate::model::canonical_capture_at(
            "2026-09-18T14:30:01.120+02:00"
        ));
        assert!(!crate::model::canonical_capture_at("2026-09-18 14:30"));
        assert!(!crate::model::canonical_capture_at("2026-09-18T14:30Z"));
    }

    eprintln!("phase the_full_bounded_selection_saves_in_one_change");
    {
        let photos: Vec<_> = (0..MAX_BATCH_PHOTOS)
            .map(|index| portable_photo(&format!("/photos/batch-{index}.png"), 20, 10))
            .collect();
        let ids: Vec<_> = photos.iter().map(|photo| photo.id.clone()).collect();
        let mut library = Replica::<PhotoLibrary>::new();
        group(&mut library, &photos.iter().map(insert).collect::<Vec<_>>()).unwrap();
        let batch = MetadataBatch {
            folder: MetadataPatch::Set("Archive".into()),
            people: MetadataLabelsPatch::Add(vec!["Sam".into()]),
            tags: MetadataLabelsPatch::Add(vec!["reviewed".into()]),
            captured_at: MetadataPatch::Keep,
            location: MetadataPatch::Keep,
        };
        let edits = library
            .state()
            .unwrap()
            .metadata_edits(&ids, &batch)
            .unwrap();
        group(&mut library, &edits).unwrap();
        assert!(library.state().unwrap().all_photos().all(|photo| {
            photo.folder.as_deref() == Some("Archive")
                && photo.people() == ["Sam"]
                && photo.tags() == ["reviewed"]
        }));
        let mut too_many = ids;
        too_many.push("photos.photo:one-more".into());
        assert!(
            library
                .state()
                .unwrap()
                .metadata_edits(&too_many, &batch)
                .is_err()
        );
    }

    eprintln!("phase concurrent_label_adds_converge_without_replacing_each_other");
    {
        let item = portable_photo("/photos/concurrent-labels.png", 20, 10);
        let mut left = library_of(std::slice::from_ref(&item));
        let mut right = left.fork();
        edit(
            &mut left,
            &photo_paths(&item.id).tags.insert(&"left".to_owned()),
        )
        .unwrap();
        edit(
            &mut right,
            &photo_paths(&item.id).tags.insert(&"right".to_owned()),
        )
        .unwrap();
        left.sync_with(&mut right).unwrap();
        assert_eq!(photo(&left, &item.id).tags(), ["left", "right"]);
        assert_eq!(photo(&right, &item.id).tags(), ["left", "right"]);
    }

    eprintln!("phase different_fields_of_one_photograph_merge");
    {
        let item = portable_photo("/photos/same.png", 20, 10);
        let mut favorite = library_of(std::slice::from_ref(&item));
        let mut folder = favorite.fork();
        edit(&mut favorite, &photo_paths(&item.id).favorite.set(true)).unwrap();
        edit(
            &mut folder,
            &photo_paths(&item.id).folder.set(Some("Travel".into())),
        )
        .unwrap();
        favorite.sync_with(&mut folder).unwrap();
        let merged = photo(&favorite, &item.id);
        assert!(merged.favorite);
        assert_eq!(merged.folder.as_deref(), Some("Travel"));
    }
}

#[test]
fn reindexing_refreshes_only_what_the_file_says() {
    let mut item = portable_photo("reindex.png", 40, 20);
    item.favorite = true;
    item.folder = Some("Family".into());
    item.people = labels(["Ada"]);
    item.tags = labels(["birthday"]);
    item.location = Some(place("user", "Chosen place"));
    let mut library = library_of(std::slice::from_ref(&item));
    let exif = PhotoLocation {
        latitude: 50.0,
        longitude: 4.0,
        label: None,
        provenance: LocationProvenance {
            source: "exif".into(),
            scope: "shared".into(),
        },
    };
    library
        .apply(PhotoLibrary::REFRESH_IMPORTED_METADATA.with((
            item.item(),
            Some("2026:09:18 10:20:30".into()),
            Some(exif),
        )))
        .unwrap();
    let refreshed = photo(&library, &item.id);
    assert_eq!(
        refreshed.captured_at.as_deref(),
        Some("2026-09-18T10:20:30")
    );
    assert_eq!(
        refreshed
            .location
            .as_ref()
            .and_then(|location| location.label.as_deref()),
        Some("Chosen place"),
        "a place the person chose stays"
    );
    assert!(refreshed.favorite);
    assert_eq!(refreshed.folder.as_deref(), Some("Family"));
    assert_eq!(refreshed.people(), ["Ada"]);
    assert_eq!(refreshed.tags(), ["birthday"]);
    assert!(
        library
            .apply(PhotoLibrary::REFRESH_IMPORTED_METADATA.with((
                item.item(),
                None,
                Some(place("user", "Not from a file")),
            )))
            .is_err(),
        "a reindex carries only what a file says"
    );
}

#[test]
fn people_are_shared_labels_offered_as_places() {
    let first = portable_photo("people-one.png", 40, 20);
    let second = portable_photo("people-two.png", 40, 20);
    let mut library = library_of(&[first.clone(), second.clone()]);
    library
        .apply(
            PhotoLibrary::SET_PERSON_MEMBERSHIP
                .with(("Sam".to_owned(), vec![first.id.clone(), second.id.clone()])),
        )
        .unwrap();
    assert_eq!(photo(&library, &first.id).people(), ["Sam"]);
    assert_eq!(photo(&library, &second.id).people(), ["Sam"]);
    let state = library.state().unwrap();
    let person = PhotoLibrary::child(&state, &crate::derived::person_resource_id("Sam"))
        .expect("an approved person is a place");
    for verb in [
        "set-person-membership",
        "rename-person",
        "remove-person",
        "view-person",
    ] {
        assert!(
            person.actions.iter().any(|action| action.id == verb),
            "a person offers {verb}"
        );
    }
    assert!(
        library
            .apply(
                PhotoLibrary::SET_PERSON_MEMBERSHIP
                    .with(("Sam".to_owned(), vec!["photos.photo:gone".to_owned()])),
            )
            .is_err(),
        "a stale membership refuses whole"
    );

    library
        .apply(PhotoLibrary::RENAME_PERSON.with(("Sam".to_owned(), "Samantha".to_owned())))
        .unwrap();
    assert_eq!(photo(&library, &first.id).people(), ["Samantha"]);
    library
        .apply_confirmed(PhotoLibrary::REMOVE_PERSON.with("Samantha"))
        .unwrap();
    assert!(photo(&library, &first.id).people.is_empty());
    let state = library.state().unwrap();
    assert!(
        PhotoLibrary::child(&state, &crate::derived::person_resource_id("Samantha")).is_none(),
        "a person no photograph carries is no place"
    );
    assert!(
        library
            .apply_confirmed(PhotoLibrary::REMOVE_PERSON.with("Nobody"))
            .is_err()
    );
}

#[test]
fn places_are_typed_shared_and_range_checked() {
    let item = portable_photo("place.png", 40, 20);
    let mut library = library_of(std::slice::from_ref(&item));
    let location = place("exif", "San Francisco");
    edit(
        &mut library,
        &photo_paths(&item.id).location.set(Some(location.clone())),
    )
    .unwrap();
    assert_eq!(photo(&library, &item.id).location, Some(location.clone()));
    let place_id = crate::derived::place_resource_id(&location.place_key());
    let state = library.state().unwrap();
    let published = PhotoLibrary::child(&state, &place_id).expect("the place is published");
    assert_eq!(published.label, "San Francisco");
    assert!(
        published
            .actions
            .iter()
            .any(|action| action.id == "view-place")
    );

    edit(&mut library, &photo_paths(&item.id).location.set(None)).unwrap();
    assert!(photo(&library, &item.id).location.is_none());
    assert!(PhotoLibrary::child(&library.state().unwrap(), &place_id).is_none());

    let invalid = PhotoLocation {
        latitude: 91.0,
        longitude: 0.0,
        label: None,
        provenance: LocationProvenance {
            source: "user".to_owned(),
            scope: "local".to_owned(),
        },
    };
    assert!(
        edit(
            &mut library,
            &photo_paths(&item.id).location.set(Some(invalid))
        )
        .is_err()
    );
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn shared_analysis_is_bounded_pinned_and_merges_by_component() {
    eprintln!("phase shared_analysis_is_bounded_readable_and_removable");
    {
        let item = portable_photo("analysis.png", 40, 20);
        let content = item.image.content.clone().unwrap();
        let mut library = library_of(std::slice::from_ref(&item));
        merge(
            &mut library,
            &item.id,
            ocr(&content, "invoice total forty two"),
        )
        .unwrap();
        merge(&mut library, &item.id, vision(&content)).unwrap();
        let state = library.state().unwrap();
        assert!(
            PhotoLibrary::child(&state, &crate::derived::object_resource_id("bicycle")).is_some()
        );
        assert!(PhotoLibrary::child(&state, &crate::derived::pet_resource_id("dog")).is_some());
        let read = PhotoLibrary::read_content(&state, library.doc(), Some(&item.id)).unwrap();
        assert_eq!(read["ocr"]["text"], "invoice total forty two");
        assert_eq!(read["vision"]["objects"][0], "bicycle");

        // Analysis that is not shared, or not of these bytes, never syncs.
        let local = ImageAnalysis {
            provenance: AnalysisProvenance {
                scope: "local".to_owned(),
                ..AnalysisProvenance::default()
            },
            ocr_text: "must not silently sync".to_owned(),
            ..ImageAnalysis::default()
        };
        assert!(merge(&mut library, &item.id, local).is_err());
        assert!(merge(&mut library, &item.id, ocr(&"f".repeat(64), "wrong bytes")).is_err());

        // Removing shared analysis is both components, as one change.
        let paths = photo_paths(&item.id);
        group(&mut library, &[paths.ocr.set(None), paths.vision.set(None)]).unwrap();
        assert!(photo(&library, &item.id).analysis().is_none());
    }

    eprintln!("phase text_then_image_and_image_then_text_keep_each_component");
    {
        let item = portable_photo("ocr-then-vision.png", 40, 20);
        let content = item.image.content.clone().unwrap();
        let mut library = library_of(std::slice::from_ref(&item));
        merge(
            &mut library,
            &item.id,
            ocr(&content, "dedicated receipt text"),
        )
        .unwrap();
        merge(&mut library, &item.id, vision(&content)).unwrap();
        let merged = photo(&library, &item.id).analysis().unwrap();
        assert_eq!(merged.ocr_text, "dedicated receipt text");
        assert_eq!(merged.description, "A dog beside a red bicycle");
        assert_eq!(merged.objects, ["bicycle"]);
        assert_eq!(merged.ocr_provenance.unwrap().source, "local-ocr");
        assert_eq!(merged.vision_provenance.unwrap().source, "local-vision");

        merge(&mut library, &item.id, ocr(&content, "new dedicated text")).unwrap();
        let merged = photo(&library, &item.id).analysis().unwrap();
        assert_eq!(merged.ocr_text, "new dedicated text");
        assert_eq!(merged.description, "A dog beside a red bicycle");
    }

    eprintln!("phase a_new_vision_pass_refreshes_only_incidental_text");
    {
        let item = portable_photo("vision-refresh.png", 40, 20);
        let content = item.image.content.clone().unwrap();
        let mut library = library_of(std::slice::from_ref(&item));
        let mut first = vision(&content);
        first.ocr_text = "old sign".into();
        merge(&mut library, &item.id, first).unwrap();
        let mut refreshed = vision(&content);
        refreshed.ocr_text.clear();
        refreshed.ocr_provenance = None;
        refreshed.description = "The same scene after a model refresh".into();
        merge(&mut library, &item.id, refreshed).unwrap();
        let merged = photo(&library, &item.id).analysis().unwrap();
        assert!(merged.ocr_text.is_empty());
        assert!(merged.ocr_provenance.is_none());
        assert_eq!(merged.description, "The same scene after a model refresh");
    }

    eprintln!("phase a_component_of_other_bytes_is_replaced_not_kept");
    {
        let item = portable_photo("stale-analysis.png", 40, 20);
        let content = item.image.content.clone().unwrap();
        let mut replaced = item.clone();
        replaced.set_analysis(Some(vision(&"f".repeat(64))));
        let mut library = Replica::<PhotoLibrary>::new();
        library
            .apply(Invoke::edit(&insert(&item)).unwrap())
            .unwrap();
        // A component of other bytes, as a replica with a replaced picture
        // left it.
        let mut state = library.state().unwrap();
        state.photo_mut(&item.id).unwrap().vision = replaced.vision;
        ui::tilo_resource::Fields::write_fields(&state, library.doc_mut(), &automerge::ROOT)
            .unwrap();
        merge(&mut library, &item.id, ocr(&content, "current bytes")).unwrap();
        let merged = photo(&library, &item.id).analysis().unwrap();
        assert_eq!(merged.ocr_text, "current bytes");
        assert!(merged.description.is_empty());
        assert!(merged.vision_provenance.is_none());
    }

    eprintln!("phase analysis_needs_content_and_a_pinned_model_but_keeps_an_empty_scene");
    {
        let path_only = LibraryPhoto::from_image(OpenImage::from_path("legacy.png", 40, 20));
        let item = portable_photo("blank-vision.png", 40, 20);
        let content = item.image.content.clone().unwrap();
        let mut library = library_of(std::slice::from_ref(&item));
        let mut with_legacy = library.state().unwrap();
        with_legacy.add_photo(path_only.clone());
        ui::tilo_resource::Fields::write_fields(&with_legacy, library.doc_mut(), &automerge::ROOT)
            .unwrap();
        assert!(merge(&mut library, &path_only.id, ocr("", "unbound text")).is_err());

        let mut unpinned = vision(&content);
        unpinned.provenance.model_revision.clear();
        unpinned.vision_provenance = Some(unpinned.provenance.clone());
        assert!(merge(&mut library, &item.id, unpinned).is_err());

        let mut blank = vision(&content);
        blank.ocr_text.clear();
        blank.description.clear();
        blank.objects.clear();
        blank.pets.clear();
        blank.ocr_provenance = None;
        merge(&mut library, &item.id, blank).unwrap();
        let stored = photo(&library, &item.id).analysis().unwrap();
        assert!(stored.is_empty());
        assert!(
            stored.vision_provenance.is_some(),
            "an empty scene is a result"
        );
    }

    eprintln!("phase concurrent_text_and_image_analysis_both_survive");
    {
        let item = portable_photo("concurrent-analysis.png", 40, 20);
        let content = item.image.content.clone().unwrap();
        let mut text = library_of(std::slice::from_ref(&item));
        let mut image = text.fork();
        merge(&mut text, &item.id, ocr(&content, "concurrent receipt")).unwrap();
        let mut found = vision(&content);
        found.ocr_text.clear();
        found.ocr_provenance = None;
        merge(&mut image, &item.id, found).unwrap();
        text.sync_with(&mut image).unwrap();
        for replica in [&text, &image] {
            let analysis = photo(replica, &item.id).analysis().unwrap();
            assert_eq!(analysis.ocr_text, "concurrent receipt");
            assert_eq!(analysis.description, "A dog beside a red bicycle");
            assert_eq!(analysis.ocr_provenance.unwrap().source, "local-ocr");
            assert_eq!(analysis.vision_provenance.unwrap().source, "local-vision");
        }
    }

    eprintln!("phase local_analysis_is_a_cancellable_background_job");
    {
        let effects = <PhotosEffect as ui::tilo_resource::Effect>::specs();
        for id in ["analyze-image", "analyze-text"] {
            let spec = effects
                .iter()
                .find(|spec| spec.id == id)
                .unwrap_or_else(|| panic!("{id} is published"));
            assert_eq!(
                spec.contract.execution,
                tilo_actions::ExecutionMode::Background { cancellable: true }
            );
            assert_eq!(spec.contract.result, tilo_actions::ResultSpec::Job);
        }
        let library = Replica::<PhotoLibrary>::new().state().unwrap();
        let analysis = PhotoLibrary::child(&library, &derived_resource_id("analysis"))
            .expect("library analysis is a place of its own");
        for id in [
            "view-library-analysis",
            "analyze-library",
            "pause-library-analysis",
            "resume-library-analysis",
            "retry-library-analysis-failures",
        ] {
            assert!(
                analysis.actions.iter().any(|action| action.id == id),
                "missing {id}"
            );
        }
    }
}

#[test]
fn albums_are_items_whose_name_and_photographs_are_fields() {
    let first = portable_photo("/photos/夏.png", 20, 10);
    let second = portable_photo("/photos/two.png", 20, 10);
    let mut library = library_of(&[first.clone(), second.clone()]);
    let summer = Album::named("photos.album:summer", "夏");
    edit(
        &mut library,
        &PhotoLibrary::paths()
            .albums
            .insert_as(&summer.item(), usize::MAX, &summer),
    )
    .unwrap();
    edit(
        &mut library,
        &album_paths(&summer.id).photo_ids.insert(&first.id),
    )
    .unwrap();
    assert!(
        edit(
            &mut library,
            &PhotoLibrary::paths()
                .albums
                .insert_as(&summer.item(), usize::MAX, &summer),
        )
        .is_err(),
        "a stale editor cannot create the album twice"
    );
    assert!(
        edit(
            &mut library,
            &album_paths("photos.album:gone").name.replace("Gone")
        )
        .is_err(),
        "a stale editor cannot save an album that is gone"
    );
    for bad in [
        Album::named("photos.album:bounded", "x".repeat(MAX_NAME_CHARS + 1)),
        Album::named("../not-a-stable-album", "Album"),
    ] {
        assert!(
            edit(
                &mut library,
                &PhotoLibrary::paths()
                    .albums
                    .insert_as(&bad.item(), usize::MAX, &bad)
            )
            .is_err()
        );
    }
    let members = album_paths(&summer.id).photo_ids;
    let too_many: Vec<_> = (0..=MAX_ALBUM_MEMBERS)
        .map(|index| members.insert(&format!("photos.photo:{index}")))
        .collect();
    assert!(group(&mut library, &too_many).is_err());

    // Removing a photograph takes it out of every album.
    library
        .apply_confirmed(PhotoLibrary::REMOVE_PHOTO.with(first.item()))
        .unwrap();
    let state = library.state().unwrap();
    assert!(state.album(&summer.id).unwrap().photo_ids.is_empty());
    // Removing an album keeps its photographs.
    edit(&mut library, &members.insert(&second.id)).unwrap();
    library
        .apply_confirmed(PhotoLibrary::REMOVE_ALBUM.with(summer.item()))
        .unwrap();
    let state = library.state().unwrap();
    assert!(state.albums.is_empty());
    assert!(state.photo(&second.id).is_some());
}

#[test]
fn albums_merge_names_and_memberships_and_deletes_win_stale_imports() {
    let item = portable_photo("member.png", 20, 10);
    let album = Album::named("photos.album:trip", "Trip");
    let mut base = library_of(std::slice::from_ref(&item));
    edit(
        &mut base,
        &PhotoLibrary::paths()
            .albums
            .insert_as(&album.item(), usize::MAX, &album),
    )
    .unwrap();
    let mut membership = base.fork();
    let mut rename = base.fork();
    let members = album_paths(&album.id).photo_ids;
    edit(&mut membership, &members.insert(&item.id)).unwrap();
    edit(
        &mut rename,
        &album_paths(&album.id).name.replace("Summer trip"),
    )
    .unwrap();
    membership.sync_with(&mut rename).unwrap();
    let merged = membership.state().unwrap();
    let album_now = merged.album(&album.id).unwrap();
    assert_eq!(album_now.name, "Summer trip");
    assert_eq!(
        merged.album_photos(album_now).collect::<Vec<_>>(),
        [item.id.as_str()]
    );

    // Filing merges per photograph: one person swapping a photograph for
    // another while someone else swaps it for a third keeps both new ones.
    let second = portable_photo("second-member.png", 20, 10);
    let third = portable_photo("third-member.png", 20, 10);
    for photo in [&second, &third] {
        edit(&mut membership, &insert(photo)).unwrap();
    }
    let mut left = membership.fork();
    let mut right = membership.fork();
    group(
        &mut left,
        &[members.remove(&item.id), members.insert(&second.id)],
    )
    .unwrap();
    group(
        &mut right,
        &[members.remove(&item.id), members.insert(&third.id)],
    )
    .unwrap();
    left.sync_with(&mut right).unwrap();
    for replica in [&left, &right] {
        let state = replica.state().unwrap();
        let filed: Vec<_> = state
            .album_photos(state.album(&album.id).unwrap())
            .map(str::to_owned)
            .collect();
        assert_eq!(filed, [second.id.clone(), third.id.clone()]);
    }

    // Concurrent imports into one library both land.
    let mut one = Replica::<PhotoLibrary>::new();
    let mut other = one.fork();
    let left_photo = portable_photo("left-root.png", 20, 10);
    let right_photo = portable_photo("right-root.png", 20, 10);
    one.apply(super::import(&left_photo)).unwrap();
    other.apply(super::import(&right_photo)).unwrap();
    one.sync_with(&mut other).unwrap();
    let state = one.state().unwrap();
    assert!(state.photo(&left_photo.id).is_some());
    assert!(state.photo(&right_photo.id).is_some());

    // A delete is not undone by an import from a replica that had the
    // photograph already.
    let mut delete = one.fork();
    let mut late_import = one.fork();
    delete
        .apply_confirmed(PhotoLibrary::REMOVE_PHOTO.with(left_photo.item()))
        .unwrap();
    late_import.apply(super::import(&left_photo)).unwrap();
    delete.sync_with(&mut late_import).unwrap();
    assert!(delete.state().unwrap().photo(&left_photo.id).is_none());
}

fn check_contracts(resource: &tilo_actions::Resource) {
    for action in &resource.actions {
        assert!(
            action.contract.validate_declaration().is_empty(),
            "{} on {}: {:?}",
            action.id,
            resource.id.0,
            action.contract.validate_declaration()
        );
    }
    resource.children.iter().for_each(check_contracts);
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn the_library_publishes_its_places_and_items_and_enumerating_writes_nothing() {
    let mut item = portable_photo("/photos/harbour.png", 800, 400);
    item.favorite = true;
    item.folder = Some("2026 travels".into());
    item.tags = labels(["harbour"]);
    item.captured_at = Some("2026-09-07".into());
    let mut library = library_of(std::slice::from_ref(&item));
    let summer = Album::named("photos.album:summer", "Summer");
    edit(
        &mut library,
        &PhotoLibrary::paths()
            .albums
            .insert_as(&summer.item(), usize::MAX, &summer),
    )
    .unwrap();
    let before = library.save();
    let root = library.published().unwrap();
    assert_eq!(library.save(), before, "enumeration changed the document");

    let photo = root
        .children
        .iter()
        .find(|child| child.id.0 == photo_resource_id(&item.id))
        .expect("the photograph is an item of the library");
    assert_eq!(photo.label, "harbour.png");
    assert_eq!(photo.kind, crate::actions::PHOTO_KIND);
    for verb in [
        "rotate-right",
        "rotate-left",
        "crop",
        "remove-photo",
        "merge-image-analysis",
        "refresh-imported-metadata",
    ] {
        assert!(
            photo.actions.iter().any(|action| action.id == verb),
            "the photograph offers {verb}"
        );
    }
    let remove = photo
        .actions
        .iter()
        .find(|action| action.id == "remove-photo")
        .unwrap();
    assert!(remove.params().is_empty(), "the item names the photograph");
    assert!(
        root.children
            .iter()
            .any(|child| child.id.0 == album_resource_id(&summer.id)),
        "the album is an item of the library"
    );
    for (id, view) in [
        ("favorites", "view-favorites"),
        ("albums", "view-albums"),
        ("places", "view-places"),
        ("people", "view-people"),
        ("timeline", "view-timeline"),
        ("analysis", "view-library-analysis"),
    ] {
        let place = root
            .children
            .iter()
            .find(|child| child.id.0 == derived_resource_id(id))
            .unwrap_or_else(|| panic!("{id} is a place of the library"));
        assert!(place.actions.iter().any(|action| action.id == view));
    }
    let state = library.state().unwrap();
    for id in [
        crate::derived::folder_resource_id("2026 travels"),
        crate::derived::tag_resource_id("harbour"),
        crate::derived::timeline_month_resource_id("2026-09"),
        crate::derived::timeline_day_resource_id("2026-09-07"),
    ] {
        let place = PhotoLibrary::child(&state, &id).unwrap_or_else(|| panic!("{id} resolves"));
        assert!(place.children.iter().any(|child| child.label == "1 photo"));
    }
    assert!(
        PhotoLibrary::child(
            &state,
            &crate::derived::timeline_day_resource_id("summer holidays")
        )
        .is_none(),
        "free text is no day of the timeline"
    );

    // Every published action has a valid contract.
    check_contracts(&root);

    // Clearing the last label of a kind takes its place away.
    group(
        &mut library,
        &[
            photo_paths(&item.id).folder.set(None),
            photo_paths(&item.id).tags.remove(&"harbour".to_owned()),
        ],
    )
    .unwrap();
    let state = library.state().unwrap();
    for id in [
        crate::derived::folder_resource_id("2026 travels"),
        crate::derived::tag_resource_id("harbour"),
    ] {
        assert!(PhotoLibrary::child(&state, &id).is_none(), "stale {id}");
    }
}

#[test]
fn an_agent_edits_a_photograph_in_one_call() {
    let item = portable_photo("/photos/agent.png", 40, 20);
    let mut library = library_of(std::slice::from_ref(&item));
    library
        .apply(PhotoLibrary::CROP.with((
            item.item(),
            Rect01 {
                x: 0.25,
                y: 0.25,
                w: 0.5,
                h: 0.5,
            },
        )))
        .expect("an agent crops by address in one call");
    let edits: Edits = photo(&library, &item.id).edits;
    assert!((edits.crop.w - 0.5).abs() < 1e-9);
    assert_eq!(library.log().len(), 2, "one import and one crop");
}

#[test]
fn timeline_days_parse_within_bounds() {
    use crate::derived::timeline_day_from_capture;
    use crate::model::canonical_capture_at_value;
    assert_eq!(
        timeline_day_from_capture("2024-02-29 12:00"),
        Some("2024-02-29".into())
    );
    for capture in ["2025-02-29", "2026-13-01", "2026-04-31", "summer 2026"] {
        assert!(timeline_day_from_capture(capture).is_none(), "{capture}");
    }
    for capture in [
        "",
        "2024",
        "2024-02-2",
        "2024-0é-01",
        "2024é02-01",
        "2024-02-01é",
    ] {
        let expected = (capture == "2024-02-01é").then(|| "2024-02-01".to_owned());
        assert_eq!(timeline_day_from_capture(capture), expected, "{capture:?}");
    }
    assert!(timeline_day_from_capture("1900-02-29").is_none());
    for value in [
        "",
        "Z",
        "+02:00",
        "2026-09-18Z",
        "2026-09-18T14Z",
        "2026-09-18T14:30:01+2:00",
        "2026-09-18T14:30:01+24:00",
        "2026-09-18T14:30+02:00",
        "2026-09-18T24:00:00",
        "2026-09-18é14:30:00",
    ] {
        assert!(canonical_capture_at_value(value).is_none(), "{value:?}");
    }
    assert_eq!(
        canonical_capture_at_value("2026:09:18 14:30:01"),
        Some("2026-09-18T14:30:01".into())
    );
    assert_eq!(
        canonical_capture_at_value("2026-09-18T14:30:01-00:00"),
        Some("2026-09-18T14:30:01Z".into())
    );
    assert_eq!(
        canonical_capture_at_value("2026-09-18T14:30:01-05:30"),
        Some("2026-09-18T14:30:01-05:30".into())
    );
}

/// What an agent does for a person is history the person reads: the app's
/// sentence for each field and verb, naming the photograph as it was
/// called, and who did it for whom.
#[test]
fn history_entries_render_as_the_apps_sentences() {
    use ui::framework::testing::TestApp;
    use ui::tilo_actions::{ActionInvoker, ActorKind};

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("lake.png");
    let png = crate::pixels::encode_png(&crate::shots_pattern(30, 20)).unwrap();
    std::fs::write(&path, &png).unwrap();
    let mut app = TestApp::<PhotosShell>::launched(&[path.to_str().unwrap()]).unwrap();
    let lake = app.state().all_photos().next().unwrap().id.clone();
    let agent = ActionInvoker::actor("agent:claude")
        .with_kind(ActorKind::Agent)
        .acting_for("ada");
    let kind = <PhotoLibrary as ui::tilo_resource::Resource>::KIND;
    let item = photo_resource_id(&lake);
    let paths = photo_paths(&lake);
    for edit in [
        paths.favorite.set(true),
        paths.folder.set(Some("Holidays".into())),
        paths.tags.insert(&"water".to_owned()),
    ] {
        app.call_by(
            agent.clone(),
            kind,
            "field-edit",
            serde_json::json!({ "edit": edit }),
        )
        .unwrap();
    }
    app.call_by(agent.clone(), &item, "rotate-right", serde_json::json!({}))
        .unwrap();
    app.call_by(
        agent.clone(),
        kind,
        "set-person-membership",
        serde_json::json!({ "person": "Ada", "photos": [lake] }),
    )
    .unwrap();
    app.call_by(
        agent,
        kind,
        "rename-person",
        serde_json::json!({ "person": "Ada", "to": "Ada L." }),
    )
    .unwrap();

    let entries: Vec<_> = app
        .history()
        .into_iter()
        .filter(|entry| {
            entry
                .invoker
                .as_ref()
                .is_some_and(|invoker| invoker.actor == "agent:claude")
        })
        .collect();
    let sentences = app.sentences(&entries);
    assert_eq!(
        sentences,
        [
            "Starred lake.png",
            "Filed lake.png under Holidays",
            "Changed the tags of lake.png",
            "Turned lake.png right",
            "Chose the photographs of Ada",
            "Renamed Ada to Ada L.",
        ]
    );
    let invoker = entries[0].invoker.as_ref().unwrap();
    assert_eq!(
        (invoker.kind, invoker.on_behalf_of.as_deref()),
        (ActorKind::Agent, Some("ada")),
        "the entry names the agent and the person it acted for"
    );
}
