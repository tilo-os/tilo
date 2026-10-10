//! The shell's one model of the session's MPRIS players.
//!
//! The observer ([`super::observer`]) turns bus signals into
//! [`PlayerEvent`]s and applies them here. Every view the shell takes of the
//! players is a projection of this one table: the now-playing card, the
//! cross-device handoff reports, the transport keys' target, the players an
//! unplugged output pauses and the candidates a resume drives. Nothing here
//! touches a bus, so all of it is exercised without one.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use media::handoff::PlaybackReport;
use zbus::zvariant::OwnedValue;

use super::{
    InstalledMediaApp, MAX_NOW_PLAYING_PLAYERS, MPRIS_PREFIX, NowPlayingProperties,
    PlayerObservation, metadata_i64, metadata_string, now_playing_from_properties,
    report_from_observation,
};

/// Players beyond this are not tracked: a client claiming thousands of MPRIS
/// names must not grow the shell, or the calls it makes, without bound.
pub(super) const MAX_TRACKED_PLAYERS: usize = 64;

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
        match metadata_i64(&self.metadata(), "mpris:length") {
            Some(length) if length > 0 => position.min(length),
            _ => position,
        }
    }

    /// Fold the playhead advanced so far into its base, so a change of
    /// status or rate does not re-extrapolate time already played.
    fn rebase(&mut self, now: Instant) {
        self.position_us = self.position_at(now);
        self.observed_at = now;
    }
}

/// Every MPRIS player on the session bus, keyed by well-known name so each
/// projection lists them in one stable order.
#[derive(Debug, Default)]
pub(super) struct PlayerTable {
    players: BTreeMap<String, PlayerEntry>,
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
                self.players.insert(
                    bus_name,
                    PlayerEntry {
                        unique_name,
                        pid,
                        package_id,
                        properties,
                        position_us,
                        observed_at: now,
                    },
                );
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

    /// The now-playing card's players, each with the local artwork URL it
    /// names. Verification is not required: the card only shows and
    /// controls what is on this machine's own bus.
    pub(super) fn now_playing(
        &self,
    ) -> Vec<(crate::shell::now_playing::PlayerSnapshot, Option<String>)> {
        self.players
            .iter()
            .filter_map(|(bus_name, entry)| {
                let properties = NowPlayingProperties {
                    identity: entry.string("Identity"),
                    status: entry.status()?,
                    position_us: entry.position_us,
                    can_play: entry.flag("CanPlay"),
                    can_pause: entry.flag("CanPause"),
                    can_seek: entry.flag("CanSeek"),
                    can_go_next: entry.flag("CanGoNext"),
                    can_go_previous: entry.flag("CanGoPrevious"),
                };
                now_playing_from_properties(
                    bus_name,
                    &properties,
                    &entry.metadata(),
                    entry.observed_at,
                )
            })
            .take(MAX_NOW_PLAYING_PLAYERS)
            .collect()
    }

    /// Handoff reports for launcher-verified players only, each playhead
    /// extrapolated to `now`.
    pub(super) fn handoff_reports(
        &self,
        apps: &[InstalledMediaApp],
        now: Instant,
    ) -> Vec<PlaybackReport> {
        self.players
            .iter()
            .filter_map(|(bus_name, entry)| {
                let observation = PlayerObservation {
                    bus_name: bus_name.clone(),
                    package_id: entry.package_id.clone()?,
                    identity: entry.string("Identity"),
                    status: entry.status()?,
                    metadata: entry.metadata(),
                    position_us: entry.position_at(now),
                };
                report_from_observation(&observation, apps)
            })
            .take(media::handoff::MAX_PLAYBACK_REPORTS)
            .collect()
    }

    /// The player the transport keys address, and whether it is playing:
    /// the first one playing, else the first one, so play/pause resumes
    /// whatever was last open.
    pub(super) fn active_player(&self) -> Option<(String, bool)> {
        self.players
            .iter()
            .find(|(_, entry)| entry.is_playing())
            .or_else(|| self.players.iter().next())
            .map(|(bus_name, entry)| (bus_name.clone(), entry.is_playing()))
    }

    /// Every player playing right now.
    pub(super) fn playing(&self) -> Vec<String> {
        self.players
            .iter()
            .filter(|(_, entry)| entry.is_playing())
            .map(|(bus_name, _)| bus_name.clone())
            .collect()
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

    /// Players whose owner was not yet attributable to a launch when it
    /// appeared, with the process to try again.
    pub(super) fn unverified(&self) -> Vec<(String, String, u32)> {
        self.players
            .iter()
            .filter(|(_, entry)| entry.package_id.is_none())
            .filter_map(|(bus_name, entry)| {
                Some((bus_name.clone(), entry.unique_name.clone(), entry.pid?))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::now_playing::PlaybackStatus;
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

    fn browser() -> InstalledMediaApp {
        InstalledMediaApp {
            package_id: "so.tilo.browser".into(),
            uri_schemes: vec!["http".into(), "https".into()],
            extensions: vec!["mp4".into()],
        }
    }

    #[test]
    fn appear_change_vanish_updates_both_projections() {
        let now = Instant::now();
        let mut table = PlayerTable::default();
        let applied = table.apply(
            appeared(PLAYER, OWNER, Some("so.tilo.browser"), "Playing"),
            now,
        );
        assert!(applied.changed);

        let players = table.now_playing();
        assert_eq!(players.len(), 1);
        assert_eq!(players[0].0.bus_name, PLAYER);
        assert_eq!(players[0].0.status, PlaybackStatus::Playing);
        let reports = table.handoff_reports(&[browser()], now);
        assert_eq!(reports.len(), 1);
        assert!(reports[0].session.playing);
        assert_eq!(reports[0].session.position_ms, 4_000);

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
        assert_eq!(table.now_playing()[0].0.status, PlaybackStatus::Paused);
        assert!(!table.handoff_reports(&[browser()], now)[0].session.playing);

        let elsewhere = table.apply(
            changed(":1.99", "PlaybackStatus", value("Playing".to_string())),
            now,
        );
        assert!(
            !elsewhere.changed,
            "a connection may only change its own players"
        );
        assert_eq!(table.now_playing()[0].0.status, PlaybackStatus::Paused);

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
        assert!(table.now_playing().is_empty());
        assert!(table.handoff_reports(&[browser()], now).is_empty());
    }

    #[test]
    fn position_extrapolates_and_seeked_resets() {
        let start = Instant::now();
        let mut table = PlayerTable::default();
        table.apply(
            appeared(PLAYER, OWNER, Some("so.tilo.browser"), "Playing"),
            start,
        );
        let later = start + Duration::from_secs(2);
        assert_eq!(
            table.handoff_reports(&[browser()], later)[0]
                .session
                .position_ms,
            6_000,
            "a playing track advances between signals"
        );

        table.apply(
            PlayerEvent::Seeked {
                unique_name: OWNER.into(),
                position_us: 1_000_000,
            },
            later,
        );
        let snapshot = &table.now_playing()[0].0;
        assert_eq!(snapshot.position_at(later), 1_000_000);
        assert_eq!(
            snapshot.position_at(later + Duration::from_secs(1)),
            2_000_000
        );

        table.apply(changed(OWNER, "Rate", value(2.0_f64)), later);
        assert_eq!(
            table.handoff_reports(&[browser()], later + Duration::from_secs(1))[0]
                .session
                .position_ms,
            3_000,
            "the playhead advances at the signalled rate"
        );

        table.apply(
            changed(OWNER, "PlaybackStatus", value("Paused".to_string())),
            later + Duration::from_secs(1),
        );
        assert_eq!(
            table.handoff_reports(&[browser()], later + Duration::from_secs(60))[0]
                .session
                .position_ms,
            3_000,
            "pausing freezes the playhead where it was"
        );
        assert_eq!(
            table.handoff_reports(&[browser()], later + Duration::from_secs(60))[0]
                .session
                .duration_ms,
            Some(9_000)
        );
    }

    #[test]
    fn unverified_player_never_yields_handoff_report() {
        let now = Instant::now();
        let mut table = PlayerTable::default();
        table.apply(appeared(PLAYER, OWNER, None, "Playing"), now);
        assert!(table.handoff_reports(&[browser()], now).is_empty());
        assert_eq!(table.now_playing().len(), 1);
        assert!(table.players_of("so.tilo.browser").is_empty());
        assert_eq!(
            table.unverified(),
            [(PLAYER.to_string(), OWNER.to_string(), 4242)]
        );

        assert!(
            !table.verify(PLAYER, ":1.7", "so.tilo.browser".into()),
            "a verification for an earlier owner is discarded"
        );
        assert!(table.verify(PLAYER, OWNER, "so.tilo.browser".into()));
        assert_eq!(table.handoff_reports(&[browser()], now).len(), 1);
        assert_eq!(
            table.players_of("so.tilo.browser"),
            [(
                PLAYER.to_string(),
                Some("https://media.example/film.mp4".to_string())
            )]
        );

        table.apply(appeared(PLAYER, ":1.43", None, "Playing"), now);
        assert!(
            table.handoff_reports(&[browser()], now).is_empty(),
            "a new owner of the same name is verified afresh"
        );
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
        assert_eq!(table.playing(), ["org.mpris.MediaPlayer2.b".to_string()]);
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
        assert_eq!(table.now_playing().len(), MAX_NOW_PLAYING_PLAYERS);
        assert!(table.admits(&format!("{MPRIS_PREFIX}p000")));
        assert!(!table.admits(&format!("{MPRIS_PREFIX}late")));
    }
}
