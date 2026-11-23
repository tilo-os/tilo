use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value, json};
use settings_store::day_schedule::Location;
use tilo_actions::ApiError;
use tilo_service::{Service, Services, Started, Wake};

use crate::device::{Change, PowerProfiles, System};
use crate::model::{KeyboardLight, Power, PowerMode, Time};
use crate::update::Update;
use crate::{KEYBOARD_BACKLIGHT, POWER, SYSTEM_UPDATE, SystemService, TIME_ZONE};

/// A machine in memory, in Amsterdam unless told it is elsewhere.
struct Fake {
    power: Mutex<Power>,
    power_reads: AtomicUsize,
    power_refusal: Mutex<Option<String>>,
    update: Mutex<Update>,
    time: Mutex<Time>,
    keyboard_light: Mutex<KeyboardLight>,
    location: Mutex<Result<Location, String>>,
    changes: Mutex<Vec<Change>>,
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
        })
    }

    fn changes(&self) -> Vec<Change> {
        self.changes.lock().unwrap().clone()
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
    revision: AtomicU64,
    wakes: Mutex<Vec<Wake>>,
}

impl FakeProfiles {
    fn new(modes: Vec<PowerMode>, mode: Option<PowerMode>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new((modes, mode)),
            revision: AtomicU64::new(0),
            wakes: Mutex::new(Vec::new()),
        })
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
