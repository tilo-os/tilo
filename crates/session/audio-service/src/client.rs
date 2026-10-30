//! The other end of the helper's lines: the players it last described, as a
//! client keeps them, and the requests that steer them.
//!
//! A client folds every [`Event`] the helper writes into a [`PlayerTable`]
//! and asks it for the [`Request`] a button stands for. Nothing here talks
//! to the helper or waits on it: the client owns the transport (the
//! helper's pipes, or a test's lines) and hands events in as they arrive.

use std::collections::HashMap;
use std::io::BufReader;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::{ArtworkImage, Event, Player, PlayerCommand, Request, Status};

/// The prefix of every MPRIS player's bus name.
pub const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";

/// Whether `bus_name` can name a player: a command never goes anywhere else.
#[must_use]
pub fn is_player_name(bus_name: &str) -> bool {
    bus_name.starts_with(MPRIS_PREFIX) && bus_name.len() <= 256
}

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

    /// The playhead `elapsed` after the player was described: advanced at
    /// its rate while playing, bounded by the track's length.
    #[must_use]
    pub fn position_after(&self, elapsed: Duration) -> i64 {
        let base = self.position_us.max(0);
        let position = if self.status == Status::Playing {
            let rate = if self.rate.is_finite() && self.rate > 0.0 {
                self.rate
            } else {
                1.0
            };
            let advanced =
                Duration::try_from_secs_f64(elapsed.as_secs_f64() * rate).unwrap_or(elapsed);
            base.saturating_add(i64::try_from(advanced.as_micros()).unwrap_or(i64::MAX))
        } else {
            base
        };
        self.length_us
            .filter(|length| *length > 0)
            .map_or(position, |length| position.min(length))
    }
}

/// The players the helper last described, with what the client showed
/// since: a play or pause pressed shows at once, until the helper describes
/// the players again.
#[derive(Debug, Default)]
pub struct PlayerTable {
    players: Vec<Player>,
    /// When the players were last described.
    received: Option<Instant>,
    /// Play or pause pressed since, by bus name.
    toggled: HashMap<String, Status>,
    /// Decoded covers by `mpris:artUrl`.
    artwork: HashMap<String, Option<Artwork>>,
}

impl PlayerTable {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one event of the helper's in, received at `now`. Whether what a
    /// client shows of the players changed.
    pub fn apply(&mut self, event: Event, now: Instant) -> bool {
        match event {
            Event::Players { players } => {
                self.players = players;
                self.players.truncate(crate::MAX_PLAYERS);
                self.received = Some(now);
                self.toggled.clear();
                if self.artwork.len() > crate::MAX_PLAYERS {
                    let wanted: Vec<&String> = self
                        .players
                        .iter()
                        .filter_map(|player| player.art_url.as_ref())
                        .collect();
                    self.artwork.retain(|url, _| wanted.contains(&url));
                }
                true
            }
            Event::Artwork { url, image } => {
                self.artwork.insert(url, image.and_then(decode_artwork));
                true
            }
            Event::Device { .. } | Event::Transported { .. } | Event::Resumed { .. } => false,
        }
    }

    /// The helper stopped: nothing plays as far as the client knows.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Every player, in bus order.
    #[must_use]
    pub fn players(&self) -> &[Player] {
        &self.players
    }

    /// `player`'s state as shown: what was pressed since it was described,
    /// else what it said.
    #[must_use]
    pub fn status(&self, player: &Player) -> Status {
        self.toggled
            .get(&player.bus_name)
            .copied()
            .unwrap_or(player.status)
    }

    /// Whether the play/pause button does anything for `player` now.
    #[must_use]
    pub fn can_toggle(&self, player: &Player) -> bool {
        if self.status(player) == Status::Playing {
            player.can_pause
        } else {
            player.can_play
        }
    }

    /// `player`'s cover, once decoded.
    #[must_use]
    pub fn artwork(&self, player: &Player) -> Option<&Artwork> {
        player
            .art_url
            .as_ref()
            .and_then(|url| self.artwork.get(url))
            .and_then(Option::as_ref)
    }

    /// `player`'s playhead at `now`, in microseconds.
    #[must_use]
    pub fn position_at(&self, player: &Player, now: Instant) -> i64 {
        let elapsed = self
            .received
            .map(|at| now.saturating_duration_since(at))
            .unwrap_or_default();
        if self.status(player) == Status::Playing {
            Player {
                status: Status::Playing,
                ..player.clone()
            }
            .position_after(elapsed)
        } else {
            player.position_us.max(0)
        }
    }

    /// Whether anything plays or is paused: what a now-playing card is
    /// shown for.
    #[must_use]
    pub fn any_playing(&self) -> bool {
        self.players
            .iter()
            .any(|player| matches!(self.status(player), Status::Playing | Status::Paused))
    }

    /// The player a card steers: the one the person picked while it is
    /// still around, else whatever plays, else whatever is paused, else the
    /// first.
    #[must_use]
    pub fn front(&self, preferred: Option<&str>) -> Option<&Player> {
        let with = |status: Status| {
            self.players
                .iter()
                .find(|player| self.status(player) == status)
        };
        preferred
            .and_then(|bus| self.players.iter().find(|player| player.bus_name == bus))
            .or_else(|| with(Status::Playing))
            .or_else(|| with(Status::Paused))
            .or_else(|| self.players.first())
    }

    /// The request that sends `command` to exactly the player at
    /// `bus_name`, never to whichever other player is active; `None` for a
    /// name no player can have. A play or pause shows at once.
    pub fn control(&mut self, bus_name: &str, command: PlayerCommand) -> Option<Request> {
        if !is_player_name(bus_name) {
            return None;
        }
        let shown = self
            .players
            .iter()
            .find(|player| player.bus_name == bus_name)
            .map(|player| self.status(player));
        if let Some(status) = shown
            && matches!(command, PlayerCommand::PlayPause | PlayerCommand::Pause)
        {
            let toggled = if status == Status::Playing {
                Status::Paused
            } else if command == PlayerCommand::Pause {
                status
            } else {
                Status::Playing
            };
            self.toggled.insert(bus_name.to_string(), toggled);
        }
        Some(Request::Control {
            bus_name: bus_name.to_string(),
            command,
        })
    }
}

/// The helper as a client started it: its process, the pipe requests go
/// down and the lines its events come up on. Dropping `requests` closes
/// the helper's standard input, which stops it.
pub struct Helper {
    pub child: Child,
    pub requests: ChildStdin,
    pub events: BufReader<ChildStdout>,
}

/// Where the helper is installed: beside the running program, else where
/// the image puts it.
#[must_use]
pub fn helper_path() -> Option<PathBuf> {
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("tilo-audio")));
    beside
        .into_iter()
        .chain([PathBuf::from("/usr/libexec/tilo-audio")])
        .find(|path| path.exists())
}

/// Start the installed helper.
///
/// # Errors
/// It is not installed here, or did not start.
pub fn start_helper() -> std::io::Result<Helper> {
    let path = helper_path().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "the audio helper is not installed",
        )
    })?;
    let mut child = Command::new(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let (Some(requests), Some(events)) = (child.stdin.take(), child.stdout.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(std::io::Error::other("the audio helper has no pipes"));
    };
    Ok(Helper {
        child,
        requests,
        events: BufReader::new(events),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn table(players: Vec<Player>, now: Instant) -> PlayerTable {
        let mut table = PlayerTable::new();
        assert!(table.apply(Event::Players { players }, now));
        table
    }

    #[test]
    fn the_picked_player_wins_then_playing_then_paused() {
        let now = Instant::now();
        let players = table(
            vec![
                player("a", Status::Stopped),
                player("b", Status::Paused),
                player("c", Status::Playing),
            ],
            now,
        );
        let front = |preferred: Option<&str>| {
            players
                .front(preferred)
                .map(|player| player.bus_name.clone())
        };
        assert_eq!(front(None).as_deref(), Some("org.mpris.MediaPlayer2.c"));
        assert_eq!(
            front(Some("org.mpris.MediaPlayer2.a")).as_deref(),
            Some("org.mpris.MediaPlayer2.a")
        );
        assert_eq!(
            front(Some("org.mpris.MediaPlayer2.gone")).as_deref(),
            Some("org.mpris.MediaPlayer2.c")
        );
        let paused = table(
            vec![player("a", Status::Stopped), player("b", Status::Paused)],
            now,
        );
        assert_eq!(
            paused.front(None).map(|p| p.bus_name.as_str()),
            Some("org.mpris.MediaPlayer2.b")
        );
        let stopped = table(vec![player("a", Status::Stopped)], now);
        assert!(stopped.front(None).is_some());
        assert!(!stopped.any_playing(), "a stopped player is no card");
        assert!(PlayerTable::new().front(None).is_none());
    }

    #[test]
    fn playheads_advance_at_the_players_rate_up_to_the_end() {
        let playing = player("a", Status::Playing);
        let second = Duration::from_secs(1);
        assert_eq!(playing.position_after(second), 6_000_000);
        assert_eq!(playing.position_after(second * 60), 10_000_000);
        let paused = player("a", Status::Paused);
        assert_eq!(paused.position_after(second * 60), 4_000_000);
        let now = Instant::now();
        let shown = table(vec![playing.clone()], now);
        assert_eq!(shown.position_at(&playing, now + second), 6_000_000);
    }

    #[test]
    fn a_press_shows_at_once_until_the_helper_says_otherwise() {
        let now = Instant::now();
        let mut players = table(vec![player("a", Status::Playing)], now);
        let request = players
            .control("org.mpris.MediaPlayer2.a", PlayerCommand::PlayPause)
            .expect("a player's name");
        assert_eq!(
            request,
            Request::Control {
                bus_name: "org.mpris.MediaPlayer2.a".into(),
                command: PlayerCommand::PlayPause,
            }
        );
        let shown = players.players()[0].clone();
        assert_eq!(players.status(&shown), Status::Paused, "shown paused");
        players.apply(
            Event::Players {
                players: vec![player("a", Status::Playing)],
            },
            now,
        );
        assert_eq!(
            players.status(&players.players()[0]),
            Status::Playing,
            "the helper's word wins"
        );
        assert_eq!(
            players.control("org.freedesktop.DBus", PlayerCommand::Pause),
            None,
            "only a player's name is ever addressed"
        );
        assert_eq!(
            players.control(
                &format!("{MPRIS_PREFIX}{}", "x".repeat(300)),
                PlayerCommand::Pause
            ),
            None
        );
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
        let mut players = table(vec![music.clone()], Instant::now());
        assert!(players.artwork(&music).is_none());
        players.apply(
            Event::Artwork {
                url: "file:///cover.png".into(),
                image: Some(ArtworkImage {
                    width: 2,
                    height: 1,
                    rgba: encode(&[1; 8]),
                }),
            },
            Instant::now(),
        );
        let cover = players.artwork(&music).expect("a 2×1 cover");
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
