//! The player the now-playing shots show.

use std::time::Instant;

/// The song the now-playing shots show: a player with a cover, a known
/// length and every control.
pub(super) fn shots_player() -> shell::shell::now_playing::PlayerSnapshot {
    use shell::shell::now_playing::{Artwork, PlaybackStatus, PlayerSnapshot};
    const SIDE: u8 = 64;
    let rgba = (0..SIDE)
        .flat_map(|y| (0..SIDE).map(move |x| (x, y)))
        .flat_map(|(x, y)| {
            [
                x.saturating_mul(2).saturating_add(60),
                y.saturating_mul(2).saturating_add(40),
                160,
                255,
            ]
        })
        .collect();
    PlayerSnapshot {
        bus_name: "org.mpris.MediaPlayer2.spotify.instance7".to_string(),
        identity: "Spotify".to_string(),
        status: PlaybackStatus::Playing,
        title: Some("Teardrop".to_string()),
        artist: Some("Massive Attack".to_string()),
        artwork: Some(Artwork {
            width: u32::from(SIDE),
            height: u32::from(SIDE),
            rgba: std::sync::Arc::new(rgba),
        }),
        track_id: Some("/org/tilo/shots/track/1".to_string()),
        length_us: Some(330_000_000),
        position_us: 94_000_000,
        can_play: true,
        can_pause: true,
        can_seek: true,
        can_go_next: true,
        can_go_previous: true,
        observed_at: Instant::now(),
    }
}
