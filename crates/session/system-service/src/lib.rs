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

pub mod device;
pub use tilo_system_resources::effects;
tilo_resource::declared_names!();

pub mod model;
pub mod update;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{Map, Value};
use tilo_actions::{ApiError, ResourceSpec};
use tilo_resource::Effect as _;
use tilo_service::{Jobs, Service, Started, Wake, Worker, decode, offer, reading, unavailable};

use device::{Change, System, follow_once};
use effects::{KeyboardLightAction, PowerAction, TimeAction, UpdateAction};
use model::{KeyboardLight, Power, PowerMode, Time};
use update::Update;

/// The resources' ids.
pub use tilo_system_resources::{KEYBOARD_BACKLIGHT, POWER, SYSTEM_UPDATE, TIME_ZONE};

/// How often a device whose zone follows it looks where it is again: often
/// enough that a flight's arrival shows the local time within the hour.
pub const LOOK_AGAIN: Duration = Duration::from_secs(30 * 60);

/// Which resource a change belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Power,
    Update,
    Time,
    KeyboardLight,
}

/// What the machine reads now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Readings {
    pub power: Power,
    pub update: Update,
    pub time: Time,
    pub keyboard_light: KeyboardLight,
}

fn read(system: &dyn System) -> Readings {
    Readings {
        power: system.power(),
        update: system.update(),
        time: system.time(),
        keyboard_light: system.keyboard_light(),
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
    },
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
}

impl SystemService {
    /// The service over `system`, reading it once at the start.
    ///
    /// # Errors
    /// The worker's thread did not start.
    pub fn new(system: Arc<dyn System>, wake: Option<Wake>) -> std::io::Result<Self> {
        let service = Self {
            readings: Readings::default(),
            worker: Worker::spawn("tilo-system", system, wake)?,
            stopped: Arc::new(AtomicBool::new(false)),
            follower: None,
            update_watch: None,
        };
        refresh(&service.worker.jobs());
        Ok(service)
    }

    /// Let the time zone follow the device while the person chose so:
    /// look now, and every [`LOOK_AGAIN`] after.
    ///
    /// # Errors
    /// The follower's thread did not start.
    pub fn follow_location(&mut self) -> std::io::Result<()> {
        let jobs = self.worker.jobs();
        let stopped = Arc::clone(&self.stopped);
        let follower = std::thread::Builder::new()
            .name("tilo-time-zone".into())
            .spawn(move || {
                while !stopped.load(Ordering::Acquire) {
                    look(&jobs);
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
        refresh(&self.worker.jobs());
    }

    /// What the machine reads now.
    #[must_use]
    pub const fn readings(&self) -> &Readings {
        &self.readings
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
        }
    }

    fn take(&mut self, mut readings: Readings) {
        let old = &mut self.readings;
        readings.power.pending = std::mem::take(&mut old.power.pending);
        readings.power.problem = old.power.problem.take();
        readings.update.pending = std::mem::take(&mut old.update.pending);
        readings.update.problem = old.update.problem.take();
        readings.time.pending = std::mem::take(&mut old.time.pending);
        readings.time.problem = old.time.problem.take();
        readings.keyboard_light.pending = std::mem::take(&mut old.keyboard_light.pending);
        readings.keyboard_light.problem = old.keyboard_light.problem.take();
        readings.update.updating = readings
            .update
            .pending
            .iter()
            .any(|action| action == "update-now");
        self.readings = readings;
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
            } => {
                {
                    let (pending, problem) = self.status(part);
                    if let Some(at) = pending.iter().position(|id| *id == action) {
                        pending.remove(at);
                    }
                    *problem = result.err();
                }
                self.take(*readings);
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
        self.worker.run(move |system| {
            let result = system.change(&change);
            Landed::Changed {
                part,
                action,
                result,
                readings: Box::new(read(system)),
            }
        });
        if follow {
            match &self.follower {
                Some(follower) => follower.unpark(),
                None => look(&self.worker.jobs()),
            }
        }
        Started::Accepted
    }
}

impl Drop for SystemService {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(follower) = &self.follower {
            follower.unpark();
        }
    }
}

fn refresh(jobs: &Jobs<dyn System, Landed>) {
    jobs.run(|system| Landed::Read(Box::new(read(system))));
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
fn look(jobs: &Jobs<dyn System, Landed>) {
    jobs.run(|system| {
        if system.time().automatic {
            match follow_once(system) {
                Ok(Some(zone)) => tracing::info!(%zone, "time zone set from the device's location"),
                Ok(None) => {}
                Err(error) => tracing::info!(%error, "the time zone stays: no location"),
            }
        }
        Landed::Read(Box::new(read(system)))
    });
}

/// The power resource as a view of another language reads and changes it: its
/// reading ([`Power`]) as its fields and its actions ([`PowerAction`]) as its
/// verbs.
#[must_use]
pub fn power_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<Power, PowerAction>(POWER, "Power")
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

impl Service for SystemService {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![
            power_resource(&self.readings.power),
            update_resource(&self.readings.update),
            time_resource(&self.readings.time),
            keyboard_light_resource(&self.readings.keyboard_light),
        ]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        match resource {
            POWER => Some(reading(&self.readings.power)),
            SYSTEM_UPDATE => Some(reading(&self.readings.update)),
            TIME_ZONE => Some(reading(&self.readings.time)),
            KEYBOARD_BACKLIGHT => Some(reading(&self.readings.keyboard_light)),
            _ => None,
        }
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        let published = self.resources();
        match resource {
            POWER => {
                let action = decode::<PowerAction>(&published, resource, action, arguments)?;
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
            SYSTEM_UPDATE => {
                let action = decode::<UpdateAction>(&published, resource, action, arguments)?;
                let id = action.id();
                let change = match action {
                    UpdateAction::UpdateNow => Change::UpdateNow,
                    UpdateAction::RestartDesktop => Change::RestartDesktop,
                    UpdateAction::RestartToUpdate => Change::Restart,
                };
                Ok(self.start(Part::Update, id, change))
            }
            TIME_ZONE => {
                let action = decode::<TimeAction>(&published, resource, action, arguments)?;
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
            KEYBOARD_BACKLIGHT => {
                let action =
                    decode::<KeyboardLightAction>(&published, resource, action, arguments)?;
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
            _ => Err(ApiError::Unknown(tilo_actions::ActionAddress::new(
                resource, action,
            ))),
        }
    }

    fn poll(&mut self) -> bool {
        if let Some(watch) = &mut self.update_watch {
            watch.follow();
        }
        let landed = self.worker.landed();
        let changed = !landed.is_empty();
        for landed in landed {
            self.land(landed);
        }
        changed
    }

    fn settle(&mut self) {
        for landed in self.worker.settle() {
            self.land(landed);
        }
    }
}

#[cfg(test)]
mod tests;
