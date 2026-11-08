//! What the surfaces read of the player: the song, the player in front, and whether there is
//! anything to steer at all.

use std::time::Instant;

use super::*;

#[test]
fn a_player_in_front_names_the_song_and_where_it_is() {
    let media = audio::media::Media {
        players: vec![
            player("a", true, "Harbour", "Ada"),
            player("b", false, "Tea", "Bo"),
        ],
        ..audio::media::Media::default()
    };
    let now = Instant::now();
    let playing = now_playing(&media, Some(now), now, None, None).expect("a song");
    assert_eq!(playing.title, "Harbour");
    assert_eq!(playing.subtitle, "Ada");
    assert_eq!(playing.player, "Music");
    assert!(playing.playing);
    assert_eq!(playing.players.len(), 2);
    assert!(playing.players[0].active);
    assert_eq!(playing.progress(), Some(0.0));
}

#[test]
fn a_chosen_player_stays_the_one_steered_while_it_runs() {
    let media = audio::media::Media {
        players: vec![
            player("a", true, "Harbour", "Ada"),
            player("b", false, "Tea", "Bo"),
        ],
        ..audio::media::Media::default()
    };
    let now = Instant::now();
    let playing = now_playing(&media, Some(now), now, Some("b"), None).expect("a song");
    assert_eq!(playing.title, "Tea");
    assert!(!playing.playing, "the chosen player stands paused");
    assert!(playing.players[1].active);
}

#[test]
fn nothing_playing_shows_only_what_another_device_offers() {
    let now = Instant::now();
    let media = audio::media::Media::default();
    assert!(now_playing(&media, Some(now), now, None, None).is_none());
}

#[test]
fn a_song_says_how_far_into_it_it_is_only_when_its_length_is_known() {
    let mut playing = NowPlaying {
        position_ms: 30_000.0,
        duration_ms: 120_000.0,
        ..NowPlaying::default()
    };
    assert_eq!(playing.progress(), Some(0.25));
    playing.duration_ms = 0.0;
    assert_eq!(playing.progress(), None);
    playing.duration_ms = 60_000.0;
    playing.position_ms = 90_000.0;
    assert_eq!(
        playing.progress(),
        Some(1.0),
        "a playhead past the end stays in the song"
    );
}

/// The labels beside the song's bar count from where it plays, and add up to how long it is.
#[test]
fn the_songs_times_round_to_seconds_and_add_up_to_its_length() {
    let mut playing = NowPlaying {
        position_ms: 72_400.0,
        duration_ms: 300_400.0,
        ..NowPlaying::default()
    };
    assert_eq!(playing.times(), Some((72, 228)));
    playing.duration_ms = 0.0;
    assert_eq!(playing.times(), None, "a length nobody knows");
    playing.duration_ms = 60_000.0;
    playing.position_ms = 90_000.0;
    assert_eq!(
        playing.times(),
        Some((60, 0)),
        "a playhead past the end stands at the end"
    );
}

#[test]
fn an_offered_session_names_itself_and_offers_no_control() {
    let offer = Offer {
        request_id: "0123456789abcdef0123456789abcdef".to_owned(),
        package_id: "so.tilo.music".to_owned(),
        title: "Tea".to_owned(),
        source_label: "Laptop".to_owned(),
        source: tilo_media::handoff::HandoffSource::RemoteStream {
            url: "https://music.example/tea".to_owned(),
        },
        position_ms: 0.0,
        playing: true,
    };
    let playing = NowPlaying::offered(&offer);
    assert_eq!(playing.title, "Tea");
    assert_eq!(playing.subtitle, "Laptop");
    assert!(playing.playing);
    assert!(!playing.can_toggle);
    assert_eq!(playing.offer, Some(offer));
}

fn player(bus: &str, playing: bool, title: &str, artist: &str) -> audio::media::Player {
    audio::media::Player {
        bus: bus.to_owned(),
        identity: "Music".to_owned(),
        title: Some(title.to_owned()),
        artist: Some(artist.to_owned()),
        playing,
        paused: !playing,
        can_play: !playing,
        can_pause: playing,
        duration_ms: Some(120_000),
        can_seek: true,
        ..audio::media::Player::default()
    }
}
