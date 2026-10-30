//! The machine under the service. The session's is [`OsSystem`]
//! (power-profiles-daemon, logind, timedated, the update units, the
//! Location portal); a test lends one of its own.

use settings_store::day_schedule::Location;

use crate::model::{Power, PowerMode, Time};
use crate::update::Update;

/// A change to make on the machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    PowerMode(PowerMode),
    Lock,
    SwitchUser,
    Suspend,
    Restart,
    PowerOff,
    LogOut,
    UpdateNow,
    /// The zone the person picked: following the device stops.
    Zone(String),
    /// The zone where the device is, while it follows the device.
    FollowedZone(String),
    NetworkTime(bool),
    AutomaticZone(bool),
}

/// The machine: reads and changes. Called on the service's worker.
pub trait System: Send + Sync {
    fn power(&self) -> Power;
    fn update(&self) -> Update;
    fn time(&self) -> Time;

    /// Make `change`.
    ///
    /// # Errors
    /// Why the machine did not make it.
    fn change(&self, change: &Change) -> Result<(), String>;

    /// Where the device is.
    ///
    /// # Errors
    /// No location: no `GeoClue`, permission denied, no fix.
    fn locate(&self) -> Result<Location, String>;

    /// The zone of the city nearest `location`.
    fn nearest_zone(&self, location: Location) -> Option<String>;
}

/// The machine as its daemons report it.
#[derive(Clone, Copy, Debug, Default)]
pub struct OsSystem;

impl System for OsSystem {
    fn power(&self) -> Power {
        let modes = os_actions::available_power_profiles()
            .unwrap_or_default()
            .into_iter()
            .map(PowerMode::from)
            .collect();
        Power {
            modes,
            mode: os_actions::get_power_profile().ok().map(PowerMode::from),
            user_switching: os_actions::user_switching_supported(),
            ..Power::default()
        }
    }

    fn update(&self) -> Update {
        let live = crate::update::live_state();
        let staged = live.is_none() && crate::update::staged();
        Update::of(staged, live.as_ref())
    }

    fn time(&self) -> Time {
        let status = os_actions::get_time_sync_status().unwrap_or_default();
        Time {
            zone: status.timezone,
            automatic: settings_store::clock::AUTOMATIC_ZONE.get(),
            network_time: status.ntp_enabled,
            synchronized: status.synchronized.unwrap_or(false),
            ..Time::default()
        }
    }

    fn change(&self, change: &Change) -> Result<(), String> {
        use os_actions::{SessionAction, perform_session_action};
        match change {
            Change::PowerMode(mode) => os_actions::set_power_profile((*mode).into()),
            Change::Lock => perform_session_action(SessionAction::Lock),
            Change::SwitchUser => perform_session_action(SessionAction::SwitchUser),
            Change::Suspend => perform_session_action(SessionAction::Suspend),
            Change::Restart => perform_session_action(SessionAction::Restart),
            Change::PowerOff => perform_session_action(SessionAction::ShutDown),
            Change::LogOut => perform_session_action(SessionAction::Logout),
            Change::UpdateNow => crate::update::update_now(),
            Change::Zone(zone) => {
                // A zone the person picks turns following the device off.
                settings_store::clock::AUTOMATIC_ZONE
                    .set(false)
                    .map_err(|error| error.to_string())?;
                os_actions::set_timezone(zone)
            }
            Change::FollowedZone(zone) => os_actions::set_timezone(zone),
            Change::NetworkTime(on) => os_actions::set_ntp(*on),
            Change::AutomaticZone(on) => settings_store::clock::AUTOMATIC_ZONE
                .set(*on)
                .map_err(|error| error.to_string()),
        }
    }

    fn locate(&self) -> Result<Location, String> {
        match tilo_location::system()
            .locate()
            .recv()
            .map_err(|_| "the location service went away".to_owned())?
        {
            tilo_location::LocationOutcome::Located { lat, lon, .. } => Ok(Location {
                latitude: lat,
                longitude: lon,
            }),
            tilo_location::LocationOutcome::Unavailable(reason) => Err(reason),
        }
    }

    fn nearest_zone(&self, location: Location) -> Option<String> {
        settings_store::clock::ZONE_TABLES
            .iter()
            .find_map(|table| std::fs::read_to_string(table).ok())
            .and_then(|table| settings_store::clock::nearest_zone_in(location, &table))
    }
}

/// Look where the device is once and set the zone there, if it is another;
/// nothing is guessed: without a location the zone stays. The zone set, if
/// one was.
///
/// # Errors
/// Why the device could not be located or its zone not set.
pub fn follow_once(system: &dyn System) -> Result<Option<String>, String> {
    let location = system.locate()?;
    let Some(zone) = system.nearest_zone(location) else {
        return Ok(None);
    };
    if system.time().zone.as_deref() == Some(zone.as_str()) {
        return Ok(None);
    }
    system.change(&Change::FollowedZone(zone.clone()))?;
    Ok(Some(zone))
}
