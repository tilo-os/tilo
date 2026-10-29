//! The device behind the panel: the [`PanelHost`] a desktop lends the panel
//! to read this machine and carry out its effects.
//!
//! [`DeviceHost`] reads battery, network, radios, Bluetooth, the levels and
//! the power mode through `os-actions` (and the audio graph through
//! `tilo-media`), follows the media players through the session's audio
//! helper, and carries out each [`QsEffect`] through the same. Nothing it
//! does waits on the device: reads and writes run on workers that wake the
//! session when they are done, and the session lands them on its own
//! thread with [`DeviceHost::pump`]. The effects only the desktop can carry
//! out (Settings, the lock screen, the power menu, notifications, the
//! clipboard history, the device fabric's rows) come back to the session
//! as [`SessionRequest`]s.
//!
//! Its seams are the reads ([`DeviceReads`]), the dispatcher
//! ([`os_actions::Dispatcher`]), the person's settings ([`Preferences`])
//! and the players' transport ([`PlayerLink`]), so a test drives the panel
//! with stand-ins for all of them.

mod apply;
mod players;
mod reads;
mod refresh;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use os_actions::{Dispatcher, RadioChange};
use settings_store::hardware::QuickSettingsConfig;

pub use players::PlayerLink;
pub use reads::{DeviceReads, OsReads};
pub use refresh::Wake;

use crate::effect::QsEffect;
use crate::host::{Feed, PanelHost, Showing};
use crate::i18n::{t, t_args};
use crate::radio::RadioTicket;
use crate::{QsSection, QuickSettingsState};
use apply::LevelChange;
use reads::{Failure, QsField, QsReading, ReadBack};

/// How often the open Wi-Fi section scans for networks again: often enough
/// that walking into range shows up while the person looks, rarely enough
/// that the radio is not kept busy.
const WIFI_RESCAN: Duration = Duration::from_secs(10);
/// How often the playhead moves on the card while something plays.
const PLAYHEAD: Duration = Duration::from_secs(1);

/// The person's settings the panel changes: kept by the settings store on a
/// desktop, in memory in a test.
pub trait Preferences {
    /// The controls the panel shows, in the person's order.
    fn arrangement(&self) -> QuickSettingsConfig;
    /// Keep the arrangement.
    ///
    /// # Errors
    /// It could not be kept.
    fn save_arrangement(&self, arrangement: &QuickSettingsConfig) -> Result<(), String>;
    fn night_light(&self) -> bool;
    /// # Errors
    /// It could not be kept.
    fn set_night_light(&self, on: bool) -> Result<(), String>;
    fn rotation_lock(&self) -> bool;
    /// # Errors
    /// It could not be kept.
    fn set_rotation_lock(&self, locked: bool) -> Result<(), String>;
    /// Whether the machine has an accelerometer, so the screen can turn.
    fn rotation_available(&self) -> bool;
}

/// The person's settings, in the settings store.
#[derive(Clone, Copy, Debug, Default)]
pub struct StoredPreferences;

impl Preferences for StoredPreferences {
    fn arrangement(&self) -> QuickSettingsConfig {
        QuickSettingsConfig::load()
    }

    fn save_arrangement(&self, arrangement: &QuickSettingsConfig) -> Result<(), String> {
        arrangement.save().map_err(|error| error.to_string())
    }

    fn night_light(&self) -> bool {
        settings_store::hardware::NIGHT_LIGHT.get()
    }

    fn set_night_light(&self, on: bool) -> Result<(), String> {
        let origin = settings_store::ChangeOrigin::local("qs.night-light");
        settings_store::hardware::NIGHT_LIGHT
            .set_by(on, &origin)
            .map_err(|error| error.to_string())
    }

    fn rotation_lock(&self) -> bool {
        settings_store::hardware::ROTATION_LOCK.get()
    }

    fn set_rotation_lock(&self, locked: bool) -> Result<(), String> {
        let origin = settings_store::ChangeOrigin::local("qs.rotation-lock");
        settings_store::hardware::ROTATION_LOCK
            .set_by(locked, &origin)
            .map_err(|error| error.to_string())
    }

    fn rotation_available(&self) -> bool {
        os_actions::has_accelerometer()
    }
}

/// What the device host reads and drives.
pub struct Device {
    pub reads: Arc<dyn DeviceReads>,
    /// Carries out every change; called on a worker only.
    pub dispatcher: Arc<dyn Dispatcher + Send + Sync>,
    pub preferences: Box<dyn Preferences>,
}

impl Device {
    /// This machine: its OS, its dispatcher and the person's settings.
    #[must_use]
    pub fn os() -> Self {
        Self {
            reads: Arc::new(OsReads),
            dispatcher: Arc::new(os_actions::RealDispatcher),
            preferences: Box::new(StoredPreferences),
        }
    }
}

/// What the panel asked of the desktop that the device host cannot do
/// itself. The session takes them with [`DeviceHost::take_requests`] after
/// a wake.
#[derive(Clone, Debug, PartialEq)]
pub enum SessionRequest {
    /// An effect only the desktop carries out: open Settings, lock the
    /// screen, show the power menu, act on a notification, the clipboard
    /// history or a Devices row, move an accessory, route sound, send what
    /// plays elsewhere, or hold notifications (Do Not Disturb).
    Effect(QsEffect),
    /// Tell the person: a change that did not happen, a network joined.
    Notify { title: String, body: String },
    /// An audio device arrived or left, for the desktop to sound.
    AudioDevice { connected: bool },
}

/// The device behind the panel. Cheap to clone; every clone is the same
/// host.
#[derive(Clone)]
pub struct DeviceHost(Rc<Inner>);

struct Inner {
    state: RefCell<QuickSettingsState>,
    feed: RefCell<Option<Feed>>,
    showing: RefCell<Showing>,
    open: Cell<bool>,
    refresh: RefCell<refresh::QsRefresh>,
    dispatcher: Arc<dyn Dispatcher + Send + Sync>,
    preferences: Box<dyn Preferences>,
    radio: RefCell<Option<RadioTicket>>,
    players: RefCell<Option<players::Players>>,
    requests: RefCell<Vec<SessionRequest>>,
    wifi_scanned_at: Cell<Option<Instant>>,
    wake: Wake,
}

impl DeviceHost {
    /// The device `device`, whose workers call `wake` when what they read
    /// or wrote is ready for [`Self::pump`]. Starts reading everything the
    /// panel shows at once, on a worker, so the top bar's status and which
    /// effects the device can do are known before the panel first opens.
    #[must_use]
    pub fn new(device: Device, wake: Wake) -> Self {
        let Device {
            reads,
            dispatcher,
            preferences,
        } = device;
        let mut state = QuickSettingsState::unread();
        state.arrangement = preferences.arrangement().normalised();
        state.night_light = preferences.night_light();
        state.rotation_lock = preferences.rotation_lock();
        state.rotation_available = preferences.rotation_available();
        let host = Self(Rc::new(Inner {
            state: RefCell::new(state),
            feed: RefCell::new(None),
            showing: RefCell::new(Showing::default()),
            open: Cell::new(false),
            refresh: RefCell::new(refresh::QsRefresh::new(reads, Arc::clone(&wake))),
            dispatcher,
            preferences,
            radio: RefCell::new(None),
            players: RefCell::new(None),
            requests: RefCell::new(Vec::new()),
            wifi_scanned_at: Cell::new(None),
            wake,
        }));
        host.0.refresh.borrow_mut().kick_fast();
        host
    }

    /// This machine's device host.
    #[must_use]
    pub fn os(wake: Wake) -> Self {
        Self::new(Device::os(), wake)
    }

    /// Follow what plays through `link`; a link given before is let go.
    pub fn follow_players(&self, link: PlayerLink) {
        let players = players::Players::start(link, Arc::clone(&self.0.wake));
        *self.0.players.borrow_mut() = Some(players);
    }

    /// The host as the panel takes it ([`crate::prepare`]).
    #[must_use]
    pub fn panel(&self) -> Rc<dyn PanelHost> {
        Rc::new(self.clone())
    }

    /// The panel opened or closed. Opening reads everything it shows
    /// afresh; it shows the last readings meanwhile.
    pub fn set_open(&self, open: bool) {
        let was = self.0.open.replace(open);
        if open && !was {
            let mut refresh = self.0.refresh.borrow_mut();
            refresh.kick_fast();
            refresh.kick_firmware();
            // The Wi-Fi and Bluetooth tiles name what is connected, which
            // only these lists know.
            refresh.kick_wifi_networks();
            refresh.kick_bt_devices();
        }
        if !open {
            self.0.wifi_scanned_at.set(None);
        }
        self.feed_now_playing();
    }

    /// The readings as they stand.
    #[must_use]
    pub fn state(&self) -> QuickSettingsState {
        self.0.state.borrow().clone()
    }

    /// What the panel shows now, as it last said: the session reads what
    /// that page or section lists from what it owns (notifications, the
    /// clipboard history, the device fabric).
    #[must_use]
    pub fn showing(&self) -> Showing {
        self.0.showing.borrow().clone()
    }

    /// Change the readings the session owns (the notifications, the
    /// clipboard history, the Devices rows, the display in use, Do Not
    /// Disturb) and show them.
    pub fn update(&self, change: impl FnOnce(&mut QuickSettingsState)) {
        change(&mut self.0.state.borrow_mut());
        self.feed();
    }

    /// What the panel asked of the desktop since the last call.
    #[must_use]
    pub fn take_requests(&self) -> Vec<SessionRequest> {
        std::mem::take(&mut *self.0.requests.borrow_mut())
    }

    /// When the session should pump again without a wake: the open Wi-Fi
    /// section's next scan, the playhead's next step.
    #[must_use]
    pub fn next_tick(&self) -> Option<Instant> {
        let scan = self
            .0
            .wifi_scanned_at
            .get()
            .and_then(|at| at.checked_add(WIFI_RESCAN));
        let playhead = (self.0.open.get()
            && self
                .0
                .players
                .borrow()
                .as_ref()
                .is_some_and(players::Players::playing))
        .then(|| Instant::now().checked_add(PLAYHEAD))
        .flatten();
        [scan, playhead].into_iter().flatten().min()
    }

    /// Land everything the workers and the players delivered, on the
    /// session's thread. Never blocks. Whether the readings changed.
    pub fn pump(&self) -> bool {
        let before = self.0.state.borrow().clone();
        let readings = self.0.refresh.borrow_mut().drain();
        for reading in readings {
            self.land(reading);
        }
        self.land_radio();
        let now = Instant::now();
        let devices = self
            .0
            .players
            .borrow_mut()
            .as_mut()
            .map(|players| players.drain(now))
            .unwrap_or_default();
        self.0.requests.borrow_mut().extend(
            devices
                .into_iter()
                .map(|connected| SessionRequest::AudioDevice { connected }),
        );
        self.rescan_open_wifi(now);
        self.feed_now_playing();
        self.feed();
        *self.0.state.borrow() != before
    }

    /// Show the readings as they stand.
    fn feed(&self) {
        let feed = self.0.feed.borrow().clone();
        if let Some(feed) = feed {
            let state = self.0.state.borrow().clone();
            feed.set(&state);
        }
    }

    fn feed_now_playing(&self) {
        let playing = self
            .0
            .players
            .borrow_mut()
            .as_mut()
            .and_then(|players| players.now_playing(Instant::now()));
        self.0.state.borrow_mut().now_playing = playing;
    }

    fn tell(&self, title: String, body: String) {
        self.0
            .requests
            .borrow_mut()
            .push(SessionRequest::Notify { title, body });
        (self.0.wake)();
    }

    fn ask(&self, effect: QsEffect) {
        self.0
            .requests
            .borrow_mut()
            .push(SessionRequest::Effect(effect));
        (self.0.wake)();
    }

    fn land(&self, reading: QsReading) {
        let mut told = None;
        {
            let mut state = self.0.state.borrow_mut();
            match reading {
                QsReading::Fast(reading) => apply::fast(&mut state, *reading),
                QsReading::Field(reading) => apply::field(&mut state, reading),
                QsReading::WifiNetworks(result) => {
                    state.wifi_scanning = false;
                    match result {
                        Ok(networks) => {
                            state.wifi_scan_error = None;
                            apply::wifi_networks(&mut state, networks);
                        }
                        Err(error) => {
                            tracing::warn!(target: "tilo::os_actions", op = "list_wifi_networks", %error, "Wi-Fi scan failed");
                            state.wifi_scan_error = Some(error);
                        }
                    }
                }
                QsReading::BtDevices(Some(devices)) => apply::bt_devices(&mut state, devices),
                QsReading::BtDevices(None) => {}
                QsReading::Firmware(updates) => state.firmware_updates = updates,
                QsReading::WifiJoin(join) => told = apply::wifi_join(&mut state, join),
                QsReading::Wrote(done) => {
                    drop(state);
                    self.wrote(done);
                    return;
                }
            }
        }
        if let Some((title, body)) = told {
            self.tell(title, body);
        }
    }

    /// A write is done: say so if it failed, and read back what it wrote
    /// either way, since even a successful request can be normalized by
    /// hardware or policy.
    fn wrote(&self, done: reads::WriteDone) {
        if let Err(error) = &done.result {
            tracing::warn!(target: "tilo::os_actions", op = done.operation, %error, "quick setting failed");
            let (title, body) = done.failure.told(error);
            self.tell(title, body);
        }
        let mut refresh = self.0.refresh.borrow_mut();
        match done.read_back {
            ReadBack::Field(field) => refresh.kick_field(field),
            ReadBack::Accessories => refresh.kick_bt_devices(),
        }
    }

    /// Land a finished radio change.
    fn land_radio(&self) {
        let finished = self
            .0
            .radio
            .borrow()
            .as_ref()
            .is_some_and(RadioTicket::finished);
        if !finished {
            return;
        }
        let Some(ticket) = self.0.radio.borrow_mut().take() else {
            return;
        };
        let landed = ticket.apply(&mut self.0.state.borrow_mut());
        if let Some(failure) = landed.and_then(|landed| landed.failure) {
            self.tell(failure.0, failure.1);
        }
        let mut refresh = self.0.refresh.borrow_mut();
        refresh.kick_field(QsField::of_radio(ticket.change));
        if ticket.change == RadioChange::Wifi(true)
            && ticket.result().is_some_and(|result| result.is_ok())
        {
            {
                let mut state = self.0.state.borrow_mut();
                state.wifi_scanning = true;
                state.wifi_scan_error = None;
            }
            refresh.kick_wifi_networks();
        }
    }

    /// The open Wi-Fi section scans for networks when it opens and again
    /// every [`WIFI_RESCAN`] while it stays open with the radio on.
    fn rescan_open_wifi(&self, now: Instant) {
        let wanted = {
            let state = self.0.state.borrow();
            let showing = self.0.showing.borrow();
            self.0.open.get()
                && showing.page == crate::Page::Root
                && showing.section == Some(QsSection::Wifi)
                && state.wifi
                && !state.radio_change_pending
        };
        if !wanted {
            self.0.wifi_scanned_at.set(None);
            return;
        }
        if self
            .0
            .wifi_scanned_at
            .get()
            .is_some_and(|at| now.saturating_duration_since(at) < WIFI_RESCAN)
        {
            return;
        }
        self.0.wifi_scanned_at.set(Some(now));
        {
            let mut state = self.0.state.borrow_mut();
            state.wifi_scanning = true;
            state.wifi_scan_error = None;
        }
        self.0.refresh.borrow_mut().kick_wifi_rescan();
    }

    /// Carry out `write` on the writer, then read back what it wrote.
    fn write<F>(&self, operation: &'static str, failure: Failure, read_back: ReadBack, write: F)
    where
        F: FnOnce(&dyn Dispatcher) -> Result<(), String> + Send + 'static,
    {
        let dispatcher = Arc::clone(&self.0.dispatcher);
        self.0
            .refresh
            .borrow_mut()
            .write(operation, failure, read_back, move || {
                write(&ValidatedQsActions::new(dispatcher.as_ref()))
            });
    }

    fn start_radio(&self, change: RadioChange) -> Result<(), String> {
        {
            let state = self.0.state.borrow();
            let available = match change {
                RadioChange::Wifi(_) => state.available.wifi || state.radios.wifi_available,
                RadioChange::Airplane(_) => {
                    state.available.airplane_mode
                        || state.radios.wifi_available
                        || state.radios.wwan_available
                }
            };
            if !available {
                return Err("radio action is unavailable".into());
            }
        }
        if self.0.radio.borrow().is_some() {
            return Err("a radio change is already pending".into());
        }
        let work = self
            .0
            .dispatcher
            .prepare_radio_change(change)
            .unwrap_or_else(|| {
                let dispatcher = Arc::clone(&self.0.dispatcher);
                Box::new(move || dispatcher.apply_radio_change(change)) as os_actions::RadioWork
            });
        let ticket = RadioTicket::start(change, work, Some(Arc::clone(&self.0.wake)))?;
        *self.0.radio.borrow_mut() = Some(ticket);
        let mut state = self.0.state.borrow_mut();
        state.radio_change_pending = true;
        state.radio_change = Some(change);
        Ok(())
    }

    fn set_level(&self, change: &LevelChange) -> Result<(), String> {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a level in 0..=1 as a percentage the OS takes in f32"
        )]
        let percent_of = |level: f64| (level.clamp(0.0, 1.0) * 100.0) as f32;
        let mut state = self.0.state.borrow_mut();
        match change {
            LevelChange::Brightness(level) => {
                apply::show_level(&mut state, change);
                drop(state);
                let pct = percent_of(*level);
                self.write(
                    "set_brightness",
                    Failure::Value(t("host-qs-change-failed-brightness")),
                    ReadBack::Field(QsField::Brightness),
                    move |d| d.set_brightness(pct),
                );
            }
            LevelChange::Display(connector, level) => {
                if !state
                    .displays
                    .iter()
                    .any(|display| display.connector == *connector)
                {
                    return Err("that monitor is no longer connected".into());
                }
                apply::show_level(&mut state, change);
                drop(state);
                let (connector, pct) = (connector.clone(), percent_of(*level));
                self.write(
                    "set_display_brightness",
                    Failure::Value(t("host-qs-change-failed-brightness")),
                    ReadBack::Field(QsField::Brightness),
                    move |d| d.set_display_brightness(&connector, pct),
                );
            }
            LevelChange::Volume(level) => {
                // A positive change unmutes a muted output; otherwise the
                // slider moves while the sound stays off.
                let previous = state.status.volume;
                let level = apply::level_of(*level);
                let was_muted = previous.is_some_and(|now| now.muted);
                let unmute = previous.is_some_and(|now| level > 0.0 && level >= now.level);
                state.status.volume = Some(media::audio::default_node::VolumeState {
                    level,
                    muted: was_muted && !unmute,
                });
                drop(state);
                let pct = level * 100.0;
                self.write(
                    "set_volume",
                    Failure::Value(t("host-qs-change-failed-volume")),
                    ReadBack::Field(QsField::Volume),
                    move |d| {
                        d.set_volume(pct)?;
                        if was_muted && unmute {
                            d.set_mute(false)?;
                        }
                        Ok(())
                    },
                );
            }
        }
        Ok(())
    }

    fn set_muted(&self, muted: bool) {
        {
            let mut state = self.0.state.borrow_mut();
            let Some(volume) = state.status.volume.as_mut() else {
                return;
            };
            volume.muted = muted;
        }
        self.write(
            "set_mute",
            Failure::Value(t("host-qs-change-failed-volume")),
            ReadBack::Field(QsField::Volume),
            move |d| d.set_mute(muted),
        );
    }

    fn accessory(&self, address: &str, connect: bool) {
        let name = {
            let mut state = self.0.state.borrow_mut();
            let device = state
                .bt_devices
                .iter_mut()
                .find(|device| device.address == address);
            device.map_or_else(
                || address.to_owned(),
                |device| {
                    device.connected = connect;
                    device.name.clone()
                },
            )
        };
        let title = t_args(
            if connect {
                "qs-bluetooth-connect-failed"
            } else {
                "qs-bluetooth-disconnect-failed"
            },
            &[("device", name.into())],
        );
        let address = address.to_owned();
        self.write(
            if connect {
                "connect_bluetooth"
            } else {
                "disconnect_bluetooth"
            },
            Failure::Refreshing(title),
            ReadBack::Accessories,
            move |d| {
                if connect {
                    d.connect_bluetooth(&address)
                } else {
                    d.disconnect_bluetooth(&address)
                }
            },
        );
    }

    fn set_vpn(&self, profile: &str, on: bool) {
        if let Some(vpn) = self
            .0
            .state
            .borrow_mut()
            .vpns
            .iter_mut()
            .find(|vpn| vpn.name == profile)
        {
            vpn.active = on;
        }
        let title = t_args("qs-vpn-change-failed", &[("profile", profile.into())]);
        let profile = profile.to_owned();
        self.write(
            "set_vpn_connection",
            Failure::Refreshing(title),
            ReadBack::Field(QsField::Vpn),
            move |d| d.set_vpn_connection(&profile, on),
        );
    }

    fn join_wifi(&self, network: String, password: Option<String>) {
        let dispatcher = Arc::clone(&self.0.dispatcher);
        self.0
            .refresh
            .borrow_mut()
            .kick_wifi_join(network, password, move |ssid, password| {
                dispatcher.connect_wifi(ssid, password)
            });
    }

    /// Carry out one of the panel's effects.
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per effect, each handing it to what carries it out"
    )]
    fn run_effect(&self, effect: QsEffect) -> Result<(), String> {
        let state = || self.0.state.borrow_mut();
        match effect {
            QsEffect::SetWifi { on } => self.start_radio(RadioChange::Wifi(on))?,
            QsEffect::ToggleWifi => {
                let on = !state().wifi;
                self.start_radio(RadioChange::Wifi(on))?;
            }
            QsEffect::SetAirplaneMode { on } => self.start_radio(RadioChange::Airplane(on))?,
            QsEffect::ToggleAirplaneMode => {
                let on = !state().radios.airplane_mode;
                self.start_radio(RadioChange::Airplane(on))?;
            }
            QsEffect::SetWired { on } => {
                if let Some(wired) = state().wired.as_mut() {
                    wired.connected = on;
                }
                self.write(
                    "set_wired",
                    Failure::State(t("host-qs-change-failed-wired")),
                    ReadBack::Field(QsField::Wired),
                    move |d| d.set_wired(on),
                );
            }
            QsEffect::ToggleBluetooth => {
                let on = !state().bluetooth;
                return self.run_effect(QsEffect::SetBluetooth { on });
            }
            QsEffect::SetBluetooth { on } => {
                state().bluetooth = on;
                self.write(
                    "set_bluetooth",
                    Failure::State(t("host-qs-change-failed-bluetooth")),
                    ReadBack::Field(QsField::Bluetooth),
                    move |d| d.set_bluetooth(on),
                );
            }
            QsEffect::SetMobileData { on } => {
                state().radios.wwan_enabled = on;
                self.write(
                    "set_mobile_broadband",
                    Failure::State(t("host-qs-change-failed-mobile")),
                    ReadBack::Field(QsField::Mobile),
                    move |d| d.set_mobile_broadband(on),
                );
            }
            QsEffect::SetNightLight { on } => {
                state().night_light = on;
                if let Err(error) = self.0.preferences.set_night_light(on) {
                    tracing::warn!(%error, "night light preference save failed");
                }
            }
            QsEffect::SetRotationLock { locked } => {
                state().rotation_lock = locked;
                if let Err(error) = self.0.preferences.set_rotation_lock(locked) {
                    tracing::warn!(%error, "rotation lock preference save failed");
                }
            }
            QsEffect::SetPowerMode { mode } => {
                state().power_profile = mode;
                let profile = apply::os_power_profile(mode);
                self.write(
                    "set_power_profile",
                    Failure::State(t("host-qs-change-failed-power")),
                    ReadBack::Field(QsField::Power),
                    move |d| d.set_power_profile(profile),
                );
            }
            QsEffect::SetVpn { profile, on } => self.set_vpn(&profile, on),
            QsEffect::SetBrightness { level } => self.set_level(&LevelChange::Brightness(level))?,
            QsEffect::SetDisplayBrightness { display, level } => {
                self.set_level(&LevelChange::Display(display, level))?;
            }
            QsEffect::BrightnessUp => {
                let change = apply::brightness_step(&state(), apply::BRIGHTNESS_STEP);
                self.set_level(&change)?;
            }
            QsEffect::BrightnessDown => {
                let change = apply::brightness_step(&state(), -apply::BRIGHTNESS_STEP);
                self.set_level(&change)?;
            }
            QsEffect::SetVolume { level } => self.set_level(&LevelChange::Volume(level))?,
            QsEffect::VolumeUp => {
                let change = apply::volume_step(&state(), apply::VOLUME_STEP);
                self.set_level(&change)?;
            }
            QsEffect::VolumeDown => {
                let change = apply::volume_step(&state(), -apply::VOLUME_STEP);
                self.set_level(&change)?;
            }
            QsEffect::SetMuted { muted } => self.set_muted(muted),
            QsEffect::ToggleMuted => {
                let muted = !state().volume_muted();
                self.set_muted(muted);
            }
            QsEffect::JoinWifi { network } => {
                {
                    let mut state = state();
                    state.wifi_password = None;
                    state.wifi_joining = Some(network.clone());
                }
                self.join_wifi(network, None);
            }
            QsEffect::AskWifiPassword { network } => {
                state().wifi_password = Some(crate::WifiPasswordPrompt::new(network));
            }
            QsEffect::ConnectWifi { network, password } => {
                if password.is_empty() {
                    return Err("a Wi-Fi password is not empty".into());
                }
                {
                    let mut state = state();
                    let prompt = state
                        .wifi_password
                        .get_or_insert_with(|| crate::WifiPasswordPrompt::new(network.clone()));
                    prompt.connecting = true;
                    prompt.error = None;
                    state.wifi_joining = Some(network.clone());
                }
                self.join_wifi(network, Some(password));
            }
            QsEffect::CancelWifiPassword => state().wifi_password = None,
            QsEffect::ConnectBluetooth { address } => self.accessory(&address, true),
            QsEffect::DisconnectBluetooth { address } => self.accessory(&address, false),
            effect @ (QsEffect::ShowControl { .. }
            | QsEffect::HideControl { .. }
            | QsEffect::MoveControl { .. }
            | QsEffect::ResetControls) => {
                let arranged = {
                    let mut state = state();
                    let visible = crate::faces::visible_controls(&state.arrangement, &state);
                    crate::host::arrange(&mut state.arrangement, &visible, &effect)
                        .then(|| state.arrangement.clone())
                };
                if let Some(arrangement) = arranged
                    && let Err(error) = self.0.preferences.save_arrangement(&arrangement)
                {
                    tracing::warn!(%error, "could not keep the Quick Settings arrangement");
                }
            }
            effect @ (QsEffect::PlayPause
            | QsEffect::PreviousTrack
            | QsEffect::NextTrack
            | QsEffect::Seek { .. }
            | QsEffect::ChoosePlayer { .. }) => {
                self.0
                    .players
                    .borrow_mut()
                    .as_mut()
                    .ok_or_else(|| "nothing plays".to_owned())?
                    .run(&effect)?;
                self.feed_now_playing();
            }
            QsEffect::SetDoNotDisturb { on } => {
                state().do_not_disturb = on;
                self.ask(QsEffect::SetDoNotDisturb { on });
            }
            effect @ (QsEffect::HandOffBluetooth { .. }
            | QsEffect::PressDevice { .. }
            | QsEffect::RouteSound { .. }
            | QsEffect::PasteCopy { .. }
            | QsEffect::PinCopy { .. }
            | QsEffect::RemoveCopy { .. }
            | QsEffect::ClearClipboard
            | QsEffect::OpenNotification { .. }
            | QsEffect::DismissNotification { .. }
            | QsEffect::DismissNotificationGroup { .. }
            | QsEffect::RunNotificationAction { .. }
            | QsEffect::HandNotificationToAgent { .. }
            | QsEffect::ClearNotifications
            | QsEffect::UndoClearNotifications
            | QsEffect::OpenSettings { .. }
            | QsEffect::LockScreen
            | QsEffect::PowerMenu
            | QsEffect::ChooseOutput { .. }) => self.ask(effect),
        }
        Ok(())
    }

    /// Read what the page or section the panel shows lists.
    fn follow_showing(&self) {
        let section = self.0.showing.borrow().section;
        let mut refresh = self.0.refresh.borrow_mut();
        match section {
            // The first scan starts at the next pump; the section keeps
            // scanning while it is open.
            Some(QsSection::Wifi) => self.0.wifi_scanned_at.set(None),
            Some(QsSection::Bluetooth) => refresh.kick_bt_devices(),
            Some(QsSection::Vpn) => refresh.kick_field(QsField::Vpn),
            Some(QsSection::Battery | QsSection::PowerMode) => refresh.kick_field(QsField::Power),
            Some(QsSection::Sound | QsSection::Clipboard | QsSection::Devices) | None => {}
        }
    }
}

impl PanelHost for DeviceHost {
    fn attach(&self, feed: Feed) {
        *self.0.feed.borrow_mut() = Some(feed);
        self.feed();
    }

    fn showing(&self, showing: &Showing) {
        if *self.0.showing.borrow() == *showing {
            return;
        }
        *self.0.showing.borrow_mut() = showing.clone();
        self.follow_showing();
        // The session reads what the page lists of what it owns, and the
        // Wi-Fi section's first scan starts.
        (self.0.wake)();
    }

    fn run(&self, effect: QsEffect) -> Result<(), String> {
        let result = self.run_effect(effect);
        self.feed();
        result
    }
}

/// Exact percentages the OS takes: 0 through 100.
fn percent(value: f32) -> Result<f32, String> {
    if value.is_finite() && (0.0..=100.0).contains(&value) {
        Ok(value)
    } else {
        Err(format!("{value} is not a percentage from 0 through 100"))
    }
}

/// A name the OS takes: not empty and at most `max` characters.
fn name(value: &str, what: &str, max: usize) -> Result<(), String> {
    let length = value.chars().count();
    if length == 0 || length > max {
        Err(format!("a {what} is 1 to {max} characters"))
    } else {
        Ok(())
    }
}

/// The dispatcher as the panel's effects reach it: every level, connector
/// and profile name is checked before it reaches the OS, and a refusal is
/// the effect's answer. Operations that take nothing to check pass through.
struct ValidatedQsActions<'a> {
    inner: &'a dyn Dispatcher,
}

impl<'a> ValidatedQsActions<'a> {
    fn new(inner: &'a dyn Dispatcher) -> Self {
        Self { inner }
    }
}

impl Dispatcher for ValidatedQsActions<'_> {
    fn prepare_radio_change(&self, change: RadioChange) -> Option<os_actions::RadioWork> {
        self.inner.prepare_radio_change(change)
    }

    fn apply_radio_change(
        &self,
        change: RadioChange,
    ) -> Result<Option<os_actions::RadioState>, String> {
        self.inner.apply_radio_change(change)
    }

    fn set_brightness(&self, percent_value: f32) -> Result<(), String> {
        self.inner.set_brightness(percent(percent_value)?)
    }

    fn set_display_brightness(&self, connector: &str, percent_value: f32) -> Result<(), String> {
        name(connector, "display connector", 64)?;
        self.inner
            .set_display_brightness(connector, percent(percent_value)?)
    }

    fn set_volume(&self, percent_value: f32) -> Result<(), String> {
        self.inner.set_volume(percent(percent_value)?)
    }

    fn set_wifi(&self, enabled: bool) -> Result<(), String> {
        self.inner.set_wifi(enabled)
    }

    fn set_power_profile(&self, profile: os_actions::PowerProfile) -> Result<(), String> {
        self.inner.set_power_profile(profile)
    }

    fn set_vpn_connection(&self, connection: &str, enabled: bool) -> Result<(), String> {
        name(connection, "VPN connection name", 255)?;
        self.inner.set_vpn_connection(connection, enabled)
    }

    fn connect_wifi(&self, ssid: &str, password: Option<&str>) -> Result<(), String> {
        self.inner.connect_wifi(ssid, password)
    }

    fn set_bluetooth(&self, enabled: bool) -> Result<(), String> {
        self.inner.set_bluetooth(enabled)
    }

    fn connect_bluetooth(&self, address: &str) -> Result<(), String> {
        self.inner.connect_bluetooth(address)
    }

    fn disconnect_bluetooth(&self, address: &str) -> Result<(), String> {
        self.inner.disconnect_bluetooth(address)
    }

    fn set_wired(&self, enabled: bool) -> Result<(), String> {
        self.inner.set_wired(enabled)
    }

    fn set_airplane_mode(&self, enabled: bool) -> Result<(), String> {
        self.inner.set_airplane_mode(enabled)
    }

    fn set_hotspot(&self, enabled: bool) -> Result<(), String> {
        self.inner.set_hotspot(enabled)
    }

    fn set_mobile_broadband(&self, enabled: bool) -> Result<(), String> {
        self.inner.set_mobile_broadband(enabled)
    }

    fn toggle_mute(&self) -> Result<(), String> {
        self.inner.toggle_mute()
    }

    fn set_mute(&self, muted: bool) -> Result<(), String> {
        self.inner.set_mute(muted)
    }

    fn brightness(&self) -> Result<f32, String> {
        self.inner.brightness()
    }

    fn keyboard_backlight(&self) -> Result<os_actions::KeyboardBacklight, String> {
        self.inner.keyboard_backlight()
    }

    fn set_keyboard_backlight(&self, level: u32) -> Result<(), String> {
        self.inner.set_keyboard_backlight(level)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Recorder {
        calls: RefCell<Vec<String>>,
        fail: Cell<bool>,
    }

    impl Recorder {
        fn result(&self) -> Result<(), String> {
            if self.fail.get() {
                Err("controlled provider failure".into())
            } else {
                Ok(())
            }
        }

        fn push(&self, call: String) -> Result<(), String> {
            self.calls.borrow_mut().push(call);
            self.result()
        }
    }

    impl Dispatcher for Recorder {
        fn set_brightness(&self, percent: f32) -> Result<(), String> {
            self.push(format!("brightness:{percent}"))
        }
        fn set_display_brightness(&self, connector: &str, percent: f32) -> Result<(), String> {
            self.push(format!("brightness:{connector}:{percent}"))
        }
        fn set_volume(&self, percent: f32) -> Result<(), String> {
            self.push(format!("volume:{percent}"))
        }
        fn set_wifi(&self, enabled: bool) -> Result<(), String> {
            self.push(format!("wifi:{enabled}"))
        }
        fn set_bluetooth(&self, enabled: bool) -> Result<(), String> {
            self.push(format!("bluetooth:{enabled}"))
        }
        fn connect_wifi(&self, ssid: &str, _password: Option<&str>) -> Result<(), String> {
            self.push(format!("connect-wifi:{ssid}"))
        }
        fn set_vpn_connection(&self, name: &str, enabled: bool) -> Result<(), String> {
            self.push(format!("vpn:{name}:{enabled}"))
        }
        fn set_power_profile(&self, profile: os_actions::PowerProfile) -> Result<(), String> {
            self.push(format!("power:{profile:?}"))
        }
    }

    /// A value the OS would refuse never reaches it, and the OS's own
    /// refusal is the caller's answer.
    #[test]
    fn values_are_checked_before_they_reach_the_os() {
        let recorder = Recorder::default();
        let validated = ValidatedQsActions::new(&recorder);
        validated.set_brightness(37.5).expect("a percentage passes");
        validated.set_volume(64.0).expect("a percentage passes");
        validated
            .set_display_brightness("DP-2", 40.0)
            .expect("a connector and a percentage pass");
        validated.set_wifi(true).expect("a switch passes");
        validated
            .set_power_profile(os_actions::PowerProfile::Saver)
            .expect("a profile passes");
        validated
            .set_vpn_connection("Work", false)
            .expect("a profile name passes");
        assert_eq!(
            recorder.calls.borrow().as_slice(),
            [
                "brightness:37.5",
                "volume:64",
                "brightness:DP-2:40",
                "wifi:true",
                "power:Saver",
                "vpn:Work:false",
            ]
        );
        assert!(validated.set_brightness(101.0).is_err());
        assert!(validated.set_volume(-1.0).is_err());
        assert!(validated.set_volume(f32::NAN).is_err());
        assert!(validated.set_display_brightness("", 40.0).is_err());
        assert!(validated.set_vpn_connection("", true).is_err());
        assert_eq!(
            recorder.calls.borrow().len(),
            6,
            "nothing invalid dispatched"
        );
        recorder.fail.set(true);
        let error = validated
            .set_brightness(50.0)
            .expect_err("the OS's refusal reaches the caller");
        assert!(error.contains("controlled"));
    }
}
