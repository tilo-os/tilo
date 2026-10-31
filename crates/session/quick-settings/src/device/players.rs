//! What plays, through the session's audio helper (`tilo-audio`): its
//! players folded into the now-playing card's reading, the card's effects
//! sent to exactly the player they name, and what another of the person's
//! devices hands over continued here.
//!
//! The transport is whatever carries the helper's lines: its pipes when
//! the session starts it ([`PlayerLink::helper`]), or a test's. Lines are
//! read and written on threads of their own, so a helper that stalls never
//! holds up the session.

use std::io::{BufRead, Write};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Instant;

use audio::client::{Artwork, PlayerTable};
use audio::{Event, Request, ResumeRequest, ResumeSource, Status};
use media::handoff::HandoffSource;
use ui::widget::TransportArt;

use super::refresh::Wake;
use crate::{MediaOffer, MediaOutput, MediaPlayer, NowPlaying, PlayingSource, QsEffect};

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
    /// The session another of the person's devices hands this one, until
    /// the person continues it here.
    offer: Option<MediaOffer>,
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
            offer: None,
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

    /// What the player in front plays, when another of the person's devices
    /// can continue it: its session, app, title, stream and playhead.
    ///
    /// Only a stream the other device can open as it is counts: a source
    /// this device opened off a workspace or an app's own resource has to be
    /// brought over first, which is not a hand-over.
    pub(crate) fn playing_source(&self, now: Instant) -> Option<PlayingSource> {
        let player = self.table.front(self.preferred.as_deref())?;
        let url = player.url.as_deref()?;
        audio::handoff::resume_uri(&ResumeSource::Uri(url.to_owned()))?;
        let app_id = player.package_id.clone()?;
        let title = player
            .title
            .clone()
            .filter(|title| !title.trim().is_empty())
            .or_else(|| player.identity.clone())
            .unwrap_or_else(|| player.name());
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a playhead in a recording is far inside u64's milliseconds"
        )]
        let position_ms = (self.table.position_at(player, now).max(0) / 1_000) as u64;
        Some(PlayingSource {
            session_id: audio::handoff::session_id(&app_id, player, url),
            app_id,
            title,
            url: url.to_owned(),
            position_ms,
            playing: self.table.status(player) == Status::Playing,
        })
    }

    /// Where the sound can go, as the audio service reads it: the card
    /// offers these as its destinations.
    pub(crate) fn set_outputs(&mut self, outputs: Vec<MediaOutput>) {
        self.outputs = outputs;
    }

    /// The session another of the person's devices hands this one, which
    /// the card offers to continue here; `None` when there is none.
    pub(crate) fn set_offer(&mut self, offer: Option<MediaOffer>) {
        self.offer = offer;
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
            self.offer.clone(),
            now,
        )
    }

    /// Carry out one of the card's effects.
    pub(crate) fn run(&mut self, effect: &QsEffect) -> Result<(), String> {
        let nothing = || "nothing plays".to_owned();
        if let QsEffect::ResumePlayback { request } = effect {
            let resume = self
                .offer
                .as_ref()
                .filter(|offer| offer.request_id == *request)
                .ok_or_else(|| "no such handoff".to_owned())
                .and_then(resume_request)?;
            self.requests
                .send(resume)
                .map_err(|_| "the audio helper is gone".to_owned())?;
            // A resume is one request: the helper answers it with the
            // player that took the session, so the card stops offering it.
            self.offer = None;
            return Ok(());
        }
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
/// paused, and the other device's session `offer` while one is handed over;
/// `None` otherwise. `image` is the front player's cover, and `outputs`
/// where the sound can go.
pub(crate) fn now_playing(
    table: &PlayerTable,
    preferred: Option<&str>,
    image: Option<ui::vello::peniko::ImageData>,
    outputs: Vec<MediaOutput>,
    offer: Option<MediaOffer>,
    now: Instant,
) -> Option<NowPlaying> {
    if !table.any_playing() {
        return offer.map(|offer| NowPlaying {
            title: offer.title.clone(),
            playing: offer.playing,
            position_ms: offer.position_ms,
            outputs,
            offer: Some(offer),
            ..NowPlaying::default()
        });
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
        offer: None,
    })
}

/// What the person is told when this device cannot open what the other
/// device played.
const CANNOT_RESUME: &str = "this device cannot resume that source";

/// The helper request that continues `offer` here, or why this device
/// cannot resume it.
fn resume_request(offer: &MediaOffer) -> Result<Request, String> {
    // The card passes the request id as hex, since an effect's parameters
    // are plain scalars.
    let Some(request_id) = request_bytes(&offer.request_id) else {
        return Err("the handoff names no request".to_owned());
    };
    let source = match &offer.source {
        // What the other device played off a stream or a file this device
        // can open itself; its work in a workspace path or an app's own
        // resource has to be brought over first, which is not this card's.
        HandoffSource::RemoteStream { url } => ResumeSource::Uri(url.clone()),
        HandoffSource::LocalFile { path } => ResumeSource::LocalFile(path.clone()),
        HandoffSource::Workspace { .. } | HandoffSource::AppResource { .. } => {
            return Err(CANNOT_RESUME.to_owned());
        }
    };
    if audio::handoff::resume_uri(&source).is_none() {
        return Err(CANNOT_RESUME.to_owned());
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a playhead in a recording is far inside u64's milliseconds"
    )]
    let position_ms = offer.position_ms.max(0.0).round() as u64;
    Ok(Request::Resume {
        request: ResumeRequest {
            request_id,
            package_id: offer.package_id.clone(),
            source,
            position_ms,
            playing: offer.playing,
        },
    })
}

/// The 16 bytes `hex` names with its 32 hex digits.
fn request_bytes(hex: &str) -> Option<[u8; 16]> {
    let mut bytes = [0u8; 16];
    let digits = hex.as_bytes();
    if digits.len() != 32 {
        return None;
    }
    for (byte, pair) in bytes.iter_mut().zip(digits.chunks_exact(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(bytes)
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
        assert_eq!(now_playing(&table, None, None, Vec::new(), None, now), None);
        table.apply(
            Event::Players {
                players: vec![
                    player("browser", "Browser", Status::Paused),
                    player("music", "Music", Status::Playing),
                ],
            },
            now,
        );
        let card = now_playing(&table, None, None, Vec::new(), None, now).expect("something plays");
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
            None,
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
            None,
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
            now_playing(&table, None, None, Vec::new(), None, now),
            None,
            "stopped"
        );
    }

    /// A card that only another device's session makes: what it played,
    /// where it stopped, and the offer to continue it here.
    #[test]
    fn a_handoff_alone_is_the_card_that_offers_it() {
        let now = Instant::now();
        let table = PlayerTable::new();
        let offered = offer(HandoffSource::RemoteStream {
            url: "https://media.example/clair.flac".into(),
        });
        let card = now_playing(&table, None, None, Vec::new(), Some(offered.clone()), now)
            .expect("the offer is what the card shows");
        assert_eq!(card.title, "Clair de lune");
        assert!(card.playing);
        assert_eq!(card.position_ms, 83_000.0);
        assert_eq!(card.offer, Some(offered));
        assert!(
            !card.can_toggle && !card.can_next && !card.can_previous,
            "nothing here plays it yet"
        );
    }

    /// Continuing a handoff is one request to the helper, naming the
    /// session's request, app, source and playhead; once asked, the offer
    /// is over.
    #[test]
    fn continuing_a_handoff_asks_the_helper_for_that_session() {
        let (mut players, heard) = over_a_helper();
        players.set_offer(Some(offer(HandoffSource::RemoteStream {
            url: "https://media.example/clair.flac".into(),
        })));
        players
            .run(&QsEffect::ResumePlayback {
                request: REQUEST.into(),
            })
            .expect("the offer is asked for");
        let line = tilo_test_env::recv("the resume", &heard);
        assert_eq!(
            Request::parse(&line),
            Some(Request::Resume {
                request: ResumeRequest {
                    request_id: request_bytes(REQUEST).expect("the offer's request"),
                    package_id: "so.tilo.music".into(),
                    source: ResumeSource::Uri("https://media.example/clair.flac".into()),
                    position_ms: 83_000,
                    playing: true,
                },
            })
        );
        assert!(heard.try_recv().is_err(), "one request, not two");
        assert_eq!(
            players.run(&QsEffect::ResumePlayback {
                request: REQUEST.into(),
            }),
            Err("no such handoff".to_owned()),
            "continued, the offer is over"
        );

        let (mut players, heard) = over_a_helper();
        players.set_offer(Some(offer(HandoffSource::LocalFile {
            path: "/home/alice/Clair de lune.flac".into(),
        })));
        players
            .run(&QsEffect::ResumePlayback {
                request: REQUEST.into(),
            })
            .expect("a file this device has is asked for");
        let line = tilo_test_env::recv("the resume", &heard);
        assert!(
            matches!(
                Request::parse(&line),
                Some(Request::Resume { request }) if request.source
                    == ResumeSource::LocalFile("/home/alice/Clair de lune.flac".into())
            ),
            "a file stays a file"
        );
    }

    /// A press that names no handoff this device holds — the card went
    /// stale — is refused and writes nothing.
    #[test]
    fn a_stale_handoff_is_refused_and_nothing_is_written() {
        let (mut players, heard) = over_a_helper();
        assert_eq!(
            players.run(&QsEffect::ResumePlayback {
                request: REQUEST.into(),
            }),
            Err("no such handoff".to_owned()),
            "no handoff at all"
        );
        players.set_offer(Some(offer(HandoffSource::RemoteStream {
            url: "https://media.example/clair.flac".into(),
        })));
        assert_eq!(
            players.run(&QsEffect::ResumePlayback {
                request: "ffeeddccbbaa99887766554433221100".into(),
            }),
            Err("no such handoff".to_owned()),
            "another handoff's press"
        );
        assert!(heard.try_recv().is_err(), "nothing was asked");
        players
            .run(&QsEffect::ResumePlayback {
                request: REQUEST.into(),
            })
            .expect("the offer itself still stands");
    }

    /// What another app keeps as its own resource is not something this
    /// device can open, and the card says so instead of asking the helper.
    #[test]
    fn a_source_this_device_cannot_open_is_refused() {
        let (mut players, heard) = over_a_helper();
        players.set_offer(Some(offer(HandoffSource::AppResource {
            uri: "tilo://music/track/1".into(),
        })));
        assert_eq!(
            players.run(&QsEffect::ResumePlayback {
                request: REQUEST.into(),
            }),
            Err(CANNOT_RESUME.to_owned())
        );
        assert!(heard.try_recv().is_err(), "the helper is not asked");
    }

    /// The request id every test's offer carries.
    const REQUEST: &str = "00112233445566778899aabbccddeeff";

    /// A session another device hands over: `source` playing at 1:23.
    fn offer(source: HandoffSource) -> MediaOffer {
        MediaOffer {
            request_id: REQUEST.into(),
            package_id: "so.tilo.music".into(),
            title: "Clair de lune".into(),
            source_label: "Studio".into(),
            source,
            position_ms: 83_000.0,
            playing: true,
        }
    }

    /// The players over a helper that reads its lines here.
    fn over_a_helper() -> (Players, Receiver<String>) {
        let (sent, heard) = channel();
        let link = PlayerLink::new(
            Lines(sent, Vec::new()),
            std::io::BufReader::new(std::io::empty()),
        );
        let wake: Wake = std::sync::Arc::new(|| {});
        (Players::start(link, wake), heard)
    }

    /// Requests, a line at a time.
    struct Lines(Sender<String>, Vec<u8>);

    impl Write for Lines {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.1.extend_from_slice(bytes);
            while let Some(end) = self.1.iter().position(|byte| *byte == b'\n') {
                let line: Vec<u8> = self.1.drain(..=end).collect();
                let _ = self
                    .0
                    .send(String::from_utf8_lossy(&line).trim_end().to_owned());
            }
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
}
