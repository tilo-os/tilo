//! The media service over the players on the session bus.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{Map, Value};
use tilo_actions::{DoorError, ResourceSpec};
use tilo_service::{Jobs, Service, Started, Wake, Worker, decode, reading};
use zbus::blocking::{Connection, MessageIterator, Proxy, fdo::DBusProxy};
use zbus::zvariant::OwnedValue;

use super::{MEDIA, Media, MediaAction, Player, PlayerAction, player_token, resource, token_of};

const PREFIX: &str = "org.mpris.MediaPlayer2.";
const PATH: &str = "/org/mpris/MediaPlayer2";
const ROOT: &str = "org.mpris.MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";

/// The players: read and controlled on the service's worker.
pub trait Players: Send + Sync {
    /// Every player there is now.
    fn players(&self) -> Vec<Player>;

    /// Call the player method `method` (`PlayPause`, `Pause`, `Stop`, `Next`, `Previous`) of
    /// exactly the player on bus name `bus`.
    ///
    /// # Errors
    /// Why the player did not take it.
    fn control(&self, bus: &str, method: &str) -> Result<(), String>;
}

/// The players on the session bus.
pub struct Mpris(Connection);

impl Mpris {
    /// The session bus's players.
    ///
    /// # Errors
    /// The session bus cannot be reached.
    pub fn new() -> zbus::Result<Self> {
        Connection::session().map(Self)
    }

    /// Call `changed` whenever a player comes, goes or changes what it
    /// plays or offers, until the bus goes away: on a thread of its own,
    /// following the players' signals, never asking them on a timer.
    ///
    /// # Errors
    /// The thread did not start or the bus refused the subscription.
    pub fn follow(&self, changed: impl Fn() + Send + Sync + 'static) -> std::io::Result<()> {
        let rule = |text: &str| -> std::io::Result<zbus::MatchRule<'static>> {
            zbus::MatchRule::try_from(text)
                .map(zbus::MatchRule::into_owned)
                .map_err(|error| std::io::Error::other(error.to_string()))
        };
        let properties = rule(&format!(
            "type='signal',interface='org.freedesktop.DBus.Properties',member='PropertiesChanged',path='{PATH}'"
        ))?;
        let owners = rule(&format!(
            "type='signal',sender='org.freedesktop.DBus',interface='org.freedesktop.DBus',member='NameOwnerChanged',arg0namespace='{ROOT}'"
        ))?;
        let subscribe = |rule| {
            MessageIterator::for_match_rule(rule, &self.0, None)
                .map_err(|error| std::io::Error::other(error.to_string()))
        };
        let properties = subscribe(properties)?;
        let owners = subscribe(owners)?;
        let changed = Arc::new(changed);
        for (name, messages) in [
            ("tilo-media-players", properties),
            ("tilo-media-owners", owners),
        ] {
            let changed = Arc::clone(&changed);
            std::thread::Builder::new()
                .name(name.into())
                .spawn(move || {
                    for message in messages {
                        if message.is_err() {
                            break;
                        }
                        changed();
                    }
                })?;
        }
        Ok(())
    }

    fn player(&self, bus: &str) -> Option<Player> {
        let root = Proxy::new(&self.0, bus, PATH, ROOT).ok()?;
        let player = Proxy::new(&self.0, bus, PATH, PLAYER).ok()?;
        let flag = |name: &str| player.get_property::<bool>(name).unwrap_or(false);
        let metadata = player
            .get_property::<HashMap<String, OwnedValue>>("Metadata")
            .unwrap_or_default();
        let text = |key: &str| {
            metadata
                .get(key)
                .and_then(|value| String::try_from(value.clone()).ok())
                .filter(|text| !text.is_empty())
        };
        let artist = metadata
            .get("xesam:artist")
            .and_then(|value| Vec::<String>::try_from(value.clone()).ok())
            .map(|artists| artists.join(", "))
            .filter(|artists| !artists.is_empty());
        Some(Player {
            token: player_token(bus),
            bus: bus.to_owned(),
            identity: root
                .get_property::<String>("Identity")
                .unwrap_or_else(|_| bus.trim_start_matches(PREFIX).to_owned()),
            title: text("xesam:title"),
            artist,
            playing: player
                .get_property::<String>("PlaybackStatus")
                .is_ok_and(|status| status == "Playing"),
            can_play: flag("CanPlay"),
            can_pause: flag("CanPause"),
            can_next: flag("CanGoNext"),
            can_previous: flag("CanGoPrevious"),
        })
    }
}

impl Players for Mpris {
    fn players(&self) -> Vec<Player> {
        let Ok(names) =
            DBusProxy::new(&self.0).and_then(|bus| bus.list_names().map_err(zbus::Error::from))
        else {
            return Vec::new();
        };
        let mut players: Vec<Player> = names
            .iter()
            .filter(|name| name.as_str().starts_with(PREFIX))
            .filter_map(|name| self.player(name.as_str()))
            .collect();
        players.sort_by(|a, b| a.bus.cmp(&b.bus));
        players
    }

    fn control(&self, bus: &str, method: &str) -> Result<(), String> {
        let player = Proxy::new(&self.0, bus, PATH, PLAYER).map_err(|error| error.to_string())?;
        player
            .call_method(method, &())
            .map(drop)
            .map_err(|error| error.to_string())
    }
}

/// Pause every player playing on `players`: for the audio policy, when
/// the private output they played on leaves. `false` when one did not
/// take it.
pub fn pause_playing(players: &dyn Players) -> bool {
    let mut paused = true;
    for player in players.players() {
        if player.playing && player.can_pause {
            paused &= players.control(&player.bus, "Pause").is_ok();
        }
    }
    paused
}

/// What lands from the worker.
enum Landed {
    Read(Vec<Player>),
    Controlled(Result<(), String>, Vec<Player>),
}

/// The media service.
pub struct MediaService {
    media: Media,
    worker: Worker<dyn Players, Landed>,
}

fn refresh(jobs: &Jobs<dyn Players, Landed>) {
    jobs.run(|players| Landed::Read(players.players()));
}

impl MediaService {
    /// The service over `players`, reading them once at the start.
    ///
    /// # Errors
    /// The worker's thread did not start.
    pub fn new(players: Arc<dyn Players>, wake: Option<Wake>) -> std::io::Result<Self> {
        let service = Self {
            media: Media::default(),
            worker: Worker::spawn("tilo-media", players, wake)?,
        };
        refresh(&service.worker.jobs());
        Ok(service)
    }

    /// What reads the players again when called: for their signals.
    pub fn refresher(&self) -> impl Fn() + Send + Sync + 'static {
        let jobs = self.worker.jobs();
        move || refresh(&jobs)
    }

    /// What plays now.
    #[must_use]
    pub const fn media(&self) -> &Media {
        &self.media
    }

    fn land(&mut self, landed: Landed) {
        match landed {
            Landed::Read(players) => self.media.players = players,
            Landed::Controlled(result, players) => {
                self.media.problem = result.err();
                self.media.players = players;
            }
        }
    }
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
        let unknown = || DoorError::Unknown(tilo_actions::ActionAddress::new(resource, action));
        let (player, method) = if resource == MEDIA {
            let control = decode::<MediaAction>(&self.resources(), resource, action, arguments)?;
            (self.media.front().ok_or_else(unknown)?, control.method())
        } else {
            let control = decode::<PlayerAction>(&self.resources(), resource, action, arguments)?;
            let player = token_of(resource)
                .and_then(|token| {
                    self.media
                        .players
                        .iter()
                        .find(|player| player.token == token)
                })
                .ok_or_else(unknown)?;
            (player, control.method())
        };
        let bus = player.bus.clone();
        self.worker.run(move |players| {
            let result = players.control(&bus, method);
            Landed::Controlled(result, players.players())
        });
        Ok(Started::Accepted)
    }

    fn poll(&mut self) -> bool {
        let landed = self.worker.landed();
        let changed = !landed.is_empty();
        for landed in landed {
            self.land(landed);
        }
        changed
    }

    fn settle(&mut self) {
        for landed in self.worker.settle() {
            self.land(landed);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct Fake {
        players: Mutex<Vec<Player>>,
        calls: Mutex<Vec<(String, String)>>,
    }

    impl Players for Fake {
        fn players(&self) -> Vec<Player> {
            self.players.lock().unwrap().clone()
        }

        fn control(&self, bus: &str, method: &str) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push((bus.to_owned(), method.to_owned()));
            let mut players = self.players.lock().unwrap();
            if method == "PlayPause"
                && let Some(player) = players.iter_mut().find(|player| player.bus == bus)
            {
                player.playing = !player.playing;
            }
            drop(players);
            Ok(())
        }
    }

    fn playing(bus: &str) -> Player {
        Player {
            token: player_token(bus),
            bus: bus.into(),
            identity: "Music".into(),
            title: Some("Blue in Green".into()),
            artist: Some("Miles Davis".into()),
            playing: true,
            can_play: true,
            can_pause: true,
            can_next: true,
            can_previous: false,
        }
    }

    #[test]
    fn a_player_offers_its_controls_and_pauses_by_address() {
        let fake = Arc::new(Fake::default());
        fake.players
            .lock()
            .unwrap()
            .push(playing("org.mpris.MediaPlayer2.music"));
        let mut service = MediaService::new(Arc::clone(&fake) as Arc<dyn Players>, None).unwrap();
        service.settle();
        let resources = service.resources();
        let player = &resources[0].children[0];
        assert_eq!(player.label, "Blue in Green · Miles Davis");
        assert!(
            !player.id.as_str().contains("music"),
            "the bus name stays out of what is published"
        );
        let labels: Vec<&str> = player
            .actions
            .iter()
            .map(|spec| spec.label.as_str())
            .collect();
        assert_eq!(
            labels,
            ["Pause", "Next track"],
            "no previous track to go to"
        );

        let id = player.id.as_str().to_owned();
        assert_eq!(
            service.invoke(&id, "play-pause", &Map::new()),
            Ok(Started::Accepted)
        );
        service.settle();
        assert_eq!(
            fake.calls.lock().unwrap().as_slice(),
            [(
                "org.mpris.MediaPlayer2.music".to_owned(),
                "PlayPause".to_owned()
            )]
        );
        assert_eq!(
            service.resources()[0].children[0].actions[0].label,
            "Play",
            "it paused"
        );
    }

    #[test]
    fn the_media_resource_controls_the_player_in_front() {
        let fake = Arc::new(Fake::default());
        let mut paused = playing("org.mpris.MediaPlayer2.radio");
        paused.playing = false;
        fake.players
            .lock()
            .unwrap()
            .extend([paused, playing("org.mpris.MediaPlayer2.music")]);
        let mut service = MediaService::new(Arc::clone(&fake) as Arc<dyn Players>, None).unwrap();
        service.settle();
        let media = &service.resources()[0];
        assert!(
            media.actions.iter().any(|spec| spec.id == "previous"
                && matches!(
                    spec.contract.availability,
                    tilo_actions::Availability::Unavailable { .. }
                )),
            "the player in front cannot go back"
        );
        assert_eq!(
            service.invoke(MEDIA, "stop", &Map::new()),
            Ok(Started::Accepted)
        );
        service.settle();
        assert!(pause_playing(&*fake));
        assert_eq!(
            fake.calls.lock().unwrap().as_slice(),
            [
                ("org.mpris.MediaPlayer2.music".to_owned(), "Stop".to_owned()),
                (
                    "org.mpris.MediaPlayer2.music".to_owned(),
                    "Pause".to_owned()
                ),
            ],
            "the key reaches the one playing, the pause only what plays"
        );

        fake.players.lock().unwrap().clear();
        service.settle();
        (service.refresher())();
        service.settle();
        assert!(service.invoke(MEDIA, "play-pause", &Map::new()).is_err());
    }
}
