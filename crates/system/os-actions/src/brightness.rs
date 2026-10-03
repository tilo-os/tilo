//! Brightness per monitor: the laptop panel through its backlight, every
//! external monitor that speaks DDC/CI through its I²C bus.
//!
//! A monitor is known by its DRM connector (`eDP-1`, `DP-2`), the same
//! name the compositor advertises through `wl_output` and publishes to
//! Settings, so UI can put the monitor's EDID name on each slider.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::ddc;

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
}

/// One monitor whose brightness can be set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonitorBrightness {
    /// Raw DRM connector label, e.g. `DP-2`.
    pub connector: String,
    pub target: BrightnessTarget,
}

/// Every connected monitor with a brightness control, built-in panel first.
/// A monitor on a DDC bus that turns out not to implement DDC/CI is still
/// listed; its first read fails and callers leave its slider out.
pub fn monitors() -> Vec<MonitorBrightness> {
    monitors_in(Path::new(DRM), Path::new(BACKLIGHT), Path::new(DEV))
}

/// Whether a connector drives a built-in panel rather than a port.
pub fn is_builtin_connector(connector: &str) -> bool {
    let upper = connector.to_ascii_uppercase();
    upper.starts_with("EDP") || upper.starts_with("LVDS") || upper.starts_with("DSI")
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
        .filter_map(|(connector, path)| {
            let target = if is_builtin_connector(&connector) {
                // One backlight drives one panel; a second built-in
                // connector (a detachable keyboard dock's) gets none.
                BrightnessTarget::Backlight(panel_backlight.take()?)
            } else {
                BrightnessTarget::Ddc(dev.join(ddc_bus(&path)?))
            };
            Some(MonitorBrightness { connector, target })
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

impl BrightnessTarget {
    /// Current brightness as a `0.0..=1.0` fraction.
    ///
    /// # Errors
    ///
    /// Returns why the monitor could not be read.
    pub fn get(&self) -> Result<f32, String> {
        match self {
            Self::Backlight(device) => crate::display::get_backlight(device),
            Self::Ddc(node) => {
                let value = read_ddc(node)?;
                value
                    .fraction()
                    .ok_or_else(|| "monitor reports no brightness range".to_owned())
            }
        }
    }

    /// Set brightness to `pct` percent, clamped to `[0, 100]`.
    ///
    /// # Errors
    ///
    /// Returns why the monitor could not be written.
    pub fn set(&self, pct: f32) -> Result<(), String> {
        let pct = pct.clamp(0.0, 100.0);
        match self {
            Self::Backlight(device) => crate::display::set_backlight(device, pct),
            Self::Ddc(node) => {
                let maximum = read_ddc(node)?.maximum;
                write_ddc(node, ddc_raw(pct, maximum))
            }
        }
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

/// One monitor's brightness as last read or set.
#[derive(Clone, Debug, PartialEq)]
pub struct MonitorLevel {
    pub connector: String,
    /// `0.0..=1.0`.
    pub level: f32,
}

enum Request {
    Rescan,
    Set(MonitorBrightness, f32),
}

#[derive(Default)]
struct Shared {
    /// Every controllable monitor with its last known level; a monitor
    /// whose first read failed (no DDC/CI) is left out.
    monitors: Vec<(MonitorBrightness, f32)>,
}

type Discover = dyn Fn() -> Vec<MonitorBrightness> + Send + Sync;
type ReadLevel = dyn Fn(&BrightnessTarget) -> Result<f32, String> + Send + Sync;
type Apply = dyn Fn(&BrightnessTarget, f32) -> Result<(), String> + Send;

/// Brightness for every monitor, applied on a worker thread so a slider
/// drag or a held brightness key never waits on a monitor. Levels are
/// cached: reads are instant and reflect the last value set, and requests
/// for one monitor that arrive while it is busy collapse into the latest.
pub struct BrightnessService {
    shared: Arc<Mutex<Shared>>,
    tx: Option<Sender<Request>>,
    discover: Arc<Discover>,
    read: Arc<ReadLevel>,
}

impl BrightnessService {
    /// The process-wide service, started (and its monitors scanned) on
    /// first use.
    pub fn global() -> &'static Self {
        static SERVICE: OnceLock<BrightnessService> = OnceLock::new();
        SERVICE.get_or_init(|| {
            let service = Self::spawn(
                Arc::new(monitors),
                Arc::new(BrightnessTarget::get),
                Box::new(BrightnessTarget::set),
                std::thread::sleep,
            );
            service.rescan();
            service
        })
    }

    fn spawn(
        discover: Arc<Discover>,
        read: Arc<ReadLevel>,
        apply: Box<Apply>,
        sleep: fn(Duration),
    ) -> Self {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (tx, rx) = channel();
        let worker_shared = Arc::clone(&shared);
        let (worker_discover, worker_read) = (Arc::clone(&discover), Arc::clone(&read));
        let spawned = std::thread::Builder::new()
            .name("tilo-brightness".into())
            .spawn(move || {
                run_worker(
                    &rx,
                    &worker_shared,
                    &*worker_discover,
                    &*worker_read,
                    &*apply,
                    sleep,
                );
            });
        if let Err(error) = &spawned {
            tracing::warn!(target: "tilo::display", %error, "brightness worker did not start");
        }
        Self {
            shared,
            tx: spawned.ok().map(|_| tx),
            discover,
            read,
        }
    }

    /// [`Self::rescan`] on the calling thread, returning once the levels
    /// are fresh. Reading a DDC/CI monitor takes tens of milliseconds, so
    /// only a worker thread may call this.
    pub fn rescan_blocking(&self) -> Vec<MonitorLevel> {
        let monitors = probe(&*self.discover, &*self.read);
        self.lock().monitors = monitors;
        self.levels()
    }

    /// Re-discover monitors and re-read their levels, e.g. after a hotplug
    /// or when a panel that shows them opens.
    pub fn rescan(&self) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Request::Rescan);
        }
    }

    /// The controllable monitors, built-in panel first.
    pub fn levels(&self) -> Vec<MonitorLevel> {
        self.lock()
            .monitors
            .iter()
            .map(|(monitor, level)| MonitorLevel {
                connector: monitor.connector.clone(),
                level: *level,
            })
            .collect()
    }

    /// Set `connector` to `pct` percent. Never blocks; `false` when no such
    /// monitor is controllable.
    pub fn set(&self, connector: &str, pct: f32) -> bool {
        let pct = pct.clamp(0.0, 100.0);
        let Some(monitor) = self
            .lock()
            .monitors
            .iter_mut()
            .find(|(monitor, _)| monitor.connector == connector)
            .map(|(monitor, level)| {
                *level = pct / 100.0;
                monitor.clone()
            })
        else {
            return false;
        };
        self.tx
            .as_ref()
            .is_some_and(|tx| tx.send(Request::Set(monitor, pct)).is_ok())
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
                connector: current.connector.clone(),
                level: next,
            })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn run_worker(
    rx: &Receiver<Request>,
    shared: &Mutex<Shared>,
    discover: &Discover,
    read: &ReadLevel,
    apply: &Apply,
    sleep: fn(Duration),
) {
    let mut last_write: BTreeMap<String, Instant> = BTreeMap::new();
    while let Ok(first) = rx.recv() {
        let mut rescan = false;
        let mut writes = Vec::new();
        for request in std::iter::once(first).chain(rx.try_iter()) {
            match request {
                Request::Rescan => rescan = true,
                Request::Set(monitor, pct) => writes.push((monitor, pct)),
            }
        }
        for (monitor, pct) in coalesce(writes) {
            if matches!(monitor.target, BrightnessTarget::Ddc(_))
                && let Some(wait) = last_write
                    .get(&monitor.connector)
                    .and_then(|at| DDC_MIN_INTERVAL.checked_sub(at.elapsed()))
            {
                sleep(wait);
            }
            if let Err(error) = apply(&monitor.target, pct) {
                tracing::warn!(
                    target: "tilo::display",
                    connector = %monitor.connector,
                    %error,
                    "brightness write failed"
                );
            }
            last_write.insert(monitor.connector.clone(), Instant::now());
        }
        if rescan {
            let monitors = probe(discover, read);
            shared
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .monitors = monitors;
        }
    }
}

/// Every discovered monitor whose level can be read, with that level.
fn probe(discover: &Discover, read: &ReadLevel) -> Vec<(MonitorBrightness, f32)> {
    discover()
        .into_iter()
        .filter_map(|monitor| match read(&monitor.target) {
            Ok(level) => Some((monitor, level)),
            Err(error) => {
                tracing::debug!(
                    target: "tilo::display",
                    connector = %monitor.connector,
                    %error,
                    "monitor brightness is not controllable"
                );
                None
            }
        })
        .collect()
}

/// Keep only the newest request per monitor, in first-seen order.
fn coalesce(
    requests: impl IntoIterator<Item = (MonitorBrightness, f32)>,
) -> Vec<(MonitorBrightness, f32)> {
    let mut out: Vec<(MonitorBrightness, f32)> = Vec::new();
    for (monitor, pct) in requests {
        if let Some(slot) = out
            .iter_mut()
            .find(|(m, _)| m.connector == monitor.connector)
        {
            *slot = (monitor, pct);
        } else {
            out.push((monitor, pct));
        }
    }
    out
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

    #[test]
    fn each_connected_monitor_gets_its_own_control() {
        let tree = Tree::new();
        tree.backlight("intel_backlight");
        tree.connector("card0-eDP-1", "connected");
        let hdmi = tree.connector("card0-HDMI-A-1", "connected");
        let bus = tree.path("i2c/i2c-5");
        fs::create_dir_all(&bus).unwrap();
        std::os::unix::fs::symlink(&bus, hdmi.join("ddc")).unwrap();
        let dp = tree.connector("card0-DP-2", "connected");
        fs::create_dir_all(dp.join("i2c-9")).unwrap();
        fs::create_dir_all(dp.join("power")).unwrap();
        tree.connector("card0-DP-3", "disconnected");
        // A connected port without any I²C adapter cannot be driven.
        tree.connector("card0-Virtual-1", "connected");
        fs::create_dir_all(tree.path("drm/card0")).unwrap();

        assert_eq!(
            tree.monitors(),
            vec![
                MonitorBrightness {
                    connector: "eDP-1".into(),
                    target: BrightnessTarget::Backlight(tree.path("backlight/intel_backlight")),
                },
                MonitorBrightness {
                    connector: "DP-2".into(),
                    target: BrightnessTarget::Ddc(tree.path("dev/i2c-9")),
                },
                MonitorBrightness {
                    connector: "HDMI-A-1".into(),
                    target: BrightnessTarget::Ddc(tree.path("dev/i2c-5")),
                },
            ]
        );
    }

    #[test]
    fn a_panel_without_a_backlight_has_no_control() {
        let tree = Tree::new();
        tree.connector("card0-eDP-1", "connected");
        assert!(tree.monitors().is_empty());
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

    fn ddc_monitor(connector: &str) -> MonitorBrightness {
        MonitorBrightness {
            connector: connector.into(),
            target: BrightnessTarget::Ddc(PathBuf::from(format!("/dev/{connector}"))),
        }
    }

    #[test]
    fn a_burst_collapses_to_the_newest_value_per_monitor() {
        let burst = vec![
            (ddc_monitor("DP-1"), 10.0),
            (ddc_monitor("DP-2"), 20.0),
            (ddc_monitor("DP-1"), 30.0),
            (ddc_monitor("DP-1"), 40.0),
        ];
        let applied: Vec<(String, f32)> = coalesce(burst)
            .into_iter()
            .map(|(m, pct)| (m.connector, pct))
            .collect();
        assert_eq!(applied, vec![("DP-1".into(), 40.0), ("DP-2".into(), 20.0)]);
    }

    static SLEPT_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn record_sleep(duration: Duration) {
        let ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
        SLEPT_MS.fetch_add(ms, std::sync::atomic::Ordering::SeqCst);
    }

    #[test]
    fn writes_to_one_ddc_monitor_are_spaced_out() {
        let (tx, rx) = channel();
        tx.send(Request::Set(ddc_monitor("DP-1"), 10.0)).unwrap();
        let applied = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&applied);
        // The second request arrives while the first is being written; its
        // sender then goes away so the worker ends once it is drained.
        let late = Mutex::new(Some(tx));
        let apply = move |_: &BrightnessTarget, pct: f32| {
            log.lock().unwrap().push(pct);
            let late = late.lock().unwrap().take();
            if let Some(late) = late {
                late.send(Request::Set(ddc_monitor("DP-1"), 20.0)).unwrap();
            }
            Ok(())
        };
        let shared = Mutex::new(Shared::default());
        run_worker(
            &rx,
            &shared,
            &Vec::new,
            &|_: &BrightnessTarget| Ok(0.0),
            &apply,
            record_sleep,
        );
        assert_eq!(*applied.lock().unwrap(), vec![10.0, 20.0]);
        assert!(
            SLEPT_MS.load(std::sync::atomic::Ordering::SeqCst) >= 100,
            "the monitor gets its settle gap"
        );
    }

    type Applied = Arc<Mutex<Vec<(String, f32)>>>;

    fn fake_service(monitors: Vec<MonitorBrightness>) -> (BrightnessService, Applied) {
        let applied = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&applied);
        let service = BrightnessService::spawn(
            Arc::new(move || monitors.clone()),
            Arc::new(|target: &BrightnessTarget| match target {
                BrightnessTarget::Ddc(node) if node.ends_with("broken") => Err("no DDC/CI".into()),
                _ => Ok(0.5),
            }),
            Box::new(move |target: &BrightnessTarget, pct| {
                let (BrightnessTarget::Ddc(node) | BrightnessTarget::Backlight(node)) = target;
                log.lock().unwrap().push((node.display().to_string(), pct));
                Ok(())
            }),
            |_| {},
        );
        service.rescan();
        (service, applied)
    }

    fn eventually(mut done: impl FnMut() -> bool) {
        for _ in 0..1000 {
            if done() {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn monitors_without_ddc_ci_are_left_out() {
        let mut broken = ddc_monitor("DP-9");
        broken.target = BrightnessTarget::Ddc(PathBuf::from("/dev/broken"));
        let (service, _) = fake_service(vec![ddc_monitor("DP-1"), broken]);
        assert_eq!(
            service.rescan_blocking(),
            vec![MonitorLevel {
                connector: "DP-1".into(),
                level: 0.5
            }]
        );
    }

    #[test]
    fn a_set_is_cached_at_once_and_written_in_the_background() {
        let (service, applied) = fake_service(vec![ddc_monitor("DP-1"), ddc_monitor("DP-2")]);
        eventually(|| service.levels().len() == 2);
        assert!(service.set("DP-2", 80.0));
        assert!(!service.set("HDMI-A-9", 80.0), "an unknown monitor");
        assert_eq!(
            service.levels()[1].level,
            0.8,
            "read back before the write lands"
        );
        eventually(|| !applied.lock().unwrap().is_empty());
        assert_eq!(*applied.lock().unwrap(), vec![("/dev/DP-2".into(), 80.0)]);
    }

    #[test]
    fn a_step_moves_the_monitor_under_the_pointer_or_else_the_first() {
        let (service, _) = fake_service(vec![ddc_monitor("DP-1"), ddc_monitor("DP-2")]);
        eventually(|| service.levels().len() == 2);
        let stepped = service.step(Some("DP-2"), 0.1).unwrap();
        assert_eq!(stepped.connector, "DP-2");
        assert!((stepped.level - 0.6).abs() < 1e-6);
        let fallback = service.step(Some("eDP-1"), -0.1).unwrap();
        assert_eq!(fallback.connector, "DP-1");
        assert!((fallback.level - 0.4).abs() < 1e-6);
        assert_eq!(service.step(None, 1.0).unwrap().level, 1.0, "clamped");
    }
}
