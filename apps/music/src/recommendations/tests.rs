use super::*;
use ui::tilo_resource::{Edit, Fields as _, Invoke, Replica};

fn library() -> Library {
    let mut lib = crate::seed::sample_library();
    lib.liked = Set::new();
    lib.saved_albums = Set::new();
    lib.followed_artists = Set::new();
    lib.recommendations = Preferences::default();
    lib
}

fn replica(lib: &Library) -> Replica<Library> {
    let mut doc = ui::tilo_resource::Doc::new();
    <Library as ui::tilo_resource::Resource>::initialize(&mut doc).unwrap();
    lib.write_fields(&mut doc, &automerge::ROOT).unwrap();
    doc.commit();
    Replica::from_doc(doc)
}

fn apply(library: &mut Replica<Library>, edit: &Edit<Library>) -> Result<(), String> {
    library
        .apply(Invoke::edit(edit).unwrap())
        .map(drop)
        .map_err(|error| error.to_string())
}

fn paths() -> <Preferences as Model>::Paths<Library> {
    Library::paths().recommendations
}

#[test]
fn recommendations_cold_start_and_unconfigured_library_are_honest() {
    assert!(suggestions(&Library::default(), 6).is_empty());
    assert!(suggestions(&library(), 6).is_empty());
}

#[test]
fn recommendations_use_explicit_inputs_and_never_play_counts() {
    let mut lib = library();
    lib.liked.insert("tk_g1".into());
    let picks = suggestions(&lib, 6);
    assert!(!picks.is_empty());
    assert!(picks.iter().all(|s| s.track != "tk_g1"));
    for track in lib.tracks.values_mut() {
        track.plays = u64::MAX;
    }
    assert_eq!(suggestions(&lib, 6), picks);
    lib.recommendations.likes = false;
    assert!(suggestions(&lib, 6).is_empty());
}

#[test]
fn recommendations_dismiss_restore_and_selective_artist_preferences_are_persistent() {
    let lib = library();
    let mut doc = replica(&lib);
    let track = lib.tracks.values().next().unwrap();
    let artist = track.artist_id.clone();
    apply(&mut doc, &paths().preferred_artists.insert(&artist)).unwrap();
    let before = doc.state().unwrap();
    assert!(before.followed_artists.is_empty());
    assert!(
        suggestions(&before, before.tracks.len())
            .iter()
            .any(|s| s.track == track.id)
    );
    apply(&mut doc, &paths().dismissed.insert(&track.id)).unwrap();
    let mut loaded = Replica::<Library>::load(&doc.save()).unwrap();
    assert!(
        suggestions(&loaded.state().unwrap(), 50)
            .iter()
            .all(|s| s.track != track.id)
    );
    apply(&mut loaded, &paths().dismissed.remove(&track.id)).unwrap();
    assert!(
        suggestions(&loaded.state().unwrap(), 50)
            .iter()
            .any(|s| s.track == track.id)
    );
}

#[test]
fn recommendations_concurrent_feedback_merges_and_stale_positive_targets_fail() {
    let lib = library();
    let mut base = replica(&lib);
    let mut peer = base.fork();
    apply(&mut base, &paths().likes.set(false)).unwrap();
    let track = lib.tracks.values().next().unwrap();
    apply(&mut peer, &paths().dismissed.insert(&track.id)).unwrap();
    base.sync_with(&mut peer).unwrap();
    let prefs = base.state().unwrap().recommendations;
    assert!(!prefs.likes);
    assert!(prefs.is_dismissed(&track.id));
    assert_eq!(peer.state().unwrap().recommendations, prefs);
    let before = base.heads();
    assert!(
        apply(
            &mut base,
            &paths().preferred_artists.insert(&"unknown".to_owned())
        )
        .is_err()
    );
    assert_eq!(base.heads(), before);
}

#[test]
fn recommendations_resources_explain_and_offer_reversible_feedback() {
    let mut lib = library();
    lib.liked.insert("tk_g1".into());
    let pick = suggestions(&lib, 1).pop().unwrap();
    let id = format!("music.recommendation:{}", pick.track);
    let resource = resource_by_id(&lib, &id).unwrap();
    assert!(resource.label.contains(&pick.reason.label(&lib)));
    for offered in ["open", "play"] {
        assert!(
            resource.actions.iter().any(|a| a.id == offered),
            "{offered}"
        );
    }
    let mut doc = replica(&lib);
    apply(&mut doc, &paths().dismissed.insert(&pick.track)).unwrap();
    let dismissed = doc.state().unwrap();
    assert!(suggestion(&dismissed, &pick.track).is_none());
    let row = resource_by_id(&dismissed, &id).expect("a dismissed song stays restorable");
    assert!(row.actions.iter().any(|a| a.id == "play"));
    assert!(resource_by_id(&lib, "music.recommendation-input:unknown").is_none());
}

#[test]
fn recommendations_seed_roundtrips_nondefault_feedback_and_disabled_inputs() {
    let mut lib = library();
    lib.recommendations.likes = false;
    lib.recommendations.saved_albums = false;
    lib.recommendations
        .preferred_artists
        .insert("ar_kavi".into());
    lib.recommendations.dismissed.insert("tk_g2".into());
    let doc = replica(&lib);
    let loaded = Replica::<Library>::load(&doc.doc().clone().save()).unwrap();
    assert_eq!(loaded.state().unwrap().recommendations, lib.recommendations);
    assert!(
        loaded
            .state()
            .unwrap()
            .recommendations
            .is_dismissed("tk_g2")
    );
}

#[test]
fn recommendations_each_signal_can_be_disabled_and_genres_name_their_provenance() {
    let mut lib = library();
    let track = lib.tracks.values().next().unwrap().clone();
    lib.saved_albums.insert(track.album_id.clone());
    assert!(
        suggestions(&lib, 50)
            .iter()
            .any(|s| matches!(s.reason, Reason::SavedAlbum(_)))
    );
    lib.recommendations.saved_albums = false;
    assert!(suggestions(&lib, 50).is_empty());
    lib.followed_artists.insert(track.artist_id.clone());
    assert!(
        suggestions(&lib, 50)
            .iter()
            .any(|s| matches!(s.reason, Reason::FollowedArtist(_)))
    );
    lib.recommendations.followed_artists = false;
    assert!(suggestions(&lib, 50).is_empty());
    let other = lib
        .artists
        .keys()
        .find(|id| **id != track.artist_id)
        .unwrap()
        .clone();
    lib.artists.get_mut(&track.artist_id).unwrap().genre = "Test genre".into();
    lib.artists.get_mut(&other).unwrap().genre = " test GENRE ".into();
    lib.liked.insert(track.id.clone());
    let pick = suggestions(&lib, 50).into_iter().find(|s| {
        matches!(&s.reason, Reason::SharedGenre { artist, .. } if artist == &track.artist_id)
    }).unwrap();
    assert_eq!(lib.track(&pick.track).unwrap().artist_id, other);
    assert!(
        pick.reason
            .label(&lib)
            .contains(lib.artist_name(&track.artist_id))
    );
    lib.recommendations.likes = false;
    assert!(suggestions(&lib, 50).is_empty());
}

/// The library never holds feedback about music it does not hold: songs
/// and artists only ever join it, so feedback naming one that is not there
/// is refused, and a suggestion row for it offers nothing to play.
#[test]
fn recommendations_feedback_names_only_music_of_the_library() {
    let lib = Library::default();
    let mut doc = replica(&lib);
    for stale in [
        paths().dismissed.insert(&"deleted".to_owned()),
        paths()
            .preferred_artists
            .insert(&"deleted-artist".to_owned()),
    ] {
        assert!(apply(&mut doc, &stale).is_err());
    }
    let state = doc.state().unwrap();
    assert!(state.recommendations.dismissed.is_empty());
    assert!(state.recommendations.preferred_artists.is_empty());
    let mut forgotten = Library::default();
    forgotten.recommendations.dismissed.insert("deleted".into());
    assert_ne!(
        resource_by_id(&forgotten, "music.recommendation:deleted")
            .unwrap()
            .actions
            .iter()
            .find(|a| a.id == "play")
            .unwrap()
            .contract
            .availability,
        ui::tilo_actions::Availability::Available
    );
}
