//! What plays: the media players on the session bus (MPRIS), published as
//! the `media` resource with one child per player and the actions its
//! controls offer (play or pause, next, previous). The `media` resource
//! itself controls the player in front (the one playing, else the first):
//! what a media key runs, where the playhead is, how long what plays is,
//! where its sound can go and what another of the person's devices hands
//! over. Quick settings, the desktop's search, a media key, a script and an
//! agent all run the same actions by address.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tilo_actions::{ResourcePresentation, ResourceSpec};
use tilo_resource::Effect;
use tilo_service::{offer, unavailable};

use crate::{ArtworkImage, PlayerCommand};

/// The resource the players are published under.
pub const MEDIA: &str = "media";
pub const MEDIA_KIND: &str = "so.tilo.audio.media";
/// The kind of one player's resource.
pub const PLAYER_KIND: &str = "so.tilo.audio.player";
/// What a player's resource id starts with, before its token.
const PLAYER_PREFIX: &str = "media-player:";

/// What plays now.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Media {
    pub players: Vec<Player>,
    /// Where the sound can go, as the audio service reads the graph: the
    /// destinations the card offers.
    pub outputs: Vec<Output>,
    /// Why the last control did not reach its player, until the next one.
    pub problem: Option<String>,
}

/// One place the sound can go.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Output {
    /// The output's continuity token, which choosing it names.
    pub token: String,
    pub label: String,
    /// The one the sound plays through now.
    pub current: bool,
}

/// One media player.
#[expect(
    clippy::struct_excessive_bools,
    reason = "what a player can do now, as MPRIS says it: independent flags"
)]
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Player {
    /// Its stable name in what is published: the bus name says which app
    /// plays, which a published id must not.
    pub token: String,
    /// Its name on the session bus.
    pub bus: String,
    /// The app playing, as it names itself ("Music", "Firefox").
    pub identity: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub playing: bool,
    /// Whether it stands paused rather than playing or stopped.
    pub paused: bool,
    pub can_play: bool,
    pub can_pause: bool,
    pub can_next: bool,
    pub can_previous: bool,
    /// Its cover, as the player names it, decoded and scaled down.
    pub art: Option<ArtworkImage>,
    /// Where the playhead was when this was read, in milliseconds.
    pub position_ms: u64,
    /// How fast the playhead advances while playing.
    pub rate: f64,
    /// How long what plays is, in milliseconds, when the player says.
    pub duration_ms: Option<u64>,
    /// Whether it can be taken to a point in what plays.
    pub can_seek: bool,
    /// The source it plays (`xesam:url`), which another device can
    /// continue.
    pub url: Option<String>,
    /// The app behind it, when the launcher verified it.
    pub app_id: Option<String>,
    /// The session another of the person's devices continues, when what it
    /// plays here can be handed over.
    pub session_id: Option<String>,
}

impl Player {
    /// What it plays, as the one thing a row says: the title, else the app.
    #[must_use]
    pub fn headline(&self) -> String {
        match self.title.as_deref() {
            Some(title) if !title.is_empty() => title.to_owned(),
            _ => self.identity.clone(),
        }
    }

    /// Who made what it plays, when the player says.
    #[must_use]
    pub fn byline(&self) -> Option<&str> {
        self.artist.as_deref().filter(|artist| !artist.is_empty())
    }
}

impl Media {
    /// The player the `media` resource's own controls reach: the one
    /// playing, else the one paused, else the first.
    #[must_use]
    pub fn front(&self) -> Option<&Player> {
        self.players
            .iter()
            .find(|player| player.playing)
            .or_else(|| self.players.iter().find(|player| player.paused))
            .or_else(|| self.players.first())
    }
}

/// The token of the player on bus name `bus`.
#[must_use]
pub fn player_token(bus: &str) -> String {
    let digest = blake3::hash(bus.as_bytes());
    digest.to_hex().get(..16).unwrap_or_default().to_owned()
}

/// The resource id of the player `token`.
#[must_use]
pub fn player_resource(token: &str) -> String {
    format!("{PLAYER_PREFIX}{token}")
}

/// The token a player's resource id names.
#[must_use]
pub fn token_of(resource: &str) -> Option<&str> {
    resource.strip_prefix(PLAYER_PREFIX)
}

/// A player's controls.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.audio.player")]
pub enum PlayerAction {
    /// Play or pause.
    #[effect(
        own_device,
        row,
        icon = "play",
        keywords = "media, music, play, pause, resume, abspielen, pausieren"
    )]
    PlayPause,
    /// Skip to the next track.
    #[effect(
        own_device,
        row,
        icon = "skip-forward",
        keywords = "media, music, next, skip, nächster titel"
    )]
    Next,
    /// Go back to the previous track.
    #[effect(
        own_device,
        row,
        icon = "skip-back",
        keywords = "media, music, previous, back, vorheriger titel"
    )]
    Previous,
}

impl PlayerAction {
    /// What it asks the player, once the player is the one it names.
    #[must_use]
    pub const fn command(&self) -> PlayerCommand {
        match self {
            Self::PlayPause => PlayerCommand::PlayPause,
            Self::Next => PlayerCommand::Next,
            Self::Previous => PlayerCommand::Previous,
        }
    }
}

/// The controls of the player in front, on the `media` resource itself.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.audio.media")]
pub enum MediaAction {
    /// Play when paused, pause when playing.
    #[effect(
        own_device,
        icon = "play",
        keywords = "music, video, resume, wiedergabe"
    )]
    PlayPause,
    /// Skip to the next track.
    #[effect(own_device, icon = "skip-forward", keywords = "skip")]
    Next,
    /// Go back to the previous track.
    #[effect(own_device, icon = "skip-back", keywords = "back")]
    Previous,
    /// Stop playing.
    #[effect(own_device, icon = "square", keywords = "halt")]
    Stop,
    /// Go to a point in what plays.
    #[effect(
        own_device,
        idempotent,
        icon = "skip-forward",
        keywords = "seek, scrub, jump"
    )]
    Seek {
        /// Where in what plays, from 0 to 1.
        #[param("Where in the track, from 0 to 1")]
        fraction: f64,
    },
    /// Continue here the session another of the person's devices hands
    /// over.
    #[effect(own_device, icon = "play", keywords = "handoff, continue, resume")]
    Resume {
        /// The handoff's request, 32 hex digits.
        #[param("The handoff's request")]
        request: String,
        /// The app that plays it, by package id.
        #[param("The app that plays it")]
        app: String,
        /// The stream it plays from.
        #[param("The stream it plays from")]
        url: Option<String>,
        /// The file it plays.
        #[param("The file it plays")]
        path: Option<String>,
        /// Where it stopped, in milliseconds.
        #[param("Where it stopped, in milliseconds")]
        position_ms: u64,
        /// Whether it plays rather than stands paused.
        #[param("Whether it plays")]
        playing: bool,
    },
}

/// The `media` resource's own controls, as the player in front allows them.
fn front_controls(media: &Media, resource: &mut ResourceSpec) {
    let Some(front) = media.front() else {
        for action in ["play-pause", "next", "previous", "stop", "seek"] {
            unavailable(resource, action, "no_player", "Nothing is playing");
        }
        return;
    };
    if !(front.can_play || front.can_pause) {
        unavailable(resource, "play-pause", "no_play", "The player cannot play");
    }
    if !front.can_next {
        unavailable(resource, "next", "no_next", "The player cannot skip");
    }
    if !front.can_previous {
        unavailable(
            resource,
            "previous",
            "no_previous",
            "The player cannot go back",
        );
    }
    if !front.can_seek || front.duration_ms.is_none() {
        unavailable(
            resource,
            "seek",
            "no_seek",
            "The player cannot go to a point",
        );
    }
}

/// The resource of one player, with the controls it offers now.
fn player_spec(player: &Player) -> ResourceSpec {
    let mut controls = Vec::new();
    let pauses = player.playing && player.can_pause;
    if pauses || (!player.playing && player.can_play) {
        let mut spec = PlayerAction::PlayPause.bound();
        let ty = spec.type_mut();
        // The row says what choosing it does now.
        if pauses { "Pause" } else { "Play" }.clone_into(&mut ty.label);
        ty.icon = Some(if pauses { "pause" } else { "play" }.to_owned());
        controls.push(spec);
    }
    if player.can_next {
        controls.push(PlayerAction::Next.bound());
    }
    if player.can_previous {
        controls.push(PlayerAction::Previous.bound());
    }
    let headline = player.headline();
    for spec in &mut controls {
        spec.type_mut().description = Some(headline.clone());
    }
    let mut resource = ResourceSpec::new(player_resource(&player.token), PLAYER_KIND, headline);
    resource.actions = controls;
    // Who made it is the row's second line, not part of its label.
    if let Some(artist) = player.byline() {
        resource = resource.with_presentation(ResourcePresentation {
            detail: Some(artist.to_owned()),
            ..ResourcePresentation::default()
        });
    }
    resource
}

/// The `media` resource reading `media`.
#[must_use]
pub fn resource(media: &Media) -> ResourceSpec {
    let mut resource = offer::<MediaAction>(ResourceSpec::new(
        MEDIA,
        MEDIA_KIND,
        crate::i18n::name(MEDIA_KIND, "Now playing"),
    ));
    front_controls(media, &mut resource);
    resource.with_children(media.players.iter().map(player_spec))
}

/// What the media service publishes, from its readings as they travel on
/// its pipes.
#[must_use]
pub fn publish(readings: &BTreeMap<String, Value>) -> Vec<ResourceSpec> {
    let media = readings
        .get(MEDIA)
        .and_then(|value| serde_json::from_value::<Media>(value.clone()).ok())
        .unwrap_or_default();
    vec![resource(&media)]
}

#[cfg(feature = "service")]
mod service;
#[cfg(feature = "service")]
pub use service::MediaService;
