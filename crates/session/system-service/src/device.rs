//! The machine under the service. The session's is [`OsSystem`]
//! (power-profiles-daemon, logind, timedated, the update units, the
//! Location portal, the keyboard's LED); a test lends one of its own.

use settings_store::day_schedule::Location;

use crate::model::{KeyboardLight, Power, PowerMode, Time};
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
    /// Finish the update that waits for the desktop, restarting it in place.
    RestartDesktop,
    /// The zone the person picked: following the device stops.
    Zone(String),
    /// The zone where the device is, while it follows the device.
    FollowedZone(String),
    NetworkTime(bool),
    AutomaticZone(bool),
    /// The keyboard light's level, in the light's own steps.
    KeyboardLight(u32),
}

/// Live power-profile readings from the session's power service, which follows
/// `power-profiles-daemon` over D-Bus.
pub trait PowerProfiles: Send + Sync {
    /// The modes the daemon currently advertises and the mode in force, if any.
    fn snapshot(&self) -> (Vec<PowerMode>, Option<PowerMode>);

    /// A revision that moves whenever [`Self::snapshot`] changes.
    fn revision(&self) -> u64;

    /// Call `wake` whenever the daemon reports a new mode or list of modes.
    fn on_change(&self, wake: tilo_service::Wake);
}

/// The machine: reads and changes. Called on the service's worker.
pub trait System: Send + Sync {
    fn power(&self) -> Power;

    /// Whether another person may sign in beside this session. Used instead of
    /// [`Self::power`] when power profiles arrive from a live [`PowerProfiles`]
    /// source rather than polling `power-profiles-daemon`.
    fn user_switching(&self) -> bool {
        self.power().user_switching
    }

    fn update(&self) -> Update;
    fn time(&self) -> Time;
    fn keyboard_light(&self) -> KeyboardLight;

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
            .map(crate::model::power_mode)
            .collect();
        Power {
            modes,
            mode: os_actions::get_power_profile()
                .ok()
                .map(crate::model::power_mode),
            user_switching: os_actions::user_switching_supported(),
            ..Power::default()
        }
    }

    fn user_switching(&self) -> bool {
        os_actions::user_switching_supported()
    }

    fn update(&self) -> Update {
        let deployments = crate::update::deployments()
            .inspect_err(|error| tracing::debug!(%error, "the deployments are not listed"))
            .ok();
        let staged = deployments.is_none() && crate::update::staged_marked();
        Update {
            firmware: os_actions::get_firmware_updates()
                .ok()
                .map(|offered| offered.len()),
            fell_back: crate::update::fallbacks(),
            ..crate::update::reading(
                deployments.as_ref(),
                staged,
                crate::update::live_state().as_ref(),
            )
        }
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

    fn keyboard_light(&self) -> KeyboardLight {
        os_actions::get_keyboard_backlight()
            .map_or_else(|_| KeyboardLight::default(), crate::model::keyboard_light)
    }

    fn change(&self, change: &Change) -> Result<(), String> {
        use os_actions::{SessionAction, perform_session_action};
        match change {
            Change::PowerMode(mode) => {
                os_actions::set_power_profile(crate::model::power_profile(*mode))
            }
            Change::Lock => perform_session_action(SessionAction::Lock),
            Change::SwitchUser => perform_session_action(SessionAction::SwitchUser),
            Change::Suspend => perform_session_action(SessionAction::Suspend),
            Change::Restart => perform_session_action(SessionAction::Restart),
            Change::PowerOff => perform_session_action(SessionAction::ShutDown),
            Change::LogOut => perform_session_action(SessionAction::Logout),
            Change::UpdateNow => crate::update::update_now(),
            Change::RestartDesktop => crate::update::restart_desktop(),
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
            Change::KeyboardLight(level) => os_actions::set_keyboard_backlight(*level),
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
