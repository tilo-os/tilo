//! The session's audio and media helper, run as its own process
//! (`tilo-audio`) so the compositor drives no audio graph and calls no media
//! player itself.
//!
//! The compositor starts the helper with the session. The helper reads one
//! [`Request`] per line on its standard input and writes one [`Event`] per
//! line on its standard output, each a JSON object: audio devices arriving
//! and leaving (which the compositor sounds under its own quiet rules, as it
//! alone knows whether the screen is being presented), the media players on
//! the session bus as MPRIS describes them, and the answers to the commands
//! the compositor sends those players. The helper stops when its standard
//! input closes, so it never outlives the compositor that started it.
//!
//! Started with `--serve`, it is the session's audio service instead: it
//! serves the [`sound`] resource over JSON lines ([`tilo_service::lines`])
//! on the same pipes, reading the graph again whenever a device arrives or
//! leaves, and the [`media`] resource: the players on the session bus and
//! their controls, read again whenever one signals a change.

pub mod client;
pub mod handoff;
pub mod media;
#[cfg(feature = "service")]
pub mod mpris;
#[cfg(feature = "service")]
pub mod policy;
pub mod sound;
#[cfg(feature = "service")]
pub mod sounds;

use serde::{Deserialize, Serialize};

/// The specs the readings a `tilo-audio --serve` publishes stand for: the
/// session's side of it ([`tilo_service::lines::Process`]).
#[must_use]
pub fn publish(
    readings: &std::collections::BTreeMap<String, serde_json::Value>,
) -> Vec<tilo_actions::ResourceSpec> {
    let mut specs = sound::publish(readings);
    specs.extend(media::publish(readings));
    specs
}

/// The most players the helper describes; a client claiming thousands of
/// MPRIS names must not grow the compositor's copy without bound.
pub const MAX_PLAYERS: usize = 16;

/// An audio device arrived or left: the one thing the helper tells the
/// compositor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceEvent {
    Connected,
    Disconnected,
}

/// A player's transport state as MPRIS reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    Playing,
    Paused,
    Stopped,
}

impl Status {
    #[must_use]
    pub fn from_mpris(status: &str) -> Option<Self> {
        match status {
            "Playing" => Some(Self::Playing),
            "Paused" => Some(Self::Paused),
            "Stopped" => Some(Self::Stopped),
            _ => None,
        }
    }
}

/// One player on the session bus, as last signalled.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each MPRIS capability is an independent property the player reports"
)]
pub struct Player {
    /// The well-known name commands go to.
    pub bus_name: String,
    /// The connection owning it: a new owner is a new player, verified
    /// afresh.
    pub unique_name: String,
    /// The owner's process, for the compositor to attribute to a launch.
    pub pid: Option<u32>,
    /// The package the compositor verified behind `pid` ([`Request::Verified`]).
    pub package_id: Option<String>,
    /// `DesktopEntry`, when the player names one.
    pub desktop_entry: Option<String>,
    /// `Identity`, trimmed; `None` when empty.
    pub identity: Option<String>,
    pub status: Status,
    pub title: Option<String>,
    /// Every artist, joined.
    pub artist: Option<String>,
    /// `xesam:url`: the source playing.
    pub url: Option<String>,
    /// A local (`file://`) cover; its pixels follow as [`Event::Artwork`].
    pub art_url: Option<String>,
    /// `mpris:trackid`, required to seek with `SetPosition`.
    pub track_id: Option<String>,
    pub length_us: Option<i64>,
    /// The playhead when the line was written.
    pub position_us: i64,
    /// How fast the playhead advances while playing.
    pub rate: f64,
    pub can_play: bool,
    pub can_pause: bool,
    pub can_seek: bool,
    pub can_go_next: bool,
    pub can_go_previous: bool,
}

/// A transport key: addresses whichever player the person last listened to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Transport {
    Pause,
    PlayPause,
    Next,
    Previous,
    Stop,
}

impl Transport {
    /// The MPRIS method, by the spec's name.
    #[must_use]
    pub fn method(self) -> &'static str {
        match self {
            Self::Pause => "Pause",
            Self::PlayPause => "PlayPause",
            Self::Next => "Next",
            Self::Previous => "Previous",
            Self::Stop => "Stop",
        }
    }
}

/// So the OSD reports the resulting state, not the button pressed —
/// `PlayPause` is a toggle, and a wrong label would be worse than none.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    NowPlaying,
    NowPaused,
    Skipped,
    Stopped,
}

/// What a button asks one exact player to do.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlayerCommand {
    PlayPause,
    Pause,
    Next,
    Previous,
    SetPosition { track_id: String, position_us: i64 },
}

/// Where a cross-device resume's media comes from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResumeSource {
    Uri(String),
    LocalFile(std::path::PathBuf),
}

/// Ask a newly launched, verified player of `package_id` to assume an exact
/// media state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeRequest {
    pub request_id: [u8; 16],
    pub package_id: String,
    pub source: ResumeSource,
    pub position_ms: u64,
    pub playing: bool,
}

/// What the compositor asks of the helper.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum Request {
    /// A transport key. Answered by [`Event::Transported`] with the same
    /// `id`.
    Transport { id: u64, command: Transport },
    /// A button on a now-playing control, for exactly `bus_name`.
    Control {
        bus_name: String,
        command: PlayerCommand,
    },
    /// Pause exactly `bus_name`; with `if_uri`, only while it still plays
    /// that source.
    Pause {
        bus_name: String,
        if_uri: Option<String>,
    },
    /// Drive a launched player to a handoff's state. Answered by
    /// [`Event::Resumed`].
    Resume { request: ResumeRequest },
    /// Revoke a resume: no player mutation for it follows.
    CancelResume { request_id: [u8; 16] },
    /// The compositor attributed `unique_name`'s process to a launch of
    /// `package_id`.
    Verified {
        bus_name: String,
        unique_name: String,
        package_id: String,
    },
}

/// What the helper tells the compositor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum Event {
    /// An audio device arrived or left.
    Device { connected: bool },
    /// Every player now, in bus order.
    Players { players: Vec<Player> },
    /// The decoded cover a player names at `url`, scaled down: RGBA rows,
    /// base64. `None` when the file is missing, too large or undecodable.
    Artwork {
        url: String,
        image: Option<ArtworkImage>,
    },
    /// The answer to [`Request::Transport`]: what the player did, or `None`
    /// when no player took the command in time.
    Transported {
        id: u64,
        command: Transport,
        outcome: Option<Outcome>,
    },
    /// The answer to [`Request::Resume`]: the player that assumed the state
    /// and the source it plays, or `None` when none did.
    Resumed {
        request_id: [u8; 16],
        player: Option<(String, String)>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtworkImage {
    pub width: u32,
    pub height: u32,
    pub rgba: String,
}

impl Event {
    /// The line the helper writes for this event.
    #[must_use]
    pub fn line(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// The event a line the helper wrote stands for.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        serde_json::from_str(line).ok()
    }
}

impl Request {
    /// The line the compositor writes for this request.
    #[must_use]
    pub fn line(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// The request a line the compositor wrote stands for.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        serde_json::from_str(line).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_and_requests_read_back_from_their_lines() {
        for event in [
            Event::Device { connected: true },
            Event::Transported {
                id: 7,
                command: Transport::PlayPause,
                outcome: Some(Outcome::NowPaused),
            },
            Event::Resumed {
                request_id: [3; 16],
                player: Some((
                    "org.mpris.MediaPlayer2.a".into(),
                    "https://x.example/a".into(),
                )),
            },
        ] {
            assert!(!event.line().contains('\n'));
            assert_eq!(Event::parse(&event.line()), Some(event));
        }
        let request = Request::Control {
            bus_name: "org.mpris.MediaPlayer2.a".into(),
            command: PlayerCommand::SetPosition {
                track_id: "/t/1".into(),
                position_us: 5,
            },
        };
        assert_eq!(Request::parse(&request.line()), Some(request));
        assert_eq!(Event::parse("volume"), None);
    }

    #[test]
    fn transport_commands_map_to_the_spec_method_names() {
        // Wire names in the MPRIS spec, not ours to rename.
        assert_eq!(Transport::PlayPause.method(), "PlayPause");
        assert_eq!(Transport::Pause.method(), "Pause");
        assert_eq!(Transport::Next.method(), "Next");
        assert_eq!(Transport::Previous.method(), "Previous");
        assert_eq!(Transport::Stop.method(), "Stop");
    }
}
