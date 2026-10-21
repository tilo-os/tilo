//! Collections: naming and saving albums, and offering only the
//! controls a view can act on.

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
