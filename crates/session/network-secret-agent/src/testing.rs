//! A stand-in `NetworkManager` over a peer-to-peer D-Bus link, an
//! in-memory secret store and a log capture: what the agent's tests and a
//! host testing it with its own sheet both need.
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::missing_panics_doc,
    reason = "test support: a fixture that cannot be built fails the calling test"
)]

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use zbus::connection::Builder;
use zbus::zvariant::{ObjectPath, OwnedValue, Value};

use crate::service::{self, AGENT_MANAGER_PATH, AGENT_PATH, Sheets};
use crate::store::{SecretStore, StoreKey};
use crate::{ConnectionDict, FLAG_ALLOW_INTERACTION, SecretRequest, SettingSecrets};
use tilo_lock::MutexRecover as _;

const TEST_GUID: &str = "0123456789abcdef0123456789abcdef";
/// The connection path every stand-in request names.
pub const WIFI_PATH: &str = "/org/freedesktop/NetworkManager/Settings/7";

/// Bounds a wait that ends as soon as its condition holds; only a hang reaches it.
pub const HANG_BOUND: Duration = Duration::from_secs(30);

/// Remembered secrets in memory, and every change made to them.
#[derive(Default)]
pub struct MemoryStore {
    entries: Mutex<BTreeMap<(String, String), SettingSecrets>>,
}

impl MemoryStore {
    pub fn get(&self, uuid: &str, setting: &str) -> Option<SettingSecrets> {
        self.entries
            .lock_recover()
            .get(&(uuid.to_string(), setting.to_string()))
            .cloned()
    }

    pub fn put(&self, uuid: &str, setting: &str, secrets: SettingSecrets) {
        self.entries
            .lock_recover()
            .insert((uuid.to_string(), setting.to_string()), secrets);
    }
}

impl SecretStore for MemoryStore {
    fn load(&self, key: &StoreKey) -> Option<SettingSecrets> {
        self.get(&key.uuid, &key.setting)
    }

    fn keep(&self, key: &StoreKey, secrets: &SettingSecrets) -> Result<(), String> {
        self.put(&key.uuid, &key.setting, secrets.clone());
        Ok(())
    }

    fn forget(&self, key: &StoreKey) -> Result<(), String> {
        self.entries
            .lock_recover()
            .remove(&(key.uuid.clone(), key.setting.clone()));
        Ok(())
    }

    fn forget_connection(&self, uuid: &str) -> Result<(), String> {
        self.entries
            .lock_recover()
            .retain(|(entry, _), _| entry != uuid);
        Ok(())
    }

    fn rejected(&self, key: &StoreKey) -> Option<SettingSecrets> {
        self.get(&key.uuid, &format!("rejected:{}", key.setting))
    }

    fn reject(&self, key: &StoreKey, secrets: &SettingSecrets) -> Result<(), String> {
        self.put(
            &key.uuid,
            &format!("rejected:{}", key.setting),
            secrets.clone(),
        );
        Ok(())
    }

    fn clear_rejected(&self, key: &StoreKey) -> Result<(), String> {
        self.entries
            .lock_recover()
            .remove(&(key.uuid.clone(), format!("rejected:{}", key.setting)));
        Ok(())
    }
}

/// `NetworkManager`'s agent manager, recording registrations.
#[derive(Clone, Default)]
struct FakeAgentManager {
    registered: Arc<Mutex<Vec<(String, u32)>>>,
}

#[zbus::interface(name = "org.freedesktop.NetworkManager.AgentManager")]
impl FakeAgentManager {
    fn register_with_capabilities(&self, identifier: String, capabilities: u32) {
        self.registered
            .lock_recover()
            .push((identifier, capabilities));
    }
}

/// Everything written through `tracing` while a test runs.
#[derive(Clone, Default)]
pub struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock_recover()).into_owned()
    }
}

impl std::io::Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock_recover().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Capture this thread's logs until the guard drops.
pub fn capture_logs() -> (Captured, tracing::subscriber::DefaultGuard) {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    (captured, tracing::subscriber::set_default(subscriber))
}

/// A stand-in `NetworkManager` wired to a registered agent.
pub struct Link {
    pub nm: zbus::Connection,
    /// Every `RegisterWithCapabilities` call: identifier and capabilities.
    pub registered: Arc<Mutex<Vec<(String, u32)>>>,
    _agent: service::AgentHandle,
}

/// Serve the agent, asking in `sheets` and remembering in `store`, to a
/// stand-in `NetworkManager` it registers with.
pub async fn link(sheets: Arc<dyn Sheets>, store: Arc<dyn SecretStore>) -> Link {
    let (agent_socket, nm_socket) = tokio::net::UnixStream::pair().expect("socket pair");
    let manager = FakeAgentManager::default();
    let registered = Arc::clone(&manager.registered);
    let agent_side = Builder::unix_stream(agent_socket)
        .server(TEST_GUID)
        .expect("guid")
        .p2p();
    let nm_side = Builder::unix_stream(nm_socket)
        .p2p()
        .serve_at(AGENT_MANAGER_PATH, manager)
        .expect("serve agent manager");
    let (agent_conn, nm) =
        futures_util::try_join!(agent_side.build(), nm_side.build()).expect("p2p pair");
    let agent = service::serve_on(agent_conn, sheets, store)
        .await
        .expect("agent served and registered");
    Link {
        nm,
        registered,
        _agent: agent,
    }
}

/// What `GetSecrets` answers.
pub type Reply = zbus::Result<HashMap<String, HashMap<String, OwnedValue>>>;

/// A connection as a test writes it: setting → key → value.
pub type Connection = HashMap<&'static str, HashMap<&'static str, Value<'static>>>;

impl Link {
    pub async fn agent(&self) -> zbus::Proxy<'static> {
        zbus::Proxy::new(
            &self.nm,
            "org.freedesktop.NetworkManager.SecretAgent",
            AGENT_PATH,
            "org.freedesktop.NetworkManager.SecretAgent",
        )
        .await
        .expect("agent proxy")
    }

    /// Ask as `NetworkManager` does, without waiting for the answer.
    pub async fn get_secrets(
        &self,
        connection: Connection,
        setting: &'static str,
        hints: Vec<&'static str>,
        flags: u32,
    ) -> tokio::task::JoinHandle<Reply> {
        let agent = self.agent().await;
        tokio::spawn(async move {
            agent
                .call(
                    "GetSecrets",
                    &(
                        connection,
                        ObjectPath::from_static_str_unchecked(WIFI_PATH),
                        setting,
                        hints,
                        flags,
                    ),
                )
                .await
        })
    }
}

/// `GetSecrets` for `setting` of `connection`, understood as the agent
/// would, with interaction allowed.
pub fn request_for(connection: Connection, setting: &str, hints: &[&str]) -> SecretRequest {
    let connection: ConnectionDict = connection
        .into_iter()
        .map(|(setting, values)| {
            (
                setting.to_string(),
                values
                    .into_iter()
                    .map(|(key, value)| {
                        (key.to_string(), OwnedValue::try_from(value).expect("owned"))
                    })
                    .collect(),
            )
        })
        .collect();
    let hints: Vec<String> = hints.iter().map(ToString::to_string).collect();
    SecretRequest::parse(
        &connection,
        WIFI_PATH,
        setting,
        &hints,
        FLAG_ALLOW_INTERACTION,
    )
    .expect("a request")
}

/// A VPN profile, "Office", whose plugin asks for its own secrets.
pub fn vpn_connection() -> Connection {
    HashMap::from([
        (
            "connection",
            HashMap::from([
                ("id", Value::from("Office")),
                ("uuid", Value::from("5f2c7c9e-0000-4000-8000-000000000003")),
                ("type", Value::from("vpn")),
            ]),
        ),
        (
            "vpn",
            HashMap::from([(
                "service-type",
                Value::from("org.freedesktop.NetworkManager.openvpn"),
            )]),
        ),
    ])
}

/// A mobile broadband profile, "Carrier", behind a SIM PIN.
pub fn gsm_connection() -> Connection {
    HashMap::from([
        (
            "connection",
            HashMap::from([
                ("id", Value::from("Carrier")),
                ("uuid", Value::from("5f2c7c9e-0000-4000-8000-000000000004")),
                ("type", Value::from("gsm")),
            ]),
        ),
        ("gsm", HashMap::from([("apn", Value::from("internet"))])),
    ])
}

/// `setting.key` of a reply, which must be a string.
pub fn string_of<S: std::hash::BuildHasher, T: std::hash::BuildHasher>(
    reply: &HashMap<String, HashMap<String, OwnedValue, T>, S>,
    setting: &str,
    key: &str,
) -> String {
    match &**reply
        .get(setting)
        .and_then(|values| values.get(key))
        .unwrap_or_else(|| panic!("{setting}.{key} missing from {reply:?}"))
    {
        Value::Str(value) => value.to_string(),
        other => panic!("{setting}.{key} is {other:?}"),
    }
}

/// A WPA2-Personal network, "Home Net", without its passphrase.
pub fn wifi_psk_connection() -> Connection {
    HashMap::from([
        (
            "connection",
            HashMap::from([
                ("id", Value::from("Home")),
                ("uuid", Value::from("5f2c7c9e-0000-4000-8000-000000000001")),
                ("type", Value::from("802-11-wireless")),
            ]),
        ),
        (
            "802-11-wireless",
            HashMap::from([("ssid", Value::from(b"Home Net".to_vec()))]),
        ),
        (
            "802-11-wireless-security",
            HashMap::from([("key-mgmt", Value::from("wpa-psk"))]),
        ),
    ])
}

/// An 802.1X (PEAP) network, "eduroam", with a user name but no password.
pub fn eap_connection() -> Connection {
    HashMap::from([
        (
            "connection",
            HashMap::from([
                ("id", Value::from("Campus")),
                ("uuid", Value::from("5f2c7c9e-0000-4000-8000-000000000002")),
                ("type", Value::from("802-11-wireless")),
            ]),
        ),
        (
            "802-11-wireless",
            HashMap::from([("ssid", Value::from(b"eduroam".to_vec()))]),
        ),
        (
            "802-11-wireless-security",
            HashMap::from([("key-mgmt", Value::from("wpa-eap"))]),
        ),
        (
            "802-1x",
            HashMap::from([
                ("eap", Value::from(vec!["peap"])),
                ("identity", Value::from("quinn@example.edu")),
                ("phase2-auth", Value::from("mschapv2")),
            ]),
        ),
    ])
}
