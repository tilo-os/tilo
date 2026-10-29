//! The one model of the session's MPRIS players.
//!
//! The observer ([`super::observer`]) turns bus signals into
//! [`PlayerEvent`]s and applies them here. Every view taken of the players
//! is a projection of this one table: the players the compositor shows and
//! hands off, the transport keys' target, the players an unplugged output
//! pauses and the candidates a resume drives. Nothing here touches a bus, so
//! all of it is exercised without one.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use zbus::zvariant::OwnedValue;

use super::MPRIS_PREFIX;
use crate::{MAX_PLAYERS, Player, Status};

/// Players beyond this are not tracked: a client claiming thousands of MPRIS
/// names must not grow the helper, or the calls it makes, without bound.
pub const MAX_TRACKED_PLAYERS: usize = 64;

/// What changed on the bus, as the observer saw it.
#[derive(Debug)]
pub(super) enum PlayerEvent {
    /// A connection now owns `bus_name`. `properties` is the union of both
    /// MPRIS interfaces' `GetAll`; `package_id` is the launcher-verified
    /// package behind `pid`, resolved once for this owner.
    Appeared {
        bus_name: String,
        unique_name: String,
        pid: Option<u32>,
        package_id: Option<String>,
        properties: HashMap<String, OwnedValue>,
    },
    /// Nobody owns `bus_name` any more.
    Vanished { bus_name: String },
    /// `PropertiesChanged` from the connection `unique_name`, or the answer
    /// to a re-read of properties it invalidated.
    Changed {
        unique_name: String,
        changed: HashMap<String, OwnedValue>,
        invalidated: Vec<String>,
    },
    /// The playhead of `unique_name`'s players, from `Seeked` or a fresh read
    /// of `Position`.
    Seeked {
        unique_name: String,
        position_us: i64,
    },
}

/// What applying one event did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Applied {
    /// The table changed, so every projection must be recomputed.
    pub(super) changed: bool,
    /// A status, rate or track change left the extrapolated playhead
    /// unreliable: the player does not signal `Position`, so the observer
    /// reads it once.
    pub(super) position_stale: bool,
}

/// One player as last signalled.
#[derive(Debug)]
pub(super) struct PlayerEntry {
    /// The connection owning the well-known name. Signals arrive from it,
    /// and a new owner is a new player, verified afresh.
    unique_name: String,
    pid: Option<u32>,
    package_id: Option<String>,
    /// Every property of both interfaces except `Position`, as signalled.
    properties: HashMap<String, OwnedValue>,
    /// `Position` when it was last read, and when: MPRIS never signals a
    /// playhead that merely advances, so it is extrapolated from here.
    position_us: i64,
    observed_at: Instant,
    /// When it appeared, and when it last started or stopped playing (`0`:
    /// never played), on the table's own count, so the transport keys find
    /// the player the person last listened to.
    appeared: u64,
    played: u64,
}

impl PlayerEntry {
    fn string(&self, key: &str) -> Option<String> {
        metadata_string(&self.properties, key)
    }

    fn flag(&self, key: &str) -> bool {
        self.properties
            .get(key)
            .and_then(|value| value.try_clone().ok())
            .and_then(|value| bool::try_from(value).ok())
            .unwrap_or(false)
    }

    fn status(&self) -> Option<String> {
        self.string("PlaybackStatus")
    }

    fn is_playing(&self) -> bool {
        self.status().as_deref() == Some("Playing")
    }

    fn metadata(&self) -> HashMap<String, OwnedValue> {
        self.properties
            .get("Metadata")
            .and_then(|value| value.try_clone().ok())
            .and_then(|value| HashMap::<String, OwnedValue>::try_from(value).ok())
            .unwrap_or_default()
    }

    /// `Rate`, when the player reports a usable one.
    fn rate(&self) -> f64 {
        self.properties
            .get("Rate")
            .and_then(|value| value.try_clone().ok())
            .and_then(|value| f64::try_from(value).ok())
            .filter(|rate| rate.is_finite() && *rate > 0.0)
            .unwrap_or(1.0)
    }

    /// The playhead at `now`: advanced at `Rate` while playing, bounded by
    /// the track length.
    fn position_at(&self, now: Instant) -> i64 {
        let base = self.position_us.max(0);
        let position = if self.is_playing() {
            let elapsed = now.saturating_duration_since(self.observed_at);
            let advanced =
                Duration::try_from_secs_f64(elapsed.as_secs_f64() * self.rate()).unwrap_or(elapsed);
            base.saturating_add(i64::try_from(advanced.as_micros()).unwrap_or(i64::MAX))
        } else {
            base
        };
        match length_us(&self.metadata()) {
            Some(length) => position.min(length),
            None => position,
        }
    }

    /// Fold the playhead advanced so far into its base, so a change of
    /// status or rate does not re-extrapolate time already played.
    fn rebase(&mut self, now: Instant) {
        self.position_us = self.position_at(now);
        self.observed_at = now;
    }

    /// The entry as the compositor sees it. `None` for a player with no
    /// transport state.
    fn player(&self, bus_name: &str, now: Instant) -> Option<Player> {
        let status = Status::from_mpris(&self.status()?)?;
        let metadata = self.metadata();
        let artist = metadata
            .get("xesam:artist")
            .and_then(|value| value.try_clone().ok())
            .and_then(|value| Vec::<String>::try_from(value).ok())
            .map(|artists| {
                artists
                    .iter()
                    .filter_map(|artist| clean_text(artist))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .filter(|artists| !artists.is_empty());
        let track_id = metadata
            .get("mpris:trackid")
            .and_then(|value| value.try_clone().ok())
            .and_then(|value| {
                let fallback = value.try_clone().ok();
                zbus::zvariant::OwnedObjectPath::try_from(value)
                    .map(|path| path.as_str().to_string())
                    .ok()
                    .or_else(|| fallback.and_then(|value| String::try_from(value).ok()))
            })
            .filter(|track| track != "/org/mpris/MediaPlayer2/TrackList/NoTrack");
        Some(Player {
            bus_name: bus_name.to_string(),
            unique_name: self.unique_name.clone(),
            pid: self.pid,
            package_id: self.package_id.clone(),
            desktop_entry: self.string("DesktopEntry"),
            identity: self.string("Identity").as_deref().and_then(clean_text),
            status,
            title: metadata_string(&metadata, "xesam:title").and_then(|title| clean_text(&title)),
            artist,
            url: metadata_string(&metadata, "xesam:url"),
            art_url: metadata_string(&metadata, "mpris:artUrl")
                .filter(|url| url.starts_with("file://") && url.len() <= 4096),
            track_id,
            length_us: length_us(&metadata),
            position_us: self.position_at(now),
            rate: self.rate(),
            can_play: self.flag("CanPlay"),
            can_pause: self.flag("CanPause"),
            can_seek: self.flag("CanSeek"),
            can_go_next: self.flag("CanGoNext"),
            can_go_previous: self.flag("CanGoPrevious"),
        })
    }
}

/// Every MPRIS player on the session bus, keyed by well-known name so each
/// projection lists them in one stable order.
#[derive(Debug, Default)]
pub(super) struct PlayerTable {
    players: BTreeMap<String, PlayerEntry>,
    /// Counts appearances and starts of playback, for [`Self::active_player`].
    events: u64,
}

impl PlayerTable {
    #[cfg(test)]
    fn len(&self) -> usize {
        self.players.len()
    }

    /// Whether a new owner of `bus_name` would be tracked.
    pub(super) fn admits(&self, bus_name: &str) -> bool {
        self.players.contains_key(bus_name) || self.players.len() < MAX_TRACKED_PLAYERS
    }

    pub(super) fn apply(&mut self, event: PlayerEvent, now: Instant) -> Applied {
        self.events = self.events.saturating_add(1);
        let sequence = self.events;
        match event {
            PlayerEvent::Appeared {
                bus_name,
                unique_name,
                pid,
                package_id,
                mut properties,
            } => {
                if !bus_name.starts_with(MPRIS_PREFIX)
                    || bus_name.len() > 256
                    || !self.admits(&bus_name)
                {
                    return Applied::default();
                }
                let position_us = properties
                    .remove("Position")
                    .and_then(|value| i64::try_from(value).ok())
                    .unwrap_or(0);
                let mut entry = PlayerEntry {
                    unique_name,
                    pid,
                    package_id,
                    properties,
                    position_us,
                    observed_at: now,
                    appeared: sequence,
                    played: 0,
                };
                if entry.is_playing() {
                    entry.played = sequence;
                }
                self.players.insert(bus_name, entry);
                Applied {
                    changed: true,
                    position_stale: false,
                }
            }
            PlayerEvent::Vanished { bus_name } => Applied {
                changed: self.players.remove(&bus_name).is_some(),
                position_stale: false,
            },
            PlayerEvent::Changed {
                unique_name,
                mut changed,
                invalidated,
            } => {
                let position = changed
                    .remove("Position")
                    .and_then(|value| i64::try_from(value).ok());
                let moves_playhead = ["PlaybackStatus", "Rate", "Metadata"].iter().any(|key| {
                    changed.contains_key(*key)
                        || invalidated.iter().any(|name| name.as_str() == *key)
                });
                let mut applied = Applied::default();
                for entry in self.owned_by(&unique_name) {
                    let was_playing = entry.is_playing();
                    entry.rebase(now);
                    for name in &invalidated {
                        entry.properties.remove(name);
                    }
                    for (name, value) in &changed {
                        if let Ok(value) = value.try_clone() {
                            entry.properties.insert(name.clone(), value);
                        }
                    }
                    if let Some(position_us) = position {
                        entry.position_us = position_us;
                        entry.observed_at = now;
                    }
                    if was_playing != entry.is_playing() {
                        entry.played = sequence;
                    }
                    applied.changed = true;
                    applied.position_stale |= moves_playhead && position.is_none();
                }
                applied
            }
            PlayerEvent::Seeked {
                unique_name,
                position_us,
            } => {
                let mut applied = Applied::default();
                for entry in self.owned_by(&unique_name) {
                    entry.position_us = position_us;
                    entry.observed_at = now;
                    applied.changed = true;
                }
                applied
            }
        }
    }

    fn owned_by<'a>(
        &'a mut self,
        unique_name: &'a str,
    ) -> impl Iterator<Item = &'a mut PlayerEntry> {
        self.players
            .values_mut()
            .filter(move |entry| entry.unique_name == unique_name)
    }

    /// Every player, its playhead extrapolated to `now`. Verification is
    /// not required: what is on this machine's own bus is shown and driven
    /// here; only a handoff needs the verified package.
    pub(super) fn players(&self, now: Instant) -> Vec<Player> {
        self.players
            .iter()
            .filter_map(|(bus_name, entry)| entry.player(bus_name, now))
            .take(MAX_PLAYERS)
            .collect()
    }

    /// The players playing now: what an unplugged private output pauses.
    pub(super) fn playing(&self) -> Vec<String> {
        self.players
            .iter()
            .filter(|(_, entry)| entry.is_playing())
            .map(|(bus_name, _)| bus_name.clone())
            .collect()
    }

    /// The player the transport keys address, and whether it is playing:
    /// the one playing that started last, else the one that stopped playing
    /// last, so play/pause resumes what the person paused rather than
    /// whichever player sorts first by name; else the one that appeared last.
    pub(super) fn active_player(&self) -> Option<(String, bool)> {
        self.players
            .iter()
            .max_by_key(|(_, entry)| (entry.is_playing(), entry.played, entry.appeared))
            .map(|(bus_name, entry)| (bus_name.clone(), entry.is_playing()))
    }

    /// The players verified to belong to `package_id`, each with the source
    /// it reports (`xesam:url`).
    pub(super) fn players_of(&self, package_id: &str) -> Vec<(String, Option<String>)> {
        self.players
            .iter()
            .filter(|(_, entry)| entry.package_id.as_deref() == Some(package_id))
            .map(|(bus_name, entry)| {
                (
                    bus_name.clone(),
                    metadata_string(&entry.metadata(), "xesam:url"),
                )
            })
            .collect()
    }

    /// Record a late verification, unless the name changed owner since.
    pub(super) fn verify(&mut self, bus_name: &str, unique_name: &str, package_id: String) -> bool {
        match self.players.get_mut(bus_name) {
            Some(entry) if entry.unique_name == unique_name && entry.package_id.is_none() => {
                entry.package_id = Some(package_id);
                true
            }
            _ => false,
        }
    }
}

fn clean_text(value: &str) -> Option<String> {
    let value = value.trim().to_string();
    (!value.is_empty() && !value.chars().any(char::is_control)).then_some(value)
}

pub(super) fn metadata_string(metadata: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    metadata
        .get(key)
        .and_then(|value| value.try_clone().ok())
        .and_then(|value| String::try_from(value).ok())
}

/// `mpris:length`, signed as the spec says or unsigned as some players send
/// it; `None` unless positive.
fn length_us(metadata: &HashMap<String, OwnedValue>) -> Option<i64> {
    let value = metadata.get("mpris:length")?;
    value
        .try_clone()
        .ok()
        .and_then(|value| i64::try_from(value).ok())
        .or_else(|| {
            value
                .try_clone()
                .ok()
                .and_then(|value| u64::try_from(value).ok())
                .and_then(|value| i64::try_from(value).ok())
        })
        .filter(|length| *length > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::Value;

    const PLAYER: &str = "org.mpris.MediaPlayer2.browser";
    const OWNER: &str = ":1.42";

    fn value(value: impl Into<Value<'static>>) -> OwnedValue {
        OwnedValue::try_from(value.into()).unwrap()
    }

    fn metadata(url: &str) -> OwnedValue {
        value(HashMap::from([
            ("xesam:url".to_string(), value(url.to_string())),
            ("xesam:title".to_string(), value("A film".to_string())),
            ("mpris:length".to_string(), value(9_000_000_i64)),
        ]))
    }

    fn appeared(
        bus_name: &str,
        unique_name: &str,
        package_id: Option<&str>,
        status: &str,
    ) -> PlayerEvent {
        PlayerEvent::Appeared {
            bus_name: bus_name.into(),
            unique_name: unique_name.into(),
            pid: Some(4242),
            package_id: package_id.map(Into::into),
            properties: HashMap::from([
                ("Identity".to_string(), value("Browser".to_string())),
                ("PlaybackStatus".to_string(), value(status.to_string())),
                (
                    "Metadata".to_string(),
                    metadata("https://media.example/film.mp4"),
                ),
                ("Position".to_string(), value(4_000_000_i64)),
                ("CanPlay".to_string(), value(true)),
                ("CanPause".to_string(), value(true)),
            ]),
        }
    }

    fn changed(unique_name: &str, key: &str, new: OwnedValue) -> PlayerEvent {
        PlayerEvent::Changed {
            unique_name: unique_name.into(),
            changed: HashMap::from([(key.to_string(), new)]),
            invalidated: Vec::new(),
        }
    }

    #[test]
    fn appear_change_vanish_updates_the_players() {
        let now = Instant::now();
        let mut table = PlayerTable::default();
        let applied = table.apply(
            appeared(PLAYER, OWNER, Some("so.tilo.browser"), "Playing"),
            now,
        );
        assert!(applied.changed);

        let players = table.players(now);
        assert_eq!(players.len(), 1);
        assert_eq!(players[0].bus_name, PLAYER);
        assert_eq!(players[0].unique_name, OWNER);
        assert_eq!(players[0].status, Status::Playing);
        assert_eq!(players[0].position_us, 4_000_000);
        assert_eq!(
            players[0].url.as_deref(),
            Some("https://media.example/film.mp4")
        );
        assert_eq!(table.playing(), [PLAYER]);

        let applied = table.apply(
            changed(OWNER, "PlaybackStatus", value("Paused".to_string())),
            now,
        );
        assert_eq!(
            applied,
            Applied {
                changed: true,
                position_stale: true,
            },
            "a status change is applied and asks for one fresh playhead"
        );
        assert_eq!(table.players(now)[0].status, Status::Paused);
        assert!(table.playing().is_empty());

        let elsewhere = table.apply(
            changed(":1.99", "PlaybackStatus", value("Playing".to_string())),
            now,
        );
        assert!(
            !elsewhere.changed,
            "a connection may only change its own players"
        );
        assert_eq!(table.players(now)[0].status, Status::Paused);

        assert!(
            table
                .apply(
                    PlayerEvent::Vanished {
                        bus_name: PLAYER.into()
                    },
                    now
                )
                .changed
        );
        assert!(table.players(now).is_empty());
    }

    #[test]
    fn position_extrapolates_and_seeked_resets() {
        let start = Instant::now();
        let mut table = PlayerTable::default();
        table.apply(appeared(PLAYER, OWNER, None, "Playing"), start);
        let later = start + Duration::from_secs(2);
        let at = |table: &PlayerTable, when| table.players(when)[0].position_us;
        assert_eq!(
            at(&table, later),
            6_000_000,
            "a playing track advances between signals"
        );

        table.apply(
            PlayerEvent::Seeked {
                unique_name: OWNER.into(),
                position_us: 1_000_000,
            },
            later,
        );
        assert_eq!(at(&table, later), 1_000_000);
        assert_eq!(at(&table, later + Duration::from_secs(1)), 2_000_000);

        table.apply(changed(OWNER, "Rate", value(2.0_f64)), later);
        assert!((table.players(later)[0].rate - 2.0).abs() < f64::EPSILON);
        assert_eq!(
            at(&table, later + Duration::from_secs(1)),
            3_000_000,
            "the playhead advances at the signalled rate"
        );

        table.apply(
            changed(OWNER, "PlaybackStatus", value("Paused".to_string())),
            later + Duration::from_secs(1),
        );
        assert_eq!(
            at(&table, later + Duration::from_secs(60)),
            3_000_000,
            "pausing freezes the playhead where it was"
        );
        assert_eq!(table.players(later)[0].length_us, Some(9_000_000));
    }

    #[test]
    fn players_are_verified_once_per_owner() {
        let now = Instant::now();
        let mut table = PlayerTable::default();
        table.apply(appeared(PLAYER, OWNER, None, "Playing"), now);
        assert_eq!(table.players(now)[0].package_id, None);
        assert_eq!(table.players(now)[0].pid, Some(4242));
        assert!(table.players_of("so.tilo.browser").is_empty());

        assert!(
            !table.verify(PLAYER, ":1.7", "so.tilo.browser".into()),
            "a verification for an earlier owner is discarded"
        );
        assert!(table.verify(PLAYER, OWNER, "so.tilo.browser".into()));
        assert_eq!(
            table.players(now)[0].package_id.as_deref(),
            Some("so.tilo.browser")
        );
        assert_eq!(
            table.players_of("so.tilo.browser"),
            [(
                PLAYER.to_string(),
                Some("https://media.example/film.mp4".to_string())
            )]
        );

        table.apply(appeared(PLAYER, ":1.43", None, "Playing"), now);
        assert_eq!(
            table.players(now)[0].package_id,
            None,
            "a new owner of the same name is verified afresh"
        );
    }

    #[test]
    fn players_read_metadata_and_only_local_artwork() {
        let now = Instant::now();
        let mut table = PlayerTable::default();
        let track = zbus::zvariant::ObjectPath::try_from("/org/tilo/track/7").unwrap();
        table.apply(
            PlayerEvent::Appeared {
                bus_name: "org.mpris.MediaPlayer2.music".into(),
                unique_name: ":1.1".into(),
                pid: None,
                package_id: None,
                properties: HashMap::from([
                    ("Identity".to_string(), value(" Music ".to_string())),
                    ("PlaybackStatus".to_string(), value("Playing".to_string())),
                    ("CanSeek".to_string(), value(true)),
                    ("CanGoPrevious".to_string(), value(true)),
                    (
                        "Metadata".to_string(),
                        value(HashMap::from([
                            ("xesam:title".to_string(), value("Song".to_string())),
                            (
                                "xesam:artist".to_string(),
                                value(vec!["One".to_string(), "Two".to_string()]),
                            ),
                            ("mpris:length".to_string(), value(180_000_000_u64)),
                            ("mpris:trackid".to_string(), value(track)),
                            (
                                "mpris:artUrl".to_string(),
                                value("file:///home/me/Music/cover%20art.png".to_string()),
                            ),
                        ])),
                    ),
                ]),
            },
            now,
        );
        table.apply(
            PlayerEvent::Appeared {
                bus_name: "org.mpris.MediaPlayer2.web".into(),
                unique_name: ":1.2".into(),
                pid: None,
                package_id: None,
                properties: HashMap::from([
                    ("PlaybackStatus".to_string(), value("Paused".to_string())),
                    (
                        "Metadata".to_string(),
                        value(HashMap::from([(
                            "mpris:artUrl".to_string(),
                            value("https://tracker.example/cover.jpg".to_string()),
                        )])),
                    ),
                ]),
            },
            now,
        );
        table.apply(
            appeared("org.mpris.MediaPlayer2.odd", ":1.3", None, "Buffering"),
            now,
        );
        let players = table.players(now);
        assert_eq!(
            players.len(),
            2,
            "a player with no transport state is left out"
        );
        let music = &players[0];
        assert_eq!(music.identity.as_deref(), Some("Music"));
        assert_eq!(music.title.as_deref(), Some("Song"));
        assert_eq!(music.artist.as_deref(), Some("One, Two"));
        assert_eq!(music.length_us, Some(180_000_000));
        assert_eq!(music.track_id.as_deref(), Some("/org/tilo/track/7"));
        assert!(music.can_seek && music.can_go_previous && !music.can_go_next);
        assert_eq!(
            music.art_url.as_deref(),
            Some("file:///home/me/Music/cover%20art.png")
        );
        let web = &players[1];
        assert_eq!(web.art_url, None, "remote artwork is never fetched");
        assert_eq!(web.identity, None);
        assert_eq!(web.length_us, None);
    }

    #[test]
    fn active_player_prefers_playing() {
        let now = Instant::now();
        let mut table = PlayerTable::default();
        assert_eq!(table.active_player(), None);
        table.apply(
            appeared("org.mpris.MediaPlayer2.a", ":1.1", None, "Paused"),
            now,
        );
        assert_eq!(
            table.active_player(),
            Some(("org.mpris.MediaPlayer2.a".to_string(), false))
        );
        table.apply(
            appeared("org.mpris.MediaPlayer2.b", ":1.2", None, "Playing"),
            now,
        );
        assert_eq!(
            table.active_player(),
            Some(("org.mpris.MediaPlayer2.b".to_string(), true))
        );
    }

    #[test]
    fn transport_keys_address_the_player_that_last_played() {
        let now = Instant::now();
        let mut table = PlayerTable::default();
        let (browser, music) = (
            "org.mpris.MediaPlayer2.chromium",
            "org.mpris.MediaPlayer2.music",
        );
        table.apply(appeared(browser, ":1.1", None, "Paused"), now);
        table.apply(appeared(music, ":1.2", None, "Playing"), now);
        table.apply(
            changed(":1.2", "PlaybackStatus", value("Paused".to_string())),
            now,
        );
        assert_eq!(
            table.active_player(),
            Some((music.to_string(), false)),
            "play resumes the music just paused, not the browser that sorts first"
        );

        table.apply(
            changed(":1.1", "PlaybackStatus", value("Playing".to_string())),
            now,
        );
        table.apply(
            changed(":1.2", "PlaybackStatus", value("Playing".to_string())),
            now,
        );
        assert_eq!(
            table.active_player(),
            Some((music.to_string(), true)),
            "of two playing, pause stops the one started last"
        );

        let opened = "org.mpris.MediaPlayer2.video";
        table.apply(
            changed(":1.2", "PlaybackStatus", value("Paused".to_string())),
            now,
        );
        table.apply(
            changed(":1.1", "PlaybackStatus", value("Paused".to_string())),
            now,
        );
        table.apply(appeared(opened, ":1.3", None, "Stopped"), now);
        assert_eq!(
            table.active_player(),
            Some((browser.to_string(), false)),
            "a player that only opened does not take the keys from one that played"
        );
    }

    #[test]
    fn the_table_tracks_only_mpris_names_and_only_so_many() {
        let now = Instant::now();
        let mut table = PlayerTable::default();
        assert!(
            !table
                .apply(
                    appeared("org.example.NotAPlayer", ":1.1", None, "Playing"),
                    now
                )
                .changed
        );
        for index in 0..=MAX_TRACKED_PLAYERS {
            table.apply(
                appeared(
                    &format!("{MPRIS_PREFIX}p{index:03}"),
                    ":1.1",
                    None,
                    "Paused",
                ),
                now,
            );
        }
        assert_eq!(table.len(), MAX_TRACKED_PLAYERS);
        assert_eq!(table.players(now).len(), MAX_PLAYERS);
        assert!(table.admits(&format!("{MPRIS_PREFIX}p000")));
        assert!(!table.admits(&format!("{MPRIS_PREFIX}late")));
    }
}
