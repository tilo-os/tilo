use super::*;
use crate::actions::MusicAction;
use ui::tilo_actions::Action;
use ui::tilo_resource::Resource as _;

fn library() -> Library {
    let mut lib = crate::seed::sample_library();
    lib.liked.clear();
    lib.saved_albums.clear();
    lib.followed_artists.clear();
    lib.recommendations = Preferences::default();
    lib
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
    let mut doc = ui::tilo_resource::Doc::new();
    let lib = library();
    crate::doc::seed(&mut doc, &lib);
    let track = lib.tracks.values().next().unwrap();
    let artist = track.artist_id.clone();
    MusicAction::SetRecommendationArtist {
        artist,
        preferred: true,
    }
    .apply(&mut doc)
    .unwrap();
    let before = crate::doc::load_library(&doc);
    assert!(before.followed_artists.is_empty());
    assert!(
        suggestions(&before, before.tracks.len())
            .iter()
            .any(|s| s.track == track.id)
    );
    MusicAction::SetRecommendationDismissed {
        track: track.id.clone(),
        dismissed: true,
    }
    .apply(&mut doc)
    .unwrap();
    let bytes = doc.save();
    let mut loaded = ui::tilo_resource::Doc::load(&bytes).unwrap();
    assert!(
        suggestions(&crate::doc::load_library(&loaded), 50)
            .iter()
            .all(|s| s.track != track.id)
    );
    MusicAction::SetRecommendationDismissed {
        track: track.id.clone(),
        dismissed: false,
    }
    .apply(&mut loaded)
    .unwrap();
    assert!(
        suggestions(&crate::doc::load_library(&loaded), 50)
            .iter()
            .any(|s| s.track == track.id)
    );
}

#[test]
fn recommendations_concurrent_feedback_merges_and_stale_positive_targets_fail() {
    use automerge::transaction::Transactable as _;
    use tilo_automerge::ApplyIncoming as _;
    let mut base = ui::tilo_resource::Doc::new();
    let lib = library();
    crate::doc::seed(&mut base, &lib);
    // A pre-feature library has no recommendation keys at all.
    for input in Input::ALL {
        base.delete(
            automerge::ROOT,
            format!("recommendations/input/{}", input.key()),
        )
        .unwrap();
    }
    base.commit();
    let mut peer = base.fork();
    MusicAction::SetRecommendationInput {
        input: Input::Likes,
        enabled: false,
    }
    .apply(&mut base)
    .unwrap();
    let track = lib.tracks.values().next().unwrap();
    MusicAction::SetRecommendationDismissed {
        track: track.id.clone(),
        dismissed: true,
    }
    .apply(&mut peer)
    .unwrap();
    base.merge_incoming(&mut peer).unwrap();
    let prefs = read(&base);
    assert!(!prefs.likes);
    assert!(prefs.dismissed.contains(&track.id));
    peer.merge_incoming(&mut base).unwrap();
    assert_eq!(read(&peer), prefs);
    let before = base.get_heads();
    assert!(
        MusicAction::SetRecommendationArtist {
            artist: "unknown".into(),
            preferred: true
        }
        .apply(&mut base)
        .is_err()
    );
    assert_eq!(base.get_heads(), before);
}

#[test]
fn recommendations_resources_explain_and_offer_reversible_feedback() {
    let mut lib = library();
    lib.liked.insert("tk_g1".into());
    let pick = suggestions(&lib, 1).pop().unwrap();
    let id = format!("music.recommendation:{}", pick.track);
    let resource = crate::Music::child(&lib, &id).unwrap();
    assert!(resource.label.contains(&pick.reason.label(&lib)));
    let dismiss = resource.actions.iter().find(|a| a.id == "dismiss").unwrap();
    assert!(dismiss.contract.reversible);
    assert!(crate::Music::child(&lib, "music.recommendation-input:unknown").is_none());
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
    let mut doc = ui::tilo_resource::Doc::new();
    crate::doc::seed(&mut doc, &lib);
    assert_eq!(
        crate::doc::load_library(&doc).recommendations,
        lib.recommendations
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

#[test]
fn recommendations_stale_feedback_can_be_cleared_without_reimporting_music() {
    let mut doc = ui::tilo_resource::Doc::new();
    let mut lib = Library::default();
    lib.recommendations.dismissed.insert("deleted".into());
    lib.recommendations
        .preferred_artists
        .insert("deleted-artist".into());
    crate::doc::seed(&mut doc, &lib);
    assert_ne!(
        crate::Music::child(&lib, "music.recommendation:deleted")
            .unwrap()
            .actions
            .iter()
            .find(|a| a.id == "play")
            .unwrap()
            .contract
            .availability,
        ui::tilo_actions::Availability::Available
    );
    MusicAction::SetRecommendationDismissed {
        track: "deleted".into(),
        dismissed: false,
    }
    .apply(&mut doc)
    .unwrap();
    MusicAction::SetRecommendationArtist {
        artist: "deleted-artist".into(),
        preferred: false,
    }
    .apply(&mut doc)
    .unwrap();
    let state = crate::doc::load_library(&doc);
    assert!(state.recommendations.dismissed.is_empty());
    assert!(state.recommendations.preferred_artists.is_empty());
    assert!(state.tracks.is_empty());
}
