//! The now-playing card, as the panel shows it: the audio service's
//! `media` reading (the players, where each's playhead is, how long what it
//! plays is, the cover it names and where its sound can go) folded into the
//! card, and the player the person chose to steer.
//!
//! Nothing here talks to a player: the card's presses are the panel's
//! effects, the session hands them to the audio service's actions, and the
//! reading it publishes afterwards is what the card shows.

use std::time::Instant;

use audio::client::decode_artwork;
use audio::media::{Media as Reading, Output, Player};
use media::handoff::HandoffSource;
use ui::widget::TransportArt;

use crate::{MediaOffer, MediaOutput, MediaPlayer, NowPlaying};

/// What the person is told when this device cannot open what the other
/// device played.
const CANNOT_RESUME: &str = "this device cannot resume that source";

/// The now-playing card and the choices around it.
#[derive(Default)]
pub(crate) struct Media {
    /// The reading and when it arrived: the playhead moves from there while
    /// a player plays.
    reading: Option<(Reading, Instant)>,
    /// The player the person chose to steer, by bus name.
    chosen: Option<String>,
    /// The cover made into a picture, with the cover it was made from, so
    /// an unchanged cover is the same picture.
    art: Option<(String, ui::vello::peniko::ImageData)>,
}

impl Media {
    /// Land what the audio service reads of the players, read at `now`.
    pub(crate) fn land(&mut self, reading: Reading, now: Instant) {
        self.reading = Some((reading, now));
    }

    /// Steer `bus` from now on, while it is a player the service reads.
    pub(crate) fn choose(&mut self, bus: &str) -> Result<(), String> {
        let known = self
            .reading
            .as_ref()
            .is_some_and(|(reading, _)| reading.players.iter().any(|player| player.bus == bus));
        if !known {
            return Err(format!("no player {bus}"));
        }
        self.chosen = Some(bus.to_owned());
        Ok(())
    }

    /// The player the card steers: the one the person chose while it is
    /// still around, else whatever plays, else whatever is paused, else the
    /// first.
    fn front(&self) -> Option<&Player> {
        let (reading, _) = self.reading.as_ref()?;
        self.chosen
            .as_ref()
            .and_then(|bus| reading.players.iter().find(|player| player.bus == *bus))
            .or_else(|| reading.players.iter().find(|player| player.playing))
            .or_else(|| reading.players.iter().find(|player| player.paused))
            .or_else(|| reading.players.first())
    }

    /// Where `player`'s playhead is at `now`: where the service read it,
    /// advanced at its rate while it plays, bounded by what it plays.
    fn position_at(&self, player: &Player, now: Instant) -> u64 {
        let elapsed = self.reading.as_ref().map_or(0.0, |(_, at)| {
            now.saturating_duration_since(*at).as_secs_f64()
        });
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

    /// The cover of `player`, made into a picture once per cover.
    fn art(&mut self, player: &Player) -> Option<ui::vello::peniko::ImageData> {
        let image = player.art.as_ref()?;
        if let Some((made, picture)) = &self.art
            && *made == image.rgba
        {
            return Some(picture.clone());
        }
        let art = decode_artwork(image.clone())?;
        let picture = ui::raster::rgba8(
            ui::vello::peniko::Blob::new(art.rgba.clone()),
            art.width,
            art.height,
        );
        self.art = Some((image.rgba.clone(), picture.clone()));
        Some(picture)
    }

    /// What the card shows at `now`: the player in front while anything
    /// plays or is paused, and the other device's session while one is
    /// handed over ([`offer`], as the session publishes it); `None`
    /// otherwise.
    pub(crate) fn now_playing(
        &mut self,
        now: Instant,
        offer: Option<&MediaOffer>,
    ) -> Option<NowPlaying> {
        let active = self.reading.as_ref().is_some_and(|(reading, _)| {
            reading
                .players
                .iter()
                .any(|player| player.playing || player.paused)
        });
        if !active {
            return self.offer_card(offer);
        }
        let Some(front) = self.front().cloned() else {
            return self.offer_card(offer);
        };
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
        #[allow(
            clippy::cast_precision_loss,
            reason = "a recording's length in milliseconds needs no more than f64's 52-bit mantissa"
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
        let position_ms = self.position_at(&front, now) as f64;
        let art = self.art(&front);
        let outputs = self.outputs();
        let players = self
            .reading
            .as_ref()
            .map(|(reading, _)| {
                reading
                    .players
                    .iter()
                    .map(|player| MediaPlayer {
                        id: player.bus.clone(),
                        name: player.identity.clone(),
                        active: player.bus == front.bus,
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(NowPlaying {
            title,
            subtitle,
            player_name: name,
            art: art.map_or(TransportArt::Placeholder, TransportArt::Image),
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
            players,
            outputs,
            offer: None,
        })
    }

    /// Where what plays can go, as the audio service reads it.
    fn outputs(&self) -> Vec<MediaOutput> {
        self.reading
            .as_ref()
            .map(|(reading, _)| reading.outputs.iter().map(Media::destination).collect())
            .unwrap_or_default()
    }

    /// One destination of the card, as the audio service names it.
    fn destination(output: &Output) -> MediaOutput {
        MediaOutput {
            token: output.token.clone(),
            label: output.label.clone(),
            current: output.current,
        }
    }

    /// The card that only another device's session makes: what it played,
    /// where it stopped, and the offer to continue it here.
    fn offer_card(&self, offer: Option<&MediaOffer>) -> Option<NowPlaying> {
        let offer = offer.cloned()?;
        Some(NowPlaying {
            title: offer.title.clone(),
            playing: offer.playing,
            position_ms: offer.position_ms,
            outputs: self.outputs(),
            offer: Some(offer),
            ..NowPlaying::default()
        })
    }
}

/// Whether `request` is a hand-off this device can continue: what the other
/// device played off a stream or a file it can open itself. Its work in a
/// workspace path or an app's own resource has to be brought over first,
/// which is not this card's.
pub(crate) fn resumable(offer: &MediaOffer, request: &str) -> Result<(), String> {
    if offer.request_id != request {
        return Err("no such handoff".to_owned());
    }
    match &offer.source {
        HandoffSource::RemoteStream { .. } | HandoffSource::LocalFile { .. } => Ok(()),
        HandoffSource::Workspace { .. } | HandoffSource::AppResource { .. } => {
            Err(CANNOT_RESUME.to_owned())
        }
    }
}
