//! What a player is to the rest of the audio service: its name, whether and
//! where it can be taken in what plays, and the cover it names decoded.
//! Nothing here talks to the bus; the one MPRIS client is [`crate::mpris`].

use std::sync::Arc;

use crate::{ArtworkImage, Player, PlayerCommand};

/// The prefix of every MPRIS player's bus name.
pub const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";

/// A decoded cover, scaled down by the helper. Only a cover a player names
/// as a local file is ever decoded: nothing is fetched on a player's behalf.
#[derive(Clone, Debug)]
pub struct Artwork {
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<Vec<u8>>,
}

impl PartialEq for Artwork {
    fn eq(&self, other: &Self) -> bool {
        self.width == other.width
            && self.height == other.height
            && Arc::ptr_eq(&self.rgba, &other.rgba)
    }
}

/// The cover the helper sent, when its pixels fill it.
#[must_use]
pub fn decode_artwork(image: ArtworkImage) -> Option<Artwork> {
    use base64::Engine as _;
    let rgba = base64::engine::general_purpose::STANDARD
        .decode(image.rgba)
        .ok()?;
    let size = u64::from(image.width)
        .checked_mul(u64::from(image.height))?
        .checked_mul(4)?;
    (u64::try_from(rgba.len()).ok()? == size).then(|| Artwork {
        width: image.width,
        height: image.height,
        rgba: Arc::new(rgba),
    })
}

impl Player {
    /// The player's name for itself, else the first part of its bus name.
    #[must_use]
    pub fn name(&self) -> String {
        self.identity.clone().unwrap_or_else(|| {
            self.bus_name
                .trim_start_matches(MPRIS_PREFIX)
                .split('.')
                .next()
                .unwrap_or_default()
                .to_string()
        })
    }

    /// A seek bar is offered only when the player can go to an absolute
    /// point: it says so, knows the track's length and names the track.
    #[must_use]
    pub fn seekable(&self) -> bool {
        self.can_seek && self.length_us.is_some_and(|length| length > 0) && self.track_id.is_some()
    }

    /// The command that takes the player to `fraction` of its track.
    #[must_use]
    pub fn seek(&self, fraction: f64) -> Option<PlayerCommand> {
        if !self.seekable() {
            return None;
        }
        let length = self.length_us?;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_precision_loss,
            reason = "the product is clamped to the track length, which is an i64"
        )]
        let position_us = ((length as f64) * fraction.clamp(0.0, 1.0)).round() as i64;
        Some(PlayerCommand::SetPosition {
            track_id: self.track_id.clone()?,
            position_us: position_us.clamp(0, length),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Status;

    fn player(bus: &str, status: Status) -> Player {
        Player {
            bus_name: format!("{MPRIS_PREFIX}{bus}"),
            unique_name: ":1.5".into(),
            pid: None,
            package_id: None,
            desktop_entry: None,
            identity: None,
            status,
            title: Some("Song".into()),
            artist: None,
            url: None,
            art_url: None,
            track_id: Some("/t/1".into()),
            length_us: Some(10_000_000),
            position_us: 4_000_000,
            rate: 2.0,
            can_play: true,
            can_pause: true,
            can_seek: true,
            can_go_next: false,
            can_go_previous: false,
        }
    }

    #[test]
    fn seeking_needs_length_track_and_capability() {
        let p = player("a", Status::Playing);
        assert_eq!(
            p.seek(0.5),
            Some(PlayerCommand::SetPosition {
                track_id: "/t/1".into(),
                position_us: 5_000_000,
            })
        );
        assert_eq!(
            p.seek(7.0),
            Some(PlayerCommand::SetPosition {
                track_id: "/t/1".into(),
                position_us: 10_000_000,
            })
        );
        for broken in [
            Player {
                can_seek: false,
                ..p.clone()
            },
            Player {
                track_id: None,
                ..p.clone()
            },
            Player {
                length_us: None,
                ..p
            },
        ] {
            assert_eq!(broken.seek(0.5), None);
        }
    }

    #[test]
    fn players_are_named_and_their_covers_arrive_as_rgba_of_their_size() {
        use base64::Engine as _;
        assert_eq!(player("web.tab3", Status::Paused).name(), "web");
        let music = Player {
            identity: Some("Music".into()),
            art_url: Some("file:///cover.png".into()),
            ..player("music", Status::Playing)
        };
        assert_eq!(music.name(), "Music");
        let encode = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
        let cover = decode_artwork(ArtworkImage {
            width: 2,
            height: 1,
            rgba: encode(&[1; 8]),
        })
        .expect("a 2×1 cover");
        assert_eq!((cover.width, cover.height, cover.rgba.len()), (2, 1, 8));
        assert!(
            decode_artwork(ArtworkImage {
                width: 2,
                height: 2,
                rgba: encode(&[1; 8]),
            })
            .is_none(),
            "a cover whose pixels do not fill it is dropped"
        );
    }
}
