#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a private D-Bus fixture must start before the reader under test is asked"
)]

//! The reader and the SIM operations against a scripted `ModemManager` on
//! a private bus: no modem, a SIM asking for its PIN, a SIM asking for its
//! PUK, registered at home, roaming, and a modem with two SIM slots.

use std::collections::HashMap;
use std::io::BufRead;
use std::sync::{Arc, Mutex};

use zbus::zvariant::{OwnedObjectPath, OwnedValue};
use zeroize::Zeroizing;

use super::*;

const MODEM: &str = "/org/freedesktop/ModemManager1/Modem/0";
const SIM_1: &str = "/org/freedesktop/ModemManager1/SIM/0";
const SIM_2: &str = "/org/freedesktop/ModemManager1/SIM/1";
const BEARER: &str = "/org/freedesktop/ModemManager1/Bearer/0";

/// A private `dbus-daemon`, never the person's buses.
struct Bus {
    child: std::process::Child,
    address: String,
    _root: tempfile::TempDir,
}

impl Bus {
    fn start() -> Self {
        let root = tempfile::tempdir().unwrap();
        let mut child = std::process::Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .arg(format!(
                "--address=unix:path={}",
                root.path().join("bus").display()
            ))
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("the test image provides dbus-daemon");
        let mut address = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        Self {
            child,
            address: address.trim().into(),
            _root: root,
        }
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// What the scripted modem reports and what it was asked.
#[derive(Clone)]
struct Script {
    state: i32,
    failed_reason: u32,
    unlock_required: u32,
    pin_retries: u32,
    puk_retries: u32,
    pin: String,
    puk: String,
    pin_enabled: bool,
    registration: u32,
    operator: String,
    technologies: u32,
    signal: u32,
    modes: (u32, u32),
    supported: Vec<(u32, u32)>,
    slots: Vec<String>,
    primary_slot: u32,
    received: u64,
    sent: u64,
}

impl Script {
    /// Registered at home on LTE, the SIM unlocked.
    fn registered() -> Self {
        Self {
            state: 8,
            failed_reason: 0,
            unlock_required: 1,
            pin_retries: 3,
            puk_retries: 10,
            pin: "1234".into(),
            puk: "12345678".into(),
            pin_enabled: true,
            registration: 1,
            operator: "Telekom.de".into(),
            technologies: 1 << 14,
            signal: 72,
            modes: (0b1_1110, 1 << 4),
            supported: vec![(0b0_1110, 1 << 3), (0b1_1110, 1 << 4), (1 << 3, 0)],
            slots: Vec::new(),
            primary_slot: 0,
            received: 3_000_000,
            sent: 1_000_000,
        }
    }

    fn locked() -> Self {
        Self {
            state: 2,
            unlock_required: 2,
            registration: 0,
            ..Self::registered()
        }
    }
}

type Shared = Arc<Mutex<Script>>;

#[derive(zbus::DBusError, Debug)]
#[zbus(prefix = "org.freedesktop.ModemManager1.Error.MobileEquipment")]
enum MobileEquipment {
    IncorrectPassword(String),
}

struct FakeModem(Shared);

#[expect(
    clippy::unused_self,
    reason = "a D-Bus property is read through its object"
)]
#[zbus::interface(name = "org.freedesktop.ModemManager1.Modem")]
impl FakeModem {
    #[zbus(property)]
    fn state(&self) -> i32 {
        self.0.lock().unwrap().state
    }
    #[zbus(property)]
    fn state_failed_reason(&self) -> u32 {
        self.0.lock().unwrap().failed_reason
    }
    #[zbus(property)]
    fn manufacturer(&self) -> String {
        "Quectel".into()
    }
    #[zbus(property)]
    fn model(&self) -> String {
        "EM120R-GL".into()
    }
    #[zbus(property)]
    fn unlock_required(&self) -> u32 {
        self.0.lock().unwrap().unlock_required
    }
    #[zbus(property)]
    fn unlock_retries(&self) -> HashMap<u32, u32> {
        let script = self.0.lock().unwrap();
        HashMap::from([(2, script.pin_retries), (4, script.puk_retries)])
    }
    #[zbus(property)]
    fn signal_quality(&self) -> (u32, bool) {
        (self.0.lock().unwrap().signal, true)
    }
    #[zbus(property)]
    fn access_technologies(&self) -> u32 {
        self.0.lock().unwrap().technologies
    }
    #[zbus(property)]
    fn supported_modes(&self) -> Vec<(u32, u32)> {
        self.0.lock().unwrap().supported.clone()
    }
    #[zbus(property)]
    fn current_modes(&self) -> (u32, u32) {
        self.0.lock().unwrap().modes
    }
    #[zbus(property)]
    fn sim(&self) -> OwnedObjectPath {
        OwnedObjectPath::try_from(SIM_1).unwrap()
    }
    #[zbus(property)]
    fn sim_slots(&self) -> Vec<OwnedObjectPath> {
        self.0
            .lock()
            .unwrap()
            .slots
            .iter()
            .map(|path| OwnedObjectPath::try_from(path.as_str()).unwrap())
            .collect()
    }
    #[zbus(property)]
    fn primary_sim_slot(&self) -> u32 {
        self.0.lock().unwrap().primary_slot
    }
    #[zbus(property)]
    fn own_numbers(&self) -> Vec<String> {
        vec!["+491701234567".into()]
    }
    #[zbus(property)]
    fn bearers(&self) -> Vec<OwnedObjectPath> {
        vec![OwnedObjectPath::try_from(BEARER).unwrap()]
    }
    fn set_current_modes(&self, modes: (u32, u32)) {
        self.0.lock().unwrap().modes = modes;
    }
    fn set_primary_sim_slot(&self, slot: u32) {
        self.0.lock().unwrap().primary_slot = slot;
    }
}

struct FakeGpp(Shared);

#[expect(
    clippy::unused_self,
    reason = "a D-Bus property is read through its object"
)]
#[zbus::interface(name = "org.freedesktop.ModemManager1.Modem.Modem3gpp")]
impl FakeGpp {
    #[zbus(property)]
    fn operator_name(&self) -> String {
        self.0.lock().unwrap().operator.clone()
    }
    #[zbus(property)]
    fn operator_code(&self) -> String {
        "26201".into()
    }
    #[zbus(property)]
    fn registration_state(&self) -> u32 {
        self.0.lock().unwrap().registration
    }
    #[zbus(property)]
    fn enabled_facility_locks(&self) -> u32 {
        u32::from(self.0.lock().unwrap().pin_enabled)
    }
}

struct FakeSim {
    script: Shared,
    path: &'static str,
    operator: &'static str,
    kind: u32,
}

impl FakeSim {
    /// A wrong code costs a try; the last wrong PIN asks for the PUK.
    fn refuse(script: &mut Script) -> MobileEquipment {
        if script.unlock_required == 4 {
            script.puk_retries = script.puk_retries.saturating_sub(1);
        } else {
            script.pin_retries = script.pin_retries.saturating_sub(1);
            if script.pin_retries == 0 {
                script.unlock_required = 4;
            }
        }
        MobileEquipment::IncorrectPassword("Incorrect password".into())
    }
}

#[expect(
    clippy::unused_self,
    reason = "a D-Bus property is read through its object"
)]
#[zbus::interface(name = "org.freedesktop.ModemManager1.Sim")]
impl FakeSim {
    #[zbus(property)]
    fn active(&self) -> bool {
        let script = self.script.lock().unwrap();
        script.slots.is_empty()
            || script
                .slots
                .iter()
                .position(|slot| slot == self.path)
                .is_some_and(|index| {
                    index.checked_add(1) == usize::try_from(script.primary_slot).ok()
                })
    }
    #[zbus(property)]
    fn sim_identifier(&self) -> String {
        "8949000000000000001".into()
    }
    #[zbus(property)]
    fn operator_identifier(&self) -> String {
        "26201".into()
    }
    #[zbus(property)]
    fn operator_name(&self) -> String {
        self.operator.into()
    }
    #[zbus(property)]
    fn sim_type(&self) -> u32 {
        self.kind
    }
    #[zbus(property)]
    fn esim_status(&self) -> u32 {
        if self.kind == 2 { 2 } else { 0 }
    }
    fn send_pin(&self, pin: &str) -> Result<(), MobileEquipment> {
        let mut script = self.script.lock().unwrap();
        if script.unlock_required != 2 || pin != script.pin {
            return Err(Self::refuse(&mut script));
        }
        script.unlock_required = 1;
        script.pin_retries = 3;
        script.state = 8;
        drop(script);
        Ok(())
    }
    fn send_puk(&self, puk: &str, pin: &str) -> Result<(), MobileEquipment> {
        let mut script = self.script.lock().unwrap();
        if puk != script.puk {
            return Err(Self::refuse(&mut script));
        }
        script.pin = pin.into();
        script.unlock_required = 1;
        script.pin_retries = 3;
        script.state = 8;
        drop(script);
        Ok(())
    }
    fn enable_pin(&self, pin: &str, enabled: bool) -> Result<(), MobileEquipment> {
        let mut script = self.script.lock().unwrap();
        if pin != script.pin {
            return Err(Self::refuse(&mut script));
        }
        script.pin_enabled = enabled;
        drop(script);
        Ok(())
    }
    fn change_pin(&self, old_pin: &str, new_pin: &str) -> Result<(), MobileEquipment> {
        let mut script = self.script.lock().unwrap();
        if old_pin != script.pin {
            return Err(Self::refuse(&mut script));
        }
        script.pin = new_pin.into();
        drop(script);
        Ok(())
    }
}

struct FakeBearer(Shared);

#[zbus::interface(name = "org.freedesktop.ModemManager1.Bearer")]
impl FakeBearer {
    #[zbus(property)]
    fn stats(&self) -> HashMap<String, OwnedValue> {
        let script = self.0.lock().unwrap();
        HashMap::from([
            (
                "total-rx-bytes".to_owned(),
                OwnedValue::from(script.received),
            ),
            ("total-tx-bytes".to_owned(), OwnedValue::from(script.sent)),
            ("rx-bytes".to_owned(), OwnedValue::from(10u64)),
            ("tx-bytes".to_owned(), OwnedValue::from(10u64)),
        ])
    }
}

/// A scripted `ModemManager` on a private bus, and a client of it.
struct Fixture {
    _bus: Bus,
    _server: zbus::Connection,
    manager: ModemManager,
    script: Shared,
}

impl Fixture {
    /// `ModemManager` with `script`'s modem, or (`None`) with no modem.
    fn new(script: Option<Script>) -> Self {
        let bus = Bus::start();
        let modem = script.is_some();
        let script = Arc::new(Mutex::new(script.unwrap_or_else(Script::registered)));
        let shared = Arc::clone(&script);
        let address = bus.address.clone();
        let (server, client) = block(async move {
            let mut builder = zbus::connection::Builder::address(address.as_str())
                .unwrap()
                .name(SERVICE)
                .unwrap()
                .serve_at(ROOT, zbus::fdo::ObjectManager)
                .unwrap();
            if modem {
                builder = builder
                    .serve_at(MODEM, FakeModem(Arc::clone(&shared)))
                    .unwrap()
                    .serve_at(MODEM, FakeGpp(Arc::clone(&shared)))
                    .unwrap()
                    .serve_at(
                        SIM_1,
                        FakeSim {
                            script: Arc::clone(&shared),
                            path: SIM_1,
                            operator: "Telekom.de",
                            kind: 1,
                        },
                    )
                    .unwrap()
                    .serve_at(
                        SIM_2,
                        FakeSim {
                            script: Arc::clone(&shared),
                            path: SIM_2,
                            operator: "Vodafone.de",
                            kind: 2,
                        },
                    )
                    .unwrap()
                    .serve_at(BEARER, FakeBearer(Arc::clone(&shared)))
                    .unwrap();
            }
            let server = builder.build().await.unwrap();
            let client = zbus::connection::Builder::address(address.as_str())
                .unwrap()
                .build()
                .await
                .unwrap();
            Ok((server, client))
        })
        .unwrap();
        Self {
            _bus: bus,
            _server: server,
            manager: ModemManager::on(client),
            script,
        }
    }

    fn read(&self) -> MobileReading {
        block(self.manager.read()).unwrap()
    }

    fn modem(&self) -> ModemInfo {
        let mut reading = self.read();
        assert_eq!(reading.modems.len(), 1, "one modem is reported");
        reading.modems.remove(0)
    }

    fn apply(&self, change: &ModemChange) -> Result<(), ModemError> {
        block(self.manager.apply(change))
    }
}

fn code(digits: &str) -> Zeroizing<String> {
    Zeroizing::new(digits.into())
}

#[test]
fn without_a_modem_modem_manager_reports_none() {
    let fixture = Fixture::new(None);
    assert_eq!(fixture.read(), MobileReading::default());
}

#[test]
fn a_registered_modem_reports_operator_technology_signal_and_traffic() {
    let fixture = Fixture::new(Some(Script::registered()));
    let modem = fixture.modem();
    assert_eq!(modem.name, "Quectel EM120R-GL");
    assert_eq!(modem.state, ModemState::Registered);
    assert!(modem.usable());
    assert_eq!(modem.registration, Registration::Home);
    assert_eq!(modem.operator.as_deref(), Some("Telekom.de"));
    assert_eq!(modem.technology, Some(Technology::Lte));
    assert_eq!(modem.signal, Some(72));
    assert_eq!(modem.pin_enabled, Some(true));
    assert_eq!(modem.numbers, ["+491701234567"]);
    assert_eq!(
        modem.traffic,
        Some(Traffic {
            received: 3_000_000,
            sent: 1_000_000
        }),
        "the totals ModemManager keeps, not the current connection's"
    );
    let current = modem.modes.unwrap();
    assert_eq!(current.generations(), [5, 4, 3, 2]);
    assert_eq!(current.preferred_generation(), Some(5));
    assert_eq!(current.key(), "5g-4g-3g-2g-prefer-5g");
    assert_eq!(modem.supported_modes.len(), 3);
    let sim = modem.active_sim().unwrap();
    assert_eq!(sim.path, SIM_1);
    assert_eq!(sim.label(), "Telekom.de");
    assert_eq!(sim.operator_code.as_deref(), Some("26201"));
    assert_eq!(sim.kind, SimKind::Physical);
}

#[test]
fn a_roaming_modem_says_so() {
    let fixture = Fixture::new(Some(Script {
        registration: 5,
        operator: "Orange F".into(),
        technologies: 1 << 15 | 1 << 14,
        ..Script::registered()
    }));
    let modem = fixture.modem();
    assert_eq!(modem.registration, Registration::Roaming);
    assert_eq!(modem.operator.as_deref(), Some("Orange F"));
    assert_eq!(
        modem.technology,
        Some(Technology::Nr5g),
        "the fastest in use"
    );
}

#[test]
fn a_locked_sim_is_unlocked_with_its_pin_and_a_wrong_pin_costs_a_try() {
    let fixture = Fixture::new(Some(Script::locked()));
    let modem = fixture.modem();
    assert_eq!(modem.state, ModemState::Locked);
    assert_eq!(modem.lock, SimLock::Pin);
    assert_eq!(modem.retries.pin, Some(3));
    assert_eq!(modem.signal, None, "no signal is claimed while locked");
    assert_eq!(modem.operator, None);

    let wrong = fixture.apply(&ModemChange::SendPin {
        sim: SIM_1.into(),
        pin: code("0000"),
    });
    assert_eq!(wrong, Err(ModemError::WrongCode));
    assert_eq!(fixture.modem().retries.pin, Some(2));

    fixture
        .apply(&ModemChange::SendPin {
            sim: SIM_1.into(),
            pin: code("1234"),
        })
        .unwrap();
    let modem = fixture.modem();
    assert_eq!(modem.lock, SimLock::None);
    assert!(modem.usable());
}

#[test]
fn the_last_wrong_pin_asks_for_the_puk_which_sets_a_new_pin() {
    let fixture = Fixture::new(Some(Script {
        pin_retries: 1,
        ..Script::locked()
    }));
    assert_eq!(
        fixture.apply(&ModemChange::SendPin {
            sim: SIM_1.into(),
            pin: code("9999"),
        }),
        Err(ModemError::WrongCode)
    );
    let modem = fixture.modem();
    assert_eq!(modem.lock, SimLock::Puk);
    assert_eq!(modem.retries.puk, Some(10));

    fixture
        .apply(&ModemChange::SendPuk {
            sim: SIM_1.into(),
            puk: code("12345678"),
            new_pin: code("4321"),
        })
        .unwrap();
    assert_eq!(fixture.modem().lock, SimLock::None);
    assert_eq!(fixture.script.lock().unwrap().pin, "4321");
}

#[test]
fn the_pin_is_changed_and_disabled_with_the_current_pin() {
    let fixture = Fixture::new(Some(Script::registered()));
    fixture
        .apply(&ModemChange::ChangePin {
            sim: SIM_1.into(),
            old_pin: code("1234"),
            new_pin: code("2468"),
        })
        .unwrap();
    assert_eq!(
        fixture.apply(&ModemChange::SetPinLock {
            sim: SIM_1.into(),
            pin: code("1234"),
            enabled: false,
        }),
        Err(ModemError::WrongCode),
        "the old PIN no longer works"
    );
    fixture
        .apply(&ModemChange::SetPinLock {
            sim: SIM_1.into(),
            pin: code("2468"),
            enabled: false,
        })
        .unwrap();
    assert_eq!(fixture.modem().pin_enabled, Some(false));
}

#[test]
fn malformed_codes_and_objects_never_reach_modem_manager() {
    let fixture = Fixture::new(Some(Script::locked()));
    for change in [
        ModemChange::SendPin {
            sim: SIM_1.into(),
            pin: code("12a4"),
        },
        ModemChange::SendPin {
            sim: SIM_1.into(),
            pin: code("123"),
        },
        ModemChange::SendPin {
            sim: "/org/freedesktop/ModemManager1/Modem/0".into(),
            pin: code("1234"),
        },
        ModemChange::SendPuk {
            sim: SIM_1.into(),
            puk: code("1234"),
            new_pin: code("1234"),
        },
        ModemChange::UseSlot {
            modem: MODEM.into(),
            slot: 0,
        },
    ] {
        assert!(
            matches!(fixture.apply(&change), Err(ModemError::Invalid(_))),
            "{change:?} is refused before it is sent"
        );
    }
    assert_eq!(
        fixture.modem().retries.pin,
        Some(3),
        "no try was spent on a malformed code"
    );
}

#[test]
fn network_types_and_the_sim_slot_are_set_on_the_modem() {
    let fixture = Fixture::new(Some(Script {
        slots: vec![SIM_1.into(), SIM_2.into()],
        primary_slot: 1,
        ..Script::registered()
    }));
    let modem = fixture.modem();
    assert_eq!(modem.sims.len(), 2, "both slots are listed");
    assert_eq!(modem.sims[0].slot, Some(1));
    assert!(modem.sims[0].active);
    assert_eq!(modem.sims[1].label(), "Vodafone.de");
    assert_eq!(
        modem.sims[1].kind,
        SimKind::Esim {
            profiles: Some(true)
        }
    );

    fixture
        .apply(&ModemChange::UseSlot {
            modem: MODEM.into(),
            slot: 2,
        })
        .unwrap();
    let lte_only = NetworkModes {
        allowed: 1 << 3,
        preferred: 0,
    };
    fixture
        .apply(&ModemChange::SetModes {
            modem: MODEM.into(),
            modes: lte_only,
        })
        .unwrap();
    let modem = fixture.modem();
    assert_eq!(modem.active_sim().unwrap().path, SIM_2);
    assert_eq!(modem.modes, Some(lte_only));
    assert_eq!(lte_only.key(), "4g");
}

#[test]
fn a_modem_without_a_sim_says_why() {
    let fixture = Fixture::new(Some(Script {
        state: -1,
        failed_reason: 2,
        ..Script::registered()
    }));
    let modem = fixture.modem();
    assert_eq!(modem.failure, Some(ModemFailure::SimMissing));
    assert!(!modem.usable());
}

#[test]
fn the_mobile_data_profile_is_the_connected_one_else_the_automatic_one() {
    let list = "\
0d7f5e66-3b1b-4a0b-9a59-0f1a2c3d4e5f:802-11-wireless:yes:yes:1700000900
5b2e8a70-1c7d-4f2e-9a61-0d3c9e4b7a10:gsm:no:no:1700000800
9e41c2d8-6a35-4b9f-8c07-2f5d1e8a3b64:gsm:yes:no:1700000100
";
    let chosen = choose_mobile_profile(list).unwrap();
    assert_eq!(chosen.uuid, "9e41c2d8-6a35-4b9f-8c07-2f5d1e8a3b64");
    assert_eq!(choose_mobile_profile("x:802-3-ethernet:yes:yes:1\n"), None);

    let profile = parse_mobile_profile(
        chosen.clone(),
        "connection.id:Mobile data\nconnection.autoconnect:yes\nconnection.metered:yes\ngsm.apn:\ngsm.auto-config:yes\ngsm.home-only:yes\n",
    );
    assert_eq!(profile.name, "Mobile data");
    assert!(profile.automatic_apn);
    assert_eq!(profile.apn, None);
    assert!(!profile.roaming, "home only means no roaming");
    assert_eq!(profile.metered, MeteredPolicy::Metered);

    let typed = parse_mobile_profile(
        chosen,
        "connection.id:Work\nconnection.autoconnect:no\nconnection.metered:unknown\ngsm.apn:internet.telekom\ngsm.auto-config:no\ngsm.home-only:no\n",
    );
    assert_eq!(typed.apn.as_deref(), Some("internet.telekom"));
    assert!(!typed.automatic_apn && typed.roaming && !typed.autoconnect);
    assert_eq!(typed.metered, MeteredPolicy::Automatic);
}

#[test]
fn an_apn_is_refused_with_characters_no_operator_uses() {
    assert!(valid_apn("internet.telekom").is_ok());
    assert!(valid_apn("web.vodafone.de").is_ok());
    assert!(valid_apn("").is_err());
    assert!(valid_apn("bad apn").is_err());
    assert!(valid_apn("inject\nline").is_err());
}
