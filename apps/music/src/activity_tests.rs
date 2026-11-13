use super::*;

fn playing() -> Playing {
    Playing {
        track: "track-1".to_owned(),
        title: "Tea".to_owned(),
        artist: "Kettle".to_owned(),
        playing: true,
        position_ms: 30_000.0,
        length_ms: 120_000.0,
        artwork: "data:image/png;base64,AAAA".to_owned(),
    }
}

#[test]
fn a_song_is_its_title_its_artist_and_where_the_playhead_stands() {
    let summary = summary(&playing());
    assert_eq!(summary.kind, Kind::Music);
    assert_eq!(summary.id, "track-1", "the song is the activity");
    assert_eq!(summary.title, "Tea");
    assert_eq!(summary.subtitle, "Kettle");
    assert_eq!(summary.hero(), "Tea");
    assert_eq!(summary.artwork, "data:image/png;base64,AAAA");
    assert_eq!(summary.length, Some(120));
    assert_eq!(summary.elapsed_seconds(), Some(30));
    assert_eq!(summary.remaining_seconds(), Some(90));
}

#[test]
fn a_song_is_steered_by_previous_play_pause_and_next() {
    let mut song = playing();
    song.playing = true;
    let controls = summary(&song).controls;
    let ids: Vec<&str> = controls.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, ["previous", "play-pause", "next"]);
    assert_eq!(controls[1].label, "Pause");
    assert_eq!(controls[1].icon, "pause");
    assert_eq!(controls[0].icon, "skip-back");
    assert_eq!(controls[2].icon, "skip-forward");
    song.playing = false;
    let controls = summary(&song).controls;
    assert_eq!(controls[1].label, "Play");
    assert_eq!(controls[1].icon, "play");
}

#[test]
fn a_song_of_unknown_length_shows_no_progress() {
    let mut song = playing();
    song.length_ms = 0.0;
    let summary = summary(&song);
    assert_eq!(summary.progress, None);
    assert_eq!(summary.length, None);
}

#[test]
fn a_song_without_an_artist_or_a_cover_says_only_what_it_knows() {
    let mut song = playing();
    song.artist = String::new();
    song.artwork = String::new();
    let summary = summary(&song);
    assert!(summary.subtitle.is_empty());
    assert!(summary.artwork.is_empty());
}

#[test]
fn the_card_never_asks_to_float_by_itself() {
    let shown = published(Some(&playing()));
    assert!(!shown.overlay, "a song stays in the pill until pulled");
    assert!(shown.summary.is_some());
    assert!(
        published(None).summary.is_none(),
        "nothing plays, nothing shows"
    );
}

#[test]
fn a_press_names_the_control_and_the_song_it_was_pressed_on() {
    let command: Command =
        serde_json::from_value(serde_json::json!({"command": "play-pause", "activity_id": "t1"}))
            .expect("a control press");
    assert_eq!(command.command, "play-pause");
    assert_eq!(command.activity_id.as_deref(), Some("t1"));
}

#[test]
fn a_seek_names_the_point_it_was_left_at() {
    let command: Command = serde_json::from_value(serde_json::json!({
        "command": "seek",
        "activity_id": "t1",
        "fraction": 0.25,
    }))
    .expect("a seek");
    assert_eq!(command.fraction, Some(0.25));
    let press: Command = serde_json::from_value(serde_json::json!({"command": "next"}))
        .expect("a transport press carries no point");
    assert_eq!(press.fraction, None);
}
