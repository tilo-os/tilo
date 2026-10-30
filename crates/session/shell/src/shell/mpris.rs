//! What is playing on the session bus, and driving it, through the session
//! audio helper (`tilo-audio`, see [`crate::shell::audio_service`]).
//!
//! The helper is the session's one MPRIS client: it follows every player
//! and makes every call. This module keeps the compositor's copy of the
//! players it last described, attributes each player's process to a launch
//! (only the compositor knows its launches), publishes the verified ones as
//! cross-device handoff reports, and turns the transport keys, the
//! now-playing controls and a handoff's resume into requests. Nothing here
//! waits on the helper: answers arrive on its reader thread and reach the
//! compositor loop through the caller's delivery or the state it samples.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;

use tilo_audio_service::handoff::{self, MediaApp};
use tilo_audio_service::{ArtworkImage, Event, Player, Request};
pub use tilo_audio_service::{Outcome, ResumeRequest, ResumeSource, Transport as Command};

use crate::shell::now_playing::{Artwork, PlaybackStatus, PlayerCommand, PlayerSnapshot};

const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
const MAX_PENDING_RESUMES: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AcceptedResume {
    pub(crate) request_id: [u8; 16],
    pub(crate) controller_bus_name: Option<String>,
    pub(crate) expected_uri: Option<String>,
}

type OnOutcome = Box<dyn FnOnce(Outcome) + Send>;

#[derive(Default)]
struct Client {
    /// Where requests go; `None` while no helper runs.
    requests: Option<Sender<Request>>,
    /// The players the helper last described, and when.
    players: Vec<Player>,
    received: Option<Instant>,
    /// Optimistic play/pause state shown until the helper describes the
    /// players again.
    toggled: HashMap<String, PlaybackStatus>,
    /// Decoded covers by `mpris:artUrl`.
    artwork: HashMap<String, Option<Artwork>>,
    /// Launch attributions sent, so each owner is verified once.
    verified: HashMap<(String, String), String>,
    handoff: bool,
    next_transport: u64,
    transports: HashMap<u64, OnOutcome>,
    /// Resumes asked for and not yet answered; one cancelled meanwhile is
    /// gone from here, so its late acceptance is dropped.
    resumes: BTreeSet<[u8; 16]>,
    accepted: VecDeque<AcceptedResume>,
}

static CLIENT: LazyLock<Mutex<Client>> = LazyLock::new(|| Mutex::new(Client::default()));

/// Wakes the compositor's loop when the players change, so a desk widget
/// showing what plays follows a track change without polling.
static NOW_PLAYING_WAKE: Mutex<Option<crate::session_runtime::Wake>> = Mutex::new(None);
/// Grows whenever the observed players change.
static NOW_PLAYING_REVISION: AtomicU64 = AtomicU64::new(0);

/// Send requests to a newly started helper through `requests`.
pub(crate) fn attach(requests: Sender<Request>) {
    if let Ok(mut client) = CLIENT.lock() {
        *client = Client {
            requests: Some(requests),
            ..Client::default()
        };
    }
    now_playing_changed();
}

/// The helper stopped: nothing is playing as far as the compositor knows.
pub(crate) fn detach() {
    if let Ok(mut client) = CLIENT.lock() {
        let handoff = client.handoff;
        *client = Client::default();
        if handoff {
            media::handoff::replace_system_reports(Vec::new());
        }
    }
    now_playing_changed();
}

fn send_request(client: &Client, request: Request) -> bool {
    client
        .requests
        .as_ref()
        .is_some_and(|requests| requests.send(request).is_ok())
}

/// One event from the helper, on its reader thread.
pub(crate) fn on_event(event: Event) {
    let Ok(mut client) = CLIENT.lock() else {
        return;
    };
    match event {
        Event::Device { .. } => {}
        Event::Players { players } => {
            client.players = players;
            client.received = Some(Instant::now());
            client.toggled.clear();
            verify(&mut client);
            let wanted = client
                .players
                .iter()
                .filter_map(|player| player.art_url.clone())
                .collect::<BTreeSet<_>>();
            if client.artwork.len() > tilo_audio_service::MAX_PLAYERS {
                client.artwork.retain(|url, _| wanted.contains(url));
            }
            drop(client);
            now_playing_changed();
        }
        Event::Artwork { url, image } => {
            client.artwork.insert(url, image.and_then(decode_artwork));
            drop(client);
            now_playing_changed();
        }
        Event::Transported { id, outcome, .. } => {
            let waiter = client.transports.remove(&id);
            drop(client);
            if let (Some(waiter), Some(outcome)) = (waiter, outcome) {
                waiter(outcome);
            }
        }
        Event::Resumed { request_id, player } => client.resumed(request_id, player),
    }
}

impl Client {
    /// A resume ended: accepted with the player that assumed the state, or
    /// not. One cancelled meanwhile stays cancelled.
    fn resumed(&mut self, request_id: [u8; 16], player: Option<(String, String)>) {
        if self.resumes.remove(&request_id)
            && let Some((bus_name, uri)) = player
        {
            if self.accepted.len() >= MAX_PENDING_RESUMES {
                self.accepted.pop_front();
            }
            self.accepted.push_back(AcceptedResume {
                request_id,
                controller_bus_name: Some(bus_name),
                expected_uri: Some(uri),
            });
        }
    }

    /// Forget a resume, running or accepted. Whether there was one, and
    /// whether the helper must revoke it.
    fn cancel(&mut self, request_id: [u8; 16]) -> (bool, bool) {
        let pending = self.resumes.remove(&request_id);
        let before = self.accepted.len();
        self.accepted
            .retain(|accepted| accepted.request_id != request_id);
        (pending || self.accepted.len() != before, pending)
    }
}

fn decode_artwork(image: ArtworkImage) -> Option<Artwork> {
    use base64::Engine as _;
    let rgba = base64::engine::general_purpose::STANDARD
        // optional-decode: artwork the audio helper sent malformed shows the player without a cover
        .decode(image.rgba)
        .ok()?;
    (u64::try_from(rgba.len()).ok()?
        == u64::from(image.width)
            .checked_mul(u64::from(image.height))?
            .checked_mul(4)?)
    .then(|| Artwork {
        width: image.width,
        height: image.height,
        rgba: Arc::new(rgba),
    })
}

/// Attribute every player not yet verified to a launch, and tell the helper
/// which package each belongs to. A bounded walk of `/proc` per player; no
/// bus call. Until it succeeds, a player is shown but never offered for
/// handoff, and a resume never drives it.
fn verify(client: &mut Client) {
    let unverified = client
        .players
        .iter()
        .filter(|player| player.package_id.is_none())
        .filter_map(|player| {
            Some((
                player.bus_name.clone(),
                player.unique_name.clone(),
                player.pid?,
            ))
        })
        .collect::<Vec<_>>();
    for (bus_name, unique_name, pid) in unverified {
        let key = (bus_name.clone(), unique_name.clone());
        if client.verified.contains_key(&key) {
            continue;
        }
        let Some(package_id) = crate::shell::launcher::verified_package_for_descendant(pid) else {
            continue;
        };
        if send_request(
            client,
            Request::Verified {
                bus_name,
                unique_name,
                package_id: package_id.clone(),
            },
        ) {
            client.verified.insert(key, package_id);
        }
    }
    let live = client
        .players
        .iter()
        .map(|player| (player.bus_name.clone(), player.unique_name.clone()))
        .collect::<BTreeSet<_>>();
    client.verified.retain(|key, _| live.contains(key));
}

/// The package verified behind `player`, as the helper knows it or as just
/// sent to it.
fn package_of<'a>(client: &'a Client, player: &'a Player) -> Option<&'a str> {
    player.package_id.as_deref().or_else(|| {
        client
            .verified
            .get(&(player.bus_name.clone(), player.unique_name.clone()))
            .map(String::as_str)
    })
}

/// Publish this machine's players as handoff reports, while media
/// continuity is permitted, playheads extrapolated to now. Called about once
/// a second, this is what keeps the reports inside `media`'s freshness
/// window while the helper describes the players. It also retries
/// attributing players whose launch was not yet recorded when they appeared.
pub fn refresh_handoff_reports() {
    let Ok(mut client) = CLIENT.lock() else {
        return;
    };
    client.handoff = true;
    verify(&mut client);
    let elapsed = client.received.map(|at| at.elapsed()).unwrap_or_default();
    let apps = crate::shell::launcher::installed_apps_without_waiting()
        .into_iter()
        .filter(|app| app.has_gui)
        .map(|app| MediaApp {
            package_id: app.package_id,
            uri_schemes: app.opens_uri_schemes,
            extensions: app.opens_extensions,
        })
        .collect::<Vec<_>>();
    let reports = client
        .players
        .iter()
        .filter_map(|player| {
            let verified = Player {
                package_id: package_of(&client, player).map(str::to_string),
                ..player.clone()
            };
            handoff::report(&verified, position_after(player, elapsed), &apps)
        })
        .take(media::handoff::MAX_PLAYBACK_REPORTS)
        .collect();
    drop(client);
    media::handoff::replace_system_reports(reports);
}

/// Stop publishing this machine's players as handoff reports: media
/// continuity is not permitted.
pub fn withdraw_handoff_reports() {
    if CLIENT
        .lock()
        .is_ok_and(|mut client| std::mem::replace(&mut client.handoff, false))
    {
        media::handoff::replace_system_reports(Vec::new());
    }
}

/// `player`'s playhead `elapsed` after it was described: advanced at its
/// rate while playing, bounded by the track length.
fn position_after(player: &Player, elapsed: std::time::Duration) -> i64 {
    let base = player.position_us.max(0);
    let position = if player.status == PlaybackStatus::Playing {
        let advanced = std::time::Duration::try_from_secs_f64(elapsed.as_secs_f64() * player.rate)
            .unwrap_or(elapsed);
        base.saturating_add(i64::try_from(advanced.as_micros()).unwrap_or(i64::MAX))
    } else {
        base
    };
    player
        .length_us
        .map_or(position, |length| position.min(length))
}

/// Ask a newly launched, launcher-verified player to assume an exact media
/// state. Completion is reported only after the player itself reflects the
/// requested source, playhead and transport status
/// ([`take_accepted_handoff_resumes`]).
pub fn resume_after_launch(request: ResumeRequest) -> bool {
    let Ok(mut client) = CLIENT.lock() else {
        return false;
    };
    let request_id = request.request_id;
    if request_id == [0; 16]
        || client.resumes.len() >= MAX_PENDING_RESUMES
        || client.resumes.contains(&request_id)
        || handoff::resume_uri(&request.source).is_none()
    {
        return false;
    }
    let sent = send_request(&client, Request::Resume { request });
    if sent {
        client.resumes.insert(request_id);
    }
    sent
}

/// Revoke one exact destination resume request: the helper issues no
/// player call for it after it reads the revocation, and an acceptance that
/// arrives later is dropped.
pub(crate) fn cancel_resume_after_launch(request_id: [u8; 16]) -> bool {
    if request_id == [0; 16] {
        return false;
    }
    let Ok(mut client) = CLIENT.lock() else {
        return false;
    };
    let (known, running) = client.cancel(request_id);
    if running {
        send_request(&client, Request::CancelResume { request_id });
    }
    known
}

pub(crate) fn take_accepted_handoff_resumes() -> Vec<AcceptedResume> {
    CLIENT.lock().map_or_else(
        |_| Vec::new(),
        |mut client| client.accepted.drain(..).collect(),
    )
}

/// Send `cmd` to the player the person last listened to and return at
/// once. `on_outcome` runs on the helper's reader thread with the resulting
/// state once the player accepted the command in time; it should only
/// publish (wake the compositor loop). With no player, no bus, a refusal or
/// a late answer it is not called — the caller shows nothing rather than
/// claim something happened. Returns `false` when the command could not be
/// dispatched.
pub fn send<F>(cmd: Command, on_outcome: F) -> bool
where
    F: FnOnce(Outcome) + Send + 'static,
{
    let Ok(mut client) = CLIENT.lock() else {
        return false;
    };
    let id = client.next_transport;
    client.next_transport = id.wrapping_add(1);
    let sent = send_request(&client, Request::Transport { id, command: cmd });
    if sent {
        client.transports.insert(id, Box::new(on_outcome));
    }
    sent
}

fn is_player_name(bus_name: &str) -> bool {
    bus_name.starts_with(MPRIS_PREFIX) && bus_name.len() <= 256
}

/// Pause one exact player after its cross-device successor has reported a
/// successfully opened source. Unlike media-key dispatch, this never falls
/// back to whichever unrelated player happens to be active.
pub fn pause_named(bus_name: &str) -> bool {
    is_player_name(bus_name)
        && CLIENT.lock().is_ok_and(|client| {
            send_request(
                &client,
                Request::Pause {
                    bus_name: bus_name.to_string(),
                    if_uri: None,
                },
            )
        })
}

/// Pause one exact player only while it still plays the URI that this
/// handoff opened, so compensating for a failed acknowledgement never
/// pauses unrelated playback that started immediately afterwards.
pub(crate) fn pause_named_if_uri(bus_name: &str, expected_uri: &str) -> bool {
    is_player_name(bus_name)
        && !expected_uri.is_empty()
        && expected_uri.len() <= 8192
        && CLIENT.lock().is_ok_and(|client| {
            send_request(
                &client,
                Request::Pause {
                    bus_name: bus_name.to_string(),
                    if_uri: Some(expected_uri.to_string()),
                },
            )
        })
}

/// Wake the loop through `wake` whenever the players change.
pub fn set_now_playing_wake(wake: crate::session_runtime::Wake) {
    if let Ok(mut slot) = NOW_PLAYING_WAKE.lock() {
        *slot = Some(wake);
    }
}

/// Grows whenever the observed players change; a reader that remembers it
/// knows whether to look again.
pub fn now_playing_revision() -> u64 {
    NOW_PLAYING_REVISION.load(Ordering::Acquire)
}

fn now_playing_changed() {
    NOW_PLAYING_REVISION.fetch_add(1, Ordering::AcqRel);
    let wake = NOW_PLAYING_WAKE.lock().ok().and_then(|slot| slot.clone());
    if let Some(wake) = wake {
        wake();
    }
}

/// The players last described. Empty while no helper runs.
pub fn now_playing_players() -> Vec<PlayerSnapshot> {
    let Ok(client) = CLIENT.lock() else {
        return Vec::new();
    };
    let observed_at = client.received.unwrap_or_else(Instant::now);
    client
        .players
        .iter()
        .map(|player| snapshot(&client, player, observed_at))
        .collect()
}

fn snapshot(client: &Client, player: &Player, observed_at: Instant) -> PlayerSnapshot {
    PlayerSnapshot {
        bus_name: player.bus_name.clone(),
        identity: player.identity.clone().unwrap_or_else(|| {
            player
                .bus_name
                .trim_start_matches(MPRIS_PREFIX)
                .split('.')
                .next()
                .unwrap_or_default()
                .to_string()
        }),
        // A sandboxed Tilo app's player is named after its package
        // (`org.mpris.MediaPlayer2.so.tilo.music`).
        app: package_of(client, player)
            .map(str::to_string)
            .or_else(|| player.desktop_entry.clone())
            .or_else(|| {
                player
                    .bus_name
                    .strip_prefix(MPRIS_PREFIX)
                    .filter(|rest| rest.contains('.'))
                    .map(str::to_string)
            }),
        status: client
            .toggled
            .get(&player.bus_name)
            .copied()
            .unwrap_or(player.status),
        title: player.title.clone(),
        artist: player.artist.clone(),
        artwork: player
            .art_url
            .as_ref()
            .and_then(|url| client.artwork.get(url).cloned().flatten()),
        track_id: player.track_id.clone(),
        length_us: player.length_us,
        position_us: player.position_us,
        can_play: player.can_play,
        can_pause: player.can_pause,
        can_seek: player.can_seek,
        can_go_next: player.can_go_next,
        can_go_previous: player.can_go_previous,
        observed_at,
    }
}

/// The helper follows the players' signals, so there is nothing to poll.
pub fn refresh_now_playing() {}

/// Send `command` to exactly the player at `bus_name`, never to whichever
/// other player happens to be active. Returns whether it was dispatched.
pub fn control(bus_name: &str, command: &PlayerCommand) -> bool {
    if !is_player_name(bus_name) {
        return false;
    }
    let Ok(mut client) = CLIENT.lock() else {
        return false;
    };
    if !send_request(
        &client,
        Request::Control {
            bus_name: bus_name.to_string(),
            command: command.clone(),
        },
    ) {
        return false;
    }
    // Show the toggle at once; the helper's next description confirms or
    // corrects it.
    let shown = client
        .players
        .iter()
        .find(|player| player.bus_name == bus_name)
        .map(|player| {
            client
                .toggled
                .get(bus_name)
                .copied()
                .unwrap_or(player.status)
        });
    if let Some(status) = shown
        && matches!(command, PlayerCommand::PlayPause | PlayerCommand::Pause)
    {
        let toggled = if status == PlaybackStatus::Playing {
            PlaybackStatus::Paused
        } else if *command == PlayerCommand::Pause {
            status
        } else {
            PlaybackStatus::Playing
        };
        client.toggled.insert(bus_name.to_string(), toggled);
        drop(client);
        now_playing_changed();
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_audio_service::Status;

    fn player(bus_name: &str, status: Status) -> Player {
        Player {
            bus_name: bus_name.into(),
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
            track_id: None,
            length_us: Some(10_000_000),
            position_us: 4_000_000,
            rate: 2.0,
            can_play: true,
            can_pause: true,
            can_seek: false,
            can_go_next: false,
            can_go_previous: false,
        }
    }

    #[test]
    fn handoff_playheads_advance_at_the_players_rate_up_to_the_end() {
        let playing = player("org.mpris.MediaPlayer2.a", Status::Playing);
        let second = std::time::Duration::from_secs(1);
        assert_eq!(position_after(&playing, second), 6_000_000);
        assert_eq!(position_after(&playing, second * 60), 10_000_000);
        let paused = player("org.mpris.MediaPlayer2.a", Status::Paused);
        assert_eq!(position_after(&paused, second * 60), 4_000_000);
    }

    #[test]
    fn snapshots_name_the_player_and_its_app() {
        let client = Client::default();
        let now = Instant::now();
        let web = snapshot(
            &client,
            &player("org.mpris.MediaPlayer2.web.tab3", Status::Paused),
            now,
        );
        assert_eq!(web.identity, "web", "no Identity: named after its bus name");
        assert_eq!(web.app.as_deref(), Some("web.tab3"));
        let music = Player {
            identity: Some("Music".into()),
            desktop_entry: Some("music".into()),
            ..player("org.mpris.MediaPlayer2.music", Status::Playing)
        };
        let music = snapshot(&client, &music, now);
        assert_eq!(music.identity, "Music");
        assert_eq!(music.app.as_deref(), Some("music"));
        assert_eq!(music.status, PlaybackStatus::Playing);
    }

    #[test]
    fn requests_need_a_helper_and_real_player_names() {
        // Unit tests start no helper: nothing can reach a player of whoever
        // runs them.
        assert!(!send(Command::PlayPause, |_| {}));
        assert!(!pause_named("org.mpris.MediaPlayer2.probe"));
        assert!(!control("org.freedesktop.DBus", &PlayerCommand::Pause));
        assert!(!control(
            &format!("{MPRIS_PREFIX}{}", "x".repeat(300)),
            &PlayerCommand::Pause
        ));
        assert!(!pause_named_if_uri("org.mpris.MediaPlayer2.probe", ""));
    }

    #[test]
    fn a_cancelled_resume_never_publishes_a_late_acceptance() {
        let player = || {
            Some((
                "org.mpris.MediaPlayer2.a".to_string(),
                "https://x.example/a".to_string(),
            ))
        };
        let mut client = Client::default();
        client.resumes.insert([1; 16]);
        assert_eq!(client.cancel([1; 16]), (true, true));
        client.resumed([1; 16], player());
        assert!(client.accepted.is_empty(), "late acceptance dropped");
        assert_eq!(client.cancel([1; 16]), (false, false));

        client.resumes.insert([2; 16]);
        client.resumed([2; 16], player());
        assert_eq!(client.accepted.len(), 1);
        assert_eq!(
            client.cancel([2; 16]),
            (true, false),
            "cancelling retracts a completed acceptance; nothing left to revoke"
        );
        assert!(client.accepted.is_empty());

        client.resumes.insert([3; 16]);
        client.resumed([3; 16], None);
        assert!(client.accepted.is_empty() && client.resumes.is_empty());
    }

    #[test]
    fn covers_arrive_as_rgba_of_their_size() {
        use base64::Engine as _;
        let encode = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
        let cover = decode_artwork(ArtworkImage {
            width: 2,
            height: 1,
            rgba: encode(&[1; 8]),
        })
        .expect("a 2×1 cover");
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
