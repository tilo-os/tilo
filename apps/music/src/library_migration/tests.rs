use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc};
use tilo_automerge::ApplyIncoming;
use ui::tilo_resource::{Fields, Resource, evolution};

use crate::model::Library;

fn empty_legacy_library() -> AutoCommit {
    let mut doc = AutoCommit::new();
    let library = doc.put_object(ROOT, "library", ObjType::Map).unwrap();
    for name in ["artists", "albums", "tracks"] {
        doc.put_object(&library, name, ObjType::Map).unwrap();
    }
    doc.put_object(ROOT, "playlists", ObjType::List).unwrap();
    for name in [
        "liked",
        "saved_albums",
        "followed_artists",
        "playlist_tombstones",
    ] {
        doc.put_object(ROOT, name, ObjType::Map).unwrap();
    }
    for name in ["likes", "saved-albums", "followed-artists"] {
        doc.put(ROOT, format!("recommendations/input/{name}"), true)
            .unwrap();
    }
    doc.put(ROOT, "unrelated-retained-value", "keep me")
        .unwrap();
    doc.commit();
    doc
}

#[test]
fn retained_legacy_library_reopens_with_collection_schema() {
    let mut doc = empty_legacy_library();
    assert_eq!(evolution::stored_version(&doc), 1);
    assert_eq!(
        Library::read_fields(&doc, &ROOT).unwrap_err().to_string(),
        "playlists: expected a collection"
    );
    let old_catalogue = doc.get(ROOT, "library").unwrap().unwrap().1;
    assert!(evolution::migrate::<Library>(&mut doc).unwrap());
    let library = evolution::read_checked::<Library>(&doc).unwrap();
    assert!(library.tracks.is_empty() && library.playlists.is_empty());
    assert_eq!(doc.get(ROOT, "library").unwrap().unwrap().1, old_catalogue);
    assert_eq!(
        doc.get(ROOT, "unrelated-retained-value")
            .unwrap()
            .unwrap()
            .0
            .as_str(),
        Some("keep me")
    );
    let reopened = AutoCommit::load(&doc.save()).unwrap();
    assert_eq!(
        evolution::read_checked::<Library>(&reopened).unwrap(),
        library
    );
    let heads = doc.get_heads();
    assert!(!evolution::migrate::<Library>(&mut doc).unwrap());
    assert_eq!(doc.get_heads(), heads);
}

fn legacy_catalogue_item<T: ui::tilo_resource::Fields>(
    doc: &mut AutoCommit,
    collection: &automerge::ObjId,
    id: &str,
    item: &T,
) {
    let entry = doc.put_object(collection, id, ObjType::Map).unwrap();
    item.write_fields(doc, &entry).unwrap();
    // The former keyed catalogue carried the identity in its map key.
    doc.delete(&entry, "id").unwrap();
}

fn legacy_playlist(
    doc: &mut AutoCommit,
    list: &automerge::ObjId,
    id: &str,
    entries: &[(&str, i64, bool)],
) -> automerge::ObjId {
    let at = doc.length(list);
    let row = doc.insert_object(list, at, ObjType::Map).unwrap();
    doc.put(&row, "id", id).unwrap();
    doc.put(&row, "name", format!("{id} – retained 🎧"))
        .unwrap();
    doc.put(&row, "description", "First line\nSecond line")
        .unwrap();
    doc.put(&row, "cover", format!("https://example.com/{id}.png"))
        .unwrap();
    let membership = doc
        .put_object(&row, "track_membership", ObjType::Map)
        .unwrap();
    let order = doc.put_object(&row, "track_order", ObjType::Map).unwrap();
    for (track, rank, present) in entries {
        doc.put(&membership, *track, *present).unwrap();
        doc.put(&order, *track, *rank).unwrap();
    }
    row
}

fn populated_legacy_library() -> AutoCommit {
    let mut doc = empty_legacy_library();
    let sample = crate::seed::sample_library();
    let root = doc.get(ROOT, "library").unwrap().unwrap().1;
    let artists = doc.get(&root, "artists").unwrap().unwrap().1;
    legacy_catalogue_item(
        &mut doc,
        &artists,
        "ar_midnight",
        sample.artist("ar_midnight").unwrap(),
    );
    let albums = doc.get(&root, "albums").unwrap().unwrap().1;
    legacy_catalogue_item(
        &mut doc,
        &albums,
        "al_afterhours",
        sample.album("al_afterhours").unwrap(),
    );
    let tracks = doc.get(&root, "tracks").unwrap().unwrap().1;
    for id in ["tk_a1", "tk_a2", "tk_a3"] {
        legacy_catalogue_item(&mut doc, &tracks, id, sample.track(id).unwrap());
    }
    let playlists = doc.get(ROOT, "playlists").unwrap().unwrap().1;
    legacy_playlist(
        &mut doc,
        &playlists,
        "p-z",
        &[
            ("tk_a1", 1024, true),
            ("tk_a2", -5, true),
            ("tk_a3", 17, false),
        ],
    );
    legacy_playlist(
        &mut doc,
        &playlists,
        "p-a",
        &[("tk_a3", 0, true), ("tk_a1", 0, true)],
    );
    legacy_playlist(
        &mut doc,
        &playlists,
        "deleted",
        &[("removed-track", 0, true)],
    );
    let tombstones = doc.get(ROOT, "playlist_tombstones").unwrap().unwrap().1;
    doc.put(&tombstones, "deleted", true).unwrap();
    let liked = doc.get(ROOT, "liked").unwrap().unwrap().1;
    doc.put(&liked, "tk_a1", true).unwrap();
    doc.put(&liked, "tk_a3", false).unwrap();
    let saved = doc.get(ROOT, "saved_albums").unwrap().unwrap().1;
    doc.put(&saved, "al_afterhours", true).unwrap();
    let followed = doc.get(ROOT, "followed_artists").unwrap().unwrap().1;
    doc.put(&followed, "ar_midnight", true).unwrap();
    doc.put(ROOT, "recommendations/input/likes", false).unwrap();
    doc.put(ROOT, "recommendations/input/saved-albums", false)
        .unwrap();
    doc.put(ROOT, "recommendations/preferred-artist/ar_midnight", true)
        .unwrap();
    doc.put(
        ROOT,
        "recommendations/preferred-artist/not-preferred",
        false,
    )
    .unwrap();
    doc.put(ROOT, "recommendations/dismissed/tk_a2", true)
        .unwrap();
    doc.put(ROOT, "recommendations/dismissed/tk_a3", false)
        .unwrap();
    doc.commit();
    doc
}

#[test]
fn legacy_catalogue_playlists_and_feedback_keep_their_identity_and_order() {
    let mut doc = populated_legacy_library();
    let old_playlists = doc.get(ROOT, "playlists").unwrap().unwrap().1;
    assert!(evolution::migrate::<Library>(&mut doc).unwrap());
    let library = evolution::read_checked::<Library>(&doc).unwrap();
    assert_eq!(
        (
            library.artists.len(),
            library.albums.len(),
            library.tracks.len()
        ),
        (1, 1, 3)
    );
    let sample = crate::seed::sample_library();
    for id in ["tk_a1", "tk_a2", "tk_a3"] {
        assert_eq!(library.track(id), sample.track(id));
    }
    assert_eq!(
        library
            .playlists()
            .map(|playlist| playlist.id.as_str())
            .collect::<Vec<_>>(),
        ["p-z", "p-a"]
    );
    let first = library.playlist("p-z").unwrap();
    assert_eq!(first.name, "p-z – retained 🎧");
    assert_eq!(first.description, "First line\nSecond line");
    assert_eq!(first.cover, "https://example.com/p-z.png");
    assert_eq!(first.track_ids(), ["tk_a2", "tk_a1"]);
    assert_eq!(
        library.playlist("p-a").unwrap().track_ids(),
        ["tk_a1", "tk_a3"]
    );
    assert!(library.playlist("deleted").is_none());
    assert!(library.is_liked("tk_a1") && !library.is_liked("tk_a3"));
    assert!(library.is_album_saved("al_afterhours"));
    assert!(library.is_artist_followed("ar_midnight"));
    assert!(!library.recommendations.likes && !library.recommendations.saved_albums);
    assert!(library.recommendations.followed_artists);
    assert!(library.recommendations.prefers("ar_midnight"));
    assert!(!library.recommendations.prefers("not-preferred"));
    assert!(library.recommendations.is_dismissed("tk_a2"));
    assert!(!library.recommendations.is_dismissed("tk_a3"));
    // A tombstoned playlist and its former list remain in the change graph;
    // the migration honors deletion instead of reviving it.
    assert_eq!(doc.length(&old_playlists), 3);
    let reopened = AutoCommit::load(&doc.save()).unwrap();
    assert_eq!(
        evolution::read_checked::<Library>(&reopened).unwrap(),
        library
    );
}

#[test]
fn typed_version_one_records_keep_their_objects_and_values() {
    let mut doc = crate::seed::sample_document();
    doc.delete(ROOT, evolution::VERSION_KEY).unwrap();
    doc.commit();
    let before = Library::read_fields(&doc, &ROOT).unwrap();
    let objects = ["artists", "albums", "tracks", "playlists"]
        .map(|key| doc.get(ROOT, key).unwrap().unwrap().1);
    assert!(evolution::migrate::<Library>(&mut doc).unwrap());
    assert_eq!(evolution::read_checked::<Library>(&doc).unwrap(), before);
    assert_eq!(
        objects,
        ["artists", "albums", "tracks", "playlists"].map(|key| doc
            .get(ROOT, key)
            .unwrap()
            .unwrap()
            .1)
    );
}

#[test]
fn duplicate_playlist_or_invalid_member_refuses_atomically() {
    for invalid in ["duplicate", "member", "cover"] {
        let mut doc = populated_legacy_library();
        let list = doc.get(ROOT, "playlists").unwrap().unwrap().1;
        match invalid {
            "duplicate" => {
                legacy_playlist(&mut doc, &list, "p-z", &[]);
            }
            "member" => {
                let row = doc.get(&list, 0).unwrap().unwrap().1;
                let membership = doc.get(&row, "track_membership").unwrap().unwrap().1;
                doc.put(&membership, "tk_a1", "not a boolean").unwrap();
            }
            _ => {
                let row = doc.get(&list, 0).unwrap().unwrap().1;
                doc.put(&row, "cover", "file:///private-cover.png").unwrap();
            }
        }
        doc.commit();
        let bytes = doc.save();
        let heads = doc.get_heads();
        assert!(
            evolution::migrate::<Library>(&mut doc).is_err(),
            "{invalid}"
        );
        assert_eq!(doc.get_heads(), heads, "{invalid}");
        assert_eq!(doc.save(), bytes, "{invalid}");
        assert_eq!(evolution::stored_version(&doc), 1);
    }
}

#[test]
fn conflicting_layouts_are_not_coerced_or_discarded() {
    let mut left = empty_legacy_library();
    let mut right = left.fork();
    left.put(ROOT, "playlists", "not a collection").unwrap();
    right.put_object(ROOT, "playlists", ObjType::Map).unwrap();
    left.commit();
    right.commit();
    left.merge_incoming(&mut right).unwrap();
    assert_eq!(left.get_all(ROOT, "playlists").unwrap().len(), 2);
    let bytes = left.save();
    assert!(evolution::migrate::<Library>(&mut left).is_err());
    assert_eq!(left.save(), bytes);
}

#[test]
fn the_same_legacy_heads_migrate_deterministically_and_merge_once() {
    let mut left = populated_legacy_library();
    let mut right = left.fork();
    evolution::migrate::<Library>(&mut left).unwrap();
    evolution::migrate::<Library>(&mut right).unwrap();
    assert_eq!(left.get_heads(), right.get_heads());
    let heads = left.get_heads();
    left.merge_incoming(&mut right).unwrap();
    assert_eq!(left.get_heads(), heads);
    assert_eq!(
        evolution::read_checked::<Library>(&left)
            .unwrap()
            .playlists
            .len(),
        2
    );
}

#[test]
fn opening_a_retained_profile_migrates_and_restores_live_player_actions() {
    use ui::framework::testing::TestApp;
    use ui::record::{RecordId, RecordKind};

    let mut doc = populated_legacy_library();
    let record = RecordId::room_default();
    assert!(ui::record::init_scoped(
        &mut doc,
        &record,
        &RecordKind::new(Library::KIND),
        "test-workspace",
        Some("test-workspace"),
    ));
    let mut app = TestApp::<crate::MusicShell>::launched_replica(&[], &mut doc).unwrap();
    assert_eq!(app.record(), &record);
    assert_eq!(
        app.state().playlist("p-z").unwrap().track_ids(),
        ["tk_a2", "tk_a1"]
    );
    assert_eq!(app.read_document(evolution::stored_version), 2);
    let rows = app.agent_surface();
    assert!(
        rows.as_array().unwrap().iter().any(|row| {
            row["resource"] == "music.player:player" && row["action"] == "playback-status"
        }),
        "the migrated view must publish its real live player row"
    );
    app.effect_on(
        "music.player:player",
        &crate::actions::MusicEffect::PlaybackStatus,
    )
    .unwrap();
    let mut reopened = app.reopen().unwrap();
    assert_eq!(reopened.record(), &record);
    assert_eq!(
        reopened.state().playlist("p-z").unwrap().name,
        "p-z – retained 🎧"
    );
    reopened
        .effect_on(
            "music.player:player",
            &crate::actions::MusicEffect::PlaybackStatus,
        )
        .unwrap();
}
