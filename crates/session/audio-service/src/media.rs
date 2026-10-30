//! What plays: the media players on the session bus (MPRIS), published as
//! the `media` resource with one child per player and the actions its
//! controls offer (play or pause, next, previous). Quick settings, the
//! desktop's search, a media key, a script and an agent all run the same
//! actions by address.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tilo_actions::ResourceSpec;
use tilo_resource::Effect;

/// The resource the players are published under.
pub const MEDIA: &str = "media";
pub const MEDIA_KIND: &str = "so.tilo.audio.media";
/// The kind of one player's resource.
pub const PLAYER_KIND: &str = "so.tilo.audio.player";
/// What a player's resource id starts with, before its token.
const PLAYER_PREFIX: &str = "media-player:";

/// What plays now.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Media {
    pub players: Vec<Player>,
    /// Why the last control did not reach its player, until the next one.
    pub problem: Option<String>,
}

/// One media player.
#[expect(
    clippy::struct_excessive_bools,
    reason = "what a player can do now, as MPRIS says it: independent flags"
)]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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
    pub can_play: bool,
    pub can_pause: bool,
    pub can_next: bool,
    pub can_previous: bool,
}

impl Player {
    /// What it plays, as one line: the title and who made it, else the app.
    #[must_use]
    pub fn headline(&self) -> String {
        match (&self.title, &self.artist) {
            (Some(title), Some(artist)) if !artist.is_empty() => format!("{title} · {artist}"),
            (Some(title), _) if !title.is_empty() => title.clone(),
            _ => self.identity.clone(),
        }
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
        label = "Play or pause",
        icon = "play",
        keywords = "media, music, play, pause, resume, abspielen, pausieren"
    )]
    PlayPause,
    /// Skip to the next track.
    #[effect(
        own_device,
        row,
        label = "Next track",
        icon = "skip-forward",
        keywords = "media, music, next, skip, nächster titel"
    )]
    Next,
    /// Go back to the previous track.
    #[effect(
        own_device,
        row,
        label = "Previous track",
        icon = "skip-back",
        keywords = "media, music, previous, back, vorheriger titel"
    )]
    Previous,
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
    resource
}

/// The `media` resource reading `media`.
#[must_use]
pub fn resource(media: &Media) -> ResourceSpec {
    ResourceSpec::new(MEDIA, MEDIA_KIND, "Now playing")
        .with_children(media.players.iter().map(player_spec))
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
pub use service::{MediaService, Mpris, Players};
