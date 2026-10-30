//! The `media:player` resource: what plays, and the transport keys
//! (play/pause, next, previous, stop) as actions, so a media key, a script
//! and an agent steer the same player through the session's door.
//!
//! The keys address whichever player the person last listened to, as MPRIS
//! transport requests do ([`Request::Transport`]); the reading names that
//! player and what it plays.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tilo_actions::ResourceSpec;
use tilo_resource::Effect;
use tilo_service::{offer, unavailable};

use crate::client::PlayerTable;
use crate::{Status, Transport};

/// The resource's id.
pub const MEDIA_PLAYER: &str = "media:player";

/// What the `media:player` resource reads.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Media {
    /// The player the keys steer, by its name for people.
    pub player: Option<String>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub playing: bool,
    pub can_go_next: bool,
    pub can_go_previous: bool,
}

impl Media {
    /// The reading of `table`: its front player.
    #[must_use]
    pub fn of(table: &PlayerTable) -> Self {
        let Some(player) = table.front(None) else {
            return Self::default();
        };
        Self {
            player: Some(player.name()),
            title: player.title.clone(),
            artist: player.artist.clone(),
            playing: table.status(player) == Status::Playing,
            can_go_next: player.can_go_next,
            can_go_previous: player.can_go_previous,
        }
    }
}

/// The `media:player` resource's actions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "so.tilo.audio.media")]
pub enum MediaAction {
    /// Play when paused, pause when playing.
    #[effect(
        own_device,
        label = "Play or pause",
        icon = "play",
        keywords = "music, video, resume, wiedergabe"
    )]
    PlayPause,
    /// Skip to the next track.
    #[effect(
        own_device,
        label = "Next track",
        icon = "skip-forward",
        keywords = "skip"
    )]
    Next,
    /// Go back to the previous track.
    #[effect(
        own_device,
        label = "Previous track",
        icon = "skip-back",
        keywords = "back"
    )]
    Previous,
    /// Stop playing.
    #[effect(own_device, label = "Stop", icon = "square", keywords = "halt")]
    Stop,
}

impl MediaAction {
    /// The transport request it sends.
    #[must_use]
    pub const fn transport(self) -> Transport {
        match self {
            Self::PlayPause => Transport::PlayPause,
            Self::Next => Transport::Next,
            Self::Previous => Transport::Previous,
            Self::Stop => Transport::Stop,
        }
    }
}

/// The media player resource reading `media`.
#[must_use]
pub fn resource(media: &Media) -> ResourceSpec {
    let mut resource = offer::<MediaAction>(ResourceSpec::new(
        MEDIA_PLAYER,
        "so.tilo.audio.media",
        "Player",
    ));
    if media.player.is_none() {
        for action in ["play-pause", "next", "previous", "stop"] {
            unavailable(&mut resource, action, "no_player", "Nothing is playing");
        }
        return resource;
    }
    if !media.can_go_next {
        unavailable(&mut resource, "next", "no_next", "The player cannot skip");
    }
    if !media.can_go_previous {
        unavailable(
            &mut resource,
            "previous",
            "no_previous",
            "The player cannot go back",
        );
    }
    resource
}

/// The specs a reading published as lines stands for.
#[must_use]
pub fn publish(readings: &BTreeMap<String, Value>) -> Vec<ResourceSpec> {
    let media = readings
        .get(MEDIA_PLAYER)
        .and_then(|value| serde_json::from_value::<Media>(value.clone()).ok())
        .unwrap_or_default();
    vec![resource(&media)]
}

#[cfg(feature = "service")]
mod service;
#[cfg(feature = "service")]
pub use service::MediaService;
