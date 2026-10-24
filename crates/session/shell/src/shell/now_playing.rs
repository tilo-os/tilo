//! What is playing on this machine, and what a person may do about it.
//!
//! The model behind the now-playing card in Quick Settings and on the lock
//! screen. Players are observed over MPRIS ([`crate::shell::mpris`]) off the
//! compositor thread; this module only holds the last snapshot, decides which
//! player the card shows and which outputs it offers, and turns a button into
//! a [`PlayerCommand`]. Everything here is pure so the card can be exercised
//! against a fake [`NowPlayingSource`].

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::shell::overlay_host::{DeviceFabricUiAction, DeviceFabricView};

/// A player's transport state as MPRIS reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackStatus {
    Playing,
    Paused,
    Stopped,
}

impl PlaybackStatus {
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

/// Decoded cover art, already scaled down on a worker. Only artwork a player
/// names as a local file is ever decoded: the shell never fetches a remote
/// URL on a player's behalf.
#[derive(Clone, Debug)]
pub struct Artwork {
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<Vec<u8>>,
}

impl Artwork {
    /// The artwork as a picture to paint.
    #[must_use]
    pub fn image(&self) -> vello::peniko::ImageData {
        ui::raster::rgba8(
            vello::peniko::Blob::new(self.rgba.clone()),
            self.width,
            self.height,
        )
    }
}

impl PartialEq for Artwork {
    fn eq(&self, other: &Self) -> bool {
        self.width == other.width
            && self.height == other.height
            && Arc::ptr_eq(&self.rgba, &other.rgba)
    }
}

/// One player as last observed.
#[derive(Clone, Debug, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each MPRIS capability is an independent property the player reports"
)]
pub struct PlayerSnapshot {
    /// The player's MPRIS bus name: the address commands go to. Never shown.
    pub bus_name: String,
    /// The application's own name for itself (`Identity`).
    pub identity: String,
    pub status: PlaybackStatus,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub artwork: Option<Artwork>,
    /// `mpris:trackid`, required to seek with `SetPosition`.
    pub track_id: Option<String>,
    pub length_us: Option<i64>,
    pub position_us: i64,
    pub can_play: bool,
    pub can_pause: bool,
    pub can_seek: bool,
    pub can_go_next: bool,
    pub can_go_previous: bool,
    /// When `position_us` was read, so a playing track's position advances
    /// between observations without another bus round trip.
    pub observed_at: Instant,
}

impl PlayerSnapshot {
    /// The playhead at `now`, extrapolated while playing and bounded by the
    /// track length.
    #[must_use]
    pub fn position_at(&self, now: Instant) -> i64 {
        let base = self.position_us.max(0);
        let position = if self.status == PlaybackStatus::Playing {
            let elapsed = now.saturating_duration_since(self.observed_at);
            base.saturating_add(i64::try_from(elapsed.as_micros()).unwrap_or(i64::MAX))
        } else {
            base
        };
        match self.length_us {
            Some(length) if length > 0 => position.min(length),
            _ => position,
        }
    }

    /// Whether the play/pause button does anything right now.
    #[must_use]
    pub fn can_toggle(&self) -> bool {
        if self.status == PlaybackStatus::Playing {
            self.can_pause
        } else {
            self.can_play
        }
    }

    /// A seek bar is offered only when the player can seek to an absolute
    /// position: it must say so, know the track length and name the track.
    #[must_use]
    pub fn seekable(&self) -> bool {
        self.can_seek && self.length_us.is_some_and(|length| length > 0) && self.track_id.is_some()
    }

    /// The card's headline: the track title, else the application.
    #[must_use]
    pub fn headline(&self) -> &str {
        self.title
            .as_deref()
            .filter(|title| !title.trim().is_empty())
            .unwrap_or(&self.identity)
    }

    /// The line under the headline: the artist, else the application when
    /// the headline is a track title.
    #[must_use]
    pub fn subline(&self) -> Option<&str> {
        self.artist
            .as_deref()
            .filter(|artist| !artist.trim().is_empty())
            .or_else(|| (self.headline() != self.identity).then_some(self.identity.as_str()))
    }

    /// The song as every player surface paints it ([`ui::widget::SongFace`]):
    /// the headline over the artist, the application under them when it is
    /// not already one of them, and `image` (the decoded artwork) or the
    /// placeholder cover seeded with the application's name.
    #[must_use]
    pub fn song(&self, image: Option<vello::peniko::ImageData>) -> ui::widget::Song {
        let subtitle = self.subline().unwrap_or_default().to_owned();
        let detail = if subtitle != self.identity && self.headline() != self.identity {
            self.identity.clone()
        } else {
            String::new()
        };
        ui::widget::Song {
            title: self.headline().to_owned(),
            subtitle,
            detail,
            error: false,
            art: image.map_or(ui::widget::TransportArt::Placeholder, |image| {
                ui::widget::TransportArt::Image(image)
            }),
            art_name: self.identity.clone(),
        }
    }
}

/// What a button asks a player to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlayerCommand {
    PlayPause,
    Pause,
    Next,
    Previous,
    SetPosition { track_id: String, position_us: i64 },
}

/// Where players come from and where commands go. Production reads the
/// shell's MPRIS projection; tests supply a fake.
pub trait NowPlayingSource {
    /// The players last observed, in bus order.
    fn players(&self) -> Vec<PlayerSnapshot>;
    /// Send `command` to exactly the player at `bus_name`. `false` when it
    /// could not be dispatched.
    fn send(&self, bus_name: &str, command: PlayerCommand) -> bool;
    /// Ask for a fresh observation. Never blocks.
    fn refresh(&self) {}
}

/// The session's players, over MPRIS.
#[derive(Clone, Copy, Debug, Default)]
pub struct MprisSource;

impl NowPlayingSource for MprisSource {
    fn players(&self) -> Vec<PlayerSnapshot> {
        crate::shell::mpris::now_playing_players()
    }

    fn send(&self, bus_name: &str, command: PlayerCommand) -> bool {
        crate::shell::mpris::control(bus_name, command)
    }

    fn refresh(&self) {
        crate::shell::mpris::refresh_now_playing();
    }
}

/// A session with no media players: hosts that must not observe the real
/// session bus (fixtures, screenshots) start from this.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoPlayers;

impl NowPlayingSource for NoPlayers {
    fn players(&self) -> Vec<PlayerSnapshot> {
        Vec::new()
    }

    fn send(&self, _bus_name: &str, _command: PlayerCommand) -> bool {
        false
    }
}

/// Players fixed when made, taking the commands sent to them: what a
/// scenario capture shows and drives without a session bus. A play/pause
/// flips the player's status, so the next frame shows what was pressed.
#[cfg(feature = "hosted-test-control")]
#[derive(Debug, Default)]
pub struct FixturePlayers {
    players: std::cell::RefCell<Vec<PlayerSnapshot>>,
    /// Every command sent, oldest first.
    pub sent: std::cell::RefCell<Vec<(String, PlayerCommand)>>,
}

#[cfg(feature = "hosted-test-control")]
impl FixturePlayers {
    #[must_use]
    pub fn new(players: Vec<PlayerSnapshot>) -> Self {
        Self {
            players: std::cell::RefCell::new(players),
            sent: std::cell::RefCell::default(),
        }
    }
}

#[cfg(feature = "hosted-test-control")]
impl NowPlayingSource for FixturePlayers {
    fn players(&self) -> Vec<PlayerSnapshot> {
        self.players.borrow().clone()
    }

    fn send(&self, bus_name: &str, command: PlayerCommand) -> bool {
        if command == PlayerCommand::PlayPause
            && let Some(player) = self
                .players
                .borrow_mut()
                .iter_mut()
                .find(|player| player.bus_name == bus_name)
        {
            player.status = if player.status == PlaybackStatus::Playing {
                PlaybackStatus::Paused
            } else {
                PlaybackStatus::Playing
            };
        }
        self.sent.borrow_mut().push((bus_name.to_string(), command));
        true
    }
}

/// The player the card shows: the one the person picked while it is still
/// around, else whatever is playing, else whatever is paused, else the first.
#[must_use]
pub fn active_index(players: &[PlayerSnapshot], preferred: Option<&str>) -> Option<usize> {
    preferred
        .and_then(|bus| players.iter().position(|player| player.bus_name == bus))
        .or_else(|| {
            players
                .iter()
                .position(|player| player.status == PlaybackStatus::Playing)
        })
        .or_else(|| {
            players
                .iter()
                .position(|player| player.status == PlaybackStatus::Paused)
        })
        .or_else(|| (!players.is_empty()).then_some(0))
}

/// What kind of place an output is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputKind {
    /// A speaker or headset on this machine.
    Local,
    /// One of the person's certified devices.
    Device,
    /// One of the person's speaker groups: the sound keeps playing here and
    /// every ready speaker of the group plays it in sync.
    SpeakerGroup,
}

/// One destination in the output picker. `token` is the destination's
/// semantic action id in the continuity provider — the same name the palette,
/// the control socket and agents use — so the card never holds a `PipeWire`
/// node id or a device identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputTarget {
    pub token: String,
    pub label: String,
    pub kind: OutputKind,
    /// Where sound goes now.
    pub current: bool,
}

/// The fabric media session the player at `player_bus` is publishing, if it
/// is portable at all. `controller_of` names the MPRIS bus behind a session.
fn player_session<'a>(
    view: &'a DeviceFabricView,
    player_bus: Option<&str>,
    controller_of: &dyn Fn(&str) -> Option<String>,
) -> Option<&'a crate::shell::overlay_host::FabricMediaView> {
    let player_bus = player_bus?;
    view.local_media
        .iter()
        .find(|media| controller_of(&media.session_id).as_deref() == Some(player_bus))
}

/// Every place the shown player's sound can go: this machine's outputs, then
/// the person's devices this session can move to, then the speaker groups
/// with at least one ready speaker it can play on.
#[must_use]
pub fn output_targets(
    view: &DeviceFabricView,
    player_bus: Option<&str>,
    controller_of: &dyn Fn(&str) -> Option<String>,
) -> Vec<OutputTarget> {
    let mut targets = view
        .local_outputs
        .iter()
        .map(|output| OutputTarget {
            token: crate::shell::search::providers::continuity::audio_route_action_id(
                output.node_id,
            ),
            label: output.label.clone(),
            kind: OutputKind::Local,
            current: output.is_default,
        })
        .collect::<Vec<_>>();
    if let Some(media) = player_session(view, player_bus, controller_of) {
        targets.extend(media.targets.iter().map(|target| {
            OutputTarget {
                token: crate::shell::search::providers::continuity::media_handoff_action_id(
                    &media.session_id,
                    target,
                ),
                label: view
                    .devices
                    .iter()
                    .find(|device| &device.id == target)
                    .map_or_else(
                        || target.as_str().to_string(),
                        |device| device.label.clone(),
                    ),
                kind: OutputKind::Device,
                current: false,
            }
        }));
        targets.extend(
            view.speaker_groups
                .groups
                .iter()
                .filter(|group| group.ready_members() > 0)
                .map(|group| OutputTarget {
                    token:
                        crate::shell::search::providers::continuity::speaker_group_media_action_id(
                            &media.session_id,
                            group.id,
                        ),
                    label: group.name.clone(),
                    kind: OutputKind::SpeakerGroup,
                    current: false,
                }),
        );
    }
    targets
}

/// Resolve a picked token against a fresh fabric snapshot. `None` when that
/// destination is gone or is already where sound goes, so a stale picker can
/// never route to something that is no longer offered.
#[must_use]
pub fn resolve_output(
    view: &DeviceFabricView,
    token: &str,
    player_bus: Option<&str>,
    controller_of: &dyn Fn(&str) -> Option<String>,
) -> Option<DeviceFabricUiAction> {
    if let Some(output) = view.local_outputs.iter().find(|output| {
        crate::shell::search::providers::continuity::audio_route_action_id(output.node_id) == token
    }) {
        return (!output.is_default).then_some(DeviceFabricUiAction::RouteLocalAudio {
            node_id: output.node_id,
        });
    }
    let media = player_session(view, player_bus, controller_of)?;
    if let Some(target) = media.targets.iter().find(|target| {
        crate::shell::search::providers::continuity::media_handoff_action_id(
            &media.session_id,
            target,
        ) == token
    }) {
        return Some(DeviceFabricUiAction::HandoffMedia {
            session_id: media.session_id.clone(),
            target: target.clone(),
        });
    }
    view.speaker_groups
        .groups
        .iter()
        .filter(|group| group.ready_members() > 0)
        .find(|group| {
            crate::shell::search::providers::continuity::speaker_group_media_action_id(
                &media.session_id,
                group.id,
            ) == token
        })
        .map(|group| DeviceFabricUiAction::PlayMediaOnSpeakerGroup {
            session_id: media.session_id.clone(),
            group: group.id,
        })
}

/// A speaker group this device is playing to, as the card reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpeakerPlayback {
    pub group: String,
    pub status: crate::shell::speaker_group_runtime::SpeakerSessionStatus,
}

impl SpeakerPlayback {
    /// The session this device plays to a group, most urgent first: a
    /// member that cannot play in time outranks one still syncing.
    #[must_use]
    pub fn from_view(view: &DeviceFabricView) -> Option<Self> {
        use crate::shell::speaker_group_runtime::SpeakerSessionStatus;
        let urgency = |status: SpeakerSessionStatus| match status {
            SpeakerSessionStatus::ClockFailed(_) => 0,
            SpeakerSessionStatus::SyncingClocks => 1,
            SpeakerSessionStatus::Connecting => 2,
            SpeakerSessionStatus::Playing => 3,
        };
        view.speaker_groups
            .sessions
            .iter()
            .filter(|session| !session.inbound)
            .map(|session| Self {
                group: session.name.clone(),
                status: session.status(),
            })
            .min_by_key(|playback| urgency(playback.status))
    }

    /// Failures read as errors, everything else as a caption.
    #[must_use]
    pub const fn failed(&self) -> bool {
        matches!(
            self.status,
            crate::shell::speaker_group_runtime::SpeakerSessionStatus::ClockFailed(_)
        )
    }

    /// The line the card shows.
    #[must_use]
    pub fn text(&self) -> String {
        use crate::shell::i18n::{t, t_args};
        use crate::shell::speaker_group_runtime::SpeakerSessionStatus;
        let group = || ("group", self.group.as_str().into());
        match self.status {
            SpeakerSessionStatus::Playing => t_args("now-playing-speakers-playing", &[group()]),
            SpeakerSessionStatus::Connecting => {
                t_args("now-playing-speakers-connecting", &[group()])
            }
            SpeakerSessionStatus::SyncingClocks => {
                t_args("now-playing-speakers-syncing", &[group()])
            }
            SpeakerSessionStatus::ClockFailed(reason) => t_args(
                "now-playing-speakers-clock-failed",
                &[group(), ("reason", t(reason.message_key()).into())],
            ),
        }
    }
}

/// Everything the card paints for one frame.
#[derive(Clone, Debug, PartialEq)]
pub struct NowPlayingView {
    pub players: Vec<PlayerSnapshot>,
    pub active: usize,
    /// The player at `active`, held apart so it is always present.
    shown: PlayerSnapshot,
    pub outputs: Vec<OutputTarget>,
    /// A speaker group this device plays to, shown under the track.
    pub speakers: Option<SpeakerPlayback>,
    pub now: Instant,
}

impl NowPlayingView {
    /// `None` when nothing is playing or paused: the card is not shown for a
    /// machine without media.
    #[must_use]
    pub fn new(
        players: Vec<PlayerSnapshot>,
        preferred: Option<&str>,
        outputs: Vec<OutputTarget>,
        now: Instant,
    ) -> Option<Self> {
        let active = active_index(&players, preferred)?;
        let shown = players.get(active)?.clone();
        Some(Self {
            players,
            active,
            shown,
            outputs,
            speakers: None,
            now,
        })
    }

    /// Report a speaker group this device is playing to.
    #[must_use]
    pub fn with_speakers(mut self, speakers: Option<SpeakerPlayback>) -> Self {
        self.speakers = speakers;
        self
    }

    #[must_use]
    pub fn player(&self) -> &PlayerSnapshot {
        &self.shown
    }

    /// Several players are running, so the card offers a picker.
    #[must_use]
    pub fn has_player_choice(&self) -> bool {
        self.players.len() > 1
    }

    /// Fraction of the track played at `now`, when the length is known.
    #[must_use]
    pub fn progress(&self) -> Option<f64> {
        let player = self.player();
        let length = player.length_us.filter(|length| *length > 0)?;
        #[allow(
            clippy::cast_precision_loss,
            reason = "a playhead fraction needs no more than f64's 52-bit mantissa"
        )]
        Some((player.position_at(self.now) as f64 / length as f64).clamp(0.0, 1.0))
    }

    /// The command that seeks the shown player to `fraction` of its track.
    #[must_use]
    pub fn seek_command(&self, fraction: f64) -> Option<PlayerCommand> {
        let player = self.player();
        if !player.seekable() {
            return None;
        }
        let length = player.length_us?;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_precision_loss,
            reason = "the product is clamped to the track length, which is an i64"
        )]
        let position_us = ((length as f64) * fraction.clamp(0.0, 1.0)).round() as i64;
        Some(PlayerCommand::SetPosition {
            track_id: player.track_id.clone()?,
            position_us: position_us.clamp(0, length),
        })
    }

    /// The command that moves the playhead by `delta` from where it is now.
    #[must_use]
    pub fn step_command(&self, delta: Duration, forward: bool) -> Option<PlayerCommand> {
        let player = self.player();
        let length = player.length_us?;
        let delta = i64::try_from(delta.as_micros()).unwrap_or(i64::MAX);
        let position = player.position_at(self.now);
        let target = if forward {
            position.saturating_add(delta)
        } else {
            position.saturating_sub(delta)
        };
        if !player.seekable() {
            return None;
        }
        Some(PlayerCommand::SetPosition {
            track_id: player.track_id.clone()?,
            position_us: target.clamp(0, length),
        })
    }
}

/// `m:ss`, or `h:mm:ss` from an hour on.
#[must_use]
pub fn format_time(us: i64) -> String {
    ui::format::timecode(u64::try_from(us.max(0) / 1_000_000).unwrap_or(0))
}

/// A plain string from the shell's Fluent catalogue.
pub(crate) fn tr(key: &str) -> String {
    ui::i18n::I18nManager::lookup_plain(key)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::shell::overlay_host::{FabricDeviceView, FabricMediaView, LocalMediaOutputView};
    use crate::shell::personal_continuity::DeviceId;
    use std::cell::RefCell;

    pub(crate) fn player(bus: &str, identity: &str, status: PlaybackStatus) -> PlayerSnapshot {
        PlayerSnapshot {
            bus_name: format!("org.mpris.MediaPlayer2.{bus}"),
            identity: identity.into(),
            status,
            title: Some(format!("{identity} track")),
            artist: Some("An Artist".into()),
            artwork: None,
            track_id: Some("/org/tilo/track/1".into()),
            length_us: Some(200_000_000),
            position_us: 50_000_000,
            can_play: true,
            can_pause: true,
            can_seek: true,
            can_go_next: true,
            can_go_previous: true,
            observed_at: Instant::now(),
        }
    }

    /// A scripted player list that records every command sent to it.
    #[derive(Default)]
    pub(crate) struct FakeSource {
        pub players: Vec<PlayerSnapshot>,
        pub sent: RefCell<Vec<(String, PlayerCommand)>>,
    }

    impl NowPlayingSource for FakeSource {
        fn players(&self) -> Vec<PlayerSnapshot> {
            self.players.clone()
        }

        fn send(&self, bus_name: &str, command: PlayerCommand) -> bool {
            self.sent.borrow_mut().push((bus_name.to_string(), command));
            true
        }
    }

    #[test]
    fn the_picked_player_wins_then_playing_then_paused() {
        let players = vec![
            player("a", "Stopped", PlaybackStatus::Stopped),
            player("b", "Paused", PlaybackStatus::Paused),
            player("c", "Playing", PlaybackStatus::Playing),
        ];
        assert_eq!(active_index(&players, None), Some(2));
        assert_eq!(
            active_index(&players, Some("org.mpris.MediaPlayer2.a")),
            Some(0)
        );
        assert_eq!(
            active_index(&players, Some("org.mpris.MediaPlayer2.gone")),
            Some(2)
        );
        assert_eq!(active_index(&players[..2], None), Some(1));
        assert_eq!(active_index(&players[..1], None), Some(0));
        assert_eq!(active_index(&[], None), None);
    }

    #[test]
    fn a_playing_position_advances_and_stops_at_the_end() {
        let mut p = player("a", "Music", PlaybackStatus::Playing);
        let later = p.observed_at + Duration::from_secs(10);
        assert_eq!(p.position_at(later), 60_000_000);
        assert_eq!(
            p.position_at(p.observed_at + Duration::from_secs(10_000)),
            200_000_000
        );
        p.status = PlaybackStatus::Paused;
        assert_eq!(p.position_at(later), 50_000_000);
    }

    #[test]
    fn seeking_needs_length_track_and_capability() {
        let p = player("a", "Music", PlaybackStatus::Playing);
        let view = NowPlayingView::new(vec![p.clone()], None, Vec::new(), p.observed_at)
            .expect("one player");
        assert_eq!(
            view.seek_command(0.5),
            Some(PlayerCommand::SetPosition {
                track_id: "/org/tilo/track/1".into(),
                position_us: 100_000_000
            })
        );
        assert_eq!(
            view.step_command(Duration::from_secs(5), false),
            Some(PlayerCommand::SetPosition {
                track_id: "/org/tilo/track/1".into(),
                position_us: 45_000_000
            })
        );
        for broken in [
            PlayerSnapshot {
                can_seek: false,
                ..p.clone()
            },
            PlayerSnapshot {
                track_id: None,
                ..p.clone()
            },
            PlayerSnapshot {
                length_us: None,
                ..p.clone()
            },
        ] {
            let view = NowPlayingView::new(vec![broken], None, Vec::new(), p.observed_at)
                .expect("one player");
            assert_eq!(view.seek_command(0.5), None);
        }
    }

    fn speaker_group(
        seed: u8,
        name: &str,
        state: crate::shell::speaker_group::SpeakerMemberStatus,
    ) -> crate::shell::speaker_group_runtime::SpeakerGroupView {
        use crate::shell::speaker_group::{
            EndpointSelector, SpeakerGroupId, SpeakerRole, SpeakerTrim, StereoSide,
        };
        crate::shell::speaker_group_runtime::SpeakerGroupView {
            id: SpeakerGroupId([seed; 16]),
            name: name.into(),
            leader: DeviceId::new("desk").expect("device id"),
            leader_label: "Desk speakers".into(),
            led_locally: true,
            members: vec![crate::shell::speaker_group_runtime::SpeakerMemberView {
                device: DeviceId::new("desk").expect("device id"),
                device_label: "Desk speakers".into(),
                output: EndpointSelector([seed.wrapping_add(1); 16]),
                output_label: None,
                role: SpeakerRole::Stereo(StereoSide::Left),
                trim: SpeakerTrim::default(),
                state,
            }],
        }
    }

    #[test]
    fn speaker_groups_are_output_targets_named_like_the_palette_and_revalidated() {
        use crate::shell::speaker_group::{SpeakerGroupId, SpeakerMemberStatus};
        let mut view = fabric();
        view.speaker_groups.groups = vec![
            speaker_group(0x51, "Living room", SpeakerMemberStatus::Ready),
            speaker_group(0x61, "Kitchen", SpeakerMemberStatus::AwaitingConsent),
        ];
        let controller = |session: &str| {
            (session == "so.tilo.music:mpris:abc").then(|| "org.mpris.MediaPlayer2.music".into())
        };
        let bus = Some("org.mpris.MediaPlayer2.music");
        let targets = output_targets(&view, bus, &controller);
        let groups = targets
            .iter()
            .filter(|target| target.kind == OutputKind::SpeakerGroup)
            .collect::<Vec<_>>();
        assert_eq!(
            groups
                .iter()
                .map(|target| (target.label.as_str(), target.current))
                .collect::<Vec<_>>(),
            vec![("Living room", false)],
            "only a group with a ready speaker is offered"
        );
        let token = &groups[0].token;
        assert_eq!(
            token,
            &crate::shell::search::providers::continuity::speaker_group_media_action_id(
                "so.tilo.music:mpris:abc",
                SpeakerGroupId([0x51; 16]),
            ),
            "the picker names the group by the continuity action id"
        );
        assert!(!token.contains("5151"), "{token} leaks the group id");

        assert_eq!(
            resolve_output(&view, token, bus, &controller),
            Some(DeviceFabricUiAction::PlayMediaOnSpeakerGroup {
                session_id: "so.tilo.music:mpris:abc".into(),
                group: SpeakerGroupId([0x51; 16]),
            })
        );

        let mut fresh = view.clone();
        fresh.speaker_groups.groups[0].members[0].state = SpeakerMemberStatus::AwaitingConsent;
        assert_eq!(
            resolve_output(&fresh, token, bus, &controller),
            None,
            "a group with no ready speaker left is not routed to"
        );
        fresh.speaker_groups.groups.clear();
        assert_eq!(resolve_output(&fresh, token, bus, &controller), None);
        assert_eq!(
            resolve_output(
                &view,
                token,
                Some("org.mpris.MediaPlayer2.other"),
                &controller
            ),
            None,
            "another player's picker cannot send this session to the group"
        );
    }

    #[test]
    fn headline_falls_back_to_the_application() {
        let mut p = player("a", "Radio", PlaybackStatus::Playing);
        assert_eq!(p.headline(), "Radio track");
        assert_eq!(p.subline(), Some("An Artist"));
        p.artist = None;
        assert_eq!(p.subline(), Some("Radio"));
        p.title = None;
        assert_eq!(p.headline(), "Radio");
        assert_eq!(p.subline(), None);
    }

    #[test]
    fn the_song_names_the_application_once() {
        let mut p = player("a", "Radio", PlaybackStatus::Playing);
        let song = p.song(None);
        assert_eq!(
            (
                song.title.as_str(),
                song.subtitle.as_str(),
                song.detail.as_str()
            ),
            ("Radio track", "An Artist", "Radio")
        );
        assert_eq!(song.art, ui::widget::TransportArt::Placeholder);
        assert_eq!(
            song.art_name, "Radio",
            "the placeholder is the application's"
        );
        p.artist = None;
        let song = p.song(None);
        assert_eq!(
            (song.subtitle.as_str(), song.detail.as_str()),
            ("Radio", ""),
            "the application is the subtitle, not a third line too"
        );
    }

    #[test]
    fn time_reads_like_a_clock() {
        assert_eq!(format_time(0), "0:00");
        assert_eq!(format_time(65_000_000), "1:05");
        assert_eq!(format_time(3_725_000_000), "1:02:05");
        assert_eq!(format_time(-5), "0:00");
    }

    fn device(id: &str, label: &str) -> FabricDeviceView {
        FabricDeviceView {
            id: DeviceId::new(id).expect("device id"),
            label: label.into(),
            kind: crate::shell::device_fabric::DeviceKind::Laptop,
            is_local: false,
            online: true,
            audio_output: true,
            video_output: false,
            display: false,
            controls_local: false,
            local_can_control: false,
        }
    }

    fn fabric() -> DeviceFabricView {
        let desk = DeviceId::new("desk").expect("device id");
        DeviceFabricView {
            devices: vec![device("desk", "Desk speakers")],
            local_media: vec![FabricMediaView {
                session_id: "so.tilo.music:mpris:abc".into(),
                title: "Song".into(),
                playing: true,
                has_video: false,
                targets: vec![desk],
            }],
            local_outputs: vec![
                LocalMediaOutputView {
                    node_id: 41,
                    label: "Headphones".into(),
                    is_default: true,
                    bluetooth_codec: None,
                },
                LocalMediaOutputView {
                    node_id: 42,
                    label: "Speakers".into(),
                    is_default: false,
                    bluetooth_codec: None,
                },
            ],
            ..DeviceFabricView::default()
        }
    }

    #[test]
    fn outputs_name_destinations_by_token_and_offer_devices_only_for_the_shown_session() {
        let view = fabric();
        let controller = |session: &str| {
            (session == "so.tilo.music:mpris:abc").then(|| "org.mpris.MediaPlayer2.music".into())
        };
        let targets = output_targets(&view, Some("org.mpris.MediaPlayer2.music"), &controller);
        assert_eq!(
            targets
                .iter()
                .map(|t| (t.label.as_str(), t.kind, t.current))
                .collect::<Vec<_>>(),
            vec![
                ("Headphones", OutputKind::Local, true),
                ("Speakers", OutputKind::Local, false),
                ("Desk speakers", OutputKind::Device, false),
            ]
        );
        for target in &targets {
            assert!(
                !target.token.contains("41")
                    && !target.token.contains("42")
                    && !target.token.contains("desk"),
                "{} leaks an identity",
                target.token
            );
        }
        assert_eq!(
            resolve_output(
                &view,
                &targets[1].token,
                Some("org.mpris.MediaPlayer2.music"),
                &controller
            ),
            Some(DeviceFabricUiAction::RouteLocalAudio { node_id: 42 })
        );
        assert_eq!(
            resolve_output(
                &view,
                &targets[0].token,
                Some("org.mpris.MediaPlayer2.music"),
                &controller
            ),
            None,
            "the current output is not a route"
        );
        assert!(matches!(
            resolve_output(
                &view,
                &targets[2].token,
                Some("org.mpris.MediaPlayer2.music"),
                &controller
            ),
            Some(DeviceFabricUiAction::HandoffMedia { .. })
        ));
        let other = output_targets(&view, Some("org.mpris.MediaPlayer2.other"), &controller);
        assert_eq!(other.len(), 2, "another player's session is not movable");
        assert_eq!(
            resolve_output(
                &view,
                &targets[2].token,
                Some("org.mpris.MediaPlayer2.other"),
                &controller
            ),
            None
        );
    }
}
