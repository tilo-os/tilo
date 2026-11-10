//! What the machine says of itself: the connection its traffic leaves on, how loud its sound is,
//! which applications use the camera or the microphone, what plays, and its charge.
//!
//! One reading for every surface that shows the machine: the bar's status cluster, the lock screen
//! under its card, and the desk's reading. The host reads it ([`crate::platform::readings`]), so no
//! two surfaces can disagree about it; the loop reads it again when the host says it moved
//! ([`Readings::follow`]), and a frame only copies what was read.

use tilo_desktop::chrome::{LinkKind, LinkStatus, SystemStatus, VolumeState};
use tilo_desktop_surface::now_playing::NowPlaying;

use crate::i18n::{t, t_args};
use crate::platform::readings::{Handover, Machine, Reading};

/// A charge at or below this percentage is low while the pack drains: the power policy's own
/// default threshold, which the bar draws as a warning.
const LOW_PERCENT: u8 = 20;

/// A charge at or below this percentage is critical while the pack drains.
const CRITICAL_PERCENT: u8 = 5;

/// The machine as its host last read it, kept between passes so a frame reads nothing.
#[derive(Clone, Default)]
pub struct Readings {
    /// The host's revision these were read at; `None` before the first pass that read them.
    revision: Option<u64>,
    reading: Reading,
}

/// The machine as the bar, the lock screen and the desk read it, shared between the loop that reads
/// it and the surfaces that show it.
pub(crate) type Shared = std::rc::Rc<std::cell::RefCell<Readings>>;

#[cfg(target_os = "linux")]
impl crate::Shared {
    /// Read the machine off the services again when they published something since the last pass:
    /// the loop passes every reading the surfaces show on, so no frame of theirs reads a service.
    pub(crate) fn follow_status(&self) {
        let services = self.services.borrow();
        self.readings.borrow_mut().follow(&*services);
        drop(services);
        // What plays reaches the desk from the same reading every other surface shows, so the
        // widget and the panel never disagree and neither reads the players again.
        let playing = self.readings.borrow().playing();
        self.music.set(playing);
    }
}

impl Readings {
    /// Read the machine again when its host says it moved since the last pass; a pass where
    /// nothing moved reads nothing.
    pub fn follow(&mut self, machine: &dyn Machine) {
        let revision = machine.revision();
        if self.revision == Some(revision) {
            return;
        }
        self.revision = Some(revision);
        self.reading = machine.read();
    }

    /// What plays now, as every surface shows it; `None` while no player holds anything and none
    /// stands paused.
    #[must_use]
    pub fn playing(&self) -> Option<NowPlaying> {
        self.reading.media.as_ref()?.playing()
    }

    /// What the front player names of itself, when another of the person's devices may continue it:
    /// the session and the source it plays.
    #[must_use]
    pub fn handover(&self) -> Option<Handover> {
        self.reading.media.as_ref()?.handover()
    }

    /// The connection this machine's traffic leaves on, in the bar's terms: `None` while the host
    /// did not read it, which draws no glyph at all, as offline is its own glyph.
    #[must_use]
    pub fn link(&self) -> Option<LinkStatus> {
        self.reading.link.clone()
    }

    /// Whether this machine is on a network now: what a join that waited for one is asked again on.
    #[must_use]
    pub fn online(&self) -> bool {
        self.reading
            .link
            .as_ref()
            .is_some_and(|link| link.kind != LinkKind::Offline)
    }

    /// How full the pack is, and whether it fills, when this machine has a battery and its host
    /// read it.
    #[must_use]
    pub fn charge(&self) -> Option<(u8, bool)> {
        self.reading
            .battery
            .map(|battery| (battery.percent, battery.charging))
    }

    /// The bar's status: the machine in the terms its status cluster draws,
    /// field for field, each part only when its host read it.
    #[must_use]
    pub fn status(&self) -> SystemStatus {
        let draining_at = |threshold| {
            self.reading
                .battery
                .is_some_and(|battery| battery.draining && battery.percent <= threshold)
        };
        SystemStatus {
            battery_percent: self.charge().map(|(percent, _)| percent),
            battery_charging: self.charge().is_some_and(|(_, charging)| charging),
            battery_low: draining_at(LOW_PERCENT),
            battery_critical: draining_at(CRITICAL_PERCENT),
            link: self.link(),
            volume: self.volume(),
            media: self.reading.uses.clone(),
            summary: self.summary(),
        }
    }

    /// The output's level and mute, when its host read it.
    #[must_use]
    pub fn volume(&self) -> Option<VolumeState> {
        self.reading.volume
    }

    /// The status in a line, for a reader that hears it rather than sees a glyph: the link, the
    /// level and the charge, as far as they were read.
    fn summary(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(link) = self.link() {
            parts.push(link_reading(&link));
        }
        if let Some(volume) = self.volume() {
            parts.push(if volume.muted {
                t("status-muted")
            } else {
                t_args(
                    "status-volume",
                    &[("percent", volume_percent(volume.level).into())],
                )
            });
        }
        if let Some((percent, charging)) = self.charge() {
            parts.push(if charging {
                t_args("status-battery-charging", &[("percent", percent.into())])
            } else {
                t_args("status-battery", &[("percent", percent.into())])
            });
        }
        parts.join(", ")
    }
}

/// The connection in the words a reader hears it: what kind it is, and its name when it has one.
fn link_reading(link: &LinkStatus) -> String {
    match link.kind {
        LinkKind::Wired => link.name.as_deref().map_or_else(
            || t("status-wired"),
            |name| t_args("status-wired-named", &[("name", name.into())]),
        ),
        LinkKind::Wifi => link.name.as_deref().map_or_else(
            || t("status-wifi"),
            |name| t_args("status-wifi-named", &[("name", name.into())]),
        ),
        LinkKind::Offline => t("status-offline"),
    }
}

/// `level` is nominally `0.0..=1.0` , but a host reads it from the sound graph and does not
/// validate it against that range: clamp defensively.
fn volume_percent(level: f32) -> u32 {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 0.0..=100.0 immediately above, so the cast neither truncates nor loses sign"
    )]
    let percent = (level * 100.0).round().clamp(0.0, 100.0) as u32;
    percent
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    use std::time::Instant;

    use super::*;
    use tilo_audio_service::media::Media;
    use tilo_audio_service::sound::{Sound, Use, UseKind};
    use tilo_connectivity_service::model::{Link, LinkKind, Network};
    use tilo_desktop::chrome::{LinkKind as BarLinkKind, MediaKind};
    use tilo_power_mgmt::BatteryState;
    use tilo_session_power::Battery;

    /// What the services published, as a test sets it.
    #[derive(Default)]
    struct Read {
        network: Option<Network>,
        sound: Option<Sound>,
        battery: Option<Battery>,
        media: Option<Media>,
        media_at: Option<Instant>,
    }

    impl Read {
        /// The machine as the session reads it off these.
        fn readings(self) -> Readings {
            Readings {
                revision: None,
                reading: crate::platform::readings::reading(
                    self.network,
                    self.sound,
                    self.battery,
                    self.media,
                    self.media_at.unwrap_or_else(Instant::now),
                ),
            }
        }
    }

    fn wifi() -> Network {
        Network {
            link: Some(Link {
                kind: LinkKind::Wifi,
                name: Some("Home".to_owned()),
                signal: 3,
            }),
            ..Network::default()
        }
    }

    fn sound() -> Sound {
        Sound {
            volume: Some(0.6),
            uses: vec![Use {
                client_id: 7,
                label: "Meet".to_owned(),
                kind: UseKind::Microphone,
                ..Use::default()
            }],
            ..Sound::default()
        }
    }

    fn charging() -> Battery {
        Battery {
            present: true,
            percentage: 42,
            state: BatteryState::Charging,
            on_battery: false,
            minutes_left: None,
            charge_limit: None,
            notice: None,
        }
    }

    /// What the audio service publishes of a player, as the session reads it.
    fn media(players: &serde_json::Value) -> Media {
        serde_json::from_value(serde_json::json!({ "players": players })).expect("a media reading")
    }

    /// The bar's glyphs: the battery's charge, the network's name and strength, the volume's level,
    /// and the stream being captured.
    #[test]
    fn the_bar_reads_the_machines_battery_network_and_volume() {
        let readings = Read {
            network: Some(wifi()),
            sound: Some(sound()),
            battery: Some(charging()),
            ..Read::default()
        }
        .readings();
        let status = readings.status();
        assert_eq!(status.battery_percent(), Some(42));
        assert!(status.battery_is_charging());
        assert!(!status.battery_is_low());
        let link = status.link.clone().expect("a link");
        assert_eq!(link.kind, BarLinkKind::Wifi);
        assert_eq!(link.name.as_deref(), Some("Home"));
        assert_eq!(link.signal, 3);
        let volume = status.volume.expect("a level");
        assert!((volume.level - 0.6).abs() < f32::EPSILON);
        assert!(!volume.muted);
        assert!(!status.audio_is_silent());
        assert_eq!(status.media.len(), 1);
        assert_eq!(status.media[0].client_id, 7);
        assert_eq!(status.media[0].label, "Meet");
        assert_eq!(status.media[0].kind, MediaKind::Microphone);
        assert_eq!(
            status.summary(),
            "Wi-Fi: Home, Volume 60%, Battery 42%, charging"
        );
    }

    /// A machine no service has read yet draws no glyph: absence, never a zero battery or a full
    /// network.
    #[test]
    fn a_machine_with_no_readings_shows_no_glyph() {
        let status = Readings::default().status();
        assert_eq!(status.battery_percent(), None);
        assert!(status.link.is_none());
        assert!(status.volume.is_none());
        assert!(status.media.is_empty());
        assert!(
            status.summary.is_empty(),
            "the desktop says nothing was read"
        );
        assert_eq!(status.summary(), "System status");
    }

    /// A charge only warns while the pack drains, and a machine with no battery draws no gauge at
    /// all.
    #[test]
    fn a_charge_warns_only_while_the_pack_drains() {
        let draining = |percentage| {
            Read {
                battery: Some(Battery {
                    state: BatteryState::Discharging,
                    percentage,
                    ..charging()
                }),
                ..Read::default()
            }
            .readings()
        };
        let low = draining(12).status();
        assert!(low.battery_is_low());
        assert!(!low.battery_is_critical());
        let critical = draining(3).status();
        assert!(critical.battery_is_critical());
        assert!(
            critical.battery_is_low(),
            "critical never skips the low styling"
        );
        let filling = Read {
            battery: Some(charging()),
            ..Read::default()
        }
        .readings();
        assert!(filling.status().battery_is_charging());
        let desktop = Read {
            battery: Some(Battery {
                present: false,
                percentage: 100,
                ..charging()
            }),
            ..Read::default()
        }
        .readings();
        assert_eq!(
            desktop.status().battery_percent(),
            None,
            "a machine without a pack shows no gauge"
        );
        assert_eq!(desktop.charge(), None);
    }

    /// The connection the traffic leaves on is the one the connectivity service read, and a reading
    /// published before it said so draws no network glyph rather than claiming offline.
    #[test]
    fn the_network_glyph_is_the_connection_the_traffic_leaves_on() {
        let offline = Read {
            network: Some(Network {
                link: Some(Link {
                    kind: LinkKind::Offline,
                    ..Link::default()
                }),
                ..Network::default()
            }),
            ..Read::default()
        }
        .readings();
        let link = offline.link().expect("a link");
        assert_eq!(link.kind, BarLinkKind::Offline);
        assert_eq!(link.name, None);

        let older = Read {
            network: Some(Network::default()),
            ..Read::default()
        }
        .readings();
        assert!(older.link().is_none());
    }

    /// The lock screen names the network the person is on and the charge of a pack this machine
    /// has.
    #[test]
    fn the_lock_screen_reads_the_network_and_the_charge() {
        let readings = Read {
            network: Some(wifi()),
            battery: Some(charging()),
            ..Read::default()
        }
        .readings();
        assert_eq!(
            readings.link().and_then(|link| link.name).as_deref(),
            Some("Home")
        );
        assert_eq!(readings.charge(), Some((42, true)));
    }

    /// What plays is the player the panel's own controls reach: the one playing, else the one
    /// paused, else the first — and only while one of them holds something.
    #[test]
    fn what_plays_is_the_player_the_panels_controls_reach() {
        let stopped = Read {
            media: Some(media(&serde_json::json!([
                { "identity": "Music", "title": "Tea", "can-play": true }
            ]))),
            ..Read::default()
        }
        .readings();
        assert!(
            stopped.playing().is_none(),
            "a player holding nothing is not what plays"
        );
        assert!(stopped.handover().is_none());

        let playing = Read {
            media: Some(media(&serde_json::json!([
                { "identity": "Firefox", "title": "A talk", "can-play": true },
                {
                    "identity": "Music",
                    "title": "Tea",
                    "artist": "Kettle",
                    "playing": true,
                    "can-pause": true,
                    "can-next": true,
                    "url": "https://music.example/tea",
                    "app-id": "so.tilo.music",
                    "session-id": "0123456789abcdef0123456789abcdef"
                }
            ]))),
            ..Read::default()
        }
        .readings();
        let now = playing.playing().expect("what plays");
        assert_eq!(now.title, "Tea");
        assert_eq!(now.subtitle, "Kettle");
        assert!(now.playing);
        assert!(now.can_toggle, "the middle control pauses what plays");
        assert!(now.can_next);
        assert!(!now.can_previous);
        // The desk's widget holds the same reading, so the song, who plays it and where it stands
        // are the panel's, not a copy of them.
        let widget = tilo_desktop::desk_widgets::music::DeskMusic::default();
        widget.set(playing.playing());
        assert_eq!(widget.get(), Some(now));
        let handover = playing.handover().expect("another device may continue it");
        assert_eq!(handover.title, "Tea");
        assert_eq!(handover.app_id, "so.tilo.music");
    }

    /// A hand-over carries the playhead to where it stands now, not where the service last
    /// published it: the other device resumes the song where the person left it. A paused playhead
    /// stands still.
    #[test]
    #[expect(
        clippy::cast_precision_loss,
        reason = "a playhead in a test stands far inside f64's exact integers"
    )]
    fn a_hand_over_resumes_where_the_playhead_stands_now() {
        let stream = |playing: bool| {
            media(&serde_json::json!([{
                "identity": "Music",
                "title": "Tea",
                "playing": playing,
                "url": "https://music.example/tea",
                "app-id": "so.tilo.music",
                "session-id": "0123456789abcdef0123456789abcdef",
                "position-ms": 72_000,
                "duration-ms": 300_000,
                "rate": 1.0
            }]))
        };
        let at = Instant::now();
        let played = Read {
            media: Some(stream(true)),
            media_at: at.checked_sub(std::time::Duration::from_secs(3)),
            ..Read::default()
        }
        .readings();
        let handover = played.handover().expect("a hand-over");
        let since = at.elapsed().as_secs_f64();
        let expected = 72_000.0 + (3.0 + since) * 1_000.0;
        assert!(
            (expected - 5.0..=expected + 5.0).contains(&(handover.position_ms as f64)),
            "the playhead moved with the clock: {} against {expected}",
            handover.position_ms
        );

        let paused = Read {
            media: Some(stream(false)),
            media_at: Some(at),
            ..Read::default()
        }
        .readings();
        assert_eq!(
            paused.handover().expect("a hand-over").position_ms,
            72_000,
            "a paused playhead does not advance"
        );
    }
}
