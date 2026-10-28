//! `tilo-nm`'s reads and changes of devices, radios, saved profiles and
//! activation, against a fake `NetworkManager` served over a socket pair:
//! the production client code, a real D-Bus wire, no bus daemon.

#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]
#![allow(
    clippy::unused_self,
    clippy::needless_pass_by_value,
    clippy::needless_pass_by_ref_mut,
    clippy::used_underscore_binding,
    clippy::unnecessary_wraps,
    clippy::significant_drop_tightening,
    reason = "the fakes' methods are NetworkManager's D-Bus signatures over state the test shares"
)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_nm::{DeviceKind, IpFamily, NmClient, NmError, Setting, Settings};
use zbus::connection::Builder;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, Guid};
use zeroize::Zeroizing;

const WIFI: &str = "/org/freedesktop/NetworkManager/Devices/2";
const WIRED: &str = "/org/freedesktop/NetworkManager/Devices/3";
const AP_HOME: &str = "/org/freedesktop/NetworkManager/AccessPoint/1";
const AP_CAFE: &str = "/org/freedesktop/NetworkManager/AccessPoint/2";
const IP4: &str = "/org/freedesktop/NetworkManager/IP4Config/1";
const SETTINGS: &str = "/org/freedesktop/NetworkManager/Settings";
const AGENT_MANAGER: &str = "/org/freedesktop/NetworkManager/AgentManager";
const HOME_UUID: &str = "0d7f5e66-3b1b-4a0b-9a59-0f1a2c3d4e5f";
const WORK_UUID: &str = "5b2e8a70-1c7d-4f2e-9a61-0d3c9e4b7a10";

type Map = HashMap<String, Setting>;

fn path(path: &str) -> OwnedObjectPath {
    OwnedObjectPath::try_from(path).unwrap()
}

fn value<'v>(value: impl Into<Value<'v>>) -> OwnedValue {
    OwnedValue::try_from(value.into()).unwrap()
}

/// What the fake `NetworkManager` holds, shared by its objects.
#[derive(Default)]
struct World {
    wifi_enabled: bool,
    wwan_enabled: bool,
    /// Profile objects with their settings and secrets.
    profiles: Vec<(String, Map, Map)>,
    active: Vec<OwnedObjectPath>,
    activations: usize,
    /// The agent registered, by the caller's unique name.
    agent: Option<String>,
    /// The password the "Work" network takes.
    work_password: String,
}

type Shared = Arc<Mutex<World>>;

struct FakeNm {
    world: Shared,
}

#[zbus::interface(name = "org.freedesktop.NetworkManager")]
impl FakeNm {
    #[zbus(property)]
    fn devices(&self) -> Vec<OwnedObjectPath> {
        vec![path(WIFI), path(WIRED)]
    }

    #[zbus(property)]
    fn active_connections(&self) -> Vec<OwnedObjectPath> {
        self.world.lock().unwrap().active.clone()
    }

    #[zbus(property)]
    fn wireless_enabled(&self) -> bool {
        self.world.lock().unwrap().wifi_enabled
    }

    #[zbus(property)]
    fn set_wireless_enabled(&mut self, enabled: bool) {
        self.world.lock().unwrap().wifi_enabled = enabled;
    }

    #[zbus(property)]
    fn wireless_hardware_enabled(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn wwan_enabled(&self) -> bool {
        self.world.lock().unwrap().wwan_enabled
    }

    #[zbus(property)]
    fn set_wwan_enabled(&mut self, enabled: bool) {
        self.world.lock().unwrap().wwan_enabled = enabled;
    }

    #[zbus(property)]
    fn wwan_hardware_enabled(&self) -> bool {
        true
    }

    async fn activate_connection(
        &self,
        connection: OwnedObjectPath,
        _device: OwnedObjectPath,
        _specific_object: OwnedObjectPath,
        #[zbus(connection)] bus: &Connection,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        let (number, uuid, id, needs_secret, password) = {
            let mut world = self.world.lock().unwrap();
            let Some((_, settings, _)) = world
                .profiles
                .iter()
                .find(|(object, _, _)| object == connection.as_str())
            else {
                return Err(zbus::fdo::Error::UnknownObject(connection.to_string()));
            };
            let settings = Settings(settings.clone());
            world.activations = world.activations.wrapping_add(1);
            (
                world.activations,
                settings.uuid(),
                settings.id(),
                settings.uint("802-1x", "password-flags") == Some(2),
                world.work_password.clone(),
            )
        };
        let active = path(&format!(
            "/org/freedesktop/NetworkManager/ActiveConnection/{number}"
        ));
        bus.object_server()
            .at(
                active.as_str(),
                FakeActive {
                    id,
                    uuid,
                    profile: connection.clone(),
                    state: 1,
                },
            )
            .await?;
        self.world.lock().unwrap().active.push(active.clone());
        let bus = bus.clone();
        let world = Arc::clone(&self.world);
        let finished = active.clone();
        tokio::spawn(async move {
            let ok = if needs_secret {
                // Ask the agent, as NetworkManager does for a secret the
                // profile is not saved with.
                let agent = world.lock().unwrap().agent.clone();
                let answer = match agent {
                    Some(agent) => bus
                        .call_method(
                            (!agent.is_empty()).then_some(agent.as_str()),
                            "/org/freedesktop/NetworkManager/SecretAgent",
                            Some("org.freedesktop.NetworkManager.SecretAgent"),
                            "GetSecrets",
                            &(
                                Map::new(),
                                connection,
                                "802-1x",
                                Vec::<String>::new(),
                                1_u32,
                            ),
                        )
                        .await
                        .ok()
                        .and_then(|reply| reply.body().deserialize::<Map>().ok()),
                    None => None,
                };
                answer
                    .and_then(|secrets| Settings(secrets).string("802-1x", "password"))
                    .is_some_and(|given| given == password)
            } else {
                true
            };
            let (state, reason) = if ok { (2, 1) } else { (4, 9) };
            let iface = bus
                .object_server()
                .interface::<_, FakeActive>(finished.as_str())
                .await
                .unwrap();
            iface.get_mut().await.state = state;
            FakeActive::state_signal(iface.signal_emitter(), state, reason)
                .await
                .unwrap();
        });
        Ok(active)
    }

    async fn add_and_activate_connection(
        &self,
        connection: Map,
        device: OwnedObjectPath,
        specific_object: OwnedObjectPath,
        #[zbus(connection)] bus: &Connection,
    ) -> zbus::fdo::Result<(OwnedObjectPath, OwnedObjectPath)> {
        let profile = add_profile(&self.world, bus, connection).await?;
        let active = self
            .activate_connection(profile.clone(), device, specific_object, bus)
            .await?;
        Ok((profile, active))
    }

    async fn deactivate_connection(
        &self,
        active: OwnedObjectPath,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        self.world
            .lock()
            .unwrap()
            .active
            .retain(|known| known != &active);
        self.active_connections_changed(&emitter).await?;
        Ok(())
    }
}

async fn add_profile(
    world: &Shared,
    bus: &Connection,
    settings: Map,
) -> zbus::fdo::Result<OwnedObjectPath> {
    let object = {
        let world = world.lock().unwrap();
        format!(
            "/org/freedesktop/NetworkManager/Settings/{}",
            world.profiles.len().wrapping_add(10)
        )
    };
    world
        .lock()
        .unwrap()
        .profiles
        .push((object.clone(), settings, Map::new()));
    bus.object_server()
        .at(
            object.as_str(),
            FakeProfile {
                object: object.clone(),
                world: Arc::clone(world),
            },
        )
        .await?;
    Ok(path(&object))
}

struct FakeActive {
    id: String,
    uuid: String,
    profile: OwnedObjectPath,
    state: u32,
}

#[zbus::interface(name = "org.freedesktop.NetworkManager.Connection.Active")]
impl FakeActive {
    #[zbus(property)]
    fn id(&self) -> String {
        self.id.clone()
    }

    #[zbus(property)]
    fn uuid(&self) -> String {
        self.uuid.clone()
    }

    #[zbus(property, name = "Type")]
    fn kind(&self) -> String {
        "802-11-wireless".into()
    }

    #[zbus(property)]
    fn state(&self) -> u32 {
        self.state
    }

    #[zbus(property)]
    fn devices(&self) -> Vec<OwnedObjectPath> {
        vec![path(WIFI)]
    }

    #[zbus(property)]
    fn connection(&self) -> OwnedObjectPath {
        self.profile.clone()
    }

    #[zbus(signal, name = "StateChanged")]
    async fn state_signal(emitter: &SignalEmitter<'_>, state: u32, reason: u32)
    -> zbus::Result<()>;
}

struct FakeDevice {
    interface: &'static str,
    kind: u32,
    state: u32,
    ip4: OwnedObjectPath,
}

#[zbus::interface(name = "org.freedesktop.NetworkManager.Device")]
impl FakeDevice {
    #[zbus(property)]
    fn interface(&self) -> String {
        self.interface.into()
    }

    #[zbus(property)]
    fn device_type(&self) -> u32 {
        self.kind
    }

    #[zbus(property)]
    fn state(&self) -> u32 {
        self.state
    }

    #[zbus(property)]
    fn hw_address(&self) -> String {
        "AA:BB:CC:DD:EE:FF".into()
    }

    #[zbus(property)]
    fn active_connection(&self) -> OwnedObjectPath {
        OwnedObjectPath::default()
    }

    #[zbus(property)]
    fn ip4_config(&self) -> OwnedObjectPath {
        self.ip4.clone()
    }

    #[zbus(property)]
    fn ip6_config(&self) -> OwnedObjectPath {
        OwnedObjectPath::default()
    }

    fn disconnect(&mut self) {
        self.state = 30;
    }
}

struct FakeWireless;

#[zbus::interface(name = "org.freedesktop.NetworkManager.Device.Wireless")]
impl FakeWireless {
    #[zbus(property)]
    fn access_points(&self) -> Vec<OwnedObjectPath> {
        vec![path(AP_HOME), path(AP_CAFE)]
    }

    #[zbus(property)]
    fn active_access_point(&self) -> OwnedObjectPath {
        path(AP_HOME)
    }
}

struct FakeWired;

#[zbus::interface(name = "org.freedesktop.NetworkManager.Device.Wired")]
impl FakeWired {
    #[zbus(property)]
    fn speed(&self) -> u32 {
        1000
    }
}

struct FakeAccessPoint {
    ssid: &'static str,
    strength: u8,
    rsn: u32,
}

#[zbus::interface(name = "org.freedesktop.NetworkManager.AccessPoint")]
impl FakeAccessPoint {
    #[zbus(property)]
    fn ssid(&self) -> Vec<u8> {
        self.ssid.as_bytes().to_vec()
    }

    #[zbus(property)]
    fn strength(&self) -> u8 {
        self.strength
    }

    #[zbus(property)]
    fn flags(&self) -> u32 {
        u32::from(self.rsn != 0)
    }

    #[zbus(property)]
    fn wpa_flags(&self) -> u32 {
        0
    }

    #[zbus(property)]
    fn rsn_flags(&self) -> u32 {
        self.rsn
    }

    #[zbus(property)]
    fn frequency(&self) -> u32 {
        5180
    }

    #[zbus(property)]
    fn max_bitrate(&self) -> u32 {
        866_000
    }
}

struct FakeIp4;

#[zbus::interface(name = "org.freedesktop.NetworkManager.IP4Config")]
impl FakeIp4 {
    #[zbus(property)]
    fn address_data(&self) -> Vec<Setting> {
        vec![Setting::from([
            ("address".to_owned(), value("192.168.1.20")),
            ("prefix".to_owned(), value(24_u32)),
        ])]
    }

    #[zbus(property)]
    fn gateway(&self) -> String {
        "192.168.1.1".into()
    }

    #[zbus(property)]
    fn nameserver_data(&self) -> Vec<Setting> {
        vec![Setting::from([(
            "address".to_owned(),
            value("192.168.1.1"),
        )])]
    }
}

struct FakeSettings {
    world: Shared,
}

#[zbus::interface(name = "org.freedesktop.NetworkManager.Settings")]
impl FakeSettings {
    fn list_connections(&self) -> Vec<OwnedObjectPath> {
        self.world
            .lock()
            .unwrap()
            .profiles
            .iter()
            .map(|(object, _, _)| path(object))
            .collect()
    }

    fn get_connection_by_uuid(&self, uuid: String) -> zbus::fdo::Result<OwnedObjectPath> {
        self.world
            .lock()
            .unwrap()
            .profiles
            .iter()
            .find(|(_, settings, _)| Settings(settings.clone()).uuid() == uuid)
            .map(|(object, _, _)| path(object))
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs("No connection with the UUID".into()))
    }

    async fn add_connection(
        &self,
        connection: Map,
        #[zbus(connection)] bus: &Connection,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        add_profile(&self.world, bus, connection).await
    }
}

struct FakeProfile {
    object: String,
    world: Shared,
}

impl FakeProfile {
    fn with<T>(&self, read: impl FnOnce(&mut (String, Map, Map)) -> T) -> zbus::fdo::Result<T> {
        let mut world = self.world.lock().unwrap();
        world
            .profiles
            .iter_mut()
            .find(|(object, _, _)| *object == self.object)
            .map(read)
            .ok_or_else(|| zbus::fdo::Error::UnknownObject(self.object.clone()))
    }
}

#[zbus::interface(name = "org.freedesktop.NetworkManager.Settings.Connection")]
impl FakeProfile {
    fn get_settings(&self) -> zbus::fdo::Result<Map> {
        self.with(|(_, settings, _)| settings.clone())
    }

    fn get_secrets(&self, setting: String) -> zbus::fdo::Result<Map> {
        self.with(|(_, _, secrets)| {
            secrets
                .get(&setting)
                .map(|values| Map::from([(setting.clone(), values.clone())]))
                .unwrap_or_default()
        })
    }

    fn update(&self, properties: Map) -> zbus::fdo::Result<()> {
        self.with(|(_, settings, _)| *settings = properties)
    }

    fn delete(&self) -> zbus::fdo::Result<()> {
        let mut world = self.world.lock().unwrap();
        world
            .profiles
            .retain(|(object, _, _)| *object != self.object);
        Ok(())
    }

    #[zbus(property)]
    fn filename(&self) -> String {
        format!("/etc/NetworkManager/system-connections{}", self.object)
    }
}

struct FakeAgentManager {
    world: Shared,
}

#[zbus::interface(name = "org.freedesktop.NetworkManager.AgentManager")]
impl FakeAgentManager {
    fn register(
        &self,
        identifier: String,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<()> {
        if !identifier.starts_with("so.tilo.") {
            return Err(zbus::fdo::Error::InvalidArgs(identifier));
        }
        // On a socket pair the caller has no unique name; the fake calls
        // it back with none.
        let caller = header.sender().map(ToString::to_string);
        self.world.lock().unwrap().agent = Some(caller.unwrap_or_default());
        Ok(())
    }

    fn unregister(&self) {
        self.world.lock().unwrap().agent = None;
    }
}

fn wifi_profile(uuid: &str, id: &str) -> Map {
    Map::from([
        (
            "connection".to_owned(),
            Setting::from([
                ("id".to_owned(), value(id)),
                ("uuid".to_owned(), value(uuid)),
                ("type".to_owned(), value("802-11-wireless")),
                ("timestamp".to_owned(), value(1_700_000_000_u64)),
            ]),
        ),
        (
            "802-11-wireless".to_owned(),
            Setting::from([("ssid".to_owned(), value(id.as_bytes().to_vec()))]),
        ),
    ])
}

#[allow(clippy::too_many_lines, reason = "the whole fake, served in one place")]
async fn serve() -> (Connection, NmClient, Shared) {
    let mut work = wifi_profile(WORK_UUID, "Work");
    work.insert(
        "802-1x".into(),
        Setting::from([("password-flags".to_owned(), value(2_u32))]),
    );
    let home_secrets = Map::from([(
        "802-11-wireless-security".to_owned(),
        Setting::from([("psk".to_owned(), value("correct horse"))]),
    )]);
    let world: Shared = Arc::new(Mutex::new(World {
        wifi_enabled: true,
        profiles: vec![
            (
                format!("{SETTINGS}/1"),
                wifi_profile(HOME_UUID, "Home"),
                home_secrets,
            ),
            (format!("{SETTINGS}/2"), work, Map::new()),
        ],
        work_password: "hunter22".into(),
        ..World::default()
    }));
    let (server, client) = tokio::net::UnixStream::pair().unwrap();
    let mut builder = Builder::unix_stream(server)
        .server(Guid::generate())
        .unwrap()
        .p2p()
        .serve_at(
            tilo_nm::ROOT_PATH,
            FakeNm {
                world: Arc::clone(&world),
            },
        )
        .unwrap()
        .serve_at(
            SETTINGS,
            FakeSettings {
                world: Arc::clone(&world),
            },
        )
        .unwrap()
        .serve_at(
            AGENT_MANAGER,
            FakeAgentManager {
                world: Arc::clone(&world),
            },
        )
        .unwrap()
        .serve_at(
            WIFI,
            FakeDevice {
                interface: "wlan0",
                kind: 2,
                state: 100,
                ip4: path(IP4),
            },
        )
        .unwrap()
        .serve_at(WIFI, FakeWireless)
        .unwrap()
        .serve_at(
            WIRED,
            FakeDevice {
                interface: "enp3s0",
                kind: 1,
                state: 20,
                ip4: OwnedObjectPath::default(),
            },
        )
        .unwrap()
        .serve_at(WIRED, FakeWired)
        .unwrap()
        .serve_at(
            AP_HOME,
            FakeAccessPoint {
                ssid: "Home",
                strength: 80,
                rsn: 0x100,
            },
        )
        .unwrap()
        .serve_at(
            AP_CAFE,
            FakeAccessPoint {
                ssid: "Cafe",
                strength: 40,
                rsn: 0,
            },
        )
        .unwrap()
        .serve_at(IP4, FakeIp4)
        .unwrap();
    for number in [1, 2] {
        let object = format!("{SETTINGS}/{number}");
        builder = builder
            .serve_at(
                path(&object).into_inner(),
                FakeProfile {
                    object: object.clone(),
                    world: Arc::clone(&world),
                },
            )
            .unwrap();
    }
    let client = Builder::unix_stream(client).p2p().build();
    let (server, client) = tokio::join!(builder.build(), client);
    (server.unwrap(), NmClient::new(client.unwrap()), world)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn radios_are_read_and_switched() {
    let (_server, nm, world) = serve().await;
    let radios = nm.radios().await.unwrap();
    assert!(radios.wifi_enabled && radios.wifi_hardware_enabled && !radios.wwan_enabled);
    nm.set_wifi_enabled(false).await.unwrap();
    nm.set_wwan_enabled(true).await.unwrap();
    assert!(!world.lock().unwrap().wifi_enabled);
    let radios = nm.radios().await.unwrap();
    assert!(!radios.wifi_enabled && radios.wwan_enabled);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn devices_access_points_and_addresses_are_read() {
    let (_server, nm, _world) = serve().await;
    let devices = nm.devices().await.unwrap();
    let kinds: Vec<_> = devices
        .iter()
        .map(|device| {
            (
                device.interface.as_str(),
                device.kind,
                device.is_activated(),
            )
        })
        .collect();
    assert_eq!(
        kinds,
        [
            ("wlan0", DeviceKind::Wifi, true),
            ("enp3s0", DeviceKind::Ethernet, false)
        ]
    );
    assert_eq!(nm.wired_speed(WIRED).await.unwrap(), Some(1000));

    let points = nm.access_points().await.unwrap();
    let seen: Vec<_> = points
        .iter()
        .map(|point| {
            (
                point.ssid.as_str(),
                point.strength,
                point.active,
                point.device.as_str(),
            )
        })
        .collect();
    assert_eq!(seen, [("Home", 80, true, WIFI), ("Cafe", 40, false, WIFI)]);
    assert_eq!(points[0].rsn_flags, 0x100);
    assert_eq!(points[0].max_bitrate, 866_000);

    let ip = nm.ip_details(WIFI, IpFamily::V4).await.unwrap();
    assert_eq!(ip.addresses, ["192.168.1.20/24"]);
    assert_eq!(ip.gateway.as_deref(), Some("192.168.1.1"));
    assert_eq!(ip.dns, ["192.168.1.1"]);
    assert_eq!(
        nm.ip_details(WIRED, IpFamily::V4).await.unwrap(),
        tilo_nm::IpDetails::default(),
        "no configuration while unplugged"
    );

    let wired = devices.into_iter().nth(1).unwrap();
    nm.disconnect(&wired).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profiles_are_listed_edited_added_and_deleted() {
    let (_server, nm, world) = serve().await;
    let profiles = nm.profiles().await.unwrap();
    let names: Vec<_> = profiles.iter().map(|p| p.settings.id()).collect();
    assert_eq!(names, ["Home", "Work"]);
    assert_eq!(profiles[0].settings.timestamp(), Some(1_700_000_000));
    assert!(profiles[0].settings.autoconnect(), "the default when unset");
    assert!(profiles[0].filename.ends_with("/Settings/1"));

    let home = nm.profile(HOME_UUID).await.unwrap();
    let secrets = nm.secrets(&home, "802-11-wireless-security").await.unwrap();
    assert_eq!(
        secrets.string("802-11-wireless-security", "psk").as_deref(),
        Some("correct horse")
    );
    assert!(matches!(
        nm.profile("11111111-1111-4111-8111-111111111111").await,
        Err(NmError::NotFound(_))
    ));

    nm.edit(HOME_UUID, |settings| {
        settings.set("connection", "autoconnect", false);
        Ok(())
    })
    .await
    .unwrap();
    assert!(!nm.profile(HOME_UUID).await.unwrap().settings.autoconnect());
    let refused = nm
        .edit(HOME_UUID, |_| Err("no such setting".into()))
        .await
        .unwrap_err();
    assert_eq!(refused.to_string(), "no such setting");

    let mut new = Settings::default();
    new.set("connection", "id", "Mobile data");
    new.set("connection", "type", "gsm");
    let uuid = nm.add(new).await.unwrap();
    assert!(
        uuid::Uuid::try_parse(&uuid).is_ok(),
        "a uuid is made: {uuid}"
    );
    let added = nm.profile(&uuid).await.unwrap();
    assert_eq!(added.settings.kind(), "gsm");
    nm.delete(&added).await.unwrap();
    assert_eq!(world.lock().unwrap().profiles.len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_activation_returns_when_the_connection_is_up_and_can_be_taken_down() {
    let (_server, nm, world) = serve().await;
    let home = nm.profile(HOME_UUID).await.unwrap();
    tilo_test_env::within(
        "the activation settles",
        nm.activate(
            &home,
            Some(WIFI),
            Some(AP_HOME),
            Some(Duration::from_secs(30)),
        ),
    )
    .await
    .unwrap();
    let active = nm.active_connections().await.unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].uuid, HOME_UUID);
    assert_eq!(active[0].profile, home.path);
    assert_eq!(active[0].state, tilo_nm::ActiveState::Activated);
    nm.deactivate(&active[0]).await.unwrap();
    assert!(world.lock().unwrap().active.is_empty());

    let mut new = Settings::default();
    new.set("connection", "id", "Cafe");
    new.set("connection", "type", "802-11-wireless");
    let uuid = tilo_test_env::within(
        "the new connection settles",
        nm.add_and_activate(new, WIFI, Some(AP_CAFE), Duration::from_secs(30)),
    )
    .await
    .unwrap();
    assert_eq!(nm.profile(&uuid).await.unwrap().settings.id(), "Cafe");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unsaved_secret_is_answered_by_an_agent_for_that_activation_only() {
    let (_server, nm, world) = serve().await;
    let work = nm.profile(WORK_UUID).await.unwrap();
    let secrets = |password: &str| {
        HashMap::from([(
            "802-1x".to_owned(),
            HashMap::from([("password".to_owned(), Zeroizing::new(password.to_owned()))]),
        )])
    };
    let wrong = tilo_test_env::within(
        "a refused secret ends the activation",
        nm.activate_with_secrets(&work, secrets("wrong"), Duration::from_secs(30)),
    )
    .await
    .unwrap_err();
    assert!(wrong.to_string().contains("password"), "{wrong}");
    assert!(world.lock().unwrap().agent.is_none(), "the agent is gone");

    tilo_test_env::within(
        "the right secret brings it up",
        nm.activate_with_secrets(&work, secrets("hunter22"), Duration::from_secs(30)),
    )
    .await
    .unwrap();
    assert!(world.lock().unwrap().agent.is_none(), "the agent is gone");
}
