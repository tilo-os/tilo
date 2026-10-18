//! Brightness per monitor: the laptop panel through its backlight, every
//! external monitor that speaks DDC/CI through its I²C bus, and every other
//! monitor dimmed by the compositor in software.
//!
//! A monitor is known by its DRM connector (`eDP-1`, `DP-2`), the same
//! name the compositor advertises through `wl_output` and publishes to
//! Settings, so UI can put the monitor's EDID name on each slider. What is
//! learnt about a monitor (whether it answers DDC/CI, its brightness scale,
//! its last level) is remembered by its EDID identity, so a monitor that
//! moves to another port keeps it.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::ddc;
use tilo_lock::MutexRecover as _;

const DRM: &str = "/sys/class/drm";
const BACKLIGHT: &str = "/sys/class/backlight";
const DEV: &str = "/dev";

/// How a monitor's brightness is driven.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrightnessTarget {
    /// A `/sys/class/backlight/<device>` directory.
    Backlight(PathBuf),
    /// An `i2c-dev` node, e.g. `/dev/i2c-5`, carrying DDC/CI.
    Ddc(PathBuf),
    /// No hardware control: the compositor darkens what the monitor shows.
    /// It saves no power and the monitor's own brightness stays as it is,
    /// so UI says "dimmed by Tilo" next to it.
    Software,
}

/// One monitor whose brightness can be set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonitorBrightness {
    /// Raw DRM connector label, e.g. `DP-2`.
    pub connector: String,
    pub target: BrightnessTarget,
    /// The monitor's EDID identity ([`edid_key`]), when it has a readable
    /// EDID: what its capability and level are remembered by.
    pub edid: Option<String>,
}

impl MonitorBrightness {
    /// The key this monitor is remembered by: its EDID identity, else its
    /// connector.
    fn memory_key(&self) -> String {
        self.edid
            .clone()
            .unwrap_or_else(|| format!("connector:{}", self.connector))
    }
}

/// Every connected monitor, built-in panel first, each with how its
/// brightness is driven. A monitor on a DDC bus that turns out not to
/// implement DDC/CI is listed as DDC here; the service's probe moves it to
/// software dimming.
pub fn monitors() -> Vec<MonitorBrightness> {
    monitors_in(Path::new(DRM), Path::new(BACKLIGHT), Path::new(DEV))
}

/// Whether a connector drives a built-in panel rather than a port.
pub fn is_builtin_connector(connector: &str) -> bool {
    let upper = connector.to_ascii_uppercase();
    upper.starts_with("EDP") || upper.starts_with("LVDS") || upper.starts_with("DSI")
}

/// A monitor's identity from its EDID: manufacturer, product code and
/// serial number (bytes 8 to 15), which survive a port change and tell two
/// monitors of one model apart when the maker filled the serial in. `None`
/// for anything that is not an EDID base block.
pub fn edid_key(edid: &[u8]) -> Option<String> {
    const HEADER: [u8; 8] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];
    if edid.get(..8)? != HEADER {
        return None;
    }
    let identity = edid.get(8..16)?;
    let mut key = String::from("edid:");
    for byte in identity {
        let _ = write!(key, "{byte:02x}");
    }
    Some(key)
}

fn monitors_in(drm: &Path, backlight: &Path, dev: &Path) -> Vec<MonitorBrightness> {
    let Ok(entries) = fs::read_dir(drm) else {
        return Vec::new();
    };
    let mut connectors: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            // `card0-DP-2`; `card0` itself and render nodes have no dash.
            let (card, connector) = name.split_once('-')?;
            card.starts_with("card")
                .then(|| (connector.to_owned(), entry.path()))
        })
        .filter(|(_, path)| {
            fs::read_to_string(path.join("status")).is_ok_and(|s| s.trim() == "connected")
        })
        .collect();
    connectors.sort_by(|a, b| {
        is_builtin_connector(&b.0)
            .cmp(&is_builtin_connector(&a.0))
            .then_with(|| a.0.cmp(&b.0))
    });
    let mut panel_backlight = crate::display::preferred_device(backlight);
    connectors
        .into_iter()
        .map(|(connector, path)| {
            let target = if is_builtin_connector(&connector) {
                // One backlight drives one panel; a second built-in
                // connector (a detachable keyboard dock's) is dimmed in
                // software.
                panel_backlight
                    .take()
                    .map_or(BrightnessTarget::Software, BrightnessTarget::Backlight)
            } else {
                ddc_bus(&path).map_or(BrightnessTarget::Software, |bus| {
                    BrightnessTarget::Ddc(dev.join(bus))
                })
            };
            let edid = fs::read(path.join("edid"))
                .ok()
                .and_then(|bytes| edid_key(&bytes));
            MonitorBrightness {
                connector,
                target,
                edid,
            }
        })
        .collect()
}

/// The I²C adapter a connector carries: the `ddc` link most drivers set,
/// else a DP AUX adapter registered beneath the connector.
fn ddc_bus(connector: &Path) -> Option<String> {
    if let Ok(link) = fs::read_link(connector.join("ddc"))
        && let Some(name) = link.file_name().and_then(|n| n.to_str())
        && name.starts_with("i2c-")
    {
        return Some(name.to_owned());
    }
    let mut adapters: Vec<String> = fs::read_dir(connector)
        .ok()?
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| {
            name.strip_prefix("i2c-")
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        })
        .collect();
    adapters.sort();
    adapters.into_iter().next()
}

/// A monitor's brightness as read from its hardware.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reading {
    /// `0.0..=1.0`.
    pub level: f32,
    /// The top of a DDC/CI monitor's own scale, so a write need not read it
    /// again first.
    pub maximum: Option<u16>,
}

impl BrightnessTarget {
    /// Current brightness.
    ///
    /// # Errors
    ///
    /// Returns why the monitor could not be read; always for
    /// [`Self::Software`], which has no hardware to read.
    pub fn read(&self) -> Result<Reading, String> {
        match self {
            Self::Backlight(device) => crate::display::get_backlight(device).map(|level| Reading {
                level,
                maximum: None,
            }),
            Self::Ddc(node) => {
                let value = read_ddc(node)?;
                let level = value
                    .fraction()
                    .ok_or_else(|| "monitor reports no brightness range".to_owned())?;
                Ok(Reading {
                    level,
                    maximum: Some(value.maximum),
                })
            }
            Self::Software => Err("software dimming has no hardware level".into()),
        }
    }

    /// Current brightness as a `0.0..=1.0` fraction.
    ///
    /// # Errors
    ///
    /// Returns why the monitor could not be read.
    pub fn get(&self) -> Result<f32, String> {
        self.read().map(|reading| reading.level)
    }

    /// Set brightness to `pct` percent, clamped to `[0, 100]`. A DDC/CI
    /// monitor whose `maximum` is known is written without reading it
    /// first. Software dimming is the compositor's to apply, so it is a
    /// no-op here.
    ///
    /// # Errors
    ///
    /// Returns why the monitor could not be written.
    pub fn set_with_maximum(&self, pct: f32, maximum: Option<u16>) -> Result<(), String> {
        let pct = pct.clamp(0.0, 100.0);
        match self {
            Self::Backlight(device) => crate::display::set_backlight(device, pct),
            Self::Ddc(node) => {
                let maximum = match maximum {
                    Some(maximum) => maximum,
                    None => read_ddc(node)?.maximum,
                };
                write_ddc(node, ddc_raw(pct, maximum))
            }
            Self::Software => Ok(()),
        }
    }

    /// Set brightness to `pct` percent, clamped to `[0, 100]`.
    ///
    /// # Errors
    ///
    /// Returns why the monitor could not be written.
    pub fn set(&self, pct: f32) -> Result<(), String> {
        self.set_with_maximum(pct, None)
    }
}

/// Percent → the monitor's raw scale, rounding to the nearest step.
fn ddc_raw(pct: f32, maximum: u16) -> u16 {
    let raw = (pct / 100.0 * f32::from(maximum)).round();
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped into 0..=maximum, which is a u16"
    )]
    let raw = raw.clamp(0.0, f32::from(maximum)) as u16;
    raw
}

#[cfg(target_os = "linux")]
fn read_ddc(node: &Path) -> Result<ddc::VcpValue, String> {
    let mut device = ddc::I2cDevice::open(node).map_err(|e| format!("{}: {e}", node.display()))?;
    ddc::get_vcp(&mut device, ddc::VCP_BRIGHTNESS).map_err(|e| e.to_string())
}

#[cfg(target_os = "linux")]
fn write_ddc(node: &Path, value: u16) -> Result<(), String> {
    let mut device = ddc::I2cDevice::open(node).map_err(|e| format!("{}: {e}", node.display()))?;
    ddc::set_vcp(&mut device, ddc::VCP_BRIGHTNESS, value).map_err(|e| e.to_string())
}

#[cfg(not(target_os = "linux"))]
fn read_ddc(_node: &Path) -> Result<ddc::VcpValue, String> {
    Err("DDC/CI needs Linux i2c-dev".into())
}

#[cfg(not(target_os = "linux"))]
fn write_ddc(_node: &Path, _value: u16) -> Result<(), String> {
    Err("DDC/CI needs Linux i2c-dev".into())
}

/// The minimum gap between two writes to one DDC/CI monitor. Faster than
/// this and many monitors drop commands or lock up their OSD.
const DDC_MIN_INTERVAL: Duration = Duration::from_millis(120);

/// The least light software dimming leaves, in linear light: white at a
/// fifth of the monitor's own white. On an LCD the backlight keeps blacks
/// where they were, so text stays readable at the bottom of the slider.
pub const SOFTWARE_DIM_FLOOR: f32 = 0.2;

/// The share of linear light software dimming lets through at slider
/// position `level` (`0.0..=1.0`). The slider is perceptual, as a
/// backlight's feels: light falls with the position raised to 2.2, so
/// each step looks like the same change, and the bottom of the slider is
/// [`SOFTWARE_DIM_FLOOR`], never black.
pub fn software_luminance(level: f32) -> f32 {
    let level = if level.is_finite() {
        level.clamp(0.0, 1.0)
    } else {
        1.0
    };
    SOFTWARE_DIM_FLOOR + (1.0 - SOFTWARE_DIM_FLOOR) * level.powf(2.2)
}

/// Every monitor's next level when all of them move together by `delta`
/// (a fraction of full scale): the brightest moves by `delta` and the rest
/// keep their share of its level, so a monitor set at half the panel's
/// brightness stays at half. Monitors that all sit at zero rise together.
/// Clamped to `0.0..=1.0`.
pub fn step_together(levels: &[f32], delta: f32) -> Vec<f32> {
    let brightest = levels
        .iter()
        .copied()
        .filter(|level| level.is_finite())
        .fold(0.0_f32, f32::max)
        .clamp(0.0, 1.0);
    let target = (brightest + delta).clamp(0.0, 1.0);
    levels
        .iter()
        .map(|&level| {
            if brightest <= f32::EPSILON || !level.is_finite() {
                target
            } else {
                (level.clamp(0.0, 1.0) * target / brightest).clamp(0.0, 1.0)
            }
        })
        .collect()
}

/// How a monitor's level is set, for UI that labels it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BrightnessKind {
    /// The panel's own backlight.
    #[default]
    Backlight,
    /// The monitor's own brightness, over DDC/CI.
    Ddc,
    /// Darkened by the compositor.
    Software,
}

impl BrightnessTarget {
    fn kind(&self) -> BrightnessKind {
        match self {
            Self::Backlight(_) => BrightnessKind::Backlight,
            Self::Ddc(_) => BrightnessKind::Ddc,
            Self::Software => BrightnessKind::Software,
        }
    }
}

/// One monitor's brightness as last read or set.
#[derive(Clone, Debug, PartialEq)]
pub struct MonitorLevel {
    pub connector: String,
    /// `0.0..=1.0`.
    pub level: f32,
    pub kind: BrightnessKind,
}

/// What is remembered about one monitor, by its EDID identity.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Remembered {
    /// Whether it answered DDC/CI when last probed.
    pub ddc: Option<bool>,
    /// The top of its DDC/CI brightness scale.
    pub maximum: Option<u16>,
    /// Its last level, `0.0..=1.0`. For a software-dimmed monitor this is
    /// the level, restored when it comes back.
    pub level: Option<f32>,
}

/// Every remembered monitor, as stored on disk.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Memory {
    pub monitors: BTreeMap<String, Remembered>,
}

impl Memory {
    fn load(path: &Path) -> Self {
        fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save(&self, path: &Path) {
        let Ok(bytes) = serde_json::to_vec_pretty(self) else {
            return;
        };
        let written = path
            .parent()
            .map_or(Ok(()), fs::create_dir_all)
            .and_then(|()| {
                // Written aside and renamed, so a reader in another process
                // never sees half a file.
                let partial = path.with_extension("json.partial");
                fs::write(&partial, bytes)?;
                fs::rename(&partial, path)
            });
        if let Err(error) = written {
            tracing::debug!(
                target: "tilo::display",
                path = %path.display(),
                %error,
                "could not remember monitor brightness"
            );
        }
    }
}

/// Where the brightness memory is kept: `$XDG_STATE_HOME/tilo`, else
/// `~/.local/state/tilo`.
pub fn default_memory_path() -> Option<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|home| home.join(".local/state"))
        })?;
    Some(state.join("tilo/display-brightness.json"))
}

enum Request {
    Rescan,
    /// Write the memory to disk.
    Remember,
    /// Call once every request sent before it is done.
    Then(Box<dyn FnOnce() + Send>),
}

/// One controllable monitor.
#[derive(Clone, Debug)]
struct Entry {
    monitor: MonitorBrightness,
    level: f32,
    maximum: Option<u16>,
}

#[derive(Default)]
struct Shared {
    /// Every controllable monitor with its last known level.
    monitors: Vec<Entry>,
    memory: Memory,
}

impl Shared {
    fn remember(&mut self, entry: &Entry) {
        let slot = self
            .memory
            .monitors
            .entry(entry.monitor.memory_key())
            .or_default();
        slot.level = Some(entry.level);
        match entry.monitor.target {
            BrightnessTarget::Ddc(_) => {
                slot.ddc = Some(true);
                slot.maximum = entry.maximum.or(slot.maximum);
            }
            BrightnessTarget::Software if entry.monitor.edid.is_some() => {
                // Only a monitor on a bus is said not to answer; a port
                // with no bus at all says nothing about the monitor.
                if slot.ddc.is_none() {
                    slot.ddc = Some(false);
                }
            }
            BrightnessTarget::Backlight(_) | BrightnessTarget::Software => {}
        }
    }
}

type Discover = dyn Fn() -> Vec<MonitorBrightness> + Send + Sync;
type ReadLevel = dyn Fn(&BrightnessTarget) -> Result<Reading, String> + Send + Sync;
type Apply = dyn Fn(&BrightnessTarget, f32, Option<u16>) -> Result<(), String> + Send + Sync;
type OnChange = Arc<dyn Fn() + Send + Sync>;

/// A pending hardware write and whether one is being made.
#[derive(Default)]
struct WriterState {
    pending: Option<(BrightnessTarget, f32, Option<u16>)>,
    busy: bool,
    closed: bool,
}

/// The one thread writing one monitor: at most one write in flight, and
/// every value asked for while it is busy collapses into the newest. A
/// slow monitor never holds up another one.
struct Writer {
    state: Arc<(Mutex<WriterState>, Condvar)>,
}

impl Writer {
    fn spawn(connector: &str, apply: Arc<Apply>, sleep: fn(Duration)) -> Option<Self> {
        let state = Arc::new((Mutex::new(WriterState::default()), Condvar::new()));
        let worker = Arc::clone(&state);
        let name = connector.to_owned();
        std::thread::Builder::new()
            .name(format!("tilo-brightness-{connector}"))
            .spawn(move || write_loop(&worker, &*apply, sleep, &name))
            .map_err(|error| {
                tracing::warn!(target: "tilo::display", %error, "brightness writer did not start");
            })
            .ok()?;
        Some(Self { state })
    }

    fn send(&self, target: BrightnessTarget, pct: f32, maximum: Option<u16>) {
        let (lock, wake) = &*self.state;
        lock.lock_recover().pending = Some((target, pct, maximum));
        wake.notify_all();
    }

    /// Wait until nothing is pending or being written.
    #[cfg(test)]
    fn wait_idle(&self) {
        wait_idle(&self.state);
    }
}

/// Wait until the writer owning `state` has nothing pending or in flight.
fn wait_idle(state: &(Mutex<WriterState>, Condvar)) {
    let (lock, wake) = state;
    let guard = lock.lock_recover();
    drop(
        wake.wait_while(guard, |state| state.pending.is_some() || state.busy)
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
}

impl Drop for Writer {
    fn drop(&mut self) {
        let (lock, wake) = &*self.state;
        lock.lock_recover().closed = true;
        wake.notify_all();
    }
}

fn write_loop(
    state: &(Mutex<WriterState>, Condvar),
    apply: &Apply,
    sleep: fn(Duration),
    connector: &str,
) {
    let (lock, wake) = state;
    let mut last_write: Option<Instant> = None;
    loop {
        let (target, pct, maximum) = {
            let mut guard = lock.lock_recover();
            loop {
                if let Some(next) = guard.pending.take() {
                    guard.busy = true;
                    break next;
                }
                if guard.closed {
                    return;
                }
                guard = wake
                    .wait(guard)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        };
        if matches!(target, BrightnessTarget::Ddc(_))
            && let Some(wait) = last_write.and_then(|at| DDC_MIN_INTERVAL.checked_sub(at.elapsed()))
        {
            sleep(wait);
        }
        if let Err(error) = apply(&target, pct, maximum) {
            tracing::warn!(
                target: "tilo::display",
                connector,
                %error,
                "brightness write failed"
            );
        }
        last_write = Some(Instant::now());
        lock.lock_recover().busy = false;
        wake.notify_all();
    }
}

/// Brightness for every monitor. Hardware writes run on a thread per
/// monitor, so a slider drag or a held brightness key never waits on a
/// monitor and a slow one never holds up another. Levels are cached: reads
/// are instant and reflect the last value set. Software-dimmed monitors
/// are the compositor's to darken: it follows [`Self::software_levels`],
/// told of each change by [`Self::on_change`].
pub struct BrightnessService {
    shared: Arc<Mutex<Shared>>,
    writers: Mutex<BTreeMap<String, Writer>>,
    tx: Option<Sender<Request>>,
    discover: Arc<Discover>,
    read: Arc<ReadLevel>,
    apply: Arc<Apply>,
    sleep: fn(Duration),
    /// Serialises probes: two threads must not talk to one bus at once.
    probing: Arc<Mutex<()>>,
    /// Bumped whenever a software-dimmed level or the monitor list changes.
    generation: Arc<AtomicU64>,
    on_change: Arc<Mutex<Option<OnChange>>>,
    memory_path: Option<PathBuf>,
}

impl BrightnessService {
    /// The process-wide service, started (and its monitors scanned) on
    /// first use.
    pub fn global() -> &'static Self {
        static SERVICE: OnceLock<BrightnessService> = OnceLock::new();
        SERVICE.get_or_init(|| {
            let service = Self::spawn(
                Arc::new(monitors),
                Arc::new(BrightnessTarget::read),
                Arc::new(BrightnessTarget::set_with_maximum),
                std::thread::sleep,
                default_memory_path(),
            );
            service.rescan();
            service
        })
    }

    fn spawn(
        discover: Arc<Discover>,
        read: Arc<ReadLevel>,
        apply: Arc<Apply>,
        sleep: fn(Duration),
        memory_path: Option<PathBuf>,
    ) -> Self {
        let memory = memory_path.as_deref().map(Memory::load).unwrap_or_default();
        let shared = Arc::new(Mutex::new(Shared {
            monitors: Vec::new(),
            memory,
        }));
        let (tx, rx) = channel();
        let generation = Arc::new(AtomicU64::new(0));
        let on_change: Arc<Mutex<Option<OnChange>>> = Arc::new(Mutex::new(None));
        let probing = Arc::new(Mutex::new(()));
        let worker = Worker {
            shared: Arc::clone(&shared),
            discover: Arc::clone(&discover),
            read: Arc::clone(&read),
            probing: Arc::clone(&probing),
            generation: Arc::clone(&generation),
            on_change: Arc::clone(&on_change),
            memory_path: memory_path.clone(),
        };
        let spawned = std::thread::Builder::new()
            .name("tilo-brightness".into())
            .spawn(move || worker.run(&rx));
        if let Err(error) = &spawned {
            tracing::warn!(target: "tilo::display", %error, "brightness worker did not start");
        }
        Self {
            shared,
            writers: Mutex::new(BTreeMap::new()),
            tx: spawned.ok().map(|_| tx),
            discover,
            read,
            apply,
            sleep,
            probing,
            generation,
            on_change,
            memory_path,
        }
    }

    /// [`Self::rescan`] on the calling thread, returning once the levels
    /// are fresh. Probing a monitor that just appeared over DDC/CI takes
    /// tens of milliseconds, so only a worker thread may call this.
    pub fn rescan_blocking(&self) -> Vec<MonitorLevel> {
        let changed = probe(&self.shared, &*self.discover, &*self.read, &self.probing);
        if changed {
            notify(&self.generation, &self.on_change);
        }
        self.levels()
    }

    /// Re-discover monitors after a hotplug, or when a panel that shows
    /// them opens. Monitors already known keep their cached level and are
    /// not asked again; a monitor that just appeared is probed once.
    pub fn rescan(&self) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Request::Rescan);
        }
    }

    /// Call `on_change` (from any thread) whenever a software-dimmed level
    /// or the set of monitors changes: the compositor's wake.
    pub fn on_change(&self, on_change: Arc<dyn Fn() + Send + Sync>) {
        *self.on_change.lock_recover() = Some(on_change);
    }

    /// Bumped on every change [`Self::on_change`] reports; equal values
    /// mean nothing changed.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Call `done` once every rescan and write asked for before it has
    /// finished, so a reader of [`Self::levels`] learns when they are fresh
    /// instead of reading them on a timer. Without a worker it is called at
    /// once.
    pub fn when_idle(&self, done: Box<dyn FnOnce() + Send>) {
        let idle: Vec<Arc<(Mutex<WriterState>, Condvar)>> = self
            .writers
            .lock_recover()
            .values()
            .map(|writer| Arc::clone(&writer.state))
            .collect();
        let done: Box<dyn FnOnce() + Send> = Box::new(move || {
            for state in &idle {
                wait_idle(state);
            }
            done();
        });
        let unsent = match &self.tx {
            Some(tx) => match tx.send(Request::Then(done)) {
                Err(std::sync::mpsc::SendError(Request::Then(done))) => Some(done),
                Ok(()) | Err(_) => None,
            },
            None => Some(done),
        };
        if let Some(done) = unsent {
            done();
        }
    }

    /// The controllable monitors, built-in panel first.
    pub fn levels(&self) -> Vec<MonitorLevel> {
        self.lock()
            .monitors
            .iter()
            .map(|entry| MonitorLevel {
                connector: entry.monitor.connector.clone(),
                level: entry.level,
                kind: entry.monitor.target.kind(),
            })
            .collect()
    }

    /// Every software-dimmed monitor and its slider level (`0.0..=1.0`);
    /// [`software_luminance`] is the light it lets through.
    pub fn software_levels(&self) -> Vec<(String, f32)> {
        self.lock()
            .monitors
            .iter()
            .filter(|entry| entry.monitor.target == BrightnessTarget::Software)
            .map(|entry| (entry.monitor.connector.clone(), entry.level))
            .collect()
    }

    /// Set `connector` to `pct` percent. Never blocks; `false` when no such
    /// monitor is controllable.
    pub fn set(&self, connector: &str, pct: f32) -> bool {
        let pct = pct.clamp(0.0, 100.0);
        let entry = {
            let mut shared = self.lock();
            let Some(entry) = shared
                .monitors
                .iter_mut()
                .find(|entry| entry.monitor.connector == connector)
            else {
                return false;
            };
            entry.level = pct / 100.0;
            let entry = entry.clone();
            shared.remember(&entry);
            entry
        };
        if entry.monitor.target == BrightnessTarget::Software {
            notify(&self.generation, &self.on_change);
        } else {
            let mut writers = self.writers.lock_recover();
            if !writers.contains_key(connector)
                && let Some(writer) = Writer::spawn(connector, Arc::clone(&self.apply), self.sleep)
            {
                writers.insert(connector.to_owned(), writer);
            }
            let Some(writer) = writers.get(connector) else {
                return false;
            };
            writer.send(entry.monitor.target.clone(), pct, entry.maximum);
            drop(writers);
        }
        if self.memory_path.is_some()
            && let Some(tx) = &self.tx
        {
            let _ = tx.send(Request::Remember);
        }
        true
    }

    /// Move one monitor's brightness by `delta` (a fraction): the one on
    /// `connector` when it is controllable, else the first. Returns the
    /// monitor and its new level.
    pub fn step(&self, connector: Option<&str>, delta: f32) -> Option<MonitorLevel> {
        let levels = self.levels();
        let current = connector
            .and_then(|c| levels.iter().find(|level| level.connector == c))
            .or_else(|| levels.first())?;
        let next = (current.level + delta).clamp(0.0, 1.0);
        self.set(&current.connector, next * 100.0)
            .then(|| MonitorLevel {
                level: next,
                ..current.clone()
            })
    }

    /// Re-read the memory another process wrote (Settings setting a
    /// software-dimmed monitor), taking its software levels. Returns
    /// whether a level changed.
    pub fn reload_memory(&self) -> bool {
        let Some(path) = self.memory_path.as_deref() else {
            return false;
        };
        let memory = Memory::load(path);
        let changed = {
            let mut shared = self.lock();
            let mut changed = false;
            let Shared { monitors, .. } = &mut *shared;
            for entry in monitors
                .iter_mut()
                .filter(|entry| entry.monitor.target == BrightnessTarget::Software)
            {
                if let Some(level) = memory
                    .monitors
                    .get(&entry.monitor.memory_key())
                    .and_then(|remembered| remembered.level)
                    && (level - entry.level).abs() > f32::EPSILON
                {
                    entry.level = level.clamp(0.0, 1.0);
                    changed = true;
                }
            }
            shared.memory = memory;
            changed
        };
        if changed {
            notify(&self.generation, &self.on_change);
        }
        changed
    }

    /// Follow the memory file for writes from other processes, on a thread
    /// that sleeps in `inotify` until one happens. The compositor calls this
    /// once, so a level Settings sets reaches the screen.
    pub fn follow_memory(&'static self) {
        let Some(path) = self.memory_path.clone() else {
            return;
        };
        let spawned = std::thread::Builder::new()
            .name("tilo-brightness-memory".into())
            .spawn(move || follow(&path, || self.reload_memory()));
        if let Err(error) = spawned {
            tracing::warn!(target: "tilo::display", %error, "brightness memory follower did not start");
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared.lock_recover()
    }
}

/// Watch `path`'s directory and call `changed` whenever `path` is written
/// there (by a rename into place, which is how [`Memory::save`] writes).
#[cfg(not(target_os = "linux"))]
fn follow(_path: &Path, _changed: impl Fn() -> bool) {}

/// Watch `path`'s directory and call `changed` whenever `path` is written
/// there (by a rename into place, which is how [`Memory::save`] writes).
#[cfg(target_os = "linux")]
fn follow(path: &Path, changed: impl Fn() -> bool) {
    use rustix::fs::inotify;
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return;
    };
    if fs::create_dir_all(dir).is_err() {
        return;
    }
    let Ok(fd) = inotify::init(inotify::CreateFlags::CLOEXEC) else {
        return;
    };
    if inotify::add_watch(
        &fd,
        dir,
        inotify::WatchFlags::CLOSE_WRITE | inotify::WatchFlags::MOVED_TO,
    )
    .is_err()
    {
        return;
    }
    let mut buffer = [std::mem::MaybeUninit::<u8>::uninit(); 4096];
    let mut reader = inotify::Reader::new(&fd, &mut buffer);
    loop {
        match reader.next() {
            Ok(event) => {
                if event
                    .file_name()
                    .is_some_and(|file| file.to_bytes() == name.as_encoded_bytes())
                {
                    changed();
                }
            }
            Err(rustix::io::Errno::INTR) => {}
            Err(_) => return,
        }
    }
}

fn notify(generation: &AtomicU64, on_change: &Mutex<Option<OnChange>>) {
    generation.fetch_add(1, Ordering::AcqRel);
    let callback = on_change.lock_recover().clone();
    if let Some(callback) = callback {
        callback();
    }
}

/// The background half of [`BrightnessService`]: rescans and memory writes.
struct Worker {
    shared: Arc<Mutex<Shared>>,
    discover: Arc<Discover>,
    read: Arc<ReadLevel>,
    probing: Arc<Mutex<()>>,
    generation: Arc<AtomicU64>,
    on_change: Arc<Mutex<Option<OnChange>>>,
    memory_path: Option<PathBuf>,
}

impl Worker {
    fn run(&self, rx: &Receiver<Request>) {
        while let Ok(first) = rx.recv() {
            let mut rescan = false;
            let mut remember = false;
            let mut then = Vec::new();
            for request in std::iter::once(first).chain(rx.try_iter()) {
                match request {
                    Request::Rescan => rescan = true,
                    Request::Remember => remember = true,
                    Request::Then(done) => then.push(done),
                }
            }
            if rescan && probe(&self.shared, &*self.discover, &*self.read, &self.probing) {
                remember = true;
                notify(&self.generation, &self.on_change);
            }
            if remember && let Some(path) = &self.memory_path {
                let memory = self.shared.lock_recover().memory.clone();
                memory.save(path);
            }
            for done in then {
                done();
            }
        }
    }
}

/// Bring the monitor list in line with what is connected. A monitor
/// already known on the same connector keeps its entry and is not asked
/// again: probing is once per appearance. A new one is read, and one on a
/// DDC bus that does not answer DDC/CI is dimmed in software. Returns
/// whether the list changed.
fn probe(
    shared: &Mutex<Shared>,
    discover: &Discover,
    read: &ReadLevel,
    probing: &Mutex<()>,
) -> bool {
    let _probing = probing.lock_recover();
    let discovered = discover();
    let (known, memory) = {
        let shared = shared.lock_recover();
        (shared.monitors.clone(), shared.memory.clone())
    };
    let mut next = Vec::with_capacity(discovered.len());
    for monitor in discovered {
        if let Some(entry) = known.iter().find(|entry| {
            entry.monitor.connector == monitor.connector && entry.monitor.edid == monitor.edid
        }) {
            next.push(entry.clone());
            continue;
        }
        let remembered = memory
            .monitors
            .get(&monitor.memory_key())
            .cloned()
            .unwrap_or_default();
        let software = |monitor: MonitorBrightness| Entry {
            monitor: MonitorBrightness {
                target: BrightnessTarget::Software,
                ..monitor
            },
            level: remembered.level.unwrap_or(1.0).clamp(0.0, 1.0),
            maximum: None,
        };
        let entry = if monitor.target == BrightnessTarget::Software {
            software(monitor)
        } else {
            match read(&monitor.target) {
                Ok(reading) => Entry {
                    level: reading.level,
                    maximum: reading.maximum.or(remembered.maximum),
                    monitor,
                },
                Err(error) => {
                    tracing::debug!(
                        target: "tilo::display",
                        connector = %monitor.connector,
                        %error,
                        "no hardware brightness control; dimming in software"
                    );
                    software(monitor)
                }
            }
        };
        next.push(entry);
    }
    let mut shared = shared.lock_recover();
    let changed = shared.monitors.len() != next.len()
        || shared
            .monitors
            .iter()
            .zip(&next)
            .any(|(a, b)| a.monitor != b.monitor);
    for entry in &next {
        shared.remember(entry);
    }
    shared.monitors = next;
    changed
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests assert exact values")]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct Tree {
        root: tempfile::TempDir,
    }

    impl Tree {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            for dir in ["drm", "backlight", "dev", "i2c"] {
                fs::create_dir_all(root.path().join(dir)).unwrap();
            }
            Self { root }
        }
        fn path(&self, sub: &str) -> PathBuf {
            self.root.path().join(sub)
        }
        fn connector(&self, name: &str, status: &str) -> PathBuf {
            let dir = self.path("drm").join(name);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("status"), format!("{status}\n")).unwrap();
            dir
        }
        fn backlight(&self, name: &str) {
            let dir = self.path("backlight").join(name);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("brightness"), "10\n").unwrap();
            fs::write(dir.join("max_brightness"), "100\n").unwrap();
        }
        fn monitors(&self) -> Vec<MonitorBrightness> {
            monitors_in(
                &self.path("drm"),
                &self.path("backlight"),
                &self.path("dev"),
            )
        }
    }

    fn edid(serial: u8) -> Vec<u8> {
        let mut edid = vec![0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];
        edid.extend_from_slice(&[0x10, 0xAC, 0x42, 0xA1, serial, 0, 0, 0, 1, 0x20]);
        edid.resize(128, 0);
        edid
    }

    #[test]
    fn each_connected_monitor_gets_its_own_control() {
        let tree = Tree::new();
        tree.backlight("intel_backlight");
        tree.connector("card0-eDP-1", "connected");
        let hdmi = tree.connector("card0-HDMI-A-1", "connected");
        fs::write(hdmi.join("edid"), edid(7)).unwrap();
        let bus = tree.path("i2c/i2c-5");
        fs::create_dir_all(&bus).unwrap();
        std::os::unix::fs::symlink(&bus, hdmi.join("ddc")).unwrap();
        let dp = tree.connector("card0-DP-2", "connected");
        fs::create_dir_all(dp.join("i2c-9")).unwrap();
        fs::create_dir_all(dp.join("power")).unwrap();
        tree.connector("card0-DP-3", "disconnected");
        // A connected port without any I²C adapter is dimmed in software.
        tree.connector("card0-Virtual-1", "connected");
        fs::create_dir_all(tree.path("drm/card0")).unwrap();

        assert_eq!(
            tree.monitors(),
            vec![
                MonitorBrightness {
                    connector: "eDP-1".into(),
                    target: BrightnessTarget::Backlight(tree.path("backlight/intel_backlight")),
                    edid: None,
                },
                MonitorBrightness {
                    connector: "DP-2".into(),
                    target: BrightnessTarget::Ddc(tree.path("dev/i2c-9")),
                    edid: None,
                },
                MonitorBrightness {
                    connector: "HDMI-A-1".into(),
                    target: BrightnessTarget::Ddc(tree.path("dev/i2c-5")),
                    edid: Some("edid:10ac42a107000000".into()),
                },
                MonitorBrightness {
                    connector: "Virtual-1".into(),
                    target: BrightnessTarget::Software,
                    edid: None,
                },
            ]
        );
    }

    #[test]
    fn a_panel_without_a_backlight_is_dimmed_in_software() {
        let tree = Tree::new();
        tree.connector("card0-eDP-1", "connected");
        assert_eq!(
            tree.monitors(),
            vec![MonitorBrightness {
                connector: "eDP-1".into(),
                target: BrightnessTarget::Software,
                edid: None,
            }]
        );
    }

    #[test]
    fn an_edid_identity_is_its_maker_product_and_serial() {
        assert_eq!(edid_key(&edid(3)).as_deref(), Some("edid:10ac42a103000000"));
        assert_ne!(edid_key(&edid(3)), edid_key(&edid(4)), "two of one model");
        assert_eq!(edid_key(&[0u8; 128]), None, "not an EDID");
        assert_eq!(edid_key(&edid(3)[..12]), None, "cut short");
    }

    #[test]
    fn the_backlight_target_reads_and_writes_its_own_device() {
        let tree = Tree::new();
        tree.backlight("amdgpu_bl0");
        let target = BrightnessTarget::Backlight(tree.path("backlight/amdgpu_bl0"));
        assert!((target.get().unwrap() - 0.1).abs() < 1e-6);
        target.set(64.0).unwrap();
        assert_eq!(
            fs::read_to_string(tree.path("backlight/amdgpu_bl0/brightness")).unwrap(),
            "64"
        );
    }

    #[test]
    fn percent_maps_onto_the_monitors_own_scale() {
        assert_eq!(ddc_raw(50.0, 100), 50);
        assert_eq!(ddc_raw(50.0, 255), 128);
        assert_eq!(ddc_raw(150.0, 100), 100);
        assert_eq!(ddc_raw(-5.0, 100), 0);
    }

    #[test]
    fn software_dimming_is_perceptual_monotonic_and_never_black() {
        assert_eq!(software_luminance(1.0), 1.0);
        assert_eq!(software_luminance(0.0), SOFTWARE_DIM_FLOOR);
        assert_eq!(
            software_luminance(f32::NAN),
            1.0,
            "a bad level dims nothing"
        );
        let mut last = 0.0;
        for step in 0..=100u8 {
            let light = software_luminance(f32::from(step) / 100.0);
            assert!(light >= last, "monotonic at {step}");
            assert!(light >= SOFTWARE_DIM_FLOOR);
            last = light;
        }
        assert!(
            software_luminance(0.5) < 0.5,
            "half the slider looks half as bright, which is well under half the light"
        );
    }

    fn ddc_monitor(connector: &str) -> MonitorBrightness {
        MonitorBrightness {
            connector: connector.into(),
            target: BrightnessTarget::Ddc(PathBuf::from(format!("/dev/{connector}"))),
            edid: Some(format!("edid:{connector}")),
        }
    }

    type Applied = Arc<Mutex<Vec<(String, f32)>>>;

    struct Fake {
        service: BrightnessService,
        applied: Applied,
        reads: Arc<std::sync::atomic::AtomicUsize>,
    }

    fn fake_service_at(monitors: Vec<MonitorBrightness>, memory: Option<PathBuf>) -> Fake {
        let applied = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&applied);
        let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = Arc::clone(&reads);
        let service = BrightnessService::spawn(
            Arc::new(move || monitors.clone()),
            Arc::new(
                move |target: &BrightnessTarget| -> Result<Reading, String> {
                    counted.fetch_add(1, Ordering::SeqCst);
                    match target {
                        BrightnessTarget::Ddc(node) if node.ends_with("broken") => {
                            Err("no DDC/CI".into())
                        }
                        _ => Ok(Reading {
                            level: 0.5,
                            maximum: Some(100),
                        }),
                    }
                },
            ),
            Arc::new(
                move |target: &BrightnessTarget,
                      pct: f32,
                      _maximum: Option<u16>|
                      -> Result<(), String> {
                    let node = match target {
                        BrightnessTarget::Ddc(node) | BrightnessTarget::Backlight(node) => {
                            node.display().to_string()
                        }
                        BrightnessTarget::Software => "software".into(),
                    };
                    log.lock().unwrap().push((node, pct));
                    Ok(())
                },
            ),
            |_| {},
            memory,
        );
        service.rescan();
        Fake {
            service,
            applied,
            reads,
        }
    }

    fn fake_service(monitors: Vec<MonitorBrightness>) -> (BrightnessService, Applied) {
        let fake = fake_service_at(monitors, None);
        (fake.service, fake.applied)
    }

    fn idle(service: &BrightnessService) {
        let (done, told) = std::sync::mpsc::channel();
        service.when_idle(Box::new(move || {
            let _ = done.send(());
        }));
        told.recv_timeout(Duration::from_secs(30)).unwrap();
    }

    #[test]
    fn a_monitor_without_ddc_ci_is_dimmed_in_software() {
        let mut broken = ddc_monitor("DP-9");
        broken.target = BrightnessTarget::Ddc(PathBuf::from("/dev/broken"));
        let (service, applied) = fake_service(vec![ddc_monitor("DP-1"), broken]);
        let levels = service.rescan_blocking();
        assert_eq!(
            levels,
            vec![
                MonitorLevel {
                    connector: "DP-1".into(),
                    level: 0.5,
                    kind: BrightnessKind::Ddc,
                },
                MonitorLevel {
                    connector: "DP-9".into(),
                    level: 1.0,
                    kind: BrightnessKind::Software,
                },
            ]
        );
        let before = service.generation();
        assert!(service.set("DP-9", 40.0));
        assert!(service.generation() > before, "the compositor is told");
        assert_eq!(service.software_levels(), vec![("DP-9".into(), 0.4)]);
        idle(&service);
        assert!(
            applied.lock().unwrap().is_empty(),
            "nothing is written to a bus that does not answer"
        );
    }

    #[test]
    fn a_known_monitor_is_probed_once_per_appearance() {
        let fake = fake_service_at(vec![ddc_monitor("DP-1")], None);
        fake.service.rescan_blocking();
        fake.service.rescan_blocking();
        fake.service.rescan();
        idle(&fake.service);
        assert_eq!(
            fake.reads.load(Ordering::SeqCst),
            1,
            "rescans while it stays connected ask the bus nothing"
        );
    }

    #[test]
    fn capability_scale_and_level_are_remembered_by_edid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tilo/display-brightness.json");
        let mut broken = ddc_monitor("HDMI-A-1");
        broken.target = BrightnessTarget::Ddc(PathBuf::from("/dev/broken"));
        {
            let fake = fake_service_at(
                vec![ddc_monitor("DP-1"), broken.clone()],
                Some(path.clone()),
            );
            fake.service.rescan_blocking();
            assert!(fake.service.set("DP-1", 70.0));
            assert!(fake.service.set("HDMI-A-1", 30.0));
            idle(&fake.service);
        }
        let memory = Memory::load(&path);
        assert_eq!(
            memory.monitors.get("edid:DP-1"),
            Some(&Remembered {
                ddc: Some(true),
                maximum: Some(100),
                level: Some(0.7),
            })
        );
        assert_eq!(
            memory.monitors.get("edid:HDMI-A-1"),
            Some(&Remembered {
                ddc: Some(false),
                maximum: None,
                level: Some(0.3),
            })
        );
        // The software-dimmed monitor comes back on another port at its level.
        broken.connector = "DP-3".into();
        let fake = fake_service_at(vec![broken], Some(path));
        fake.service.rescan_blocking();
        assert_eq!(fake.service.software_levels(), vec![("DP-3".into(), 0.3)]);
    }

    #[test]
    fn a_level_another_process_remembered_reaches_this_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("display-brightness.json");
        let mut software = ddc_monitor("DP-4");
        software.target = BrightnessTarget::Software;
        let fake = fake_service_at(vec![software], Some(path.clone()));
        fake.service.rescan_blocking();
        let mut memory = Memory::load(&path);
        memory.monitors.entry("edid:DP-4".into()).or_default().level = Some(0.25);
        memory.save(&path);
        let before = fake.service.generation();
        assert!(fake.service.reload_memory());
        assert!(fake.service.generation() > before);
        assert_eq!(fake.service.software_levels(), vec![("DP-4".into(), 0.25)]);
        assert!(!fake.service.reload_memory(), "nothing new the second time");
    }

    /// The fake I²C bus: a monitor that takes 80 ms per write and records
    /// every frame on the wire.
    struct SlowBus {
        frames: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    impl ddc::I2cTransport for SlowBus {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
            std::thread::sleep(Duration::from_millis(80));
            self.frames.lock().unwrap().push(bytes.to_vec());
            Ok(())
        }
        fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<()> {
            Err(std::io::Error::other("write-only fake"))
        }
        fn pause(&mut self, _duration: Duration) {}
    }

    /// A held brightness key on a slow DDC/CI monitor: the cache moves at
    /// once, the bus sees a handful of writes rather than one per press,
    /// never two at a time, and ends on the last value; a second monitor's
    /// write is not held up behind the first's queue.
    #[test]
    fn ddc_writes_coalesce_to_the_newest_value_one_at_a_time_per_monitor() {
        /// The writes one monitor's bus received, in order.
        type BusWrites = Arc<Mutex<Vec<Vec<u8>>>>;
        let buses: Arc<Mutex<BTreeMap<String, BusWrites>>> = Arc::new(Mutex::new(BTreeMap::new()));
        let in_flight = Arc::new(Mutex::new(BTreeMap::<String, usize>::new()));
        let overlapped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (wire, flight, overlap) = (
            Arc::clone(&buses),
            Arc::clone(&in_flight),
            Arc::clone(&overlapped),
        );
        let service = BrightnessService::spawn(
            Arc::new(|| vec![ddc_monitor("DP-1"), ddc_monitor("DP-2")]),
            Arc::new(|_: &BrightnessTarget| -> Result<Reading, String> {
                Ok(Reading {
                    level: 0.5,
                    maximum: Some(100),
                })
            }),
            Arc::new(
                move |target: &BrightnessTarget,
                      pct: f32,
                      maximum: Option<u16>|
                      -> Result<(), String> {
                    let BrightnessTarget::Ddc(node) = target else {
                        return Ok(());
                    };
                    let name = node.display().to_string();
                    let writing = {
                        let mut flight = flight.lock().unwrap();
                        let count = flight.entry(name.clone()).or_default();
                        *count = count.saturating_add(1);
                        *count
                    };
                    if writing > 1 {
                        overlap.store(true, Ordering::SeqCst);
                    }
                    let frames = Arc::clone(wire.lock().unwrap().entry(name.clone()).or_default());
                    let mut bus = SlowBus { frames };
                    let raw = ddc_raw(pct, maximum.expect("the scale is cached"));
                    let result = ddc::set_vcp(&mut bus, ddc::VCP_BRIGHTNESS, raw)
                        .map_err(|error| error.to_string());
                    if let Some(count) = flight.lock().unwrap().get_mut(&name) {
                        *count = count.saturating_sub(1);
                    }
                    result
                },
            ),
            |_| {},
            None,
        );
        service.rescan_blocking();
        let started = Instant::now();
        for press in 1..=20u8 {
            assert!(service.set("DP-1", f32::from(press) * 5.0));
            assert_eq!(
                service.levels()[0].level,
                f32::from(press) * 0.05,
                "the cache moves with every press"
            );
        }
        assert!(
            started.elapsed() < Duration::from_millis(80),
            "setting never waits on the bus"
        );
        assert!(service.set("DP-2", 10.0));
        let second_started = Instant::now();
        let dp2 = loop {
            if let Some(frames) = buses.lock().unwrap().get("/dev/DP-2")
                && !frames.lock().unwrap().is_empty()
            {
                break second_started.elapsed();
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(
            dp2 < Duration::from_millis(400),
            "the second monitor is not queued behind the first ({dp2:?})"
        );
        idle(&service);
        let dp1 = buses.lock().unwrap().get("/dev/DP-1").cloned().unwrap();
        let frames = dp1.lock().unwrap().clone();
        assert!(
            frames.len() <= 4,
            "a burst of 20 collapses to a few writes, got {}",
            frames.len()
        );
        assert_eq!(
            frames.last().unwrap(),
            &ddc::request(&[0x03, ddc::VCP_BRIGHTNESS, 0x00, 100]),
            "the bus ends on the last value"
        );
        assert!(
            !overlapped.load(Ordering::SeqCst),
            "one write in flight per monitor"
        );
    }

    static SLEPT_MS: AtomicU64 = AtomicU64::new(0);

    fn record_sleep(duration: Duration) {
        let ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
        SLEPT_MS.fetch_add(ms, Ordering::SeqCst);
    }

    #[test]
    fn writes_to_one_ddc_monitor_are_spaced_out() {
        let applied: Applied = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&applied);
        let writer = Writer::spawn(
            "DP-1",
            Arc::new(
                move |_: &BrightnessTarget, pct: f32, _: Option<u16>| -> Result<(), String> {
                    log.lock().unwrap().push(("DP-1".into(), pct));
                    Ok(())
                },
            ),
            record_sleep,
        )
        .unwrap();
        let target = ddc_monitor("DP-1").target;
        writer.send(target.clone(), 10.0, Some(100));
        writer.wait_idle();
        writer.send(target, 20.0, Some(100));
        writer.wait_idle();
        let pcts: Vec<f32> = applied.lock().unwrap().iter().map(|(_, p)| *p).collect();
        assert_eq!(pcts, vec![10.0, 20.0]);
        assert!(
            SLEPT_MS.load(Ordering::SeqCst) >= 100,
            "the monitor gets its settle gap"
        );
    }

    #[test]
    fn a_set_is_cached_at_once_and_written_in_the_background() {
        let (service, applied) = fake_service(vec![ddc_monitor("DP-1"), ddc_monitor("DP-2")]);
        service.rescan_blocking();
        assert!(service.set("DP-2", 80.0));
        assert!(!service.set("HDMI-A-9", 80.0), "an unknown monitor");
        assert_eq!(
            service.levels()[1].level,
            0.8,
            "read back before the write lands"
        );
        idle(&service);
        assert_eq!(*applied.lock().unwrap(), vec![("/dev/DP-2".into(), 80.0)]);
    }

    #[test]
    fn a_reader_is_told_once_the_scan_asked_before_it_lands() {
        let (service, _) = fake_service(vec![ddc_monitor("DP-1")]);
        idle(&service);
        assert_eq!(
            service.levels().len(),
            1,
            "the scan asked for before landed first"
        );
    }

    #[test]
    fn stepping_together_keeps_each_monitor_at_its_share_of_the_brightest() {
        let next = step_together(&[0.8, 0.4], 0.1);
        assert!(
            (next[0] - 0.9).abs() < 1e-6,
            "the brightest moves by the step"
        );
        assert!(
            (next[1] - 0.45).abs() < 1e-6,
            "the other stays at half of it"
        );
        let down = step_together(&[0.8, 0.4], -0.4);
        assert!((down[0] - 0.4).abs() < 1e-6 && (down[1] - 0.2).abs() < 1e-6);
        assert_eq!(
            step_together(&[1.0, 0.5], 0.1),
            vec![1.0, 0.5],
            "clamped at full"
        );
        assert_eq!(
            step_together(&[0.0, 0.0], 0.1),
            vec![0.1, 0.1],
            "dark ones rise together"
        );
        assert_eq!(step_together(&[0.2, 0.1], -0.5), vec![0.0, 0.0]);
    }

    #[test]
    fn a_step_moves_the_monitor_asked_for_or_else_the_first() {
        let (service, _) = fake_service(vec![ddc_monitor("DP-1"), ddc_monitor("DP-2")]);
        service.rescan_blocking();
        let stepped = service.step(Some("DP-2"), 0.1).unwrap();
        assert_eq!(stepped.connector, "DP-2");
        assert!((stepped.level - 0.6).abs() < 1e-6);
        let fallback = service.step(Some("eDP-1"), -0.1).unwrap();
        assert_eq!(fallback.connector, "DP-1");
        assert!((fallback.level - 0.4).abs() < 1e-6);
        assert_eq!(service.step(None, 1.0).unwrap().level, 1.0, "clamped");
    }
}
