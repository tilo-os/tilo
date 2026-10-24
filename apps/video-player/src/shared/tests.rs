//! The shared library through the door the host uses: verbs, field edits,
//! the invariants and merging.

use super::*;
use ui::tilo_resource::{Edit, Invoke, Replica};

fn edit(library: &mut Replica<Library>, edit: &Edit<Library>) -> Result<(), String> {
    let invoke = Invoke::edit(edit).map_err(|error| error.to_string())?;
    library
        .apply(invoke)
        .map(drop)
        .map_err(|error| error.to_string())
}

fn playlist(library: &mut Replica<Library>, name: &str) -> Id {
    library.apply(Library::CREATE_PLAYLIST.with(name)).unwrap();
    library
        .state()
        .unwrap()
        .playlists
        .iter()
        .find(|(_, playlist)| playlist.name == name)
        .map(|(id, _)| id.clone())
        .unwrap()
}

fn annotate(library: &mut Replica<Library>, media_ref: &str, time_ms: u64, text: &str) -> Id {
    library
        .apply(Library::ADD_ANNOTATION.with(library::AddAnnotation {
            media_ref: media_ref.into(),
            time_ms,
            text: Some(text.into()),
        }))
        .unwrap();
    library
        .state()
        .unwrap()
        .annotations
        .iter()
        .find(|(_, note)| note.text == text)
        .map(|(id, _)| id.clone())
        .unwrap()
}

/// Playlists hold their videos in order; renaming, reordering and editing
/// a note are field edits; deleting a playlist takes its videos with it.
#[test]
fn playlists_and_annotations_change_through_verbs_and_field_edits() {
    let mut library = Replica::<Library>::new();
    let trip = playlist(&mut library, "Trip");
    for (media_ref, title) in [("clip-1", "Harbour"), ("clip-2", "Lighthouse")] {
        library
            .apply(Library::ADD_TO_PLAYLIST.with(library::AddToPlaylist {
                playlist: trip.clone(),
                media_ref: media_ref.into(),
                title: title.into(),
            }))
            .unwrap();
    }
    let paths = Library::paths().playlists.item(&trip);
    let lighthouse = library.state().unwrap().items.ids().nth(1).unwrap().clone();
    edit(
        &mut library,
        &Library::paths().items.move_to(&lighthouse, 0),
    )
    .unwrap();
    edit(&mut library, &paths.name.replace("Holiday")).unwrap();
    let state = library.state().unwrap();
    assert_eq!(state.playlists.get(&trip).unwrap().name, "Holiday");
    assert_eq!(
        state
            .items_of(&trip)
            .map(|(_, item)| item.title.as_str())
            .collect::<Vec<_>>(),
        ["Lighthouse", "Harbour"]
    );
    assert!(
        edit(&mut library, &paths.name.replace("  ")).is_err(),
        "a playlist keeps a name"
    );
    assert!(
        edit(
            &mut library,
            &Library::paths()
                .items
                .item(&lighthouse)
                .media_ref
                .replace("elsewhere")
        )
        .is_err(),
        "which video an item is never changes"
    );

    let note = annotate(&mut library, "clip-1", 1_000, "Old");
    let at = Library::paths().annotations.item(&note);
    edit(&mut library, &at.text.replace("New")).unwrap();
    edit(&mut library, &at.time_ms.set(1_250)).unwrap();
    let state = library.state().unwrap();
    let shown = state.annotations.get(&note).unwrap();
    assert_eq!((shown.text.as_str(), shown.time_ms), ("New", 1_250));

    // Undo takes a deleted playlist back, videos and all, so deleting asks
    // nothing (the one undo step is proved in the app's tests).
    library
        .apply(Library::DELETE_PLAYLIST.with(trip))
        .expect("an undoable delete asks nothing");
    assert!(library.state().unwrap().playlists.is_empty());
    assert!(
        library.state().unwrap().items.is_empty(),
        "its videos went with it"
    );
}

#[test]
fn watch_later_is_made_once_and_holds_a_video_once() {
    let mut ours = Replica::<Library>::new();
    let mut theirs = ours.fork();
    let add = |media_ref: &str| {
        Library::ADD_TO_WATCH_LATER.with(library::AddToWatchLater {
            media_ref: media_ref.into(),
            title: media_ref.to_uppercase(),
        })
    };
    ours.apply(add("clip-1")).unwrap();
    ours.apply(add("clip-1")).unwrap();
    theirs.apply(add("clip-2")).unwrap();
    ours.sync_with(&mut theirs).unwrap();
    let merged = ours.state().unwrap();
    assert_eq!(merged, theirs.state().unwrap());
    assert_eq!(merged.playlists.len(), 1, "one Watch later playlist");
    assert!(merged.on_watch_later("clip-1") && merged.on_watch_later("clip-2"));
    assert_eq!(merged.items_of(&Id::from_raw(WATCH_LATER)).count(), 2);
}

/// Two people editing the library at once converge, and a note one deleted
/// stays deleted though the other edited it meanwhile.
#[test]
fn concurrent_edits_converge_and_a_deleted_note_stays_deleted() {
    let mut ours = Replica::<Library>::new();
    let note = annotate(&mut ours, "clip-1", 42, "note");
    let mut theirs = ours.fork();
    playlist(&mut ours, "Left");
    ours.apply_confirmed(Library::DELETE_ANNOTATION.with(note.clone()))
        .unwrap();
    playlist(&mut theirs, "Right");
    edit(
        &mut theirs,
        &Library::paths()
            .annotations
            .item(&note)
            .text
            .replace("edited"),
    )
    .unwrap();
    ours.sync_with(&mut theirs).unwrap();
    let merged = ours.state().unwrap();
    assert_eq!(merged, theirs.state().unwrap());
    assert_eq!(merged.playlists.len(), 2);
    assert!(merged.annotations.is_empty(), "the delete wins");
}

/// Where playback stands is kept for the last few videos, and only valid
/// speeds are said.
#[test]
fn the_library_remembers_the_playback_of_the_last_videos() {
    let mut library = Replica::<Library>::new();
    for n in 0..=MAX_TRANSPORTS as u64 {
        library
            .apply(Library::SET_TRANSPORT.with(library::SetTransport {
                media_ref: format!("clip-{n}"),
                playing: true,
                position_ms: n * 1_000,
                rate: 1.0,
                epoch_ms: 1_000 + n,
            }))
            .unwrap();
    }
    let state = library.state().unwrap();
    assert_eq!(state.transports.len(), MAX_TRANSPORTS);
    assert!(
        state.transport_for("clip-0").is_none(),
        "the oldest made room"
    );
    assert!(
        state
            .transport_for(&format!("clip-{MAX_TRANSPORTS}"))
            .is_some()
    );
    assert!(
        library
            .apply(Library::SET_TRANSPORT.with(library::SetTransport {
                media_ref: "clip-1".into(),
                playing: true,
                position_ms: 0,
                rate: 9.0,
                epoch_ms: 5_000,
            }))
            .is_err()
    );
}
