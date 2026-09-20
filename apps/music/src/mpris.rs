//! Music's identity for the toolkit-owned desktop media-control service.
//!
//! Snapshots, command delivery, publication, and host support live in `ui`.
//! Music only supplies its identity and routes commands through its actions.

pub use ui::mpris::{
    MprisCommand, MprisConfig, MprisHandle, OBJECT_PATH, PlaybackStatus, PlayerSnapshot,
    track_object_path,
};

/// The bus identity also used by the shell's playback handoff record.
pub const BUS_NAME: &str = "org.mpris.MediaPlayer2.tilo-music";

pub fn config() -> MprisConfig {
    let mut config = MprisConfig::new("tilo-music", "Tilo Music");
    config.desktop_entry = "music".into();
    config.supported_uri_schemes = vec!["http".into(), "https".into()];
    config.supported_mime_types = vec!["audio/mpeg".into(), "audio/mp4".into(), "audio/ogg".into()];
    config
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_media_control_retains_music_identity() {
        let config = config();
        assert_eq!(config.bus_name, BUS_NAME);
        assert_eq!(config.identity, "Tilo Music");
        assert_eq!(config.desktop_entry, "music");
        assert_eq!(config.supported_uri_schemes, ["http", "https"]);
        assert_eq!(
            config.supported_mime_types,
            ["audio/mpeg", "audio/mp4", "audio/ogg"]
        );
    }
}
