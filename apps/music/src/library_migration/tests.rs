use automerge::{AutoCommit, ROOT, ReadDoc};
use tilo_automerge::ApplyIncoming;
use ui::tilo_resource::{Atom, Fields, Resource, evolution};

mod fixtures;
use fixtures::{LegacyLibrary, LegacyPlaylist, populated_fixture};

use crate::model::Library;

fn empty_legacy_library() -> AutoCommit {
    LegacyLibrary::empty().document()
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

fn populated_legacy_library() -> AutoCommit {
    populated_fixture().document()
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
    let mut doc = AutoCommit::new();
    crate::seed::sample_library()
        .write_fields(&mut doc, &ROOT)
        .unwrap();
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
        let mut fixture = populated_fixture();
        match invalid {
            "duplicate" => fixture.playlists.push(LegacyPlaylist::new("p-z", &[])),
            "cover" => fixture.playlists[0].cover = Atom("file:///private-cover.png".into()),
            _ => {}
        }
        let mut doc = fixture.document();
        if invalid == "member" {
            let list = doc.get(ROOT, "playlists").unwrap().unwrap().1;
            let row = doc.get(&list, 0).unwrap().unwrap().1;
            fixtures::InvalidMembership {
                track_membership: [("tk_a1".into(), Atom("not a boolean".into()))].into(),
            }
            .write_fields(&mut doc, &row)
            .unwrap();
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
    fixtures::ScalarLayout {
        playlists: Atom("not a collection".into()),
    }
    .write_fields(&mut left, &ROOT)
    .unwrap();
    Library::default().write_fields(&mut right, &ROOT).unwrap();
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

    // A retained profile kept the library in the record of the window that
    // opened it; the app now keeps it in the workspace's one record, which
    // adopts what that window record held.
    let mut doc = populated_legacy_library();
    let window = RecordId::room_default();
    assert!(ui::record::init_scoped(
        &mut doc,
        &window,
        &RecordKind::new(Library::KIND),
        "test-workspace",
        Some("test-workspace"),
    ));
    let record = RecordId::package_primary();
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
