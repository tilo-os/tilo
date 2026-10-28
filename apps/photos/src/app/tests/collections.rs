//! Collections: naming and saving albums, offering only the controls a
//! view can act on, and what collection views show and act on.

use super::*;

#[test]
fn a_new_album_is_named_by_typing_and_saves_without_a_conflict() {
    // A window opens on the Timeline, not an empty single-photo viewer.
    assert!(PhotosApp::new().library_mode);

    let mut app = test_app();
    let mut doc = crate::test_doc::new_doc();
    let photo = LibraryPhoto::from_image(OpenImage::fixture("/pictures/a.png", 800, 600));
    crate::test_doc::set_library(
        &mut doc,
        &crate::model::PhotoLibrary::of(vec![photo], Vec::new()),
    );
    sync(&mut app, &mut doc);
    app.begin_new_album();
    sync(&mut app, &mut doc);
    for ch in ["T", "r", "i", "p"] {
        ui::testing::Dispatch::new()
            .doc(&mut doc)
            .send(&mut app, &ui::synthetic_input::char_key(ch));
    }
    assert_eq!(
        app.album_name_input.value(),
        "Trip",
        "typing replaces the suggested name"
    );
    ui::testing::Dispatch::new().doc(&mut doc).send(
        &mut app,
        &ui::synthetic_input::named_key(winit::keyboard::NamedKey::Enter),
    );
    assert!(!app.album_editor, "Enter in the name field saves");
    settle(&mut app, &mut doc);
    sync(&mut app, &mut doc);
    assert_eq!(
        app.notice.as_deref(),
        Some(tr("photos-album-saved").as_str()),
        "its own save is not someone else's change"
    );
    let library = crate::test_doc::library(&doc);
    assert!(library.albums.values().any(|album| album.name == "Trip"));

    // The new album, still empty, says how to fill it.
    let (title, _) = app.library_empty_state_text();
    assert_eq!(title, tr("photos-album-empty-title"));
    app.show_library();
    app.active_favorites = true;
    assert_eq!(
        app.library_empty_state_text().0,
        tr("photos-favorites-empty-title")
    );
}

#[test]
fn people_offers_to_delete_face_data_only_when_there_is_some() {
    let deletes = |app: &PhotosApp| {
        app.people_controls
            .iter()
            .any(|control| matches!(control.intent, PeopleIntent::DeleteLocalData))
    };
    let mut app = test_app();
    app.show_people();
    app.refresh_people_controls();
    assert!(!deletes(&app), "nothing to delete, no red button");
    assert!(
        app.people_controller_mut().add_cluster(
            "someone",
            [
                "photos.photo:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .to_owned()
            ],
            crate::people::ProposalOrigin::ImportedOrManual,
        )
    );
    app.refresh_people_controls();
    assert!(deletes(&app));

    // Places with no located photo has no map, so no list or zoom controls.
    let mut app = test_app();
    app.show_places_for_shot();
    assert!(app.places_items.is_empty());
    let hits = app.command_hits();
    assert!(
        !hits.iter().any(|hit| matches!(
            hit,
            Hit::PlacesList | Hit::PlacesZoomIn | Hit::PlacesZoomOut
        )),
        "map controls over no map: {hits:?}",
    );
}

#[test]
fn collection_actions_leave_unrelated_library_destinations() {
    let mut photo = LibraryPhoto::from_image(OpenImage::fixture("trip.png", 20, 10));
    photo.favorite = true;
    photo.tags = crate::model::labels(["Trip".to_owned()]);
    let library = PhotoLibrary::of(
        vec![photo.clone()],
        vec![Album {
            id: "photos.album:trip".into(),
            name: "Trip".into(),
            photo_ids: crate::model::labels([photo.id]),
        }],
    );
    for destination in [
        LibraryDestination::People,
        LibraryDestination::Albums,
        LibraryDestination::Places,
        LibraryDestination::Analysis,
    ] {
        for effect in [
            PhotosEffect::ViewAlbum {
                album_id: "photos.album:trip".into(),
            },
            PhotosEffect::ViewFavorites,
            PhotosEffect::ViewTag { tag: "Trip".into() },
        ] {
            let mut app = test_app();
            app.library_destination = destination;
            app.people_mode = destination == LibraryDestination::People;
            app.selection_mode = true;
            app.answer_effect(effect, tilo_actions::InvocationId::new(), &library)
                .unwrap();
            assert!(app.library_mode);
            assert_eq!(app.library_destination, LibraryDestination::Timeline);
            assert!(
                !app.people_mode,
                "the requested photo collection replaces People"
            );
            assert!(
                !app.selection_mode,
                "old collection selection is not carried over"
            );
            assert!(app.active_album.is_some() || app.active_favorites || app.active_tag.is_some());
        }
    }
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn collection_views_are_local_complete_and_refuse_stale_ids() {
    eprintln!(
        "phase opening_an_album_operation_only_changes_the_local_filter_and_refuses_stale_ids"
    );
    {
        let mut app = test_app();
        let mut doc = crate::test_doc::new_doc();
        let photo = LibraryPhoto::from_image(OpenImage::fixture("/photos/one.png", 20, 10));
        let mut library = crate::model::PhotoLibrary::default();
        library.add_photo(photo.clone());
        library.add_album(Album {
            id: "photos.album:trip".into(),
            name: "Trip".into(),
            photo_ids: crate::model::labels([photo.id]),
        });
        crate::test_doc::set_library(&mut doc, &library);
        let before = crate::test_doc::library(&doc);
        let payload = PhotosEffect::ViewAlbum {
            album_id: "photos.album:trip".into(),
        };
        assert!(
            app.answer_effect(
                payload,
                ui::tilo_actions::InvocationId::new(),
                &crate::test_doc::library(&doc)
            )
            .unwrap()
            .is_some()
        );
        assert!(app.library_mode);
        assert_eq!(app.active_album.as_deref(), Some("photos.album:trip"));
        assert_eq!(
            crate::test_doc::library(&doc),
            before,
            "navigation must not alter membership"
        );

        let stale = PhotosEffect::ViewAlbum {
            album_id: "photos.album:gone".into(),
        };
        assert!(
            app.answer_effect(
                stale,
                ui::tilo_actions::InvocationId::new(),
                &crate::test_doc::library(&doc)
            )
            .is_err()
        );
        assert_eq!(crate::test_doc::library(&doc), before);
    }

    eprintln!("phase opening_favorites_uses_a_local_view_and_refuses_an_empty_stale_collection");
    {
        let mut app = test_app();
        let mut doc = crate::test_doc::new_doc();
        let mut favorite =
            LibraryPhoto::from_image(OpenImage::fixture("/photos/favorite.png", 20, 10));
        favorite.favorite = true;
        crate::test_doc::set_library(
            &mut doc,
            &crate::model::PhotoLibrary::of(vec![favorite], Vec::new()),
        );
        let before = crate::test_doc::library(&doc);
        let payload = PhotosEffect::ViewFavorites;
        assert!(
            app.answer_effect(
                payload.clone(),
                ui::tilo_actions::InvocationId::new(),
                &crate::test_doc::library(&doc)
            )
            .expect("favorite collection is present")
            .is_some()
        );
        assert!(app.library_mode);
        assert!(app.active_favorites);
        assert_eq!(
            crate::test_doc::library(&doc),
            before,
            "opening a view is not a write"
        );

        crate::test_doc::set_library(&mut doc, &crate::model::PhotoLibrary::default());
        assert!(
            app.answer_effect(
                payload,
                ui::tilo_actions::InvocationId::new(),
                &crate::test_doc::library(&doc)
            )
            .is_err()
        );
    }

    eprintln!("phase metadata_views_are_local_filters_with_honest_stale_results");
    {
        let mut app = test_app();
        let mut doc = crate::test_doc::new_doc();
        let mut in_folder = LibraryPhoto::from_image(OpenImage::fixture("/photos/one.png", 20, 10));
        in_folder.folder = Some("Travel".into());
        in_folder.people = crate::model::labels(["Élodie".to_owned()]);
        in_folder.tags = crate::model::labels(["golden-hour".to_owned()]);
        let other = LibraryPhoto::from_image(OpenImage::fixture("/photos/two.png", 20, 10));
        let library = crate::model::PhotoLibrary::of(vec![in_folder, other], Vec::new());
        crate::test_doc::set_library(&mut doc, &library);

        app.view_metadata_filter(
            &crate::test_doc::library(&doc),
            Some("Travel".into()),
            None,
            None,
        )
        .expect("existing folder enters a view");
        sync(&mut app, &mut doc);
        assert!(app.library_mode);
        assert_eq!(app.library_total, 1);
        assert_eq!(app.active_folder.as_deref(), Some("Travel"));
        assert!(app.active_person.is_none());

        app.view_metadata_filter(
            &crate::test_doc::library(&doc),
            None,
            Some("Élodie".into()),
            None,
        )
        .expect("existing person enters a view");
        sync(&mut app, &mut doc);
        assert_eq!(app.library_total, 1);
        assert_eq!(app.active_person.as_deref(), Some("Élodie"));
        assert!(app.active_folder.is_none());
        app.view_metadata_filter(
            &crate::test_doc::library(&doc),
            None,
            None,
            Some("golden-hour".into()),
        )
        .expect("existing tag enters a view");
        sync(&mut app, &mut doc);
        assert_eq!(app.library_total, 1);
        assert_eq!(app.active_tag.as_deref(), Some("golden-hour"));
        assert!(app.active_folder.is_none());
        assert!(app.active_person.is_none());
        assert!(
            app.view_metadata_filter(
                &crate::test_doc::library(&doc),
                Some("Missing".into()),
                None,
                None
            )
            .is_err()
        );
    }

    eprintln!("phase people_and_places_keep_complete_paged_collections");
    {
        let mut app = test_app();
        let mut doc = crate::test_doc::new_doc();
        let mut library = crate::model::PhotoLibrary::default();
        for index in 0..29 {
            let mut photo = LibraryPhoto::from_image(OpenImage::fixture(
                &format!("/photos/{index}.png"),
                40,
                20,
            ));
            photo.people = crate::model::labels([format!("Person {index:02}")]);
            photo.location = Some(crate::model::PhotoLocation {
                latitude: 10.0 + f64::from(index) / 100.0,
                longitude: 20.0,
                label: Some(format!("Place {index:02}")),
                provenance: crate::model::LocationProvenance::default(),
            });
            library.add_photo(photo);
            library.add_album(crate::model::Album {
                id: format!("photos.album:album-{index:02}"),
                name: format!("Album {index:02}"),
                photo_ids: ui::tilo_resource::Set::new(),
            });
        }
        crate::test_doc::set_library(&mut doc, &library);
        sync(&mut app, &mut doc);

        assert_eq!(app.library_people.len(), 29);
        assert_eq!(app.places_items.len(), 29);
        assert_eq!(app.library_albums.len(), 29);
        assert!(app.page_collection(CollectionPageHit::PeopleNext));
        assert!(app.page_collection(CollectionPageHit::PlacesNext));
        assert_eq!((app.people_page, app.places_page), (1, 1));
        assert!(app.perform_library_semantic_action(
            "photos:collection-page:people-next",
            &AccessAction::Activate,
            &doc,
        ));
        assert_eq!(app.people_page, 2);
        assert!(app.perform_library_semantic_action(
            "photos:collection-page:people-previous",
            &AccessAction::Activate,
            &doc,
        ));
        assert_eq!(app.people_page, 1);
    }
}
