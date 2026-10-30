//! What plays, through the session's audio helper (`tilo-audio`): its
//! players folded into the now-playing card's reading, and the card's
//! effects sent to exactly the player they name.
//!
//! The transport is whatever carries the helper's lines: its pipes when
//! the session starts it ([`PlayerLink::helper`]), or a test's. Lines are
//! read and written on threads of their own, so a helper that stalls never
//! holds up the session.

use std::io::{BufRead, Write};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Instant;

use audio::client::{Artwork, PlayerTable};
use audio::{Event, Request, Status};
use ui::widget::TransportArt;

use super::refresh::Wake;
use crate::{MediaOutput, MediaPlayer, NowPlaying, QsEffect};

/// Where the helper's lines come and go.
pub struct PlayerLink {
    requests: Box<dyn Write + Send>,
    events: Box<dyn BufRead + Send>,
    child: Option<std::process::Child>,
}

impl PlayerLink {
    /// Requests written to `requests`, events read from `events`, a JSON
    /// object per line each.
    pub fn new(
        requests: impl Write + Send + 'static,
        events: impl BufRead + Send + 'static,
    ) -> Self {
        Self {
            requests: Box::new(requests),
            events: Box::new(events),
            child: None,
        }
    }

    /// The installed helper, started now.
    ///
    /// # Errors
    /// It is not installed here, or did not start.
    pub fn helper() -> std::io::Result<Self> {
        let helper = audio::client::start_helper()?;
        Ok(Self {
            requests: Box::new(helper.requests),
            events: Box::new(helper.events),
            child: Some(helper.child),
        })
    }
}

/// What the reader hands over.
enum Line {
    Event(Event),
    /// The helper is gone.
    Closed,
}

/// The players as the session follows them.
pub(crate) struct Players {
    table: PlayerTable,
    /// The player the person chose to steer, by bus name.
    preferred: Option<String>,
    requests: Sender<Request>,
    lines: Receiver<Line>,
    child: Option<std::process::Child>,
    /// The last cover made into a picture, so an unchanged cover is the
    /// same picture and the card is not built again for it.
    art: Option<(Artwork, ui::vello::peniko::ImageData)>,
    /// Where the sound can go, as the audio service last read it.
    outputs: Vec<MediaOutput>,
}

impl Players {
    pub(crate) fn start(transport: PlayerLink, wake: Wake) -> Self {
        let PlayerLink {
            mut requests,
            events,
            child,
        } = transport;
        let (sent, heard) = channel();
        let read = std::thread::Builder::new()
            .name("quick-settings-players".into())
            .spawn(move || {
                for line in events.lines() {
                    let Ok(line) = line else { break };
                    if let Some(event) = Event::parse(&line) {
                        if sent.send(Line::Event(event)).is_err() {
                            return;
                        }
                        wake();
                    }
                }
                let _ = sent.send(Line::Closed);
                wake();
            });
        if let Err(error) = read {
            tracing::warn!(%error, "what plays goes unread");
        }
        // Dropping the last sender ends the writer, which closes the
        // helper's standard input: its signal to stop.
        let (to_helper, pending) = channel::<Request>();
        let write = std::thread::Builder::new()
            .name("quick-settings-player-requests".into())
            .spawn(move || {
                for request in pending {
                    if writeln!(requests, "{}", request.line())
                        .and_then(|()| requests.flush())
                        .is_err()
                    {
                        break;
                    }
                }
            });
        if let Err(error) = write {
            tracing::warn!(%error, "players cannot be steered");
        }
        Self {
            table: PlayerTable::new(),
            preferred: None,
            requests: to_helper,
            lines: heard,
            child,
            art: None,
            outputs: Vec::new(),
        }
    }

    /// Fold in what the helper said since the last call. The audio
    /// devices that arrived (`true`) or left meanwhile.
    pub(crate) fn drain(&mut self, now: Instant) -> Vec<bool> {
        let mut devices = Vec::new();
        while let Ok(line) = self.lines.try_recv() {
            match line {
                Line::Event(Event::Device { connected }) => devices.push(connected),
                Line::Event(event) => {
                    self.table.apply(event, now);
                }
                Line::Closed => self.table.clear(),
            }
        }
        devices
    }

    /// Whether something plays now: the playhead moves.
    pub(crate) fn playing(&self) -> bool {
        self.table
            .front(self.preferred.as_deref())
            .is_some_and(|player| self.table.status(player) == Status::Playing)
    }

    /// Where the sound can go, as the audio service reads it: the card
    /// offers these as its destinations.
    pub(crate) fn set_outputs(&mut self, outputs: Vec<MediaOutput>) {
        self.outputs = outputs;
    }

    /// What the card shows at `now`.
    pub(crate) fn now_playing(&mut self, now: Instant) -> Option<NowPlaying> {
        let art = self
            .table
            .front(self.preferred.as_deref())
            .and_then(|player| self.table.artwork(player))
            .cloned();
        let image = art.map(|art| match &self.art {
            Some((made, image)) if *made == art => image.clone(),
            _ => {
                let image = ui::raster::rgba8(
                    ui::vello::peniko::Blob::new(art.rgba.clone()),
                    art.width,
                    art.height,
                );
                self.art = Some((art, image.clone()));
                image
            }
        });
        now_playing(
            &self.table,
            self.preferred.as_deref(),
            image,
            self.outputs.clone(),
            now,
        )
    }

    /// Carry out one of the card's effects.
    pub(crate) fn run(&mut self, effect: &QsEffect) -> Result<(), String> {
        let nothing = || "nothing plays".to_owned();
        if let QsEffect::ChoosePlayer { player } = effect {
            if !self
                .table
                .players()
                .iter()
                .any(|known| known.bus_name == *player)
            {
                return Err(format!("no player {player}"));
            }
            self.preferred = Some(player.clone());
            return Ok(());
        }
        let front = self
            .table
            .front(self.preferred.as_deref())
            .ok_or_else(nothing)?;
        let bus_name = front.bus_name.clone();
        let command = match effect {
            QsEffect::PlayPause => audio::PlayerCommand::PlayPause,
            QsEffect::PreviousTrack => audio::PlayerCommand::Previous,
            QsEffect::NextTrack => audio::PlayerCommand::Next,
            QsEffect::Seek { fraction } => front
                .seek(*fraction)
                .ok_or_else(|| "this player cannot seek".to_owned())?,
            _ => return Ok(()),
        };
        let request = self.table.control(&bus_name, command).ok_or_else(nothing)?;
        self.requests
            .send(request)
            .map_err(|_| "the audio helper is gone".to_owned())
    }
}

impl Drop for Players {
    fn drop(&mut self) {
        // Closing its standard input stops the helper; reap it.
        let (closed, _) = channel();
        self.requests = closed;
        if let Some(mut child) = self.child.take() {
            let _ = child.wait();
        }
    }
}

/// The now-playing card's reading: the player in front (the one chosen,
/// else what plays, else what is paused) while anything plays or is
/// paused; `None` otherwise. `image` is the front player's cover, and
/// `outputs` where the sound can go.
pub(crate) fn now_playing(
    table: &PlayerTable,
    preferred: Option<&str>,
    image: Option<ui::vello::peniko::ImageData>,
    outputs: Vec<MediaOutput>,
    now: Instant,
) -> Option<NowPlaying> {
    if !table.any_playing() {
        return None;
    }
    let front = table.front(preferred)?;
    let name = front.name();
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
        reason = "a playhead in milliseconds needs no more than f64's 52-bit mantissa"
    )]
    let ms = |us: i64| us as f64 / 1000.0;
    let duration_ms = if front.seekable() {
        front.length_us.map_or(0.0, ms)
    } else {
        0.0
    };
    Some(NowPlaying {
        title,
        subtitle,
        player_name: name,
        art: image.map_or(TransportArt::Placeholder, TransportArt::Image),
        playing: table.status(front) == Status::Playing,
        can_toggle: table.can_toggle(front),
        can_previous: front.can_go_previous,
        can_next: front.can_go_next,
        position_ms: ms(table.position_at(front, now)),
        duration_ms,
        players: table
            .players()
            .iter()
            .map(|player| MediaPlayer {
                id: player.bus_name.clone(),
                name: player.name(),
                active: player.bus_name == front.bus_name,
            })
            .collect(),
        outputs,
        speakers: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use audio::Player;

    fn player(bus: &str, identity: &str, status: Status) -> Player {
        Player {
            bus_name: format!("org.mpris.MediaPlayer2.{bus}"),
            unique_name: ":1.5".into(),
            pid: None,
            package_id: None,
            desktop_entry: None,
            identity: Some(identity.into()),
            status,
            title: Some(format!("{identity} track")),
            artist: Some("An Artist".into()),
            url: None,
            art_url: None,
            track_id: Some("/t/1".into()),
            length_us: Some(200_000_000),
            position_us: 50_000_000,
            rate: 1.0,
            can_play: true,
            can_pause: true,
            can_seek: true,
            can_go_next: true,
            can_go_previous: false,
        }
    }

    #[test]
    fn the_card_shows_the_player_in_front_while_something_plays() {
        let now = Instant::now();
        let mut table = PlayerTable::new();
        assert_eq!(now_playing(&table, None, None, Vec::new(), now), None);
        table.apply(
            Event::Players {
                players: vec![
                    player("browser", "Browser", Status::Paused),
                    player("music", "Music", Status::Playing),
                ],
            },
            now,
        );
        let card = now_playing(&table, None, None, Vec::new(), now).expect("something plays");
        assert_eq!(card.title, "Music track");
        assert_eq!(card.subtitle, "An Artist");
        assert_eq!(card.player_name, "Music");
        assert!(card.playing && card.can_toggle && card.can_next && !card.can_previous);
        assert_eq!(card.position_ms, 50_000.0);
        assert_eq!(card.duration_ms, 200_000.0);
        assert_eq!(
            card.players
                .iter()
                .map(|p| (p.name.as_str(), p.active))
                .collect::<Vec<_>>(),
            [("Browser", false), ("Music", true)]
        );
        let chosen = now_playing(
            &table,
            Some("org.mpris.MediaPlayer2.browser"),
            None,
            Vec::new(),
            now,
        )
        .expect("the chosen one");
        assert_eq!(chosen.player_name, "Browser");
        assert!(!chosen.playing);
        let later = now_playing(
            &table,
            None,
            None,
            Vec::new(),
            now + std::time::Duration::from_secs(2),
        )
        .expect("still playing");
        assert_eq!(later.position_ms, 52_000.0, "the playhead moves");

        table.apply(
            Event::Players {
                players: vec![player("music", "Music", Status::Stopped)],
            },
            now,
        );
        assert_eq!(
            now_playing(&table, None, None, Vec::new(), now),
            None,
            "stopped"
        );
    }
}
