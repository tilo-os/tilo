//! The media service: what this process's MPRIS client sees of the
//! session bus, published as the `media` resource, and the controls that
//! steer it taken as that resource's actions.
//!
//! The client ([`Mpris`]) is the session's one MPRIS observer: it follows
//! the players' signals, keeps what each reports, drives them and resumes
//! a handoff. The service folds the players it publishes into the reading
//! and hands its actions back to it, so a reading and a press are two views
//! of one client, never a second connection with a mind of its own.

use std::collections::HashMap;
use std::sync::mpsc::Receiver;

use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, DoorError, ResourceSpec};
use tilo_service::{Service, Started, Wake, decode, reading};

use crate::mpris::Mpris;
use crate::{
    ArtworkImage, Event, MAX_PLAYERS, PlayerCommand, Request, ResumeRequest, ResumeSource, Status,
};

use super::{
    MEDIA, Media, MediaAction, Output, Player, PlayerAction, player_token, resource, token_of,
};

/// Where the sound can go, as this process reads the graph.
pub type Outputs = Box<dyn Fn() -> Vec<Output> + Send + Sync>;

/// The media service.
pub struct MediaService {
    /// What is published: the players as the observer last described them
    /// and where the sound can go.
    media: Media,
    /// The players as the observer last described them, with its playhead
    /// extrapolated to when it published.
    known: Vec<crate::Player>,
    /// A play or pause pressed since then, by bus name: the reading shows
    /// the press at once, and the observer's word replaces it.
    toggled: HashMap<String, bool>,
    /// The cover each player named, by `mpris:artUrl`.
    art: HashMap<String, Option<ArtworkImage>>,
    mpris: Mpris,
    events: Receiver<Event>,
    outputs: Outputs,
}

impl MediaService {
    /// The service over the MPRIS client `mpris`, taking in what it
    /// publishes on `events` and reading where the sound can go with
    /// `outputs`.
    #[must_use]
    pub fn new(mpris: Mpris, events: Receiver<Event>, outputs: Outputs) -> Self {
        let mut service = Self {
            media: Media::default(),
            known: Vec::new(),
            toggled: HashMap::new(),
            art: HashMap::new(),
            mpris,
            events,
            outputs,
        };
        service.refresh();
        service
    }

    /// The service over the session bus's players: the MPRIS client
    /// started on `runtime`, waking `wake` for every change it sees.
    #[must_use]
    pub fn start(runtime: tokio::runtime::Handle, wake: Option<Wake>, outputs: Outputs) -> Self {
        let (events, heard) = std::sync::mpsc::channel();
        let mpris = Mpris::start(runtime, move |event| {
            let _ = events.send(event);
            if let Some(wake) = &wake {
                wake();
            }
        });
        Self::new(mpris, heard, outputs)
    }

    /// The MPRIS client, for what pauses the players when an output
    /// playing them leaves.
    #[must_use]
    pub const fn mpris(&self) -> &Mpris {
        &self.mpris
    }

    /// What plays now.
    #[must_use]
    pub const fn media(&self) -> &Media {
        &self.media
    }

    /// Fold one event the observer published in. Whether the reading
    /// changed.
    fn land(&mut self, event: Event) -> bool {
        match event {
            Event::Players { players } => {
                self.known = players;
                self.known.truncate(MAX_PLAYERS);
                self.toggled.clear();
                if self.art.len() > MAX_PLAYERS {
                    let named: Vec<&String> = self
                        .known
                        .iter()
                        .filter_map(|player| player.art_url.as_ref())
                        .collect();
                    self.art.retain(|url, _| named.contains(&url));
                }
                true
            }
            Event::Artwork { url, image } => {
                self.art.insert(url, image);
                true
            }
            Event::Device { .. } | Event::Transported { .. } | Event::Resumed { .. } => false,
        }
    }

    /// The reading as the players stand now.
    fn refresh(&mut self) {
        self.media.players = self
            .known
            .iter()
            .map(|player| self.published(player))
            .collect();
        self.media.outputs = (self.outputs)();
    }

    /// One player as it is published: what it plays, where, how, and the
    /// cover it named.
    fn published(&self, player: &crate::Player) -> Player {
        let millis = |micros: i64| u64::try_from(micros.max(0) / 1_000).unwrap_or(0);
        let playing = self
            .toggled
            .get(&player.bus_name)
            .copied()
            .unwrap_or(player.status == Status::Playing);
        let paused = if self.toggled.contains_key(&player.bus_name) {
            !playing
        } else {
            player.status == Status::Paused
        };
        Player {
            token: player_token(&player.bus_name),
            bus: player.bus_name.clone(),
            identity: player.name(),
            title: player
                .title
                .clone()
                .filter(|title| !title.trim().is_empty()),
            artist: player
                .artist
                .clone()
                .filter(|artist| !artist.trim().is_empty()),
            playing,
            paused,
            can_play: player.can_play,
            can_pause: player.can_pause,
            can_next: player.can_go_next,
            can_previous: player.can_go_previous,
            art: player
                .art_url
                .as_ref()
                .and_then(|url| self.art.get(url))
                .cloned()
                .flatten(),
            position_ms: millis(player.position_us),
            rate: player.rate,
            duration_ms: player.length_us.filter(|length| *length > 0).map(millis),
            can_seek: player.seekable(),
            url: player.url.clone(),
            app_id: player.package_id.clone(),
            session_id: Self::handed_over(player),
        }
    }

    /// The session another of the person's devices continues, when
    /// `player` can be handed over: a verified app, and a source the other
    /// device can open as it is.
    fn handed_over(player: &crate::Player) -> Option<String> {
        let app = player.package_id.as_deref()?;
        let url = player.url.as_deref()?;
        let openable = crate::handoff::resume_uri(&ResumeSource::Uri(url.to_owned()))?;
        Some(crate::handoff::session_id(app, player, &openable))
    }

    /// The player an action of the `media` resource reaches: the one in
    /// front, by the service's own reading.
    fn front_bus(&self) -> Option<String> {
        self.media.front().map(|front| front.bus.clone())
    }

    /// What `control` asks of the player at `bus`: the one command, or why
    /// it cannot be.
    fn command(&self, bus: &str, control: &MediaAction) -> Option<PlayerCommand> {
        match control {
            MediaAction::PlayPause => Some(PlayerCommand::PlayPause),
            MediaAction::Next => Some(PlayerCommand::Next),
            MediaAction::Previous => Some(PlayerCommand::Previous),
            MediaAction::Stop => Some(PlayerCommand::Stop),
            MediaAction::Seek { fraction } => self
                .known
                .iter()
                .find(|player| player.bus_name == bus)?
                .seek(*fraction),
            MediaAction::Resume { .. } => None,
        }
    }

    /// A play or pause of the player at `bus` shows in the reading at once.
    fn toggled_at(&mut self, bus: &str, command: &PlayerCommand) {
        if *command != PlayerCommand::PlayPause {
            return;
        }
        let playing = self
            .media
            .players
            .iter()
            .find(|player| player.bus == bus)
            .is_some_and(|player| player.playing);
        self.toggled.insert(bus.to_owned(), !playing);
    }
}

/// The 16 bytes `hex` names with its 32 hex digits.
fn request_id(hex: &str) -> Option<[u8; 16]> {
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

impl Service for MediaService {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![resource(&self.media)]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        (resource == MEDIA).then(|| reading(&self.media))
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, DoorError> {
        let unknown = || DoorError::Unknown(ActionAddress::new(resource, action));
        let request = if resource == MEDIA {
            match decode::<MediaAction>(&self.resources(), resource, action, arguments)? {
                MediaAction::Resume {
                    request,
                    app,
                    url,
                    path,
                    position_ms,
                    playing,
                } => {
                    let request_id = request_id(&request).ok_or_else(unknown)?;
                    let source = match (url, path) {
                        (Some(url), _) => ResumeSource::Uri(url),
                        (None, Some(path)) => ResumeSource::LocalFile(path.into()),
                        (None, None) => return Err(unknown()),
                    };
                    self.mpris.handle(Request::Resume {
                        request: ResumeRequest {
                            request_id,
                            package_id: app,
                            source,
                            position_ms,
                            playing,
                        },
                    });
                    return Ok(Started::Accepted);
                }
                control => {
                    let bus = self.front_bus().ok_or_else(unknown)?;
                    let command = self.command(&bus, &control).ok_or_else(unknown)?;
                    self.toggled_at(&bus, &command);
                    Request::Control {
                        bus_name: bus,
                        command,
                    }
                }
            }
        } else {
            let control = decode::<PlayerAction>(&self.resources(), resource, action, arguments)?;
            let bus_name = token_of(resource)
                .and_then(|token| {
                    self.media
                        .players
                        .iter()
                        .find(|player| player.token == token)
                })
                .map(|player| player.bus.clone())
                .ok_or_else(unknown)?;
            self.toggled_at(&bus_name, &control.command());
            Request::Control {
                bus_name,
                command: control.command(),
            }
        };
        self.mpris.handle(request);
        self.refresh();
        Ok(Started::Accepted)
    }

    fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(event) = self.events.try_recv() {
            changed |= self.land(event);
        }
        if changed {
            self.refresh();
        }
        changed
    }

    fn settle(&mut self) {
        self.poll();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player(bus: &str, status: Status) -> crate::Player {
        crate::Player {
            bus_name: format!("org.mpris.MediaPlayer2.{bus}"),
            unique_name: ":1.5".into(),
            pid: None,
            package_id: None,
            desktop_entry: None,
            identity: Some(bus.to_owned()),
            status,
            title: Some("Blue in Green".into()),
            artist: Some("Miles Davis".into()),
            url: Some("https://media.example/blue.flac".into()),
            art_url: None,
            track_id: Some("/track/1".into()),
            length_us: Some(300_000_000),
            position_us: 72_500_000,
            rate: 1.0,
            can_play: true,
            can_pause: true,
            can_seek: true,
            can_go_next: true,
            can_go_previous: false,
        }
    }

    /// A service over a client that is never asked anything: the tests
    /// hand it the events a bus would have published.
    fn service() -> (MediaService, std::sync::mpsc::Sender<Event>) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");
        // The observer's threads outlive these tests.
        let runtime = Box::leak(Box::new(runtime));
        let (sent, heard) = std::sync::mpsc::channel();
        let mpris = Mpris::start(runtime.handle().clone(), move |_| {});
        let service = MediaService::new(
            mpris,
            heard,
            Box::new(|| {
                vec![Output {
                    token: "sound-output:41".into(),
                    label: "Speakers".into(),
                    current: true,
                }]
            }),
        );
        (service, sent)
    }

    #[test]
    fn the_reading_says_what_plays_where_how_and_where_its_sound_can_go() {
        let (mut service, sent) = service();
        assert!(
            service.media().players.is_empty(),
            "nothing plays on a bus this client has not seen"
        );
        sent.send(Event::Players {
            players: vec![player("music", Status::Playing)],
        })
        .unwrap();
        assert!(service.poll());
        let media = serde_json::from_value::<Media>(service.read(MEDIA).unwrap()).unwrap();
        let one = &media.players[0];
        assert_eq!(one.identity, "music");
        assert!(one.playing);
        assert_eq!(one.position_ms, 72_500, "where the playhead was");
        assert_eq!(one.duration_ms, Some(300_000), "how long what plays is");
        assert!(
            (one.rate - 1.0).abs() < f64::EPSILON,
            "how fast the playhead moves: {}",
            one.rate
        );
        assert!(one.can_seek);
        assert_eq!(
            one.url.as_deref(),
            Some("https://media.example/blue.flac"),
            "the source another device continues"
        );
        assert_eq!(
            media.outputs,
            [Output {
                token: "sound-output:41".into(),
                label: "Speakers".into(),
                current: true,
            }],
            "where the card offers to play it"
        );
        assert!(
            !service
                .published(&player("music", Status::Playing))
                .can_previous
        );
    }

    #[test]
    fn the_cover_a_player_names_is_part_of_the_reading() {
        let (mut service, sent) = service();
        let mut named = player("music", Status::Playing);
        named.art_url = Some("file:///home/alice/cover.png".into());
        sent.send(Event::Players {
            players: vec![named],
        })
        .unwrap();
        assert!(service.poll());
        assert!(
            service.media().players[0].art.is_none(),
            "no cover decoded yet"
        );
        let image = ArtworkImage {
            width: 1,
            height: 1,
            rgba: "AAAA/w==".into(),
        };
        sent.send(Event::Artwork {
            url: "file:///home/alice/cover.png".into(),
            image: Some(image.clone()),
        })
        .unwrap();
        assert!(service.poll(), "the cover changed the reading");
        assert_eq!(service.media().players[0].art, Some(image));
    }

    #[test]
    fn the_media_resource_offers_a_seek_and_the_handoff() {
        let (mut service, sent) = service();
        sent.send(Event::Players {
            players: vec![player("music", Status::Playing)],
        })
        .unwrap();
        service.poll();
        let actions: Vec<String> = service.resources()[0]
            .actions
            .iter()
            .map(|spec| spec.id.clone())
            .collect();
        assert!(
            actions.iter().any(|action| action == "seek"),
            "the card takes the playhead to a point: {actions:?}"
        );
        assert!(
            actions.iter().any(|action| action == "resume"),
            "and continues what another device hands over: {actions:?}"
        );
        assert!(
            actions.iter().any(|action| action == "stop"),
            "and stops: {actions:?}"
        );
    }

    #[test]
    fn the_session_asks_to_continue_what_another_device_hands_over() {
        let (mut service, _) = service();
        let args = |request: &str, source: Option<&str>| {
            let mut args = serde_json::Map::new();
            args.insert("request".into(), request.into());
            args.insert("app".into(), "so.tilo.music".into());
            if let Some(source) = source {
                args.insert("url".into(), source.into());
            }
            args.insert("position_ms".into(), 83_000.into());
            args.insert("playing".into(), true.into());
            args
        };
        service
            .invoke(
                MEDIA,
                "resume",
                &args(
                    "00112233445566778899aabbccddeeff",
                    Some("https://media.example/clair.flac"),
                ),
            )
            .expect("a stream this device can open");
        assert!(
            service
                .invoke(
                    MEDIA,
                    "resume",
                    &args("not-hex", Some("https://media.example/clair.flac"))
                )
                .is_err(),
            "a request must be 32 hex digits"
        );
        assert!(
            service
                .invoke(
                    MEDIA,
                    "resume",
                    &args("00112233445566778899aabbccddeeff", None)
                )
                .is_err(),
            "a handoff names the source it continues"
        );
    }
}
