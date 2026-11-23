//! The machine under the service. The session's is [`OsSystem`]
//! (power-profiles-daemon, logind, timedated, the update units, the
//! Location portal, the keyboard's LED); a test lends one of its own.

use settings_store::day_schedule::Location;
use tilo_system_resources::codec_pack::CodecPack;
use tilo_system_resources::disk_unlock::DiskUnlock;
use tilo_system_resources::firmware::Firmware;
use tilo_system_resources::drives::Drives;
use tilo_system_resources::host_info::HostInfo;
use tilo_system_resources::storage::Storage;
use tilo_system_resources::printers::{
    DiscoveredPrinter, JobState as PrintJobState, PrintJob as PrintJobReading, PrinterQueue,
    PrinterSupply, Printers, QueueState as PrintQueueState,
};
use tilo_system_resources::users::Users;
use tilo_system_resources::windows_compat::WindowsCompat;

use crate::model::{KeyboardLight, Power, PowerMode, Time};
use crate::update::Update;

/// A change to make on the printers: what a person, an agent or a script asked of the
/// `printers` resource.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrintChange {
    /// Print to the printer found at `uri`, known here by `name`.
    Add { uri: String, name: String },
    Remove(String),
    SetDefault(String),
    /// Print to the queue on the paper size `media`.
    SetMedia(String, String),
    /// Hold the queue's work, or let it print again.
    SetPaused(String, bool),
    CancelJob(String),
    RetryJob(String),
}

/// A change to make on the drives: what a person, an agent or a script asked of the `drives`
/// resource.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DriveChange {
    /// Open the drive, in the mode chosen, remembering the choice when asked to.
    Mount {
        drive: String,
        safely: bool,
        remember: bool,
    },
    Unmount { drive: String },
    Forget { grant: String },
    SetMode { grant: String, safely: bool },
}

/// A change to make on the media codecs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodecChange {
    Install,
    Remove,
    Refresh,
}

/// A change to make on the Windows compatibility layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowsChange {
    Install,
    Remove,
}

/// A change to make on the firmware.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FirmwareChange {
    Refresh,
    Install,
}

/// A change to make on the disk unlock: what a person, an agent or a script asked of the
/// `disk-unlock` resource.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiskUnlockChange {
    /// Choose the whole new list of Wi-Fi connections the volume may use at boot.
    SetNetworks {
        volume: String,
        networks: Vec<String>,
    },
}

/// A change to make on the storage: what a person, an agent or a script asked of the `storage`
/// resource.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageChange {
    /// Remove the apps' caches.
    ClearCache,
}

/// A change to make on the accounts: what a person, an agent or a script asked of the `users`
/// resource.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UserChange {
    Add {
        username: String,
        full_name: String,
        password: String,
    },
    SetAdmin { user: String, admin: bool },
    SetAutomaticLogin { user: String, on: bool },
    /// Remove the account, keeping its home folder when `keep_files`.
    Remove { user: String, keep_files: bool },
}

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
    /// Boot the previous deployment from now on, and restart into it.
    Rollback,
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
    /// Whether the live power service is attached to the system bus and has
    /// already received a reading from `power-profiles-daemon`. Until this is
    /// `true`, [`crate::SystemService`] polls [`System::power`].
    fn is_ready(&self) -> bool;

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

    /// The active power mode read once directly from the daemon. Called on the
    /// live path right after [`Change::PowerMode`] lands so the first reading
    /// where `pending` clears already carries the new mode.
    fn power_mode(&self) -> Option<PowerMode> {
        self.power().mode
    }

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

    /// The queues this computer prints to, the jobs they carry, and the printers found
    /// nearby. A machine with no printer service answers with `serving: false`.
    fn printers(&self) -> Printers {
        Printers::default()
    }

    /// The document scanners this machine reaches, and whether a scanner service answers.
    fn scanners(&self) -> tilo_system_resources::scanners::Scanners {
        tilo_system_resources::scanners::Scanners::default()
    }

    /// The printers found on the network now.
    ///
    /// # Errors
    /// Why they could not be looked for.
    fn find_printers(&self) -> Result<Vec<DiscoveredPrinter>, String> {
        Err("this machine cannot look for printers".to_owned())
    }

    /// Make `change` on the printers.
    ///
    /// # Errors
    /// Why it did not happen.
    fn change_print(&self, _change: &PrintChange) -> Result<(), String> {
        Err("this machine cannot print".to_owned())
    }

    /// Call `changed` whenever the printers change: a queue's state, a job, the default.
    /// The handle keeps the subscription alive while it is held.
    fn follow_printers(&self, _changed: Box<dyn Fn() + Send + Sync>) -> Option<Box<dyn Send>> {
        None
    }

    /// The local login accounts this computer keeps. A machine that cannot read them answers
    /// with `serving: false`.
    fn users(&self) -> Users {
        Users::default()
    }

    /// Make `change` on the accounts.
    ///
    /// # Errors
    /// Why it did not happen.
    fn change_user(&self, _change: &UserChange) -> Result<(), String> {
        Err("this machine cannot change its accounts".to_owned())
    }

    /// The NTFS volumes this computer sees and the drives the person chose to remember. A
    /// machine with no drive service answers with `serving: false`.
    fn drives(&self) -> Drives {
        Drives::default()
    }

    /// Make `change` on the drives.
    ///
    /// # Errors
    /// Why it did not happen.
    fn change_drive(&self, _change: &DriveChange) -> Result<(), String> {
        Err("this machine has no drive service".to_owned())
    }

    /// What this computer's disk holds, as the last scan read it.
    fn storage(&self) -> Storage {
        Storage::default()
    }

    /// Read what this computer's disk holds now.
    fn scan_storage(&self) -> Storage {
        Storage::default()
    }

    /// Make `change` on the storage.
    ///
    /// # Errors
    /// Why it did not happen.
    fn change_storage(&self, _change: &StorageChange) -> Result<(), String> {
        Err("this machine cannot scan its storage".to_owned())
    }

    /// What this device is: its system image, kernel, hardware and uptime.
    fn host_info(&self) -> HostInfo {
        HostInfo::default()
    }

    /// The encrypted volumes enrolled for sibling unlock, as the last read found them.
    fn disk_unlock(&self) -> DiskUnlock {
        DiskUnlock::default()
    }

    /// Read the enrolled volumes now.
    fn scan_disk_unlock(&self) -> DiskUnlock {
        DiskUnlock::default()
    }

    /// Make `change` on the enrolment.
    ///
    /// # Errors
    /// Why it did not happen.
    fn change_disk_unlock(&self, _change: &DiskUnlockChange) -> Result<(), String> {
        Err("this machine has no unlocking to change".to_owned())
    }

    /// The media codecs this computer offers.
    fn codec_pack(&self) -> CodecPack {
        CodecPack::default()
    }

    /// Make `change` on the codecs.
    ///
    /// # Errors
    /// Why it did not happen.
    fn change_codec(&self, _change: &CodecChange) -> Result<(), String> {
        Err("this machine has no media codecs to install".to_owned())
    }

    /// The Windows compatibility layer this computer offers.
    fn windows_compat(&self) -> WindowsCompat {
        WindowsCompat::default()
    }

    /// Make `change` on the Windows compatibility layer.
    ///
    /// # Errors
    /// Why it did not happen.
    fn change_windows(&self, _change: &WindowsChange) -> Result<(), String> {
        Err("this machine cannot install Windows support".to_owned())
    }

    /// The firmware updates fwupd offers and what it installed before.
    fn firmware(&self) -> Firmware {
        Firmware::default()
    }

    /// Make `change` on the firmware.
    ///
    /// # Errors
    /// Why it did not happen.
    fn change_firmware(&self, _change: &FirmwareChange) -> Result<(), String> {
        Err("this machine has no firmware to update".to_owned())
    }
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

    fn power_mode(&self) -> Option<PowerMode> {
        os_actions::get_power_profile()
            .ok()
            .map(crate::model::power_mode)
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
            Change::Rollback => {
                crate::update::rollback()?;
                perform_session_action(SessionAction::Restart)
            }
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

    fn printers(&self) -> Printers {
        let queues = os_actions::printing::queues();
        let jobs = os_actions::printing::jobs();
        let serving = queues.is_ok();
        Printers {
            serving,
            queues: queues.map(queues_reading).unwrap_or_default(),
            jobs: jobs.map(jobs_reading).unwrap_or_default(),
            ..Printers::default()
        }
    }

    fn scanners(&self) -> tilo_system_resources::scanners::Scanners {
        crate::scanners::reading()
    }

    fn find_printers(&self) -> Result<Vec<DiscoveredPrinter>, String> {
        os_actions::printing::discover_driverless().map(|found| {
            found
                .into_iter()
                .map(|printer| DiscoveredPrinter {
                    uri: printer.uri,
                    name: printer.name,
                    location: printer.location,
                })
                .collect()
        })
    }

    fn change_print(&self, change: &PrintChange) -> Result<(), String> {
        match change {
            PrintChange::Add { uri, name } => os_actions::printing::add_driverless(uri, name),
            PrintChange::Remove(printer) => os_actions::printing::remove_queue(printer),
            PrintChange::SetDefault(printer) => os_actions::printing::set_default(printer),
            PrintChange::SetMedia(printer, media) => {
                os_actions::printing::set_media(printer, media)
            }
            PrintChange::SetPaused(printer, paused) => {
                os_actions::printing::set_enabled(printer, !paused)
            }
            PrintChange::CancelJob(job) => os_actions::printing::cancel_job(job),
            PrintChange::RetryJob(job) => os_actions::printing::retry_job(job),
        }
    }

    fn follow_printers(&self, changed: Box<dyn Fn() + Send + Sync>) -> Option<Box<dyn Send>> {
        os_actions::printing::follow_changes(move || changed())
            .ok()
            .map(|follow| Box::new(follow) as Box<dyn Send>)
    }

    fn users(&self) -> Users {
        match crate::users::accounts() {
            Ok(accounts) => Users {
                serving: true,
                accounts,
                ..Users::default()
            },
            Err(error) => Users {
                read_error: Some(error),
                ..Users::default()
            },
        }
    }

    fn change_user(&self, change: &UserChange) -> Result<(), String> {
        match change {
            UserChange::Add {
                username,
                full_name,
                password,
            } => {
                os_actions::users::create_user(
                    username,
                    full_name,
                    os_actions::AccountRole::Standard,
                )?;
                let mut password = password.as_bytes().to_vec();
                let outcome = os_actions::users::set_password(username, &mut password);
                password.fill(0);
                outcome
            }
            UserChange::SetAdmin { user, admin } => os_actions::users::set_role(
                user,
                if *admin {
                    os_actions::AccountRole::Administrator
                } else {
                    os_actions::AccountRole::Standard
                },
            ),
            UserChange::SetAutomaticLogin { user, on } => {
                os_actions::users::set_automatic_login(user, *on)
            }
            UserChange::Remove { user, keep_files } => {
                let uid = crate::users::uid(user)?;
                os_actions::users::delete_user(user, uid, !keep_files)
            }
        }
    }

    fn drives(&self) -> Drives {
        crate::drives::reading()
    }

    fn change_drive(&self, change: &DriveChange) -> Result<(), String> {
        crate::drives::change(change)
    }

    fn scan_storage(&self) -> Storage {
        crate::storage::scan()
    }

    fn change_storage(&self, change: &StorageChange) -> Result<(), String> {
        match change {
            StorageChange::ClearCache => crate::storage::clear_cache(),
        }
    }

    fn host_info(&self) -> HostInfo {
        crate::host_info::facts()
    }

    fn scan_disk_unlock(&self) -> DiskUnlock {
        crate::disk_unlock::scan()
    }

    fn change_disk_unlock(&self, change: &DiskUnlockChange) -> Result<(), String> {
        match change {
            DiskUnlockChange::SetNetworks { volume, networks } => {
                crate::disk_unlock::set_networks(volume, networks)
            }
        }
    }

    fn codec_pack(&self) -> CodecPack {
        crate::codec_pack::reading()
    }

    fn change_codec(&self, change: &CodecChange) -> Result<(), String> {
        crate::codec_pack::change(change)
    }

    fn windows_compat(&self) -> WindowsCompat {
        crate::windows_compat::reading()
    }

    fn change_windows(&self, change: &WindowsChange) -> Result<(), String> {
        crate::windows_compat::change(change)
    }

    fn firmware(&self) -> Firmware {
        crate::firmware::reading()
    }

    fn change_firmware(&self, change: &FirmwareChange) -> Result<(), String> {
        match change {
            FirmwareChange::Refresh => crate::firmware::refresh(),
            FirmwareChange::Install => crate::firmware::install(),
        }
    }
}

/// The queues CUPS lists, as the `printers` resource reads them.
fn queues_reading(queues: Vec<os_actions::printing::PrinterQueue>) -> Vec<PrinterQueue> {
    use os_actions::printing::QueueState;
    queues
        .into_iter()
        .map(|queue| {
            let options = os_actions::printing::printer_options(&queue.name).ok();
            let (media, default_media) = options.as_ref().map_or_else(
                || (Vec::new(), None),
                |options| {
                    (
                        options
                            .page_sizes
                            .iter()
                            .map(|choice| choice.key.clone())
                            .collect(),
                        os_actions::printing::PrinterOptions::default_key(&options.page_sizes)
                            .map(str::to_owned),
                    )
                },
            );
            PrinterQueue {
                name: queue.name,
                state: match queue.state {
                    QueueState::Ready => PrintQueueState::Ready,
                    QueueState::Printing => PrintQueueState::Printing,
                    QueueState::Paused => PrintQueueState::Paused,
                    QueueState::Offline => PrintQueueState::Offline,
                },
                default: queue.default,
                accepting: queue.accepting_jobs,
                reason: queue.reason.or_else(|| {
                    let reasons = queue.telemetry.state_reasons;
                    (!reasons.is_empty()).then(|| reasons.join(", "))
                }),
                supplies: queue
                    .telemetry
                    .supplies
                    .unwrap_or_default()
                    .into_iter()
                    .map(|supply| PrinterSupply {
                        name: supply.name,
                        color: supply.color,
                        level: supply.level_percent,
                    })
                    .collect(),
                reachable: queue.telemetry.available,
                media,
                default_media,
            }
        })
        .collect()
}

/// The jobs CUPS lists, as the `printers` resource reads them.
fn jobs_reading(jobs: Vec<os_actions::printing::PrintJob>) -> Vec<PrintJobReading> {
    use os_actions::printing::JobState;
    jobs.into_iter()
        .map(|job| {
            let progress = job.progress_percent();
            PrintJobReading {
                id: job.id,
                title: job.title.unwrap_or_default(),
                owner: job.owner,
                size_bytes: job.size_bytes,
                submitted: job.submitted,
                destination: job.destination,
                state: match job.state {
                    Some(JobState::Held) => PrintJobState::Held,
                    Some(JobState::Stopped) => PrintJobState::Stopped,
                    Some(JobState::Processing) => PrintJobState::Printing,
                    Some(JobState::Canceled) => PrintJobState::Cancelled,
                    Some(JobState::Aborted) => PrintJobState::Failed,
                    Some(JobState::Completed) => PrintJobState::Printed,
                    Some(JobState::Pending | JobState::Unknown) | None => PrintJobState::Waiting,
                },
                progress,
            }
        })
        .collect()
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
