//! The media service over the MPRIS client.

use std::sync::mpsc::{Receiver, channel};
use std::time::Instant;

use serde_json::{Map, Value};
use tilo_actions::{DoorError, ResourceSpec};
use tilo_service::{Service, Started, Wake, decode, reading};

use super::{MEDIA_PLAYER, Media, MediaAction, resource};
use crate::client::PlayerTable;
use crate::mpris::Mpris;
use crate::{Event, Request};

/// The media service: the players MPRIS describes, and the transport keys
/// sent through it.
pub struct MediaService {
    media: Media,
    table: PlayerTable,
    mpris: Mpris,
    events: Receiver<Event>,
    sent: u64,
}

impl MediaService {
    /// Follow the session bus's players on `runtime`, `wake` called
    /// whenever they change.
    #[must_use]
    pub fn start(runtime: tokio::runtime::Handle, wake: Option<Wake>) -> Self {
        let (events, received) = channel();
        let mpris = Mpris::start(runtime, move |event| {
            if events.send(event).is_ok()
                && let Some(wake) = &wake
            {
                wake();
            }
        });
        Self::over(mpris, received)
    }

    /// The service over `mpris`, whose events arrive on `events`.
    #[must_use]
    pub fn over(mpris: Mpris, events: Receiver<Event>) -> Self {
        Self {
            media: Media::default(),
            table: PlayerTable::new(),
            mpris,
            events,
            sent: 0,
        }
    }

    /// The MPRIS client it drives, for the policy's pause.
    #[must_use]
    pub const fn mpris(&self) -> &Mpris {
        &self.mpris
    }

    /// What it reads now.
    #[must_use]
    pub const fn media(&self) -> &Media {
        &self.media
    }
}

impl Service for MediaService {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![resource(&self.media)]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        (resource == MEDIA_PLAYER).then(|| reading(&self.media))
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, DoorError> {
        let action = decode::<MediaAction>(&self.resources(), resource, action, arguments)?;
        self.sent = self.sent.wrapping_add(1);
        self.mpris.handle(Request::Transport {
            id: self.sent,
            command: action.transport(),
        });
        Ok(Started::Accepted)
    }

    fn poll(&mut self) -> bool {
        let now = Instant::now();
        while let Ok(event) = self.events.try_recv() {
            self.table.apply(event, now);
        }
        let media = Media::of(&self.table);
        let changed = media != self.media;
        self.media = media;
        changed
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::channel;

    use serde_json::Map;
    use tilo_actions::DoorError;
    use tilo_service::{Service as _, Started};

    use super::MediaService;
    use crate::mpris::Mpris;
    use crate::mpris::test_bus::{PrivateBus, called, eventually, serve_playing_probe};

    /// What plays is the reading, and the keys reach the player through
    /// the resource's actions; with nothing playing they are unavailable.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_player_resource_reads_what_plays_and_its_keys_reach_the_player() {
        let bus = PrivateBus::start();
        let (events, received) = channel();
        let mpris = Mpris::start_on(
            tokio::runtime::Handle::current(),
            Some(bus.address.clone()),
            move |event| {
                let _ = events.send(event);
            },
        );
        let mut media = MediaService::over(mpris, received);
        let none = Map::new();
        assert!(matches!(
            media.invoke(super::MEDIA_PLAYER, "play-pause", &none),
            Err(DoorError::Unavailable { .. })
        ));

        let player = serve_playing_probe(&bus).await;
        eventually("the player is read", || {
            media.poll();
            media.media().playing
        })
        .await;
        assert_eq!(media.media().player.as_deref(), Some("Probe"));
        assert_eq!(media.media().title.as_deref(), Some("Song"));

        assert_eq!(
            media.invoke(super::MEDIA_PLAYER, "play-pause", &none),
            Ok(Started::Accepted)
        );
        let reached = || {
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(called(&player))
            })
        };
        eventually("the key reaches the player", || {
            reached().iter().any(|call| call == "PlayPause")
        })
        .await;
    }
}
