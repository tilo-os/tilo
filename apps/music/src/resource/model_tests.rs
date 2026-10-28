//! The music library on its own: verbs and field edits through the door the
//! host uses, merging between devices, and the history people read.

use super::*;
use crate::resource::library::CreatePlaylist;
use ui::tilo_resource::{Edit, Replica, Resource as _};

fn sample() -> Replica<Library> {
    let mut library = Replica::<Library>::new();
    for action in crate::seed::sample_actions() {
        library.apply(action).unwrap();
    }
    library
}

fn edit(library: &mut Replica<Library>, edit: &Edit<Library>) -> Result<(), String> {
    library
        .apply(Invoke::edit(edit).map_err(|error| error.to_string())?)
        .map(drop)
        .map_err(|error| error.to_string())
}

fn create(library: &mut Replica<Library>, name: &str) -> String {
    library
        .apply(Library::CREATE_PLAYLIST.with(CreatePlaylist {
            name: name.into(),
            track: None,
        }))
        .unwrap();
    library
        .state()
        .unwrap()
        .playlists()
        .find(|playlist| playlist.name == name.trim())
        .map(|playlist| playlist.id.clone())
        .expect("the new playlist")
}

/// A playlist is made, filled, ordered, renamed and deleted; what the
/// library refers to must be in it.
#[test]
fn playlists_and_likes_change_through_verbs_and_field_edits() {
    let mut library = sample();
    let mix = create(&mut library, "  Road mix ");
    let state = library.state().unwrap();
    for track in ["tk_g1", "tk_g2", "tk_a1"] {
        let add = library
            .state()
            .unwrap()
            .add_to_playlist(&mix, track)
            .unwrap();
        edit(&mut library, &add).unwrap();
    }
    assert_eq!(
        library.state().unwrap().playlist(&mix).unwrap().track_ids(),
        ["tk_g1", "tk_g2", "tk_a1"]
    );
    let again = library
        .state()
        .unwrap()
        .add_to_playlist(&mix, "tk_g1")
        .unwrap();
    assert!(
        edit(&mut library, &again).is_err(),
        "a song is in a playlist once"
    );
    edit(&mut library, &Library::move_in_playlist(&mix, "tk_a1", 0)).unwrap();
    edit(&mut library, &Library::remove_from_playlist(&mix, "tk_g2")).unwrap();
    edit(
        &mut library,
        &Library::playlist_paths(&mix).name.replace("Night mix"),
    )
    .unwrap();
    let playlist = library.state().unwrap().playlist(&mix).cloned().unwrap();
    assert_eq!(playlist.name, "Night mix");
    assert_eq!(playlist.track_ids(), ["tk_a1", "tk_g1"]);

    edit(&mut library, &Library::like("tk_v2", true)).unwrap();
    edit(&mut library, &Library::save_album("al_strata", true)).unwrap();
    edit(&mut library, &Library::follow_artist("ar_kavi", true)).unwrap();
    let now = library.state().unwrap();
    assert!(now.is_liked("tk_v2") && now.is_album_saved("al_strata"));
    assert!(now.is_artist_followed("ar_kavi"));
    for refused in [
        Library::like("tk_gone", true),
        Library::save_album("al_gone", true),
        Library::follow_artist("ar_gone", true),
        Library::follow_artist("spotify:artist:1234567890123456789012", true),
    ] {
        assert!(edit(&mut library, &refused).is_err(), "{refused:?}");
    }
    assert!(
        edit(
            &mut library,
            &Library::paths()
                .tracks
                .key("tk_g1")
                .title
                .replace("Renamed")
        )
        .is_err(),
        "catalogue content only arrives by importing"
    );

    // The playlist, its name, cover and songs live in the document, so Undo
    // takes a delete back whole: deleting asks nothing (the one undo step is
    // proved in the resource's tests).
    library
        .apply(Library::DELETE_PLAYLIST.with(Id::from_raw(mix.as_str())))
        .expect("an undoable delete asks nothing");
    assert!(library.state().unwrap().playlist(&mix).is_none());
    assert_eq!(
        state.playlists.len(),
        library.state().unwrap().playlists.len() + 1
    );
}

/// Two devices editing one library converge: songs added to one playlist
/// on both are kept in one order, a song both add is listed once, likes
/// and a rename merge.
#[test]
fn concurrent_edits_converge() {
    let mut laptop = sample();
    let mix = create(&mut laptop, "Shared");
    let mut phone = laptop.fork();

    for (device, tracks) in [
        (&mut laptop, ["tk_g1", "tk_a1"]),
        (&mut phone, ["tk_g1", "tk_h1"]),
    ] {
        for track in tracks {
            let add = device
                .state()
                .unwrap()
                .add_to_playlist(&mix, track)
                .unwrap();
            edit(device, &add).unwrap();
        }
    }
    edit(&mut laptop, &Library::like("tk_v2", true)).unwrap();
    edit(
        &mut phone,
        &Library::playlist_paths(&mix).name.replace("Shared mix"),
    )
    .unwrap();
    laptop.sync_with(&mut phone).unwrap();

    let merged = laptop.state().unwrap();
    assert_eq!(merged, phone.state().unwrap());
    let tracks = merged.playlist(&mix).unwrap().track_ids();
    assert_eq!(tracks.len(), 3, "{tracks:?}");
    for track in ["tk_g1", "tk_a1", "tk_h1"] {
        assert_eq!(tracks.iter().filter(|id| *id == track).count(), 1);
    }
    assert!(merged.is_liked("tk_v2"));
    assert_eq!(merged.playlist(&mix).unwrap().name, "Shared mix");
}

/// An agent makes a playlist, fills it and likes a song with one call
/// each, and the person reads what it did in Music's sentences.
#[test]
fn an_agents_changes_render_as_musics_sentences() {
    use ui::tilo_actions::{ActionInvoker, ActorKind};

    let mut app = ui::framework::testing::TestApp::<MusicShell>::new().unwrap();
    for action in crate::seed::sample_actions() {
        app.invoke(action).unwrap();
    }
    let agent = ActionInvoker::actor("agent:claude")
        .with_kind(ActorKind::Agent)
        .acting_for("ada");
    let kind = Library::KIND;
    app.call_by(
        agent.clone(),
        kind,
        "create-playlist",
        serde_json::json!({ "name": "Focus", "track": "tk_g1" }),
    )
    .unwrap();
    let focus = app
        .state()
        .playlists()
        .find(|playlist| playlist.name == "Focus")
        .map(|playlist| playlist.id.clone())
        .unwrap();
    let add = app.state().add_to_playlist(&focus, "tk_g2").unwrap();
    for change in [
        add,
        Library::playlist_paths(&focus).name.replace("Deep focus"),
        Library::like("tk_v2", true),
    ] {
        app.call_by(
            agent.clone(),
            kind,
            "field-edit",
            serde_json::json!({ "edit": change }),
        )
        .unwrap();
    }
    app.call_confirmed_on(
        &crate::actions::playlist_resource_id(&focus),
        "delete-playlist",
        serde_json::json!({}),
    )
    .unwrap();

    let versions = app.history();
    assert_eq!(
        app.sentences(versions.last()),
        ["Deleted the playlist Deep focus"]
    );
    let entries: Vec<_> = versions
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
            "Created the playlist Focus",
            "Changed the songs of Focus",
            "Renamed the playlist Focus",
            "Changed the liked songs",
        ]
    );
    let invoker = entries[0].invoker.as_ref().unwrap();
    assert_eq!(
        (invoker.kind, invoker.on_behalf_of.as_deref()),
        (ActorKind::Agent, Some("ada"))
    );
}
