//! The agent over a peer-to-peer D-Bus link to a stand-in `NetworkManager`,
//! answered through the same sheet controller the compositor draws.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use winit::keyboard::{Key, NamedKey};
use zbus::connection::Builder;
use zbus::zvariant::{ObjectPath, OwnedValue, Value};

use super::service::{AGENT_IDENTIFIER, AGENT_MANAGER_PATH, AGENT_PATH, CAPABILITY_VPN_HINTS};
use super::store::{RoamingWifi, RoamingWifiSlot, SecretStore, StoreKey, VaultStore};
use super::*;
use crate::shell::portal_dialog::{PortalDialogController, PortalDialogQueue};

const TEST_GUID: &str = "0123456789abcdef0123456789abcdef";
const WIFI_PATH: &str = "/org/freedesktop/NetworkManager/Settings/7";

/// Bounds a wait that ends as soon as its condition holds; only a hang reaches it.
const HANG_BOUND: std::time::Duration = std::time::Duration::from_secs(30);

/// Remembered secrets in memory, and every change made to them.
#[derive(Default)]
struct MemoryStore {
    entries: Mutex<BTreeMap<(String, String), SettingSecrets>>,
}

impl MemoryStore {
    fn get(&self, uuid: &str, setting: &str) -> Option<SettingSecrets> {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(uuid.to_string(), setting.to_string()))
            .cloned()
    }

    fn put(&self, uuid: &str, setting: &str, secrets: SettingSecrets) {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
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
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&(key.uuid.clone(), key.setting.clone()));
        Ok(())
    }

    fn forget_connection(&self, uuid: &str) -> Result<(), String> {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(entry, _), _| entry != uuid);
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
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((identifier, capabilities));
    }
}

/// Everything written through `tracing` while a test runs.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap_or_else(PoisonError::into_inner)).into_owned()
    }
}

impl std::io::Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn capture_logs() -> (Captured, tracing::subscriber::DefaultGuard) {
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
struct Link {
    nm: zbus::Connection,
    registered: Arc<Mutex<Vec<(String, u32)>>>,
    _agent: service::AgentHandle,
}

async fn link(queue: PortalDialogQueue, store: Arc<dyn SecretStore>) -> Link {
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
    let agent = service::serve_on(agent_conn, queue, store)
        .await
        .expect("agent served and registered");
    Link {
        nm,
        registered,
        _agent: agent,
    }
}

type Reply = zbus::Result<HashMap<String, HashMap<String, OwnedValue>>>;

impl Link {
    async fn agent(&self) -> zbus::Proxy<'static> {
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
    async fn get_secrets(
        &self,
        connection: HashMap<&'static str, HashMap<&'static str, Value<'static>>>,
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

#[allow(
    clippy::future_not_send,
    reason = "the sheet's text fields are UI widgets; each test awaits this on its own \
    current-thread runtime and never sends it"
)]
async fn sheet_up(controller: &mut PortalDialogController) -> serde_json::Value {
    let deadline = std::time::Instant::now()
        .checked_add(HANG_BOUND)
        .expect("the hang bound fits in an Instant");
    while std::time::Instant::now() < deadline {
        controller.pump();
        let state = controller.state_json();
        if !state.is_null() {
            return state;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("no sheet appeared");
}

fn type_text(controller: &mut PortalDialogController, text: &str) {
    for ch in text.chars() {
        let key = if ch == ' ' {
            Key::Named(NamedKey::Space)
        } else {
            Key::Character(ch.to_string().into())
        };
        assert!(edit(controller, &key));
    }
}

fn edit(controller: &mut PortalDialogController, key: &Key) -> bool {
    let event = ui::synthetic_input::key_input(key, winit::event::ElementState::Pressed);
    crate::shell::test_support::with_scratch_event_ctx(|ctx| controller.edit_secret(&event, ctx))
}

fn string_of(
    reply: &HashMap<String, HashMap<String, OwnedValue>>,
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

fn wifi_psk_connection() -> HashMap<&'static str, HashMap<&'static str, Value<'static>>> {
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

fn eap_connection() -> HashMap<&'static str, HashMap<&'static str, Value<'static>>> {
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

#[tokio::test]
async fn a_wifi_password_is_asked_in_the_sheet_and_returned_only_to_networkmanager() {
    let (logs, _guard) = capture_logs();
    let queue = PortalDialogQueue::default();
    let store = Arc::new(MemoryStore::default());
    let link = link(queue.clone(), Arc::clone(&store) as Arc<dyn SecretStore>).await;
    assert_eq!(
        link.registered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_slice(),
        &[(AGENT_IDENTIFIER.to_string(), CAPABILITY_VPN_HINTS)],
        "registered once, under the stable identifier"
    );

    let reply = link
        .get_secrets(
            wifi_psk_connection(),
            "802-11-wireless-security",
            vec!["psk"],
            FLAG_ALLOW_INTERACTION,
        )
        .await;
    let mut controller = PortalDialogController::new(queue);
    let state = sheet_up(&mut controller).await;
    assert_eq!(state["kind"], "network_secrets");
    assert_eq!(state["title"], "Wi-Fi password required");
    assert!(
        state["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("“Home Net”")),
        "{state}"
    );
    assert_eq!(state["fields"][0]["key"], "psk");
    assert_eq!(state["fields"][0]["masked"], true);
    assert_eq!(state["remember"], true);

    // Too short for WPA: the sheet stays and says why.
    type_text(&mut controller, "short");
    assert!(!controller.confirm());
    assert!(controller.state_json()["error"].is_string());

    type_text(&mut controller, " horse");
    let state = controller.state_json();
    assert_eq!(state["fields"][0]["length"], 11);
    assert!(
        state["fields"][0].get("value").is_none(),
        "a masked value never reaches state-dump"
    );
    assert!(!state.to_string().contains("short horse"));
    assert!(controller.confirm());

    let reply = reply.await.expect("call task").expect("secrets returned");
    assert_eq!(
        string_of(&reply, "802-11-wireless-security", "psk"),
        "short horse"
    );
    assert_eq!(reply.len(), 1, "only the setting that was asked for");
    assert_eq!(
        store
            .get(
                "5f2c7c9e-0000-4000-8000-000000000001",
                "802-11-wireless-security"
            )
            .and_then(|secrets| secrets.get("psk").map(str::to_string))
            .as_deref(),
        Some("short horse"),
        "remembered, as the sheet offered"
    );
    assert!(controller.state_json().is_null(), "the sheet closed");

    let logs = logs.text();
    assert!(
        logs.contains("network secrets request finished"),
        "the capture must observe the agent: {logs}"
    );
    assert!(
        !logs.contains("short horse"),
        "a secret reached the log: {logs}"
    );
}

#[tokio::test]
async fn an_8021x_sign_in_returns_identity_and_password_in_the_8021x_setting() {
    let (logs, _guard) = capture_logs();
    let queue = PortalDialogQueue::default();
    let store = Arc::new(MemoryStore::default());
    let link = link(queue.clone(), Arc::clone(&store) as Arc<dyn SecretStore>).await;
    let reply = link
        .get_secrets(
            eap_connection(),
            "802-1x",
            Vec::new(),
            FLAG_ALLOW_INTERACTION | FLAG_USER_REQUESTED,
        )
        .await;
    let mut controller = PortalDialogController::new(queue);
    let state = sheet_up(&mut controller).await;
    assert_eq!(state["title"], "Network sign-in");
    let keys: Vec<&str> = state["fields"]
        .as_array()
        .expect("fields")
        .iter()
        .filter_map(|field| field["key"].as_str())
        .collect();
    assert_eq!(keys, ["identity", "anonymous-identity", "password"]);
    assert_eq!(state["fields"][0]["value"], "quinn@example.edu");
    assert_eq!(
        state["fields"][2]["focused"], true,
        "starts on the password"
    );
    assert!(
        state["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("certificate is not checked")),
        "an unchecked server certificate is said out loud: {state}"
    );

    type_text(&mut controller, "tr0ub4dor&3");
    // Untick "Remember": Tab to its row, Space.
    assert!(controller.select_next(false));
    assert!(edit(&mut controller, &Key::Named(NamedKey::Space)));
    assert_eq!(controller.state_json()["remember"], false);
    assert!(controller.confirm());

    let reply = reply.await.expect("call task").expect("secrets returned");
    let setting = reply.get("802-1x").expect("the 802-1x setting");
    assert_eq!(string_of(&reply, "802-1x", "identity"), "quinn@example.edu");
    assert_eq!(string_of(&reply, "802-1x", "password"), "tr0ub4dor&3");
    assert!(
        !setting.contains_key("anonymous-identity"),
        "an empty optional field is left out"
    );
    assert!(
        store
            .get("5f2c7c9e-0000-4000-8000-000000000002", "802-1x")
            .is_none(),
        "not remembered when unticked"
    );
    assert!(!logs.text().contains("tr0ub4dor"));
}

#[tokio::test]
async fn networkmanager_cancelling_takes_the_sheet_down() {
    let queue = PortalDialogQueue::default();
    let link = link(queue.clone(), Arc::new(MemoryStore::default())).await;
    let reply = link
        .get_secrets(
            wifi_psk_connection(),
            "802-11-wireless-security",
            Vec::new(),
            FLAG_ALLOW_INTERACTION,
        )
        .await;
    let mut controller = PortalDialogController::new(queue);
    sheet_up(&mut controller).await;
    type_text(&mut controller, "half typed");

    link.agent()
        .await
        .call::<_, _, ()>(
            "CancelGetSecrets",
            &(
                ObjectPath::from_static_str_unchecked(WIFI_PATH),
                "802-11-wireless-security",
            ),
        )
        .await
        .expect("cancel accepted");
    let error = reply
        .await
        .expect("call task")
        .expect_err("a withdrawn request has no secrets");
    assert!(
        error
            .to_string()
            .contains("org.freedesktop.NetworkManager.SecretAgent.AgentCanceled"),
        "{error}"
    );
    assert!(controller.pump(), "the withdrawn sheet is let go");
    assert!(controller.state_json().is_null());
    assert!(controller.secret_fields().is_empty());
}

#[tokio::test]
async fn dismissing_the_sheet_is_a_user_cancel() {
    let queue = PortalDialogQueue::default();
    let link = link(queue.clone(), Arc::new(MemoryStore::default())).await;
    let reply = link
        .get_secrets(
            wifi_psk_connection(),
            "802-11-wireless-security",
            Vec::new(),
            FLAG_ALLOW_INTERACTION,
        )
        .await;
    let mut controller = PortalDialogController::new(queue);
    sheet_up(&mut controller).await;
    assert!(controller.cancel());
    let error = reply.await.expect("call task").expect_err("dismissed");
    assert!(
        error
            .to_string()
            .contains("org.freedesktop.NetworkManager.SecretAgent.UserCanceled"),
        "{error}"
    );
}

#[tokio::test]
async fn remembered_secrets_answer_without_asking_unless_new_ones_are_wanted() {
    let queue = PortalDialogQueue::default();
    let store = Arc::new(MemoryStore::default());
    let mut remembered = SettingSecrets::default();
    remembered.insert("psk", "correct horse");
    store.put(
        "5f2c7c9e-0000-4000-8000-000000000001",
        "802-11-wireless-security",
        remembered,
    );
    let link = link(queue.clone(), Arc::clone(&store) as Arc<dyn SecretStore>).await;

    // Background activation: no interaction allowed, remembered answer.
    let reply = link
        .get_secrets(
            wifi_psk_connection(),
            "802-11-wireless-security",
            Vec::new(),
            0,
        )
        .await
        .await
        .expect("call task")
        .expect("remembered secrets");
    assert_eq!(
        string_of(&reply, "802-11-wireless-security", "psk"),
        "correct horse"
    );
    let mut idle = PortalDialogController::new(queue.clone());
    assert!(!idle.pump(), "answered without a sheet");

    // The remembered password was rejected: ask again, and say why.
    let reply = link
        .get_secrets(
            wifi_psk_connection(),
            "802-11-wireless-security",
            Vec::new(),
            FLAG_ALLOW_INTERACTION | FLAG_REQUEST_NEW,
        )
        .await;
    let mut controller = PortalDialogController::new(queue);
    let state = sheet_up(&mut controller).await;
    assert!(
        state["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("did not work")),
        "{state}"
    );
    type_text(&mut controller, "battery staple");
    assert!(controller.confirm());
    let reply = reply.await.expect("call task").expect("new secrets");
    assert_eq!(
        string_of(&reply, "802-11-wireless-security", "psk"),
        "battery staple"
    );
}

#[tokio::test]
async fn nothing_remembered_and_no_asking_is_no_secrets() {
    let queue = PortalDialogQueue::default();
    let link = link(queue, Arc::new(MemoryStore::default())).await;
    let error = link
        .get_secrets(
            wifi_psk_connection(),
            "802-11-wireless-security",
            Vec::new(),
            0,
        )
        .await
        .await
        .expect("call task")
        .expect_err("no secrets");
    assert!(
        error
            .to_string()
            .contains("org.freedesktop.NetworkManager.SecretAgent.NoSecrets"),
        "{error}"
    );
}

#[tokio::test]
async fn saved_agent_owned_secrets_are_kept_and_deleted_with_the_connection() {
    let queue = PortalDialogQueue::default();
    let store = Arc::new(MemoryStore::default());
    let link = link(queue, Arc::clone(&store) as Arc<dyn SecretStore>).await;
    let mut connection = wifi_psk_connection();
    connection
        .get_mut("802-11-wireless-security")
        .expect("security")
        .extend([
            ("psk", Value::from("correct horse")),
            ("psk-flags", Value::from(SECRET_AGENT_OWNED)),
            // A system-owned secret stays NetworkManager's business.
            ("leap-password", Value::from("not ours")),
        ]);
    let agent = link.agent().await;
    agent
        .call::<_, _, ()>(
            "SaveSecrets",
            &(
                &connection,
                ObjectPath::from_static_str_unchecked(WIFI_PATH),
            ),
        )
        .await
        .expect("saved");
    let kept = store
        .get(
            "5f2c7c9e-0000-4000-8000-000000000001",
            "802-11-wireless-security",
        )
        .expect("kept");
    assert_eq!(kept.get("psk"), Some("correct horse"));
    assert_eq!(kept.get("leap-password"), None);

    agent
        .call::<_, _, ()>(
            "DeleteSecrets",
            &(
                &connection,
                ObjectPath::from_static_str_unchecked(WIFI_PATH),
            ),
        )
        .await
        .expect("deleted");
    assert!(
        store
            .get(
                "5f2c7c9e-0000-4000-8000-000000000001",
                "802-11-wireless-security"
            )
            .is_none()
    );
}

fn request_for(
    connection: HashMap<&'static str, HashMap<&'static str, Value<'static>>>,
    setting: &str,
    hints: &[&str],
) -> SecretRequest {
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

#[test]
fn vpn_secrets_come_from_hints_and_go_back_under_vpn_secrets() {
    let connection = HashMap::from([
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
    ]);
    let request = request_for(
        connection,
        "vpn",
        &["password", "x-vpn-message:Use your one-time code"],
    );
    assert_eq!(request.fields.len(), 1);
    assert_eq!(request.fields[0].key, "password");
    assert!(request.detail.contains("one-time code"));
    assert!(
        !request.remember_default,
        "a VPN secret is remembered by choice"
    );
    let mut secrets = SettingSecrets::default();
    secrets.insert("password", "123456");
    let reply = request.response(&secrets);
    let vpn = reply.get("vpn").expect("vpn setting");
    let inner = HashMap::<String, String>::try_from(
        vpn.get("secrets")
            .expect("secrets")
            .try_clone()
            .expect("clone"),
    )
    .expect("a{ss}");
    assert_eq!(inner.get("password").map(String::as_str), Some("123456"));
}

#[test]
fn a_sim_pin_is_asked_as_a_pin() {
    let connection = HashMap::from([
        (
            "connection",
            HashMap::from([
                ("id", Value::from("Carrier")),
                ("uuid", Value::from("5f2c7c9e-0000-4000-8000-000000000004")),
                ("type", Value::from("gsm")),
            ]),
        ),
        ("gsm", HashMap::from([("apn", Value::from("internet"))])),
    ]);
    let request = request_for(connection, "gsm", &["pin"]);
    assert_eq!(request.title, "SIM PIN required");
    assert_eq!(request.fields[0].key, "pin");
    assert!(request.fields[0].masked);
}

/// A roaming store that records what was shared.
#[derive(Default)]
struct FakeRoaming {
    shared: Mutex<Vec<(String, os_actions::WifiSecurity, String)>>,
}

impl RoamingWifi for FakeRoaming {
    fn password(&self, ssid: &str, security: os_actions::WifiSecurity) -> Option<String> {
        self.shared
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|(name, mode, _)| name == ssid && *mode == security)
            .map(|(_, _, password)| password.clone())
    }

    fn share(
        &self,
        ssid: &str,
        security: os_actions::WifiSecurity,
        password: &str,
    ) -> Result<(), String> {
        self.shared
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((ssid.into(), security, password.into()));
        Ok(())
    }
}

#[test]
fn the_vault_keeps_secrets_sealed_and_wpa_passphrases_roam() {
    let dir = tempfile::tempdir().expect("temp dir");
    let roaming = RoamingWifiSlot::default();
    let vault = VaultStore::at(
        dir.path().join("network-secrets"),
        secret_vault::Custody::LocalFile,
        roaming.clone(),
    );
    let eap = StoreKey {
        uuid: "uuid-eap".into(),
        setting: "802-1x".into(),
        roaming_wifi: None,
    };
    let mut secrets = SettingSecrets::default();
    secrets.insert("identity", "quinn@example.edu");
    secrets.insert("password", "tr0ub4dor&3");
    vault.keep(&eap, &secrets).expect("kept");
    let sealed = std::fs::read(vault.dir().join(secret_vault::VAULT_FILE)).expect("vault file");
    assert!(
        !String::from_utf8_lossy(&sealed).contains("tr0ub4dor"),
        "the vault file is sealed"
    );
    assert_eq!(vault.load(&eap), Some(secrets));

    // With the person's devices sharing Wi-Fi, a WPA passphrase roams and
    // is not also kept locally.
    let fake = Arc::new(FakeRoaming::default());
    roaming.set(Arc::clone(&fake) as Arc<dyn RoamingWifi>);
    let wifi = StoreKey {
        uuid: "uuid-wifi".into(),
        setting: "802-11-wireless-security".into(),
        roaming_wifi: Some(("Home Net".into(), os_actions::WifiSecurity::Wpa2)),
    };
    let mut psk = SettingSecrets::default();
    psk.insert("psk", "correct horse");
    vault.keep(&wifi, &psk).expect("shared");
    assert_eq!(
        fake.shared
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_slice(),
        &[(
            "Home Net".to_string(),
            os_actions::WifiSecurity::Wpa2,
            "correct horse".to_string()
        )]
    );
    assert_eq!(
        vault
            .load(&wifi)
            .and_then(|s| s.get("psk").map(str::to_string))
            .as_deref(),
        Some("correct horse")
    );

    vault.forget_connection("uuid-eap").expect("forgotten");
    assert_eq!(vault.load(&eap), None);
}

/// The session serves the agent from its multi-thread runtime; choosing the
/// vault's custody there probes the secret store over blocking D-Bus, which
/// once aborted the whole session at startup.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serving_from_the_session_runtime_never_blocks_a_runtime_thread() {
    let config = tempfile::tempdir().expect("config dir");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("XDG_CONFIG_HOME", config.path());
    env.set(
        "DBUS_SESSION_BUS_ADDRESS",
        "unix:path=/nonexistent/session-bus",
    );
    env.set(
        "DBUS_SYSTEM_BUS_ADDRESS",
        "unix:path=/nonexistent/system-bus",
    );
    let served = tokio::time::timeout(
        HANG_BOUND,
        service::serve(PortalDialogQueue::default(), RoamingWifiSlot::default()),
    )
    .await
    .expect("serve answers");
    assert!(served.is_err(), "no system bus to serve on");
}
