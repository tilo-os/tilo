//! The session's system service.
//!
//! It publishes four resources, each a reading of the machine and the
//! actions that change it:
//!
//! * `power`: the power mode, and the session's power actions (lock,
//!   switch user, sleep, restart, power off, log out; the last three ask for
//!   confirmation);
//! * `system-update`: what is booted, what the update service staged and
//!   what finishing it needs, with updating now and restarting to finish it.
//!   The live update's state is followed as it is written
//!   ([`SystemService::follow_update`]), so the reading and the actions it
//!   offers change the moment an update applies, finishes or fails, whoever
//!   started it;
//! * `time-zone`: the zone, whether it follows where the device is, and
//!   network time. While it follows the device, the service looks where the
//!   device is when that is turned on and every [`LOOK_AGAIN`] after, and
//!   sets the zone of the nearest city; nothing is guessed without a
//!   location;
//! * `device:keyboard-backlight`: the keyboard light's level, with the
//!   steps its keys take.
//!
//! The machine's calls run on the service's worker; an action is pending
//! in its reading until it lands, and why a change did not happen is the
//! reading's problem until the next.

pub mod codec_pack;
pub mod containers;
pub mod device;
pub mod disk_unlock;
pub mod disks;
pub mod drives;
pub mod firewall;
pub mod firmware;
pub mod host_info;
pub mod journal;
pub mod storage;
pub mod windows_apps;
pub mod windows_compat;
pub use tilo_system_resources::effects;
tilo_resource::declared_names!();

pub mod model;
pub mod printers;
pub mod processes;
mod rows;
pub mod scanners;
pub mod update;
pub mod users;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Map, Value};
use tilo_actions::{ApiError, ResourceSpec};
use tilo_resource::Effect as _;
use tilo_service::{Jobs, Service, Started, Wake, Worker, decode, offer, reading, unavailable};

use device::{
    Change, CodecChange, ContainerChange, DiskChange, DiskUnlockChange, DriveChange,
    FirmwareChange, PowerProfiles, PrintChange, ProcessChange, StorageChange, System, UserChange,
    WindowsChange, follow_once,
};
use effects::{KeyboardLightAction, PowerAction, TimeAction, UpdateAction};
use model::{KeyboardLight, Power, PowerMode, Time};
use update::Update;

/// The resources' ids.
pub use tilo_system_resources::{
    CODEC_PACK, CONTAINERS, DISK_UNLOCK, DISKS, DRIVES, FIREWALL, FIRMWARE, HOST_INFO, JOURNAL,
    KEYBOARD_BACKLIGHT, POWER, PRINTERS, PROCESSES, SCANNERS, STORAGE, SYSTEM_UPDATE, TIME_ZONE,
    USERS, WINDOWS_APPS, WINDOWS_COMPAT, WINDOWS_PROGRAM,
};

/// How often a device whose zone follows it looks where it is again: often
/// enough that a flight's arrival shows the local time within the hour.
/// The most journal records the reading keeps: older ones leave as new ones arrive.
const MAX_JOURNAL_ENTRIES: usize = 5000;

pub const LOOK_AGAIN: Duration = Duration::from_secs(30 * 60);

/// Which resource a change belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Power,
    Update,
    Time,
    KeyboardLight,
    Printers,
    Users,
    Drives,
    Storage,
    DiskUnlock,
    HostInfo,
    Scanners,
    CodecPack,
    WindowsCompat,
    Firmware,
    Containers,
    Processes,
    Disks,
    Firewall,
    Journal,
}

/// A change to one of the resources a privileged helper answers for an extension.
enum ExtensionChange {
    Codec(CodecChange),
    Windows(WindowsChange),
    Firmware(FirmwareChange),
}

/// What the machine reads now.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Readings {
    pub power: Power,
    pub update: Update,
    pub time: Time,
    pub keyboard_light: KeyboardLight,
    pub printers: tilo_system_resources::printers::Printers,
    pub users: tilo_system_resources::users::Users,
    pub drives: tilo_system_resources::drives::Drives,
    pub storage: tilo_system_resources::storage::Storage,
    pub disk_unlock: tilo_system_resources::disk_unlock::DiskUnlock,
    pub host_info: tilo_system_resources::host_info::HostInfo,
    pub scanners: tilo_system_resources::scanners::Scanners,
    pub codec_pack: tilo_system_resources::codec_pack::CodecPack,
    pub windows_compat: tilo_system_resources::windows_compat::WindowsCompat,
    pub firmware: tilo_system_resources::firmware::Firmware,
    pub containers: tilo_system_resources::containers::Containers,
    pub processes: tilo_system_resources::processes::Processes,
    pub disks: tilo_system_resources::disks::Disks,
    pub firewall: tilo_system_resources::firewall::Firewall,
    pub journal: tilo_system_resources::journal::Journal,
    pub windows_apps: tilo_system_resources::windows_apps::WindowsApps,
}

fn read(system: &dyn System, live_profiles: bool) -> Readings {
    let power = if live_profiles {
        Power {
            user_switching: system.user_switching(),
            ..Power::default()
        }
    } else {
        system.power()
    };
    Readings {
        power,
        update: system.update(),
        time: system.time(),
        keyboard_light: system.keyboard_light(),
        printers: system.printers(),
        users: system.users(),
        drives: system.drives(),
        storage: system.storage(),
        disk_unlock: system.disk_unlock(),
        host_info: system.host_info(),
        scanners: system.scanners(),
        codec_pack: system.codec_pack(),
        windows_compat: system.windows_compat(),
        firmware: system.firmware(),
        containers: system.containers(),
        processes: system.processes(),
        disks: system.disks(),
        firewall: system.firewall(),
        journal: tilo_system_resources::journal::Journal::default(),
        windows_apps: tilo_system_resources::windows_apps::WindowsApps::default(),
    }
}

/// What lands from the worker.
enum Landed {
    Read(Box<Readings>),
    /// The update read again, alone: its state changed.
    Update(Box<Update>),
    Changed {
        part: Part,
        action: String,
        result: Result<(), String>,
        readings: Box<Readings>,
        /// On the live path, the active power mode read once directly from the
        /// daemon right after `Change::PowerMode` landed, paired with the live
        /// source's revision before that read so a newer daemon signal still wins.
        live_power_mode: Option<(u64, PowerMode)>,
    },
    /// The kernel's drops read from this device's journal, with the window and the
    /// moment it was read: a report of its own, not a fresh reading of the firewall.
    Blocked {
        action: String,
        window_minutes: u32,
        read_at_ms: u64,
        result: Result<BlockedReport, String>,
    },
    /// The journal read, or the records written since a cursor.
    Journal {
        action: String,
        result: Result<(Vec<tilo_system_resources::journal::JournalEntry>, String), String>,
        /// The read replaced the entries, rather than continuing them.
        replace: bool,
        read_at_ms: u64,
    },
    /// An action on the Windows programs landed: what it came to, and the reading to forget.
    Windows {
        action: String,
        result: Result<(), String>,
    },
}

/// One window's live follow of the journal, and the cursor its next read starts after.
struct JournalFollow {
    following: Arc<AtomicBool>,
    cursor: Arc<Mutex<String>>,
    thread: std::thread::Thread,
}

/// What the kernel dropped in a window: its groups, the packets together, and what the report
/// leaves out.
type BlockedReport = (
    Vec<tilo_system_resources::firewall::BlockedGroup>,
    u64,
    Option<String>,
);

/// Live power-profile readings followed beside the service's worker.
struct LiveProfiles {
    source: Arc<dyn PowerProfiles>,
    /// Local mirror of `active_shared` so the live poll path needs only one
    /// atomic load (`source.revision()`).
    active: bool,
    /// Monotonic (`false -> true`) flag shared with worker closures so
    /// `refresh`, `follow_location` and `start` stop calling `System::power`
    /// once the live source takes over.
    active_shared: Arc<AtomicBool>,
    seen: u64,
}

fn is_live(flag: Option<&Arc<AtomicBool>>) -> bool {
    flag.is_some_and(|active| active.load(Ordering::Acquire))
}

/// The system service.
pub struct SystemService {
    readings: Readings,
    worker: Worker<dyn System, Landed>,
    /// Stops the zone follower when the service goes.
    stopped: Arc<AtomicBool>,
    follower: Option<std::thread::Thread>,
    /// Follows the live update's state, once [`Self::follow_update`] started it.
    update_watch: Option<StateWatch>,
    /// Live power profiles from the session's power service when one runs.
    live_profiles: Option<LiveProfiles>,
    /// Keeps the printer bus subscription alive while the service runs.
    print_watch: Option<Box<dyn Send>>,
    /// Keeps the engine's event subscription alive while the service runs.
    container_watch: Option<Box<dyn Send>>,
    /// Keeps the process reading's subscription alive while the service runs.
    process_watch: Option<Box<dyn Send>>,
    /// Follows the journal while a window asks for it.
    journal_follow: Option<JournalFollow>,
    /// The Windows programs and appliance, when this session runs their service.
    windows: Option<Arc<dyn windows_apps::WindowsManager>>,
    /// What the Windows reading last answered for each workspace, and the actions still running.
    windows_cache: std::sync::Mutex<WindowsCache>,
}

/// The Windows programs as the service last read them, by the workspace the reader is in, and
/// the actions taken and not landed yet.
#[derive(Default)]
struct WindowsCache {
    by_workspace: std::collections::HashMap<
        String,
        (
            std::time::Instant,
            tilo_system_resources::windows_apps::WindowsApps,
        ),
    >,
    pending: Vec<String>,
    problem: Option<String>,
}

/// How long a workspace's Windows reading is served before the service asks again.
const WINDOWS_FRESH: std::time::Duration = std::time::Duration::from_secs(2);

impl WindowsCache {
    fn fresh(&self, workspace: &str) -> Option<tilo_system_resources::windows_apps::WindowsApps> {
        self.by_workspace
            .get(workspace)
            .filter(|(at, _)| at.elapsed() < WINDOWS_FRESH)
            .map(|(_, reading)| reading.clone())
    }
}

impl SystemService {
    /// The service over `system`, reading it once at the start.
    ///
    /// # Errors
    /// The worker's thread did not start.
    pub fn new(system: Arc<dyn System>, wake: Option<Wake>) -> std::io::Result<Self> {
        Self::build(system, wake, None, None)
    }

    /// The service over `system`, taking the machine's power modes and active
    /// mode live from `profiles` instead of polling `system.power()`.
    ///
    /// # Errors
    /// The worker's thread did not start.
    pub fn with_power_profiles(
        system: Arc<dyn System>,
        wake: Option<Wake>,
        profiles: Arc<dyn PowerProfiles>,
    ) -> std::io::Result<Self> {
        Self::build(system, wake, Some(profiles), None)
    }

    /// The service over `system`, publishing this computer's Windows programs as the device's
    /// `windows-apps` resource through `windows`.
    ///
    /// # Errors
    /// The worker's thread did not start.
    pub fn with_windows_manager(
        system: Arc<dyn System>,
        wake: Option<Wake>,
        windows: Arc<dyn windows_apps::WindowsManager>,
    ) -> std::io::Result<Self> {
        Self::build(system, wake, None, Some(windows))
    }

    /// The service over `system`, with both the live power modes and the Windows programs.
    ///
    /// # Errors
    /// The worker's thread did not start.
    pub fn with_power_and_windows(
        system: Arc<dyn System>,
        wake: Option<Wake>,
        profiles: Arc<dyn PowerProfiles>,
        windows: Arc<dyn windows_apps::WindowsManager>,
    ) -> std::io::Result<Self> {
        Self::build(system, wake, Some(profiles), Some(windows))
    }

    fn build(
        system: Arc<dyn System>,
        wake: Option<Wake>,
        profiles: Option<Arc<dyn PowerProfiles>>,
        windows: Option<Arc<dyn windows_apps::WindowsManager>>,
    ) -> std::io::Result<Self> {
        let mut readings = Readings::default();
        let live_profiles = profiles.map(|source| {
            if let Some(wake) = &wake {
                source.on_change(Arc::clone(wake));
            }
            let active = source.is_ready();
            let seen = if active {
                // Capture the revision before reading the snapshot so a concurrent
                // update between the two is still picked up on the next poll.
                let seen = source.revision();
                let (modes, mode) = source.snapshot();
                readings.power.modes = modes;
                readings.power.mode = mode;
                seen
            } else {
                0
            };
            LiveProfiles {
                source,
                active,
                active_shared: Arc::new(AtomicBool::new(active)),
                seen,
            }
        });
        let live_active = live_profiles
            .as_ref()
            .map(|live| Arc::clone(&live.active_shared));
        // What the followers below watch; the worker keeps the system itself.
        let watched = Arc::clone(&system);
        let mut service = Self {
            readings,
            worker: Worker::spawn("tilo-system", system, wake.clone())?,
            stopped: Arc::new(AtomicBool::new(false)),
            follower: None,
            update_watch: None,
            live_profiles,
            print_watch: None,
            container_watch: None,
            process_watch: None,
            journal_follow: None,
            windows,
            windows_cache: std::sync::Mutex::default(),
        };
        refresh(&service.worker.jobs(), live_active.clone());
        service.follow_printers(watched.as_ref(), wake.clone(), live_active.clone());
        service.follow_containers(watched.as_ref(), wake.clone(), live_active.clone());
        service.follow_processes(watched.as_ref(), wake, live_active);
        // The first scan runs where the worker does, so the machine's start waits for no disk
        // scan; the reading arrives when it answers.
        service.start_storage("scan", None);
        Ok(service)
    }

    /// Read the queues and jobs again whenever the printer bus says they moved: a job
    /// started, a queue paused, a supply ran low. Nothing polls.
    fn follow_printers(
        &mut self,
        system: &dyn System,
        wake: Option<Wake>,
        live_active: Option<Arc<AtomicBool>>,
    ) {
        let Some(wake) = wake else {
            return;
        };
        let jobs = self.worker.jobs();
        let reread = move || {
            let live_active = live_active.clone();
            jobs.clone().run(move |system| {
                Landed::Read(Box::new(read(system, is_live(live_active.as_ref()))))
            });
            wake();
        };
        self.print_watch = system.follow_printers(Box::new(reread));
    }

    /// Read the engine again whenever podman reports an event: a container started,
    /// stopped, removed, an image pulled. Nothing polls.
    fn follow_containers(
        &mut self,
        system: &dyn System,
        wake: Option<Wake>,
        live_active: Option<Arc<AtomicBool>>,
    ) {
        let Some(wake) = wake else {
            return;
        };
        let jobs = self.worker.jobs();
        let reread = move || {
            let live_active = live_active.clone();
            jobs.clone().run(move |system| {
                Landed::Read(Box::new(read(system, is_live(live_active.as_ref()))))
            });
            wake();
        };
        self.container_watch = system.follow_containers(Box::new(reread));
    }

    /// Read the machine again whenever the system information service sends a snapshot.
    fn follow_processes(
        &mut self,
        system: &dyn System,
        wake: Option<Wake>,
        live_active: Option<Arc<AtomicBool>>,
    ) {
        let Some(wake) = wake else {
            return;
        };
        let jobs = self.worker.jobs();
        let reread = move || {
            let live_active = live_active.clone();
            jobs.clone().run(move |system| {
                Landed::Read(Box::new(read(system, is_live(live_active.as_ref()))))
            });
            wake();
        };
        self.process_watch = system.follow_processes(Box::new(reread));
    }

    fn live_active_flag(&self) -> Option<Arc<AtomicBool>> {
        self.live_profiles
            .as_ref()
            .map(|live| Arc::clone(&live.active_shared))
    }

    /// Let the time zone follow the device while the person chose so:
    /// look now, and every [`LOOK_AGAIN`] after.
    ///
    /// # Errors
    /// The follower's thread did not start.
    pub fn follow_location(&mut self) -> std::io::Result<()> {
        let jobs = self.worker.jobs();
        let stopped = Arc::clone(&self.stopped);
        let live_profiles = self.live_active_flag();
        let follower = std::thread::Builder::new()
            .name("tilo-time-zone".into())
            .spawn(move || {
                while !stopped.load(Ordering::Acquire) {
                    look(&jobs, live_profiles.clone());
                    std::thread::park_timeout(LOOK_AGAIN);
                }
            })?;
        self.follower = Some(follower.thread().clone());
        Ok(())
    }

    /// Read the update again whenever the live update writes its state: the system's own timer
    /// staging an update, an admin applying one, the desktop half finishing or failing. Nothing
    /// polls; the reading follows the state's directory.
    pub fn follow_update(&mut self) {
        self.follow_update_in(Path::new(crate::update::LIVE_STATE).parent().map_or_else(
            || PathBuf::from(crate::update::LIVE_STATE),
            Path::to_path_buf,
        ));
    }

    /// [`Self::follow_update`] with the state in `dir`.
    pub fn follow_update_in(&mut self, dir: PathBuf) {
        let mut watch = StateWatch::new(dir, self.worker.jobs());
        watch.arm();
        self.update_watch = Some(watch);
    }

    /// Read the machine again.
    pub fn refresh(&self) {
        refresh(&self.worker.jobs(), self.live_active_flag());
    }

    /// What the machine reads now.
    #[must_use]
    pub const fn readings(&self) -> &Readings {
        &self.readings
    }

    /// This computer's Windows programs as an app in `workspace` reads them now, with the
    /// actions still running and why the last one did not.
    fn windows_reading(&self, workspace: &str) -> tilo_system_resources::windows_apps::WindowsApps {
        let Some(manager) = &self.windows else {
            return tilo_system_resources::windows_apps::WindowsApps {
                summary: "This computer's Windows compatibility service is not running.".to_owned(),
                ..tilo_system_resources::windows_apps::WindowsApps::default()
            };
        };
        let fresh = {
            let cache = self
                .windows_cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            cache.fresh(workspace)
        };
        let mut reading = if let Some(reading) = fresh {
            reading
        } else {
            let reading = manager.reading(workspace);
            self.windows_cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .by_workspace
                .insert(workspace.to_owned(), (std::time::Instant::now(), reading.clone()));
            reading
        };
        {
            let cache = self
                .windows_cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            reading.pending.clone_from(&cache.pending);
            reading.problem.clone_from(&cache.problem);
        }
        reading
    }

    /// Carry out a Windows action for an app in `workspace`, landing what it came to.
    fn start_windows(
        &self,
        workspace: &str,
        action: tilo_system_resources::windows_apps::WindowsAppsAction,
    ) -> Started {
        let id = action.id().to_owned();
        {
            let mut cache = self
                .windows_cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            cache.pending.push(id.clone());
            cache.problem = None;
            cache.by_workspace.clear();
        }
        let manager = self.windows.clone();
        let workspace = workspace.to_owned();
        self.worker.run(move |_system| {
            let result = match &manager {
                Some(manager) => manager.change(&workspace, &action),
                None => {
                    Err("This computer's Windows compatibility service is not running.".to_owned())
                }
            };
            Landed::Windows { action: id, result }
        });
        Started::Accepted
    }

    /// Take in the power modes from the live power source when its revision moved.
    fn sync_power_profiles(&mut self) -> bool {
        let Some(live) = &mut self.live_profiles else {
            return false;
        };
        let just_activated = if live.active {
            false
        } else if live.source.is_ready() {
            live.active = true;
            live.active_shared.store(true, Ordering::Release);
            true
        } else {
            return false;
        };
        let revision = live.source.revision();
        if !just_activated && revision == live.seen {
            return false;
        }
        live.seen = revision;
        let (modes, mode) = live.source.snapshot();
        let changed = self.readings.power.modes != modes || self.readings.power.mode != mode;
        self.readings.power.modes = modes;
        self.readings.power.mode = mode;
        changed
    }

    fn status(&mut self, part: Part) -> (&mut Vec<String>, &mut Option<String>) {
        let readings = &mut self.readings;
        match part {
            Part::Power => (&mut readings.power.pending, &mut readings.power.problem),
            Part::Update => (&mut readings.update.pending, &mut readings.update.problem),
            Part::Time => (&mut readings.time.pending, &mut readings.time.problem),
            Part::KeyboardLight => (
                &mut readings.keyboard_light.pending,
                &mut readings.keyboard_light.problem,
            ),
            Part::Printers => (
                &mut readings.printers.pending,
                &mut readings.printers.problem,
            ),
            Part::Users => (&mut readings.users.pending, &mut readings.users.problem),
            Part::Drives => (&mut readings.drives.pending, &mut readings.drives.problem),
            Part::Storage => (&mut readings.storage.pending, &mut readings.storage.problem),
            Part::DiskUnlock => (
                &mut readings.disk_unlock.pending,
                &mut readings.disk_unlock.problem,
            ),
            Part::HostInfo => (
                &mut readings.host_info.pending,
                &mut readings.host_info.problem,
            ),
            Part::Scanners => (
                &mut readings.scanners.pending,
                &mut readings.scanners.problem,
            ),
            Part::CodecPack => (
                &mut readings.codec_pack.pending,
                &mut readings.codec_pack.problem,
            ),
            Part::WindowsCompat => (
                &mut readings.windows_compat.pending,
                &mut readings.windows_compat.problem,
            ),
            Part::Firmware => (
                &mut readings.firmware.pending,
                &mut readings.firmware.problem,
            ),
            Part::Containers => (
                &mut readings.containers.pending,
                &mut readings.containers.problem,
            ),
            Part::Processes => (
                &mut readings.processes.pending,
                &mut readings.processes.problem,
            ),
            Part::Disks => (&mut readings.disks.pending, &mut readings.disks.problem),
            Part::Firewall => (
                &mut readings.firewall.pending,
                &mut readings.firewall.problem,
            ),
            Part::Journal => (&mut readings.journal.pending, &mut readings.journal.problem),
        }
    }

    fn take(&mut self, mut readings: Readings) {
        let old = &mut self.readings;
        if self.live_profiles.as_ref().is_some_and(|live| live.active) {
            readings.power.modes = std::mem::take(&mut old.power.modes);
            readings.power.mode = old.power.mode.take();
        }
        readings.power.pending = std::mem::take(&mut old.power.pending);
        readings.power.problem = old.power.problem.take();
        readings.update.pending = std::mem::take(&mut old.update.pending);
        readings.update.problem = old.update.problem.take();
        readings.time.pending = std::mem::take(&mut old.time.pending);
        readings.time.problem = old.time.problem.take();
        readings.keyboard_light.pending = std::mem::take(&mut old.keyboard_light.pending);
        readings.keyboard_light.problem = old.keyboard_light.problem.take();
        readings.printers.pending = std::mem::take(&mut old.printers.pending);
        readings.printers.problem = old.printers.problem.take();
        readings.users.pending = std::mem::take(&mut old.users.pending);
        readings.users.problem = old.users.problem.take();
        readings.drives.pending = std::mem::take(&mut old.drives.pending);
        readings.drives.problem = old.drives.problem.take();
        // A scan's reading replaces the last one; any other read keeps it, and lands only the
        // action's own state.
        readings.storage.pending = std::mem::take(&mut old.storage.pending);
        readings.storage.problem = old.storage.problem.take();
        if !readings.storage.scanned {
            readings.storage.disk = old.storage.disk.take();
            readings.storage.categories = std::mem::take(&mut old.storage.categories);
            readings.storage.largest = std::mem::take(&mut old.storage.largest);
            readings.storage.cache_bytes = old.storage.cache_bytes;
            readings.storage.read_error = old.storage.read_error.take();
            readings.storage.scanned = old.storage.scanned;
        }
        readings.host_info.pending = std::mem::take(&mut old.host_info.pending);
        readings.host_info.problem = old.host_info.problem.take();
        readings.codec_pack.pending = std::mem::take(&mut old.codec_pack.pending);
        readings.codec_pack.problem = old.codec_pack.problem.take();
        readings.windows_compat.pending = std::mem::take(&mut old.windows_compat.pending);
        readings.windows_compat.problem = old.windows_compat.problem.take();
        readings.firmware.pending = std::mem::take(&mut old.firmware.pending);
        readings.firmware.problem = old.firmware.problem.take();
        readings.containers.pending = std::mem::take(&mut old.containers.pending);
        readings.containers.problem = old.containers.problem.take();
        readings.processes.pending = std::mem::take(&mut old.processes.pending);
        readings.processes.problem = old.processes.problem.take();
        readings.disks.pending = std::mem::take(&mut old.disks.pending);
        readings.disks.problem = old.disks.problem.take();
        // The blocked report is a read of its own: a plain reading of the firewall keeps the
        // last one until another is asked for. Its own problem is the reading's.
        readings.firewall.pending = std::mem::take(&mut old.firewall.pending);
        readings.firewall.problem = old.firewall.problem.take();
        readings.firewall.blocked = std::mem::take(&mut old.firewall.blocked);
        readings.firewall.blocked_total = old.firewall.blocked_total;
        readings.firewall.blocked_window_minutes = old.firewall.blocked_window_minutes;
        readings.firewall.blocked_read_at_ms = old.firewall.blocked_read_at_ms;
        readings.firewall.blocked_error = old.firewall.blocked_error.take();
        readings.firewall.blocked_note = old.firewall.blocked_note.take();
        // The journal is read by its own verbs: a plain read of the machine keeps it.
        readings.journal.entries = std::mem::take(&mut old.journal.entries);
        readings.journal.cursor = std::mem::take(&mut old.journal.cursor);
        readings.journal.serving = old.journal.serving;
        readings.journal.read_error = old.journal.read_error.take();
        readings.journal.following = old.journal.following;
        readings.journal.read_at_ms = old.journal.read_at_ms;
        readings.journal.pending = std::mem::take(&mut old.journal.pending);
        readings.journal.problem = old.journal.problem.take();
        // A benchmark stays until the next one: a plain read does not clear it.
        if readings.disks.benchmark.is_none() {
            readings.disks.benchmark = old.disks.benchmark.take();
        }
        readings.firmware.installing = readings
            .firmware
            .pending
            .iter()
            .any(|action| action == "install");
        readings.disk_unlock.pending = std::mem::take(&mut old.disk_unlock.pending);
        readings.disk_unlock.problem = old.disk_unlock.problem.take();
        if !readings.disk_unlock.scanned {
            readings.disk_unlock.volumes = std::mem::take(&mut old.disk_unlock.volumes);
            readings.disk_unlock.saved_networks =
                std::mem::take(&mut old.disk_unlock.saved_networks);
            readings.disk_unlock.read_error = old.disk_unlock.read_error.take();
            readings.disk_unlock.scanned = old.disk_unlock.scanned;
        }
        // What a find turned up stays until the next one: a plain read of the queues and
        // jobs replaces neither.
        if readings.printers.nearby.is_empty() {
            readings.printers.nearby = std::mem::take(&mut old.printers.nearby);
        }
        readings.update.updating = readings
            .update
            .pending
            .iter()
            .any(|action| action == "update-now");
        self.readings = readings;
        self.sync_power_profiles();
    }

    /// Take in a journal read: its records, its cursor and why it failed, if it did.
    fn land_journal(
        &mut self,
        action: &str,
        result: Result<(Vec<tilo_system_resources::journal::JournalEntry>, String), String>,
        replace: bool,
        read_at_ms: u64,
    ) {
        let pending = &mut self.readings.journal.pending;
        if let Some(at) = pending.iter().position(|id| *id == action) {
            pending.remove(at);
        }
        match result {
            Ok((entries, cursor)) => {
                let journal = &mut self.readings.journal;
                if replace {
                    journal.entries = entries;
                } else {
                    journal.entries.extend(entries);
                    let too_many = journal.entries.len().saturating_sub(MAX_JOURNAL_ENTRIES);
                    if too_many > 0 {
                        journal.entries.drain(..too_many);
                    }
                }
                journal.cursor = cursor;
                journal.serving = true;
                journal.read_error = None;
                journal.read_at_ms = read_at_ms;
                journal.problem = None;
            }
            Err(error) => {
                let journal = &mut self.readings.journal;
                journal.problem = Some(error.clone());
                if replace {
                    journal.serving = false;
                    journal.read_error = Some(error);
                }
            }
        }
        let cursor = self.readings.journal.cursor.clone();
        if let Some(follow) = &self.journal_follow
            && let Ok(mut held) = follow.cursor.lock()
        {
            *held = cursor;
        }
    }

    fn land(&mut self, landed: Landed) {
        match landed {
            Landed::Read(readings) => self.take(*readings),
            Landed::Update(update) => {
                let readings = Readings {
                    update: *update,
                    ..self.readings.clone()
                };
                self.take(readings);
            }
            Landed::Changed {
                part,
                action,
                result,
                readings,
                live_power_mode,
            } => {
                {
                    let (pending, problem) = self.status(part);
                    if let Some(at) = pending.iter().position(|id| *id == action) {
                        pending.remove(at);
                    }
                    *problem = result.err();
                }
                self.take(*readings);
                if let Some((rev_before, mode)) = live_power_mode {
                    let superseded = self
                        .live_profiles
                        .as_ref()
                        .is_some_and(|live| live.seen > rev_before);
                    if !superseded {
                        self.readings.power.mode = Some(mode);
                    }
                }
            }
            Landed::Journal {
                action,
                result,
                replace,
                read_at_ms,
            } => self.land_journal(&action, result, replace, read_at_ms),
            Landed::Blocked {
                action,
                window_minutes,
                read_at_ms,
                result,
            } => {
                let pending = &mut self.readings.firewall.pending;
                if let Some(at) = pending.iter().position(|id| *id == action) {
                    pending.remove(at);
                }
                let firewall = &mut self.readings.firewall;
                match result {
                    Ok((groups, total, note)) => {
                        firewall.blocked = groups;
                        firewall.blocked_total = total;
                        firewall.blocked_window_minutes = window_minutes;
                        firewall.blocked_read_at_ms = read_at_ms;
                        firewall.blocked_error = None;
                        firewall.blocked_note = note;
                    }
                    Err(error) => firewall.blocked_error = Some(error),
                }
            }
            Landed::Windows { action, result } => {
                let mut cache = self
                    .windows_cache
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                cache.pending.retain(|id| *id != action);
                cache.problem = result.err();
                cache.by_workspace.clear();
            }
        }
    }

    fn start(&mut self, part: Part, action: &str, change: Change) -> Started {
        self.status(part).0.push(action.to_owned());
        if action == "update-now" {
            self.readings.update.updating = true;
        }
        let action = action.to_owned();
        let follow = matches!(change, Change::AutomaticZone(true));
        let live_active = self.live_active_flag();
        let live_source = self
            .live_profiles
            .as_ref()
            .map(|live| Arc::clone(&live.source));
        let look_active = live_active.clone();
        self.worker.run(move |system| {
            let result = system.change(&change);
            let live_now = is_live(live_active.as_ref());
            let live_power_mode =
                if live_now && result.is_ok() && matches!(change, Change::PowerMode(_)) {
                    let rev_before = live_source.as_ref().map_or(0, |source| source.revision());
                    system.power_mode().map(|mode| (rev_before, mode))
                } else {
                    None
                };
            Landed::Changed {
                part,
                action,
                result,
                readings: Box::new(read(system, live_now)),
                live_power_mode,
            }
        });
        if follow {
            match &self.follower {
                Some(follower) => follower.unpark(),
                None => look(&self.worker.jobs(), look_active),
            }
        }
        Started::Accepted
    }

    /// Make `change` on the printers, landing the machine's answer and a fresh reading.
    fn start_print(&mut self, action: &str, change: PrintChange) -> Started {
        self.readings.printers.pending.push(action.to_owned());
        let action = action.to_owned();
        let live_active = self.live_active_flag();
        self.worker.run(move |system| {
            let result = system.change_print(&change);
            Landed::Changed {
                part: Part::Printers,
                action,
                result,
                readings: Box::new(read(system, is_live(live_active.as_ref()))),
                live_power_mode: None,
            }
        });
        Started::Accepted
    }

    /// Read one part of the machine again, landing its pending action and a fresh reading.
    fn start_reread(&mut self, part: Part, action: &str) -> Started {
        self.status(part).0.push(action.to_owned());
        let action = action.to_owned();
        let live_active = self.live_active_flag();
        self.worker.run(move |system| Landed::Changed {
            part,
            action,
            result: Ok(()),
            readings: Box::new(read(system, is_live(live_active.as_ref()))),
            live_power_mode: None,
        });
        Started::Accepted
    }

    /// Read the storage again, or make `change` on it, landing the machine's answer and the scan
    /// that follows.
    fn start_storage(&mut self, action: &str, change: Option<StorageChange>) -> Started {
        self.readings.storage.pending.push(action.to_owned());
        let action = action.to_owned();
        let live_active = self.live_active_flag();
        self.worker.run(move |system| {
            let result = match change {
                Some(change) => system.change_storage(&change),
                None => Ok(()),
            };
            let mut readings = read(system, is_live(live_active.as_ref()));
            readings.storage = system.scan_storage();
            Landed::Changed {
                part: Part::Storage,
                action,
                result,
                readings: Box::new(readings),
                live_power_mode: None,
            }
        });
        Started::Accepted
    }

    /// Make `change` on one of the extension-backed resources (its status follows a helper), or
    /// read it again, landing the machine's answer and a fresh reading.
    fn start_extension(&mut self, part: Part, action: &str, change: ExtensionChange) -> Started {
        self.status(part).0.push(action.to_owned());
        if part == Part::Firmware && action == "install" {
            self.readings.firmware.installing = true;
        }
        let action = action.to_owned();
        let live_active = self.live_active_flag();
        self.worker.run(move |system| {
            let result = match &change {
                ExtensionChange::Codec(change) => system.change_codec(change),
                ExtensionChange::Windows(change) => system.change_windows(change),
                ExtensionChange::Firmware(change) => system.change_firmware(change),
            };
            Landed::Changed {
                part,
                action,
                result,
                readings: Box::new(read(system, is_live(live_active.as_ref()))),
                live_power_mode: None,
            }
        });
        Started::Accepted
    }

    /// Read the enrolled volumes again, or make `change` on them, landing the machine's answer
    /// and the read that follows.
    fn start_disk_unlock(&mut self, action: &str, change: Option<DiskUnlockChange>) -> Started {
        self.readings.disk_unlock.pending.push(action.to_owned());
        let action = action.to_owned();
        let live_active = self.live_active_flag();
        self.worker.run(move |system| {
            let result = match change {
                Some(change) => system.change_disk_unlock(&change),
                None => Ok(()),
            };
            let mut readings = read(system, is_live(live_active.as_ref()));
            readings.disk_unlock = system.scan_disk_unlock();
            Landed::Changed {
                part: Part::DiskUnlock,
                action,
                result,
                readings: Box::new(readings),
                live_power_mode: None,
            }
        });
        Started::Accepted
    }

    /// Make `change` on the engine, landing its answer and a fresh reading.
    fn start_container(&mut self, action: &str, change: ContainerChange) -> Started {
        self.readings.containers.pending.push(action.to_owned());
        let action = action.to_owned();
        let live_active = self.live_active_flag();
        self.worker.run(move |system| {
            let result = system.change_container(&change);
            Landed::Changed {
                part: Part::Containers,
                action,
                result,
                readings: Box::new(read(system, is_live(live_active.as_ref()))),
                live_power_mode: None,
            }
        });
        Started::Accepted
    }

    /// Make `change` on the drives, landing the machine's answer and a fresh reading.
    fn start_disk(&mut self, action: &str, change: DiskChange) -> Started {
        self.readings.disks.pending.push(action.to_owned());
        let action = action.to_owned();
        let live_active = self.live_active_flag();
        self.worker.run(move |system| {
            let result = system.change_disk(&change);
            Landed::Changed {
                part: Part::Disks,
                action,
                result,
                readings: Box::new(read(system, is_live(live_active.as_ref()))),
                live_power_mode: None,
            }
        });
        Started::Accepted
    }

    /// Apply a configuration to this computer's firewalld, landing its answer and a fresh
    /// reading of what firewalld holds.
    fn start_firewall_apply(
        &mut self,
        action: &str,
        config: tilo_system_resources::firewall::Firewall,
    ) -> Started {
        self.readings.firewall.pending.push(action.to_owned());
        let action = action.to_owned();
        let live_active = self.live_active_flag();
        self.worker.run(move |system| {
            let result = system.apply_firewall(&config).map(|_| ());
            Landed::Changed {
                part: Part::Firewall,
                action,
                result,
                readings: Box::new(read(system, is_live(live_active.as_ref()))),
                live_power_mode: None,
            }
        });
        Started::Accepted
    }

    /// Read the kernel's drops from this device's journal, landing them as the blocked report.
    fn start_blocked(&mut self, action: &str, window_minutes: u32) -> Started {
        self.readings.firewall.pending.push(action.to_owned());
        let action = action.to_owned();
        self.worker.run(move |system| Landed::Blocked {
            action,
            window_minutes,
            read_at_ms: crate::firewall::now_ms(),
            result: system.blocked(window_minutes),
        });
        Started::Accepted
    }

    /// Read the journal, replacing its records or continuing them.
    fn start_journal(
        &mut self,
        action: &str,
        after: Option<String>,
        limit: u32,
        replace: bool,
    ) -> Started {
        self.status(Part::Journal).0.push(action.to_owned());
        let action = action.to_owned();
        self.worker.run(move |system| {
            let result = match &after {
                Some(cursor) => system.read_journal(Some(cursor), limit),
                None => system.read_journal(None, limit),
            };
            Landed::Journal {
                action,
                result,
                replace,
                read_at_ms: crate::journal::now_ms(),
            }
        });
        Started::Accepted
    }

    /// Start or stop following the journal as new records arrive.
    fn start_following(&mut self, on: bool) {
        self.readings.journal.following = on;
        if !on {
            if let Some(follow) = self.journal_follow.take() {
                follow.following.store(false, Ordering::Release);
                follow.thread.unpark();
            }
            return;
        }
        if self.journal_follow.is_some() {
            return;
        }
        let following = Arc::new(AtomicBool::new(true));
        let cursor = Arc::new(Mutex::new(self.readings.journal.cursor.clone()));
        let jobs = self.worker.jobs();
        let flag = Arc::clone(&following);
        let held = Arc::clone(&cursor);
        let spawned = std::thread::Builder::new()
            .name("tilo-journal".into())
            .spawn(move || {
                while flag.load(Ordering::Acquire) {
                    let after = held.lock().map(|c| c.clone()).unwrap_or_default();
                    jobs.run(move |system| {
                        let cursor = if after.is_empty() {
                            None
                        } else {
                            Some(after.as_str())
                        };
                        Landed::Journal {
                            action: "read-after".to_owned(),
                            result: system.read_journal(cursor, 200),
                            replace: false,
                            read_at_ms: crate::journal::now_ms(),
                        }
                    });
                    std::thread::park_timeout(Duration::from_secs(1));
                }
            });
        match spawned {
            Ok(thread) => {
                self.journal_follow = Some(JournalFollow {
                    following,
                    cursor,
                    thread: thread.thread().clone(),
                });
            }
            Err(error) => {
                tracing::warn!(%error, "the journal is not followed live");
                self.readings.journal.following = false;
            }
        }
    }

    /// Measure the device, landing the answer as the reading's benchmark.
    fn start_benchmark(&mut self, action: &str, device: &str) -> Started {
        self.readings.disks.pending.push(action.to_owned());
        let action = action.to_owned();
        let device = device.to_owned();
        let live_active = self.live_active_flag();
        self.worker.run(move |system| {
            let measured = system.benchmark_device(&device);
            let mut readings = read(system, is_live(live_active.as_ref()));
            let result = match measured {
                Ok(benchmark) => {
                    readings.disks.benchmark = Some(benchmark);
                    Ok(())
                }
                Err(error) => Err(error),
            };
            Landed::Changed {
                part: Part::Disks,
                action,
                result,
                readings: Box::new(readings),
                live_power_mode: None,
            }
        });
        Started::Accepted
    }

    /// Make `change` on the processes, landing the service's answer and a fresh reading.
    fn start_process(&mut self, action: &str, change: ProcessChange) -> Started {
        self.readings.processes.pending.push(action.to_owned());
        let action = action.to_owned();
        let live_active = self.live_active_flag();
        self.worker.run(move |system| {
            let result = system.change_process(&change);
            Landed::Changed {
                part: Part::Processes,
                action,
                result,
                readings: Box::new(read(system, is_live(live_active.as_ref()))),
                live_power_mode: None,
            }
        });
        Started::Accepted
    }

    /// Make `change` on the drives, landing the machine's answer and a fresh reading.
    fn start_drive(&mut self, action: &str, change: DriveChange) -> Started {
        self.readings.drives.pending.push(action.to_owned());
        let action = action.to_owned();
        let live_active = self.live_active_flag();
        self.worker.run(move |system| {
            let result = system.change_drive(&change);
            Landed::Changed {
                part: Part::Drives,
                action,
                result,
                readings: Box::new(read(system, is_live(live_active.as_ref()))),
                live_power_mode: None,
            }
        });
        Started::Accepted
    }

    /// Make `change` on the accounts, landing the machine's answer and a fresh reading.
    fn start_user(&mut self, action: &str, change: UserChange) -> Started {
        self.readings.users.pending.push(action.to_owned());
        let action = action.to_owned();
        let live_active = self.live_active_flag();
        self.worker.run(move |system| {
            let result = system.change_user(&change);
            Landed::Changed {
                part: Part::Users,
                action,
                result,
                readings: Box::new(read(system, is_live(live_active.as_ref()))),
                live_power_mode: None,
            }
        });
        Started::Accepted
    }

    /// Look for printers on the network, landing them beside a fresh reading of the queues
    /// and jobs.
    /// Look for the machine's scanners again, and land what SANE answers.
    fn start_scanners(&mut self, action: &str) -> Started {
        self.readings.scanners.pending.push(action.to_owned());
        let action = action.to_owned();
        let live_active = self.live_active_flag();
        self.worker.run(move |system| {
            let readings = read(system, is_live(live_active.as_ref()));
            Landed::Changed {
                part: Part::Scanners,
                action,
                result: Ok(()),
                readings: Box::new(readings),
                live_power_mode: None,
            }
        });
        Started::Accepted
    }

    fn start_find(&mut self, action: &str) -> Started {
        self.readings.printers.pending.push(action.to_owned());
        let action = action.to_owned();
        let live_active = self.live_active_flag();
        self.worker.run(move |system| {
            let found = system.find_printers();
            let mut readings = read(system, is_live(live_active.as_ref()));
            let result = match found {
                Ok(nearby) => {
                    readings.printers.nearby = nearby;
                    Ok(())
                }
                Err(error) => Err(error),
            };
            Landed::Changed {
                part: Part::Printers,
                action,
                result,
                readings: Box::new(readings),
                live_power_mode: None,
            }
        });
        Started::Accepted
    }
}

impl Drop for SystemService {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(follower) = &self.follower {
            follower.unpark();
        }
        if let Some(follow) = self.journal_follow.take() {
            follow.following.store(false, Ordering::Release);
            follow.thread.unpark();
        }
    }
}

fn refresh(jobs: &Jobs<dyn System, Landed>, live_profiles: Option<Arc<AtomicBool>>) {
    jobs.run(move |system| Landed::Read(Box::new(read(system, is_live(live_profiles.as_ref())))));
}

fn reread_update(jobs: &Jobs<dyn System, Landed>) {
    jobs.run(|system| Landed::Update(Box::new(system.update())));
}

/// The live update's state as it is written: its directory watched, and until the first live
/// update makes that directory, its parent watched for it.
struct StateWatch {
    dir: PathBuf,
    jobs: Jobs<dyn System, Landed>,
    /// The state's own directory, once it exists.
    state: Option<tilo_files::native::ChangeWatch>,
    /// Its parent, until it exists.
    parent: Option<tilo_files::native::ChangeWatch>,
    /// The directory appeared or went: [`Self::arm`] again, on the service's thread.
    moved: Arc<AtomicBool>,
}

impl StateWatch {
    fn new(dir: PathBuf, jobs: Jobs<dyn System, Landed>) -> Self {
        Self {
            dir,
            jobs,
            state: None,
            parent: None,
            moved: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Watch the state's directory where it is, else its parent for it; read the update
    /// either way, since it may have changed before the watch began.
    fn arm(&mut self) {
        let state_name = Path::new(crate::update::LIVE_STATE)
            .file_name()
            .map(std::ffi::OsStr::to_os_string);
        let jobs = self.jobs.clone();
        let dir = self.dir.clone();
        let named = self.dir.clone();
        let moved = Arc::clone(&self.moved);
        // The state file itself, by the name it is kept under.
        let is_state = move |path: &Path| path.file_name() == state_name.as_deref();
        let watched = tilo_files::native::watch_changes(
            &self.dir,
            move |path| path == named || is_state(path),
            move || {
                // The directory went (a test, a tidy-up): its parent is watched for it again.
                if !dir.is_dir() {
                    moved.store(true, Ordering::Release);
                }
                reread_update(&jobs);
            },
        );
        match watched {
            Ok(watch) => {
                self.state = Some(watch);
                self.parent = None;
            }
            Err(error) => {
                self.state = None;
                if error.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(%error, dir = %self.dir.display(), "the live update's state is not followed");
                }
                self.watch_parent();
            }
        }
        reread_update(&self.jobs);
    }

    fn watch_parent(&mut self) {
        if self.parent.is_some() {
            return;
        }
        let Some(parent) = self.dir.parent() else {
            return;
        };
        let dir = self.dir.clone();
        let jobs = self.jobs.clone();
        let moved = Arc::clone(&self.moved);
        match tilo_files::native::watch_changes(
            parent,
            move |path| path == dir,
            move || {
                moved.store(true, Ordering::Release);
                // A reading that lands wakes the service, which arms the watch then.
                reread_update(&jobs);
            },
        ) {
            Ok(watch) => self.parent = Some(watch),
            Err(error) => tracing::warn!(
                %error,
                dir = %parent.display(),
                "the live update's state is not followed until the desktop restarts"
            ),
        }
    }

    /// Arm again where the directory appeared or went.
    fn follow(&mut self) {
        if self.moved.swap(false, Ordering::AcqRel) {
            self.arm();
        }
    }
}

/// Set the zone where the device is, when it follows the device.
fn look(jobs: &Jobs<dyn System, Landed>, live_profiles: Option<Arc<AtomicBool>>) {
    jobs.run(move |system| {
        if system.time().automatic {
            match follow_once(system) {
                Ok(Some(zone)) => tracing::info!(%zone, "time zone set from the device's location"),
                Ok(None) => {}
                Err(error) => tracing::info!(%error, "the time zone stays: no location"),
            }
        }
        Landed::Read(Box::new(read(system, is_live(live_profiles.as_ref()))))
    });
}

/// The power resource as a view of another language reads and changes it: its
/// reading ([`Power`]) as its fields and its actions ([`PowerAction`]) as its
/// verbs.
#[must_use]
pub fn power_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<Power, PowerAction>(POWER, "Power")
}

/// The `printers` resource's surface, as a shell reads it over `so.tilo.device`.
#[must_use]
pub fn printers_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<
        tilo_system_resources::printers::Printers,
        tilo_system_resources::printers::PrintAction,
    >(PRINTERS, "Printers")
}

/// The `codec-pack` resource's surface, as a shell reads it over `so.tilo.device`.
#[must_use]
pub fn codec_pack_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<
        tilo_system_resources::codec_pack::CodecPack,
        tilo_system_resources::codec_pack::CodecPackAction,
    >(CODEC_PACK, "Media codecs")
}

/// The `windows-compat` resource's surface, as a shell reads it over `so.tilo.device`.
#[must_use]
pub fn windows_compat_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<
        tilo_system_resources::windows_compat::WindowsCompat,
        tilo_system_resources::windows_compat::WindowsCompatAction,
    >(WINDOWS_COMPAT, "Windows support")
}

/// The `firmware` resource's surface, as a shell reads it over `so.tilo.device`.
#[must_use]
pub fn firmware_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<
        tilo_system_resources::firmware::Firmware,
        tilo_system_resources::firmware::FirmwareAction,
    >(FIRMWARE, "Firmware")
}

/// The `disks` resource's surface, as a shell reads it over `so.tilo.device`.
#[must_use]
pub fn disks_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<
        tilo_system_resources::disks::Disks,
        tilo_system_resources::disks::DiskAction,
    >(DISKS, "Drives")
}

/// The `firewall` resource's surface, as a shell reads it over `so.tilo.device`.
#[must_use]
pub fn firewall_surface() -> tilo_resource::surface::Surface {
    firewall::surface()
}

/// The `journal` resource's surface, as a shell reads it over `so.tilo.device`.
#[must_use]
pub fn journal_surface() -> tilo_resource::surface::Surface {
    journal::surface()
}

/// The `processes` resource's surface, as a shell reads it over `so.tilo.device`.
#[must_use]
pub fn processes_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<
        tilo_system_resources::processes::Processes,
        tilo_system_resources::processes::ProcessAction,
    >(PROCESSES, "Processes")
}

/// The `containers` resource's surface, as a shell reads it over `so.tilo.device`.
#[must_use]
pub fn containers_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<
        tilo_system_resources::containers::Containers,
        tilo_system_resources::containers::ContainerAction,
    >(CONTAINERS, "Containers")
}

/// The `host-info` resource's surface, as a shell reads it over `so.tilo.device`.
#[must_use]
pub fn host_info_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<
        tilo_system_resources::host_info::HostInfo,
        tilo_system_resources::host_info::HostInfoAction,
    >(HOST_INFO, "About this device")
}

/// The `disk-unlock` resource's surface, as a shell reads it over `so.tilo.device`.
#[must_use]
pub fn disk_unlock_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<
        tilo_system_resources::disk_unlock::DiskUnlock,
        tilo_system_resources::disk_unlock::DiskUnlockAction,
    >(DISK_UNLOCK, "Disk unlock")
}

/// The `storage` resource's surface, as a shell reads it over `so.tilo.device`.
#[must_use]
pub fn storage_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<
        tilo_system_resources::storage::Storage,
        tilo_system_resources::storage::StorageAction,
    >(STORAGE, "Storage")
}

/// The `scanners` resource's surface, as a shell reads it over `so.tilo.device`.
#[must_use]
pub fn scanners_surface() -> tilo_resource::surface::Surface {
    scanners::surface()
}

/// The `drives` resource's surface, as a shell reads it over `so.tilo.device`.
#[must_use]
pub fn drives_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<
        tilo_system_resources::drives::Drives,
        tilo_system_resources::drives::DrivesAction,
    >(DRIVES, "Drives")
}

/// The `users` resource's surface, as a shell reads it over `so.tilo.device`.
#[must_use]
pub fn users_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<
        tilo_system_resources::users::Users,
        tilo_system_resources::users::UsersAction,
    >(USERS, "Users")
}

/// The `system-update` resource's surface, as a shell reads it over `so.tilo.device`: what the
/// machine booted, what waits for a restart, and the actions that update it.
#[must_use]
pub fn update_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<Update, UpdateAction>(SYSTEM_UPDATE, "System update")
}

/// The `time-zone` resource's surface: the zone the device keeps and whether it follows the
/// network.
#[must_use]
pub fn time_zone_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<Time, TimeAction>(TIME_ZONE, "Time zone")
}

/// The `power` resource, as a declared type: the modes this device offers and
/// who may use it.
#[derive(tilo_resource::Row)]
#[row(kind = "so.tilo.system.power", label = "Power")]
pub struct PowerReading;

/// The `system-update` resource, as a declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "so.tilo.system.update", label = "System update")]
pub struct UpdateReading;

/// The `time-zone` resource, as a declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "so.tilo.system.time", label = "Time zone")]
pub struct TimeReading;

/// The keyboard's backlight, as a declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "so.tilo.system.keyboard-backlight", label = "Keyboard light")]
pub struct KeyboardLightReading;

/// The `power` resource reading `power`.
#[must_use]
pub fn power_resource(power: &Power) -> ResourceSpec {
    let mut resource = offer::<PowerAction>(tilo_resource::typed_resource::<PowerReading>(
        POWER,
        crate::i18n::name("so.tilo.system.power", "Power"),
    ));
    if power.modes.is_empty() {
        unavailable(
            &mut resource,
            "set-power-mode",
            "no_power_daemon",
            "This device offers no power modes",
        );
    }
    if !power.user_switching {
        unavailable(
            &mut resource,
            "switch-user",
            "no_user_switching",
            "Nobody else can sign in on this device while this session runs",
        );
    }
    resource
}

/// The `system-update` resource reading `update`.
#[must_use]
pub fn update_resource(update: &Update) -> ResourceSpec {
    let mut resource = offer::<UpdateAction>(tilo_resource::typed_resource::<UpdateReading>(
        SYSTEM_UPDATE,
        crate::i18n::name("so.tilo.system.update", "System update"),
    ));
    if update.updating {
        unavailable(
            &mut resource,
            crate::update::UPDATE_NOW,
            "updating",
            "The system is updating",
        );
    }
    if !update.restart_needed {
        unavailable(
            &mut resource,
            crate::update::RESTART_TO_UPDATE,
            "no_restart_needed",
            "No update waits for a restart",
        );
    }
    if !update.desktop_pending || update.restart_needed {
        unavailable(
            &mut resource,
            crate::update::RESTART_DESKTOP,
            "no_desktop_restart",
            "No part of the update waits for the desktop",
        );
    }
    if update.rollback.is_none() {
        unavailable(
            &mut resource,
            crate::update::ROLLBACK,
            "nothing_to_roll_back",
            "There is no previous system to go back to",
        );
    }
    resource
}

/// The `time-zone` resource reading `time`.
#[must_use]
pub fn time_resource(time: &Time) -> ResourceSpec {
    let mut resource = offer::<TimeAction>(tilo_resource::typed_resource::<TimeReading>(
        TIME_ZONE,
        crate::i18n::name("so.tilo.system.time", "Time zone"),
    ));
    if time.network_time.is_none() {
        unavailable(
            &mut resource,
            "set-network-time",
            "no_time_daemon",
            "This device has no network time",
        );
    }
    resource
}

/// The `device:keyboard-backlight` resource reading `light`.
#[must_use]
pub fn keyboard_light_resource(light: &KeyboardLight) -> ResourceSpec {
    let mut resource =
        offer::<KeyboardLightAction>(tilo_resource::typed_resource::<KeyboardLightReading>(
            KEYBOARD_BACKLIGHT,
            crate::i18n::name("so.tilo.system.keyboard-backlight", "Keyboard light"),
        ));
    if light.level.is_none() {
        for action in ["brighter", "dimmer", "toggle", "set-level"] {
            unavailable(
                &mut resource,
                action,
                "no_keyboard_light",
                "This keyboard has no light",
            );
        }
    }
    resource
}

impl SystemService {
    /// An action on the `power` resource.
    fn invoke_power(
        &mut self,
        published: &[ResourceSpec],
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        let action = decode::<PowerAction>(published, resource, action, arguments)?;
        let id = action.id();
        let change = match action {
            PowerAction::SetPowerMode { mode } => {
                let mode = PowerMode::named(&mode)
                    .filter(|mode| self.readings.power.modes.contains(mode))
                    .ok_or_else(|| {
                        ApiError::InvalidArguments(format!(
                            "this device offers no power mode {mode}"
                        ))
                    })?;
                Change::PowerMode(mode)
            }
            PowerAction::Lock => Change::Lock,
            PowerAction::SwitchUser => Change::SwitchUser,
            PowerAction::Suspend => Change::Suspend,
            PowerAction::Restart => Change::Restart,
            PowerAction::PowerOff => Change::PowerOff,
            PowerAction::LogOut => Change::LogOut,
        };
        Ok(self.start(Part::Power, id, change))
    }

    /// An action on the `system-update` resource.
    fn invoke_system_update(
        &mut self,
        published: &[ResourceSpec],
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        let action = decode::<UpdateAction>(published, resource, action, arguments)?;
        let id = action.id();
        let change = match action {
            UpdateAction::UpdateNow => Change::UpdateNow,
            UpdateAction::RestartDesktop => Change::RestartDesktop,
            UpdateAction::RestartToUpdate => Change::Restart,
            UpdateAction::Rollback => Change::Rollback,
        };
        Ok(self.start(Part::Update, id, change))
    }

    /// An action on the `time-zone` resource.
    fn invoke_time_zone(
        &mut self,
        published: &[ResourceSpec],
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        let action = decode::<TimeAction>(published, resource, action, arguments)?;
        let id = action.id();
        let change = match action {
            TimeAction::SetZone { zone } => {
                if !os_actions::valid_timezone(&zone) {
                    return Err(ApiError::InvalidArguments(format!(
                        "{zone} is no time zone"
                    )));
                }
                Change::Zone(zone)
            }
            TimeAction::SetAutomaticZone { on } => Change::AutomaticZone(on),
            TimeAction::SetNetworkTime { on } => Change::NetworkTime(on),
        };
        Ok(self.start(Part::Time, id, change))
    }

    /// An action on the `windows-apps` resource: the programs and the appliance the session's
    /// compatibility service reads.
    fn invoke_windows_apps(
        &self,
        published: &[ResourceSpec],
        workspace: &str,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        if self.windows.is_none() {
            return Err(ApiError::Unavailable {
                code: "no_windows".into(),
                reason: "This computer's Windows compatibility service is not running".into(),
            });
        }
        let action = decode::<tilo_system_resources::windows_apps::WindowsAppsAction>(
            published, resource, action, arguments,
        )?;
        Ok(self.start_windows(workspace, action))
    }

    /// An action on one program of the `windows-apps` reading, called on the address its row
    /// carries (`windows-program:<id>`): opening or stopping the program that id names.
    fn invoke_windows_program(
        &mut self,
        published: &[ResourceSpec],
        workspace: &str,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        use tilo_system_resources::windows_apps::{WindowsAppsAction, WindowsProgramAction};
        if self.windows.is_none() {
            return Err(ApiError::Unavailable {
                code: "no_windows".into(),
                reason: "This computer's Windows compatibility service is not running".into(),
            });
        }
        let program = resource
            .split_once(':')
            .map(|(_, program)| program.to_owned())
            .unwrap_or_default();
        let action = decode::<WindowsProgramAction>(published, resource, action, arguments)?;
        let action = match action {
            WindowsProgramAction::Launch => WindowsAppsAction::Launch { program },
            WindowsProgramAction::Stop => WindowsAppsAction::Stop { program },
        };
        Ok(self.start_windows(workspace, action))
    }

    /// An action on the `keyboard-backlight` resource.
    fn invoke_keyboard_backlight(
        &mut self,
        published: &[ResourceSpec],
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        let action = decode::<KeyboardLightAction>(published, resource, action, arguments)?;
        let id = action.id();
        let Some(light) = model::backlight_of(&self.readings.keyboard_light) else {
            return Err(ApiError::Unavailable {
                code: "no_keyboard_light".into(),
                reason: "This keyboard has no light".into(),
            });
        };
        let level = match action {
            KeyboardLightAction::Brighter => light.step(true),
            KeyboardLightAction::Dimmer => light.step(false),
            KeyboardLightAction::Toggle => light.toggled(),
            KeyboardLightAction::SetLevel { level } => {
                if !(0.0..=1.0).contains(&level) {
                    return Err(ApiError::InvalidArguments(format!(
                        "a level is from 0 to 1, not {level}"
                    )));
                }
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "a fraction of the light's range is within it"
                )]
                let level = (level * f64::from(light.max)).round() as u32;
                level
            }
        };
        Ok(self.start(Part::KeyboardLight, id, Change::KeyboardLight(level)))
    }

    /// An action on the `printers` resource.
    fn invoke_printers(
        &mut self,
        published: &[ResourceSpec],
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        if !self.readings.printers.serving {
            return Err(ApiError::Unavailable {
                code: "no_printing".into(),
                reason: "This computer has no printer service".into(),
            });
        }
        let action = decode::<tilo_system_resources::printers::PrintAction>(
            published, resource, action, arguments,
        )?;
        let id = action.id();
        match action {
            tilo_system_resources::printers::PrintAction::Find => Ok(self.start_find(id)),
            tilo_system_resources::printers::PrintAction::Add { uri, name } => {
                Ok(self.start_print(id, PrintChange::Add { uri, name }))
            }
            tilo_system_resources::printers::PrintAction::Remove { printer } => {
                Ok(self.start_print(id, PrintChange::Remove(printer)))
            }
            tilo_system_resources::printers::PrintAction::SetDefault { printer } => {
                Ok(self.start_print(id, PrintChange::SetDefault(printer)))
            }
            tilo_system_resources::printers::PrintAction::SetMedia { printer, media } => {
                Ok(self.start_print(id, PrintChange::SetMedia(printer, media)))
            }
            tilo_system_resources::printers::PrintAction::SetPaused { printer, paused } => {
                Ok(self.start_print(id, PrintChange::SetPaused(printer, paused)))
            }
            tilo_system_resources::printers::PrintAction::CancelJob { job } => {
                Ok(self.start_print(id, PrintChange::CancelJob(job)))
            }
            tilo_system_resources::printers::PrintAction::RetryJob { job } => {
                Ok(self.start_print(id, PrintChange::RetryJob(job)))
            }
        }
    }

    /// An action on the `scanners` resource.
    fn invoke_scanners(
        &mut self,
        published: &[ResourceSpec],
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        if !self.readings.scanners.available {
            return Err(ApiError::Unavailable {
                code: "no_scanner".into(),
                reason: "This computer has no scanner service".into(),
            });
        }
        let action = decode::<tilo_system_resources::scanners::ScanAction>(
            published, resource, action, arguments,
        )?;
        let id = action.id();
        match action {
            tilo_system_resources::scanners::ScanAction::FindScanners => {
                Ok(self.start_scanners(id))
            }
        }
    }

    /// An action on the `users` resource.
    fn invoke_users(
        &mut self,
        published: &[ResourceSpec],
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        if !self.readings.users.serving {
            return Err(ApiError::Unavailable {
                code: "no_account_service".into(),
                reason: "This computer cannot change its accounts".into(),
            });
        }
        let action = decode::<tilo_system_resources::users::UsersAction>(
            published, resource, action, arguments,
        )?;
        let id = action.id();
        match action {
            tilo_system_resources::users::UsersAction::Add {
                username,
                full_name,
                password,
            } => Ok(self.start_user(
                id,
                UserChange::Add {
                    username,
                    full_name,
                    password,
                },
            )),
            tilo_system_resources::users::UsersAction::SetAdmin { user, admin } => {
                Ok(self.start_user(id, UserChange::SetAdmin { user, admin }))
            }
            tilo_system_resources::users::UsersAction::SetAutomaticLogin { user, on } => {
                Ok(self.start_user(id, UserChange::SetAutomaticLogin { user, on }))
            }
            tilo_system_resources::users::UsersAction::Remove { user, keep_files } => {
                Ok(self.start_user(id, UserChange::Remove { user, keep_files }))
            }
        }
    }

    /// An action on the `drives` resource.
    fn invoke_drives(
        &mut self,
        published: &[ResourceSpec],
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        if !self.readings.drives.serving {
            return Err(ApiError::Unavailable {
                code: "no_drive_service".into(),
                reason: "This computer has no drive service".into(),
            });
        }
        let action = decode::<tilo_system_resources::drives::DrivesAction>(
            published, resource, action, arguments,
        )?;
        let id = action.id();
        match action {
            tilo_system_resources::drives::DrivesAction::Refresh => {
                Ok(self.start_reread(Part::Drives, id))
            }
            tilo_system_resources::drives::DrivesAction::Mount {
                drive,
                safely,
                remember,
            } => Ok(self.start_drive(
                id,
                DriveChange::Mount {
                    drive,
                    safely,
                    remember,
                },
            )),
            tilo_system_resources::drives::DrivesAction::Unmount { drive } => {
                Ok(self.start_drive(id, DriveChange::Unmount { drive }))
            }
            tilo_system_resources::drives::DrivesAction::Forget { grant } => {
                Ok(self.start_drive(id, DriveChange::Forget { grant }))
            }
            tilo_system_resources::drives::DrivesAction::SetMode { grant, safely } => {
                Ok(self.start_drive(id, DriveChange::SetMode { grant, safely }))
            }
        }
    }

    /// An action on the `storage` resource.
    fn invoke_storage(
        &mut self,
        published: &[ResourceSpec],
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        let action = decode::<tilo_system_resources::storage::StorageAction>(
            published, resource, action, arguments,
        )?;
        let id = action.id();
        match action {
            tilo_system_resources::storage::StorageAction::Scan => Ok(self.start_storage(id, None)),
            tilo_system_resources::storage::StorageAction::ClearCache => {
                Ok(self.start_storage(id, Some(StorageChange::ClearCache)))
            }
        }
    }

    /// An action on the `disk-unlock` resource.
    fn invoke_disk_unlock(
        &mut self,
        published: &[ResourceSpec],
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        let action = decode::<tilo_system_resources::disk_unlock::DiskUnlockAction>(
            published, resource, action, arguments,
        )?;
        let id = action.id();
        match action {
            tilo_system_resources::disk_unlock::DiskUnlockAction::Refresh => {
                Ok(self.start_disk_unlock(id, None))
            }
            tilo_system_resources::disk_unlock::DiskUnlockAction::SetNetworks {
                volume,
                networks,
            } => Ok(self
                .start_disk_unlock(id, Some(DiskUnlockChange::SetNetworks { volume, networks }))),
        }
    }

    /// An action on the `host-info` resource.
    fn invoke_host_info(
        &mut self,
        published: &[ResourceSpec],
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        let action = decode::<tilo_system_resources::host_info::HostInfoAction>(
            published, resource, action, arguments,
        )?;
        let id = action.id();
        match action {
            tilo_system_resources::host_info::HostInfoAction::Refresh => {
                Ok(self.start_reread(Part::HostInfo, id))
            }
        }
    }

    /// An action on the `codec-pack` resource.
    fn invoke_codec_pack(
        &mut self,
        published: &[ResourceSpec],
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        let action = decode::<tilo_system_resources::codec_pack::CodecPackAction>(
            published, resource, action, arguments,
        )?;
        let id = action.id();
        let change = match action {
            tilo_system_resources::codec_pack::CodecPackAction::Install => CodecChange::Install,
            tilo_system_resources::codec_pack::CodecPackAction::Remove => CodecChange::Remove,
            tilo_system_resources::codec_pack::CodecPackAction::Refresh => CodecChange::Refresh,
        };
        Ok(self.start_extension(Part::CodecPack, id, ExtensionChange::Codec(change)))
    }

    /// An action on the `windows-compat` resource.
    fn invoke_windows_programs(
        &mut self,
        published: &[ResourceSpec],
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        let action = decode::<tilo_system_resources::windows_compat::WindowsCompatAction>(
            published, resource, action, arguments,
        )?;
        let id = action.id();
        let change = match action {
            tilo_system_resources::windows_compat::WindowsCompatAction::Install => {
                WindowsChange::Install
            }
            tilo_system_resources::windows_compat::WindowsCompatAction::Remove => {
                WindowsChange::Remove
            }
        };
        Ok(self.start_extension(Part::WindowsCompat, id, ExtensionChange::Windows(change)))
    }

    /// An action on the `firmware` resource.
    fn invoke_firmware(
        &mut self,
        published: &[ResourceSpec],
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        let action = decode::<tilo_system_resources::firmware::FirmwareAction>(
            published, resource, action, arguments,
        )?;
        let id = action.id();
        let change = match action {
            tilo_system_resources::firmware::FirmwareAction::Refresh => FirmwareChange::Refresh,
            tilo_system_resources::firmware::FirmwareAction::Install => FirmwareChange::Install,
        };
        Ok(self.start_extension(Part::Firmware, id, ExtensionChange::Firmware(change)))
    }

    /// An action on the `containers` resource.
    fn invoke_containers(
        &mut self,
        published: &[ResourceSpec],
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        if !self.readings.containers.serving {
            return Err(ApiError::Unavailable {
                code: "no_podman".into(),
                reason: "This computer has no podman".into(),
            });
        }
        let action = decode::<tilo_system_resources::containers::ContainerAction>(
            published, resource, action, arguments,
        )?;
        let id = action.id();
        let change = match action {
            tilo_system_resources::containers::ContainerAction::Refresh => {
                return Ok(self.start_reread(Part::Containers, id));
            }
            tilo_system_resources::containers::ContainerAction::Start { container } => {
                ContainerChange::Start(container)
            }
            tilo_system_resources::containers::ContainerAction::Stop { container } => {
                ContainerChange::Stop(container)
            }
            tilo_system_resources::containers::ContainerAction::Restart { container } => {
                ContainerChange::Restart(container)
            }
            tilo_system_resources::containers::ContainerAction::Pause { container } => {
                ContainerChange::Pause(container)
            }
            tilo_system_resources::containers::ContainerAction::Unpause { container } => {
                ContainerChange::Unpause(container)
            }
            tilo_system_resources::containers::ContainerAction::Remove { container } => {
                ContainerChange::Remove(container)
            }
            tilo_system_resources::containers::ContainerAction::Tag { image, tag } => {
                ContainerChange::Tag { image, tag }
            }
            tilo_system_resources::containers::ContainerAction::RemoveImage { image } => {
                ContainerChange::RemoveImage(image)
            }
            tilo_system_resources::containers::ContainerAction::CreateVolume { name } => {
                ContainerChange::CreateVolume(name)
            }
            tilo_system_resources::containers::ContainerAction::RemoveVolume { volume } => {
                ContainerChange::RemoveVolume(volume)
            }
            tilo_system_resources::containers::ContainerAction::Create {
                name,
                image,
                ports,
                env,
                volumes,
                command,
            } => ContainerChange::Create {
                name,
                image,
                ports,
                env,
                volumes,
                command,
            },
            tilo_system_resources::containers::ContainerAction::Pull { .. } => {
                return Err(ApiError::Unavailable {
                    code: "use_the_app".into(),
                    reason: "Pulling an image streams in the app that started it".into(),
                });
            }
        };
        Ok(self.start_container(id, change))
    }

    /// An action on the `processes` resource.
    fn invoke_processes(
        &mut self,
        published: &[ResourceSpec],
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        if !self.readings.processes.serving && self.readings.processes.sampled_ms == 0 {
            return Err(ApiError::Unavailable {
                code: "no_system_info".into(),
                reason: "This computer's system information service is not answering".into(),
            });
        }
        let action = decode::<tilo_system_resources::processes::ProcessAction>(
            published, resource, action, arguments,
        )?;
        let id = action.id();
        let change = match action {
            tilo_system_resources::processes::ProcessAction::Refresh => {
                return Ok(self.start_reread(Part::Processes, id));
            }
            tilo_system_resources::processes::ProcessAction::End { process } => {
                ProcessChange::End(process)
            }
            tilo_system_resources::processes::ProcessAction::Kill { process } => {
                ProcessChange::Kill(process)
            }
            tilo_system_resources::processes::ProcessAction::EndApp { application } => {
                ProcessChange::EndApp(application)
            }
            tilo_system_resources::processes::ProcessAction::SetInterval { ms } => {
                ProcessChange::SetInterval(ms)
            }
        };
        Ok(self.start_process(id, change))
    }

    /// An action on the `firewall` resource.
    fn invoke_firewall(
        &mut self,
        published: &[ResourceSpec],
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        let action = decode::<tilo_system_resources::firewall::FirewallAction>(
            published, resource, action, arguments,
        )?;
        let id = action.id();
        match action {
            tilo_system_resources::firewall::FirewallAction::Refresh => {
                Ok(self.start_reread(Part::Firewall, id))
            }
            tilo_system_resources::firewall::FirewallAction::Apply { config } => {
                let parsed: tilo_system_resources::firewall::Firewall =
                    serde_json::from_str(&config).map_err(|error| {
                        ApiError::InvalidArguments(format!("not a firewall configuration: {error}"))
                    })?;
                Ok(self.start_firewall_apply(id, parsed))
            }
            tilo_system_resources::firewall::FirewallAction::ReadBlocked { window_minutes } => {
                if window_minutes == 0 {
                    return Err(ApiError::InvalidArguments(
                        "a report reads at least one minute".to_owned(),
                    ));
                }
                Ok(self.start_blocked(id, window_minutes))
            }
        }
    }

    /// An action on the `journal` resource.
    fn invoke_journal(
        &mut self,
        published: &[ResourceSpec],
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        let action = decode::<tilo_system_resources::journal::JournalAction>(
            published, resource, action, arguments,
        )?;
        let id = action.id();
        match action {
            tilo_system_resources::journal::JournalAction::Refresh { limit } => {
                Ok(self.start_journal(id, None, limit, true))
            }
            tilo_system_resources::journal::JournalAction::ReadAfter { cursor, limit } => {
                Ok(self.start_journal(id, Some(cursor), limit, false))
            }
            tilo_system_resources::journal::JournalAction::SetFollowing { on } => {
                self.start_following(on);
                Ok(Started::Accepted)
            }
        }
    }

    /// An action on the `disks` resource.
    fn invoke_disks(
        &mut self,
        published: &[ResourceSpec],
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        let action = decode::<tilo_system_resources::disks::DiskAction>(
            published, resource, action, arguments,
        )?;
        let id = action.id();
        match action {
            tilo_system_resources::disks::DiskAction::Refresh => {
                Ok(self.start_reread(Part::Disks, id))
            }
            tilo_system_resources::disks::DiskAction::Mount { device } => {
                Ok(self.start_disk(id, DiskChange::Mount(device)))
            }
            tilo_system_resources::disks::DiskAction::Unmount { device } => {
                Ok(self.start_disk(id, DiskChange::Unmount(device)))
            }
            tilo_system_resources::disks::DiskAction::Format { device, filesystem } => {
                Ok(self.start_disk(id, DiskChange::Format { device, filesystem }))
            }
            tilo_system_resources::disks::DiskAction::Eject { device } => {
                Ok(self.start_disk(id, DiskChange::Eject(device)))
            }
            tilo_system_resources::disks::DiskAction::Benchmark { device } => {
                Ok(self.start_benchmark(id, &device))
            }
        }
    }
}

impl Service for SystemService {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![
            power_resource(&self.readings.power),
            update_resource(&self.readings.update),
            time_resource(&self.readings.time),
            keyboard_light_resource(&self.readings.keyboard_light),
            printers::resource(&self.readings.printers),
            users::resource(&self.readings.users),
            drives::resource(&self.readings.drives),
            storage::resource(&self.readings.storage),
            disk_unlock::resource(&self.readings.disk_unlock),
            host_info::resource(&self.readings.host_info),
            scanners::resource(&self.readings.scanners),
            codec_pack::resource(&self.readings.codec_pack),
            windows_compat::resource(&self.readings.windows_compat),
            // Every program this computer can open is a row of its own, so search finds one by
            // its name and opens it on the address its row carries.
            windows_apps::resource(&self.windows_reading("")),
            firmware::resource(&self.readings.firmware),
            containers::resource(&self.readings.containers),
            processes::resource(&self.readings.processes),
            disks::resource(&self.readings.disks),
            firewall::resource(&self.readings.firewall),
            journal::resource(&self.readings.journal),
        ]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        match resource {
            POWER => Some(reading(&self.readings.power)),
            SYSTEM_UPDATE => Some(reading(&self.readings.update)),
            TIME_ZONE => Some(reading(&self.readings.time)),
            KEYBOARD_BACKLIGHT => Some(reading(&self.readings.keyboard_light)),
            PRINTERS => Some(reading(&self.readings.printers)),
            USERS => Some(reading(&self.readings.users)),
            DRIVES => Some(reading(&self.readings.drives)),
            STORAGE => Some(reading(&self.readings.storage)),
            DISK_UNLOCK => Some(reading(&self.readings.disk_unlock)),
            HOST_INFO => Some(reading(&self.readings.host_info)),
            SCANNERS => Some(reading(&self.readings.scanners)),
            CODEC_PACK => Some(reading(&self.readings.codec_pack)),
            WINDOWS_COMPAT => Some(reading(&self.readings.windows_compat)),
            WINDOWS_APPS => Some(reading(&self.windows_reading(""))),
            FIRMWARE => Some(reading(&self.readings.firmware)),
            CONTAINERS => Some(reading(&self.readings.containers)),
            PROCESSES => Some(reading(&self.readings.processes)),
            DISKS => Some(reading(&self.readings.disks)),
            FIREWALL => Some(reading(&self.readings.firewall)),
            JOURNAL => Some(reading(&self.readings.journal)),
            _ => None,
        }
    }

    fn search_rows(&self) -> Vec<(String, tilo_actions::ReadingRow)> {
        rows::rows(&self.readings)
    }

    /// This computer's Windows programs answered for the workspace reading them: a program the
    /// appliance shows opens under that workspace's sandbox, and its running state is read
    /// there.
    fn read_for(&self, _app: &str, workspace: &str, resource: &str) -> Option<Value> {
        if resource == WINDOWS_APPS {
            return Some(reading(&self.windows_reading(workspace)));
        }
        self.read(resource)
    }

    fn invoke_for(
        &mut self,
        _app: &str,
        workspace: &str,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        let published = self.resources();
        if resource == WINDOWS_APPS {
            return self.invoke_windows_apps(&published, workspace, resource, action, arguments);
        }
        // A program of the reading is a resource of its own: its actions are called on the
        // address its row carries.
        if resource
            .split_once(':')
            .is_some_and(|(kind, _)| kind == WINDOWS_PROGRAM)
        {
            return self.invoke_windows_program(&published, workspace, resource, action, arguments);
        }
        self.invoke(resource, action, arguments)
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        let published = self.resources();
        match resource {
            POWER => self.invoke_power(&published, resource, action, arguments),
            SYSTEM_UPDATE => self.invoke_system_update(&published, resource, action, arguments),
            TIME_ZONE => self.invoke_time_zone(&published, resource, action, arguments),
            KEYBOARD_BACKLIGHT => {
                self.invoke_keyboard_backlight(&published, resource, action, arguments)
            }
            PRINTERS => self.invoke_printers(&published, resource, action, arguments),
            SCANNERS => self.invoke_scanners(&published, resource, action, arguments),
            USERS => self.invoke_users(&published, resource, action, arguments),
            DRIVES => self.invoke_drives(&published, resource, action, arguments),
            STORAGE => self.invoke_storage(&published, resource, action, arguments),
            DISK_UNLOCK => self.invoke_disk_unlock(&published, resource, action, arguments),
            HOST_INFO => self.invoke_host_info(&published, resource, action, arguments),
            CODEC_PACK => self.invoke_codec_pack(&published, resource, action, arguments),
            WINDOWS_COMPAT => self.invoke_windows_programs(&published, resource, action, arguments),
            WINDOWS_APPS => self.invoke_windows_apps(&published, "", resource, action, arguments),
            WINDOWS_PROGRAM => {
                self.invoke_windows_program(&published, "", resource, action, arguments)
            }
            FIRMWARE => self.invoke_firmware(&published, resource, action, arguments),
            CONTAINERS => self.invoke_containers(&published, resource, action, arguments),
            PROCESSES => self.invoke_processes(&published, resource, action, arguments),
            DISKS => self.invoke_disks(&published, resource, action, arguments),
            FIREWALL => self.invoke_firewall(&published, resource, action, arguments),
            JOURNAL => self.invoke_journal(&published, resource, action, arguments),
            _ => Err(ApiError::Unknown(tilo_actions::ActionAddress::new(
                resource, action,
            ))),
        }
    }

    fn poll(&mut self) -> bool {
        if let Some(watch) = &mut self.update_watch {
            watch.follow();
        }
        let mut changed = self.sync_power_profiles();
        let landed = self.worker.landed();
        if !landed.is_empty() {
            changed = true;
            for landed in landed {
                self.land(landed);
            }
        }
        changed
    }

    fn settle(&mut self) {
        self.sync_power_profiles();
        for landed in self.worker.settle() {
            self.land(landed);
        }
        self.sync_power_profiles();
    }
}

#[cfg(test)]
mod tests;
