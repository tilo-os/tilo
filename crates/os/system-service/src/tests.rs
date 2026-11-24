use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value, json};
use settings_store::day_schedule::Location;
use tilo_actions::ApiError;
use tilo_service::{Service, Services, Started, Wake};

use tilo_system_resources::codec_pack::{CodecPack, CodecState, HardwareVideo};
use tilo_system_resources::disk_unlock::{DiskUnlock, Sealing, UnlockVolume};
use tilo_system_resources::drives::{
    DriveMode, DriveVolume, Drives, Placement, RememberOffer, RememberedDrive,
};
use tilo_system_resources::firmware::{Firmware, FirmwareUpdate};
use tilo_system_resources::host_info::HostInfo;
use tilo_system_resources::printers::{
    DiscoveredPrinter, JobState, PrintJob, PrinterQueue, Printers, QueueState,
};
use tilo_system_resources::storage::{CategoryUsage, DiskUsage, Storage, StoredItem};
use tilo_system_resources::update::Deployment;
use tilo_system_resources::users::{LocalAccount, Users};
use tilo_system_resources::windows_compat::{WindowsCompat, WindowsState};

use crate::device::{
    Change, CodecChange, DiskUnlockChange, DriveChange, FirmwareChange, PowerProfiles, PrintChange,
    StorageChange, System, UserChange, WindowsChange,
};
use crate::model::{KeyboardLight, Power, PowerMode, Time};
use crate::update::Update;
use crate::{
    CODEC_PACK, DISK_UNLOCK, DRIVES, FIRMWARE, HOST_INFO, KEYBOARD_BACKLIGHT, POWER, PRINTERS,
    SCANNERS, STORAGE, SYSTEM_UPDATE, SystemService, TIME_ZONE, USERS, WINDOWS_COMPAT,
};

/// A machine in memory, in Amsterdam unless told it is elsewhere.
struct Fake {
    power: Mutex<Power>,
    power_reads: AtomicUsize,
    power_refusal: Mutex<Option<String>>,
    on_power_mode: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    update: Mutex<Update>,
    time: Mutex<Time>,
    keyboard_light: Mutex<KeyboardLight>,
    location: Mutex<Result<Location, String>>,
    changes: Mutex<Vec<Change>>,
    printers: Mutex<Printers>,
    nearby: Mutex<Result<Vec<DiscoveredPrinter>, String>>,
    print_changes: Mutex<Vec<PrintChange>>,
    print_refusal: Mutex<Option<String>>,
    users: Mutex<Users>,
    user_changes: Mutex<Vec<UserChange>>,
    user_refusal: Mutex<Option<String>>,
    rollback_refusal: Mutex<Option<String>>,
    drives: Mutex<Drives>,
    drive_changes: Mutex<Vec<DriveChange>>,
    drive_refusal: Mutex<Option<String>>,
    storage: Mutex<Storage>,
    storage_changes: Mutex<Vec<StorageChange>>,
    storage_refusal: Mutex<Option<String>>,
    disk_unlock: Mutex<DiskUnlock>,
    disk_unlock_changes: Mutex<Vec<DiskUnlockChange>>,
    disk_unlock_refusal: Mutex<Option<String>>,
    host_info: Mutex<HostInfo>,
    codec: Mutex<CodecPack>,
    codec_changes: Mutex<Vec<CodecChange>>,
    codec_refusal: Mutex<Option<String>>,
    windows: Mutex<WindowsCompat>,
    windows_changes: Mutex<Vec<WindowsChange>>,
    windows_refusal: Mutex<Option<String>>,
    firmware: Mutex<Firmware>,
    firmware_changes: Mutex<Vec<FirmwareChange>>,
    firmware_refusal: Mutex<Option<String>>,
}

impl Fake {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            power: Mutex::new(Power {
                modes: vec![PowerMode::Saver, PowerMode::Balanced],
                mode: Some(PowerMode::Balanced),
                ..Power::default()
            }),
            power_reads: AtomicUsize::new(0),
            power_refusal: Mutex::new(None),
            on_power_mode: Mutex::new(None),
            update: Mutex::new(Update::default()),
            time: Mutex::new(Time {
                zone: Some("Europe/Amsterdam".into()),
                network_time: Some(true),
                ..Time::default()
            }),
            keyboard_light: Mutex::new(KeyboardLight {
                level: Some(1),
                max: 3,
                ..KeyboardLight::default()
            }),
            location: Mutex::new(Err("no fix".into())),
            changes: Mutex::new(Vec::new()),
            printers: Mutex::new(Printers {
                serving: true,
                queues: vec![PrinterQueue {
                    name: "Office".into(),
                    state: QueueState::Ready,
                    default: true,
                    accepting: true,
                    reason: None,
                    supplies: Vec::new(),
                    reachable: true,
                    media: vec!["A4".into(), "Letter".into()],
                    default_media: Some("A4".into()),
                }],
                jobs: vec![PrintJob {
                    id: "12".into(),
                    title: "Report".into(),
                    owner: "ada".into(),
                    size_bytes: 2048,
                    submitted: "2026-11-23 09:00".into(),
                    destination: "Office".into(),
                    state: JobState::Waiting,
                    progress: None,
                }],
                ..Printers::default()
            }),
            nearby: Mutex::new(Ok(Vec::new())),
            print_changes: Mutex::new(Vec::new()),
            print_refusal: Mutex::new(None),
            users: Mutex::new(Users {
                serving: true,
                accounts: vec![LocalAccount {
                    username: "ada".into(),
                    full_name: "Ada Lovelace".into(),
                    uid: 1000,
                    admin: true,
                    current: true,
                    automatic_login: false,
                    automatic_login_error: None,
                }],
                ..Users::default()
            }),
            user_changes: Mutex::new(Vec::new()),
            user_refusal: Mutex::new(None),
            rollback_refusal: Mutex::new(None),
            drives: Mutex::new(Drives::default()),
            drive_changes: Mutex::new(Vec::new()),
            drive_refusal: Mutex::new(None),
            storage: Mutex::new(Storage::default()),
            storage_changes: Mutex::new(Vec::new()),
            storage_refusal: Mutex::new(None),
            disk_unlock: Mutex::new(DiskUnlock::default()),
            disk_unlock_changes: Mutex::new(Vec::new()),
            disk_unlock_refusal: Mutex::new(None),
            host_info: Mutex::new(HostInfo::default()),
            codec: Mutex::new(CodecPack::default()),
            codec_changes: Mutex::new(Vec::new()),
            codec_refusal: Mutex::new(None),
            windows: Mutex::new(WindowsCompat::default()),
            windows_changes: Mutex::new(Vec::new()),
            windows_refusal: Mutex::new(None),
            firmware: Mutex::new(Firmware::default()),
            firmware_changes: Mutex::new(Vec::new()),
            firmware_refusal: Mutex::new(None),
        })
    }

    fn changes(&self) -> Vec<Change> {
        self.changes.lock().unwrap().clone()
    }

    fn print_changes(&self) -> Vec<PrintChange> {
        self.print_changes.lock().unwrap().clone()
    }

    fn user_changes(&self) -> Vec<UserChange> {
        self.user_changes.lock().unwrap().clone()
    }

    fn drive_changes(&self) -> Vec<DriveChange> {
        self.drive_changes.lock().unwrap().clone()
    }

    fn storage_changes(&self) -> Vec<StorageChange> {
        self.storage_changes.lock().unwrap().clone()
    }

    fn disk_unlock_changes(&self) -> Vec<DiskUnlockChange> {
        self.disk_unlock_changes.lock().unwrap().clone()
    }

    fn power_reads(&self) -> usize {
        self.power_reads.load(Ordering::SeqCst)
    }
}

impl System for Fake {
    fn power(&self) -> Power {
        self.power_reads.fetch_add(1, Ordering::SeqCst);
        self.power.lock().unwrap().clone()
    }

    fn power_mode(&self) -> Option<PowerMode> {
        if let Some(hook) = self.on_power_mode.lock().unwrap().as_ref() {
            hook();
        }
        self.power.lock().unwrap().mode
    }

    fn user_switching(&self) -> bool {
        self.power.lock().unwrap().user_switching
    }

    fn update(&self) -> Update {
        self.update.lock().unwrap().clone()
    }

    fn time(&self) -> Time {
        self.time.lock().unwrap().clone()
    }

    fn keyboard_light(&self) -> KeyboardLight {
        self.keyboard_light.lock().unwrap().clone()
    }

    fn change(&self, change: &Change) -> Result<(), String> {
        self.changes.lock().unwrap().push(change.clone());
        match change {
            Change::PowerMode(mode) => {
                let refusal = self.power_refusal.lock().unwrap().clone();
                if let Some(reason) = refusal {
                    return Err(reason);
                }
                self.power.lock().unwrap().mode = Some(*mode);
            }
            Change::Zone(zone) => {
                let mut time = self.time.lock().unwrap();
                time.zone = Some(zone.clone());
                time.automatic = false;
            }
            Change::FollowedZone(zone) => self.time.lock().unwrap().zone = Some(zone.clone()),
            Change::AutomaticZone(on) => self.time.lock().unwrap().automatic = *on,
            Change::UpdateNow => return Err("nothing newer to fetch".into()),
            Change::Rollback => {
                let refusal = self.rollback_refusal.lock().unwrap().clone();
                if let Some(reason) = refusal {
                    return Err(reason);
                }
            }
            Change::KeyboardLight(level) => {
                self.keyboard_light.lock().unwrap().level = Some(*level);
            }
            _ => {}
        }
        Ok(())
    }

    fn locate(&self) -> Result<Location, String> {
        self.location.lock().unwrap().clone()
    }

    fn nearest_zone(&self, location: Location) -> Option<String> {
        settings_store::clock::nearest_zone_in(
            location,
            "NL\t+5222+00454\tEurope/Amsterdam\nJP\t+353916+1394441\tAsia/Tokyo\n",
        )
    }

    fn printers(&self) -> Printers {
        self.printers.lock().unwrap().clone()
    }

    fn scanners(&self) -> tilo_system_resources::scanners::Scanners {
        tilo_system_resources::scanners::Scanners {
            available: true,
            found: vec![tilo_system_resources::scanners::ScannerShown {
                device: "epson2:libusb:001:004".into(),
                description: "Epson Perfection V39".into(),
            }],
            ..tilo_system_resources::scanners::Scanners::default()
        }
    }

    fn find_printers(&self) -> Result<Vec<DiscoveredPrinter>, String> {
        self.nearby.lock().unwrap().clone()
    }

    fn change_print(&self, change: &PrintChange) -> Result<(), String> {
        self.print_changes.lock().unwrap().push(change.clone());
        let refusal = self.print_refusal.lock().unwrap().clone();
        if let Some(reason) = refusal {
            return Err(reason);
        }
        let mut printers = self.printers.lock().unwrap();
        match change {
            PrintChange::CancelJob(job) => {
                if let Some(found) = printers.jobs.iter_mut().find(|j| j.id == *job) {
                    found.state = JobState::Cancelled;
                }
            }
            PrintChange::RetryJob(job) => {
                if let Some(found) = printers.jobs.iter_mut().find(|j| j.id == *job) {
                    found.state = JobState::Waiting;
                }
            }
            PrintChange::SetPaused(printer, paused) => {
                if let Some(found) = printers.queues.iter_mut().find(|q| q.name == *printer) {
                    found.state = if *paused {
                        QueueState::Paused
                    } else {
                        QueueState::Ready
                    };
                    found.accepting = !paused;
                }
            }
            PrintChange::SetDefault(printer) => {
                for queue in &mut printers.queues {
                    queue.default = queue.name == *printer;
                }
            }
            PrintChange::SetMedia(printer, media) => {
                if let Some(found) = printers.queues.iter_mut().find(|q| q.name == *printer) {
                    found.default_media = Some(media.clone());
                }
            }
            PrintChange::Remove(printer) => printers.queues.retain(|q| q.name != *printer),
            PrintChange::Add { uri: _, name } => {
                let empty = printers.queues.is_empty();
                printers.queues.push(PrinterQueue {
                    name: name.clone(),
                    state: QueueState::Ready,
                    default: empty,
                    accepting: true,
                    reason: None,
                    supplies: Vec::new(),
                    reachable: true,
                    media: Vec::new(),
                    default_media: None,
                });
            }
        }
        drop(printers);
        Ok(())
    }

    fn users(&self) -> Users {
        self.users.lock().unwrap().clone()
    }

    fn change_user(&self, change: &UserChange) -> Result<(), String> {
        self.user_changes.lock().unwrap().push(change.clone());
        let refusal = self.user_refusal.lock().unwrap().clone();
        if let Some(reason) = refusal {
            return Err(reason);
        }
        let mut users = self.users.lock().unwrap();
        match change {
            UserChange::Add {
                username,
                full_name,
                ..
            } => users.accounts.push(LocalAccount {
                username: username.clone(),
                full_name: full_name.clone(),
                uid: 1001,
                admin: false,
                current: false,
                automatic_login: false,
                automatic_login_error: None,
            }),
            UserChange::SetAdmin { user, admin } => {
                if let Some(found) = users.accounts.iter_mut().find(|a| a.username == *user) {
                    found.admin = *admin;
                }
            }
            UserChange::SetAutomaticLogin { user, on } => {
                if let Some(found) = users.accounts.iter_mut().find(|a| a.username == *user) {
                    found.automatic_login = *on;
                }
            }
            UserChange::Remove { user, .. } => {
                users.accounts.retain(|account| account.username != *user);
            }
        }
        drop(users);
        Ok(())
    }

    fn drives(&self) -> Drives {
        self.drives.lock().unwrap().clone()
    }

    fn change_drive(&self, change: &DriveChange) -> Result<(), String> {
        self.drive_changes.lock().unwrap().push(change.clone());
        let refusal = self.drive_refusal.lock().unwrap().clone();
        if let Some(reason) = refusal {
            return Err(reason);
        }
        let mut drives = self.drives.lock().unwrap();
        match change {
            DriveChange::Mount { drive, .. } => {
                if let Some(found) = drives.drives.iter_mut().find(|d| d.id == *drive) {
                    found.mounted = Some(tilo_system_resources::drives::Mounted {
                        path: "/run/media/ada/Windows".into(),
                        mode: DriveMode::OpenSafely,
                        read_only: None,
                    });
                }
            }
            DriveChange::Unmount { drive } => {
                if let Some(found) = drives.drives.iter_mut().find(|d| d.id == *drive) {
                    found.mounted = None;
                }
            }
            DriveChange::Forget { grant } => {
                drives.remembered.retain(|d| d.grant != *grant);
            }
            DriveChange::SetMode { grant, safely } => {
                if let Some(found) = drives.remembered.iter_mut().find(|d| d.grant == *grant) {
                    found.mode = if *safely {
                        DriveMode::OpenSafely
                    } else {
                        DriveMode::Open
                    };
                }
            }
        }
        Ok(())
    }

    fn storage(&self) -> Storage {
        self.storage.lock().unwrap().clone()
    }

    fn scan_storage(&self) -> Storage {
        self.storage.lock().unwrap().clone()
    }

    fn change_storage(&self, change: &StorageChange) -> Result<(), String> {
        self.storage_changes.lock().unwrap().push(*change);
        let refusal = self.storage_refusal.lock().unwrap().clone();
        if let Some(reason) = refusal {
            return Err(reason);
        }
        match change {
            StorageChange::ClearCache => self.storage.lock().unwrap().cache_bytes = 0,
        }
        Ok(())
    }

    fn host_info(&self) -> HostInfo {
        self.host_info.lock().unwrap().clone()
    }

    fn codec_pack(&self) -> CodecPack {
        self.codec.lock().unwrap().clone()
    }

    fn change_codec(&self, change: &CodecChange) -> Result<(), String> {
        self.codec_changes.lock().unwrap().push(*change);
        let refusal = self.codec_refusal.lock().unwrap().clone();
        if let Some(reason) = refusal {
            return Err(reason);
        }
        let mut codec = self.codec.lock().unwrap();
        match change {
            CodecChange::Install => codec.state = CodecState::Installed,
            CodecChange::Remove => codec.state = CodecState::NotInstalled,
            CodecChange::Refresh => {}
        }
        drop(codec);
        Ok(())
    }

    fn windows_compat(&self) -> WindowsCompat {
        self.windows.lock().unwrap().clone()
    }

    fn change_windows(&self, change: &WindowsChange) -> Result<(), String> {
        self.windows_changes.lock().unwrap().push(*change);
        let refusal = self.windows_refusal.lock().unwrap().clone();
        if let Some(reason) = refusal {
            return Err(reason);
        }
        let mut windows = self.windows.lock().unwrap();
        match change {
            WindowsChange::Install => windows.state = WindowsState::Ready,
            WindowsChange::Remove => windows.state = WindowsState::NeedsInstall,
        }
        drop(windows);
        Ok(())
    }

    fn firmware(&self) -> Firmware {
        self.firmware.lock().unwrap().clone()
    }

    fn change_firmware(&self, change: &FirmwareChange) -> Result<(), String> {
        self.firmware_changes.lock().unwrap().push(*change);
        let refusal = self.firmware_refusal.lock().unwrap().clone();
        if let Some(reason) = refusal {
            return Err(reason);
        }
        if matches!(change, FirmwareChange::Install) {
            self.firmware.lock().unwrap().updates.clear();
        }
        Ok(())
    }

    fn disk_unlock(&self) -> DiskUnlock {
        self.disk_unlock.lock().unwrap().clone()
    }

    fn scan_disk_unlock(&self) -> DiskUnlock {
        self.disk_unlock.lock().unwrap().clone()
    }

    fn change_disk_unlock(&self, change: &DiskUnlockChange) -> Result<(), String> {
        self.disk_unlock_changes
            .lock()
            .unwrap()
            .push(change.clone());
        let refusal = self.disk_unlock_refusal.lock().unwrap().clone();
        if let Some(reason) = refusal {
            return Err(reason);
        }
        let mut disk = self.disk_unlock.lock().unwrap();
        match change {
            DiskUnlockChange::SetNetworks { volume, networks } => {
                if let Some(found) = disk.volumes.iter_mut().find(|v| v.volume == *volume) {
                    found.networks = networks.clone();
                }
            }
        }
        Ok(())
    }
}

fn service(fake: &Arc<Fake>) -> SystemService {
    let system: Arc<dyn System> = Arc::clone(fake) as Arc<dyn System>;
    let mut service = SystemService::new(system, None).unwrap();
    service.settle();
    service
}

fn arguments(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

#[test]
fn the_zone_follows_the_device_only_while_chosen_and_never_guesses() {
    let fake = Fake::new();
    let mut service = service(&fake);
    *fake.location.lock().unwrap() = Ok(Location {
        latitude: 34.69,
        longitude: 135.5,
    });
    service
        .invoke(
            TIME_ZONE,
            "set-automatic-zone",
            &arguments(json!({"on": true})),
        )
        .unwrap();
    service.settle();
    let time = service.read(TIME_ZONE).unwrap();
    assert_eq!(time["zone"], json!("Asia/Tokyo"));
    assert_eq!(time["automatic"], json!(true));

    // A zone the person picks stops the following.
    service
        .invoke(
            TIME_ZONE,
            "set-zone",
            &arguments(json!({"zone": "Europe/Amsterdam"})),
        )
        .unwrap();
    service.settle();
    let time = service.read(TIME_ZONE).unwrap();
    assert_eq!(time["zone"], json!("Europe/Amsterdam"));
    assert_eq!(time["automatic"], json!(false));
    assert!(matches!(
        service.invoke(
            TIME_ZONE,
            "set-zone",
            &arguments(json!({"zone": "../etc/passwd"}))
        ),
        Err(ApiError::InvalidArguments(_))
    ));

    // No location: the zone stays as it is.
    *fake.location.lock().unwrap() = Err("no fix".into());
    service
        .invoke(
            TIME_ZONE,
            "set-automatic-zone",
            &arguments(json!({"on": true})),
        )
        .unwrap();
    service.settle();
    assert_eq!(
        service.read(TIME_ZONE).unwrap()["zone"],
        json!("Europe/Amsterdam")
    );
}

#[test]
fn the_keyboard_keys_step_the_keyboard_light_through_its_levels() {
    let fake = Fake::new();
    let mut service = service(&fake);
    let level =
        |service: &SystemService| service.read(KEYBOARD_BACKLIGHT).unwrap()["level"].clone();
    assert_eq!(level(&service), json!(1));
    for (action, arguments_, expected) in [
        ("brighter", json!({}), 2),
        ("brighter", json!({}), 3),
        ("brighter", json!({}), 3),
        ("toggle", json!({}), 0),
        ("toggle", json!({}), 3),
        ("dimmer", json!({}), 2),
        ("set-level", json!({"level": 0.34}), 1),
    ] {
        assert_eq!(
            service.invoke(KEYBOARD_BACKLIGHT, action, &arguments(arguments_)),
            Ok(Started::Accepted),
            "{action}"
        );
        service.settle();
        assert_eq!(level(&service), json!(expected), "{action}");
    }
    assert!(matches!(
        service.invoke(
            KEYBOARD_BACKLIGHT,
            "set-level",
            &arguments(json!({"level": 2.0}))
        ),
        Err(ApiError::InvalidArguments(_))
    ));

    *fake.keyboard_light.lock().unwrap() = KeyboardLight::default();
    service.refresh();
    service.settle();
    assert!(matches!(
        service.invoke(KEYBOARD_BACKLIGHT, "brighter", &Map::new()),
        Err(ApiError::Unavailable { .. })
    ));
}

/// The power actions switch, ask for confirmation where it is a real
/// interruption, and refuse what this machine cannot do.
#[test]
fn the_power_actions_switch_ask_and_refuse() {
    let fake = Fake::new();
    let service = service(&fake);
    assert_eq!(service.read(POWER).unwrap()["mode"], json!("balanced"));
    let mut services = Services::default();
    services.add(Box::new(service));
    assert_eq!(
        services.invoke(
            POWER,
            "set-power-mode",
            &arguments(json!({"mode": "saver"}))
        ),
        Some(Ok(Started::Accepted))
    );
    services.settle();
    assert_eq!(services.read(POWER).unwrap()["mode"], json!("saver"));
    assert_eq!(
        services.invoke(
            POWER,
            "set-power-mode",
            &arguments(json!({"mode": "performance"}))
        ),
        Some(Err(ApiError::InvalidArguments(
            "this device offers no power mode performance".into()
        )))
    );
    for action in ["restart", "power-off", "log-out"] {
        assert_eq!(
            services.invoke(POWER, action, &Map::new()),
            Some(Err(ApiError::ConfirmationRequired)),
            "{action}"
        );
    }
    assert!(matches!(
        services.invoke(POWER, "suspend", &Map::new()),
        Some(Ok(_))
    ));
    assert!(matches!(
        services.invoke_confirmed(POWER, "restart", &Map::new()),
        Some(Ok(_))
    ));
    assert!(matches!(
        services.invoke(POWER, "lock", &Map::new()),
        Some(Ok(_))
    ));
    assert!(
        matches!(
            services.invoke(POWER, "switch-user", &Map::new()),
            Some(Err(ApiError::Unavailable { .. }))
        ),
        "nobody else can sign in here"
    );
    services.settle();
    assert_eq!(
        fake.changes(),
        [
            Change::PowerMode(PowerMode::Saver),
            Change::Suspend,
            Change::Restart,
            Change::Lock,
        ]
    );
}

#[test]
fn a_power_mode_refusal_shows_in_problem() {
    let fake = Fake::new();
    *fake.power_refusal.lock().unwrap() = Some("power-profiles-daemon refused the switch".into());
    let mut service = service(&fake);

    assert_eq!(
        service.invoke(
            POWER,
            "set-power-mode",
            &arguments(json!({"mode": "saver"}))
        ),
        Ok(Started::Accepted)
    );
    assert_eq!(
        service.read(POWER).unwrap()["pending"],
        json!(["set-power-mode"])
    );
    service.settle();

    let power = service.read(POWER).unwrap();
    assert_eq!(power["mode"], json!("balanced"));
    assert_eq!(power["pending"], json!([]));
    assert_eq!(
        power["problem"],
        json!("power-profiles-daemon refused the switch")
    );
    assert_eq!(fake.changes(), [Change::PowerMode(PowerMode::Saver)]);
}

#[test]
fn the_update_resource_reports_what_it_needs_and_what_it_takes() {
    let fake = Fake::new();
    let mut service = service(&fake);
    assert_eq!(
        service.read(SYSTEM_UPDATE).unwrap()["firmware"],
        Value::Null,
        "no fwupd reading yet"
    );
    *fake.update.lock().unwrap() = Update {
        firmware: Some(2),
        ..Update::default()
    };
    service.refresh();
    service.settle();
    assert_eq!(
        service.read(SYSTEM_UPDATE).unwrap()["firmware"],
        json!(2),
        "what fwupd offers is published beside the system update"
    );
    *fake.update.lock().unwrap() = Update::default();
    service.refresh();
    service.settle();
    assert!(
        matches!(
            service.invoke(SYSTEM_UPDATE, "restart-to-update", &Map::new()),
            Err(ApiError::Unavailable { .. })
        ),
        "nothing waits for a restart"
    );
    assert!(
        matches!(
            service.invoke(SYSTEM_UPDATE, "restart-desktop", &Map::new()),
            Err(ApiError::Unavailable { .. })
        ),
        "no part of the update waits for the desktop"
    );
    service
        .invoke(SYSTEM_UPDATE, "update-now", &Map::new())
        .unwrap();
    assert_eq!(
        service.read(SYSTEM_UPDATE).unwrap()["updating"],
        json!(true)
    );
    assert!(
        matches!(
            service.invoke(SYSTEM_UPDATE, "update-now", &Map::new()),
            Err(ApiError::Unavailable { .. })
        ),
        "one update at a time"
    );
    service.settle();
    let update = service.read(SYSTEM_UPDATE).unwrap();
    assert_eq!(update["updating"], json!(false));
    assert_eq!(update["problem"], json!("nothing newer to fetch"));

    // A pending desktop restart is offered only while the desktop waits.
    *fake.update.lock().unwrap() = Update {
        desktop_pending: true,
        ..Update::default()
    };
    service.refresh();
    service.settle();
    assert!(
        service
            .invoke(SYSTEM_UPDATE, "restart-desktop", &Map::new())
            .is_ok()
    );
    service.settle();
    assert_eq!(fake.changes().last(), Some(&Change::RestartDesktop));
    // Once the machine itself must restart, restarting the desktop would not
    // finish anything.
    *fake.update.lock().unwrap() = Update {
        desktop_pending: true,
        restart_needed: true,
        ..Update::default()
    };
    service.refresh();
    service.settle();
    assert!(
        matches!(
            service.invoke(SYSTEM_UPDATE, "restart-desktop", &Map::new()),
            Err(ApiError::Unavailable { .. })
        ),
        "only the machine finishing the update is offered"
    );
}

/// Wait for the service's worker to wake it, then take in what landed, until `done` holds or the
/// wait gives up.
fn until(
    service: &mut SystemService,
    woken: &std::sync::mpsc::Receiver<()>,
    done: impl Fn(&SystemService) -> bool,
) -> bool {
    let give_up = std::time::Instant::now()
        .checked_add(std::time::Duration::from_secs(20))
        .expect("a deadline within range");
    while !done(service) {
        let left = give_up.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return false;
        }
        let _ = woken.recv_timeout(left);
        service.poll();
    }
    true
}

/// An update staged and applied by the system's own timer, or by an admin in a root shell, is
/// read the moment the live update writes its state, not at the next look: the reading changes,
/// and with it whether finishing on the desktop is offered. Before the first live update the
/// state's directory does not exist yet; it is followed from its parent until it does.
#[test]
fn the_reading_follows_the_live_update_as_it_writes_its_state() {
    let fake = Fake::new();
    let (wake, woken) = std::sync::mpsc::channel();
    let wake = Mutex::new(wake);
    let system: Arc<dyn System> = Arc::clone(&fake) as Arc<dyn System>;
    let mut service = SystemService::new(
        system,
        Some(Arc::new(move || {
            let _ = wake.lock().unwrap().send(());
        })),
    )
    .unwrap();
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("live-update");
    service.follow_update_in(dir.clone());
    service.settle();
    assert!(
        matches!(
            service.invoke(SYSTEM_UPDATE, "restart-desktop", &Map::new()),
            Err(ApiError::Unavailable { .. })
        ),
        "nothing waits for the desktop yet"
    );

    // The first live update makes the directory and writes the state.
    *fake.update.lock().unwrap() = Update {
        desktop_pending: true,
        ..Update::default()
    };
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("state.json"), "{}").unwrap();
    assert!(
        until(&mut service, &woken, |service| service
            .readings()
            .update
            .desktop_pending),
        "the new state is read without anyone asking"
    );
    assert!(
        service
            .invoke(SYSTEM_UPDATE, "restart-desktop", &Map::new())
            .is_ok(),
        "and finishing on the desktop is offered"
    );
    service.settle();

    // The desktop half fails and is taken back: rewritten in place, as the live update does.
    *fake.update.lock().unwrap() = Update {
        restart_needed: true,
        failed: Some("the desktop did not come back".into()),
        ..Update::default()
    };
    let written = dir.join(".state.json.new");
    std::fs::write(&written, "{\"failed\":true}").unwrap();
    std::fs::rename(&written, dir.join("state.json")).unwrap();
    assert!(
        until(&mut service, &woken, |service| service
            .readings()
            .update
            .failed
            .is_some()),
        "the failure is read as it is written"
    );
    let reading = service.read(SYSTEM_UPDATE).unwrap();
    assert_eq!(reading["failed"], json!("the desktop did not come back"));
    assert_eq!(reading["restart-needed"], json!(true));
}

struct FakeProfiles {
    state: Mutex<(Vec<PowerMode>, Option<PowerMode>)>,
    ready: std::sync::atomic::AtomicBool,
    revision: AtomicU64,
    wakes: Mutex<Vec<Wake>>,
}

impl FakeProfiles {
    fn new(modes: Vec<PowerMode>, mode: Option<PowerMode>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new((modes, mode)),
            ready: std::sync::atomic::AtomicBool::new(true),
            revision: AtomicU64::new(1),
            wakes: Mutex::new(Vec::new()),
        })
    }

    fn unready() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new((Vec::new(), None)),
            ready: std::sync::atomic::AtomicBool::new(false),
            revision: AtomicU64::new(0),
            wakes: Mutex::new(Vec::new()),
        })
    }

    fn feed(&self, modes: Vec<PowerMode>, mode: Option<PowerMode>) {
        *self.state.lock().unwrap() = (modes, mode);
        self.ready.store(true, Ordering::Release);
        self.revision.fetch_add(1, Ordering::Release);
        self.notify();
    }

    fn set_mode(&self, mode: Option<PowerMode>) {
        self.state.lock().unwrap().1 = mode;
        self.revision.fetch_add(1, Ordering::Release);
        self.notify();
    }

    fn set_modes(&self, modes: Vec<PowerMode>) {
        self.state.lock().unwrap().0 = modes;
        self.revision.fetch_add(1, Ordering::Release);
        self.notify();
    }

    fn notify(&self) {
        let wakes = self.wakes.lock().unwrap().clone();
        for wake in wakes {
            wake();
        }
    }
}

impl PowerProfiles for FakeProfiles {
    fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    fn snapshot(&self) -> (Vec<PowerMode>, Option<PowerMode>) {
        self.state.lock().unwrap().clone()
    }

    fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    fn on_change(&self, wake: Wake) {
        self.wakes.lock().unwrap().push(wake);
    }
}

#[test]
fn with_live_power_profiles_daemon_changes_reach_power_without_polling() {
    let fake = Fake::new();
    *fake.power.lock().unwrap() = Power {
        modes: vec![PowerMode::Performance],
        mode: Some(PowerMode::Performance),
        user_switching: true,
        ..Power::default()
    };
    let live = FakeProfiles::new(
        vec![PowerMode::Saver, PowerMode::Balanced],
        Some(PowerMode::Balanced),
    );
    let (wake, woken) = std::sync::mpsc::channel();
    let wake = Mutex::new(wake);
    let system: Arc<dyn System> = Arc::clone(&fake) as Arc<dyn System>;
    let mut service = SystemService::with_power_profiles(
        system,
        Some(Arc::new(move || {
            let _ = wake.lock().unwrap().send(());
        })),
        Arc::clone(&live) as Arc<dyn PowerProfiles>,
    )
    .unwrap();
    service.settle();
    while woken.try_recv().is_ok() {}

    let reading = service.read(POWER).unwrap();
    assert_eq!(reading["modes"], json!(["saver", "balanced"]));
    assert_eq!(reading["mode"], json!("balanced"));
    assert_eq!(reading["user-switching"], json!(true));
    assert_eq!(
        fake.power_reads(),
        0,
        "live power profiles never poll System::power()"
    );

    live.set_mode(Some(PowerMode::Saver));
    woken
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("a daemon profile change wakes the service");
    assert!(service.poll(), "poll reports the live profile change");
    assert!(!service.poll(), "and only once");
    assert_eq!(service.read(POWER).unwrap()["mode"], json!("saver"));
    assert_eq!(fake.power_reads(), 0);
}

#[test]
fn with_live_power_profiles_modes_and_set_power_mode_stay_in_step() {
    let fake = Fake::new();
    let live = FakeProfiles::new(
        vec![PowerMode::Saver, PowerMode::Balanced],
        Some(PowerMode::Balanced),
    );
    let system: Arc<dyn System> = Arc::clone(&fake) as Arc<dyn System>;
    let mut service = SystemService::with_power_profiles(
        system,
        None,
        Arc::clone(&live) as Arc<dyn PowerProfiles>,
    )
    .unwrap();
    service.settle();

    assert_eq!(
        service.invoke(
            POWER,
            "set-power-mode",
            &arguments(json!({"mode": "performance"}))
        ),
        Err(ApiError::InvalidArguments(
            "this device offers no power mode performance".into()
        ))
    );

    live.set_modes(vec![PowerMode::Performance, PowerMode::Balanced]);
    assert!(service.poll());
    assert_eq!(
        service.read(POWER).unwrap()["modes"],
        json!(["performance", "balanced"])
    );
    assert_eq!(
        service.invoke(
            POWER,
            "set-power-mode",
            &arguments(json!({"mode": "performance"}))
        ),
        Ok(Started::Accepted)
    );
    service.settle();
    assert_eq!(fake.changes(), [Change::PowerMode(PowerMode::Performance)]);
    assert_eq!(
        service.invoke(
            POWER,
            "set-power-mode",
            &arguments(json!({"mode": "saver"}))
        ),
        Err(ApiError::InvalidArguments(
            "this device offers no power mode saver".into()
        ))
    );

    live.set_modes(Vec::new());
    assert!(service.poll());
    assert_eq!(service.read(POWER).unwrap()["modes"], json!([]));
    assert_eq!(
        service.invoke(
            POWER,
            "set-power-mode",
            &arguments(json!({"mode": "balanced"}))
        ),
        Err(ApiError::Unavailable {
            code: "no_power_daemon".into(),
            reason: "This device offers no power modes".into(),
        })
    );
    assert_eq!(fake.power_reads(), 0);
}

#[test]
fn without_live_power_profiles_system_service_polls_and_runs_set_power_mode() {
    let fake = Fake::new();
    let mut service = service(&fake);
    assert!(fake.power_reads() >= 1, "without a power link it polls");
    assert_eq!(
        service.read(POWER).unwrap()["modes"],
        json!(["saver", "balanced"])
    );
    assert_eq!(service.read(POWER).unwrap()["mode"], json!("balanced"));
    assert_eq!(
        service.invoke(
            POWER,
            "set-power-mode",
            &arguments(json!({"mode": "saver"}))
        ),
        Ok(Started::Accepted)
    );
    service.settle();
    assert_eq!(service.read(POWER).unwrap()["mode"], json!("saver"));
    assert_eq!(fake.changes(), [Change::PowerMode(PowerMode::Saver)]);
}

#[test]
fn unready_live_source_polls_until_fed_then_hands_off_without_gap_or_double_publish() {
    let fake = Fake::new();
    let live = FakeProfiles::unready();
    let system: Arc<dyn System> = Arc::clone(&fake) as Arc<dyn System>;
    let mut service = SystemService::with_power_profiles(
        system,
        None,
        Arc::clone(&live) as Arc<dyn PowerProfiles>,
    )
    .unwrap();
    service.settle();

    // While unready, SystemService polls System::power().
    assert_eq!(fake.power_reads(), 1);
    assert_eq!(
        service.read(POWER).unwrap()["modes"],
        json!(["saver", "balanced"])
    );
    assert_eq!(service.read(POWER).unwrap()["mode"], json!("balanced"));

    // Feeding the exact same modes/mode hands off cleanly with no double publish and no gap.
    live.feed(
        vec![PowerMode::Saver, PowerMode::Balanced],
        Some(PowerMode::Balanced),
    );
    assert!(
        !service.poll(),
        "identical live state after polling produces no double publish"
    );
    assert_eq!(service.read(POWER).unwrap()["mode"], json!("balanced"));

    // Further refreshes no longer poll System::power().
    service.refresh();
    service.settle();
    assert_eq!(fake.power_reads(), 1);

    // Subsequent live changes still arrive directly.
    live.set_mode(Some(PowerMode::Saver));
    assert!(service.poll());
    assert_eq!(service.read(POWER).unwrap()["mode"], json!("saver"));
    assert_eq!(fake.power_reads(), 1);
}

#[test]
fn live_set_power_mode_carries_new_mode_in_first_reading_when_pending_clears() {
    let fake = Fake::new();
    let live = FakeProfiles::new(
        vec![PowerMode::Saver, PowerMode::Balanced],
        Some(PowerMode::Balanced),
    );
    let system: Arc<dyn System> = Arc::clone(&fake) as Arc<dyn System>;
    let mut service = SystemService::with_power_profiles(
        system,
        None,
        Arc::clone(&live) as Arc<dyn PowerProfiles>,
    )
    .unwrap();
    service.settle();

    assert_eq!(
        service.invoke(
            POWER,
            "set-power-mode",
            &arguments(json!({"mode": "saver"}))
        ),
        Ok(Started::Accepted)
    );
    assert_eq!(
        service.read(POWER).unwrap()["pending"],
        json!(["set-power-mode"])
    );
    // Settle WITHOUT pushing any D-Bus signal to `live`.
    service.settle();
    let power = service.read(POWER).unwrap();
    assert_eq!(power["pending"], json!([]));
    assert_eq!(power["mode"], json!("saver"));
    assert_eq!(fake.power_reads(), 0);

    // When the delayed D-Bus signal later arrives with the same mode, poll reports no second publish.
    live.set_mode(Some(PowerMode::Saver));
    assert!(
        !service.poll(),
        "delayed signal matching the landed mode does not publish twice"
    );
}

#[test]
fn a_live_signal_during_power_mode_read_beats_the_stale_direct_read() {
    let fake = Fake::new();
    let live = FakeProfiles::new(
        vec![
            PowerMode::Saver,
            PowerMode::Balanced,
            PowerMode::Performance,
        ],
        Some(PowerMode::Balanced),
    );
    // Simulate a newer daemon signal moving the live source while `power_mode()`
    // is reading back the earlier `Saver` change.
    *fake.on_power_mode.lock().unwrap() = Some(Box::new({
        let live = Arc::clone(&live);
        move || live.set_mode(Some(PowerMode::Performance))
    }));

    let system: Arc<dyn System> = Arc::clone(&fake) as Arc<dyn System>;
    let mut service = SystemService::with_power_profiles(
        system,
        None,
        Arc::clone(&live) as Arc<dyn PowerProfiles>,
    )
    .unwrap();
    service.settle();

    assert_eq!(
        service.invoke(
            POWER,
            "set-power-mode",
            &arguments(json!({"mode": "saver"}))
        ),
        Ok(Started::Accepted)
    );
    service.settle();

    let power = service.read(POWER).unwrap();
    assert_eq!(power["pending"], json!([]));
    assert_eq!(
        power["mode"],
        json!("performance"),
        "a newer live signal that moved revision past rev_before must not be overwritten by the stale direct read"
    );
    assert_eq!(fake.power_reads(), 0);
}

#[test]
fn the_printers_resource_reads_the_queues_and_jobs_and_takes_their_actions() {
    let fake = Fake::new();
    let mut service = service(&fake);
    let printers = service.read(PRINTERS).unwrap();
    assert_eq!(printers["serving"], json!(true));
    assert_eq!(printers["queues"][0]["name"], json!("Office"));
    assert_eq!(printers["queues"][0]["state"], json!("ready"));
    assert_eq!(printers["jobs"][0]["id"], json!("12"));
    assert_eq!(printers["jobs"][0]["state"], json!("waiting"));

    let published = service
        .resources()
        .into_iter()
        .find(|resource| resource.id.as_str() == PRINTERS)
        .expect("the printers resource is published");
    let actions: Vec<&str> = published
        .actions
        .iter()
        .map(|action| action.id.as_str())
        .collect();
    for action in [
        "find",
        "add",
        "remove",
        "set-default",
        "set-media",
        "set-paused",
        "cancel-job",
        "retry-job",
    ] {
        assert!(
            actions.contains(&action),
            "{action} is published: {actions:?}"
        );
    }

    assert_eq!(
        service.invoke(PRINTERS, "cancel-job", &arguments(json!({"job": "12"}))),
        Ok(Started::Accepted)
    );
    assert_eq!(
        service.read(PRINTERS).unwrap()["pending"],
        json!(["cancel-job"])
    );
    service.settle();
    assert_eq!(fake.print_changes(), [PrintChange::CancelJob("12".into())]);
    let printers = service.read(PRINTERS).unwrap();
    assert_eq!(printers["pending"], json!([]));
    assert_eq!(printers["jobs"][0]["state"], json!("cancelled"));

    // A queue's paper sizes are what its driver lists, and choosing one is the machine's change.
    assert_eq!(printers["queues"][0]["media"], json!(["A4", "Letter"]));
    assert_eq!(printers["queues"][0]["default_media"], json!("A4"));
    assert_eq!(
        service.invoke(
            PRINTERS,
            "set-media",
            &arguments(json!({"printer": "Office", "media": "Letter"})),
        ),
        Ok(Started::Accepted)
    );
    service.settle();
    assert_eq!(
        fake.print_changes().last(),
        Some(&PrintChange::SetMedia("Office".into(), "Letter".into()))
    );
    assert_eq!(
        service.read(PRINTERS).unwrap()["queues"][0]["default_media"],
        json!("Letter")
    );

    // A machine whose printer service refuses says why, and the reading keeps nothing pending.
    *fake.print_refusal.lock().unwrap() = Some("The queue is gone".into());
    service
        .invoke(
            PRINTERS,
            "set-paused",
            &arguments(json!({"printer": "Office", "paused": true})),
        )
        .unwrap();
    service.settle();
    let printers = service.read(PRINTERS).unwrap();
    assert_eq!(printers["problem"], json!("The queue is gone"));
    assert_eq!(printers["pending"], json!([]));
}

#[test]
fn the_scanners_resource_lists_what_sane_finds_and_looks_again() {
    let fake = Fake::new();
    let mut service = service(&fake);
    let scanners = service.read(SCANNERS).unwrap();
    assert_eq!(scanners["available"], json!(true));
    assert_eq!(
        scanners["found"][0]["device"],
        json!("epson2:libusb:001:004")
    );
    assert_eq!(
        scanners["found"][0]["description"],
        json!("Epson Perfection V39")
    );

    let published = service
        .resources()
        .into_iter()
        .find(|resource| resource.id.as_str() == SCANNERS)
        .expect("the scanners resource is published");
    assert!(
        published
            .actions
            .iter()
            .any(|action| action.id.as_str() == "find-scanners"),
        "the look-again action is published"
    );

    assert_eq!(
        service.invoke(SCANNERS, "find-scanners", &Map::new()),
        Ok(Started::Accepted)
    );
    assert_eq!(
        service.read(SCANNERS).unwrap()["pending"],
        json!(["find-scanners"])
    );
    service.settle();
    assert_eq!(service.read(SCANNERS).unwrap()["pending"], json!([]));
}

#[test]
fn finding_printers_lands_what_the_network_answered() {
    let fake = Fake::new();
    let mut service = service(&fake);
    *fake.nearby.lock().unwrap() = Ok(vec![DiscoveredPrinter {
        uri: "ipp://printer.local/ipp/print".into(),
        name: "Hallway".into(),
        location: Some("Second floor".into()),
    }]);
    assert_eq!(
        service.invoke(PRINTERS, "find", &Map::new()),
        Ok(Started::Accepted)
    );
    service.settle();
    let printers = service.read(PRINTERS).unwrap();
    assert_eq!(printers["nearby"][0]["name"], json!("Hallway"));
    assert_eq!(printers["pending"], json!([]));

    // A plain read keeps what the find turned up until the next one.
    service.refresh();
    service.settle();
    assert_eq!(
        service.read(PRINTERS).unwrap()["nearby"][0]["name"],
        json!("Hallway")
    );

    *fake.nearby.lock().unwrap() = Err("no network".into());
    service.invoke(PRINTERS, "find", &Map::new()).unwrap();
    service.settle();
    let printers = service.read(PRINTERS).unwrap();
    assert_eq!(printers["problem"], json!("no network"));
}

#[test]
fn a_machine_with_no_printer_service_offers_no_print_actions() {
    let fake = Fake::new();
    fake.printers.lock().unwrap().serving = false;
    let mut service = service(&fake);
    let published = service
        .resources()
        .into_iter()
        .find(|resource| resource.id.as_str() == PRINTERS)
        .expect("the printers resource is published");
    let find = published
        .actions
        .iter()
        .find(|action| action.id.as_str() == "find")
        .expect("the find action is published, marked unavailable");
    assert_eq!(
        find.contract.availability,
        tilo_actions::Availability::Unavailable {
            code: "no_printing".into(),
            reason: "This computer has no printer service".into(),
        },
        "a machine without a printer service says so"
    );
    assert!(matches!(
        service.invoke(PRINTERS, "find", &Map::new()),
        Err(ApiError::Unavailable { .. })
    ));
}

#[test]
fn the_users_resource_reads_the_accounts_and_takes_their_actions() {
    let fake = Fake::new();
    let mut service = service(&fake);
    let users = service.read(USERS).unwrap();
    assert_eq!(users["serving"], json!(true));
    assert_eq!(users["accounts"][0]["username"], json!("ada"));
    assert_eq!(users["accounts"][0]["full_name"], json!("Ada Lovelace"));
    assert_eq!(users["accounts"][0]["admin"], json!(true));
    assert_eq!(users["accounts"][0]["current"], json!(true));

    let published = service
        .resources()
        .into_iter()
        .find(|resource| resource.id.as_str() == USERS)
        .expect("the users resource is published");
    let actions: Vec<&str> = published
        .actions
        .iter()
        .map(|action| action.id.as_str())
        .collect();
    for action in ["add", "set-admin", "set-automatic-login", "remove"] {
        assert!(
            actions.contains(&action),
            "{action} is published: {actions:?}"
        );
    }
    let add = published
        .actions
        .iter()
        .find(|action| action.id.as_str() == "add")
        .expect("adding an account is published");
    let password = add
        .params()
        .iter()
        .find(|param| &*param.name == "password")
        .expect("the password is a parameter");
    assert_eq!(
        password.ty,
        tilo_actions::ParamType::Secret,
        "the password never reaches history, logs or a confirmation"
    );

    assert_eq!(
        service.invoke(
            USERS,
            "add",
            &arguments(json!({"username": "grace", "full_name": "Grace Hopper", "password": "correct horse"})),
        ),
        Ok(Started::Accepted)
    );
    assert_eq!(
        service.read(USERS).unwrap()["pending"],
        json!(["add"]),
        "the action is pending until it lands"
    );
    service.settle();
    assert_eq!(
        fake.user_changes(),
        [UserChange::Add {
            username: "grace".into(),
            full_name: "Grace Hopper".into(),
            password: "correct horse".into(),
        }]
    );
    let users = service.read(USERS).unwrap();
    assert_eq!(users["pending"], json!([]));
    assert_eq!(users["accounts"][1]["username"], json!("grace"));

    // A machine whose account service refuses says why, and nothing stays pending.
    *fake.user_refusal.lock().unwrap() = Some("account change was refused: polkit".into());
    service
        .invoke(
            USERS,
            "set-admin",
            &arguments(json!({"user": "ada", "admin": false})),
        )
        .unwrap();
    service.settle();
    let users = service.read(USERS).unwrap();
    assert_eq!(
        users["problem"],
        json!("account change was refused: polkit")
    );
    assert_eq!(users["pending"], json!([]));
}

#[test]
fn a_machine_with_no_account_service_offers_no_account_actions() {
    let fake = Fake::new();
    fake.users.lock().unwrap().serving = false;
    let mut service = service(&fake);
    let published = service
        .resources()
        .into_iter()
        .find(|resource| resource.id.as_str() == USERS)
        .expect("the users resource is published");
    let add = published
        .actions
        .iter()
        .find(|action| action.id.as_str() == "add")
        .expect("the add action is published, marked unavailable");
    assert_eq!(
        add.contract.availability,
        tilo_actions::Availability::Unavailable {
            code: "no_account_service".into(),
            reason: "This computer cannot change its accounts".into(),
        },
        "a machine without an account service says so"
    );
    assert!(matches!(
        service.invoke(USERS, "add", &Map::new()),
        Err(ApiError::Unavailable { .. })
    ));
}

#[test]
fn rollback_goes_back_to_the_previous_system_and_refuses_without_the_grant() {
    let fake = Fake::new();
    *fake.update.lock().unwrap() = Update {
        booted: Some(Deployment {
            version: Some("44.2".into()),
            ..Deployment::default()
        }),
        rollback: Some(Deployment {
            version: Some("44.1".into()),
            ..Deployment::default()
        }),
        ..Update::default()
    };
    let mut service = service(&fake);
    let published = service
        .resources()
        .into_iter()
        .find(|resource| resource.id.as_str() == SYSTEM_UPDATE)
        .expect("the system-update resource is published");
    let rollback = published
        .actions
        .iter()
        .find(|action| action.id.as_str() == "rollback")
        .expect("the rollback action is published");
    assert_eq!(
        rollback.contract.availability,
        tilo_actions::Availability::Available,
        "a machine with a previous system offers to go back to it"
    );

    // The person at the machine, in wheel, is allowed by polkit; anyone else is refused, and
    // nothing is restarted.
    *fake.rollback_refusal.lock().unwrap() = Some("Interactive authentication required.".into());
    assert_eq!(
        service.invoke(SYSTEM_UPDATE, "rollback", &Map::new()),
        Ok(Started::Accepted)
    );
    assert_eq!(
        service.read(SYSTEM_UPDATE).unwrap()["pending"],
        json!(["rollback"])
    );
    service.settle();
    let update = service.read(SYSTEM_UPDATE).unwrap();
    assert_eq!(
        update["problem"],
        json!("Interactive authentication required."),
        "a person without the grant is told why it did not happen"
    );
    assert_eq!(update["pending"], json!([]));
    assert!(
        !fake
            .changes()
            .iter()
            .any(|change| matches!(change, Change::Restart)),
        "a refused rollback does not restart the machine"
    );
}

#[test]
fn a_machine_with_nothing_to_go_back_to_offers_no_rollback() {
    let fake = Fake::new();
    let mut service = service(&fake);
    let published = service
        .resources()
        .into_iter()
        .find(|resource| resource.id.as_str() == SYSTEM_UPDATE)
        .expect("the system-update resource is published");
    let rollback = published
        .actions
        .iter()
        .find(|action| action.id.as_str() == "rollback")
        .expect("the rollback action is published, marked unavailable");
    assert_eq!(
        rollback.contract.availability,
        tilo_actions::Availability::Unavailable {
            code: "nothing_to_roll_back".into(),
            reason: "There is no previous system to go back to".into(),
        }
    );
    assert!(matches!(
        service.invoke(SYSTEM_UPDATE, "rollback", &Map::new()),
        Err(ApiError::Unavailable { .. })
    ));
}

fn a_windows_drive() -> Drives {
    Drives {
        serving: true,
        drives: vec![DriveVolume {
            id: "sdb1:7".into(),
            label: "Windows".into(),
            size_bytes: 64 * 1024 * 1024 * 1024,
            placement: Placement::External,
            hot_added: true,
            remembered: None,
            needs_consent: true,
            preselect: DriveMode::OpenSafely,
            can_remember: RememberOffer::AnyMode,
            mounted: None,
        }],
        remembered: vec![RememberedDrive {
            grant: "sdb1-7".into(),
            label: "Windows".into(),
            placement: Placement::External,
            mode: DriveMode::OpenSafely,
            mounted: None,
        }],
        ..Drives::default()
    }
}

#[test]
fn the_drives_resource_reads_the_volumes_and_opens_them() {
    let fake = Fake::new();
    *fake.drives.lock().unwrap() = a_windows_drive();
    let mut service = service(&fake);
    let drives = service.read(DRIVES).unwrap();
    assert_eq!(drives["serving"], json!(true));
    assert_eq!(drives["drives"][0]["label"], json!("Windows"));
    assert_eq!(drives["drives"][0]["placement"], json!("external"));
    assert_eq!(drives["drives"][0]["needs_consent"], json!(true));
    assert_eq!(drives["remembered"][0]["grant"], json!("sdb1-7"));

    let published = service
        .resources()
        .into_iter()
        .find(|resource| resource.id.as_str() == DRIVES)
        .expect("the drives resource is published");
    let actions: Vec<&str> = published
        .actions
        .iter()
        .map(|action| action.id.as_str())
        .collect();
    for action in ["refresh", "mount", "unmount", "forget", "set-mode"] {
        assert!(
            actions.contains(&action),
            "{action} is published: {actions:?}"
        );
    }

    assert_eq!(
        service.invoke(
            DRIVES,
            "mount",
            &arguments(json!({"drive": "sdb1:7", "safely": true, "remember": true})),
        ),
        Ok(Started::Accepted)
    );
    assert_eq!(service.read(DRIVES).unwrap()["pending"], json!(["mount"]));
    service.settle();
    assert_eq!(
        fake.drive_changes(),
        [DriveChange::Mount {
            drive: "sdb1:7".into(),
            safely: true,
            remember: true,
        }]
    );
    let drives = service.read(DRIVES).unwrap();
    assert_eq!(drives["pending"], json!([]));
    assert_eq!(
        drives["drives"][0]["mounted"]["path"],
        json!("/run/media/ada/Windows")
    );

    // The service refuses a drive it does not know, and the reading says why.
    *fake.drive_refusal.lock().unwrap() = Some("This drive needs checking first".into());
    service
        .invoke(DRIVES, "unmount", &arguments(json!({"drive": "sdb1:7"})))
        .unwrap();
    service.settle();
    let drives = service.read(DRIVES).unwrap();
    assert_eq!(drives["problem"], json!("This drive needs checking first"));
    assert_eq!(drives["pending"], json!([]));
}

#[test]
fn a_machine_with_no_drive_service_offers_no_drive_actions() {
    let fake = Fake::new();
    let mut service = service(&fake);
    let published = service
        .resources()
        .into_iter()
        .find(|resource| resource.id.as_str() == DRIVES)
        .expect("the drives resource is published");
    let mount = published
        .actions
        .iter()
        .find(|action| action.id.as_str() == "mount")
        .expect("the mount action is published, marked unavailable");
    assert_eq!(
        mount.contract.availability,
        tilo_actions::Availability::Unavailable {
            code: "no_drive_service".into(),
            reason: "This computer has no drive service".into(),
        }
    );
    assert!(matches!(
        service.invoke(DRIVES, "mount", &Map::new()),
        Err(ApiError::Unavailable { .. })
    ));
}

fn a_storage_scan() -> Storage {
    Storage {
        disk: Some(DiskUsage {
            total_bytes: 512 * 1024 * 1024 * 1024,
            free_bytes: 256 * 1024 * 1024 * 1024,
        }),
        categories: vec![
            CategoryUsage {
                category: "Cache".into(),
                bytes: 7_300_000,
                files: 40,
                reclaimable: true,
            },
            CategoryUsage {
                category: "App data".into(),
                bytes: 300_000_000,
                files: 900,
                reclaimable: false,
            },
        ],
        largest: vec![StoredItem {
            category: "App data".into(),
            owner: "notes".into(),
            bytes: 300_000_000,
            files: 900,
            reclaimable: false,
        }],
        cache_bytes: 7_300_000,
        scanned: true,
        ..Storage::default()
    }
}

#[test]
fn the_storage_resource_reads_the_disk_and_clears_the_cache() {
    let fake = Fake::new();
    *fake.storage.lock().unwrap() = a_storage_scan();
    let mut service = service(&fake);
    let storage = service.read(STORAGE).unwrap();
    assert_eq!(storage["scanned"], json!(true));
    assert_eq!(
        storage["disk"]["total_bytes"],
        json!(512_u64 * 1024 * 1024 * 1024)
    );
    assert_eq!(storage["categories"][0]["category"], json!("Cache"));
    assert_eq!(storage["largest"][0]["owner"], json!("notes"));
    assert_eq!(storage["cache_bytes"], json!(7_300_000));

    let published = service
        .resources()
        .into_iter()
        .find(|resource| resource.id.as_str() == STORAGE)
        .expect("the storage resource is published");
    let actions: Vec<&str> = published
        .actions
        .iter()
        .map(|action| action.id.as_str())
        .collect();
    assert!(actions.contains(&"scan"), "{actions:?}");
    assert!(actions.contains(&"clear-cache"), "{actions:?}");

    assert_eq!(
        service.invoke(STORAGE, "clear-cache", &Map::new()),
        Ok(Started::Accepted)
    );
    assert_eq!(
        service.read(STORAGE).unwrap()["pending"],
        json!(["clear-cache"])
    );
    service.settle();
    assert_eq!(fake.storage_changes(), [StorageChange::ClearCache]);
    let storage = service.read(STORAGE).unwrap();
    assert_eq!(storage["pending"], json!([]));
    assert_eq!(storage["cache_bytes"], json!(0));

    // A cache an item changed under is refused, and the reading says why.
    *fake.storage_refusal.lock().unwrap() = Some("cleanup target changed after preview".into());
    service.invoke(STORAGE, "clear-cache", &Map::new()).unwrap();
    service.settle();
    let storage = service.read(STORAGE).unwrap();
    assert_eq!(
        storage["problem"],
        json!("cleanup target changed after preview")
    );
    assert_eq!(storage["pending"], json!([]));
}

#[test]
fn scanning_the_storage_again_lands_the_new_reading() {
    let fake = Fake::new();
    *fake.storage.lock().unwrap() = a_storage_scan();
    let mut service = service(&fake);
    fake.storage.lock().unwrap().largest[0].owner = "calendar".into();
    assert_eq!(
        service.invoke(STORAGE, "scan", &Map::new()),
        Ok(Started::Accepted)
    );
    service.settle();
    let storage = service.read(STORAGE).unwrap();
    assert_eq!(storage["largest"][0]["owner"], json!("calendar"));
    assert_eq!(storage["pending"], json!([]));
}

fn an_enrolled_volume() -> DiskUnlock {
    DiskUnlock {
        volumes: vec![UnlockVolume {
            volume: "0f3a9c1e-1111-4222-8333-444455556666".into(),
            device: "/dev/disk/by-uuid/0f3a9c1e-1111-4222-8333-444455556666".into(),
            keyslot: 2,
            sealing: Sealing::Tpm2,
            networks: vec!["Home".into()],
            siblings: vec!["Ada's laptop".into()],
        }],
        saved_networks: vec!["Home".into(), "Office 5G".into()],
        scanned: true,
        ..DiskUnlock::default()
    }
}

#[test]
fn the_disk_unlock_resource_reads_the_enrolment_and_chooses_its_networks() {
    let fake = Fake::new();
    *fake.disk_unlock.lock().unwrap() = an_enrolled_volume();
    let mut service = service(&fake);
    // Nothing is read until the resource is asked to.
    assert_eq!(service.read(DISK_UNLOCK).unwrap()["scanned"], json!(false));

    assert_eq!(
        service.invoke(DISK_UNLOCK, "refresh", &Map::new()),
        Ok(Started::Accepted)
    );
    service.settle();
    let disk = service.read(DISK_UNLOCK).unwrap();
    assert_eq!(disk["scanned"], json!(true));
    assert_eq!(
        disk["volumes"][0]["volume"],
        json!("0f3a9c1e-1111-4222-8333-444455556666")
    );
    assert_eq!(disk["volumes"][0]["sealing"], json!("tpm2"));
    assert_eq!(disk["volumes"][0]["networks"][0], json!("Home"));
    assert_eq!(disk["saved_networks"][1], json!("Office 5G"));

    let published = service
        .resources()
        .into_iter()
        .find(|resource| resource.id.as_str() == DISK_UNLOCK)
        .expect("the disk-unlock resource is published");
    let actions: Vec<&str> = published
        .actions
        .iter()
        .map(|action| action.id.as_str())
        .collect();
    assert!(actions.contains(&"refresh"), "{actions:?}");
    assert!(actions.contains(&"set-networks"), "{actions:?}");

    assert_eq!(
        service.invoke(
            DISK_UNLOCK,
            "set-networks",
            &arguments(json!({
                "volume": "0f3a9c1e-1111-4222-8333-444455556666",
                "networks": ["Home", "Office 5G"],
            })),
        ),
        Ok(Started::Accepted)
    );
    service.settle();
    assert_eq!(
        fake.disk_unlock_changes(),
        [DiskUnlockChange::SetNetworks {
            volume: "0f3a9c1e-1111-4222-8333-444455556666".into(),
            networks: vec!["Home".into(), "Office 5G".into()],
        }]
    );
    let disk = service.read(DISK_UNLOCK).unwrap();
    assert_eq!(disk["volumes"][0]["networks"][1], json!("Office 5G"));
    assert_eq!(disk["pending"], json!([]));
}

#[test]
fn changing_the_unlocking_without_the_grant_is_refused() {
    let fake = Fake::new();
    *fake.disk_unlock.lock().unwrap() = an_enrolled_volume();
    let mut service = service(&fake);
    *fake.disk_unlock_refusal.lock().unwrap() =
        Some("Not authorised to change this computer's unlocking".into());
    service
        .invoke(
            DISK_UNLOCK,
            "set-networks",
            &arguments(json!({
                "volume": "0f3a9c1e-1111-4222-8333-444455556666",
                "networks": ["Office 5G"],
            })),
        )
        .unwrap();
    service.settle();
    let disk = service.read(DISK_UNLOCK).unwrap();
    assert_eq!(
        disk["problem"],
        json!("Not authorised to change this computer's unlocking"),
        "a person the grant does not cover is told why nothing changed"
    );
    assert_eq!(disk["pending"], json!([]));
}

#[test]
fn the_host_resource_reads_the_devices_facts_and_refreshes_them() {
    let fake = Fake::new();
    *fake.host_info.lock().unwrap() = HostInfo {
        pending: Vec::new(),
        problem: None,
        pretty_name: "Tilo Linux 44".into(),
        kernel: "6.18.58".into(),
        uptime: "3d 4h 12m".into(),
        hostname: "ada-laptop".into(),
        arch: "x86_64".into(),
        cpu: "AMD Ryzen 7".into(),
        memory: "32 GB".into(),
        unknown: false,
    };
    let mut service = service(&fake);
    let host = service.read(HOST_INFO).unwrap();
    assert_eq!(host["hostname"], json!("ada-laptop"));
    assert_eq!(host["pretty_name"], json!("Tilo Linux 44"));
    assert_eq!(host["kernel"], json!("6.18.58"));
    assert_eq!(host["memory"], json!("32 GB"));

    let published = service
        .resources()
        .into_iter()
        .find(|resource| resource.id.as_str() == HOST_INFO)
        .expect("the host-info resource is published");
    let actions: Vec<&str> = published
        .actions
        .iter()
        .map(|action| action.id.as_str())
        .collect();
    assert!(actions.contains(&"refresh"), "{actions:?}");

    fake.host_info.lock().unwrap().uptime = "3d 4h 13m".into();
    assert_eq!(
        service.invoke(HOST_INFO, "refresh", &Map::new()),
        Ok(Started::Accepted)
    );
    assert_eq!(
        service.read(HOST_INFO).unwrap()["pending"],
        json!(["refresh"])
    );
    service.settle();
    let host = service.read(HOST_INFO).unwrap();
    assert_eq!(host["uptime"], json!("3d 4h 13m"));
    assert_eq!(host["pending"], json!([]));
}

#[test]
fn a_fact_this_machine_cannot_read_is_unknown_and_said_so() {
    let missing = std::path::Path::new("/nonexistent/tilo-host-info");
    let facts = crate::host_info::facts_from(
        missing,
        missing,
        missing,
        missing,
        &[missing, missing],
        missing,
    );
    assert_eq!(facts.kernel, "unknown");
    assert_eq!(facts.memory, "unknown");
    assert_eq!(facts.cpu, "unknown");
    assert_eq!(facts.hostname, "this-device");
    assert!(facts.unknown, "the reading says a fact is missing");
}

#[test]
fn the_codec_pack_resource_installs_removes_and_refuses() {
    let fake = Fake::new();
    *fake.codec.lock().unwrap() = CodecPack {
        available: true,
        state: CodecState::NotInstalled,
        hardware: HardwareVideo::Off,
        summary: "Off".into(),
        offered: vec!["install".into()],
        ..CodecPack::default()
    };
    let mut service = service(&fake);
    let codec = service.read(CODEC_PACK).unwrap();
    assert_eq!(codec["state"], json!("not-installed"));
    let published = service
        .resources()
        .into_iter()
        .find(|resource| resource.id.as_str() == CODEC_PACK)
        .expect("the codec-pack resource is published");
    let actions: Vec<&str> = published
        .actions
        .iter()
        .map(|action| action.id.as_str())
        .collect();
    for action in ["install", "remove", "refresh"] {
        assert!(
            actions.contains(&action),
            "{action} is published: {actions:?}"
        );
    }
    assert_eq!(
        service.invoke(CODEC_PACK, "install", &Map::new()),
        Ok(Started::Accepted)
    );
    service.settle();
    assert_eq!(
        fake.codec_changes.lock().unwrap().clone(),
        [CodecChange::Install]
    );
    assert_eq!(
        service.read(CODEC_PACK).unwrap()["state"],
        json!("installed")
    );

    *fake.codec_refusal.lock().unwrap() = Some("the helper refused".into());
    service.invoke(CODEC_PACK, "remove", &Map::new()).unwrap();
    service.settle();
    let codec = service.read(CODEC_PACK).unwrap();
    assert_eq!(codec["problem"], json!("the helper refused"));
    assert_eq!(codec["pending"], json!([]));
}

#[test]
fn the_windows_compat_resource_installs_and_refuses_without_the_grant() {
    let fake = Fake::new();
    *fake.windows.lock().unwrap() = WindowsCompat {
        available: true,
        state: WindowsState::NeedsInstall,
        summary: "Windows support is not installed.".into(),
        size: Some("3.4 GB".into()),
        ..WindowsCompat::default()
    };
    let mut service = service(&fake);
    let windows = service.read(WINDOWS_COMPAT).unwrap();
    assert_eq!(windows["state"], json!("needs-install"));
    assert_eq!(windows["size"], json!("3.4 GB"));
    assert_eq!(
        service.invoke(WINDOWS_COMPAT, "install", &Map::new()),
        Ok(Started::Accepted)
    );
    service.settle();
    assert_eq!(
        fake.windows_changes.lock().unwrap().clone(),
        [WindowsChange::Install]
    );
    assert_eq!(
        service.read(WINDOWS_COMPAT).unwrap()["state"],
        json!("ready")
    );

    *fake.windows_refusal.lock().unwrap() = Some("authentication was refused".into());
    service
        .invoke(WINDOWS_COMPAT, "remove", &Map::new())
        .unwrap();
    service.settle();
    let windows = service.read(WINDOWS_COMPAT).unwrap();
    assert_eq!(windows["problem"], json!("authentication was refused"));
}

#[test]
fn the_firmware_resource_reads_the_updates_and_installs_them() {
    let fake = Fake::new();
    *fake.firmware.lock().unwrap() = Firmware {
        available: true,
        updates: vec![FirmwareUpdate {
            device_id: Some("abc".into()),
            device: "Fingerprint reader".into(),
            current_version: Some("1.0".into()),
            new_version: Some("1.1".into()),
            ..FirmwareUpdate::default()
        }],
        ..Firmware::default()
    };
    let mut service = service(&fake);
    assert_eq!(
        service.read(FIRMWARE).unwrap()["updates"][0]["device"],
        json!("Fingerprint reader")
    );
    let published = service
        .resources()
        .into_iter()
        .find(|resource| resource.id.as_str() == FIRMWARE)
        .expect("the firmware resource is published");
    let actions: Vec<&str> = published
        .actions
        .iter()
        .map(|action| action.id.as_str())
        .collect();
    assert!(actions.contains(&"refresh"), "{actions:?}");
    assert!(actions.contains(&"install"), "{actions:?}");

    assert_eq!(
        service.invoke(FIRMWARE, "install", &Map::new()),
        Ok(Started::Accepted)
    );
    assert_eq!(service.read(FIRMWARE).unwrap()["installing"], json!(true));
    service.settle();
    assert_eq!(
        fake.firmware_changes.lock().unwrap().clone(),
        [FirmwareChange::Install]
    );
    assert_eq!(service.read(FIRMWARE).unwrap()["updates"], json!([]));

    *fake.firmware_refusal.lock().unwrap() = Some("connect power before updating firmware".into());
    service.invoke(FIRMWARE, "install", &Map::new()).unwrap();
    service.settle();
    let firmware = service.read(FIRMWARE).unwrap();
    assert_eq!(
        firmware["problem"],
        json!("connect power before updating firmware")
    );
}

/// The Windows programs this device has, in memory: what the resource reads, and what its
/// actions asked for, by the workspace they were asked in.
#[derive(Default)]
struct FakeWindows {
    reading: Mutex<tilo_system_resources::windows_apps::WindowsApps>,
    changes: Mutex<Vec<(String, String)>>,
    refusal: Mutex<Option<String>>,
}

impl FakeWindows {
    fn changes(&self) -> Vec<(String, String)> {
        self.changes.lock().unwrap().clone()
    }
}

impl crate::windows_apps::WindowsManager for FakeWindows {
    fn reading(&self, _workspace: &str) -> tilo_system_resources::windows_apps::WindowsApps {
        self.reading.lock().unwrap().clone()
    }

    fn change(
        &self,
        workspace: &str,
        action: &tilo_system_resources::windows_apps::WindowsAppsAction,
    ) -> Result<(), String> {
        use tilo_resource::Effect as _;
        let refusal = self.refusal.lock().unwrap().clone();
        if let Some(refusal) = refusal {
            return Err(refusal);
        }
        self.changes
            .lock()
            .unwrap()
            .push((workspace.to_owned(), action.id().to_owned()));
        Ok(())
    }
}

/// A Windows appliance whose one program is Adobe Reader, verified, installed and ready.
fn adobe_reader_ready() -> tilo_system_resources::windows_apps::WindowsApps {
    use tilo_system_resources::windows_apps::{
        Access, AppStatus, Confidence, Route, Source, WindowsApp, WindowsApps,
    };
    WindowsApps {
        serving: true,
        summary: "The Windows appliance is ready.".to_owned(),
        apps: vec![WindowsApp {
            id: "adobe-reader".to_owned(),
            name: "Adobe Acrobat Reader".to_owned(),
            publisher: "Adobe".to_owned(),
            route: Route::Wine,
            recommended_route: Route::Wine,
            runner: "Wine 11.0".to_owned(),
            capsule: "Adobe Reader".to_owned(),
            setup_name: Some("Adobe Acrobat Reader".to_owned()),
            source: Source::Catalogue,
            confidence: Confidence::Verified,
            reason: "A verified recipe opens Reader in Wine.".to_owned(),
            status: AppStatus::Ready,
            access: Access {
                files_read_only: 1,
                ..Access::default()
            },
            fallback_detail: String::new(),
            launch_available: true.into(),
            launch_unavailable_reason: None,
            portal_file_open_available: false.into(),
            portal_file_open_unavailable_reason: None,
            route_override_available: true.into(),
            route_override_unavailable_reason: None,
            can_retry_install: false.into(),
            can_escalate_install_to_vm: false.into(),
            can_remove_launcher: true.into(),
            can_vendor_uninstall: true.into(),
            can_delete_data: true.into(),
            can_publish_vm_candidate: false.into(),
            installation: None,
            vm_fallback_available: false.into(),
            vm_fallback_unavailable_reason: None,
        }],
        ..WindowsApps::default()
    }
}

#[test]
fn the_windows_programs_are_the_device_s_resource_and_their_actions_act_in_a_workspace() {
    use tilo_system_resources::windows_apps::WINDOWS_APPS;

    let fake = Fake::new();
    let windows = Arc::new(FakeWindows::default());
    *windows.reading.lock().unwrap() = adobe_reader_ready();
    let system: Arc<dyn System> = Arc::clone(&fake) as Arc<dyn System>;
    let mut service = SystemService::with_windows_manager(
        system,
        None,
        Arc::clone(&windows) as Arc<dyn crate::windows_apps::WindowsManager>,
    )
    .unwrap();
    service.settle();

    let reading = service
        .read_for("so.tilo.windows", "w1", WINDOWS_APPS)
        .expect("the Windows programs are published");
    assert_eq!(reading["apps"][0]["name"], json!("Adobe Acrobat Reader"));
    assert_eq!(reading["apps"][0]["route"], json!("wine"));

    let published = service
        .resources()
        .into_iter()
        .find(|resource| resource.id.as_str() == WINDOWS_APPS)
        .expect("the resource is published");
    let actions: Vec<&str> = published
        .actions
        .iter()
        .map(|action| action.id.as_str())
        .collect();
    for action in ["launch", "stop", "open-file", "install", "set-route", "delete-data"] {
        assert!(
            actions.contains(&action),
            "{action} is published: {actions:?}"
        );
    }

    assert_eq!(
        service.invoke_for(
            "so.tilo.windows",
            "w1",
            WINDOWS_APPS,
            "launch",
            &arguments(json!({"program": "adobe-reader"})),
        ),
        Ok(Started::Accepted)
    );
    service.settle();
    assert_eq!(
        windows.changes(),
        [("w1".to_owned(), "launch".to_owned())]
    );

    // The action's own refusal is said in the reading until the next.
    *windows.refusal.lock().unwrap() = Some("The capsule's runner is not installed.".to_owned());
    service
        .invoke_for(
            "so.tilo.windows",
            "w1",
            WINDOWS_APPS,
            "launch",
            &arguments(json!({"program": "adobe-reader"})),
        )
        .unwrap();
    service.settle();
    let reading = service
        .read_for("so.tilo.windows", "w1", WINDOWS_APPS)
        .unwrap();
    assert_eq!(
        reading["problem"],
        json!("The capsule's runner is not installed.")
    );
    assert_eq!(reading["pending"], json!([]));
}
