//! The session's system service.
//!
//! It publishes three resources, each a reading of the machine and the
//! actions that change it:
//!
//! * `power`: the power mode, and the session's power actions (sleep,
//!   restart, power off, log out; the last three ask for confirmation);
//! * `system-update`: what the update service staged and what finishing it
//!   needs, with updating now and restarting to finish it;
//! * `time-zone`: the zone, whether it follows where the device is, and
//!   network time. While it follows the device, the service looks where the
//!   device is when that is turned on and every [`LOOK_AGAIN`] after, and
//!   sets the zone of the nearest city; nothing is guessed without a
//!   location.
//!
//! The machine's calls run on the service's worker; an action is pending
//! in its reading until it lands, and why a change did not happen is the
//! reading's problem until the next.

pub mod device;
pub mod effects;
pub mod model;
pub mod update;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{Map, Value};
use tilo_actions::{DoorError, ResourceSpec};
use tilo_resource::Effect as _;
use tilo_service::{Jobs, Service, Started, Wake, Worker, decode, offer, reading, unavailable};

use device::{Change, System, follow_once};
use effects::{PowerAction, TimeAction, UpdateAction};
use model::{Power, PowerMode, Time};
use update::Update;

/// The resources' ids.
pub const POWER: &str = "power";
pub const SYSTEM_UPDATE: &str = "system-update";
pub const TIME_ZONE: &str = "time-zone";

/// How often a device whose zone follows it looks where it is again: often
/// enough that a flight's arrival shows the local time within the hour.
pub const LOOK_AGAIN: Duration = Duration::from_secs(30 * 60);

/// Which resource a change belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Power,
    Update,
    Time,
}

/// What the machine reads now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Readings {
    pub power: Power,
    pub update: Update,
    pub time: Time,
}

fn read(system: &dyn System) -> Readings {
    Readings {
        power: system.power(),
        update: system.update(),
        time: system.time(),
    }
}

/// What lands from the worker.
enum Landed {
    Read(Box<Readings>),
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

/// The `power` resource reading `power`.
#[must_use]
pub fn power_resource(power: &Power) -> ResourceSpec {
    let mut resource =
        offer::<PowerAction>(ResourceSpec::new(POWER, "so.tilo.system.power", "Power"));
    if power.modes.is_empty() {
        unavailable(
            &mut resource,
            "set-power-mode",
            "no_power_daemon",
            "This device offers no power modes",
        );
    }
    resource
}

/// The `system-update` resource reading `update`.
#[must_use]
pub fn update_resource(update: &Update) -> ResourceSpec {
    let mut resource = offer::<UpdateAction>(ResourceSpec::new(
        SYSTEM_UPDATE,
        "so.tilo.system.update",
        "System update",
    ));
    if update.updating {
        unavailable(
            &mut resource,
            "update-now",
            "updating",
            "The system is updating",
        );
    }
    if !update.restart_needed {
        unavailable(
            &mut resource,
            "restart-to-update",
            "no_restart_needed",
            "No update waits for a restart",
        );
    }
    resource
}

/// The `time-zone` resource reading `time`.
#[must_use]
pub fn time_resource(time: &Time) -> ResourceSpec {
    let mut resource = offer::<TimeAction>(ResourceSpec::new(
        TIME_ZONE,
        "so.tilo.system.time",
        "Time zone",
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

impl Service for SystemService {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![
            power_resource(&self.readings.power),
            update_resource(&self.readings.update),
            time_resource(&self.readings.time),
        ]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        match resource {
            POWER => Some(reading(&self.readings.power)),
            SYSTEM_UPDATE => Some(reading(&self.readings.update)),
            TIME_ZONE => Some(reading(&self.readings.time)),
            _ => None,
        }
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, DoorError> {
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
                                DoorError::InvalidArguments(format!(
                                    "this device offers no power mode {mode}"
                                ))
                            })?;
                        Change::PowerMode(mode)
                    }
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
                            return Err(DoorError::InvalidArguments(format!(
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
            _ => Err(DoorError::Unknown(tilo_actions::ActionAddress::new(
                resource, action,
            ))),
        }
    }

    fn poll(&mut self) -> bool {
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
