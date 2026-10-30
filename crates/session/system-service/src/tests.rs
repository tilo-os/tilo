use std::sync::{Arc, Mutex};

use serde_json::{Map, Value, json};
use settings_store::day_schedule::Location;
use tilo_actions::DoorError;
use tilo_service::{Service, Services, Started};

use crate::device::{Change, System};
use crate::model::{KeyboardLight, Power, PowerMode, Time};
use crate::update::Update;
use crate::{KEYBOARD_BACKLIGHT, POWER, SYSTEM_UPDATE, SystemService, TIME_ZONE};

/// A machine in memory, in Amsterdam unless told it is elsewhere.
struct Fake {
    power: Mutex<Power>,
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
}

impl System for Fake {
    fn power(&self) -> Power {
        self.power.lock().unwrap().clone()
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
            Change::PowerMode(mode) => self.power.lock().unwrap().mode = Some(*mode),
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
fn the_power_mode_is_one_the_machine_offers() {
    let fake = Fake::new();
    let mut service = service(&fake);
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
    assert!(matches!(
        service.invoke(
            POWER,
            "set-power-mode",
            &arguments(json!({"mode": "performance"}))
        ),
        Err(DoorError::InvalidArguments(_))
    ));
    assert_eq!(fake.changes(), [Change::PowerMode(PowerMode::Saver)]);
}

#[test]
fn restarting_and_powering_off_ask_for_confirmation() {
    let fake = Fake::new();
    let mut services = Services::default();
    services.add(Box::new(service(&fake)));
    for action in ["restart", "power-off", "log-out"] {
        assert_eq!(
            services.invoke(POWER, action, &Map::new()),
            Some(Err(DoorError::ConfirmationRequired)),
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
    services.settle();
    assert_eq!(fake.changes(), [Change::Suspend, Change::Restart]);
}

#[test]
fn an_update_reports_what_it_needs() {
    let fake = Fake::new();
    let mut service = service(&fake);
    assert!(
        matches!(
            service.invoke(SYSTEM_UPDATE, "restart-to-update", &Map::new()),
            Err(DoorError::Unavailable { .. })
        ),
        "nothing waits for a restart"
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
            Err(DoorError::Unavailable { .. })
        ),
        "one update at a time"
    );
    service.settle();
    let update = service.read(SYSTEM_UPDATE).unwrap();
    assert_eq!(update["updating"], json!(false));
    assert_eq!(update["problem"], json!("nothing newer to fetch"));
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
        Err(DoorError::InvalidArguments(_))
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
        Err(DoorError::InvalidArguments(_))
    ));

    *fake.keyboard_light.lock().unwrap() = KeyboardLight::default();
    service.refresh();
    service.settle();
    assert!(matches!(
        service.invoke(KEYBOARD_BACKLIGHT, "brighter", &Map::new()),
        Err(DoorError::Unavailable { .. })
    ));
}
