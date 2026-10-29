//! Music's identity for the toolkit-owned desktop media-control service.
//!
//! Snapshots, command delivery, publication, and host support live in `ui`.
//! Music only supplies its identity and routes commands through its actions.

pub use ui::mpris::{
    MprisCommand, MprisConfig, MprisHandle, OBJECT_PATH, PlaybackStatus, PlayerSnapshot,
    track_object_path,
};

/// Music's player. Its bus name, also named in the shell's playback handoff
/// record, is the launched package's (see [`MprisConfig::new`]).
pub fn config() -> MprisConfig {
    let mut config = MprisConfig::new("tilo-music", "Tilo Music");
    config.desktop_entry = "music".into();
    config.supported_uri_schemes = vec!["http".into(), "https".into()];
    config.supported_mime_types = vec!["audio/mpeg".into(), "audio/mp4".into(), "audio/ogg".into()];
    config
}

/// What the platform player's system controls are told plays, from what
/// MPRIS publishes.
pub(crate) fn now_playing_of(snap: &PlayerSnapshot) -> media::NowPlaying {
    media::NowPlaying {
        title: snap.title.clone(),
        artist: snap.artist.clone(),
        album: snap.album.clone(),
        artwork_url: Some(snap.art_url.clone()).filter(|url| !url.is_empty()),
    }
}

/// A press on the platform player's system controls, as the MPRIS command
/// that asks the same.
pub(crate) fn command_for(action: media::NowPlayingAction) -> MprisCommand {
    match action {
        media::NowPlayingAction::Play => MprisCommand::Play,
        media::NowPlayingAction::Pause => MprisCommand::Pause,
        media::NowPlayingAction::Stop => MprisCommand::Stop,
        media::NowPlayingAction::Next => MprisCommand::Next,
        media::NowPlayingAction::Previous => MprisCommand::Previous,
        media::NowPlayingAction::SeekTo(at) => {
            MprisCommand::SetPosition(i64::try_from(at.as_micros()).unwrap_or(i64::MAX))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_media_control_retains_music_identity() {
        let config = config();
        assert!(config.bus_name.starts_with("org.mpris.MediaPlayer2."));
        assert_eq!(config.identity, "Tilo Music");
        assert_eq!(config.desktop_entry, "music");
        assert_eq!(config.supported_uri_schemes, ["http", "https"]);
        assert_eq!(
            config.supported_mime_types,
            ["audio/mpeg", "audio/mp4", "audio/ogg"]
        );
    }

    #[test]
    fn the_platform_players_controls_hear_the_track_and_ask_as_mpris_does() {
        let snap = PlayerSnapshot {
            title: "Harbour".into(),
            artist: "Ada".into(),
            album: "Quays".into(),
            ..PlayerSnapshot::default()
        };
        assert_eq!(
            now_playing_of(&snap),
            media::NowPlaying {
                title: "Harbour".into(),
                artist: "Ada".into(),
                album: "Quays".into(),
                artwork_url: None,
            }
        );
        assert_eq!(
            command_for(media::NowPlayingAction::Next),
            MprisCommand::Next
        );
        assert_eq!(
            command_for(media::NowPlayingAction::Previous),
            MprisCommand::Previous
        );
        assert_eq!(
            command_for(media::NowPlayingAction::SeekTo(
                std::time::Duration::from_secs(2)
            )),
            MprisCommand::SetPosition(2_000_000)
        );
    }
}
