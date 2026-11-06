//! The now-playing card, as the panel shows it: the audio service's `media`
//! reading (the players, where each's playhead is, how long what it plays is,
//! the cover it names and where its sound can go) as the one now-playing
//! reading every surface shows, and the player the person chose to steer.
//!
//! Nothing here talks to a player: the card's presses are the panel's
//! effects, the session hands them to the audio service's actions, and the
//! reading it publishes afterwards is what the card shows.

use std::time::Instant;

use audio::media::Media as Reading;
use media::handoff::HandoffSource;

use crate::{MediaOffer, NowPlaying};

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

    /// What the card shows at `now`: the player in front while anything
    /// plays or is paused, and the other device's session while one is
    /// handed over ([`MediaOffer`], as the session publishes it); `None`
    /// otherwise.
    pub(crate) fn now_playing(
        &self,
        now: Instant,
        offer: Option<&MediaOffer>,
    ) -> Option<NowPlaying> {
        let Some((reading, at)) = self.reading.as_ref() else {
            return offer.map(NowPlaying::offered);
        };
        tilo_desktop_surface::now_playing::now_playing(
            reading,
            Some(*at),
            now,
            self.chosen.as_deref(),
            offer,
        )
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
