//! The panel while it runs as an app of the session: what it reads, and
//! where its presses go.
//!
//! The panel is a process of its own, started by the session like any other
//! app, so nothing here is lent it: what it shows is what the session's
//! services publish ([`crate::readings`]), and what a press does is one of
//! those resources' actions ([`crate::verbs`]). One worker carries both — it
//! opens each resource, hands every reading to the panel and takes the
//! presses out — so no reading is missed while an action runs, and the frame
//! thread never waits on a service.
//!
//! What the panel does itself stays here: the prompts it opens, what it
//! shows, the player it steers, and the reading of the Wi-Fi section that
//! keeps looking for networks while it is open.

use std::rc::Rc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use ui::WakeHandle;
use ui::serde_json;

use crate::effect::QsEffect;
use crate::host::{Feed, PanelHost, Showing};
use crate::media::Media;
use crate::readings::{Readings, land};
use crate::session::{BLUETOOTH, MEDIA, NETWORK, Watch};
use crate::{HiddenNetworkPrompt, QsSection, WifiPasswordPrompt};

/// How often the playhead of what plays moves on while it plays.
const PLAYHEAD: Duration = Duration::from_secs(1);

/// How long the open Wi-Fi section is left before it looks for networks
/// again: often enough that walking into range shows up while the person
/// looks, rarely enough that the radio is not kept busy.
const WIFI_RESCAN: Duration = Duration::from_secs(10);

/// How long the worker waits when nothing is under way.
const IDLE: Duration = Duration::from_secs(120);

/// What the panel asks its worker to do.
enum Job {
    /// Carry out `effect` on the device.
    Effect(QsEffect),
    /// The panel shows what `Showing` says now.
    Showing(Showing),
    /// A reading the worker watches was published.
    Reading,
}

/// The running panel's panel: the readings the view shows, and the worker
/// that keeps them current.
pub(crate) struct Live {
    wake: WakeHandle,
    readings: OnceLock<Readings>,
    media: Arc<Mutex<Media>>,
    jobs: OnceLock<Sender<Job>>,
}

impl Live {
    /// The panel of this process, woken through `wake` when a reading moved.
    pub(crate) fn new(wake: WakeHandle) -> Self {
        Self {
            wake,
            readings: OnceLock::new(),
            media: Arc::new(Mutex::new(Media::default())),
            jobs: OnceLock::new(),
        }
    }

    /// The panel of this process, when the session started it, or `None`
    /// when it runs on its own: a screenshot and a test lend a fixture
    /// instead, and an app run by hand has no session to read.
    pub(crate) fn of(wake: WakeHandle) -> Option<Rc<Self>> {
        std::env::var_os("TILO_SURFACE_LAYER")?;
        Some(Rc::new(Self::new(wake)))
    }

    /// The readings the view shows, once the panel attached them.
    fn readings(&self) -> Result<&Readings, String> {
        self.readings
            .get()
            .ok_or_else(|| "the panel's readings are not attached".to_owned())
    }

    fn media(&self) -> MutexGuard<'_, Media> {
        self.media
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Carry out the effects the panel keeps to itself: the prompts it opens,
    /// the arrangement it keeps, the player it steers. `false` for an effect
    /// that changes the device, which goes to the session.
    fn local(&self, effect: &QsEffect) -> Result<bool, String> {
        let readings = self.readings()?;
        match effect {
            QsEffect::AskWifiPassword { network } => readings.update(|state| {
                state.wifi_password = Some(WifiPasswordPrompt::new(network.clone()));
            }),
            QsEffect::CancelWifiPassword => readings.update(|state| state.wifi_password = None),
            QsEffect::AskHiddenNetwork => {
                readings.update(|state| state.wifi_hidden = Some(HiddenNetworkPrompt::new()));
            }
            QsEffect::CancelHiddenNetwork => readings.update(|state| state.wifi_hidden = None),
            QsEffect::JoinWifi { network } => readings.update(|state| {
                state.wifi_password = None;
                state.wifi_joining = Some(network.clone());
            }),
            QsEffect::ConnectWifi { network, password } => {
                if password.is_empty() {
                    return Err("a Wi-Fi password is not empty".into());
                }
                readings.update(|state| {
                    let prompt = state
                        .wifi_password
                        .get_or_insert_with(|| WifiPasswordPrompt::new(network.clone()));
                    prompt.connecting = true;
                    prompt.error = None;
                    state.wifi_joining = Some(network.clone());
                });
            }
            QsEffect::ConnectHiddenWifi { network, .. } => {
                if network.is_empty() {
                    return Err("a network whose name is hidden still has a name".into());
                }
                readings.update(|state| {
                    let prompt = state
                        .wifi_hidden
                        .get_or_insert_with(HiddenNetworkPrompt::new);
                    prompt.ssid.clone_from(network);
                    prompt.connecting = true;
                    prompt.error = None;
                    state.wifi_joining = Some(network.clone());
                });
            }
            QsEffect::SetNightLight { on } => {
                // The session turns the screen warm and keeps the switch; the
                // panel shows what its reading says, before and after.
                readings.update(|state| state.night_light = *on);
            }
            QsEffect::SetRotationLock { locked } => {
                readings.update(|state| state.rotation_lock = *locked);
            }
            QsEffect::SetDoNotDisturb { on } => {
                readings.update(|state| state.do_not_disturb = *on);
            }
            QsEffect::DismissRefusal => readings.update(|state| state.refusal = None),
            QsEffect::ChoosePlayer { player } => {
                self.media().choose(player)?;
                self.advance();
            }
            QsEffect::ResumePlayback { request } => {
                let offer = readings.read(|state| state.offer.clone());
                if let Some(offer) = offer {
                    crate::media::resumable(&offer, request)?;
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// What plays, moved on to this moment.
    fn advance(&self) {
        let Ok(readings) = self.readings() else {
            return;
        };
        let now = Instant::now();
        readings.update(|state| {
            let playing = self.media().now_playing(now, state.offer.as_ref());
            state.now_playing = playing;
        });
    }
}

impl PanelHost for Live {
    fn attach(&self, feed: Feed) {
        if self.readings.set(feed.readings()).is_err() {
            return;
        }
        let readings = feed.readings();
        let (jobs, queue) = channel();
        if self.jobs.set(jobs.clone()).is_err() {
            return;
        }
        let media = Arc::clone(&self.media);
        let wake = self.wake.clone();
        let spawned = std::thread::Builder::new()
            .name("qs-session".into())
            .spawn(move || watch(readings, media, queue, jobs, wake));
        if let Err(error) = spawned {
            tracing::warn!(%error, "quick settings: the session is not followed");
        }
    }

    fn showing(&self, showing: &Showing) {
        if let Some(jobs) = self.jobs.get() {
            let _ = jobs.send(Job::Showing(showing.clone()));
        }
    }

    fn run(&self, effect: QsEffect) -> Result<(), String> {
        // The panel's own part happens here, at once; whatever reaches the
        // device is the worker's to send, so a slow service never holds the
        // frame. A press puts away why the one before did not land.
        let readings = self.readings()?;
        readings.update(|state| state.refusal = None);
        let local = self.local(&effect)?;
        if readings
            .read(|state| crate::verbs::of(state, &effect))
            .is_some()
        {
            if let Some(jobs) = self.jobs.get() {
                let _ = jobs.send(Job::Effect(effect));
            }
            return Ok(());
        }
        if local {
            return Ok(());
        }
        // Nothing carries it: the person is told at the control they pressed,
        // instead of a press that does nothing.
        Err(crate::i18n::t("qs-refused-nothing-answers"))
    }
}

/// Open the session's resources, hand their readings to the panel, and take
/// the panel's presses out to them, until the panel ends.
#[allow(
    clippy::needless_pass_by_value,
    reason = "the thread owns the readings, the panel it steers and its ends of both channels"
)]
fn watch(
    readings: Readings,
    media: Arc<Mutex<Media>>,
    jobs: Receiver<Job>,
    wakeups: Sender<Job>,
    wake: WakeHandle,
) {
    let mut watch = Watch::new();
    let mut scope = ui::WakeScope::new();
    if let Err(error) = scope.bind(move |_| {
        let _ = wakeups.send(Job::Reading);
    }) {
        tracing::warn!(%error, "quick settings: the session's readings are not heard");
        return;
    }
    let mut section = None;
    let mut scan_at = None;
    loop {
        let mut moved = watch.open(&scope.handle());
        moved |= watch.drain(&|resource, text| {
            if resource == MEDIA {
                land_media(&readings, &media, text);
            } else {
                land(&readings, resource, text);
            }
        });
        // The playhead moves between two readings while a player plays.
        let playing = readings.read(|state| {
            state
                .now_playing
                .as_ref()
                .is_some_and(|playing| playing.playing)
        });
        if playing {
            let now = Instant::now();
            readings.update(|state| {
                let playing = media
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .now_playing(now, state.offer.as_ref());
                state.now_playing = playing;
            });
            moved = true;
        }
        // The open Wi-Fi section keeps looking for networks.
        if scan_at.is_some_and(|at: Instant| Instant::now() >= at) {
            let now = Instant::now();
            scan_at = Some(now.checked_add(WIFI_RESCAN).unwrap_or(now));
            if let Err(error) = watch.invoke(NETWORK, "scan-wifi", &serde_json::Map::new()) {
                tracing::debug!(%error, "Wi-Fi did not look for networks");
            }
        }
        if moved {
            wake.wake();
        }
        let now = Instant::now();
        let deadline = [
            playing.then(|| now.checked_add(PLAYHEAD).unwrap_or(now)),
            scan_at,
        ]
        .into_iter()
        .flatten()
        .min();
        let timeout = deadline.map_or(IDLE, |at| at.saturating_duration_since(Instant::now()));
        match jobs.recv_timeout(timeout) {
            Ok(Job::Effect(effect)) => {
                let state = readings.read(std::clone::Clone::clone);
                if let Some((resource, action, arguments)) = crate::verbs::of(&state, &effect) {
                    if let Err(error) = watch.invoke(resource, action, &arguments) {
                        tracing::debug!(%error, %action, "the session did not take the press");
                        readings.update(|state| state.refusal = Some(error));
                        wake.wake();
                    }
                } else {
                    tracing::debug!(?effect, "the panel's effect has no action yet");
                    let reason = crate::i18n::t("qs-refused-nothing-answers");
                    readings.update(|state| state.refusal = Some(reason));
                    wake.wake();
                }
            }
            Ok(Job::Showing(showing)) => {
                if showing.section != section {
                    section = showing.section;
                    follow_section(&readings, &watch, section, &mut scan_at);
                }
            }
            Ok(Job::Reading) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// The panel opened or closed a section: the Wi-Fi one starts looking for
/// networks again, the Bluetooth one looks for devices while it shows and
/// stops when it does not.
fn follow_section(
    readings: &Readings,
    watch: &Watch,
    section: Option<QsSection>,
    scan_at: &mut Option<Instant>,
) {
    if section == Some(QsSection::Wifi) {
        *scan_at = Some(Instant::now());
    } else {
        *scan_at = None;
    }
    let discovering = readings.read(|state| state.bluetooth.discovering);
    let wanted = section == Some(QsSection::Bluetooth);
    if wanted != discovering
        && let Err(error) = watch.invoke(
            BLUETOOTH,
            "scan",
            &serde_json::Map::from_iter([("on".to_owned(), serde_json::Value::from(wanted))]),
        )
    {
        tracing::debug!(%error, "Bluetooth did not follow the section");
    }
}

/// Land what the audio service reads of the players: the card the panel
/// shows is folded from it, and its playhead moves from where it was read.
fn land_media(readings: &Readings, media: &Mutex<Media>, text: &str) {
    let reading = match serde_json::from_str::<audio::media::Media>(text) {
        Ok(reading) => reading,
        Err(error) => {
            tracing::warn!(%error, "the media reading did not parse");
            return;
        }
    };
    let now = Instant::now();
    media
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .land(reading, now);
    readings.update(|state| {
        let playing = media
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .now_playing(now, state.offer.as_ref());
        state.now_playing = playing;
    });
}
