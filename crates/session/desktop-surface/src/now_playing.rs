//! What plays, as every surface that shows it reads it: the player in front
//! and its song, the other players one can switch to, where its sound can go,
//! and what another of the person's devices hands this one.
//!
//! One reading, kept here, so the Quick Settings card, the desk's music
//! widget, the lock screen's card and the bar's pill can never disagree about
//! the song. A host that runs the media stack builds it from the audio
//! service's `media` reading ([`now_playing`]); the transport every surface
//! offers asks the player this names.

use ui::widget::TransportArt;

/// A song, and what its player offers.
#[expect(
    clippy::struct_excessive_bools,
    reason = "what a player is doing and what it offers, as MPRIS states them: independent flags"
)]
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NowPlaying {
    pub title: String,
    /// Who it is by, else the app, when that is not the title.
    pub subtitle: String,
    /// The app that plays it.
    pub player: String,
    /// The app behind the player, by package id, when the launcher verified it: what a surface
    /// opens to show the player itself.
    pub app: Option<String>,
    /// Its cover, or the cover a surface draws for a song without one.
    pub art: TransportArt,
    pub playing: bool,
    pub can_toggle: bool,
    pub can_previous: bool,
    pub can_next: bool,
    /// Where the song is, and how long it is; the length is zero when the player does not say or
    /// cannot seek.
    pub position_ms: f64,
    pub duration_ms: f64,
    /// Every running player, the one in front marked.
    pub players: Vec<Player>,
    /// Where its sound can play, the current one marked.
    pub outputs: Vec<Output>,
    /// The session another of the person's devices hands this one, which the card offers to
    /// continue here.
    pub offer: Option<Offer>,
}

impl NowPlaying {
    /// The song as the toolkit's one song face paints it.
    #[must_use]
    pub fn song(&self) -> ui::widget::Song {
        ui::widget::Song {
            title: self.title.clone(),
            subtitle: self.subtitle.clone(),
            detail: String::new(),
            error: false,
            art: self.art.clone(),
            art_name: self.title.clone(),
        }
    }

    /// How far into the song it is, from 0 to 1, while the player says how long it is.
    #[must_use]
    pub fn progress(&self) -> Option<f64> {
        (self.duration_ms > 0.0).then(|| (self.position_ms / self.duration_ms).clamp(0.0, 1.0))
    }

    /// Whether the player can go to a point in the song.
    #[must_use]
    pub fn seekable(&self) -> bool {
        self.duration_ms > 0.0
    }

    /// Where the song stands and what is left of it, in whole seconds, as the card's bar labels
    /// them: missing while the player does not say how long it is.
    #[must_use]
    pub fn times(&self) -> Option<(u64, u64)> {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a song's length in seconds is far inside u64"
        )]
        let round = |milliseconds: f64| (milliseconds.max(0.0) / 1_000.0).round() as u64;
        let length = (self.duration_ms > 0.0).then(|| round(self.duration_ms))?;
        let elapsed = round(self.position_ms).min(length);
        Some((elapsed, length.saturating_sub(elapsed)))
    }

    /// What another of the person's devices offers to continue here: a title and no controls,
    /// because nothing here plays it yet.
    #[must_use]
    pub fn offered(offer: &Offer) -> Self {
        Self {
            title: offer.title.clone(),
            subtitle: offer.source_label.clone(),
            playing: offer.playing,
            position_ms: offer.position_ms,
            offer: Some(offer.clone()),
            ..Self::default()
        }
    }

    /// A song playing in one player with two outputs, for shots and tests.
    #[must_use]
    pub fn fixture() -> Self {
        Self {
            title: "Clair de lune".into(),
            subtitle: "Claude Debussy".into(),
            player: "Music".into(),
            app: Some("so.tilo.music".into()),
            art: TransportArt::Picture(ui::widget::PictureSource {
                src: ui::headless::FIXTURE_PICTURE.to_owned(),
            }),
            playing: true,
            can_toggle: true,
            can_previous: true,
            can_next: true,
            position_ms: 72_000.0,
            duration_ms: 300_000.0,
            players: vec![
                Player {
                    id: "org.mpris.MediaPlayer2.music".into(),
                    name: "Music".into(),
                    active: true,
                },
                Player {
                    id: "org.mpris.MediaPlayer2.browser".into(),
                    name: "Browser".into(),
                    active: false,
                },
            ],
            outputs: vec![
                Output {
                    token: "local".into(),
                    label: "On this computer".into(),
                    current: true,
                },
                Output {
                    token: "living-room".into(),
                    label: "Living room".into(),
                    current: false,
                },
            ],
            offer: None,
        }
    }
}

/// One running player.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Player {
    /// The bus name the desktop sends its commands to.
    pub id: String,
    pub name: String,
    pub active: bool,
}

/// One place its sound can go.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Output {
    /// The destination's continuity token.
    pub token: String,
    pub label: String,
    pub current: bool,
}

/// A session another device hands this one, at the point it stopped.
/// The session that received it publishes it beside the person's devices, so the card offers
/// exactly the hand-off a reading names and continuing it here names the same one back.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Offer {
    /// The handoff's request, 32 hex digits: the card names it when it asks to continue it here.
    pub request_id: String,
    /// The app that plays it, by package id.
    pub package_id: String,
    pub title: String,
    /// The device it comes from, as the person knows it.
    pub source_label: String,
    /// What it plays, as the locator this device resumes.
    pub source: tilo_media::handoff::HandoffSource,
    /// Where it stopped, in milliseconds.
    pub position_ms: f64,
    pub playing: bool,
}

/// The reading of the players, and where it stands, as the audio service published it.
#[cfg(not(target_arch = "wasm32"))]
pub mod playing {
    use std::time::Instant;

    use ui::widget::TransportArt;

    use super::{NowPlaying, Offer, Output, Player};

    /// What plays at `now` : the player `chosen` names while the service reads it, else the one
    /// playing, else the one paused, else the first. `None` while no player holds anything and none
    /// stands paused, and nothing is handed over.
    #[must_use]
    pub fn now_playing(
        media: &audio::media::Media,
        read_at: Option<Instant>,
        now: Instant,
        chosen: Option<&str>,
        offer: Option<&Offer>,
    ) -> Option<NowPlaying> {
        let outputs: Vec<Output> = media
            .outputs
            .iter()
            .map(|output| Output {
                token: output.token.clone(),
                label: output.label.clone(),
                current: output.current,
            })
            .collect();
        if !media
            .players
            .iter()
            .any(|player| player.playing || player.paused)
        {
            return offer.map(|offer| NowPlaying {
                outputs,
                ..NowPlaying::offered(offer)
            });
        }
        let front = chosen
            .and_then(|bus| media.players.iter().find(|player| player.bus == bus))
            .or_else(|| media.players.iter().find(|player| player.playing))
            .or_else(|| media.players.iter().find(|player| player.paused))
            .or_else(|| media.players.first())?;
        let name = front.identity.clone();
        let title = front
            .title
            .clone()
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| name.clone());
        let subtitle = front
            .artist
            .clone()
            .filter(|artist| !artist.trim().is_empty())
            .or_else(|| (title != name).then(|| name.clone()))
            .unwrap_or_default();
        // A player that cannot be taken to a point does not say how long what it plays is, so no
        // surface shows a progress bar for it.
        #[allow(
            clippy::cast_precision_loss,
            reason = "a recording's length needs no more than f64's 52-bit mantissa"
        )]
        let duration_ms = if front.can_seek {
            front.duration_ms.map_or(0.0, |duration| duration as f64)
        } else {
            0.0
        };
        #[allow(
            clippy::cast_precision_loss,
            reason = "a playhead in milliseconds needs no more than f64's 52-bit mantissa"
        )]
        let position_ms = playhead_at(front, read_at, now) as f64;
        Some(NowPlaying {
            title,
            subtitle,
            player: name,
            app: front.app_id.clone(),
            art: cover(front.art.clone()),
            playing: front.playing,
            can_toggle: if front.playing {
                front.can_pause
            } else {
                front.can_play
            },
            can_previous: front.can_previous,
            can_next: front.can_next,
            position_ms,
            duration_ms,
            players: media
                .players
                .iter()
                .map(|player| Player {
                    id: player.bus.clone(),
                    name: player.identity.clone(),
                    active: player.bus == front.bus,
                })
                .collect(),
            outputs,
            offer: offer.cloned(),
        })
    }

    /// `art` as the picture address a surface reads: a small `data:` picture cut from the cover the
    /// player sent, so a surface in its own process (the lock screen) shows the same cover without
    /// the playhead's pixels crossing to it. Empty when the player sent no cover.
    #[must_use]
    pub fn artwork_address(art: &TransportArt) -> String {
        /// The most the cover is encoded to, on a side: a card shows it small,
        /// and the picture travels to another process with the song's own name.
        const EDGE: u32 = 112;
        let TransportArt::Image(image) = art else {
            return String::new();
        };
        ui::held_images::cover(image, EDGE)
    }

    /// Where `player`'s playhead stands at `now`: where `media` published it,
    /// advanced at its rate while it plays, bounded by what it plays.
    #[must_use]
    pub fn playhead_at(
        player: &audio::media::Player,
        read_at: Option<Instant>,
        now: Instant,
    ) -> u64 {
        let elapsed = read_at.map_or(0.0, |at| now.saturating_duration_since(at).as_secs_f64());
        let rate = if player.rate.is_finite() && player.rate > 0.0 {
            player.rate
        } else {
            1.0
        };
        let advanced = if player.playing {
            elapsed * rate * 1_000.0
        } else {
            0.0
        };
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss,
            reason = "a playhead in a recording is far inside u64's milliseconds"
        )]
        let position = (player.position_ms as f64 + advanced) as u64;
        match player.duration_ms {
            Some(duration) if duration > 0 => position.min(duration),
            _ => position,
        }
    }

    /// `image` as the transport draws a cover, or the placeholder when it does not decode.
    pub fn cover(image: Option<audio::ArtworkImage>) -> TransportArt {
        thread_local! {
            /// The cover made last, with the pixels it was made from: a cover read again — a frame,
            /// a pass over the readings, the card the panel builds again — is the picture already
            /// made.
            static MADE: std::cell::RefCell<Option<(String, TransportArt)>> =
                const { std::cell::RefCell::new(None) };
        }
        let Some(image) = image else {
            return TransportArt::Placeholder;
        };
        MADE.with(|made| {
            let mut made = made.borrow_mut();
            if let Some((from, art)) = made.as_ref()
                && *from == image.rgba
            {
                return art.clone();
            }
            let art = audio::client::decode_artwork(image.clone()).map_or(
                TransportArt::Placeholder,
                |art| {
                    TransportArt::Image(ui::raster::rgba8(
                        ui::vello::peniko::Blob::new(art.rgba.clone()),
                        art.width,
                        art.height,
                    ))
                },
            );
            *made = Some((image.rgba, art.clone()));
            art
        })
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use playing::{artwork_address, cover, now_playing, playhead_at};

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "now_playing_tests.rs"]
mod tests;
